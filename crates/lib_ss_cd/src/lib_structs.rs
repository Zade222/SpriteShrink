use std::{
    collections::HashMap,
    fmt::{self, Display, Formatter},
    hash::Hash,
    marker::PhantomData,
    mem,
    ops::Range,
    sync::atomic::{AtomicU16, AtomicU32, Ordering,}
};


use crate::{
    flac::{audio_block_len, decompress_flac_block},
    lib_error_handling::SpriteShrinkCDError,
    mapper::MapperError,
    reconstruction::{
        MsfOffsets, ReconstructionError,
        build_decoded_map, exception_data_for, expand_exception_index,
        expand_sector_map, expand_subheader_map, rebuild_sector_simd,
        subheader_for
    },
    util::read_stored_at,
};

use bitcode::{Decode, Encode};
use bytemuck::{Pod, Zeroable};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sprite_shrink::{
    ChunkLocation, FileHeader, FileRegion, Hashable,
    decompress_chunk_checked
};
use zerocopy::{Immutable, IntoBytes, FromBytes};


pub const SSMD_UID: u16 = 0xd541;

/// The most sectors a disc can have: a GD-ROM's 549,150, which is more than
/// a 99-minute CD's 450,000. Archiving rejects a larger disc, and reading
/// rejects a manifest that claims more, before anything is sized from it.
pub const MAX_DISC_SECTORS: u64 = 549_150;

/// The most sectors one audio block may hold. The compressor writes 16; the
/// rest is room to change that without a format change. Decoding an audio
/// block never produces more than this many sectors of audio.
pub const MAX_AUDIO_BLOCK_SECTORS: u32 = 1024;


#[derive(Clone, Debug, Decode, Deserialize, Encode, Eq, PartialEq, Serialize)]
pub struct ContentBlock<H> {
    pub start_sector: u32,
    pub sector_count: u32,
    pub content_hash: H,
    pub sector_type: SectorType,
}

pub struct CueAnalysisResult {
    pub total_data_size: u64,
    pub cue_sheets: Vec<CueSheet>,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueFile {
    pub name: String,
    pub tracks: Vec<Track>,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueSheet {
    pub source_filename: String,
    pub files: Vec<CueFile>,
}


impl CueSheet {
    /// Converts the CueSheet struct into a String with the format of a .cue
    /// file.
    ///
    /// This is an alias for the standard `to_string()` method provided by
    /// implementing the `Display` trait.
    pub fn to_cue_string(&self) -> String {
        self.to_string()
    }
}

impl Display for CueSheet {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let tracks = || self.files.iter().flat_map(|file| &file.tracks);
        let is_gdrom = tracks()
            .any(|track| track.area == DiscArea::HighDensity);
        if is_gdrom {
            writeln!(f, "REM SINGLE-DENSITY AREA")?;
        }
        let mut area = DiscArea::SingleDensity;

        for file in &self.files {
            /*The marker goes before the FILE line when the area starts with
            the file, as Redump's sheets have it.*/
            if let Some(first) = file.tracks.first()
                && first.area != area
            {
                writeln!(f, "REM HIGH-DENSITY AREA")?;
                area = first.area;
            }
            writeln!(f, "FILE \"{}\" BINARY", file.name)?;

            for track in &file.tracks {
                if track.area != area {
                    writeln!(f, "REM HIGH-DENSITY AREA")?;
                    area = track.area;
                }
                writeln!(f, "  TRACK {:02} {}", track.number, track.track_type)?;
                if track.pregap > 0 {
                    let gap = MsfTime::from_total_frames(track.pregap);
                    writeln!(f, "    PREGAP {gap}")?;
                }
                for index in &track.indices {
                    writeln!(f, "    INDEX {:02} {}", index.number, index.position)?;
                }
                if track.postgap > 0 {
                    let gap = MsfTime::from_total_frames(track.postgap);
                    writeln!(f, "    POSTGAP {gap}")?;
                }
            }
        }
        Ok(())
    }
}





#[derive(Clone, Debug, Decode, Deserialize, Encode, Eq, PartialEq,  Serialize)]
pub struct DataChunkLayout<H> {
    pub hash: H,
    pub uncomp_len: u32,
}


#[derive(Clone, Copy, Debug)]
pub struct DecodedSectorInfo {
    pub sector_type: SectorType,
    pub stream_offset: u64,
}


/// The metadata of an SSMD archive, read from its prefix by
/// [`parse_ssmd_prefix`].
///
/// [`parse_ssmd_prefix`]: crate::parse_ssmd_prefix
#[derive(Debug)]
pub struct ParsedSSMD<H> {
    pub header:          FileHeader,
    pub format_data:     SSMDFormatData,
    pub toc:             Vec<SSMDTocEntry>,
    pub disc_manifests:  Vec<DiscManifest<H>>,
    pub data_dictionary: Vec<u8>,
    /// The chunk and audio block indexes hold absolute file offsets, so a
    /// host can seek straight to a chunk or block.
    pub indices:         SSMDIndices<H>,
    pub subheader_table: Vec<[u8; 8]>,
    pub exception_blob:  Vec<u8>,
}


/// One source file of a disc, as its CUE sheet listed it.
#[derive(Clone, Deserialize, Debug, Decode, Encode, Eq, PartialEq, Serialize)]
pub struct DiscFile {
    /// The file name as the CUE sheet's FILE line gave it.
    pub name: String,
    /// The file's length in 2352-byte sectors.
    pub sector_count: u32,
    /// The tracks in the file, in order.
    pub tracks: Vec<DiscTrack>,
}


/// One track of a disc.
#[derive(Clone, Deserialize, Debug, Decode, Encode, Eq, PartialEq, Serialize)]
pub struct DiscTrack {
    pub number: u8,
    pub track_type: TrackType,
    pub indices: Vec<DiscIndex>,
    /// Sectors of silence before the track that aren't in its file, from
    /// the CUE sheet's PREGAP command. 0 when it has none.
    pub pregap: u32,
    /// Sectors of silence after the track that aren't in its file, from
    /// the CUE sheet's POSTGAP command. 0 when it has none.
    pub postgap: u32,
    pub area: DiscArea,
}


/// The area of a GD-ROM a track is in. Every track of a CD is in the
/// single-density area.
///
/// A GD-ROM's CUE sheet marks its areas with `REM SINGLE-DENSITY AREA`
/// before the first track and `REM HIGH-DENSITY AREA` before the first
/// track of the high-density area, which starts at sector 45000.
#[derive(
    Clone, Copy, Debug, Decode, Default, Deserialize, Encode, Eq, PartialEq,
    Serialize
)]
pub enum DiscArea {
    #[default]
    SingleDensity,
    HighDensity,
}


