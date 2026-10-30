//! Defines the core data structures used throughout the sprite-shrink library.
//!
//! This module contains all fundamental structs that represent the various
//! components of a sprite-shrink archive. These structures are used for
//! everything from in-memory representation of files to the final
//! serialized format on disk. They are designed to be efficient for both
//! compression and extraction operations.

use std::{
    collections::HashMap,
    mem
};
use bitcode::{
    Decode, Encode
};
use bytemuck::{Pod, Zeroable};
use fastcdc::v2020::{Chunk};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, FromBytes};

use crate::{
    MAGIC_NUMBER, SUPPORTED_VERSION,
    lib_error_handling::SpriteShrinkError,
    parsing::ParsingError,
    serialization::SerializationError,
};


pub const SSMC_UID: u16 = 0x0000;

/// The `algorithm` value in [`FileHeader`] for archives whose data is
/// compressed with zstd.
pub const ALGORITHM_ZSTD: u16 = 98;

/// The `hash_type` value in [`FileHeader`] for archives whose chunks are
/// identified by 64-bit xxHash3 hashes (`u64`).
pub const HASH_TYPE_XXH3_64: u8 = 1;

/// The `hash_type` value in [`FileHeader`] for archives whose chunks are
/// identified by 128-bit xxHash3 hashes (`u128`).
pub const HASH_TYPE_XXH3_128: u8 = 2;

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
#[derive(Clone, Copy, Debug, Decode, Encode, Serialize, Deserialize)]
pub struct ChunkLocation {
    pub offset: u64,
    pub compressed_length: u32,
}

/// The compressed sections that [`ArchiveBuilder::build`] produces, ready to
/// be placed in an archive's metadata prefix.
///
/// The compressed chunk data isn't included: `build` hands it to the
/// caller's write callback as it's produced.
///
/// # Fields
///
/// * `dictionary`: The zstd dictionary the chunks were compressed with.
/// * `dictionary_size`: The length of `dictionary` in bytes.
/// * `enc_chunk_index`: The bitcode-encoded chunk index, mapping each chunk
///   hash to its location in the compressed data.
/// * `enc_chunk_index_size`: The length of `enc_chunk_index` in bytes.
///
/// [`ArchiveBuilder::build`]: crate::ArchiveBuilder::build
#[derive(Clone, Debug)]
pub struct CompressionResult {
    pub dictionary: Vec<u8>,
    pub dictionary_size: u64,
    pub enc_chunk_index: Vec<u8>,
    pub enc_chunk_index_size: u64
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
/// * `_pad1`: Empty data padding to keep data aligned. Must be zero.
/// * `pub format_id`: Field for specifying the ID of the format specific
///   comrpession that was used for making the archive.
/// * `_pad2`: Empty data padding to keep data aligned. Must be zeros.
/// * `man_offset`: The byte offset where the file manifest begins.
/// * `man_length`: The total length of the file manifest in bytes.
/// * `dict_offset`: The starting offset of the compression dictionary.
/// * `dict_length`: The total length of the dictionary in bytes.
/// * `chunk_index_offset`: The byte offset for the chunk index.
/// * `chunk_index_length`: The total length of the chunk index.
/// * `data_offset`: The starting offset of the compressed file data.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, PartialEq, Pod,
    Zeroable
)]
pub struct FileHeader {
    pub magic_num:          [u8; 8],
    pub file_version:       u32,
    pub file_count:         u32,
    pub algorithm:          u16,
    pub hash_type:          u8,
    pub _pad1:              u8,
    pub format_id:          u16,
    pub _pad2:              [u8; 2],
    pub enc_toc_offset:     u32,
    pub enc_toc_length:     u32,
    pub format_data_offset: u32,
}

impl FileHeader {
    pub const HEADER_SIZE: u32 = mem::size_of::<FileHeader>() as u32;

