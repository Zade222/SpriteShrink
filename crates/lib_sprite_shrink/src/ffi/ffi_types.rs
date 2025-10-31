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

use std::{
    ffi::CStr,
    os::raw::c_void,
    slice,
};

use libc::c_char;

use crate::lib_structs::{
    ChunkLocation, FileManifestParent, SSAChunkMeta
};

use crate::processing::Hashable;

/// An opaque FFI handle to the `ArchiveBuilder`, configured for `u64` hashes.
///
/// This struct acts as a pointer to a Rust `ArchiveBuilder` instance without
/// exposing its internal layout to C. It allows C code to create, configure,
/// and ultimately build a sprite-shrink archive in a memory-safe way.
///
/// # Lifecycle
///
/// 1.  An instance of this handle is created and returned by
///     [`archive_builder_new_u64`].
/// 2.  The C caller can then use the handle with various
///     `archive_builder_set_*_u64` functions to configure the archive's
///     parameters (e.g., compression level, dictionary size).
/// 3.  Finally, the handle must be passed to [`archive_builder_build_u64`],
///     which consumes the builder, produces the final archive data, and
///     deallocates the underlying Rust object.
/// 4.  If the archive creation process is aborted before calling `build`, the
///     handle **must** be passed to [`archive_builder_free_u64`] to prevent
///     a memory leak.
///
/// # Safety
///
/// The C caller owns the pointer to this handle. It is the caller's
/// responsibility to ensure that the handle is used correctly and that its
/// memory is eventually freed by calling either the `build` or `free`
/// function exactly once. Failure to do so will result in memory leaks or
/// undefined behavior.
#[repr(C)]
pub struct ArchiveBuilderU64 { _private: [u8; 0] }

/// An opaque FFI handle to the `ArchiveBuilder`, configured for `u128` hashes.
///
/// This struct acts as a pointer to a Rust `ArchiveBuilder` instance without
/// exposing its internal layout to C. It allows C code to create, configure,
/// and ultimately build a sprite-shrink archive in a memory-safe way.
///
/// # Lifecycle
///
/// 1.  An instance of this handle is created and returned by
///     [`archive_builder_new_u128`].
/// 2.  The C caller can then use the handle with various
///     `archive_builder_set_*_u128` functions to configure the archive's
///     parameters (e.g., compression level, dictionary size).
/// 3.  Finally, the handle must be passed to [`archive_builder_build_u128`],
///     which consumes the builder, produces the final archive data, and
///     deallocates the underlying Rust object.
/// 4.  If the archive creation process is aborted before calling `build`, the
///     handle **must** be passed to [`archive_builder_free_u128`] to prevent
///     a memory leak.
///
/// # Safety
///
/// The C caller owns the pointer to this handle. It is the caller's
/// responsibility to ensure that the handle is used correctly and that its
/// memory is eventually freed by calling either the `build` or `free`
/// function exactly once. Failure to do so will result in memory leaks or
/// undefined behavior.
#[repr(C)]
pub struct ArchiveBuilderU128 { _private: [u8; 0] }

/// Represents a buffer containing the final, compressed archive data.
///
/// This struct is returned by a successful build operation (e.g., from
/// `archive_builder_build_u64`). When a C caller receives a pointer to this
/// struct, it takes ownership of both the struct itself and the underlying
/// data buffer it points to.
///
/// # Memory Management
///
/// The memory for this struct and its data buffer is allocated by the Rust
/// library. Therefore, it **MUST** be deallocated by the Rust library.
///
/// The C caller is responsible for passing the pointer they receive back to the
/// `archive_data_free` function to release the memory. Failure to do so will
/// result in a memory leak.
///
/// **Warning:** Do NOT attempt to free the `data` pointer manually using `free()`
/// in C. The memory is managed by Rust's allocator, and attempting to free it
/// with a different allocator will lead to heap corruption and undefined behavior.
#[repr(C)]
pub struct FFIArchiveData {
    pub data: *mut u8,
    pub data_len: usize,
    pub data_cap: usize,
}

/// FFI-safe equivalent of a `fastcdc::Chunk`, representing a content-defined
/// segment of a file.
///
/// This struct is used to pass chunking information from Rust to a C caller.
/// It describes a single chunk identified by the FastCDC algorithm, including
/// its hash, its starting offset in the original file, and its length.
///
/// # Fields
///
/// * `hash`: The gear hash value calculated by the chunking algorithm as of
///   the end of the chunk. Note that this is **not** the deduplication hash
///   of the chunk's content.
/// * `offset`: The starting byte position of this chunk within the original,
///   uncompressed source file.
/// * `length`: The total length of the chunk in bytes.
#[derive(Clone, Copy)]
pub struct FFIChunk{
    /// Gear hash value as of the end of the chunk.
    pub hash: u64,
    /// Starting byte position in the source.
    pub offset: usize,
    /// Length of the chunk in bytes.
    pub length: usize,
}

/// Represents a single entry in the archive's chunk index.
///
/// This struct maps a unique chunk hash to its physical location within the
/// compressed data section of the archive. An array of these entries makes up
/// the complete chunk index, which is essential for locating and decompressing
/// chunk data during file extraction.
///
/// This is the FFI-safe equivalent of a `(H, ChunkLocation)` tuple.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used to identify the
///   chunk.
///
/// # Fields
///
/// * `hash`: The unique deduplication hash of the data chunk.
/// * `data`: An `FFIChunkLocation` struct specifying the offset and length of
///   the compressed chunk data in the archive.
#[repr(C)]
pub struct FFIChunkIndexEntry<H> {
    pub hash: H,
    pub data: FFIChunkLocation,
}

