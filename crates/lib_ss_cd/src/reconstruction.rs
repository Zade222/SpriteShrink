use std::{
    collections::{VecDeque},
    fmt::Display,
    fs::File,
    hash::Hasher,
    io::{BufWriter, Write},
    sync::{Arc, Condvar, Mutex},
    thread::scope
};

use crate::{
    analyze::SYNC_PATTERN,
    ecc::{calc_ecc_simd_inplace, calc_ecc_bitwise, calculate_edc},
    lib_error_handling::SpriteShrinkCDError,
    lib_structs::{
        DecodedSectorInfo, DiscManifest, MsfTime, RleSectorMap,
        SectorType, SSMDIndices, SubHeaderEntry
    },
    util::{
        BufferState, ConsumerGuard, SharedBuffer,
        pull_data, spawn_audio_decomp_worker, spawn_chunk_decomp_worker,
        spawn_data_fetcher
    },
};

use flume::{
    bounded,
};
use sha2::{Digest,Sha512};
use sprite_shrink::{
    Hashable, SpriteShrinkError,
};
use thiserror::Error;
use xxhash_rust::xxh3::Xxh3;


#[derive(Error, Debug)]
pub enum ReconstructionError {
    #[error("ECC Reconstruction failed at sector {sector_idx}: {source}")]
    EccError { sector_idx: u32, source: crate::ecc::EccError },

    #[error("Verification failed.\n
        Original Hash: {orig_hash}\n
        Calculated Hash: {calc_hash}")]
    HashMismatchError{orig_hash: String, calc_hash: String},

    #[error("An internal logic error occurred: {0}")]
    InternalError(String),

    #[error("A thread panic occurred: {0}")]
    ThreadPanic(String),

    #[error("Unsupported sector type for reconstruction: {0:?}")]
    UnsupportedType(SectorType),

    #[error("Data length mismatch. \n
        Type {sector_type}, Expected {expected}, got {actual}")]
    DataLengthMismatch { sector_type: String, expected: usize, actual: usize },

    #[error("Missing subheader data for Mode 2 sector number {0}")]
    MissingSubheader(usize),

    #[error("Missing exception data for Exception sector type")]
    MissingExceptionData,

    #[error("The exception index lists sector {sector}, but the disc has \
        {total_sectors} sectors")]
    ExceptionSectorOutOfRange { sector: u32, total_sectors: usize },

    #[error("Exception id {0} isn't in the exception index")]
    UnknownException(u32),

    #[error("Exception data at offset {offset} ({size} bytes) runs past the \
        end of the {blob_len}-byte exception blob")]
    ExceptionDataOutOfRange { offset: u64, size: usize, blob_len: usize },

    #[error("Subheader id {0} isn't in the subheader table")]
    UnknownSubheader(u16),

    #[error("Audio block failed its integrity check (expected hash \
        {expected}, got {actual}). The archive may be damaged.")]
    AudioBlockHashMismatch { expected: String, actual: String },

    #[error("Reconstruction failure.")]
    ReconstructionFailure,
}


pub fn build_decoded_map(rle_map: &RleSectorMap) -> Vec<DecodedSectorInfo> {
    let total_sectors: usize = rle_map.runs
        .iter()
        .map(|(count, _)| *count as usize)
        .sum();

    let mut map = Vec::with_capacity(total_sectors);
    let mut current_stream_offset = 0u64;

    for (run_count, run_type) in &rle_map.runs {
        // Audio sectors are stored as FLAC blocks, not in the data stream
        // (`UserDataStream` skips them), so they don't move the offset.
        let stream_bytes = match run_type {
            SectorType::Audio | SectorType::PregapAudio |
            SectorType::ZeroedAudio => 0,
            _ => u64::from(run_type.data_size()),
        };
        for _ in 0..*run_count {
            map.push(DecodedSectorInfo {
                sector_type: *run_type,
                stream_offset: current_stream_offset,
            });

            current_stream_offset += stream_bytes;
        }
    }
    map
}


/// Expands the disc's exception index into one entry per sector.
///
/// # Errors
/// [`ReconstructionError::ExceptionSectorOutOfRange`] when an entry names a
/// sector at or past `total_sectors`. The index is read from the archive, so
/// this means the archive is corrupt.
pub fn expand_exception_index(
    excep_idx: &[(u32, u32)],
    total_sectors: usize
) -> Result<Vec<Option<u32>>, ReconstructionError> {
    let mut lookup = vec![None; total_sectors];

    for &(sector, excep_id) in excep_idx {
        let slot = lookup.get_mut(sector as usize).ok_or(
            ReconstructionError::ExceptionSectorOutOfRange {
                sector,
                total_sectors
            }
        )?;
        *slot = Some(excep_id);
    }

    Ok(lookup)
}


/// Returns the `size` bytes of exception data stored for `excep_id`. The id
/// and the offset it leads to are both read from the archive, so each is
/// checked before use.
pub(crate) fn exception_data_for<'a>(
    exception_index: &[u64],
    exception_blob: &'a [u8],
    excep_id: u32,
    size: usize,
) -> Result<&'a [u8], ReconstructionError> {
    let &offset = exception_index.get(excep_id as usize)
        .ok_or(ReconstructionError::UnknownException(excep_id))?;

    usize::try_from(offset).ok()
        .and_then(|start| exception_blob.get(start..start.checked_add(size)?))
        .ok_or(ReconstructionError::ExceptionDataOutOfRange {
            offset,
            size,
            blob_len: exception_blob.len()
        })
}


/// Returns the subheader (or Mode 1 reserved bytes) stored as `data_id`.
pub(crate) fn subheader_for(
    subheader_table: &[[u8; 8]],
    data_id: u16,
) -> Result<&[u8; 8], ReconstructionError> {
    subheader_table.get(usize::from(data_id))
        .ok_or(ReconstructionError::UnknownSubheader(data_id))
}


/// Walks a disc's LBA map in sector order and gives each sector the frame
/// number its header holds.
///
/// Each map entry is `(sector, offset)`: from that point on, a sector's
/// frame number is its index plus `offset`. The map is empty when the
/// mapper saw no data sector header, as on an audio-only disc; the offset is
/// then 0.
pub(crate) struct MsfOffsets<'a> {
    lba_map: &'a [(u32, u32)],
    index: usize,
    offset: u32,
}

impl<'a> MsfOffsets<'a> {
    pub(crate) fn new(lba_map: &'a [(u32, u32)]) -> Self {
        let offset = lba_map.first().map_or(0, |&(_, offset)| offset);
        Self { lba_map, index: 0, offset }
    }

    /// The frame number of `sector_idx`. Calls must come in increasing
    /// sector order. A corrupt offset saturates instead of overflowing; the
    /// wrong header it gives fails the integrity check.
    pub(crate) fn sector_num(&mut self, sector_idx: u32) -> u32 {
        while let Some(&(threshold, offset)) = self.lba_map.get(self.index + 1)
            && self.offset.saturating_add(sector_idx) >= threshold
        {
            self.index += 1;
            self.offset = offset;
        }
        sector_idx.saturating_add(self.offset)
    }
}