/// One INDEX of a track.
#[derive(Clone, Copy, Deserialize, Debug, Decode, Encode, Eq, PartialEq, Serialize)]
pub struct DiscIndex {
    pub number: u8,
    /// The index's position, in sectors from the start of its file.
    pub sector: u32,
}


#[derive(Clone, Deserialize, Debug, Decode, Encode, Eq, PartialEq, Serialize)]
pub struct DiscManifest<H> {
    /// The disc's files, tracks and indices, as its CUE sheet gave them.
    /// Empty for archives written before format version 2.1, whose CUE
    /// sheet is rebuilt from the sector map instead.
    pub files: Vec<DiscFile>,
    pub lba_map: Vec<(u32, u32)>,
    pub rle_sector_map: RleSectorMap,
    pub audio_block_map: Vec<ContentBlock<H>>,
    pub data_stream_layout: Vec<DataChunkLayout<H>>,
    pub subheader_index: Vec<SubHeaderEntry>,
    pub disc_exception_index: Vec<(u32, u32)>,
    pub integrity_hash: u64,
}


impl<H: Copy + Eq> DiscManifest<H> {
    pub fn build_stream_info(
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
                    uncomp_len: chunk_layout.uncomp_len,
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
            Some(ReconstructionInfo::FromStream {
                chunks: needed_chunks
            })
        }
    }
}


/// Reads single sectors of a disc from an archive.
///
/// `read_stored` returns the `compressed_length` bytes stored at a location
/// in the archive, as for [`write_disc`]. `E` is its error type.
///
/// [`write_disc`]: crate::write_disc
pub struct DiscReconstructor<'a, H, R, E> {
    manifest: &'a DiscManifest<H>,
    indices: &'a SSMDIndices<H>,
    dictionary: &'a [u8],
    read_stored: R,
    exception_blob: &'a [u8],
    subheader_table: &'a [[u8; 8]],

    context: ReconstructionContext<'a, H>,

    expanded_subheader_map: Vec<Option<u16>>,
    expanded_exception_map: Vec<Option<u32>>,
    expanded_sector_types: Vec<SectorType>,

    _error_type: PhantomData<fn() -> E>,
}

