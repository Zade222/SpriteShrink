use std::os::raw::c_void;

use libc::c_char;

//Note: Many of these structs use raw pointers for strings and vectors.

#[derive(Clone, Copy)]
pub struct CUserData(pub *mut c_void);

///Holds the final output data from a successful archive build.
#[repr(C)]
pub struct FFIArchiveData {
    pub data: *mut u8,
    pub data_len: usize,
}

//FFI-safe equivalent of SSAChunkMeta, derived from the Chunk struct in FastCDC
#[derive(Clone, Copy)]
pub struct FFIChunk{
    /// The gear hash value as of the end of the chunk.
    pub hash: u64,
    /// Starting byte position within the source.
    pub offset: usize,
    /// Length of the chunk in bytes.
    pub length: usize,
}

//FFI-safe equivalent of ChunkLocation
#[repr(C)]
#[derive(Clone)]
pub struct FFIChunkLocation {
    pub offset: u64,
    pub length: u32,
}

//FFI-safe representation for a single entry in the ChunkIndex HashMap
#[repr(C)]
pub struct FFIChunkIndexEntry {
    pub hash: u64,
    pub data: FFIChunkLocation,
}

//Holds all the output data from the create_file_manifest_and_chunks_ffi function.
#[repr(C)]
pub struct FFIFileManifestChunks {
    pub fmp: FFIFileManifestParent,
    pub hashed_chunks: *mut FFIHashedChunkData,
    pub hashed_chunks_len: usize,
}

#[repr(C)]
pub struct FFIHashedChunkData {
    pub hash: u64,
    pub chunk_data: *mut u8,
    pub chunk_data_len: usize,
}

//FFI-safe equivalent of SSAChunkMeta
#[repr(C)]
#[derive(Clone)]
pub struct FFISSAChunkMeta {
    pub hash: u64,
    pub offset: u32,
    pub length: u32,
}

//FFI-safe equivalent of FileData
#[repr(C)]
pub struct FFIFileData {
    pub filename: *const c_char,
    pub file_data: *const u8,
    pub file_data_len: usize
}

//FFI-safe equivalent of FileManifestParent
#[repr(C)]
pub struct FFIFileManifestParent {
    pub filename: *mut c_char,
    pub chunk_count: u64,
    pub chunk_metadata: *const FFISSAChunkMeta,
}

//FFI-safe representation for a single entry in the data_store HashMap
#[repr(C)]
pub struct FFIDataStoreEntry {
    pub hash: u64,
    pub data: *const u8,
    pub data_len: usize,
}

// The final struct returned by parse_file_chunk_index_ffi
#[repr(C)]
pub struct FFIParsedChunkIndexArray {
    pub entries: *mut FFIChunkIndexEntry,
    pub entries_len: usize,
}

//FFI-safe representation for the return data for parse_file_metadata_ffi
#[repr(C)]
pub struct FFIParsedManifestArray {
    pub manifests: *mut FFIFileManifestParent,
    pub manifests_len: usize
}

pub struct FFIProcessedFileData {
    pub filename: *mut c_char,
    pub filename_len: usize,
    pub veri_hash: *mut [u8; 64],
    pub chunks: *mut FFIChunk,
    pub chunks_len: usize,
    pub file_data: *mut u8,
    pub file_data_len: usize
}

/// FFI safe enum to represent the type of progress update.
#[repr(C)]
pub enum FFIProgressType {
    GeneratingDictionary,
    DictionaryDone,
    Compressing,
    ChunkCompressed,
    Finalizing,
}

/// FFI-safe struct to pass progress information across the C boundary.
#[repr(C)]
pub struct FFIProgress {
    pub ty: FFIProgressType,
    /// Only valid when `ty` is `Compressing`.
    pub total_chunks: u64,
}

//Holds all the output data from the serialize_uncompressed_data_ffi function.
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

//FFI-safe representation for a single entry in the ChunkIndex HashMap
#[repr(C)]
pub struct FFIVeriHashesEntry {
    pub key: *const c_char,
    pub value: *const [u8; 64],
}