pub fn expand_sector_map(rle_map: &RleSectorMap) -> Vec<SectorType> {
    let mut sectors = Vec::new();
    for (count, s_type) in &rle_map.runs {
        sectors.extend(std::iter::repeat_n(*s_type, *count as usize));
    }
    sectors
}


pub fn expand_subheader_map(
    index: &[SubHeaderEntry],
    total_sectors: usize
) -> Vec<Option<u16>> {
    let mut metadata_map = vec![None; total_sectors];

    // Entries come from the archive. Clamping the range to the disc skips
    // sectors past the end without overflowing on a corrupt `start_lba`.
    for entry in index {
        let start = entry.start_lba as usize;
        let end = start.saturating_add(entry.count as usize).min(total_sectors);
        if let Some(slots) = metadata_map.get_mut(start..end) {
            slots.fill(Some(entry.data_id));
        }
    }

    metadata_map
}


pub fn verify_disc_integrity<A, D, E, H>(
    manifest: &DiscManifest<H>,
    exception_index: &[u64],
    exception_blob: &[u8],
    subheader_data: &[[u8; 8]],
    veri_hash: &[u8; 64],
    get_data_chunks: D,
    get_audio_blocks: A,
    //mut progress_cb: P
) -> Result<(), SpriteShrinkCDError>
where
    A: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    D: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
    H: Hashable,
    //P: FnMut(u64) + Sync + Send + 'static,
{
    /*A batch size of 64 provides a good balance between reducing the overhead
    of the get_chunk_data callback and keeping memory usage low per
    verification task.*/
    const BATCH_SIZE: usize = 64;

    let (data_tx, data_rx) = bounded(4);
    let (audio_tx, audio_rx) = bounded(4);

    let data_layout = Arc::new(manifest.data_stream_layout.clone());
    let audio_blocks = Arc::new(manifest.audio_block_map.clone());

    let chunk_hashes: Vec<H> = data_layout.iter().map(|data| {
        data.hash
    }).collect();

    let audio_hashes: Vec<H> = audio_blocks.iter().map(|block|{
        block.content_hash
    }).collect();

    let data_fetch_handle = spawn_data_fetcher(
        chunk_hashes,
        get_data_chunks,
        data_tx,
        BATCH_SIZE
    );

    let audio_fetch_handle = spawn_data_fetcher(
        audio_hashes,
        get_audio_blocks,
        audio_tx,
        BATCH_SIZE
    );

    let expanded_sector_types = expand_sector_map(&manifest.rle_sector_map);
    let total_sectors = expanded_sector_types.len();

    let subheader_map = expand_subheader_map(
        &manifest.subheader_index,
        total_sectors
    );
    let expand_disc_excep_idx = expand_exception_index(
        &manifest.disc_exception_index,
        total_sectors
    )?;
    let mut msf_offsets = MsfOffsets::new(&manifest.lba_map);

    let mut hasher = Sha512::new();
    let mut data_buffer: VecDeque<Vec<u8>> = VecDeque::new();
    let mut current_data_buf_size = 0usize;
    let mut audio_buffer:VecDeque<Vec<u8>> = VecDeque::new();
    let mut current_audio_buf_size = 0usize;

    for (i, sector_type) in expanded_sector_types.iter().enumerate() {
        let sector_idx = i as u32;

        let result = match sector_type {
            SectorType::Audio | SectorType::PregapAudio |
            SectorType::ZeroedAudio => {
                let mut reconstructed_sector = [0u8; 2352];
                while current_audio_buf_size < 2352 {
                    let batch = audio_rx
                        .recv()
                        .map_err(|_| ReconstructionError::InternalError(
                            "Audio fetcher failed".to_string()
                        ))??;
                    for chunk in batch {
                        current_audio_buf_size += chunk.len();
                        audio_buffer.push_back(chunk);
                    }

                }
                let audio_data = drain_buffer(&mut audio_buffer, 2352);
                current_audio_buf_size -= 2352;
                reconstructed_sector.copy_from_slice(&audio_data);
                Ok(reconstructed_sector)
            }
            _ => {
                let needed = sector_type.data_size();

                while current_data_buf_size < needed as usize {
                    let batch = data_rx
                        .recv()
                        .map_err(|_| ReconstructionError::InternalError(
                            "Data fetcher failed".to_string()
                        ))??;
                    for chunk in batch {
                        current_data_buf_size += chunk.len();
                        data_buffer.push_back(chunk);
                    }

                }
                let user_data = drain_buffer(
                    &mut data_buffer,
                    needed as usize
                );
                current_data_buf_size -= needed as usize;

                // `transpose` turns the `Option<Result>` from the lookup
                // into a `Result<Option>`, so `?` can return the error.
                let exception = expand_disc_excep_idx.get(i).copied().flatten()
                    .map(|excep_id| exception_data_for(
                        exception_index,
                        exception_blob,
                        excep_id,
                        sector_type.excep_size()
                    ))
                    .transpose()?;

                let data_id = subheader_map.get(i).copied().flatten()
                    .ok_or(ReconstructionError::MissingSubheader(i))?;
                let metadata = subheader_for(subheader_data, data_id)?;

                rebuild_sector_simd(
                    &user_data,
                    metadata,
                    *sector_type,
                    msf_offsets.sector_num(sector_idx),
                    exception,
                )
            }
        };

        let reconstructed_sector = result.map_err(|e| {
            SpriteShrinkCDError::Reconstruction(e)
        })?;

        hasher.update(reconstructed_sector);
        //progress_cb(2352);

    }

    data_fetch_handle.join().map_err(|_| ReconstructionError::ThreadPanic(
        "Chunk fetching thread panicked.".to_string()))?;

    audio_fetch_handle.join().map_err(|_| ReconstructionError::ThreadPanic(
        "Audio fetching thread panicked.".to_string()))?;

    let calculated_hash: [u8; 64] = hasher.finalize().into();

    if calculated_hash.as_slice() == veri_hash {
        Ok(())
    } else {
        let orig_hash_string: String = veri_hash.iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        let calc_hash_string: String = calculated_hash.iter()
            .map(|b| format!("{:02x}", b))
            .collect();

        Err(ReconstructionError::HashMismatchError{
            orig_hash: orig_hash_string,
            calc_hash: calc_hash_string
        }.into())
    }
}


fn drain_buffer(buf: &mut VecDeque<Vec<u8>>, n: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(n);
    while result.len() < n {
        let mut head = buf.pop_front().expect("Buffer underflow");
        let remaining_needed = n - result.len();

        if head.len() <= remaining_needed {
            result.extend(head);
        } else {
            result.extend(head.drain(..remaining_needed));
            buf.push_front(head);
        }
    }
    result
}