    /// Builds the header for an archive of `file_count` files whose encoded
    /// table of contents is `enc_toc_length` bytes. The TOC directly follows
    /// the header, and the format data directly follows the TOC.
    ///
    /// The counts are taken as `usize`, the lengths of the caller's buffers,
    /// so the checks against the header's 32-bit fields happen here.
    ///
    /// # Errors
    ///
    /// Returns `SerializationError::ArchiveTooLarge` if `file_count` doesn't
    /// fit in a `u32`, or the format data offset,
    /// `HEADER_SIZE + enc_toc_length`, doesn't.
    pub fn build_file_header(
        file_count: usize,
        algorithm_code: u16,
        hash_type: u8,
        format_id: u16,
        enc_toc_length: usize,
    ) -> Result<FileHeader, SpriteShrinkError> {
        let file_count = u32::try_from(file_count).ok().ok_or_else(|| {
            SerializationError::ArchiveTooLarge(format!("{file_count} files"))
        })?;

        let toc_too_large = || SerializationError::ArchiveTooLarge(format!(
            "the table of contents is {enc_toc_length} bytes"
        ));
        let enc_toc_length = u32::try_from(enc_toc_length)
            .ok()
            .ok_or_else(toc_too_large)?;
        let format_data_offset = Self::HEADER_SIZE
            .checked_add(enc_toc_length)
            .ok_or_else(toc_too_large)?;

        Ok(FileHeader {
            magic_num:      MAGIC_NUMBER,
            file_version:   SUPPORTED_VERSION,
            file_count,
            algorithm:      algorithm_code,
            hash_type,
            _pad1:          0,
            format_id,
            _pad2:          [0, 0],
            enc_toc_offset:     Self::HEADER_SIZE,
            enc_toc_length,
            format_data_offset,
        })
    }

    /// Returns the header as it's stored in an archive: the fields in
    /// declaration order, with every multi-byte integer little-endian
    /// whatever the host's byte order. The padding bytes are always written
    /// as zero.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; size_of::<Self>()] {
        bytemuck::cast(FileHeader {
            magic_num:          self.magic_num,
            file_version:       self.file_version.to_le(),
            file_count:         self.file_count.to_le(),
            algorithm:          self.algorithm.to_le(),
            hash_type:          self.hash_type,
            _pad1:              0,
            format_id:          self.format_id.to_le(),
            _pad2:              [0; 2],
            enc_toc_offset:     self.enc_toc_offset.to_le(),
            enc_toc_length:     self.enc_toc_length.to_le(),
            format_data_offset: self.format_data_offset.to_le(),
        })
    }

    /// Reads a header written by [`FileHeader::to_le_bytes`]. The bytes are
    /// copied, so `bytes` needs no particular alignment.
    ///
    /// This only converts the bytes, and ignores the padding.
    /// [`parse_file_header`] also checks the magic number and version.
    ///
    /// [`parse_file_header`]: crate::parse_file_header
    #[must_use]
    pub fn from_le_bytes(bytes: &[u8; size_of::<Self>()]) -> Self {
        let raw: FileHeader = bytemuck::cast(*bytes);
        FileHeader {
            magic_num:          raw.magic_num,
            file_version:       u32::from_le(raw.file_version),
            file_count:         u32::from_le(raw.file_count),
            algorithm:          u16::from_le(raw.algorithm),
            hash_type:          raw.hash_type,
            _pad1:              0,
            format_id:          u16::from_le(raw.format_id),
            _pad2:              [0; 2],
            enc_toc_offset:     u32::from_le(raw.enc_toc_offset),
            enc_toc_length:     u32::from_le(raw.enc_toc_length),
            format_data_offset: u32::from_le(raw.format_data_offset),
        }
    }
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
#[derive(Clone, Debug, Decode, Deserialize, Encode, Serialize)]
pub struct FileManifestParent<H> {
    pub chunk_count:    u64,
    pub chunk_metadata: Vec<SSAChunkMeta<H>>,
}


#[repr(C)]
#[derive(
    Clone, Copy, Debug, Decode, Deserialize, Encode, Eq, FromBytes, Immutable,
    IntoBytes, PartialEq, Pod, Serialize, Zeroable
)]
pub struct FileRegion {
    pub offset: u64,
    pub length: u64,
}

impl FileRegion {
    /// Converts both fields from native to little-endian byte order, like
    /// `u64::to_le`. Used by the format data structs' `to_le_bytes`.
    #[must_use]
    pub fn to_le(region: Self) -> Self {
        FileRegion {
            offset: region.offset.to_le(),
            length: region.length.to_le(),
        }
    }

    /// Converts both fields from little-endian to native byte order, like
    /// `u64::from_le`. Used by the format data structs' `from_le_bytes`.
    #[must_use]
    pub fn from_le(region: Self) -> Self {
        FileRegion {
            offset: u64::from_le(region.offset),
            length: u64::from_le(region.length),
        }
    }