impl<'a, H, R, E> DiscReconstructor<'a, H, R, E>
where
    H: Hashable + Display + Copy + Eq,
    R: Fn(ChunkLocation) -> Result<Vec<u8>, E>,
    E: std::error::Error + Send + Sync + 'static,
{
    /// # Errors
    /// [`ReconstructionError::ExceptionSectorOutOfRange`] when the manifest's
    /// exception index names a sector past the end of the disc.
    pub fn new(
        manifest: &'a DiscManifest<H>,
        indices: &'a SSMDIndices<H>,
        dictionary: &'a [u8],
        read_stored: R,
        exception_blob: &'a [u8],
        subheader_table: &'a [[u8; 8]],
    ) -> Result<Self, ReconstructionError> {
        let context = ReconstructionContext::new(manifest);

        let expanded_sector_types = expand_sector_map(
            &manifest.rle_sector_map
        );
        let total_sectors = expanded_sector_types.len();

        let expanded_subheader_map = expand_subheader_map(
            &manifest.subheader_index,
            total_sectors
        );
        let expanded_exception_map = expand_exception_index(
            &manifest.disc_exception_index,
            total_sectors
        )?;

        Ok(Self {
            manifest,
            indices,
            dictionary,
            read_stored,
            exception_blob,
            subheader_table,
            context,
            expanded_subheader_map,
            expanded_exception_map,
            expanded_sector_types,
            _error_type: PhantomData,
        })
    }

    /// Reads and decodes the audio block stored under `content_hash`, which
    /// holds `sector_count` sectors, and checks that it decodes to the PCM
    /// that hash was taken from. FLAC can decode some damage without an
    /// error.
    fn read_audio_block(
        &self,
        content_hash: H,
        sector_count: u32,
    ) -> Result<Vec<u8>, SpriteShrinkCDError> {
        let chunk_loc = self.indices.audio_block_index.get(&content_hash)
            .ok_or_else(|| ReconstructionError::InternalError(
                format!("Missing audio hash {content_hash}")
            ))?;

        let comp_data = read_stored_at(&self.read_stored, *chunk_loc)?;
        let decomp = decompress_flac_block(
            &comp_data,
            audio_block_len(sector_count)
        )
            .map_err(|e| SpriteShrinkCDError::External(Box::new(e)))?;

        let actual = H::from_bytes_with_seed(&decomp);
        if actual != content_hash {
            return Err(ReconstructionError::AudioBlockHashMismatch {
                expected: content_hash.to_string(),
                actual: actual.to_string(),
            }.into());
        }
        Ok(decomp)
    }

    /// Reads and reconstructs a specific sector by its index
    /// (0-based from start of disc).
    pub fn get_sector_data(
        &self, sector_idx: u32
    ) -> Result<[u8; 2352], SpriteShrinkCDError> {
        let sector_idx_usize = sector_idx as usize;

        let sector_type = *self.expanded_sector_types.get(sector_idx_usize)
            .ok_or_else(|| ReconstructionError::InternalError(format!(
                "Sector index {} out of bounds (the disc has {} sectors)",
                sector_idx,
                self.expanded_sector_types.len()
            )))?;

        let reconstruction_info = self.context.get_reconstruction_info(
            sector_idx
        );
        let mut sector_data = [0u8; 2352];

        match reconstruction_info {
            Some(ReconstructionInfo::FromBlock {
                content_hash,
                sector_count,
                offset_in_block
            }) => {
                let decomp = self.read_audio_block(content_hash, sector_count)?;

                let start = offset_in_block as usize;
                let end = start + 2352;
                if decomp.len() < end {
                    return Err(ReconstructionError::InternalError(
                        "Audio block too short for requested sector".into()
                    ).into());
                }
                sector_data.copy_from_slice(&decomp[start..end]);
            },
            Some(ReconstructionInfo::FromStream { chunks }) => {
                let mut write_cursor: usize = 0;
                for chunk in chunks {
                    let chunk_loc = self.indices.chunk_index.get(
                        &chunk.chunk_hash
                    ).ok_or_else(|| ReconstructionError::InternalError(
                        format!("Missing data hash {}", chunk.chunk_hash)
                    ))?;

                    let comp_data = read_stored_at(&self.read_stored, *chunk_loc)?;
                    let decomp = decompress_chunk_checked(
                        &comp_data,
                        self.dictionary,
                        chunk.chunk_hash,
                        chunk.uncomp_len as usize
                    )?;

                    let start = chunk.read_from_offset as usize;
                    let len = chunk.read_length as usize;
                    if decomp.len() < start + len {
                        return Err(ReconstructionError::InternalError(
                            "Data chunk too short for requested slice".into()
                        ).into());
                    }

                    let target_slice = &mut sector_data[
                        write_cursor..write_cursor + len
                    ];
                    target_slice.copy_from_slice(&decomp[start..start + len]);
                    write_cursor += len;
                }

                /*The chunks found may cover only part of the sector, if
                the manifest's data stream ends early or has a gap.*/
                if write_cursor != usize::from(sector_type.data_size()) {
                    return Err(
                        ReconstructionError::MissingSectorData(sector_idx)
                            .into()
                    );
                }
            },
            //A zeroed sector stores no data; it is rebuilt from zeros.
            Some(ReconstructionInfo::None) => {}
            /*A sector type that stores data, but no audio block or data
            chunk covers it. Filling it with zeros would return a sector
            that was never on the disc.*/
            None => {
                return Err(
                    ReconstructionError::MissingSectorData(sector_idx).into()
                );
            }
        }

        if matches!(sector_type, SectorType::Audio |
            SectorType::PregapAudio | SectorType::ZeroedAudio
        ) {
            return Ok(sector_data);
        }


        let exception = self.expanded_exception_map.get(sector_idx_usize)
            .copied().flatten()
            .map(|excep_id| exception_data_for(
                &self.indices.exception_index,
                self.exception_blob,
                excep_id,
                sector_type.excep_size()
            ))
            .transpose()?;

        let data_id = self.expanded_subheader_map.get(sector_idx_usize)
            .copied().flatten()
            .ok_or(ReconstructionError::MissingSubheader(sector_idx_usize))?;
        let metadata = subheader_for(self.subheader_table, data_id)?;

        let lba = MsfOffsets::new(&self.manifest.lba_map)
            .sector_num(sector_idx);

        let user_data_len = sector_type.data_size() as usize;

        let rebuilt_sector = rebuild_sector_simd(
            &sector_data[..user_data_len],
            metadata,
            sector_type,
            lba,
            exception,
        ).map_err(SpriteShrinkCDError::Reconstruction)?;

        Ok(rebuilt_sector)
    }
}