fn rebuild_mode1_sec(
    sector_buf: &mut [u8],
    user_data: &[u8],
    metadata_bytes: &[u8; 8],
    sector_num: u32,
) -> Result<(), ReconstructionError> {
    if user_data.len() != 2048 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode1".to_string(),
            expected: 2048,
            actual: user_data.len()
        });
    }
    sector_buf[0..12].copy_from_slice(&SYNC_PATTERN);

    let msf_time: MsfTime = MsfTime::from_total_frames(sector_num);
    sector_buf[12] = to_bcd(msf_time.minute);
    sector_buf[13] = to_bcd(msf_time.second);
    sector_buf[14] = to_bcd(msf_time.frame);

    sector_buf[15] = 0x01; //mode1 byte
    sector_buf[16..2064].copy_from_slice(user_data);
    let edc_bytes = calculate_edc(
        sector_buf,
        SectorType::Mode1
    );
    sector_buf[2064..2068].copy_from_slice(&edc_bytes);
    sector_buf[2068..2076].copy_from_slice(metadata_bytes);

    Ok(())
}


fn rebuild_mode1_sec_excep(
    sector_buf: &mut [u8],
    user_data: &[u8],
    metadata_bytes: &[u8; 8],
    exception_data: Option<&[u8]>
) -> Result<(), ReconstructionError> {
    if user_data.len() != 2048 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode1Exception".to_string(),
            expected: 2048,
            actual: user_data.len()
        });
    }

    let exception_data = exception_data
        .ok_or(ReconstructionError::MissingExceptionData)?;
    if exception_data.len() != 296 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode1".to_string(),
            expected: 296,
            actual: exception_data.len()
        });
    }

    //Copy the Sync and header bytes
    sector_buf[0..16].copy_from_slice(&exception_data[..16]);
    sector_buf[16..2064].copy_from_slice(user_data);
    //Copy the EDC bytes
    sector_buf[2064..2068].copy_from_slice(&exception_data[16..20]);
    //Copy the unused header bytes
    sector_buf[2068..2076].copy_from_slice(metadata_bytes);
    //Copy the the ECC bytes
    sector_buf[2076..].copy_from_slice(&exception_data[20..]);

    Ok(())
}


fn rebuild_mode2form1_sec(
    sector_buf: &mut [u8],
    user_data: &[u8],
    metadata_bytes: &[u8; 8],
) -> Result<(), ReconstructionError> {
    if user_data.len() != 2048 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode2Form1".to_string(),
            expected: 2048,
            actual: user_data.len()
        });
    }

    sector_buf[0..12].copy_from_slice(&SYNC_PATTERN);



    sector_buf[16..24].copy_from_slice(metadata_bytes);
    sector_buf[24..2072].copy_from_slice(user_data);
    let edc_bytes = calculate_edc(
        sector_buf,
        SectorType::Mode2Form1
    );
    sector_buf[2072..2076].copy_from_slice(&edc_bytes);

    Ok(())
}


fn rebuild_mode2form1_sec_excep(
    sector_buf: &mut [u8],
    user_data: &[u8],
    metadata_bytes: &[u8; 8],
    exception_data: Option<&[u8]>
) -> Result<(), ReconstructionError> {
    if user_data.len() != 2048 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode2Form1Exception".to_string(),
            expected: 2048,
            actual: user_data.len()
        });
    }

    let exception_data = exception_data
        .ok_or(ReconstructionError::MissingExceptionData)?;
    if exception_data.len() != 296 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode2Form1Exception".to_string(),
            expected: 296,
            actual: exception_data.len()
        });
    }

    //Sync and header bytes
    sector_buf[0..16].copy_from_slice(&exception_data[..16]);
    //Subheader bytes
    sector_buf[16..24].copy_from_slice(metadata_bytes);
    sector_buf[24..2072].copy_from_slice(user_data);
    //EDC and ECC bytes
    sector_buf[2072..].copy_from_slice(&exception_data[16..]);

    Ok(())
}


fn rebuild_mode2form2_sec(
    sector_buf: &mut [u8],
    user_data: &[u8],
    metadata_bytes: &[u8; 8],
    sector_num: u32,
) -> Result<(), ReconstructionError> {
    if user_data.len() != 2324 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode2Form2".to_string(),
            expected: 2324,
            actual: user_data.len()
        });
    }

    sector_buf[0..12].copy_from_slice(&SYNC_PATTERN);

    let msf_time: MsfTime = MsfTime::from_total_frames(sector_num);
    sector_buf[12] = to_bcd(msf_time.minute);
    sector_buf[13] = to_bcd(msf_time.second);
    sector_buf[14] = to_bcd(msf_time.frame);

    sector_buf[15] = 0x02; //mode2 byte
    //Subheader bytes
    sector_buf[16..24].copy_from_slice(metadata_bytes);
    sector_buf[24..2348].copy_from_slice(user_data);
    let edc_bytes = calculate_edc(
        sector_buf,
        SectorType::Mode2Form2
    );
    sector_buf[2348..].copy_from_slice(&edc_bytes);



    Ok(())
}


fn rebuild_mode2form2_sec_excep(
    sector_buf: &mut [u8],
    user_data: &[u8],
    metadata_bytes: &[u8; 8],
    exception_data: Option<&[u8]>
) -> Result<(), ReconstructionError> {
    if user_data.len() != 2324 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode2Form2Exception".to_string(),
            expected: 2324,
            actual: user_data.len()
        });
    }
    let exception_data = exception_data
        .ok_or(ReconstructionError::MissingExceptionData)?;
    if exception_data.len() != 20 {
        return Err(ReconstructionError::DataLengthMismatch {
            sector_type: "Mode2Form2Exception".to_string(),
            expected: 20,
            actual: exception_data.len()
        });
    }

    //Sync and header bytes
    sector_buf[0..16].copy_from_slice(&exception_data[..16]);
    //Subheader bytes
    sector_buf[16..24].copy_from_slice(metadata_bytes);
    sector_buf[24..2348].copy_from_slice(user_data);
    //EDC and ECC bytes
    sector_buf[2348..].copy_from_slice(&exception_data[16..]);

    Ok(())
}


pub fn rebuild_sector_bitwise(
    user_data: &[u8],
    metadata_bytes: &[u8; 8], //reserved or subheader bytes
    sector_type: SectorType,
    sector_num: u32,
    exception_data: Option<&[u8]>,
) -> Result<[u8; 2352], ReconstructionError> {
    let mut sector_buf = [0u8; 2352];

    match sector_type {
        SectorType::Mode1 | SectorType::PregapMode1 => {
            rebuild_mode1_sec(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                sector_num
            )?;

            calc_ecc_bitwise(&mut sector_buf);
        }
        SectorType::Mode1Exception | SectorType::PregapMode1Exception => {
            rebuild_mode1_sec_excep(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                exception_data
            )?;
        }
        SectorType::Mode2Form1 | SectorType::PregapMode2 => {
            rebuild_mode2form1_sec(
                &mut sector_buf,
                user_data,
                metadata_bytes,
            )?;

            calc_ecc_bitwise(&mut sector_buf);

            let msf_time: MsfTime = MsfTime::from_total_frames(sector_num);
            sector_buf[12] = to_bcd(msf_time.minute);
            sector_buf[13] = to_bcd(msf_time.second);
            sector_buf[14] = to_bcd(msf_time.frame);
            sector_buf[15] = 0x02; //mode2 byte
        }
        SectorType::Mode2Form1Exception | SectorType::PregapMode2Exception => {
            rebuild_mode2form1_sec_excep(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                exception_data
            )?;
        }
        SectorType::Mode2Form2 => {
            rebuild_mode2form2_sec(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                sector_num
            )?;
            //No P/Q bytes for Form2
        }
        SectorType::Mode2Form2Exception => {
            rebuild_mode2form2_sec_excep(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                exception_data
            )?;
        }
        SectorType::ZeroedMode1Data | SectorType::ZeroedMode2Data => {
            if user_data.len() != 2352 {
                return Err(ReconstructionError::DataLengthMismatch {
                    sector_type: "ZeroedData".to_string(),
                    expected: 2352,
                    actual: user_data.len()
                });
            }
            sector_buf.copy_from_slice(user_data);
        }
        _ => return Err(ReconstructionError::UnsupportedType(sector_type)),
    };

    Ok(sector_buf)
}


