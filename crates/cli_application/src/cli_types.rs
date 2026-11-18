//! Defines core data structures for the CLI application.
//!
//! This module centralizes the definitions of fundamental structs used across
//! the application's lifecycle. These structures facilitate communication
//! between threads and manage database interactions.

use std::{
    collections::HashMap,
    hash::Hash,
    path::PathBuf,
    sync::Arc,
};

use dashmap::DashMap;
use serde::{Serialize, Deserialize};

use sprite_shrink::{
    FileManifestParent, Hashable, SSAChunkMeta
};
use sprite_shrink_cd::{
    ContentBlock, DataChunkLayout, DiscFile, RleSectorMap, SubHeaderEntry
};


pub enum CacheBackend<H: Hash + Eq> {
    InMemory{
        map: Arc<DashMap<H, Vec<u8>>>,
        data_size: u64,
    },
    OnDisk {
        file_path: PathBuf,
        location_map: HashMap<H, LocationData>,
        current_offset: u64,
    },
}


pub struct CacheInfo {
    pub cache_path: PathBuf,
    pub at_cache_path: PathBuf,
}

#[derive(Debug)]
pub enum CacheTarget {
    Data,
    Audio,
}

/// A message containing a single data chunk and its hash.
///
/// This struct is used to send chunk data from file processing workers to a
/// central aggregator thread, enabling concurrent chunking and storage.
///
/// # Fields
///
/// * `chunk_hash`: The unique hash identifier of the data chunk.
/// * `chunk_data`: The raw binary data of the chunk.
#[derive(Debug)]
pub struct ChunkMessage<H: Hashable> {
    pub chunk_hash: H,
    pub chunk_data: Vec<u8>,
    pub chunk_size: usize
}


#[derive(Debug)]
pub struct DiscCompleteData<H: Hashable> {
    pub title: String,
    pub files: Vec<DiscFile>,
    pub lba_map: Vec<(u32, u32)>,
    pub rle_sector_map: RleSectorMap,
    pub audio_block_map: Vec<ContentBlock<H>>,
    pub data_stream_layout: Vec<DataChunkLayout<H>>,
    pub disc_exception_index: Vec<(u32, u32)>,
    pub subheader_index: Vec<SubHeaderEntry>,
    pub verification_hash: [u8; 64],
    pub integrity_hash: u64,
}

/// A container for all metadata generated after processing a single file.
///
/// This struct holds the complete results from the chunking and hashing stage
/// for one file, including its verification hash and the metadata for all of
/// its constituent chunks.
///
/// # Fields
///
/// * `file_name`: The original name of the processed file.
/// * `verification_hash`: The SHA-512 hash of the entire original file, for
///   verification.
/// * `chunk_count`: The total number of chunks the file was divided into.
/// * `chunk_meta`: A vector of metadata for each chunk, including its hash and
///   position.
#[derive(Debug)]
pub struct FileCompleteData<H: Hashable> {
    pub file_name: String,
    pub verification_hash: [u8; 64],
    pub chunk_count: u64,
    pub chunk_meta: Vec<SSAChunkMeta<H>>
}

/// Holds all per‑file metadata that is produced by the chunking stage.
///
/// `process_files` returns a `Result<FileData<H>, CliError>`. The function
///  builds two concurrent hash maps while it processes the input files.
///
/// * `file_manifest`: A dashmap that contains FileManifestParents and is used
///   to describe the structure of each file within the archive.
///
/// * `veri_hashes`: maps a *file name* (`String`) to the SHA‑512
///   verification hash (`[u8; 64]`) of the original file. After the
///   archive is written the verification step reads this map to ensure the
///   reconstructed file matches the original data.
///
/// Both maps are `DashMap`s, which means they can be written to from many
/// worker threads without additional synchronization. The struct is generic
/// over the hash type `H` (must implement `Hashable`) so that any supported
/// hash algorithm can be used.
pub struct FileData<H: Hashable>{
    pub file_manifest: DashMap<String, FileManifestParent<H>>,
    pub veri_hashes: DashMap<String, [u8; 64]>,
}


pub struct LocationData{
    pub offset: u64,
    pub length: u32
}

#[derive(Debug)]
pub struct OpticalChunkMessage<H: Hashable> {
    pub cache_target: CacheTarget,
    pub chunk_hash: H,
    pub chunk_data: Vec<u8>,
}

/// Defines cross-platform identifiers for the application.
///
/// These fields are used by crates like `directories` to locate standard
/// paths for configuration, cache, and data directories in a way that
/// conforms to the conventions of each operating system.
#[derive(Debug, PartialEq, Eq)]
pub struct ProjectIdentifier {
    pub qualifier: &'static str,
    pub organization: &'static str,
    pub application: &'static str,
    pub config_name: &'static str
}

/// Static instance of the application's project identifiers.
pub static APPIDENTIFIER: ProjectIdentifier =  ProjectIdentifier {
    qualifier: "",
    organization: "Zade222",
    application: "SpriteShrink",
    config_name: "SpriteShrink-config"
};

/// Represents the user-configurable settings for the SpriteShrink application.
///
/// This struct is automatically serialized to and deserialized from a TOML
/// configuration file, allowing users to persist their preferred settings
/// across sessions. It holds all parameters related to compression,
/// performance, and output formatting.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SpriteShrinkConfig {
    pub compression_level: u8,
    pub window_size: String,
    pub dictionary_size: String,
    pub hash_bit_length: u32,
    pub auto_tune: bool,
    pub autotune_timeout: u64,
    pub optimize_dictionary: bool,
    pub threads: usize,
    pub low_memory: bool,
    pub json_output: bool,
    pub log_retention_days: u16,
    pub quiet_output: bool,
    pub log_level: String
}

impl Default for SpriteShrinkConfig {
    fn default() -> Self {
        Self {
            json_output: false,
            compression_level: 19,
            window_size: "2kb".to_string(),
            dictionary_size: "16kb".to_string(),
            hash_bit_length: 64,
            auto_tune: false,
            autotune_timeout: 15,
            optimize_dictionary: false,
            threads: 0,
            low_memory: false,
            log_retention_days: 7,
            quiet_output: false,
            log_level: "error".to_string()
        }
    }
}

