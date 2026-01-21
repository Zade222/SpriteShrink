//! Defines core data structures for the CLI application.
//!
//! This module centralizes the definitions of fundamental structs used across
//! the application's lifecycle. These structures facilitate communication
//! between threads, manage database interactions, and handle temporary
//! resources, ensuring a clean and organized architecture.

use std::{
    collections::HashMap,
    fs::{Metadata, metadata, remove_file},
    hash::Hash,
    path::{Path, PathBuf},
    sync::Arc,
};

use dashmap::DashMap;
use serde::{Serialize, Deserialize};
use tracing::{
    debug,
    error
};

use sprite_shrink::{
    FileManifestParent, Hashable, SSAChunkMeta
};
use sprite_shrink_cd::{
    ContentBlock, DataChunkLayout, ExceptionInfo, RleSectorMap, SubHeaderEntry
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
    pub id: u32,
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
    pub chunk_size: usize
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

/// A resource guard that ensures a temporary file is deleted when it goes out
/// of scope.
///
/// This struct implements the **RAII** (Resource Acquisition Is
/// Initialization) pattern. When an instance of `TempFileGuard` is created, it
/// holds a reference to a temporary file's path. When the guard is dropped
/// (i.e., it goes out of scope), its `drop` implementation is automatically
/// called, ensuring the temporary file is cleaned up from the filesystem.
///
/// This is particularly useful for handling temporary data that should not
/// persist if an operation is interrupted, cancelled, or fails.
///
/// # Fields
///
/// * `path`: A reference to the `Path` of the temporary file to be managed.
pub struct TempFileGuard<'a> {
    path: &'a Path,
}

/// Creates a new `TempFileGuard` for the specified file path.
///
/// This function does not create the file itself, but rather establishes
/// the guard that will be responsible for deleting the file at `path` when
/// the guard is dropped.
///
/// # Arguments
///
/// * `path`: A reference to the `Path` of the temporary file to manage.
impl<'a> TempFileGuard<'a> {
    pub fn new(path: &'a Path) -> Self {
        Self { path }
    }


    pub fn size(&self) -> u64 {
        metadata(self.path).map_or(0, |meta| meta.len())
    }
}

/// The drop implementation for `TempFileGuard`.
///
/// This method is called automatically when the `TempFileGuard` instance
/// goes out of scope. It attempts to remove the temporary file at the
/// stored path.
///
/// /// # Arguments
///
/// * `path`: A reference to the `Path` of the temporary file being managed.
///
/// If the file has already been removed or was renamed (e.g., after a
/// successful operation), this method does nothing. If an error occurs
/// during file removal, it is logged, but the program will not panic.
impl<'a> Drop for TempFileGuard<'a> {
    fn drop(&mut self) {
        /*Only remove the file if it still exists. If the operation
        succeeded, it will have been renamed, and this will do nothing.*/
        if self.path.exists() {
            if let Err(e) = remove_file(self.path) {
                error!(
                    "Failed to remove temporary file at {:?}: {}",
                    self.path,
                    e
                );
            } else {
                debug!(
                    "Successfully removed temporary file: {:?}",
                    self.path
                );
            }
        }
    }
}