/// An `FFIChunkIndexEntry` specialized for `u64` hashes.
///
/// This type is used in FFI functions that deal with chunk indices where the
/// hash algorithm produces a 64-bit integer.
pub type FFIChunkIndexEntryU64 = FFIChunkIndexEntry<u64>;


/// An `FFIChunkIndexEntry` specialized for `u128` hashes.
///
/// This type is used in FFI functions that deal with chunk indices where the
/// hash algorithm produces a 128-bit integer.
pub type FFIChunkIndexEntryU128 = FFIChunkIndexEntry<u128>;

// Converts a Rust-native tuple of a hash and its `ChunkLocation` into the
// FFI-safe `FFIChunkIndexEntry` struct.
impl<H> From<(H, ChunkLocation)> for FFIChunkIndexEntry<H> {
    fn from((hash, location): (H, ChunkLocation)) -> Self {
        Self {
            hash,
            data: FFIChunkLocation {
                offset: location.offset,
                length: location.compressed_length,
            },
        }
    }
}

/// Represents an array of byte vectors, used to pass multiple chunks of data
/// across the FFI boundary.
///
/// This struct is primarily used as the return type for C callback functions
/// that provide raw chunk data to Rust. For example, the `ArchiveBuilder`
/// uses it to retrieve uncompressed chunk data from the C application during
/// the archive creation process.
///
/// # Fields
///
/// * `ptr`: A pointer to the beginning of a contiguous array of
///   [`FFIVecBytes`] structs. Each struct in this array represents one chunk's
///   data.
/// * `len`: The number of [`FFIVecBytes`] structs in the array pointed to by
///   `ptr`.
///
/// # Memory Management
///
/// The C caller is responsible for allocating the memory for both the array of
/// `FFIVecBytes` and the underlying byte buffers for each chunk's data. When
/// this struct is returned to Rust, the Rust side takes ownership of all this
/// memory and will deallocate it. The C caller **must not** free this memory
/// after the function call returns.
/// FFI-safe equivalent of `ChunkLocation`, representing a slice of the
/// archive.
#[repr(C)]
pub struct FFIChunkDataArray {
    pub ptr: *mut FFIVecBytes,
    pub len: usize,
}

/// FFI-safe equivalent of `ChunkLocation`, representing a slice of the
/// compressed data in an archive.
///
/// This struct is used within the chunk index to specify the exact location
/// and size of a compressed data chunk. It provides the necessary information
/// to read a chunk's data from the archive's data section for decompression.
///
/// # Fields
///
/// * `offset`: The starting byte position of the chunk, relative to the
///   beginning of the archive's main data section.
/// * `length`: The total size of the compressed chunk in bytes.
#[repr(C)]
#[derive(Clone)]
pub struct FFIChunkLocation {
    pub offset: u64,
    pub length: u32,
}

// Converts a Rust-native `ChunkLocation` into the FFI-safe
// `FFIChunkLocation` struct.
impl From<ChunkLocation> for FFIChunkLocation {
    fn from(location: ChunkLocation) -> Self {
        Self {
            offset: location.offset,
            length: location.compressed_length,
        }
    }
}

/// Represents a single entry in a key-value data store, mapping a chunk's
/// hash to its raw, uncompressed data.
///
/// This struct is used to pass chunk data from a C caller to Rust. An array of
/// these entries can represent a complete data store, which is used during
/// serialization to build the final archive.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used to identify the
///   chunk.
///
/// # Fields
///
/// * `hash`: The unique deduplication hash of the data chunk.
/// * `data`: A raw pointer to the byte array containing the uncompressed
///   chunk data.
/// * `data_len`: The length of the `data` byte array.
///
/// # Safety
///
/// The C caller retains ownership of the memory pointed to by `data`. The
/// Rust code will only read from this pointer for the duration of the FFI
/// call and will not attempt to deallocate it. The C caller must ensure that
/// the pointer is valid and points to `data_len` bytes of readable memory for
/// the lifetime of the call.
#[repr(C)]
#[derive(Clone)]
pub struct FFIDataStoreEntry<H> {
    pub hash: H,
    pub data: *const u8,
    pub data_len: usize,
}

/// An `FFIDataStoreEntry` specialized for `u64` hashes.
///
/// This type is used in FFI functions that pass key-value data for chunks
/// where the hash algorithm produces a 64-bit integer.
pub type FFIDataStoreEntryU64 = FFIDataStoreEntry<u64>;


/// An `FFIDataStoreEntry` specialized for `u128` hashes.
///
/// This type is used in FFI functions that pass key-value data for chunks
/// where the hash algorithm produces a 128-bit integer.
pub type FFIDataStoreEntryU128 = FFIDataStoreEntry<u128>;

/// FFI-safe equivalent of `FileData`, used to pass the contents of a single
/// file from a C caller into Rust.
///
/// This struct provides a language-agnostic way to represent a file's name
/// and its complete binary data, allowing Rust functions to process files
/// managed by a C application without needing to perform file I/O directly.
///
/// # Fields
///
/// * `filename`: A pointer to a null-terminated C string representing the
///   name of the file.
/// * `file_data`: A raw pointer to the beginning of a byte array containing
///   the file's entire contents.
/// * `file_data_len`: The number of bytes in the `file_data` array.
///
/// # Safety
///
/// The C caller retains ownership of the memory pointed to by `filename` and
/// `file_data`. Rust will only read from these pointers for the duration of
/// the FFI call and will not attempt to deallocate them. The C caller must
/// ensure these pointers are valid, non-null, and point to memory that is
/// readable for their specified lengths for the lifetime of the call.
#[repr(C)]
pub struct FFIFileData {
    pub filename: *const c_char,
    pub file_data: *const u8,
    pub file_data_len: usize
}

