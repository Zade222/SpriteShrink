//! Defines the core data structures used throughout the sprite-shrink library.
//!
//! This module contains all fundamental structs that represent the various
//! components of a sprite-shrink archive. These structures are used for
//! everything from in-memory representation of files to the final
//! serialized format on disk. They are designed to be efficient for both
//! compression and extraction operations.

use std::collections::HashMap;
use bytemuck::{Pod, Zeroable};
use fastcdc::v2020::{Chunk};
use serde::{Deserialize, Serialize};
use zerocopy::Immutable;
use zerocopy::{IntoBytes, FromBytes};

/// Represents the location of a data chunk within the archive.
///
/// This struct is used to pinpoint the exact position and size of a
/// compressed data chunk in the final archive file. It contains the
/// starting offset and the total length of the chunk in bytes. An index
/// of these structs is used during extraction to quickly find data.
///
/// # Fields
///
/// * `offset`: The starting position of the chunk in bytes, relative
///   to the beginning of the archive's data section.
/// * `compressed_length`: The size of the compressed chunk in bytes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ChunkLocation {
    pub offset: u64,
    pub compressed_length: u32,
}

/// Defines the header structure for an archive file.
///
/// This struct represents the first block of data in an archive. It acts
/// as a primary index, providing metadata required to parse the rest of
/// the file. It includes a magic number, version, and the offsets and
/// lengths of all major data sections, such as the file manifest, chunk
/// index, and compressed data.
///
/// # Fields
///
/// * `magic_num`: A 4-byte array to identify the file as a valid archive.
/// * `file_version`: The version number of the archive format.
/// * `file_count`: The total number of files stored in the archive.
/// * `algorithm`: Numerical value representing the compression used on the
///   compressed data.
/// * `hash_type`: Numerical value representing the hash type used when
///   the data was hashed. 1 = xxhash3_64, 2 = xxhash3_128
/// * `pad`: Empty data padding to keep data aligned. Must be all zeros.
/// * `man_offset`: The byte offset where the file manifest begins.
/// * `man_length`: The total length of the file manifest in bytes.
/// * `dict_offset`: The starting offset of the compression dictionary.
/// * `dict_length`: The total length of the dictionary in bytes.
/// * `chunk_index_offset`: The byte offset for the chunk index.
/// * `chunk_index_length`: The total length of the chunk index.
/// * `data_offset`: The starting offset of the compressed file data.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, Debug, Copy, Clone, Pod, Zeroable)]
pub struct FileHeader {
    pub magic_num:      [u8; 8],
    pub file_version:   u32,
    pub file_count:     u32,
    pub algorithm:      u16,
    pub hash_type:      u8,
    pub pad:            [u8; 5],
    pub man_offset:     u64,
    pub man_length:     u64,
    pub dict_offset:    u64,
    pub dict_length:    u64,
    pub chunk_index_offset: u64,
    pub chunk_index_length: u64,
    pub data_offset:    u64
}

/// Encapsulates the in-memory representation of a single file.
///
/// This struct holds a file's essential data once it has been read from
/// disk. It contains the file's name and its entire contents as a byte
/// vector. This allows the application to pass file data around in a
/// simple, self-contained package for processing without further I/O.
///
/// # Fields
///
/// * `file_name`: A `String` that stores the base name of the file.
/// * `file_data`: A `Vec<u8>` containing the complete binary contents.
#[derive(Clone, Debug)]
pub struct FileData{
    pub file_name:  String,
    pub file_data:  Vec<u8>
}

/// Describes the structure of a single file within the archive.
///
/// This struct holds all metadata needed to reconstruct a file from its
/// constituent data chunks. It includes the original filename, a count
/// of how many chunks the file is made of, and a vector of `SSAChunkMeta`
/// structs. Each of these contains the hash needed to locate the chunk's
/// data within the main archive.
///
/// # Fields
///
/// * `filename`: The original name of the file.
/// * `chunk_count`: The total number of chunks that make up the file.
/// * `chunk_metadata`: A vector of metadata for each chunk, sorted in
///   the order needed for reconstruction.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FileManifestParent<H> {
    pub filename:       String,
    pub chunk_count:    u64,
    pub chunk_metadata: Vec<SSAChunkMeta<H>>,
}

/// Holds the results of the initial file processing stage.
///
/// This struct is an intermediate container that bundles all critical
/// information generated after a single file has been read and analyzed.
/// It includes the file's name, its binary data, a list of its data
/// chunks, and a verification hash. This data is then passed to the next
/// stage of the compression pipeline.
///
/// # Fields
///
/// * `file_name`: The name of the processed file.
/// * `veri_hash`: A SHA-512 hash of the original file's contents, used
///   to ensure data integrity.
/// * `chunks`: A vector of `Chunk` objects, where each object represents
///   a distinct segment of the file's data.
/// * `file_data`: A byte vector holding the entire file contents, used
///   for chunk reconstruction and verification.
pub struct ProcessedFileData {
    pub file_name: String,
    pub veri_hash: [u8; 64],
    pub chunks: Vec<Chunk>,
    pub file_data: Vec<u8>,
}

