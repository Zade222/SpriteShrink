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
    use bitcode::encode;

    use super::*;
    use crate::lib_structs::{SSAChunkMeta, SSMC_UID};

    /// Keeps test buffers 8-byte aligned so a test controls alignment
    /// explicitly instead of depending on where the compiler or allocator
    /// put the bytes.
    #[repr(C, align(8))]
    struct Aligned<const N: usize>([u8; N]);

    fn header_bytes(header: &FileHeader) -> Aligned<36> {
        let mut buf = Aligned([0_u8; 36]);
        buf.0.copy_from_slice(bytemuck::bytes_of(header));
        buf
    }

    fn sample_header() -> FileHeader {
        FileHeader::build_file_header(3, 98, 1, SSMC_UID, 40)
    }

    fn sample_manifests<H: Copy>(hashes: [H; 3]) -> Vec<FileManifestParent<H>> {
        vec![
            FileManifestParent {
                chunk_count: 2,
                chunk_metadata: vec![
                    SSAChunkMeta { hash: hashes[0], offset: 0, length: 4096 },
                    SSAChunkMeta { hash: hashes[1], offset: 4096, length: 100 },
                ],
            },
            FileManifestParent {
                chunk_count: 1,
                chunk_metadata: vec![
                    SSAChunkMeta { hash: hashes[2], offset: 0, length: 8192 },
                ],
            },
        ]
    }

    fn assert_manifests_eq<H: PartialEq + std::fmt::Debug>(
        actual: &[FileManifestParent<H>],
        expected: &[FileManifestParent<H>],
    ) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert_eq!(a.chunk_count, e.chunk_count);
            assert_eq!(a.chunk_metadata.len(), e.chunk_metadata.len());
            for (am, em) in a.chunk_metadata.iter().zip(&e.chunk_metadata) {
                assert_eq!(am.hash, em.hash);
                assert_eq!(am.offset, em.offset);
                assert_eq!(am.length, em.length);
            }
        }
    }

    #[test]
    fn parse_file_header_valid_bytes_round_trips() {
        let header = sample_header();
        let buf = header_bytes(&header);

        let parsed = parse_file_header(&buf.0).unwrap();

        assert_eq!(bytemuck::bytes_of(&parsed), bytemuck::bytes_of(&header));
    }

    #[test]
    fn parse_file_header_wrong_magic_returns_invalid_header() {
        let mut header = sample_header();
        header.magic_num = *b"SSARCHV0";
        let buf = header_bytes(&header);

        let err = parse_file_header(&buf.0).unwrap_err();

        assert!(
            matches!(err, SpriteShrinkError::Parsing(
                ParsingError::InvalidHeader(_)
            )),
            "{err:?}"
        );
    }

    #[test]
    fn parse_file_header_newer_version_returns_invalid_file_version() {
        let mut header = sample_header();
        header.file_version = SUPPORTED_VERSION + 1;
        let buf = header_bytes(&header);

        let err = parse_file_header(&buf.0).unwrap_err();

        assert!(
            matches!(err, SpriteShrinkError::Parsing(
                ParsingError::InvalidFileVersion()
            )),
            "{err:?}"
        );
    }

    #[test]
    fn parse_file_header_current_and_older_versions_are_accepted() {
        for version in [SUPPORTED_VERSION, 0x0001_0000, 0] {
            let mut header = sample_header();
            header.file_version = version;
            let buf = header_bytes(&header);

            let parsed = parse_file_header(&buf.0).unwrap();

            assert_eq!(parsed.file_version, version);
        }
    }

    #[test]
    fn parse_file_header_wrong_length_returns_invalid_header() {
        let mut buf = Aligned([0_u8; 40]);
        buf.0[..36].copy_from_slice(bytemuck::bytes_of(&sample_header()));

        for len in [0, 35, 37] {
            let err = parse_file_header(&buf.0[..len]).unwrap_err();

            assert!(
                matches!(err, SpriteShrinkError::Parsing(
                    ParsingError::InvalidHeader(_)
                )),
                "len {len}: {err:?}"
            );
        }
    }

    // `bytemuck::try_from_bytes` requires the slice to be aligned for
    // `FileHeader`, so valid bytes at an odd offset are rejected. The CLI
    // only avoids this because it reads the header into a fresh Vec, which
    // the allocator happens to align.
    #[test]
    #[ignore = "parse_file_header rejects misaligned input"]
    fn parse_file_header_misaligned_slice_is_accepted() {
        let header = sample_header();
        let mut buf = Aligned([0_u8; 40]);
        buf.0[1..37].copy_from_slice(bytemuck::bytes_of(&header));

        let parsed = parse_file_header(&buf.0[1..37]).unwrap();

        assert_eq!(bytemuck::bytes_of(&parsed), bytemuck::bytes_of(&header));
    }

    #[test]
    fn parse_format_data_wrong_length_returns_invalid_format_data() {
        let buf = Aligned([0_u8; 64]);

        for len in [0, 55, 57] {
            let result = parse_format_data(&buf.0[..len]);

            assert!(
                matches!(
                    result,
                    Err(SpriteShrinkError::Parsing(ParsingError::InvalidFormatData(_)))
                ),
                "len {len}"
            );
        }
    }

    #[test]
    #[ignore = "parse_format_data rejects misaligned input"]
    fn parse_format_data_misaligned_slice_is_accepted() {
        let format_data = SSMCFormatData::build_format_data(76, 100, 2000, 300);
        let mut buf = Aligned([0_u8; 64]);
        buf.0[4..60].copy_from_slice(bytemuck::bytes_of(&format_data));

        let parsed = parse_format_data(&buf.0[4..60]).unwrap();

        assert_eq!(parsed.data_offset, format_data.data_offset);
    }

    #[test]
    fn parse_file_toc_encoded_toc_round_trips() {
        let toc = vec![
            SSMCTocEntry { filename: "a.gba".to_string(), uncompressed_size: 4196 },
            SSMCTocEntry { filename: "b (Rev 1).gba".to_string(), uncompressed_size: 0 },
        ];

        let parsed = parse_file_toc(&encode(&toc)).unwrap();

        assert_eq!(parsed, toc);
    }

    #[test]
    fn parse_file_toc_truncated_input_returns_toc_decode_error() {
        let toc = vec![
            SSMCTocEntry { filename: "a.gba".to_string(), uncompressed_size: 4196 },
        ];
        let bytes = encode(&toc);

        let result = parse_file_toc(&bytes[..bytes.len() - 1]);

        assert!(
            matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::TOCDecodeError(_)))),
            "{result:?}"
        );
    }

    #[test]
    fn parse_file_metadata_u64_hashes_round_trip() {
        let manifests = sample_manifests::<u64>([1, u64::MAX, 7]);

        let parsed = parse_file_metadata::<u64>(&encode(&manifests)).unwrap();

        assert_manifests_eq(&parsed, &manifests);
    }

    #[test]
    fn parse_file_metadata_u128_hashes_round_trip() {
        let manifests = sample_manifests::<u128>([1, u128::MAX, 7]);

        let parsed = parse_file_metadata::<u128>(&encode(&manifests)).unwrap();

        assert_manifests_eq(&parsed, &manifests);
    }

    #[test]
    fn parse_file_metadata_truncated_input_returns_manifest_decode_error() {
        let bytes = encode(&sample_manifests::<u64>([1, 2, 3]));

        let result = parse_file_metadata::<u64>(&bytes[..bytes.len() - 1]);

        assert!(
            matches!(
                result,
                Err(SpriteShrinkError::Parsing(
                    ParsingError::ManifestDecodeError(_)
                ))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn parse_file_chunk_index_u64_and_u128_round_trip() {
        let index_64: Vec<(u64, ChunkLocation)> = vec![
            (10, ChunkLocation { offset: 0, compressed_length: 512 }),
            (20, ChunkLocation { offset: 512, compressed_length: 33 }),
        ];
        let index_128: Vec<(u128, ChunkLocation)> = vec![
            (u128::MAX, ChunkLocation { offset: 0, compressed_length: 1 }),
        ];

        let parsed_64 = parse_file_chunk_index::<u64>(
            &encode(&index_64)
        ).unwrap();
        let parsed_128 = parse_file_chunk_index::<u128>(
            &encode(&index_128)
        ).unwrap();

        assert_eq!(parsed_64.len(), 2);
        assert_eq!(parsed_64[&20].offset, 512);
        assert_eq!(parsed_64[&20].compressed_length, 33);
        assert_eq!(parsed_128[&u128::MAX].compressed_length, 1);
    }

    #[test]
    fn parse_file_chunk_index_truncated_input_returns_index_decode_error() {
        let index: Vec<(u64, ChunkLocation)> =
            vec![(10, ChunkLocation { offset: 0, compressed_length: 512 })];
        let bytes = encode(&index);

        let result = parse_file_chunk_index::<u64>(&bytes[..bytes.len() - 1]);

        assert!(
            matches!(result, Err(SpriteShrinkError::Parsing(
                ParsingError::IndexDecodeError(_)
            ))),
            "{result:?}"
        );
    }
}