/// A composite FFI structure that bundles a file's manifest with its
/// associated hashed chunk data.
///
/// This struct is returned by the `create_file_manifest_and_chunks_ffi_*`
/// functions. It contains all the necessary information generated from
/// processing a single file: the metadata needed for reconstruction (the
/// manifest) and the list of unique, hashed chunks that compose the file's
/// content.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used to identify the
///   chunks.
///
/// # Fields
///
/// * `fmp`: An [`FFIFileManifestParent`] struct containing the file's
///   metadata, such as its name and an ordered list of its chunk metadata.
/// * `hashed_chunks`: A pointer to an array of [`FFIHashedChunkData`] structs.
///   Each struct in this array contains a chunk's unique hash and a pointer
///   to its raw, uncompressed data.
/// * `hashed_chunks_len`: The number of elements in the `hashed_chunks` array.
///
/// # Safety
///
/// The C caller takes full ownership of the memory for this struct and all
/// the nested data it points to. To prevent significant memory leaks, the
/// top-level pointer to this struct **must** be passed to the corresponding
/// `free_file_manifest_and_chunks_ffi_*` function. This free function will
/// correctly deallocate the `fmp`'s internal data, the `hashed_chunks` array,
/// and the raw data buffer for each individual chunk.
#[repr(C)]
pub struct FFIFileManifestChunks<H> {
    pub fmp: FFIFileManifestParent<H>,
    pub hashed_chunks: *mut FFIHashedChunkData<H>,
    pub hashed_chunks_len: usize,
}

/// An `FFIFileManifestChunks` struct specialized for `u64` hashes.
///
/// This type is returned by [`create_file_manifest_and_chunks_ffi_u64`] and
/// is the expected input for [`free_file_manifest_and_chunks_ffi_u64`]. It
/// bundles the file manifest and its chunk data when the hash algorithm
/// produces a 64-bit integer.
pub type FFIFileManifestChunksU64 = FFIFileManifestChunks<u64>;

/// An `FFIFileManifestChunks` struct specialized for `u128` hashes.
///
/// This type is returned by [`create_file_manifest_and_chunks_ffi_u128`] and
/// is the expected input for [`free_file_manifest_and_chunks_ffi_u128`]. It
/// bundles the file manifest and its chunk data when the hash algorithm
/// produces a 128-bit integer.
pub type FFIFileManifestChunksU128 = FFIFileManifestChunks<u128>;

// Converts a tuple containing the constituent parts into the FFI-safe
// `FFIFileManifestChunks` struct.
impl<H> From<(
    FFIFileManifestParent<H>,
    *mut FFIHashedChunkData<H>,
    usize
)> for FFIFileManifestChunks<H> {
    fn from(
        (fmp, hashed_chunks, hashed_chunks_len): (
            FFIFileManifestParent<H>,
            *mut FFIHashedChunkData<H>,
            usize,
        ),
    ) -> Self {
        Self {
            fmp,
            hashed_chunks,
            hashed_chunks_len,
        }
    }
}

/// FFI-safe equivalent of `FileManifestParent`, describing a single file's
/// metadata within an archive.
///
/// This struct holds all the necessary information to reconstruct a file from
/// its constituent chunks. It contains the filename and a sorted list of chunk
/// metadata, which allows the extraction logic to retrieve and assemble the
/// file's data in the correct order. An array of these structs forms the
/// complete file manifest.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used in the chunk
///   metadata.
///
/// # Fields
///
/// * `filename`: A pointer to a null-terminated C string representing the
///   original name of the file.
/// * `chunk_metadata`: A pointer to a contiguous array of [`FFISSAChunkMeta`]
///   structs. This array is sorted by the chunk's original offset, ensuring
///   correct file reconstruction.
/// * `chunk_metadata_len`: The total number of `FFISSAChunkMeta` structs in
///   the `chunk_metadata` array.
///
/// # Safety
///
/// When this struct is created by Rust and passed to C, the C caller takes
/// ownership of the memory pointed to by `filename` and `chunk_metadata`.
/// This memory must be deallocated by calling the appropriate free function
/// (e.g., `free_parsed_manifest_ffi_*` or
/// `free_file_manifest_and_chunks_ffi_*`) to prevent memory leaks.
#[repr(C)]
pub struct FFIFileManifestParent<H> {
    pub filename: *mut c_char,
    pub chunk_metadata: *const FFISSAChunkMeta<H>,
    pub chunk_metadata_len: u64,
}

/// An `FFIFileManifestParent` specialized for `u64` hashes.
///
/// This type is used in FFI functions that deal with file manifests where the
/// chunk hash algorithm produces a 64-bit integer.
pub type FFIFileManifestParentU64 = FFIFileManifestParent<u64>;

/// An `FFIFileManifestParent` specialized for `u128` hashes.
///
/// This type is used in FFI functions that deal with file manifests where the
/// chunk hash algorithm produces a 128-bit integer.
pub type FFIFileManifestParentU128 = FFIFileManifestParent<u128>;

/// FFI-safe struct for passing an array of keys from C to Rust.
#[repr(C)]
pub struct FFIKeyArray<H> {
    pub ptr: *mut H,
    pub len: usize,
}

/// An `FFIKeyArray` specialized for `u64` hashes.
///
/// This type is used in FFI functions that deal with hash keys where the
/// chunk hash algorithm produces a 64-bit integer.
pub type FFIKeyArrayU64 = FFIKeyArray<u64>;

