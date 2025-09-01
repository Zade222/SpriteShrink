//! Defines FFI-safe structs for interoperability with C.
//!
//! # Safety
//!
//! All structs in this module that are passed across the FFI boundary are 
//! marked with `#[repr(C)]` to ensure a C-compatible memory layout.
//!
//! When interacting with these structs from C, the caller is responsible for:
//! - Ensuring that all pointers are valid and not null unless otherwise 
//!   specified.
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

#[repr(C)]
pub struct FFIChunkDataArray {
    pub ptr: *mut FFIVecBytes,
    pub len: usize,
}

/// FFI-safe representation for a single entry in the ChunkIndex HashMap with a
/// u64 hash.*/
#[repr(C)]
pub struct FFIChunkIndexEntryU64 {
    pub hash: u64,
    pub data: FFIChunkLocation,
}

/// FFI-safe representation for a single entry in the ChunkIndex HashMap with a
/// u128 hash.*/
#[repr(C)]
pub struct FFIChunkIndexEntryU128 {
    pub hash: u128,
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

/*FFI-safe representation for a single entry in the data_store HashMap with a
u64 hash.*/
#[repr(C)]
pub struct FFIDataStoreEntryU64 {
    pub hash: u64,
    pub data: *const u8,
    pub data_len: usize,
}

/*FFI-safe representation for a single entry in the data_store HashMap with a
u128 hash.*/
#[repr(C)]
pub struct FFIDataStoreEntryU128 {
    pub hash: u128,
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

/// Holds the output data from `create_file_manifest_and_chunks_ffi` for a
/// u64 hash
/// All pointers within this struct are owned by the C caller and must be 
/// freed.
#[repr(C)]
pub struct FFIFileManifestChunksU64 {
    pub fmp: FFIFileManifestParentU64,
    pub hashed_chunks: *mut FFIHashedChunkDataU64,
    pub hashed_chunks_len: usize,
}

/// Holds the output data from `create_file_manifest_and_chunks_ffi` for a
/// u128 hash
/// All pointers within this struct are owned by the C caller and must be 
/// freed.
#[repr(C)]
pub struct FFIFileManifestChunksU128 {
    pub fmp: FFIFileManifestParentU128,
    pub hashed_chunks: *mut FFIHashedChunkDataU128,
    pub hashed_chunks_len: usize,
}

/// FFI-safe equivalent of `FileManifestParent` for u64 ChunkMeta hashes.
/// The C caller must free the `filename` and `chunk_metadata` pointers.
#[repr(C)]
pub struct FFIFileManifestParentU64 {
    pub filename: *mut c_char,
    pub chunk_metadata: *const FFISSAChunkMetaU64,
    pub chunk_metadata_len: usize,
}

/// FFI-safe equivalent of `FileManifestParent` for u128 ChunkMeta hashes.
/// The C caller must free the `filename` and `chunk_metadata` pointers.
#[repr(C)]
pub struct FFIFileManifestParentU128 {
    pub filename: *mut c_char,
    pub chunk_metadata: *const FFISSAChunkMetaU128,
    pub chunk_metadata_len: usize,
}

/// FFI-safe representation of a chunk's hash and its associated data for u64
/// hashes
#[repr(C)]
pub struct FFIHashedChunkDataU64 {
    pub hash: u64,
    pub chunk_data: *mut u8,
    pub chunk_data_len: usize,
}

/// FFI-safe representation of a chunk's hash and its associated data for u128
/// hashes
#[repr(C)]
pub struct FFIHashedChunkDataU128 {
    pub hash: u128,
    pub chunk_data: *mut u8,
    pub chunk_data_len: usize,
}

/// The final struct returned by `parse_file_chunk_index_ffi` for u64
/// hashes
/// The `entries` pointer is owned by the C caller and must be freed.
#[repr(C)]
pub struct FFIParsedChunkIndexArrayU64 {
    pub entries: *mut FFIChunkIndexEntryU64,
    pub entries_len: usize,
}

/// The final struct returned by `parse_file_chunk_index_ffi` for u128
/// hashes
/// The `entries` pointer is owned by the C caller and must be freed.
#[repr(C)]
pub struct FFIParsedChunkIndexArrayU128 {
    pub entries: *mut FFIChunkIndexEntryU128,
    pub entries_len: usize,
}

/// The final struct returned by `parse_file_metadata_ffi` for a u64 hash.
/// The `manifests` pointer is owned by the C caller and must be freed.
#[repr(C)]
pub struct FFIParsedManifestArrayU64 {
    pub manifests: *mut FFIFileManifestParentU64,
    pub manifests_len: usize
}

/// The final struct returned by `parse_file_metadata_ffi` for a u128 hash.
/// The `manifests` pointer is owned by the C caller and must be freed.
#[repr(C)]
pub struct FFIParsedManifestArrayU128 {
    pub manifests: *mut FFIFileManifestParentU128,
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

#[repr(C)]
pub struct FFISeekChunkInfoU64 {
    pub hash: u64,
    pub read_start: u64,
    pub read_end: u64,
}

#[repr(C)]
pub struct FFISeekChunkInfoU128 {
    pub hash: u128,
    pub read_start: u64,
    pub read_end: u64,
}

#[repr(C)]
pub struct FFISeekInfoArrayU64 {
    pub chunks: *mut FFISeekChunkInfoU64,
    pub chunks_len: usize,
}

#[repr(C)]
pub struct FFISeekInfoArrayU128 {
    pub chunks: *mut FFISeekChunkInfoU128,
    pub chunks_len: usize,
}

/// Holds all the output data from `serialize_uncompressed_data_ffi` for a u64
/// hash.
/// All pointers within this struct are owned by the C caller and must be 
/// freed.
#[repr(C)]
pub struct FFISerializedOutputU64 {
    pub ser_manifest_ptr: *mut FFIFileManifestParentU64,
    pub ser_manifest_len: usize,
    pub ser_chunk_index_ptr: *mut FFIChunkIndexEntryU64,
    pub ser_chunk_index_len: usize,
    pub sorted_hashes_ptr: *const u64,
    pub sorted_hashes_len: usize,
}

/// Holds all the output data from `serialize_uncompressed_data_ffi` for a u128
/// hash.
/// All pointers within this struct are owned by the C caller and must be 
/// freed.
#[repr(C)]
pub struct FFISerializedOutputU128 {
    pub ser_manifest_ptr: *mut FFIFileManifestParentU128,
    pub ser_manifest_len: usize,
    pub ser_chunk_index_ptr: *mut FFIChunkIndexEntryU128,
    pub ser_chunk_index_len: usize,
    pub sorted_hashes_ptr: *const u128,
    pub sorted_hashes_len: usize,
}

//FFI-safe equivalent of SSAChunkMeta with a u64 hash
#[repr(C)]
#[derive(Clone)]
pub struct FFISSAChunkMetaU64 {
    pub hash: u64,
    pub offset: u64,
    pub length: u32,
}

//FFI-safe equivalent of SSAChunkMeta with a u128 hash
#[repr(C)]
#[derive(Clone)]
pub struct FFISSAChunkMetaU128 {
    pub hash: u128,
    pub offset: u64,
    pub length: u32,
}

/// A wrapper for a mutable C `void` pointer, used to pass user-defined
/// context data across the FFI boundary.
#[derive(Clone, Copy)]
pub struct FFIUserData(pub *mut c_void);

#[repr(C)]
pub struct FFIVecBytes {
    pub ptr: *mut u8,
    pub len: usize,
}

/// FFI-safe representation for a single entry in the VeriHashes DashMap.
/// The C caller retains ownership of the memory pointed to by `key` and 
/// `value`.
#[repr(C)]
pub struct FFIVeriHashesEntry {
    pub key: *const c_char,
    pub value: *const [u8; 64],
}

#[derive(Clone, Copy)]
pub struct ThreadSafeUserData(pub *mut c_void);
unsafe impl Send for ThreadSafeUserData {}
unsafe impl Sync for ThreadSafeUserData {}