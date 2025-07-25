//! Provides functions for parsing and validating sprite-shrink archives.
//!
//! This module contains the logic required to read the binary data from
//! an archive file and interpret its structure. It includes functions
//! for parsing the main file header, the file manifest, and the chunk
//! index. It also defines critical constants, such as the magic number
//! and supported version, to ensure file integrity and compatibility.

use std::collections::HashMap;

use crate::lib_error_handling::{LibError};
use crate::lib_structs::{FileHeader, FileManifestParent, ChunkLocation};

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
pub const SUPPORTED_VERSION: u32 = 0x00010000;

/// The seed value used for deterministic chunking and hashing.
///
/// This constant is passed to the FastCDC chunking algorithm and the
/// xxHash hashing function. Using a fixed seed ensures that the same
/// file will always produce the same set of chunks and hashes, which is
/// critical for reliable deduplication.
pub const SS_SEED: u64 = 0x4202803010192019;


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
/// - `Err(LibError)` if the data is malformed, the magic number is
///   incorrect, or the file version is unsupported.
pub fn parse_file_header(header_data: &[u8]) -> Result<FileHeader, LibError>{
    //Attempt to cast the byte slice to a FileHeader.
    let file_header = 
        bytemuck::try_from_bytes::<FileHeader>(&header_data)
        .map_err(|e| LibError::InvalidHeaderError(e.to_string()))?;

    //Verify the magic number to ensure it's the correct file type.
    if file_header.magic_num != MAGIC_NUMBER {
        return Err(LibError::InvalidHeaderError(
            "File Magic Number is invalid.".to_string(),
        ));
    }

    //Check if the file version is supported by this library version.
    if file_header.file_version > SUPPORTED_VERSION {
        return Err(LibError::FileVersionError());
    };

    Ok(*file_header)
}
    
/// Deserializes the file manifest from a raw byte slice.
///
/// This function is responsible for parsing the binary representation
/// of the file manifest, which contains metadata for every file in the
/// archive. It uses `bincode` to decode the byte slice into a
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
/// - `Err(LibError)` if the byte slice cannot be decoded due to data
///   corruption or a format mismatch.
pub fn parse_file_metadata(
    manifest_data: &[u8]
) -> Result<Vec<FileManifestParent>, LibError> {
    let config = bincode::config::standard();

    let (file_manifest, _len) =
        bincode::serde::decode_from_slice(&manifest_data, config)
            .map_err(|e| {LibError::ManifestDecodeError(e.to_string())})?;

    Ok(file_manifest)
}

/// Deserializes the chunk index from a raw byte slice.
///
/// This function parses the binary data representing the chunk index,
/// which maps each unique chunk hash to its `ChunkLocation`. It uses
/// `bincode` to decode the data into a `HashMap` for efficient lookups
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
/// - `Err(LibError)` if the byte slice cannot be decoded.
pub fn parse_file_chunk_index(chunk_index_data: &[u8]) 
    -> Result<HashMap<u64, ChunkLocation>, LibError>
{
    let config = bincode::config::standard();

    let (chunk_index, _len) =
        bincode::serde::decode_from_slice(&chunk_index_data, config)
            .map_err(|e| {LibError::ManifestDecodeError(e.to_string())})?;

    Ok(chunk_index)
}