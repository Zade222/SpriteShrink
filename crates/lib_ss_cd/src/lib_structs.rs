use std::{
    collections::HashMap,
    fmt,
    hash::{Hasher, BuildHasherDefault},
    mem,
    ops::Range,
    sync::atomic::{AtomicU16, AtomicU32, Ordering,}
};

use bitcode::{Decode, Encode};
use bytemuck::{Pod, Zeroable};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sprite_shrink::FileRegion;
use zerocopy::{Immutable, IntoBytes, FromBytes};


pub const SSMD_UID: u16 = 0xd541;


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

impl fmt::Display for CueSheet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for file in &self.files {
            writeln!(f, "FILE \"{}\" BINARY", file.name)?;

            for track in &file.tracks {
                writeln!(f, "  TRACK {:02} {}", track.number, track.track_type)?;

                for index in &track.indices {
                    writeln!(f, "    INDEX {:02} {}", index.number, index.position)?;
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


#[derive(Clone, Deserialize, Debug, Decode, Encode, Eq, PartialEq, Serialize)]
pub struct DiscManifest<H> {
    pub lba_map: Vec<(u32, u32)>,
    pub rle_sector_map: RleSectorMap,
    pub audio_block_map: Vec<ContentBlock<H>>,
    pub data_stream_layout: Vec<DataChunkLayout<H>>,
    pub subheader_index: Vec<SubHeaderEntry>,
    pub disc_exception_index: Vec<(u32, u32)>,
    pub integrity_hash: u64,
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

    pub fn generate_blob_and_index(&self) -> (Vec<u8>, Vec<u64>) {
        let mut pairs: Vec<(Vec<u8>, u32)> = self.map
            .iter()
            .map(|pair| (pair.key().clone(), *pair.value()))
            .collect();

        pairs.sort_by_key(|(_data, id)| *id);

        let mut blob = Vec::new();
        let mut excep_index = Vec::new();
        let mut current_offset = 0u64;

        for (data, _id) in pairs {
            blob.extend_from_slice(&data);
            excep_index.push(current_offset);
            current_offset += data.len() as u64;
        };

        (blob, excep_index)
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


impl ExceptionType {
    pub const MODE1_EXCEP_SIZE: u32 = 296;
    pub const MODE2FORM1_EXCEP_SIZE: u32 = 296;
    pub const MODE2FORM2_EXCEP_SIZE: u32 = 20;

    pub fn metadata_size(&self) -> u32 {
        match self {
            ExceptionType::Mode1 => Self::MODE1_EXCEP_SIZE,
            ExceptionType::Mode2Form1 => Self::MODE2FORM1_EXCEP_SIZE,
            ExceptionType::Mode2Form2 => Self::MODE2FORM2_EXCEP_SIZE,
            ExceptionType::None => 0
        }
    }
}


pub struct IdentityHasher {
    hash: u64,
}


impl Hasher for IdentityHasher {
    fn write(&mut self, _: &[u8]) {
        panic!("IdentityHasher only supports primitive integer types");
    }
    fn write_u64(&mut self, i: u64) {
        self.hash = i;
    }
    fn write_u32(&mut self, i: u32) {
        self.hash = i as u64;
    }
    fn write_usize(&mut self, i: usize) {
        self.hash = i as u64;
    }
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type IntMap<K, V> = HashMap<K, V, BuildHasherDefault<IdentityHasher>>;


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

    /// Converts the map into sorted vector
    pub fn generate_blob(&self) -> Vec<[u8; 8]> {
        let mut pairs: Vec<([u8; 8], u16)> = self.map
            .iter()
            .map(|pair| (*pair.key(), *pair.value()))
            .collect();

        pairs.sort_by_key(|(_data, id)| *id);

        pairs.into_iter().map(|(data, _)| data).collect()
    }
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

impl fmt::Display for MsfTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.minute, self.second, self.frame)
    }
}


pub struct ReconstructionContext<'a, H> {
    pub manifest: &'a DiscManifest<H>,
    pub decoded_map: Vec<DecodedSectorInfo>,
}


pub enum ReconstructionInfo<H> {
    FromBlock {
        content_hash: H,
        offset_in_block: u32,
    },

    FromStream {
        chunks: Vec<StreamChunkInfo<H>>,
    },

    None,
}


#[derive(Clone, Debug, Decode, Deserialize, Encode, Eq, PartialEq, Serialize)]
pub struct RleSectorMap {
    pub runs: Vec<(u32, SectorType)>,
}


pub struct SectorAnalysis{
    pub sector_type: SectorType,
    pub user_data_range: Range<usize>,
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
    ZeroedData,
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
            SectorType::ZeroedData => 2352,
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
}


#[repr(C)]
#[derive(Copy, Clone, FromBytes, Immutable, IntoBytes, Pod, Zeroable)]
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
        let enc_except_idx_offset = enc_audio_idx_offset + enc_excep_index_length;
        let subheader_tbl_offset = enc_except_idx_offset + enc_audio_idx_length;
        let exception_data_offset = subheader_tbl_offset + subheader_tbl_size;
        let audio_data_offset = exception_data_offset + subheader_tbl_size;
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
}


#[derive(Decode, Encode, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SSMDTocEntry {
    pub filename: String,
    pub collection_id: u8,
    pub uncompressed_size: u64,
}

pub struct StreamChunkInfo<H> {
    pub chunk_hash: H,
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
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackIndex {
    pub number: u8,
    pub position: MsfTime,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackType {
    Audio,
    Mode1_2352,
    Mode2_2352,
}

impl fmt::Display for TrackType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrackType::Audio => write!(f, "AUDIO"),
            TrackType::Mode1_2352 => write!(f, "MODE1/2352"),
            TrackType::Mode2_2352 => write!(f, "MODE2/2352"),
        }
    }
}
