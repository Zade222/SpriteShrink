//! Provides functions for parsing and validating sprite-shrink archives.
//!
//! This module contains the logic required to read the binary data from
//! an archive file and interpret its structure. It includes functions
//! for parsing the main file header, the file manifest, and the chunk
//! index. It also defines critical constants, such as the magic number
//! and supported version, to ensure file integrity and compatibility.

use std::collections::HashMap;

use bitcode::{
    Decode, decode
};
use serde::{Deserialize};
use thiserror::Error;

use crate::lib_error_handling::SpriteShrinkError;
use crate::lib_structs::{
    ChunkLocation, FileHeader, FileManifestParent, SSMCTocEntry,
    SSMCFormatData
};

#[derive(Error, Debug)]
pub enum ParsingError {
    #[error("Failed to decode chunk index: {0}")]
    IndexDecodeError(String),

    #[error("Read file is of a newer version than what this library supports.")]
    InvalidFileVersion(),

    #[error("File header is malformed. {0}")]
    InvalidHeader(String),

    #[error("Format data is malformed. {0}")]
    InvalidFormatData(String),

    #[error("Failed to decode file manifest: {0}")]
    ManifestDecodeError(String),

    #[error("Failed to decode table of contents: {0}")]
    TOCDecodeError(String)
}

/// The magic number used to identify a sprite-shrink archive file.
///
/// This 8-byte signature is at the beginning of the file and is used to
/// quickly verify that the file is a valid archive before parsing. The
/// value is "SSARCHV1".
#[unsafe(no_mangle)]
pub static MAGIC_NUMBER: [u8; 8] = *b"SSARCHV1";

/// The latest archive format version that this library supports.
///
/// This constant is used during header parsing to ensure the library
/// does not attempt to read files created by a newer, incompatible
/// version of the software.
pub static SUPPORTED_VERSION: u32 = 0x00020000;

/// The seed value used for deterministic chunking and hashing.
///
/// This constant is passed to the FastCDC chunking algorithm and the
/// xxHash hashing function. Using a fixed seed ensures that the same
/// file will always produce the same set of chunks and hashes, which is
/// critical for reliable deduplication.
pub static SS_SEED: u64 = 0x4202803010192019;


/// Parses the binary header data from a sprite-shrink archive.
///
/// This function takes a slice of bytes from the beginning of an archive
/// and interprets it as a `FileHeader`. It performs critical validation,
/// such as checking the magic number and ensuring the archive version
/// is supported by the current library.
///
/// # Arguments
///
/// * `header_data`: A byte slice of the raw header data from an archive.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(FileHeader)` containing the parsed and validated header.
/// - `Err(SpriteShrinkError)` if the data is malformed, the magic number is
///   incorrect, or the file version is unsupported.
pub fn parse_file_header(
    header_data: &[u8]
) -> Result<FileHeader, SpriteShrinkError> {
    //Attempt to cast the byte slice to a FileHeader.
    let file_header =
        bytemuck::try_from_bytes::<FileHeader>(header_data)
        .map_err(|e| ParsingError::InvalidHeader(e.to_string()))?;

    //Verify the magic number to ensure it's the correct file type.
    if file_header.magic_num != MAGIC_NUMBER {
        return Err(ParsingError::InvalidHeader(
            "File Magic Number is invalid.".to_string(),
        ).into());
    }

    //Check if the file version is supported by this library version.
    if file_header.file_version > SUPPORTED_VERSION {
        return Err(ParsingError::InvalidFileVersion().into());
    };

    Ok(*file_header)
}

/// Deserializes the file manifest from a raw byte slice.
///
/// This function is responsible for parsing the binary representation
/// of the file manifest, which contains metadata for every file in the
/// archive. It uses `bitcode` to decode the byte slice into a
/// structured `Vec<FileManifestParent>`.
///
/// # Arguments
///
/// * `manifest_data`: A byte slice holding the binary-encoded manifest.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<FileManifestParent>)` containing the parsed file manifests.
/// - `Err(SpriteShrinkError)` if the byte slice cannot be decoded due to data
///   corruption or a format mismatch.
pub fn parse_file_metadata<H>(
    manifest_data: &[u8]
) -> Result<Vec<FileManifestParent<H>>, SpriteShrinkError>
where
    for<'de> H: serde::Deserialize<'de> + Decode<'de>
{
    let file_manifest = decode(manifest_data)
        .map_err(|e| ParsingError::ManifestDecodeError(e.to_string()))?;

    Ok(file_manifest)
}

