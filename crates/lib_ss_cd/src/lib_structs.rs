use std::ops::Range;
use serde::{Deserialize, Serialize};


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlobLocation {
    pub offset: u64,
    pub length: u64,
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


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueSheetType {
    Absolute,
    Relative,
}


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DiscManifest<H> {
    pub title: String,
    pub collection_id: u8,
    pub normalized_cue_sheet: String,
    pub sector_map: RleSectorMap,
    pub block_map: Vec<BlockRun<H>>,
    pub data_stream_layout: Vec<DataChunkLayout<H>>,
}


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlockRun<H> {
    pub sector_type: SectorType,
    pub sector_count: u32,
    pub content_hash: H,
}


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DataChunkLayout<H> {
    pub hash: H,
    pub uncompressed_len: u32,
}


#[derive(Debug, PartialEq, Eq)]
pub struct DiscValidationResult {
    pub is_multiple_of_2352: bool,
    pub has_iso_pvd_marker: bool,
    pub has_ps1_system_marker: bool,
    pub has_ps1_xa_marker: bool,
}


impl DiscValidationResult {
    pub fn is_high_confidence(&self) -> bool {
        self.is_multiple_of_2352 &&
            self.has_iso_pvd_marker &&
            self.has_ps1_system_marker &&
            self.has_ps1_xa_marker
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


#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OpticalArchiveHeader {
    pub manifest_version: u16,

    pub manifests_location: BlobLocation,

    pub audio_block_index_location: BlobLocation,
    pub audio_blob_location: BlobLocation,

    pub pregap_block_index_location: BlobLocation,
    pub pregap_blob_location: BlobLocation,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RleSectorMap {
    pub runs: Vec<(u32, SectorType)>,
}


pub struct SectorAnalysis{
    pub sector_type: SectorType,
    pub user_data_range: Range<usize>,
    pub meta_data: Option<Vec<u8>>,
}

pub struct SectorMap {
    pub sectors: Vec<SectorType>,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectorType {
    Mode1,
    Mode1Exception,
    Mode1SbiException,
    Mode2Form1,
    Mode2Form2,
    Mode2Form1Exception,
    Audio,
    Pregap,
    None
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