#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ExceptionInfo {
    pub exception_type: ExceptionType,
    pub data_offset: u32,
}


pub struct ExceptionRegistry {
    map: DashMap<Vec<u8>, u32>,
    next_id: AtomicU32,
}


impl ExceptionRegistry {
    pub fn get_or_register(&self, data: Vec<u8>) -> u32 {
        *self.map.entry(data).or_insert_with(|| {
            self.next_id.fetch_add(1, Ordering::SeqCst)
        })
    }

    /// Builds the exception blob and its index with the entries sorted by
    /// their bytes.
    ///
    /// Discs analysed in parallel register entries in whatever order their
    /// threads reach them, so registration IDs differ from run to run.
    /// Sorting makes the blob depend only on which entries exist, and the
    /// returned table moves each disc's registration IDs onto it.
    ///
    /// # Errors
    ///
    /// Returns `MapperError::TooManyExceptions` if more entries were
    /// registered than a `u32` ID can number.
    pub fn sorted_table(&self) -> Result<ExceptionTable, SpriteShrinkCDError> {
        let (entries, new_ids) = order_by_bytes(&self.map)
            .ok_or(MapperError::TooManyExceptions)?;

        let mut blob = Vec::new();
        let mut index = Vec::with_capacity(entries.len());

        for data in entries {
            index.push(blob.len() as u64);
            blob.extend_from_slice(&data);
        }

        Ok(ExceptionTable { blob, index, new_ids })
    }
}


/// The exception blob shared by every disc in an archive, sorted by bytes.
#[derive(Debug)]
pub struct ExceptionTable {
    /// Every exception's data, one after another.
    pub blob: Vec<u8>,
    /// The offset of each exception in `blob`, by ID.
    pub index: Vec<u64>,
    /// The sorted ID of each registration ID.
    new_ids: Vec<u32>,
}


impl ExceptionTable {
    /// Replaces the registration IDs in a disc's exception index with
    /// sorted IDs. Apply it once to each disc's index.
    ///
    /// # Errors
    ///
    /// Returns `MapperError::UnregisteredException` for an ID the registry
    /// never handed out. Entries before it have already been replaced.
    pub fn renumber(
        &self,
        exception_index: &mut [(u32, u32)]
    ) -> Result<(), SpriteShrinkCDError> {
        for (_lba, id) in exception_index {
            *id = usize::try_from(*id).ok()
                .and_then(|old| self.new_ids.get(old))
                .copied()
                .ok_or(MapperError::UnregisteredException(*id))?;
        }

        Ok(())
    }
}


impl Default for ExceptionRegistry {
    fn default() -> Self {
        Self {
            map: DashMap::new(),
            next_id: AtomicU32::new(0),
        }
    }
}


#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExceptionType {
    Mode1,
    Mode2Form1,
    Mode2Form2,
    None
}


pub struct SubheaderRegistry {
    map: DashMap<[u8; 8], u16>,
    next_id: AtomicU16,
}


impl SubheaderRegistry {
    pub fn get_or_register(&self, data: [u8; 8]) -> u16 {
        *self.map.entry(data).or_insert_with(|| {
            self.next_id.fetch_add(1, Ordering::SeqCst)
        })
    }

    /// Builds the subheader table with the entries sorted by their bytes.
    ///
    /// Discs analysed in parallel register entries in whatever order their
    /// threads reach them, so registration IDs differ from run to run.
    /// Sorting makes the table depend only on which entries exist, and the
    /// returned table moves each disc's registration IDs onto it.
    ///
    /// # Errors
    ///
    /// Returns `MapperError::TooManySubheaders` if more than 65,536
    /// different subheaders were registered. Their `u16` IDs wrapped around
    /// during registration, so some discs share IDs that mean different
    /// subheaders.
    pub fn sorted_table(&self) -> Result<SubheaderTable, SpriteShrinkCDError> {
        let (entries, new_ids) = order_by_bytes(&self.map)
            .ok_or(MapperError::TooManySubheaders)?;

        Ok(SubheaderTable { entries, new_ids })
    }
}


/// The subheader table shared by every disc in an archive, sorted by bytes.
#[derive(Debug)]
pub struct SubheaderTable {
    /// The subheaders, by ID.
    pub entries: Vec<[u8; 8]>,
    /// The sorted ID of each registration ID.
    new_ids: Vec<u16>,
}


impl SubheaderTable {
    /// Replaces the registration IDs in a disc's subheader index with
    /// sorted IDs. Apply it once to each disc's index.
    ///
    /// Each registration ID maps to exactly one sorted ID, so neighbouring
    /// runs that had different IDs still do.
    ///
    /// # Errors
    ///
    /// Returns `MapperError::UnregisteredSubheader` for an ID the registry
    /// never handed out. Entries before it have already been replaced.
    pub fn renumber(
        &self,
        subheader_index: &mut [SubHeaderEntry]
    ) -> Result<(), SpriteShrinkCDError> {
        for entry in subheader_index {
            entry.data_id = self.new_ids.get(usize::from(entry.data_id))
                .copied()
                .ok_or(MapperError::UnregisteredSubheader(entry.data_id))?;
        }

        Ok(())
    }
}