pub fn rebuild_sector_simd(
    user_data: &[u8],
    metadata_bytes: &[u8; 8], //reserved or subheader bytes
    sector_type: SectorType,
    sector_num: u32,
    exception_data: Option<&[u8]>,
) -> Result<[u8; 2352], ReconstructionError> {
    let mut sector_buf = [0u8; 2352];

    match sector_type {
        SectorType::Mode1 | SectorType::PregapMode1 => {
            rebuild_mode1_sec(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                sector_num
            )?;

            calc_ecc_simd_inplace(&mut sector_buf);
        }
        SectorType::Mode1Exception | SectorType::PregapMode1Exception => {
            rebuild_mode1_sec_excep(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                exception_data
            )?;
        }
        SectorType::Mode2Form1 | SectorType::PregapMode2 => {
            rebuild_mode2form1_sec(
                &mut sector_buf,
                user_data,
                metadata_bytes,
            )?;

            calc_ecc_simd_inplace(&mut sector_buf);

            let msf_time: MsfTime = MsfTime::from_total_frames(sector_num);
            sector_buf[12] = to_bcd(msf_time.minute);
            sector_buf[13] = to_bcd(msf_time.second);
            sector_buf[14] = to_bcd(msf_time.frame);
            sector_buf[15] = 0x02; //mode2 byte
        }
        SectorType::Mode2Form1Exception | SectorType::PregapMode2Exception => {
            rebuild_mode2form1_sec_excep(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                exception_data
            )?;
        }
        SectorType::Mode2Form2 => {
            rebuild_mode2form2_sec(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                sector_num
            )?;
            //No P/Q bytes for Form2
        }
        SectorType::Mode2Form2Exception => {
            rebuild_mode2form2_sec_excep(
                &mut sector_buf,
                user_data,
                metadata_bytes,
                exception_data
            )?;
        }
        SectorType::ZeroedMode1Data | SectorType::ZeroedMode2Data => {
            if user_data.len() != 2352 {
                return Err(ReconstructionError::DataLengthMismatch {
                    sector_type: "ZeroedData".to_string(),
                    expected: 2352,
                    actual: user_data.len()
                });
            }
            sector_buf.copy_from_slice(user_data);
        }
        _ => return Err(ReconstructionError::UnsupportedType(sector_type)),
    };

    Ok(sector_buf)
}


const fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}



/// Rebuilds the disc `manifest` describes from the archive and writes its
/// raw image to `writer`, checking it against the manifest's integrity hash.
///
/// `is_cancelled` is called before each sector. Once it returns `true`,
/// writing stops and the error is a `SpriteShrinkError::Cancelled`, for which
/// `is_cancelled()` holds. Pass `|| false` to never cancel.
///
/// # Errors
///
/// Returns an error if a chunk or audio block can't be read or decompressed,
/// a sector can't be rebuilt, writing fails, the rebuilt disc doesn't match
/// the integrity hash, or `is_cancelled` returns `true`. `writer` may hold
/// part of the disc after an error.
#[expect(
    clippy::too_many_arguments,
    reason = "each archive section is a separate borrow; grouping them is \
              an API change of its own"
)]
pub fn write_disc<H, W, C>(
    manifest: &DiscManifest<H>,
    indices: &SSMDIndices<H>,
    dictionary: &[u8],
    archive_file: &File,
    exception_blob: &[u8],
    subheader_table: &[[u8; 8]],
    writer: &mut BufWriter<W>,
    is_cancelled: C,
    //mut progress_cb: P
) -> Result<(), SpriteShrinkCDError>
where
    H: Hashable + Display,
    W: Write,
    C: Fn() -> bool,
{
    const MAX_BUFFER_SIZE: usize = 25 * 1024 * 1024; //25 megabytes

    let (data_tx, data_rx) = bounded(4);
    let (audio_tx, audio_rx) = bounded(4);

    let chunk_hashes: Vec<H> = manifest.data_stream_layout.iter().map(|data| {
        data.hash
    }).collect();

    let audio_hashes: Vec<H> = manifest.audio_block_map.iter().map(|block| {
        block.content_hash
    }).collect();

    let expanded_sector_types = expand_sector_map(&manifest.rle_sector_map);
    let total_sectors = expanded_sector_types.len();

    let subheader_map = expand_subheader_map(
        &manifest.subheader_index,
        total_sectors
    );
    let expand_disc_excep_idx = expand_exception_index(
        &manifest.disc_exception_index,
        total_sectors
    )?;

    let mut hasher = Xxh3::new();

    let mut loop_count = 0usize;

    let mut msf_offsets = MsfOffsets::new(&manifest.lba_map);

    let shared_data_buffer: SharedBuffer = Arc::new((
        Mutex::new(BufferState::with_capacity(MAX_BUFFER_SIZE)),
        Condvar::new(),
    ));

    let shared_audio_buffer: SharedBuffer = Arc::new((
        Mutex::new(BufferState::with_capacity(MAX_BUFFER_SIZE)),
        Condvar::new(),
    ));

    let cloned_data_buffer =  shared_data_buffer.clone();
    let cloned_audio_buffer =  shared_audio_buffer.clone();

    scope(|scope| -> Result<(), SpriteShrinkCDError>{
        /*Dropped on any early return from this closure, which is still
        before `scope` joins the workers.*/
        let consumer_guard = ConsumerGuard {
            buffers: [&shared_data_buffer, &shared_audio_buffer],
        };

        let data_fetch_handle = spawn_chunk_decomp_worker(
            scope,
            chunk_hashes,
            &indices.chunk_index,
            dictionary,
            archive_file,
            cloned_data_buffer,
            MAX_BUFFER_SIZE,
            data_tx
        );

        let audio_fetch_handle = spawn_audio_decomp_worker(
            scope,
            audio_hashes,
            &indices.audio_block_index,
            archive_file,
            cloned_audio_buffer,
            MAX_BUFFER_SIZE,
            audio_tx
        );

        for (i, sector_type) in expanded_sector_types.iter().enumerate() {
            if is_cancelled() {
                return Err(SpriteShrinkError::Cancelled.into());
            }

            let sector_idx = i as u32;

            let sector_result = match sector_type {
                SectorType::Audio | SectorType::PregapAudio |
                SectorType::ZeroedAudio => {
                    let mut reconstructed_sector = [0u8; 2352];

                    if let Ok(err) = audio_rx.try_recv() {
                        return Err(err);
                    }

                    let audio_data = pull_data(
                        &shared_audio_buffer,
                        2352,
                        &audio_rx
                    )?;

                    if audio_data.len() != 2352 {

                        return Err(ReconstructionError::InternalError(
                            format!("Audio stream ended prematurely. Expected 2352 bytes, got {}", audio_data.len())
                        ).into());
                    }

                    reconstructed_sector.copy_from_slice(&audio_data);
                    Ok(reconstructed_sector)
                }
                _ => {
                    let needed = sector_type.data_size() as usize;

                    if let Ok(err) = data_rx.try_recv() {
                        return Err(err);
                    }

                    let user_data = pull_data(
                        &shared_data_buffer,
                        needed,
                        &data_rx
                    )?;

                    if user_data.len() != sector_type.data_size() as usize {
                        return Err(ReconstructionError::InternalError(
                            format!("Data stream ended prematurely. Expected {} bytes, got {}", sector_type.data_size(), user_data.len())
                        ).into());
                    }

                    let exception = expand_disc_excep_idx.get(i).copied().flatten()
                        .map(|excep_id| exception_data_for(
                            &indices.exception_index,
                            exception_blob,
                            excep_id,
                            sector_type.excep_size()
                        ))
                        .transpose()?;

                    let data_id = subheader_map.get(i).copied().flatten()
                        .ok_or(ReconstructionError::MissingSubheader(i))?;
                    let metadata = subheader_for(subheader_table, data_id)?;

                    rebuild_sector_simd(
                        &user_data,
                        metadata,
                        *sector_type,
                        msf_offsets.sector_num(sector_idx),
                        exception,
                    )
                }
            };

            let reconstructed_sector = sector_result.map_err(|e| {
                SpriteShrinkCDError::Reconstruction(e)
            })?;

            hasher.update(&reconstructed_sector);

            writer.write_all(&reconstructed_sector)?;

            loop_count += 1;
        }

        /*Every sector is written, so stop the workers before joining them.
        One with data left over, from a manifest that lists more than the
        sectors use, would otherwise wait for room forever.*/
        drop(consumer_guard);

        data_fetch_handle.join().map_err(|_| ReconstructionError::ThreadPanic(
            "Chunk fetching thread panicked.".to_string()))?;

        audio_fetch_handle.join().map_err(|_| ReconstructionError::ThreadPanic(
            "Audio fetching thread panicked.".to_string()))?;

        Ok(())
    })?;

    let calculated_hash = hasher.finish();

    if calculated_hash == manifest.integrity_hash {
        writer.flush()?;

        Ok(())
    } else {
        let orig_hash_string: String = format!(
            "{:02x}",
            manifest.integrity_hash
        );
        let calc_hash_string: String = format!(
            "{:02x}",
            calculated_hash
        );

        Err(ReconstructionError::HashMismatchError{
            orig_hash: orig_hash_string,
            calc_hash: calc_hash_string
        }.into())
    }
}


