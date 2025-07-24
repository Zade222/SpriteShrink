//! Defines FFI-safe structs for interoperability with C.
//!
//! # Safety
//!
//! All structs in this module that are passed across the FFI boundary are 
//! marked with `#[repr(C)]` to ensure a C-compatible memory layout.
//!
//! When interacting with these structs from C, the caller is responsible for:
//! - Ensuring that all pointers are valid and not null unless otherwise 
//! specified.
//! - Managing the memory for any data pointed to by these structs, including
//!   allocating it before calling Rust and freeing it after Rust returns it.
//! - Ensuring that all C-strings (`c_char*`) are null-terminated.

use std::os::raw::c_void;

use libc::c_char;

/// Holds the final output data from a successful archive build.
/// The `data` pointer is owned by the C caller and must be freed by calling
/// the appropriate free function (e.g., `archive_data_free`).
#[repr(C)]
pub struct FFIArchiveData {
    pub data: *mut u8,
    pub data_len: usize,
}

/// FFI-safe equivalent of a `fastcdc::Chunk`.
#[derive(Clone, Copy)]
pub struct FFIChunk{
    /// The gear hash value as of the end of the chunk.
    pub hash: u64,
    /// Starting byte position within the source.
    pub offset: usize,
    /// Length of the chunk in bytes.
    pub length: usize,
}

/// FFI-safe representation for a single entry in the ChunkIndex HashMap.
#[repr(C)]
pub struct FFIChunkIndexEntry {
    pub hash: u64,
    pub data: FFIChunkLocation,
}

/// FFI-safe equivalent of `ChunkLocation`, representing a slice of the 
/// archive.
#[repr(C)]
#[derive(Clone)]
pub struct FFIChunkLocation {
    pub offset: u64,
    pub length: u32,
}

//FFI-safe representation for a single entry in the data_store HashMap
#[repr(C)]
pub struct FFIDataStoreEntry {
    pub hash: u64,
    pub data: *const u8,
    pub data_len: usize,
}

/// FFI-safe equivalent of `FileData`, used to pass file contents into Rust.
/// The C caller retains ownership of the memory pointed to by `filename` and 
/// `file_data`.
#[repr(C)]
pub struct FFIFileData {
    pub filename: *const c_char,
    pub file_data: *const u8,
    pub file_data_len: usize
}

/// Holds the output data from `create_file_manifest_and_chunks_ffi`.
/// All pointers within this struct are owned by the C caller and must be 
/// freed.
#[repr(C)]
pub struct FFIFileManifestChunks {
    pub fmp: FFIFileManifestParent,
    pub hashed_chunks: *mut FFIHashedChunkData,
    pub hashed_chunks_len: usize,
}

/// FFI-safe equivalent of `FileManifestParent`.
/// The C caller must free the `filename` and `chunk_metadata` pointers.
#[repr(C)]
pub struct FFIFileManifestParent {
    pub filename: *mut c_char,
    pub chunk_metadata: *const FFISSAChunkMeta,
    pub chunk_metadata_len: usize,
}

/// FFI-safe representation of a chunk's hash and its associated data.
#[repr(C)]
pub struct FFIHashedChunkData {
    pub hash: u64,
    pub chunk_data: *mut u8,
    pub chunk_data_len: usize,
}

/// The final struct returned by `parse_file_chunk_index_ffi`.
/// The `entries` pointer is owned by the C caller and must be freed.
#[repr(C)]
pub struct FFIParsedChunkIndexArray {
    pub entries: *mut FFIChunkIndexEntry,
    pub entries_len: usize,
}

/// The final struct returned by `parse_file_metadata_ffi`.
/// The `manifests` pointer is owned by the C caller and must be freed.
#[repr(C)]
pub struct FFIParsedManifestArray {
    pub manifests: *mut FFIFileManifestParent,
    pub manifests_len: usize
}

/// Holds the output from `process_file_in_memory_ffi`.
/// All pointers within are owned by the C caller and must be freed.
#[repr(C)]
pub struct FFIProcessedFileData {
    pub filename: *mut c_char,
    pub filename_len: usize,
    pub veri_hash: *mut [u8; 64],
    pub chunks: *mut FFIChunk,
    pub chunks_len: usize,
    pub file_data: *mut u8,
    pub file_data_len: usize
}

/// FFI-safe struct to pass progress information across the C boundary.
#[repr(C)]
pub struct FFIProgress {
    pub ty: FFIProgressType,
    /// Only valid when `ty` is `Compressing`.
    pub total_chunks: u64,
}

/// FFI-safe enum to represent the type of progress update.
#[repr(C)]
pub enum FFIProgressType {
    GeneratingDictionary,
    DictionaryDone,
    Compressing,
    ChunkCompressed,
    Finalizing,
}

/// Holds all the output data from `serialize_uncompressed_data_ffi`.
/// All pointers within this struct are owned by the C caller and must be 
/// freed.
#[repr(C)]
pub struct FFISerializedOutput {
    pub ser_manifest_ptr: *mut FFIFileManifestParent,
    pub ser_manifest_len: usize,
    pub ser_data_store_ptr: *const u8,
    pub ser_data_store_len: usize,
    pub ser_chunk_index_ptr: *mut FFIChunkIndexEntry,
    pub ser_chunk_index_len: usize,
    pub sorted_hashes_ptr: *const u64,
    pub sorted_hashes_len: usize,
}

//FFI-safe equivalent of SSAChunkMeta
#[repr(C)]
#[derive(Clone)]
pub struct FFISSAChunkMeta {
    pub hash: u64,
    pub offset: u32,
    pub length: u32,
}

/// A wrapper for a mutable C `void` pointer, used to pass user-defined
/// context data across the FFI boundary.
#[derive(Clone, Copy)]
pub struct FFIUserData(pub *mut c_void);

/// FFI-safe representation for a single entry in the VeriHashes DashMap.
/// The C caller retains ownership of the memory pointed to by `key` and `value`.
#[repr(C)]
pub struct FFIVeriHashesEntry {
    pub key: *const c_char,
    pub value: *const [u8; 64],
}