/// Sorts a registry's entries by their bytes. Returns the entries in that
/// order, and a table giving each registration ID's position in it.
///
/// A registry hands out IDs from a counter inside the map's entry lock, so
/// the IDs are exactly `0..len` unless the counter wrapped. Returns `None`
/// then, as a position no longer fits in `I`.
fn order_by_bytes<K, I>(map: &DashMap<K, I>) -> Option<(Vec<K>, Vec<I>)>
where
    K: Clone + Eq + Hash + Ord,
    I: Copy + Default + TryFrom<usize>,
    usize: TryFrom<I>,
{
    let mut pairs: Vec<(K, I)> = map
        .iter()
        .map(|pair| (pair.key().clone(), *pair.value()))
        .collect();

    // Keys are unique, so an unstable sort still has one result.
    pairs.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

    let mut new_ids = vec![I::default(); pairs.len()];
    let mut entries = Vec::with_capacity(pairs.len());

    for (position, (key, old_id)) in pairs.into_iter().enumerate() {
        let slot = usize::try_from(old_id).ok()
            .and_then(|old| new_ids.get_mut(old))?;
        *slot = I::try_from(position).ok()?;
        entries.push(key);
    }

    Some((entries, new_ids))
}

impl Default for SubheaderRegistry {
    fn default() -> Self {
        Self {
            map: DashMap::new(),
            next_id: AtomicU16::new(0),
        }
    }
}



#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsfTime {
    pub minute: u8,
    pub second: u8,
    pub frame: u8,
}


impl MsfTime {
    /// Converts the MSF time into an absolute total number of frames, aka
    /// sectors, from the beginning of the file.
    pub fn to_total_frames(&self) -> u32 {
        ((self.minute as u32 * 60) + self.second as u32) * 75 + self.frame as u32
    }

    /// Creates a new MsfTime from an absolute sector count.
    /// If called on an already relative CUE sheet type, will return the same
    /// values.
    pub fn from_total_frames(total_frames: u32) -> Self {
        let minutes = total_frames / (60 * 75);
        let seconds = (total_frames / 75) % 60;
        let frames = total_frames % 75;
        MsfTime {
            minute: minutes as u8,
            second: seconds as u8,
            frame: frames as u8,
        }
    }
}

impl Display for MsfTime {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.minute, self.second, self.frame)
    }
}


#[derive(Clone, Debug, Decode, Deserialize, Encode, Eq, PartialEq, Serialize)]
pub struct RleSectorMap {
    pub runs: Vec<(u32, SectorType)>,
}


pub struct ReconstructionContext<'a, H> {
    pub manifest: &'a DiscManifest<H>,
    pub decoded_map: Vec<DecodedSectorInfo>,
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
                for block in &self.manifest.audio_block_map {
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
                            sector_count: block.sector_count,
                            offset_in_block,
                        });
                    }
                }
                None
            }
            SectorType::Mode1 | SectorType::Mode1Exception |
                SectorType::Mode2Form1 | SectorType::Mode2Form1Exception |
                SectorType::Mode2Form2 | SectorType::Mode2Form2Exception |
                SectorType::PregapMode1 | SectorType::PregapMode1Exception |
                SectorType::PregapMode2 | SectorType::PregapMode2Exception =>
            {
                self.manifest.build_stream_info(
                    sector_info
                )
            }
            _ => Some(ReconstructionInfo::None),
        }
    }
}


pub enum ReconstructionInfo<H> {
    FromBlock {
        content_hash: H,
        /// The whole block's length in sectors, from the disc manifest.
        sector_count: u32,
        offset_in_block: u32,
    },

    FromStream {
        chunks: Vec<StreamChunkInfo<H>>,
    },

    None,
}


pub struct SectorAnalysis{
    pub sector_type: SectorType,
    pub exception_data: Option<Vec<u8>>,
}

pub struct SectorMap {
    pub sectors: Vec<SectorType>,
}


pub struct SectorMapResult {
    pub sector_map: SectorMap,
    pub exception_index: Vec<(u32, u32)>,
    pub subheader_index: Vec<SubHeaderEntry>,
    pub lba_map: Vec<(u32, u32)>,
}


#[derive(
    Clone, Copy, Debug, Decode, Deserialize, Encode, Eq, PartialEq, Serialize
)]
pub enum SectorType {
    Audio,
    Mode1,
    Mode1Exception,
    Mode2Form1,
    Mode2Form1Exception,
    Mode2Form2,
    Mode2Form2Exception,
    PregapMode1,
    PregapMode1Exception,
    PregapMode2,
    PregapMode2Exception,
    PregapAudio,
    ZeroedAudio,
    ZeroedMode1Data,
    ZeroedMode2Data,
    None
}

impl SectorType {
    pub fn data_size(&self) -> u16 {
        match self {
            SectorType::Audio => 2352,
            SectorType::Mode1 => 2048,
            SectorType::Mode1Exception => 2048,
            SectorType::Mode2Form1 => 2048,
            SectorType::Mode2Form1Exception => 2048,
            SectorType::Mode2Form2 => 2324,
            SectorType::Mode2Form2Exception => 2324,
            SectorType::PregapMode1 => 2048,
            SectorType::PregapMode1Exception => 2048,
            SectorType::PregapMode2 => 2048,
            SectorType::PregapMode2Exception => 2048,
            SectorType::PregapAudio => 2352,
            SectorType::ZeroedAudio => 2352,
            SectorType::ZeroedMode1Data => 2352,
            SectorType::ZeroedMode2Data => 2352,
            SectorType::None => 0
        }
    }