/// Deserializes the chunk index from a raw byte slice.
///
/// This function parses the binary data representing the chunk index,
/// which maps each unique chunk hash to its `ChunkLocation`. It uses
/// `bitcode` to decode the data into a `HashMap` for efficient lookups
/// during file extraction.
///
/// # Arguments
///
/// * `chunk_index_data`: A byte slice of the binary-encoded chunk index.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(HashMap<u64, ChunkLocation>)` containing the parsed index.
/// - `Err(SpriteShrinkError)` if the byte slice cannot be decoded.
pub fn parse_file_chunk_index<H>(
    chunk_index_data: &[u8]
) -> Result<HashMap<H, ChunkLocation>, SpriteShrinkError>
where
    for<'de> H: Eq + std::hash::Hash + Deserialize<'de> + Decode<'de>,
{
    let bin_chunk_index: Vec<(H, ChunkLocation)> = decode(chunk_index_data)
        .map_err(|e| ParsingError::IndexDecodeError(e.to_string()))?;

    let chunk_index: HashMap<H, ChunkLocation> = bin_chunk_index
        .into_iter()
        .collect();

    Ok(chunk_index)
}


pub fn parse_file_toc(
    enc_toc_data: &[u8]
) -> Result<Vec<SSMCTocEntry>, SpriteShrinkError> {
    let bin_toc: Vec<SSMCTocEntry> = decode(enc_toc_data)
        .map_err(|e| ParsingError::TOCDecodeError(e.to_string()))?;

    Ok(bin_toc)
}