#[cfg(test)]
mod tests {
    use proptest::{prelude::*, sample::Index};

    use super::*;
    use crate::{
        analyze::{AnalysisError, analyze_data_sector},
        lib_structs::{ContentBlock, DataChunkLayout},
        mapper::from_bcd,
    };

    const SECTOR_SIZE: usize = 2352;

    /// First frame of the program area in MSF terms (00:02:00). The rebuild
    /// functions take this absolute frame count, not a 0-based LBA, because
    /// the mapper derives its offsets from the MSF header.
    const LEAD_IN_FRAMES: u32 = 150;

    /// Last frame an MSF header can hold (99:59:74).
    const MAX_FRAME: u32 = 100 * 60 * 75 - 1;

    /// Mode 2 subheader bit that marks a Form 2 sector (submode byte 18).
    const FORM2_BIT: u8 = 0x20;

    #[derive(Clone, Copy, Debug)]
    enum Layout {
        Mode1,
        Mode2Form1,
        Mode2Form2,
    }

    impl Layout {
        const ALL: [Layout; 3] =
            [Layout::Mode1, Layout::Mode2Form1, Layout::Mode2Form2];

        fn user_data_len(self) -> usize {
            match self {
                Layout::Mode1 | Layout::Mode2Form1 => 2048,
                Layout::Mode2Form2 => 2324,
            }
        }

        /// Bytes an analysis turns into exception data when the EDC or ECC
        /// doesn't match.
        fn check_bytes(self) -> Vec<usize> {
            match self {
                Layout::Mode1 => (2064..2068).chain(2076..SECTOR_SIZE).collect(),
                Layout::Mode2Form1 => (2072..SECTOR_SIZE).collect(),
                Layout::Mode2Form2 => (2348..SECTOR_SIZE).collect(),
            }
        }
    }

    fn msf_bcd(sector_num: u32) -> [u8; 3] {
        let msf = MsfTime::from_total_frames(sector_num);
        [to_bcd(msf.minute), to_bcd(msf.second), to_bcd(msf.frame)]
    }

    /// Builds a valid sector field by field from the ECMA-130 layout, without
    /// going through the rebuild functions under test. `metadata` is the
    /// Mode 1 reserved area or the Mode 2 subheader; for Mode 2 its form bit
    /// is set to match `layout`.
    fn valid_sector(
        layout: Layout,
        sector_num: u32,
        user_data: &[u8],
        metadata: [u8; 8],
    ) -> [u8; SECTOR_SIZE] {
        let mut sector = [0_u8; SECTOR_SIZE];
        sector[..12].copy_from_slice(&SYNC_PATTERN);
        let header = msf_bcd(sector_num);

        match layout {
            Layout::Mode1 => {
                sector[12..15].copy_from_slice(&header);
                sector[15] = 1;
                sector[16..2064].copy_from_slice(user_data);
                let edc = calculate_edc(&sector, SectorType::Mode1);
                sector[2064..2068].copy_from_slice(&edc);
                sector[2068..2076].copy_from_slice(&metadata);
                calc_ecc_bitwise(&mut sector);
            }
            Layout::Mode2Form1 => {
                let mut subheader = metadata;
                subheader[2] &= !FORM2_BIT;
                sector[16..24].copy_from_slice(&subheader);
                sector[24..2072].copy_from_slice(user_data);
                let edc = calculate_edc(&sector, SectorType::Mode2Form1);
                sector[2072..2076].copy_from_slice(&edc);
                // Form 1 parity is computed with the header zeroed.
                calc_ecc_bitwise(&mut sector);
                sector[12..15].copy_from_slice(&header);
                sector[15] = 2;
            }
            Layout::Mode2Form2 => {
                sector[12..15].copy_from_slice(&header);
                sector[15] = 2;
                let mut subheader = metadata;
                subheader[2] |= FORM2_BIT;
                sector[16..24].copy_from_slice(&subheader);
                sector[24..2348].copy_from_slice(user_data);
                let edc = calculate_edc(&sector, SectorType::Mode2Form2);
                sector[2348..].copy_from_slice(&edc);
            }
        }

        sector
    }