    pub fn excep_size(&self) -> usize {
        match self {
            SectorType::Mode1Exception => 296,
            SectorType::Mode2Form1Exception => 296,
            SectorType::Mode2Form2Exception => 20,
            SectorType::PregapMode1Exception => 296,
            SectorType::PregapMode2Exception => 296,
            _ => 0
        }
    }

    /// Where a sector of this type keeps the bytes the data stream stores
    /// for it, or `None` for audio, which is stored as FLAC blocks.
    pub(crate) fn user_data_range(self) -> Option<Range<usize>> {
        match self {
            SectorType::Mode1 | SectorType::Mode1Exception
            | SectorType::PregapMode1 | SectorType::PregapMode1Exception =>
                Some(16..2064),
            SectorType::Mode2Form1 | SectorType::Mode2Form1Exception
            | SectorType::PregapMode2 | SectorType::PregapMode2Exception =>
                Some(24..2072),
            SectorType::Mode2Form2 | SectorType::Mode2Form2Exception =>
                Some(24..2348),
            SectorType::ZeroedMode1Data | SectorType::ZeroedMode2Data =>
                Some(0..2352),
            SectorType::Audio | SectorType::PregapAudio
            | SectorType::ZeroedAudio | SectorType::None => None,
        }
    }

    /// Where a data sector of this type keeps the 8 bytes stored in the
    /// subheader table: the subheader for Mode 2, the reserved area
    /// otherwise.
    pub(crate) fn metadata_range(self) -> Range<usize> {
        match self {
            SectorType::Mode2Form1 | SectorType::Mode2Form1Exception
            | SectorType::Mode2Form2 | SectorType::Mode2Form2Exception
            | SectorType::PregapMode2 | SectorType::PregapMode2Exception =>
                16..24,
            _ => 2068..2076,
        }
    }

    pub fn is_pregap(&self) -> bool {
        matches!(
            self,
            SectorType::PregapAudio
                | SectorType::PregapMode1
                | SectorType::PregapMode1Exception
                | SectorType::PregapMode2
                | SectorType::PregapMode2Exception
        )
    }
}


#[repr(C)]
#[derive(
    Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, PartialEq, Pod,
    Zeroable
)]
pub struct SSMDFormatData {
    pub enc_disc_manifest:      FileRegion,
    pub data_dictionary:        FileRegion,
    pub enc_chunk_index:        FileRegion,
    pub enc_audio_index:        FileRegion,
    pub enc_excep_index:        FileRegion,
    pub subheader_table:        FileRegion,
    pub exception_data:         FileRegion,
    pub audio_data:             FileRegion,
    pub data_offset:            u64,
}


impl SSMDFormatData {
    pub const SIZE: u64 = mem::size_of::<SSMDFormatData>() as u64;

    pub fn build_format_data (
        format_data_offset:     usize,
        man_length:             usize,
        data_dict_length:       u64,
        enc_chunk_idx_length:   u64,
        enc_audio_idx_length:   u64,
        enc_excep_index_length: u64,
        subheader_tbl_size:     u64,
        exception_data_length:  u64,
        audio_data_length:      u64,
    ) -> Self {
        let man_offset = format_data_offset as u64 + Self::SIZE;
        let data_dict_offset = man_offset + man_length as u64;
        let enc_chunk_idx_offset = data_dict_offset + data_dict_length;
        let enc_audio_idx_offset = enc_chunk_idx_offset + enc_chunk_idx_length;
        let enc_except_idx_offset = enc_audio_idx_offset + enc_audio_idx_length;
        let subheader_tbl_offset = enc_except_idx_offset + enc_excep_index_length;
        let exception_data_offset = subheader_tbl_offset + subheader_tbl_size;
        let audio_data_offset = exception_data_offset + exception_data_length;
        let data_offset = audio_data_offset + audio_data_length;

        SSMDFormatData {
            enc_disc_manifest: FileRegion {
                offset: man_offset,
                length: man_length as u64
            },
            data_dictionary: FileRegion {
                offset: data_dict_offset,
                length: data_dict_length
            },
            enc_chunk_index: FileRegion {
                offset: enc_chunk_idx_offset,
                length: enc_chunk_idx_length
            },
            enc_audio_index: FileRegion {
                offset: enc_audio_idx_offset,
                length: enc_audio_idx_length
            },
            enc_excep_index: FileRegion {
                offset: enc_except_idx_offset,
                length: enc_excep_index_length
            },
            subheader_table: FileRegion {
                offset: subheader_tbl_offset,
                length: subheader_tbl_size
            },
            exception_data: FileRegion {
                offset: exception_data_offset,
                length: exception_data_length
            },
            audio_data: FileRegion {
                offset: audio_data_offset,
                length: audio_data_length
            },
            data_offset
        }
    }

