use libc::c_char;

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

//FFI-safe equivalent of SSAChunkMeta
#[repr(C)]
#[derive(Clone)]
pub struct FFISSAChunkMeta {
    pub hash: u64,
    pub offset: u32,
    pub length: u32,
}

//FFI-safe equivalent of FileManifestParent
//Note: Uses raw pointers for strings and vectors.
#[repr(C)]
pub struct FFIFileManifestParent {
    pub filename: *const c_char,
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

//Holds all the output data from the serialize_uncompressed_data_ffi function.
#[repr(C)]
pub struct FFISerializedOutput {
    pub ser_manifest_ptr: *const u8,
    pub ser_manifest_len: usize,
    pub ser_data_store_ptr: *const u8,
    pub ser_data_store_len: usize,
    pub ser_chunk_index_ptr: *const u8,
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