    /// The bytes the stream stores for each sector type (as in
    /// `UserDataStream`).
    fn user_data_of(sector: &[u8; SECTOR_SIZE], sector_type: SectorType) -> &[u8] {
        match sector_type {
            SectorType::Mode1 | SectorType::Mode1Exception
            | SectorType::PregapMode1 | SectorType::PregapMode1Exception =>
                &sector[16..2064],
            SectorType::Mode2Form1 | SectorType::Mode2Form1Exception
            | SectorType::PregapMode2 | SectorType::PregapMode2Exception =>
                &sector[24..2072],
            SectorType::Mode2Form2 | SectorType::Mode2Form2Exception =>
                &sector[24..2348],
            SectorType::ZeroedMode1Data | SectorType::ZeroedMode2Data =>
                &sector[..],
            other => panic!("{other:?} has no user data"),
        }
    }

    /// The 8 bytes the rebuild functions take as `metadata_bytes`: the
    /// reserved area for Mode 1, the subheader for Mode 2.
    fn metadata_of(sector: &[u8; SECTOR_SIZE], sector_type: SectorType) -> [u8; 8] {
        let range = match sector_type {
            SectorType::Mode1 | SectorType::Mode1Exception
            | SectorType::PregapMode1 | SectorType::PregapMode1Exception =>
                2068..2076,
            _ => 16..24,
        };
        sector[range].try_into().unwrap()
    }

    /// Analyzes `raw`, checks the type and the exception data size, then
    /// rebuilds it both ways and checks both give `raw` back.
    fn assert_round_trip(
        raw: &[u8; SECTOR_SIZE],
        hint: SectorType,
        sector_num: u32,
        expected: SectorType,
    ) {
        let analysis = analyze_data_sector(raw, hint).unwrap();
        assert_eq!(analysis.sector_type, expected, "hint {hint:?}");

        let exception = analysis.exception_data.as_deref();
        assert_eq!(
            exception.map_or(0, <[u8]>::len),
            expected.excep_size(),
            "{expected:?} exception data size"
        );

        let user_data = user_data_of(raw, expected);
        let metadata = metadata_of(raw, expected);

        let bitwise = rebuild_sector_bitwise(
            user_data, &metadata, expected, sector_num, exception
        ).unwrap();
        assert!(bitwise == *raw, "{expected:?}: bitwise rebuild differs");

        let simd = rebuild_sector_simd(
            user_data, &metadata, expected, sector_num, exception
        ).unwrap();
        assert!(simd == *raw, "{expected:?}: SIMD rebuild differs");
    }

    fn sector_num() -> impl Strategy<Value = u32> {
        LEAD_IN_FRAMES..=MAX_FRAME
    }

    fn user_data(layout: Layout) -> impl Strategy<Value = Vec<u8>> {
        proptest::collection::vec(any::<u8>(), layout.user_data_len())
    }

    fn layout_and_data() -> impl Strategy<Value = (Layout, Vec<u8>)> {
        proptest::sample::select(Layout::ALL.to_vec())
            .prop_flat_map(|layout| (Just(layout), user_data(layout)))
    }