    /// Where the archive's metadata ends and the compressed audio begins.
    /// Every byte before this offset is part of the prefix that
    /// [`build_ssmd_prefix`] returns.
    ///
    /// This is `audio_data.offset`, not `data_offset`: the audio blocks sit
    /// between the metadata and the data chunks, so reading up to
    /// `data_offset` would load every audio block to get the metadata.
    ///
    /// [`build_ssmd_prefix`]: crate::build_ssmd_prefix
    #[must_use]
    pub fn metadata_prefix_end(&self) -> u64 {
        self.audio_data.offset
    }

    /// Returns the format data as it's stored in an archive, with every
    /// integer little-endian whatever the host's byte order.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; size_of::<Self>()] {
        bytemuck::cast(SSMDFormatData {
            enc_disc_manifest: FileRegion::to_le(self.enc_disc_manifest),
            data_dictionary:   FileRegion::to_le(self.data_dictionary),
            enc_chunk_index:   FileRegion::to_le(self.enc_chunk_index),
            enc_audio_index:   FileRegion::to_le(self.enc_audio_index),
            enc_excep_index:   FileRegion::to_le(self.enc_excep_index),
            subheader_table:   FileRegion::to_le(self.subheader_table),
            exception_data:    FileRegion::to_le(self.exception_data),
            audio_data:        FileRegion::to_le(self.audio_data),
            data_offset:       self.data_offset.to_le(),
        })
    }

    /// Reads format data written by [`SSMDFormatData::to_le_bytes`]. The
    /// bytes are copied, so `bytes` needs no particular alignment.
    #[must_use]
    pub fn from_le_bytes(bytes: &[u8; size_of::<Self>()]) -> Self {
        let raw: SSMDFormatData = bytemuck::cast(*bytes);
        SSMDFormatData {
            enc_disc_manifest: FileRegion::from_le(raw.enc_disc_manifest),
            data_dictionary:   FileRegion::from_le(raw.data_dictionary),
            enc_chunk_index:   FileRegion::from_le(raw.enc_chunk_index),
            enc_audio_index:   FileRegion::from_le(raw.enc_audio_index),
            enc_excep_index:   FileRegion::from_le(raw.enc_excep_index),
            subheader_table:   FileRegion::from_le(raw.subheader_table),
            exception_data:    FileRegion::from_le(raw.exception_data),
            audio_data:        FileRegion::from_le(raw.audio_data),
            data_offset:       u64::from_le(raw.data_offset),
        }
    }
}


#[derive(Debug)]
pub struct SSMDIndices<H> {
    pub chunk_index: HashMap<H, ChunkLocation>,
    pub audio_block_index: HashMap<H, ChunkLocation>,
    pub exception_index: Vec<u64>,
}


/// The `collection_id` of a disc in no collection (no playlist). Groups
/// use 0 to 254, so an archive holds at most 255 of them.
pub const NO_COLLECTION: u8 = u8::MAX;

#[derive(Decode, Encode, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SSMDTocEntry {
    pub filename: String,
    pub collection_id: u8,
    pub uncompressed_size: u64,
}


/// The part of one data chunk that a sector's user data needs.
pub struct StreamChunkInfo<H> {
    pub chunk_hash: H,
    /// The whole chunk's decoded length, from the disc manifest.
    pub uncomp_len: u32,
    pub read_from_offset: u32,
    pub read_length: u32,
}


#[derive(Clone, Debug, Decode, Deserialize, Encode, Eq, PartialEq, Serialize)]
pub struct SubHeaderEntry {
    pub start_lba: u32,
    pub count: u32,
    pub data_id: u16
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub number: u8,
    pub track_type: TrackType,
    pub indices: Vec<TrackIndex>,
    /// PREGAP length in sectors; 0 when the sheet gives none.
    pub pregap: u32,
    /// POSTGAP length in sectors; 0 when the sheet gives none.
    pub postgap: u32,
    pub area: DiscArea,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackIndex {
    pub number: u8,
    pub position: MsfTime,
}


#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TrackMode {
    Audio,
    Mode1,
    Mode2,
}

impl TrackMode {
    pub fn from_sector_type(st: SectorType) -> Option<Self> {
        match st {
            SectorType::Audio | SectorType::PregapAudio | SectorType::ZeroedAudio => {
                Some(TrackMode::Audio)
            }
            SectorType::Mode1
            | SectorType::PregapMode1
            | SectorType::Mode1Exception
            | SectorType::PregapMode1Exception
            | SectorType::ZeroedMode1Data=> Some(TrackMode::Mode1),
            SectorType::Mode2Form1
            | SectorType::PregapMode2
            | SectorType::Mode2Form1Exception
            | SectorType::PregapMode2Exception
            | SectorType::Mode2Form2
            | SectorType::Mode2Form2Exception
            | SectorType::ZeroedMode2Data=> Some(TrackMode::Mode2),
            SectorType::None => None,
        }
    }

    pub fn to_cue_type_string(self) -> &'static str {
        match self {
            TrackMode::Audio => "AUDIO",
            TrackMode::Mode1 => "MODE1/2352",
            TrackMode::Mode2 => "MODE2/2352",
        }
    }
}