/// An `FFIKeyArray` specialized for `u128` hashes.
///
/// This type is used in FFI functions that deal with hash keys where the
/// chunk hash algorithm produces a 128-bit integer.
pub type FFIKeyArrayU128 = FFIKeyArray<u128>;

// Converts a tuple containing a Rust-native `FileManifestParent` and raw
// pointers for its heap-allocated fields into the FFI-safe
// `FFIFileManifestParent` struct.
//
// This is a convenience for assembling the FFI struct after its constituent
// parts (like the C-string filename and the chunk metadata array) have been
// manually allocated and prepared for C ownership.
impl<H> From<(
    FileManifestParent<H>,
    *mut std::os::raw::c_char,
    *const FFISSAChunkMeta<H>
)> for FFIFileManifestParent<H> {
    fn from(parts: (
        FileManifestParent<H>,
        *mut std::os::raw::c_char,
        *const FFISSAChunkMeta<H>
    )) -> Self {
        let (fmp, filename, chunk_metadata) = parts;
        Self {
            filename,
            chunk_metadata,
            chunk_metadata_len: fmp.chunk_count,
        }
    }
}

//Converts a reference to an FFI-safe `FFIFileManifestParent` into the
//Rust-native `FileManifestParent` struct.
//
// This is a crucial conversion for functions that receive manifest data from a
// C caller. It safely handles the reconstruction of Rust-native types, such
// as `String` and `Vec`, from the raw pointers provided in the FFI struct.

// # Safety

// This implementation contains an `unsafe` block because it dereferences raw
// pointers (`fmp.filename` and `fmp.chunk_metadata`). The caller must ensure
// that the `FFIFileManifestParent` instance contains valid, non-null pointers
// that are readable for their specified lengths for the duration of this
// conversion.
impl<H> From<&FFIFileManifestParent<H>> for FileManifestParent<H>
where
    H: Copy,
{
    fn from(fmp: &FFIFileManifestParent<H>) -> Self {
        unsafe{
            /*Convert the C string pointer to a Rust `String`.
            `to_string_lossy` ensures that even if the C string contains
            invalid UTF-8 sequences, the conversion will not panic.*/
            let filename = CStr::from_ptr(fmp.filename)
                .to_string_lossy()
                .into_owned();

            /*Reconstruct the slice of chunk metadata from the raw pointer
            and length.*/
            let chunk_metadata_slice = slice::from_raw_parts(
                fmp.chunk_metadata,
                fmp.chunk_metadata_len as usize
            );

            /*Convert each FFI-safe `FFISSAChunkMeta` in the slice to its
            Rust-native `SSAChunkMeta` equivalent and collect them into a Vec.*/
            let chunk_metadata = chunk_metadata_slice
                .iter()
                .map(SSAChunkMeta::from)
                .collect();

            //Construct the final Rust-native `FileManifestParent` struct.
            FileManifestParent {
                filename,
                chunk_metadata,
                chunk_count: fmp.chunk_metadata_len
            }
        }
    }
}

/// Represents a chunk's unique hash and its associated raw, uncompressed data.
///
/// This struct is used to pass the fundamental components of a file's content
/// from Rust to a C caller. An array of these structs is a key output of the
/// `create_file_manifest_and_chunks_ffi_*` functions, providing the C
/// application with a complete set of all data chunks from a processed file,
/// each paired with its deduplication hash.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used to uniquely
///   identify the chunk's content.
///
/// # Fields
///
/// * `hash`: The unique deduplication hash (e.g., xxhash3) of the chunk's
///   content.
/// * `chunk_data`: A raw pointer to the beginning of a byte array containing
///   the chunk's uncompressed data.
/// * `chunk_data_len`: The number of bytes in the `chunk_data` array.
///
/// # Safety
///
/// When this struct is created by Rust and passed to C, the C caller takes
/// full ownership of the memory pointed to by `chunk_data`. To prevent a
/// memory leak, this memory **must** be deallocated by calling the
/// appropriate free function (e.g., `free_file_manifest_and_chunks_ffi_*`),
/// which handles the cleanup of all nested data.
#[repr(C)]
#[derive(Clone)]
pub struct FFIHashedChunkData<H> {
    pub hash: H,
    pub chunk_data: *mut u8,
    pub chunk_data_len: usize,
}

/// An `FFIHashedChunkData` struct specialized for `u64` hashes.
///
/// This type is used to represent a chunk's data and its 64-bit hash when
/// passing information from Rust to a C caller.
pub type FFIHashedChunkDataU64 = FFIHashedChunkData<u64>;

/// An `FFIHashedChunkData` struct specialized for `u128` hashes.
///
/// This type is used to represent a chunk's data and its 128-bit hash when
/// passing information from Rust to a C caller.
pub type FFIHashedChunkDataU128 = FFIHashedChunkData<u128>;

// Converts a tuple containing the constituent parts into the FFI-safe
// `FFIHashedChunkData` struct. This is a convenience for assembling the
// struct from its raw components.
impl<H> From<(H, *mut u8, usize)> for FFIHashedChunkData<H> {
    fn from((hash, chunk_data, chunk_data_len):
    (H, *mut u8, usize)) -> Self {
        Self {
            hash,
            chunk_data,
            chunk_data_len,
        }
    }
}