    /// Expected analysis of a valid sector of `layout` under each hint the
    /// mapper passes for that track type.
    fn valid_cases(layout: Layout) -> &'static [(SectorType, SectorType)] {
        match layout {
            Layout::Mode1 => &[
                (SectorType::Mode1, SectorType::Mode1),
                (SectorType::PregapMode1, SectorType::PregapMode1),
            ],
            Layout::Mode2Form1 => &[
                (SectorType::Mode2Form1, SectorType::Mode2Form1),
                (SectorType::PregapMode2, SectorType::PregapMode2),
            ],
            Layout::Mode2Form2 => &[
                (SectorType::Mode2Form1, SectorType::Mode2Form2),
                (SectorType::PregapMode2, SectorType::Mode2Form2),
            ],
        }
    }

    /// Like `valid_cases`, for a sector whose EDC or ECC doesn't match.
    fn exception_cases(layout: Layout) -> &'static [(SectorType, SectorType)] {
        match layout {
            Layout::Mode1 => &[
                (SectorType::Mode1, SectorType::Mode1Exception),
                (SectorType::PregapMode1, SectorType::PregapMode1Exception),
            ],
            Layout::Mode2Form1 => &[
                (SectorType::Mode2Form1, SectorType::Mode2Form1Exception),
                (SectorType::PregapMode2, SectorType::PregapMode2Exception),
            ],
            Layout::Mode2Form2 => &[
                (SectorType::Mode2Form1, SectorType::Mode2Form2Exception),
                (SectorType::PregapMode2, SectorType::Mode2Form2Exception),
            ],
        }
    }

    proptest! {
        #[test]
        fn a_valid_sector_rebuilds_to_the_same_bytes(
            (layout, data) in layout_and_data(),
            metadata in any::<[u8; 8]>(),
            sector_num in sector_num(),
        ) {
            let raw = valid_sector(layout, sector_num, &data, metadata);
            for &(hint, expected) in valid_cases(layout) {
                assert_round_trip(&raw, hint, sector_num, expected);
            }
        }

        #[test]
        fn a_sector_with_a_bad_edc_or_ecc_rebuilds_from_its_exception_data(
            (layout, data) in layout_and_data(),
            metadata in any::<[u8; 8]>(),
            sector_num in sector_num(),
            position in any::<Index>(),
            flip in 1..=u8::MAX,
        ) {
            // libcrypt protection is a valid sector with
            // deliberately wrong check bytes.
            let mut raw = valid_sector(layout, sector_num, &data, metadata);
            raw[*position.get(&layout.check_bytes())] ^= flip;

            for &(hint, expected) in exception_cases(layout) {
                assert_round_trip(&raw, hint, sector_num, expected);
            }
        }

        #[test]
        fn a_form2_sector_with_a_zero_edc_is_an_exception(
            data in user_data(Layout::Mode2Form2),
            metadata in any::<[u8; 8]>(),
            sector_num in sector_num(),
        ) {
            // Form 2 EDC is optional. Some older PS1 discs leave it zeroed
            // across their Form 2 sectors, while later discs store valid EDC.
            let mut raw = valid_sector(
                Layout::Mode2Form2, sector_num, &data, metadata
            );
            prop_assume!(raw[2348..] != [0; 4]);
            raw[2348..].fill(0);

            assert_round_trip(
                &raw,
                SectorType::Mode2Form1,
                sector_num,
                SectorType::Mode2Form2Exception,
            );
        }

        #[test]
        fn a_mode_byte_of_3_or_more_is_unknown(
            mode in 3..=u8::MAX,
            data in user_data(Layout::Mode1),
        ) {
            let mut raw = valid_sector(Layout::Mode1, LEAD_IN_FRAMES, &data, [0; 8]);
            raw[15] = mode;
            let result = analyze_data_sector(&raw, SectorType::Mode1);
            prop_assert!(
                matches!(result, Err(AnalysisError::UnknownMode(m)) if m == mode)
            );
        }
    }

    #[test]
    fn an_all_zero_sector_is_zeroed_data_of_the_hinted_mode() {
        let raw = [0_u8; SECTOR_SIZE];
        for (hint, expected) in [
            (SectorType::Mode1, SectorType::ZeroedMode1Data),
            (SectorType::PregapMode1, SectorType::ZeroedMode1Data),
            (SectorType::Mode2Form1, SectorType::ZeroedMode2Data),
            (SectorType::PregapMode2, SectorType::ZeroedMode2Data),
        ] {
            assert_round_trip(&raw, hint, LEAD_IN_FRAMES, expected);
        }
    }

    #[test]
    fn mode_byte_0_with_nonzero_data_is_unknown() {
        let mut raw = [0_u8; SECTOR_SIZE];
        raw[1000] = 1;
        let result = analyze_data_sector(&raw, SectorType::Mode1);
        assert!(matches!(result, Err(AnalysisError::UnknownMode(0))));
    }

    #[test]
    fn exception_data_sizes() {
        for (sector_type, size) in [
            (SectorType::Mode1Exception, 296),
            (SectorType::Mode2Form1Exception, 296),
            (SectorType::Mode2Form2Exception, 20),
            (SectorType::PregapMode1Exception, 296),
            (SectorType::PregapMode2Exception, 296),
        ] {
            assert_eq!(sector_type.excep_size(), size, "{sector_type:?}");
        }
    }

    type Rebuild = fn(
        &[u8], &[u8; 8], SectorType, u32, Option<&[u8]>
    ) -> Result<[u8; SECTOR_SIZE], ReconstructionError>;

    const REBUILDERS: [(&str, Rebuild); 2] = [
        ("bitwise", rebuild_sector_bitwise),
        ("simd", rebuild_sector_simd),
    ];

    #[test]
    fn rebuild_rejects_user_data_of_the_wrong_length() {
        let buffer = [0_u8; SECTOR_SIZE + 1];
        let exception = [0_u8; 296];

        for (name, rebuild) in REBUILDERS {
            for sector_type in [
                SectorType::Mode1, SectorType::Mode1Exception,
                SectorType::Mode2Form1, SectorType::Mode2Form1Exception,
                SectorType::Mode2Form2, SectorType::Mode2Form2Exception,
                SectorType::PregapMode1, SectorType::PregapMode1Exception,
                SectorType::PregapMode2, SectorType::PregapMode2Exception,
                SectorType::ZeroedMode1Data, SectorType::ZeroedMode2Data,
            ] {
                let expected = usize::from(sector_type.data_size());
                let exception = &exception[..sector_type.excep_size()];
                for len in [0, expected - 1, expected + 1] {
                    let result = rebuild(
                        &buffer[..len], &[0; 8], sector_type,
                        LEAD_IN_FRAMES, Some(exception),
                    );
                    assert!(
                        matches!(
                            result,
                            Err(ReconstructionError::DataLengthMismatch {
                                expected: e, actual, ..
                            }) if e == expected && actual == len
                        ),
                        "{name} {sector_type:?} with {len} bytes"
                    );
                }
            }
        }
    }

    #[test]
    fn rebuild_rejects_types_without_a_data_layout() {
        for (name, rebuild) in REBUILDERS {
            for sector_type in [
                SectorType::Audio, SectorType::PregapAudio,
                SectorType::ZeroedAudio, SectorType::None,
            ] {
                let result = rebuild(
                    &[0; 2352], &[0; 8], sector_type, LEAD_IN_FRAMES, None
                );
                assert!(
                    matches!(
                        result,
                        Err(ReconstructionError::UnsupportedType(t))
                            if t == sector_type
                    ),
                    "{name} {sector_type:?}"
                );
            }
        }
    }

    #[test]
    fn rebuild_of_an_exception_type_needs_exception_data() {
        for (name, rebuild) in REBUILDERS {
            for sector_type in [
                SectorType::Mode1Exception,
                SectorType::Mode2Form1Exception,
                SectorType::Mode2Form2Exception,
            ] {
                let user_data = vec![0; usize::from(sector_type.data_size())];
                let result = rebuild(
                    &user_data, &[0; 8], sector_type, LEAD_IN_FRAMES, None
                );
                assert!(
                    matches!(result, Err(ReconstructionError::MissingExceptionData)),
                    "{name} {sector_type:?}"
                );
            }
        }
    }

    #[test]
    fn the_header_holds_the_msf_of_the_absolute_frame() {
        // LBA 0 is MSF 00:02:00, after the 150-frame lead-in.
        for (lba, msf) in [
            (0, [0x00, 0x02, 0x00]),
            (74, [0x00, 0x02, 0x74]),
            (75, [0x00, 0x03, 0x00]),
            (4499, [0x01, 0x01, 0x74]),
            (4500, [0x01, 0x02, 0x00]),
        ] {
            for (sector_type, mode) in [
                (SectorType::Mode1, 1),
                (SectorType::Mode2Form1, 2),
                (SectorType::Mode2Form2, 2),
            ] {
                let user_data = vec![0; usize::from(sector_type.data_size())];
                for (name, rebuild) in REBUILDERS {
                    let sector = rebuild(
                        &user_data, &[0; 8], sector_type,
                        LEAD_IN_FRAMES + lba, None,
                    ).unwrap();
                    assert_eq!(
                        sector[12..16],
                        [msf[0], msf[1], msf[2], mode],
                        "{name} {sector_type:?} LBA {lba}"
                    );
                }
            }
        }
    }

    #[test]
    fn bcd_round_trips_0_to_99() {
        for value in 0..=99 {
            let bcd = to_bcd(value);
            assert_eq!(bcd >> 4, value / 10);
            assert_eq!(bcd & 0x0F, value % 10);
            assert_eq!(from_bcd(bcd), value);
        }
    }

    #[test]
    fn stream_offsets_advance_by_the_bytes_the_data_stream_holds() {
        let rle = RleSectorMap {
            runs: vec![
                (2, SectorType::Mode1),
                (1, SectorType::Audio),
                (1, SectorType::Mode2Form2),
                (2, SectorType::PregapAudio),
                (1, SectorType::ZeroedMode1Data),
                (1, SectorType::None),
                (1, SectorType::Mode2Form1Exception),
            ],
        };
        let offsets: Vec<(SectorType, u64)> = build_decoded_map(&rle)
            .iter()
            .map(|info| (info.sector_type, info.stream_offset))
            .collect();

        assert_eq!(offsets, [
            (SectorType::Mode1, 0),
            (SectorType::Mode1, 2048),
            (SectorType::Audio, 4096),
            (SectorType::Mode2Form2, 4096),
            (SectorType::PregapAudio, 6420),
            (SectorType::PregapAudio, 6420),
            (SectorType::ZeroedMode1Data, 6420),
            (SectorType::None, 8772),
            (SectorType::Mode2Form1Exception, 8772),
        ]);
    }

    #[test]
    fn subheader_entries_past_the_disc_are_skipped() {
        let entry = |start_lba, count, data_id| SubHeaderEntry {
            start_lba, count, data_id
        };
        let map = expand_subheader_map(
            &[
                entry(0, 2, 7),
                entry(3, 4, 8),
                entry(9, 1, 9),
                entry(u32::MAX, 2, 10),
            ],
            5,
        );
        assert_eq!(map, [Some(7), Some(7), None, Some(8), Some(8)]);
    }

    #[test]
    fn exception_entries_expand_to_their_sectors() {
        let map = expand_exception_index(&[(1, 4), (3, 0)], 4).unwrap();
        assert_eq!(map, [None, Some(4), None, Some(0)]);
    }

    #[test]
    fn an_exception_entry_past_the_disc_is_an_error() {
        let result = expand_exception_index(&[(1, 0), (4, 1)], 4);
        assert!(matches!(
            result,
            Err(ReconstructionError::ExceptionSectorOutOfRange {
                sector: 4, total_sectors: 4
            })
        ));
    }

    #[test]
    fn exception_data_is_looked_up_through_the_index() {
        let blob: Vec<u8> = (0..60).collect();
        let index = [0, 20, 40];

        assert_eq!(exception_data_for(&index, &blob, 1, 20).unwrap(), &blob[20..40]);
        assert_eq!(exception_data_for(&index, &blob, 2, 20).unwrap(), &blob[40..]);
    }

    #[test]
    fn exception_lookups_with_bad_archive_values_are_errors() {
        let blob = [0_u8; 60];
        let index = [0, 50, u64::MAX];

        assert!(matches!(
            exception_data_for(&index, &blob, 3, 20),
            Err(ReconstructionError::UnknownException(3))
        ));
        assert!(matches!(
            exception_data_for(&index, &blob, 1, 20),
            Err(ReconstructionError::ExceptionDataOutOfRange {
                offset: 50, size: 20, blob_len: 60
            })
        ));
        assert!(matches!(
            exception_data_for(&index, &blob, 2, 20),
            Err(ReconstructionError::ExceptionDataOutOfRange {
                offset: u64::MAX, ..
            })
        ));
    }

    #[test]
    fn an_unknown_subheader_id_is_an_error() {
        let table = [[1_u8; 8], [2; 8]];
        assert_eq!(subheader_for(&table, 1).unwrap(), &[2; 8]);
        assert!(matches!(
            subheader_for(&table, 2),
            Err(ReconstructionError::UnknownSubheader(2))
        ));
    }

    #[test]
    fn msf_offsets_start_at_0_without_an_lba_map() {
        let mut offsets = MsfOffsets::new(&[]);
        assert_eq!(offsets.sector_num(0), 0);
        assert_eq!(offsets.sector_num(10), 10);
    }

    #[test]
    fn msf_offsets_apply_the_first_entry_from_sector_0() {
        let mut offsets = MsfOffsets::new(&[(0, LEAD_IN_FRAMES)]);
        assert_eq!(offsets.sector_num(0), LEAD_IN_FRAMES);
        assert_eq!(offsets.sector_num(500), 500 + LEAD_IN_FRAMES);
        assert_eq!(offsets.sector_num(u32::MAX), u32::MAX);
    }

    /// The fetch callback `verify_disc_integrity` takes: returns `blocks`
    /// for the hashes in `hashes`, in order.
    fn fetch_from(
        blocks: Vec<(u64, Vec<u8>)>
    ) -> impl Fn(&[u64]) -> Result<Vec<Vec<u8>>, std::io::Error> + Send + Sync + 'static {
        move |hashes| {
            hashes.iter().map(|hash| {
                blocks.iter()
                    .find(|(h, _)| h == hash)
                    .map(|(_, data)| data.clone())
                    .ok_or_else(|| std::io::Error::other("unknown hash"))
            }).collect()
        }
    }

    fn empty_manifest() -> DiscManifest<u64> {
        DiscManifest {
            files: Vec::new(),
            lba_map: Vec::new(),
            rle_sector_map: RleSectorMap { runs: Vec::new() },
            audio_block_map: Vec::new(),
            data_stream_layout: Vec::new(),
            subheader_index: Vec::new(),
            disc_exception_index: Vec::new(),
            integrity_hash: 0,
        }
    }

    #[test]
    fn an_audio_only_disc_verifies() {
        // An audio-only disc has no data sector headers, so its LBA map is
        // empty. Verification used to index `lba_map[0]` and panic.
        let audio: Vec<u8> = (0..2 * SECTOR_SIZE).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let manifest = DiscManifest {
            rle_sector_map: RleSectorMap { runs: vec![(2, SectorType::Audio)] },
            audio_block_map: vec![ContentBlock {
                start_sector: 0,
                sector_count: 2,
                content_hash: 1,
                sector_type: SectorType::Audio,
            }],
            ..empty_manifest()
        };
        let hash: [u8; 64] = Sha512::digest(&audio).into();

        verify_disc_integrity(
            &manifest, &[], &[], &[], &hash,
            fetch_from(Vec::new()),
            fetch_from(vec![(1, audio)]),
        ).unwrap();
    }

    /// Verifies a one-sector Mode 1 Exception disc whose exception index
    /// and subheader tables are given, and returns the error.
    fn verify_one_exception_sector(
        disc_exception_index: Vec<(u32, u32)>,
        exception_index: &[u64],
        subheader_id: u16,
    ) -> SpriteShrinkCDError {
        let manifest = DiscManifest {
            rle_sector_map: RleSectorMap {
                runs: vec![(1, SectorType::Mode1Exception)]
            },
            data_stream_layout: vec![DataChunkLayout { hash: 1, uncomp_len: 2048 }],
            subheader_index: vec![SubHeaderEntry {
                start_lba: 0, count: 1, data_id: subheader_id
            }],
            disc_exception_index,
            ..empty_manifest()
        };

        verify_disc_integrity(
            &manifest, exception_index, &[0; 296], &[[0; 8]], &[0; 64],
            fetch_from(vec![(1, vec![0; 2048])]),
            fetch_from(Vec::new()),
        ).unwrap_err()
    }

    #[test]
    fn verification_reports_corrupt_exception_and_subheader_ids() {
        assert!(matches!(
            verify_one_exception_sector(vec![(1, 0)], &[0], 0),
            SpriteShrinkCDError::Reconstruction(
                ReconstructionError::ExceptionSectorOutOfRange { sector: 1, .. }
            )
        ));
        assert!(matches!(
            verify_one_exception_sector(vec![(0, 5)], &[0], 0),
            SpriteShrinkCDError::Reconstruction(
                ReconstructionError::UnknownException(5)
            )
        ));
        assert!(matches!(
            verify_one_exception_sector(vec![(0, 0)], &[100], 0),
            SpriteShrinkCDError::Reconstruction(
                ReconstructionError::ExceptionDataOutOfRange { offset: 100, .. }
            )
        ));
        assert!(matches!(
            verify_one_exception_sector(vec![(0, 0)], &[0], 3),
            SpriteShrinkCDError::Reconstruction(
                ReconstructionError::UnknownSubheader(3)
            )
        ));
        // The same disc with valid tables gets as far as the hash check.
        assert!(matches!(
            verify_one_exception_sector(vec![(0, 0)], &[0], 0),
            SpriteShrinkCDError::Reconstruction(
                ReconstructionError::HashMismatchError { .. }
            )
        ));
    }
}
