use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use crate::{
    analyze::SYNC_PATTERN,
    ecc::{calc_ecc_simd_inplace, calc_ecc_bitwise, calculate_edc},
    lib_error_handling::SpriteShrinkCDError,
    lib_structs::{
        DecodedSectorInfo, DiscManifest, ExceptionInfo, MsfTime,
        ReconstructionContext, ReconstructionInfo, RleSectorMap, SectorType,
        StreamChunkInfo
    },
    util::{spawn_data_fetcher},
};

use flume::{
    bounded,
};
use sha2::{Digest,Sha512};
use sprite_shrink::{
    Hashable
};
use thiserror::Error;


#[derive(Error, Debug)]
pub enum ReconstructionError {
    #[error("ECC Reconstruction failed at sector {sector_idx}: {source}")]
    EccError { sector_idx: u32, source: crate::ecc::EccError },

    #[error("Verification failed for file:  {title}\n
        Original Hash: {orig_hash}\n
        Calculated Hash: {calc_hash}")]
    HashMismatchError{title: String, orig_hash: String, calc_hash: String},

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

    #[error("Reconstruction failure.")]
    ReconstructionFailure,
}


impl<H: Copy + Eq> DiscManifest<H> {
    fn build_stream_info(
        &self,
        sector_info: &DecodedSectorInfo
    ) -> Option<ReconstructionInfo<H>> {
        let target_stream_offset = sector_info.stream_offset;

        let target_user_data_len = sector_info.sector_type.data_size();


        if target_user_data_len == 0 { return None; }

        let mut needed_chunks = Vec::new();
        let mut bytes_remaining_in_sector = target_user_data_len;
        let mut current_search_offset = target_stream_offset;
        let mut chunk_stream_offset = 0u64;

        for chunk_layout in &self.data_stream_layout {
            let chunk_start = chunk_stream_offset;
            let chunk_end = chunk_stream_offset + chunk_layout.uncomp_len as u64;

            if chunk_start < (
                current_search_offset + bytes_remaining_in_sector as u64
            ) && chunk_end > current_search_offset {
                let overlap_start_in_stream = current_search_offset
                    .max(chunk_start);

                let overlap_end_in_stream = (
                    target_stream_offset + target_user_data_len as u64
                    ).min(chunk_end);
                let overlap_len = (
                    overlap_end_in_stream - overlap_start_in_stream
                ) as u32;

                needed_chunks.push(StreamChunkInfo {
                    chunk_hash: chunk_layout.hash,
                    read_from_offset: (
                        overlap_start_in_stream - chunk_start
                    ) as u32,
                    read_length: overlap_len,
                });

                bytes_remaining_in_sector -= overlap_len as u16;
                current_search_offset += overlap_len as u64;
                if bytes_remaining_in_sector == 0 {
                    break;
                }
            }

            chunk_stream_offset = chunk_end;
        }

        if needed_chunks.is_empty() {
            None
        } else {
            Some(ReconstructionInfo::FromStream { chunks: needed_chunks })
        }
    }
}

fn build_decoded_map(rle_map: &RleSectorMap) -> Vec<DecodedSectorInfo> {
    let total_sectors: usize = rle_map.runs
        .iter()
        .map(|(count, _)| *count as usize)
        .sum();

    let mut map = Vec::with_capacity(total_sectors);
    let mut current_stream_offset = 0u64;

    for (run_count, run_type) in &rle_map.runs {
        let user_data_size = run_type.data_size() as u64;
        for _ in 0..*run_count {
            map.push(DecodedSectorInfo {
                sector_type: *run_type,
                stream_offset: current_stream_offset,
            });

            if user_data_size > 0 {
                current_stream_offset += user_data_size;
            }
        }
    }
    map
}


pub fn expand_sector_map(rle_map: &RleSectorMap) -> Vec<SectorType> {
    let mut sectors = Vec::new();
    for (count, s_type) in &rle_map.runs {
        sectors.extend(std::iter::repeat(*s_type).take(*count as usize));
    }
    sectors
}


pub fn expand_metadata_map(
    subheader_map: &[(u32, u32, [u8; 8])]
) -> HashMap<u64, [u8; 8]> {
    let mut metadata_map: HashMap<u64, [u8; 8]> = HashMap::new();

    for (start_sector, count, data) in subheader_map {
        for offset in 0..*count {
            metadata_map.insert(
                (*start_sector + offset) as u64,
                *data
            );
        }
    };

    metadata_map
}


impl<'a, H: Copy + Eq> ReconstructionContext<'a, H> {
    pub fn new(manifest: &'a DiscManifest<H>) -> Self {
        let decoded_map = build_decoded_map(&manifest.rle_sector_map);

        Self {manifest, decoded_map}
    }

