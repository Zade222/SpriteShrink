use std::{
    fmt::{self, Display},
    ops::Range
};
use serde::{Deserialize, Serialize};


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlobLocation {
    pub offset: u64,
    pub length: u64,
}


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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





#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DataChunkLayout<H> {
    pub hash: H,
    pub uncomp_len: u32,
}


#[derive(Clone, Copy, Debug)]
pub struct DecodedSectorInfo {
    pub sector_type: SectorType,
    pub stream_offset: u64,
}


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DiscManifest<H> {
    pub title: String,
    pub collection_id: u8,
    pub normalized_cue_sheet: String,
    pub lba_map: Vec<(u32, u32)>,
    pub rle_sector_map: RleSectorMap,
    pub block_map: Vec<ContentBlock<H>>,
    pub data_stream_layout: Vec<DataChunkLayout<H>>,
    pub exception_blob_offset: u64,
    pub exception_index_length: u32,
    pub subheader_map: Vec<(u32, u32, [u8; 8])>,
}


#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExceptionType {
    Mode1,
    Mode2Form1,
    Mode2Form2,
    None
}


#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ExceptionInfo {
    pub exception_type: ExceptionType,
    pub data_offset: u32,
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


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OpticalArchiveHeader {
    pub manifest_version: u16,

    pub manifests_location: BlobLocation,

    pub audio_block_index_location: BlobLocation,
    pub audio_blob_location: BlobLocation,

    pub pregap_block_index_location: BlobLocation,
    pub pregap_blob_location: BlobLocation,
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


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub exception_metadata: Vec<(u64, ExceptionType, Vec<u8>)>,
    pub subheader_map: Vec<(u32, u32, [u8; 8])>,
    pub normalized_cue_sheet: CueSheet,
    pub lba_map: Vec<(u32, u32)>,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}


pub struct StreamChunkInfo<H> {
    pub chunk_hash: H,
    pub read_from_offset: u32,
    pub read_length: u32,
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