pub fn parse_format_data(
    format_data: &[u8]
) -> Result<SSMCFormatData, SpriteShrinkError> {
    let parsed_format_data = bytemuck::try_from_bytes::<SSMCFormatData>(format_data)
        .map_err(|e| ParsingError::InvalidFormatData(e.to_string()))?;

    Ok(*parsed_format_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib_structs::{SSAChunkMeta, SSMCFormatData};

    fn valid_header() -> FileHeader {
        FileHeader::build_file_header(2, 1, 1, 0, 64)
    }

    // --- Constants ---

    #[test]
    fn test_magic_number_spells_ssarchv1() {
        assert_eq!(&MAGIC_NUMBER, b"SSARCHV1");
    }

    #[test]
    fn test_ss_seed_has_expected_value() {
        assert_eq!(SS_SEED, 0x4202803010192019);
    }

    // --- parse_file_header ---

    #[test]
    fn test_parse_file_header_succeeds_with_valid_header() {
        let header = valid_header();
        let result = parse_file_header(bytemuck::bytes_of(&header)).unwrap();
        assert_eq!(result.magic_num, MAGIC_NUMBER);
        assert_eq!(result.file_version, SUPPORTED_VERSION);
        assert_eq!(result.file_count, 2);
    }

    #[test]
    fn test_parse_file_header_rejects_wrong_magic_number() {
        let mut header = valid_header();
        header.magic_num = *b"BADMAGIC";
        let result = parse_file_header(bytemuck::bytes_of(&header));
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::InvalidHeader(_)))));
    }

    #[test]
    fn test_parse_file_header_rejects_newer_file_version() {
        let mut header = valid_header();
        header.file_version = SUPPORTED_VERSION + 1;
        let result = parse_file_header(bytemuck::bytes_of(&header));
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::InvalidFileVersion()))));
    }

    #[test]
    fn test_parse_file_header_accepts_older_file_version() {
        let mut header = valid_header();
        header.file_version = SUPPORTED_VERSION - 1;
        let result = parse_file_header(bytemuck::bytes_of(&header));
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_file_header_accepts_current_file_version() {
        let header = valid_header();
        let result = parse_file_header(bytemuck::bytes_of(&header));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().file_version, SUPPORTED_VERSION);
    }

    #[test]
    fn test_parse_file_header_rejects_slice_too_small() {
        let result = parse_file_header(&[0u8; 4]);
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::InvalidHeader(_)))));
    }

    #[test]
    fn test_parse_file_header_rejects_slice_too_large() {
        let bytes = vec![0u8; std::mem::size_of::<FileHeader>() + 1];
        let result = parse_file_header(&bytes);
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::InvalidHeader(_)))));
    }

    // --- parse_file_metadata ---

    #[test]
    fn test_parse_file_metadata_round_trips_manifest() {
        let manifests: Vec<FileManifestParent<u64>> = vec![FileManifestParent {
            chunk_count: 2,
            chunk_metadata: vec![
                SSAChunkMeta { hash: 0xdead_beef_u64, offset: 0,   length: 100 },
                SSAChunkMeta { hash: 0xcafe_babe_u64, offset: 100, length: 200 },
            ],
        }];
        let encoded = bitcode::encode(&manifests);
        let parsed = parse_file_metadata::<u64>(&encoded).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].chunk_count, 2);
        assert_eq!(parsed[0].chunk_metadata[0].hash, 0xdead_beef_u64);
        assert_eq!(parsed[0].chunk_metadata[1].offset, 100);
    }

    #[test]
    fn test_parse_file_metadata_round_trips_u128_hashes() {
        let manifests: Vec<FileManifestParent<u128>> = vec![FileManifestParent {
            chunk_count: 1,
            chunk_metadata: vec![
                SSAChunkMeta { hash: u128::MAX, offset: 0, length: 50 },
            ],
        }];
        let encoded = bitcode::encode(&manifests);
        let parsed = parse_file_metadata::<u128>(&encoded).unwrap();
        assert_eq!(parsed[0].chunk_metadata[0].hash, u128::MAX);
    }

    #[test]
    fn test_parse_file_metadata_returns_empty_vec_for_empty_manifest() {
        let encoded = bitcode::encode(&Vec::<FileManifestParent<u64>>::new());
        let parsed = parse_file_metadata::<u64>(&encoded).unwrap();
        assert_eq!(parsed.len(), 0);
    }

    #[test]
    fn test_parse_file_metadata_rejects_corrupt_data() {
        let result = parse_file_metadata::<u64>(b"not valid bitcode");
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::ManifestDecodeError(_)))));
    }

    // --- parse_file_chunk_index ---

    #[test]
    fn test_parse_file_chunk_index_round_trips_index() {
        let entries: Vec<(u64, ChunkLocation)> = vec![
            (0xaabb_u64, ChunkLocation { offset: 0,  compressed_length: 50 }),
            (0xccdd_u64, ChunkLocation { offset: 50, compressed_length: 75 }),
        ];
        let encoded = bitcode::encode(&entries);
        let map = parse_file_chunk_index::<u64>(&encoded).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&0xaabb_u64].offset, 0);
        assert_eq!(map[&0xccdd_u64].compressed_length, 75);
    }

    #[test]
    fn test_parse_file_chunk_index_returns_empty_map_for_empty_index() {
        let encoded = bitcode::encode(&Vec::<(u64, ChunkLocation)>::new());
        let map = parse_file_chunk_index::<u64>(&encoded).unwrap();
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn test_parse_file_chunk_index_rejects_corrupt_data() {
        let result = parse_file_chunk_index::<u64>(b"not valid bitcode");
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::IndexDecodeError(_)))));
    }

    // --- parse_file_toc ---

    #[test]
    fn test_parse_file_toc_round_trips_entries() {
        let toc = vec![
            SSMCTocEntry { filename: "sprite.png".into(), uncompressed_size: 4096 },
            SSMCTocEntry { filename: "level.dat".into(),  uncompressed_size: 1024 },
        ];
        let encoded = bitcode::encode(&toc);
        let parsed = parse_file_toc(&encoded).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].filename, "sprite.png");
        assert_eq!(parsed[1].uncompressed_size, 1024);
    }

    #[test]
    fn test_parse_file_toc_returns_empty_vec_for_empty_toc() {
        let encoded = bitcode::encode(&Vec::<SSMCTocEntry>::new());
        let parsed = parse_file_toc(&encoded).unwrap();
        assert_eq!(parsed.len(), 0);
    }

    #[test]
    fn test_parse_file_toc_rejects_corrupt_data() {
        let result = parse_file_toc(b"not valid bitcode");
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::TOCDecodeError(_)))));
    }

    // --- parse_format_data ---

    #[test]
    fn test_parse_format_data_round_trips_format_data() {
        let fd = SSMCFormatData::build_format_data(100, 200, 300, 400);
        let parsed = parse_format_data(bytemuck::bytes_of(&fd)).unwrap();
        assert_eq!(parsed.data_offset, fd.data_offset);
        assert_eq!(parsed.enc_manifest.length, fd.enc_manifest.length);
        assert_eq!(parsed.data_dictionary.offset, fd.data_dictionary.offset);
    }

    #[test]
    fn test_parse_format_data_rejects_slice_too_small() {
        let result = parse_format_data(&[0u8; 8]);
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::InvalidFormatData(_)))));
    }

    #[test]
    fn test_parse_format_data_rejects_slice_too_large() {
        let bytes = vec![0u8; std::mem::size_of::<SSMCFormatData>() + 1];
        let result = parse_format_data(&bytes);
        assert!(matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::InvalidFormatData(_)))));
    }
}