/// Holds the result of parsing a serialized chunk index from an archive.
///
/// This struct is returned by the `parse_file_chunk_index_ffi_*` functions.
/// It provides a C-compatible representation of the entire chunk index, which
/// maps every unique chunk hash to its location within the archive's data
/// section.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used in the chunk
///   index.
///
/// # Fields
///
/// * `entries`: A pointer to the beginning of a contiguous array of
///   [`FFIChunkIndexEntry`] structs.
/// * `entries_len`: The total number of entries in the `entries` array.
///
/// # Safety
///
/// The C caller takes full ownership of the memory for this struct and the
/// `entries` array it points to. To prevent a memory leak, the top-level
/// pointer to this struct **must** be passed to the corresponding
/// `free_parsed_chunk_index_ffi_*` function once it is no longer needed.
#[repr(C)]
pub struct FFIParsedChunkIndexArray<H> {
    pub entries: *mut FFIChunkIndexEntry<H>,
    pub entries_len: usize,
}

/// An `FFIParsedChunkIndexArray` specialized for `u64` hashes.
///
/// This type is returned by [`parse_file_chunk_index_ffi_u64`] and is the
/// expected input for [`free_parsed_chunk_index_ffi_u64`]. It represents a
/// complete chunk index where each chunk is identified by a 64-bit hash.
pub type FFIParsedChunkIndexArrayU64 = FFIParsedChunkIndexArray<u64>;

/// An `FFIParsedChunkIndexArray` specialized for `u128` hashes.
///
/// This type is returned by [`parse_file_chunk_index_ffi_u128`] and is the
/// expected input for [`free_parsed_chunk_index_ffi_u128`]. It represents a
/// complete chunk index where each chunk is identified by a 128-bit hash.
pub type FFIParsedChunkIndexArrayU128 = FFIParsedChunkIndexArray<u128>;

// Converts a tuple containing a raw pointer to the entries and their length
// into the FFI-safe `FFIParsedChunkIndexArray` struct.
impl<H> From<(*mut FFIChunkIndexEntry<H>, usize)> for
FFIParsedChunkIndexArray<H> {
    fn from(parts: (*mut FFIChunkIndexEntry<H>, usize)) -> Self {
        Self {
            entries: parts.0,
            entries_len: parts.1,
        }
    }
}

/// Holds the result of parsing a serialized file manifest from an archive.
///
/// This struct is returned by the `parse_file_metadata_ffi_*` functions and
/// provides a C-compatible representation of the entire file manifest. It
/// contains an array of [`FFIFileManifestParent`] structs, each describing a
/// single file within the archive.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used in the chunk
///   metadata within each manifest.
///
/// # Fields
///
/// * `manifests`: A pointer to the beginning of a contiguous array of
///   [`FFIFileManifestParent`] structs.
/// * `manifests_len`: The total number of manifest entries in the `manifests`
///   array.
///
/// # Safety
///
/// The C caller takes full ownership of the memory for this struct and all the
/// nested data it points to (the `manifests` array, and the internal pointers
/// for filenames and chunk metadata within each manifest). To prevent
/// significant memory leaks, the top-level pointer to this struct **must** be
/// passed to the corresponding `free_parsed_manifest_ffi_*` function once it
/// is no longer needed.
#[repr(C)]
pub struct FFIParsedManifestArray<H> {
    pub manifests: *mut FFIFileManifestParent<H>,
    pub manifests_len: usize
}

/// An `FFIParsedManifestArray` specialized for `u64` hashes.
///
/// This type is returned by [`parse_file_metadata_ffi_u64`] and is the
/// expected input for [`free_parsed_manifest_ffi_u64`]. It represents a
/// complete file manifest where each chunk is identified by a 64-bit hash.
pub type FFIParsedManifestArrayU64 = FFIParsedManifestArray<u64>;

/// An `FFIParsedManifestArray` specialized for `u128` hashes.
///
/// This type is returned by [`parse_file_metadata_ffi_u128`] and is the
/// expected input for [`free_parsed_manifest_ffi_u128`]. It represents a
/// complete file manifest where each chunk is identified by a 128-bit hash.
pub type FFIParsedManifestArrayU128 = FFIParsedManifestArray<u128>;

// Converts a tuple containing a raw pointer to the manifests and their
// length into the FFI-safe `FFIParsedManifestArray` struct.
impl<H> From<(*mut FFIFileManifestParent<H>, usize)> for FFIParsedManifestArray<H> {
    fn from(parts: (*mut FFIFileManifestParent<H>, usize)) -> Self {
        Self {
            manifests: parts.0,
            manifests_len: parts.1,
        }
    }
}

/// Holds all the output data from processing a single file in memory via the
/// FFI.
///
/// This struct is returned by the [`process_file_in_memory_ffi`] function. It
/// bundles all critical information generated after a file has been read and
/// analyzed, including its name, its content-defined chunks, its raw data,
/// and a verification hash for data integrity.
///
/// # Fields
///
/// * `filename`: A pointer to a null-terminated C string containing the name
///   of the processed file.
/// * `filename_len`: The length of the `filename` string in bytes, excluding
///   the null terminator.
/// * `veri_hash`: A pointer to a 64-byte array containing the SHA-512 hash of
///   the original file's contents.
/// * `chunks`: A pointer to the beginning of a contiguous array of [`FFIChunk`]
///   structs, representing the file's content-defined chunks.
/// * `chunks_len`: The total number of `FFIChunk` structs in the `chunks`
///   array.
/// * `file_data`: A raw pointer to a byte array holding the complete, original
///   contents of the file.
/// * `file_data_len`: The number of bytes in the `file_data` array.
///
/// # Safety
///
/// The C caller takes full ownership of the memory for this struct and all the
/// nested data it points to (`filename`, `veri_hash`, `chunks`, and
/// `file_data`). To prevent significant memory leaks, the top-level pointer to
/// this struct **must** be passed to the [`free_processed_file_data_ffi`]
/// function once it is no longer needed.
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

