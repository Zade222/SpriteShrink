//! Parses and extracts data from sprite-shrink archive files.
//!
//! This module provides functions to read and interpret the binary
//! structure of an archive. It is responsible for reading the file
//! header and the metadata before the compressed data, which are the
//! core components needed to locate and extract the contained files.

use std::{
    fs::metadata,
    hash::Hash,
    path::Path,
};

use bitcode::Decode;
use serde::Deserialize;

use sprite_shrink::{
    FileHeader, ParsedSSMC, SSMCFormatData,
    parse_file_header, parse_format_data, parse_ssmc_prefix,
};
use sprite_shrink_cd::{
    ParsedSSMD, SSMDFormatData,
    decode_ssmd_format_data, parse_ssmd_prefix,
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

/// Reads and parses every part of an SSMC archive's metadata.
///
/// Reads the format data to find where the metadata ends, then reads the
/// archive up to there and parses it with `parse_ssmc_prefix`. The chunk
/// index it returns holds absolute file offsets.
///
/// # Errors
///
/// Returns `CliError::DataIntegrity` if the format data says the metadata
/// ends past the end of the file, or an error from reading the file or
/// parsing the metadata.
pub fn get_ssmc_prefix<H>(
    file_path: &Path,
    header: &FileHeader,
) -> Result<ParsedSSMC<H>, CliError>
where
    for<'de> H: Eq + Hash + Deserialize<'de> + Decode<'de>,
{
    let bin_format_data = read_file_data(
        file_path,
        header.format_data_offset.into(),
        size_of::<SSMCFormatData>()
    )?;
    let format_data = parse_format_data(&bin_format_data)?;

    let prefix = read_archive_prefix(
        file_path,
        format_data.metadata_prefix_end()
    )?;

    Ok(parse_ssmc_prefix(&prefix)?)
}

/// Reads and parses every part of an SSMD archive's metadata.
///
/// Reads the format data to find where the metadata ends, then reads the
/// archive up to there and parses it with `parse_ssmd_prefix`. The chunk
/// and audio block indexes it returns hold absolute file offsets.
///
/// # Errors
///
/// Returns `CliError::DataIntegrity` if the format data says the metadata
/// ends past the end of the file, or an error from reading the file or
/// parsing the metadata.
pub fn get_ssmd_prefix<H>(
    file_path: &Path,
    header: &FileHeader,
) -> Result<ParsedSSMD<H>, CliError>
where
    for<'de> H: Eq + Hash + Decode<'de>,
{
    let bin_format_data = read_file_data(
        file_path,
        header.format_data_offset.into(),
        size_of::<SSMDFormatData>()
    )?;
    let format_data = decode_ssmd_format_data(&bin_format_data)?;

    let prefix = read_archive_prefix(
        file_path,
        format_data.metadata_prefix_end()
    )?;

    Ok(parse_ssmd_prefix(&prefix)?)
}

/// Reads the first `prefix_end` bytes of an archive. The length comes from
/// the archive itself, so it's checked against the file size before a
/// buffer that large is allocated.
fn read_archive_prefix(
    file_path: &Path,
    prefix_end: u64,
) -> Result<Vec<u8>, CliError> {
    let file_len = metadata(file_path)?.len();

    if prefix_end > file_len {
        return Err(CliError::DataIntegrity(format!(
            "the metadata ends at byte {prefix_end}, past the end of the \
            {file_len}-byte file"
        )));
    }
    let Ok(prefix_len) = usize::try_from(prefix_end) else {
        return Err(CliError::InternalError(format!(
            "The archive's {prefix_end}-byte metadata is too large to read \
            on this platform."
        )));
    };

    read_file_data(file_path, 0, prefix_len)
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