/// Represents the state of a long-running operation in the library.
///
/// This enum is used with a callback function to report the progress of
/// time-consuming tasks, such as compression or dictionary creation. It
/// allows the calling application to provide detailed feedback to the user.
///
/// # Variants
///
/// * `GeneratingDictionary` - Indicates that the compression dictionary
///   generation process has started.
/// * `DictionaryDone` - Signals that the dictionary has been successfully
///   created.
/// * `Compressing` - Reports that the data chunk compression phase has
///   begun. It includes the total number of chunks to be processed.
/// * `ChunkCompressed` - Sent after each individual data chunk is
///   compressed. This is useful for incrementing a progress bar.
/// * `Finalizing` - Indicates that the final archive assembly is in
///   progress.
#[derive(Debug, Clone)]
pub enum Progress {
    /// Dictionary generation has started.
    GeneratingDictionary,
    /// Dictionary generation is complete.
    DictionaryDone,
    /// Compression has started, reports total chunks to compress.
    Compressing { total_chunks: u64 },
    /// Reports that a single chunk has been compressed.
    ChunkCompressed,
    /// Finalizing the archive file.
    Finalizing,
}


/// Metadata describing a single chunk that must be read for a seek request.
///
/// `SeekMetadata` is produced by [`get_seek_chunks`]. It contains the hash of
/// the required chunk together with the byte range (relative to the start of
/// that chunk) that should be copied after the chunk has been decompressed.
///
/// The `start_offset` is inclusive and the `end_offset` is exclusive,
/// mirroring the semantics used throughout the library for slice ranges.
///
/// # Fields
/// * `hash`: The unique identifier for the data chunk.
/// * `start_offset`:  Inclusive start offset within the chunk where the
///   requested data begins.
/// * `end_offset`: Exclusive end offset within the chunk where the needed data
///   ends.
///
/// # Type parameters
///
/// * `H` – The hash type used by the archive (e.g. `u64` or `u128`). It must
///   implement the traits required by the surrounding APIs (`Copy`, `Eq`,
///   etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekMetadata<H>{
    pub hash: H,
    pub start_offset: u64,
    pub end_offset: u64
}

/// Implementation of helper method for [`SeekMetadata`].
///
/// This `impl` provides a convenient constructor that creates a
/// fully‑initialized `SeekMetadata` value in a single call. Using the
/// constructor makes call‑sites clearer and avoids having to repeat the
/// field names each time a new instance is needed.
///
/// # Type parameter
///
/// * `H` – The hash type used by the archive (e.g. `u64`, `u128`). The type is
///   generic so the same struct can be used with any hash algorithm that the
///   library supports.
impl<H> SeekMetadata<H> {
    pub fn new(hash: H, start_offset: u64, end_offset: u64) -> Self {
        Self { hash, start_offset, end_offset }
    }
}

/// Holds all data produced by a single *uncompressed* serialization pass.
/// The three fields together constitute the complete description of an
/// archive before the actual byte payload is written.
///
/// # Type Parameters
/// * `H` – The hash type used to identify a chunk. It must implement
///   `Copy`, `Ord`, `Eq`, `Hash` and `Display`.
///
/// # Fields
/// * `ser_file_manifest` – A vector of `FileManifestParent<H>` records, one
///   per file in the archive. The vector is sorted by filename and each
///   file’s `chunk_metadata` slice is sorted by the original byte offset,
///   making it easy to rebuild files during extraction.
/// * `chunk_index` – A map from each chunk hash (`H`) to its location
///   (`ChunkLocation`) within the final data blob. This index is produced by
///   `serialize_store` and is required for locating chunks when reading an
///   archive.
/// * `sorted_hashes` – The list of all unique chunk hashes, sorted in a
///   deterministic order. The order is used both for generating `chunk_index`
///   and for the later compression step that writes the actual chunk bytes.
#[derive(Debug)]
pub struct SerializedData<H>{
    pub ser_file_manifest: Vec<FileManifestParent<H>>,
    pub chunk_index: HashMap<H, ChunkLocation>,
    pub sorted_hashes: Vec<H>,
}

/// Implements a `Default` value for `SerializedData<H>`.
///
/// This implementation allows callers to create an empty `SerializedData`
/// instance without having to manually initialise each field. The generic type
/// `H` represents the hash type used throughout the archive and does not need
/// to implement `Default` because the default instance only contains empty
/// collections (`Vec` and `HashMap`).
///
/// # Fields
/// * `ser_file_manifest`: an empty `Vec<FileManifestParent<H>>`.
///   This will later hold a sorted list of file manifests needed to
///   reconstruct a file.
/// * `chunk_index`: an empty `HashMap<H, ChunkLocation>`.
///   The index maps each chunk hash to its location in the final data blob; it
///   is filled by `serialize_store` once the chunk data is known.
/// * `sorted_hashes`: an empty `Vec<H>`.
///   This vector will be populated with all unique chunk hashes in a
///   deterministic order before the chunk index is built.
impl<H> Default for SerializedData<H> {
    fn default() -> Self {
        SerializedData {
            ser_file_manifest: Vec::new(),
            chunk_index: HashMap::new(),
            sorted_hashes: Vec::new(),
        }
    }
}

/// Contains metadata for a single chunk in a file's manifest.
///
/// This struct stores essential information used to identify and place a
/// data chunk during file reconstruction. It holds the unique hash of
/// the chunk, which is used to look up its compressed data, and the
/// chunk's original offset and length within the uncompressed file.
///
/// # Fields
///
/// * `hash`: The unique identifier for the data chunk.
/// * `offset`: The starting position of this chunk in bytes within the
///   original, uncompressed file.
/// * `length`: The size of the chunk in bytes.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct SSAChunkMeta<H>{
    pub hash:   H,
    pub offset: u64,
    pub length: u32,
}