/// FFI-safe struct to pass progress information from Rust to a C caller.
///
/// This struct is used by C callback functions that are registered to receive
/// updates on long-running operations, such as archive building. It provides
/// a clear, structured way to report the current state of the process.
///
/// # Fields
///
/// * `ty`: An [`FFIProgressType`] enum value that specifies the kind of
///   progress being reported (e.g., dictionary generation, compression).
/// * `total_chunks`: This field is only valid when `ty` is
///   [`FFIProgressType::Compressing`]. It indicates the total number of
///   unique data chunks that need to be compressed, allowing the C caller to
///   set up a progress bar or other indicator.
#[repr(C)]
pub struct FFIProgress {
    pub ty: FFIProgressType,
    /// Only valid when `ty` is `Compressing`.
    pub total_chunks: u64,
}

/// FFI-safe enum representing the different stages of a long-running
/// operation.
///
/// This enum is used within the [`FFIProgress`] struct to signal which phase
/// of the archive-building process is currently active.
#[repr(C)]
pub enum FFIProgressType {
    GeneratingDictionary,
    DictionaryDone,
    Compressing,
    ChunkCompressed,
    Finalizing,
}

/// Contains the necessary information to read a specific byte range from a
/// single chunk to fulfill a file seek request.
///
/// When a caller requests a specific byte range from a file (a seek
/// operation), this struct identifies one of the chunks that contains part of
/// the required data. An array of these structs tells the caller exactly which
/// chunks to decompress and which parts of the resulting data to copy to
/// reconstruct the requested byte range.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used to identify the
///   chunk.
///
/// # Fields
///
/// * `hash`: The unique hash of the required data chunk.
/// * `read_start`: The starting byte offset within the *decompressed* chunk
///   data from where to begin reading.
/// * `read_end`: The exclusive ending byte offset within the *decompressed*
///   chunk data where reading should stop.
#[repr(C)]
#[derive(Clone)]
pub struct FFISeekChunkInfo<H> {
    pub hash: H,
    pub read_start: u64,
    pub read_end: u64,
}

/// An `FFISeekChunkInfo` specialized for `u64` hashes.
///
/// This type is used in the `FFISeekInfoArrayU64` struct that is returned by
/// [`get_seek_chunks_ffi_u64`], identifying a required chunk with a 64-bit
/// hash.
pub type FFISeekChunkInfoU64 = FFISeekChunkInfo<u64>;

/// An `FFISeekChunkInfo` specialized for `u128` hashes.
///
/// This type is used in the `FFISeekInfoArrayU64` struct that is returned by
/// [`get_seek_chunks_ffi_u128`], identifying a required chunk with a 128-bit
/// hash.
pub type FFISeekChunkInfoU128 = FFISeekChunkInfo<u128>;

// Converts a tuple containing a chunk's hash and the start/end read boundaries
// into the FFI-safe `FFISeekChunkInfo` struct.
impl<H: Hashable> From<(H, (u64, u64))> for FFISeekChunkInfo<H> {
    fn from((hash, (start, end)): (H, (u64, u64))) -> Self {
        Self {
            hash,
            read_start: start,
            read_end: end,
        }
    }
}

/// Holds the result of a file seek calculation, specifying which chunks are
/// needed to reconstruct a requested byte range.
///
/// This struct is returned by the `get_seek_chunks_ffi_*` functions. It
/// contains a complete list of all the chunks that must be read and
/// decompressed to satisfy a seek request, along with the specific byte
/// ranges to copy from each one.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used to identify the
///   chunks.
///
/// # Fields
///
/// * `chunks`: A pointer to the beginning of a contiguous array of
///   [`FFISeekChunkInfo`] structs. Each struct in this array identifies a
///   required chunk and the byte range to read from it after decompression.
/// * `chunks_len`: The total number of `FFISeekChunkInfo` structs in the
///   `chunks` array.
///
/// # Safety
///
/// The C caller takes full ownership of the memory for this struct and the
/// `chunks` array it points to. To prevent a memory leak, the top-level
/// pointer to this struct **must** be passed to the corresponding
/// `free_seek_chunks_ffi_*` function once it is no longer needed.
#[repr(C)]
pub struct FFISeekInfoArray<H> {
    pub chunks: *mut FFISeekChunkInfo<H>,
    pub chunks_len: usize,
}

/// An `FFISeekInfoArray` specialized for `u64` hashes.
///
/// This type is returned by [`get_seek_chunks_ffi_u64`] and is the
/// expected input for [`free_seek_chunks_ffi_u64`]. It provides a complete
/// list of all chunks and byte ranges required to fulfill a seek request where
/// chunks are identified by 64-bit hashes.
pub type FFISeekInfoArrayU64 = FFISeekInfoArray<u64>;

/// An `FFISeekInfoArray` specialized for `u128` hashes.
///
/// This type is returned by [`get_seek_chunks_ffi_u128`] and is the
/// expected input for [`free_seek_chunks_ffi_u128`]. It provides a complete
/// list of all chunks and byte ranges required to fulfill a seek request where
/// chunks are identified by 128-bit hashes.
pub type FFISeekInfoArrayU128 = FFISeekInfoArray<u128>;

//Converts a tuple containing a raw pointer to the seek info chunks and
// their length into the FFI-safe `FFISeekInfoArray` struct.
impl<H: Hashable> From<(*mut FFISeekChunkInfo<H>, usize)> for
    FFISeekInfoArray<H> {
        fn from((
            chunks_ptr,
            chunks_len
        ): (*mut FFISeekChunkInfo<H>, usize)) -> Self {
            Self {
                chunks: chunks_ptr,
                chunks_len,
            }
        }
}

