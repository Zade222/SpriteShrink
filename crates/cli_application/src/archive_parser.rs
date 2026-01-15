//! Parses and extracts data from sprite-shrink archive files.
//!
//! This module provides functions to read and interpret the binary
//! structure of an archive. It is responsible for parsing the file
//! header, manifest, and chunk index, which are the core components
//! needed to locate and extract the contained files.

use std::{
    collections::HashMap,
    fmt::Display,
    path::Path,
};

use bitcode::Decode;
use serde::Serialize;

use sprite_shrink::{
    ChunkLocation, FileHeader, FileManifestParent, Hashable, SSMCTocEntry,
    parse_file_chunk_index, parse_file_header, parse_file_metadata,
    parse_file_toc
};

use crate::error_handling::CliError;
use crate::storage_io::read_file_data;

/// Reads and parses the header from an archive file.
///
/// This function reads a fixed-size block of data from the start of the
/// specified file. It then parses this binary data into a `FileHeader`
/// struct, which contains essential metadata about the archive, such as
/// file counts and the locations of other critical data sections.
///
/// # Arguments
///
/// * `file_path`: A `PathBuf` pointing to the archive file.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(FileHeader)` containing the parsed file header data.
/// - `Err(CliError)` if reading the file fails or if the header
///   data cannot be correctly parsed.
pub fn get_file_header(file_path: &Path) -> Result<FileHeader, CliError> {
    let header_size = FileHeader::HEADER_SIZE as usize;

    let byte_header_data: Vec<u8> = read_file_data(
        file_path,
        0,
        header_size
    )?;

    let header: FileHeader = parse_file_header(&byte_header_data)?;

    Ok(header)
}

/// Reads and parses the file manifest from an archive.
///
/// This function extracts the binary file manifest from a given location
/// within an archive. It reads the raw byte data of the manifest, then
/// deserializes it into a vector of `FileManifestParent` structs, which
/// describe the contents and structure of the files in the archive.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the archive file.
/// * `man_offset`: The starting byte offset of the manifest data.
/// * `man_length`: The total length in bytes of the manifest data.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<FileManifestParent>)` containing the parsed file manifest.
/// - `Err(CliError)` if reading or parsing the manifest fails.
pub fn get_file_manifest<H>(
    file_path: &Path,
    man_offset: u64,
    man_length: usize
) -> Result<Vec<FileManifestParent<H>>, CliError>
where
    H: Hashable
        + Ord
        + Display
        + Serialize
        + for<'de> serde::Deserialize<'de>
        + for<'de> Decode<'de>,
{
    //Read file manifest from file.
    let bin_vec_manifest = read_file_data(
        file_path,
        man_offset,
        man_length
    )?;

    //Parse it into the required Vec<FileManifestParent> via the library.
    parse_file_metadata(&bin_vec_manifest).map_err(CliError::from)
}

/// Retrieves the maximum valid ROM index from an archive.
///
/// This function reads the archive's header to find the total number of
/// files, which determines the highest possible ROM index. It is used to
/// validate user-provided indices before extraction to ensure they are
/// within a valid range. The maximum index is 255.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the archive file.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(u8)` containing the max ROM index, which is the file count.
/// - `Err(CliError)` if the header cannot be read or if the file
///   count exceeds the supported limit of 255.
pub fn get_max_rom_index(file_path: &Path)
    -> Result<u8, CliError>
{
    let header = get_file_header(file_path)?;

    header.file_count.try_into().map_err(|_| {
        CliError::InternalError(
            "The number of files in the archive exceeds the supported limit of\
                255.".to_string(),
        )
    })
}

/// Reads and parses the chunk index from an archive file.
///
/// This function extracts a binary segment containing the chunk index
/// from an archive. It reads the raw data from a specified offset and
/// length, then parses it into a `HashMap`. This map associates each
/// unique chunk hash with its `ChunkLocation`, which provides the offset
/// and length of the chunk's data within the archive.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the archive file.
/// * `chunk_index_offset`: The starting byte offset of the index.
/// * `chunk_index_length`: The total length in bytes of the index.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(HashMap<u64, ChunkLocation>)` containing the parsed index.
/// - `Err(CliError)` if reading the file or parsing the data fails.
pub fn get_chunk_index<H>(
    file_path: &Path,
    chunk_index_offset: u64,
    chunk_index_length: u64
) -> Result<HashMap<H, ChunkLocation>, CliError>
where
    H: Hashable
        + Ord
        + Display
        + Serialize
        + for<'de> serde::Deserialize<'de>
        + for<'de> Decode<'de>,
    {
    //Read the chunk_index from the file.
    let bin_vec_chunk_index = read_file_data(
        file_path,
        chunk_index_offset,
        chunk_index_length as usize
    )?;

    //Parse the binary data into a chunk index HashMap and return the value.
    parse_file_chunk_index(
        &bin_vec_chunk_index
    ).map_err(CliError::from)
}


pub fn get_toc<T> (
    file_path: &Path,
    toc_offset: u32,
    toc_length: usize
) -> Result<Vec<T>, CliError>
where
    T: for<'de> Decode<'de>,
{
    let enc_toc_data = read_file_data(
        file_path,
        toc_offset as u64,
        toc_length
    )?;

    bitcode::decode(&enc_toc_data).map_err(|e| {
        CliError::InternalError(format!("Failed to decode TOC: {}", e))
    })
}