    /// Returns the bytes this region covers in `bytes`, which must start at
    /// the beginning of the archive. `section` names the region in the
    /// error message.
    ///
    /// # Errors
    ///
    /// Returns `ParsingError::SectionOutOfBounds` if the region ends past
    /// the end of `bytes`, or its end doesn't fit in a `u64`.
    pub fn bytes_in<'a>(
        &self,
        bytes: &'a [u8],
        section: &str,
    ) -> Result<&'a [u8], SpriteShrinkError> {
        let range = self.offset.checked_add(self.length).and_then(|end| {
            Some(usize::try_from(self.offset).ok()?..usize::try_from(end).ok()?)
        });

        range.and_then(|range| bytes.get(range)).ok_or_else(|| {
            ParsingError::SectionOutOfBounds(format!(
                "The {section} at offset {} with length {} doesn't fit in \
                {} bytes.",
                self.offset,
                self.length,
                bytes.len()
            )).into()
        })
    }
}

/// The metadata of an SSMC archive, read from its prefix by
/// [`parse_ssmc_prefix`].
///
/// [`parse_ssmc_prefix`]: crate::parse_ssmc_prefix
#[derive(Clone, Debug)]
pub struct ParsedSSMC<H> {
    pub header:      FileHeader,
    pub format_data: SSMCFormatData,
    pub toc:         Vec<SSMCTocEntry>,
    pub manifests:   Vec<FileManifestParent<H>>,
    pub dictionary:  Vec<u8>,
    /// Where each chunk is stored. Unlike the encoded index, each `offset`
    /// is a position in the archive file, so a host can seek straight to
    /// it.
    pub chunk_index: HashMap<H, ChunkLocation>,
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
    pub archive_toc: Vec<SSMCTocEntry>,
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
            archive_toc: Vec::new(),
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
#[derive(Clone, Copy, Debug, Decode, Deserialize, Encode, Serialize)]
pub struct SSAChunkMeta<H>{
    pub hash:   H,
    pub offset: u64,
    pub length: u32,
}

#[repr(C)]
#[derive(
    Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, PartialEq, Pod,
    Zeroable
)]
pub struct SSMCFormatData {
    pub enc_manifest:       FileRegion,
    pub data_dictionary:    FileRegion,
    pub enc_chunk_index:    FileRegion,
    pub data_offset:        u64,
}


impl SSMCFormatData {
    pub const SIZE: u64 = mem::size_of::<SSMCFormatData>() as u64;

    pub fn build_format_data(
        format_data_offset: usize,
        enc_man_length: usize,
        data_dict_length: u64,
        enc_chunk_idx_length: u64,
    ) -> Self {
        let enc_man_offset = format_data_offset as u64 + Self::SIZE;
        let data_dict_offset = enc_man_offset + enc_man_length as u64;
        let enc_chunk_idx_offset = data_dict_offset + data_dict_length;
        let data_offset = enc_chunk_idx_offset + enc_chunk_idx_length;


        SSMCFormatData {
            enc_manifest: FileRegion {
                offset: enc_man_offset,
                length: enc_man_length as u64
            },
            data_dictionary: FileRegion {
                offset: data_dict_offset,
                length: data_dict_length
            },
            enc_chunk_index: FileRegion {
                offset: enc_chunk_idx_offset,
                length: enc_chunk_idx_length
            },
            data_offset,
        }
    }

    /// Where the archive's metadata ends and the compressed data begins.
    /// Every byte before this offset is part of the prefix that
    /// [`build_ssmc_prefix`] returns, so a host that reads this far has
    /// everything it needs to locate any chunk.
    ///
    /// [`build_ssmc_prefix`]: crate::build_ssmc_prefix
    #[must_use]
    pub fn metadata_prefix_end(&self) -> u64 {
        self.data_offset
    }

    /// Returns the format data as it's stored in an archive, with every
    /// integer little-endian whatever the host's byte order.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; size_of::<Self>()] {
        bytemuck::cast(SSMCFormatData {
            enc_manifest:    FileRegion::to_le(self.enc_manifest),
            data_dictionary: FileRegion::to_le(self.data_dictionary),
            enc_chunk_index: FileRegion::to_le(self.enc_chunk_index),
            data_offset:     self.data_offset.to_le(),
        })
    }

    /// Reads format data written by [`SSMCFormatData::to_le_bytes`]. The
    /// bytes are copied, so `bytes` needs no particular alignment.
    #[must_use]
    pub fn from_le_bytes(bytes: &[u8; size_of::<Self>()]) -> Self {
        let raw: SSMCFormatData = bytemuck::cast(*bytes);
        SSMCFormatData {
            enc_manifest:    FileRegion::from_le(raw.enc_manifest),
            data_dictionary: FileRegion::from_le(raw.data_dictionary),
            enc_chunk_index: FileRegion::from_le(raw.enc_chunk_index),
            data_offset:     u64::from_le(raw.data_offset),
        }
    }
}

#[derive(Decode, Encode, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SSMCTocEntry {
    pub filename: String,
    pub uncompressed_size: u64,
}