/// Holds all the serialized data prepared for the archive-building process.
///
/// This struct is the output of the `serialize_uncompressed_data_ffi_*`
/// functions. It bundles together all the necessary components—the file
/// manifest, the chunk index, and the sorted list of unique hashes—into a
/// single, C-compatible structure. This data is then passed to the
/// `archive_builder_new_*` functions to begin the final archive construction.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used throughout the
///   serialized data.
///
/// # Fields
///
/// * `ser_manifest_ptr`: A pointer to an array of [`FFIFileManifestParent`]
///   structs, representing the complete, sorted file manifest.
/// * `ser_manifest_len`: The number of entries in the `ser_manifest_ptr`
///   array.
/// * `ser_chunk_index_ptr`: A pointer to an array of [`FFIChunkIndexEntry`]
///   structs, representing the complete, sorted chunk index.
/// * `ser_chunk_index_len`: The number of entries in the
///   `ser_chunk_index_ptr` array.
/// * `sorted_hashes_ptr`: A pointer to a contiguous array of all unique chunk
///   hashes, sorted in a deterministic order.
/// * `sorted_hashes_len`: The number of hashes in the `sorted_hashes_ptr`
///   array.
///
/// # Safety
///
/// The C caller takes full ownership of the memory for this struct and all the
/// arrays it points to. To prevent significant memory leaks, the top-level
/// pointer to this struct **must** be passed to the corresponding
/// `free_serialized_output_*` function once it is no longer needed.
#[repr(C)]
pub struct FFISerializedOutput<H> {
    pub ser_manifest_ptr: *mut FFIFileManifestParent<H>,
    pub ser_manifest_len: usize,
    pub ser_chunk_index_ptr: *mut FFIChunkIndexEntry<H>,
    pub ser_chunk_index_len: usize,
    pub sorted_hashes_ptr: *const H,
    pub sorted_hashes_len: usize,
}

/// An `FFISerializedOutput` struct specialized for `u64` hashes.
///
/// This type is returned by [`serialize_uncompressed_data_ffi_u64`] and is
/// the expected input for [`free_serialized_output_u64`]. It bundles all the
/// necessary serialized data (manifest, chunk index, sorted hashes) for
/// building an archive where chunks are identified by 64-bit hashes.
pub type FFISerializedOutputU64 = FFISerializedOutput<u64>;

/// An `FFISerializedOutput` struct specialized for `u128` hashes.
///
/// This type is returned by [`serialize_uncompressed_data_ffi_u128`] and is
/// the expected input for [`free_serialized_output_u128`]. It bundles all the
/// necessary serialized data (manifest, chunk index, sorted hashes) for
/// building an archive where chunks are identified by 128-bit hashes.
pub type FFISerializedOutputU128 = FFISerializedOutput<u128>;

// Converts a tuple containing all the constituent raw pointers and lengths
// into the FFI-safe `FFISerializedOutput` struct.
impl<H> From<(
    *mut FFIFileManifestParent<H>,
    usize,
    *mut FFIChunkIndexEntry<H>,
    usize,
    *const H,
    usize
)> for FFISerializedOutput<H>{
    fn from((
        ser_manifest_ptr,
        ser_manifest_len,
        ser_chunk_index_ptr,
        ser_chunk_index_len,
        sorted_hashes_ptr,
        sorted_hashes_len
    ): (
        *mut FFIFileManifestParent<H>,
        usize,
        *mut FFIChunkIndexEntry<H>,
        usize,
        *const H,
        usize)) -> Self {
        Self {
            ser_manifest_ptr,
            ser_manifest_len,
            ser_chunk_index_ptr,
            ser_chunk_index_len,
            sorted_hashes_ptr,
            sorted_hashes_len
        }
    }
}

/// FFI-safe equivalent of `SSAChunkMeta`, containing the metadata for a single
/// chunk within a file's manifest.
///
/// This struct holds the essential information needed to identify and correctly
/// place a data chunk during file reconstruction. An array of these structs,
/// sorted by `offset`, forms a key part of the `FFIFileManifestParent`.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used to uniquely
///   identify the chunk's content.
///
/// # Fields
///
/// * `hash`: The unique deduplication hash of the chunk's data. This is used
///   to look up the chunk's compressed data in the archive's chunk index.
/// * `offset`: The starting byte position of this chunk within the original,
///   uncompressed file.
/// * `length`: The size of the chunk in bytes in its original, uncompressed
///   form.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FFISSAChunkMeta<H> {
    pub hash: H,
    pub offset: u64,
    pub length: u32,
}

/// An `FFISSAChunkMeta` specialized for `u64` hashes.
///
/// This type is used within the `FFIFileManifestParentU64` to describe a
/// single chunk's metadata using a 64-bit hash.
pub type FFISSAChunkMetaU64 = FFISSAChunkMeta<u64>;

/// An `FFISSAChunkMeta` specialized for `u128` hashes.
///
/// This type is used within the `FFIFileManifestParentU128` to describe a
/// single chunk's metadata using a 128-bit hash.
pub type FFISSAChunkMetaU128 = FFISSAChunkMeta<u128>;