    pub fn get_reconstruction_info(
        &self,
        target_sector: u32,
    ) -> Option<ReconstructionInfo<H>> {
        let sector_info = self.decoded_map.get(target_sector as usize)?;

        match sector_info.sector_type {
            SectorType::Audio | SectorType::PregapAudio => {
                for block in &self.manifest.block_map {
                    if target_sector >= block.start_sector &&
                        target_sector < (
                            block.start_sector + block.sector_count
                        )
                    {
                        let offset_in_block = (
                            target_sector - block.start_sector
                        ) * 2352;

                        return Some(ReconstructionInfo::FromBlock {
                            content_hash: block.content_hash,
                            offset_in_block,
                        });
                    }
                }
                None
            }
            SectorType::Mode1 | SectorType::Mode1Exception |
                SectorType::Mode2Form1 | SectorType::Mode2Form1Exception |
                SectorType::Mode2Form2 | SectorType::Mode2Form2Exception =>
            {
                self.manifest.build_stream_info(
                    sector_info
                )
            }
            _ => Some(ReconstructionInfo::None),
        }
    }
}


pub fn verify_disc_integrity<A, D, E, H>(
    manifest: Arc<DiscManifest<H>>,
    exception_index: &HashMap<u64, ExceptionInfo>,
    exception_blob: &[u8],
    veri_hash: &[u8; 64],
    get_data_chunks: D,
    get_audio_blocks: A,
    //mut progress_cb: P
    //original_file: &mut R,
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
    let audio_blocks = Arc::new(manifest.block_map.clone());

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

    //let recon_ctx = ReconstructionContext::new(&manifest);
    let expanded_sector_types = expand_sector_map(&manifest.rle_sector_map);
    let subheader_map = expand_metadata_map(&manifest.subheader_map);
    let lba_map = &manifest.lba_map;
    let mut current_msf_offset = lba_map[0].1;
    let mut lba_index = 0;

    println!("Starting offset = {}", current_msf_offset);

    let total_data_sectors: u32 = expanded_sector_types.iter()
        .filter(|&&t| !matches!(
            t,
            SectorType::Audio | SectorType::PregapAudio
        )).count() as u32;

    let layout_total_bytes: u64 = manifest.data_stream_layout.iter()
        .map(|c| c.uncomp_len as u64)
        .sum();


    println!("Reconstruction Sector Count: {}", total_data_sectors);
    println!("Manifest Layout Byte Count: {}", layout_total_bytes);

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

                let exception = exception_index.get(
                    &(sector_idx as u64)
                ).map(|info| {
                    let start = info.data_offset as usize;
                    let end = start + info.exception_type.metadata_size() as usize;
                    &exception_blob[start..end]
                });

                let metadata = subheader_map.get(&(i as u64))
                    .ok_or(ReconstructionError::MissingSubheader(i))?;

                if lba_map.len() - 1 > lba_index &&
                    current_msf_offset + i as u32 >= lba_map[lba_index + 1].0
                {
                    lba_index += 1;
                    current_msf_offset = lba_map[lba_index].1;
                }

                rebuild_sector_simd(
                    &user_data,
                    metadata,
                    *sector_type,
                    sector_idx + current_msf_offset,
                    exception,
                )
            }
        };

        let reconstructed_sector = result.map_err(|e| {
            SpriteShrinkCDError::Reconstruction(e)
        })?;

        /*let mut original_sector = [0u8; 2352];
        original_file.read_exact(&mut original_sector)
            .map_err(|e| SpriteShrinkCDError::Io(e))?;

        if reconstructed_sector != original_sector {
            println!("Mismatch at Sector {}", i);
            for b in 0..2352 {
                if reconstructed_sector[b] != original_sector[b] {
                    println!("First mismatch at byte {}: Expected {:02X}, Got {:02X}",
                        b, original_sector[b], reconstructed_sector[b]);
                    break;
                }
            }

            println!("Recon Sync bytes:");
            for b in 0..12 {
                print!("{:02x}", reconstructed_sector[b])
            }
            println!("\nOrig Sync bytes:");
            for b in 0..12 {
                print!("{:02x}", original_sector[b])
            }

            println!("\nRecon Header bytes:");
            for b in 12..16 {
                print!("{:02x}", reconstructed_sector[b])
            }
            println!("\nOrig Header bytes:");
            for b in 12..16 {
                print!("{:02x}", original_sector[b])
            }

            println!("\nRecon EDC bytes:");
            for b in 2072..2076 {
                print!("{:02x}", reconstructed_sector[b])
            }
            println!("\nOrig EDC bytes:");
            for b in 2072..2076 {
                print!("{:02x}", original_sector[b])
            }

            println!("\nRecon Subheader bytes:");
            for b in 16..20 {
                print!("{:02x}", reconstructed_sector[b])
            }
            println!("\nOrig Subheader bytes:");
            for b in 16..20 {
                print!("{:02x}", original_sector[b])
            }

            println!("\nRecon Sector Mode/Form {:?}", sector_type);

            println!();

            return Err(ReconstructionError::ReconstructionFailure.into())
        }*/


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
            title: manifest.title.clone(),
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
    //sector_num: u32,
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
        SectorType::ZeroedData => {
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
        SectorType::ZeroedData => {
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