#[derive(
    Clone, Copy, Debug, Decode, Deserialize, Encode, Eq, PartialEq, Serialize
)]
pub enum TrackType {
    Audio,
    Mode1_2352,
    Mode2_2352,
}

impl Display for TrackType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            TrackType::Audio => write!(f, "AUDIO"),
            TrackType::Mode1_2352 => write!(f, "MODE1/2352"),
            TrackType::Mode2_2352 => write!(f, "MODE2/2352"),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn msf_time_round_trips_through_frames(frames in 0u32..100 * 60 * 75) {
            prop_assert_eq!(
                MsfTime::from_total_frames(frames).to_total_frames(),
                frames
            );
        }
    }

    #[test]
    fn msf_time_splits_frames_into_minutes_seconds_and_frames() {
        assert_eq!(
            MsfTime::from_total_frames(150),
            MsfTime { minute: 0, second: 2, frame: 0 }
        );
        assert_eq!(
            MsfTime::from_total_frames(99 * 4500 + 59 * 75 + 74),
            MsfTime { minute: 99, second: 59, frame: 74 }
        );
    }

    #[test]
    fn msf_time_prints_two_digits_per_field() {
        assert_eq!(MsfTime::from_total_frames(0).to_string(), "00:00:00");
        assert_eq!(MsfTime::from_total_frames(4500 + 75 + 1).to_string(), "01:01:01");
        assert_eq!(
            MsfTime { minute: 99, second: 59, frame: 74 }.to_string(),
            "99:59:74"
        );
    }

    #[test]
    fn registering_the_same_exception_twice_returns_the_same_id() {
        let registry = ExceptionRegistry::default();
        let a = registry.get_or_register(vec![1; 20]);
        let b = registry.get_or_register(vec![2; 296]);
        assert_eq!(registry.get_or_register(vec![1; 20]), a);
        assert_eq!(registry.get_or_register(vec![2; 296]), b);
        assert_ne!(a, b);
    }

    #[test]
    fn exceptions_get_the_same_table_whatever_order_they_were_registered() {
        let entries = [vec![3; 296], vec![1; 20], vec![2; 296]];
        let build = |order: [usize; 3]| {
            let registry = ExceptionRegistry::default();
            for i in order {
                registry.get_or_register(entries[i].clone());
            }
            // A disc that met the entries in their listed order.
            let mut disc: Vec<(u32, u32)> = (0u32..)
                .zip(&entries)
                .map(|(lba, data)| (lba, registry.get_or_register(data.clone())))
                .collect();
            let table = registry.sorted_table().unwrap();
            table.renumber(&mut disc).unwrap();
            (table.blob, table.index, disc)
        };

        let (blob, index, disc) = build([0, 1, 2]);
        assert_eq!((blob.clone(), index.clone(), disc.clone()), build([2, 0, 1]));
        assert_eq!(index, [0, 20, 316]);
        for ((_, id), data) in disc.iter().zip(&entries) {
            let start = usize::try_from(index[*id as usize]).unwrap();
            assert_eq!(&blob[start..start + data.len()], data.as_slice());
        }
    }

    #[test]
    fn subheaders_get_the_same_table_whatever_order_they_were_registered() {
        let mode2 = [0, 0, 8, 0, 0, 0, 8, 0];
        let form2 = [1, 0, 0x20, 0, 1, 0, 0x20, 0];
        let zeros = [0; 8];
        let build = |order: [[u8; 8]; 3]| {
            let registry = SubheaderRegistry::default();
            for data in order {
                registry.get_or_register(data);
            }
            let entry = |start_lba, data| SubHeaderEntry {
                start_lba, count: 1, data_id: registry.get_or_register(data)
            };
            let mut disc = vec![entry(0, form2), entry(1, mode2), entry(2, form2)];
            let table = registry.sorted_table().unwrap();
            table.renumber(&mut disc).unwrap();
            (table.entries, disc)
        };

        let (entries, disc) = build([form2, mode2, zeros]);
        assert_eq!((entries.clone(), disc.clone()), build([zeros, mode2, form2]));
        assert_eq!(entries, [zeros, mode2, form2]);
        let ids: Vec<u16> = disc.iter().map(|entry| entry.data_id).collect();
        assert_eq!(ids, [2, 1, 2]);
    }

    #[test]
    fn renumbering_an_id_the_registry_never_gave_fails() {
        let registry = SubheaderRegistry::default();
        registry.get_or_register([0; 8]);
        let mut disc = [SubHeaderEntry { start_lba: 0, count: 1, data_id: 1 }];
        let err = registry.sorted_table().unwrap().renumber(&mut disc).unwrap_err();
        assert!(matches!(
            err,
            SpriteShrinkCDError::Mapper(MapperError::UnregisteredSubheader(1))
        ));
    }

    #[test]
    fn more_subheaders_than_a_u16_can_number_are_refused() {
        let registry = SubheaderRegistry::default();
        for n in 0..=u32::from(u16::MAX) + 1 {
            let [a, b, c, d] = n.to_le_bytes();
            registry.get_or_register([a, b, c, d, 0, 0, 0, 0]);
        }
        assert!(matches!(
            registry.sorted_table().unwrap_err(),
            SpriteShrinkCDError::Mapper(MapperError::TooManySubheaders)
        ));
    }
}