// Converts a reference to an FFI-safe `FFISSAChunkMeta` into the Rust-native
// `SSAChunkMeta` struct.
//
// This is used when reconstructing a `FileManifestParent` from its FFI
// representation, allowing for a direct and idiomatic conversion of the
// chunk metadata array.
impl<H> From<&FFISSAChunkMeta<H>> for SSAChunkMeta<H>
where
    H: Copy,
{
    fn from(meta: &FFISSAChunkMeta<H>) -> Self {
        SSAChunkMeta {
            hash: meta.hash,
            offset: meta.offset,
            length: meta.length,
        }
    }
}

// Converts a Rust-native `SSAChunkMeta` struct into the FFI-safe
// `FFISSAChunkMeta` struct.
//
// This is used when preparing a `FileManifestParent` to be passed to a C
// caller, allowing for a direct and idiomiatic conversion of the chunk
// metadata array.
impl<H: Copy> From<SSAChunkMeta<H>> for FFISSAChunkMeta<H> {
    fn from(meta: SSAChunkMeta<H>) -> Self {
        Self {
            hash: meta.hash,
            offset: meta.offset,
            length: meta.length,
        }
    }
}


/// A wrapper for a mutable C `void` pointer, used to pass user-defined
/// context data across the FFI boundary.
///
/// This struct allows a C caller to provide an opaque pointer to its own
/// state or context. Rust code can then pass this pointer back to C-defined
/// callback functions, enabling them to access their original environment
/// without Rust needing to know anything about the data's structure.
///
/// For example, if the C application manages its state in a struct, it can
/// pass a pointer to that struct as `user_data`. When Rust invokes a C
/// callback, it includes this pointer, allowing the callback to safely cast
/// it back to its original type and access its fields.
#[derive(Clone, Copy)]
pub struct FFIUserData(pub *mut c_void);

/// Represents a C-compatible, heap-allocated byte vector.
///
/// This struct is used to pass ownership of a dynamically sized byte array
/// from C to Rust. It is a fundamental building block for more complex data
/// structures, such as `FFIChunkDataArray`, which represents an array of these
/// byte vectors.
///
/// # Fields
///
/// * `ptr`: A raw pointer to the beginning of the heap-allocated byte array.
/// * `len`: The number of bytes in the array pointed to by `ptr`.
///
/// # Memory Management
///
/// When a C callback returns this struct to Rust, Rust takes full ownership
/// of the memory pointed to by `ptr`. Rust will deallocate this memory using
/// `Vec::from_raw_parts` when it is no longer needed. The C caller **must
/// not** free this memory after returning it to Rust.
#[repr(C)]
pub struct FFIVecBytes {
    pub ptr: *mut u8,
    pub len: usize,
}

/// Represents a single entry in a key-value map of filenames to their
/// SHA-512 verification hashes.
///
/// This struct is used to pass collection of file verification hashes from
/// a C caller to Rust. An array of these entries can be used to provide the
/// `verify_single_file_ffi_*` functions with the necessary checksums to
/// confirm file integrity.
///
/// # Fields
///
/// * `key`: A pointer to a null-terminated C string representing the filename.
/// * `value`: A pointer to a 64-byte array containing the SHA-512 hash of the
///   file's original, uncompressed content.
///
/// # Safety
///
/// The C caller retains ownership of the memory pointed to by `key` and
/// `value`. Rust will only read from these pointers for the duration of the
/// FFI call and will not attempt to deallocate them. The C caller must ensure
/// that these pointers are valid, non-null, and point to readable memory for
/// the lifetime of the call.
#[repr(C)]
pub struct FFIVeriHashesEntry {
    pub key: *const c_char,
    pub value: *const [u8; 64],
}

/// A thread-safe wrapper for a mutable C `void` pointer, enabling it to be
/// safely sent across thread boundaries.
///
/// This struct is a variant of [`FFIUserData`] specifically designed for use in
/// concurrent operations. By implementing `Send` and `Sync`, it signals to the
/// Rust compiler that the raw pointer it contains can be safely transferred
/// and accessed from multiple threads.
///
/// This is crucial for multi-threaded FFI functions, such as the parallel
/// compression pipeline in the `ArchiveBuilder`, where a C-provided context
/// pointer needs to be accessible to worker threads.
///
/// # Safety
///
/// The `unsafe impl Send for ThreadSafeUserData` and `unsafe impl Sync for
/// ThreadSafeUserData` declarations are a contract with the Rust compiler.
/// The C caller **must** guarantee that the `*mut c_void` pointer passed into
/// Rust is itself thread-safe. This means that any data it points to must be
/// protected against data races (e.g., by using mutexes or other
/// synchronization primitives on the C side). Failure to ensure the thread
/// safety of the underlying C data will result in undefined behavior when it
/// is accessed concurrently.
#[derive(Clone, Copy)]
pub struct ThreadSafeUserData(pub *mut c_void);

// This unsafe implementation marks `ThreadSafeUserData` as `Send`, allowing it
// to be transferred across thread boundaries.
//
// # Safety
//
// This is a contract with the Rust compiler. The C caller MUST ensure that
// the underlying `*mut c_void` pointer is safe to send to another thread.
// This typically means the data it points to is either immutable or managed
// by thread-safe mechanisms (like mutexes) on the C side.
unsafe impl Send for ThreadSafeUserData {}

// This unsafe implementation marks `ThreadSafeUserData` as `Sync`, allowing it
// to be accessed from multiple threads simultaneously (via a shared reference).
//
// # Safety
//
// This is a contract with the Rust compiler. The C caller MUST ensure that
// the underlying `*mut c_void` pointer is safe to be accessed concurrently.
// This means the data it points to must be protected against data races.
// For example, the C code might use its own mutex to protect the data
// pointed to by this `void` pointer. Failure to ensure this will result in
// undefined behavior.
unsafe impl Sync for ThreadSafeUserData {}
