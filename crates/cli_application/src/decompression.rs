//! Handles decompression of data chunks from a sprite-shrink archive.
//!
//! This module provides functions to read compressed data from an archive,
//! apply the appropriate decompression algorithm with a shared dictionary,
//! and restore the original data chunks. It is a key component of the
//! file extraction process.

use std::collections::HashMap;
use std::io::Read;
use std::path::{PathBuf};

use lib_sprite_shrink::{ChunkLocation, SSAChunkMeta};

use crate::error_handling::CliError;
use crate::storage_io::{read_file_data};

/// Decompresses a single data chunk from an archive.
///
/// This function locates a compressed chunk using its hash, reads it,
/// and then decompresses it using a shared dictionary. It finds the
/// chunk's location via the `chunk_index`, reads the compressed data
/// from the archive, and applies the zstd algorithm with the provided
/// dictionary to restore the original data.
///
/// # Arguments
///
/// * `file_path`: A `PathBuf` for the archive file to read from.
/// * `chunk_index`: A `HashMap` that maps chunk hashes to their
///   locations within the archive.
/// * `dictionary`: A byte slice of the shared compression dictionary.
/// * `scm`: A reference to the `SSAChunkMeta` for the chunk, which
///   contains the hash needed to look up its location.
/// * `data_offset`: The absolute starting offset of the 
///   compressed_data section within the archive file.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the decompressed chunk data.
/// - `Err(CliError)` if the chunk hash is not found in the index,
///   or if reading or decompression fails.
pub fn get_decomp_chunk(file_path: &PathBuf,
    chunk_index: &HashMap<u64, ChunkLocation>, 
    dictionary: &[u8],
    scm: &SSAChunkMeta,
    data_offset: u64) -> Result<Vec<u8>, CliError>
{
    /*Get the chunk location from the chunk index.*/
    let chunk_location = chunk_index.get(&scm.hash).ok_or_else(|| {
        CliError::InternalError(format!(
            "Decompression failed: Missing chunk with hash {}",
            scm.hash
        ))
    })?;

    /*Store the chunk length, from the ChunkLocation struct.*/
    let chunk_length: usize = chunk_location.length as usize;

    /*Store the chunk data offset within the compressed store, 
    from the ChunkLocation struct.*/
    let absolute_offset = chunk_location.offset + data_offset;

    // Read the compressed chunk data from the file.
    let comp_chunk_data = read_file_data(
        &file_path, 
        &absolute_offset, 
        &chunk_length)?;
    
    /*Create a zstd decoder with the prepared dictionary from the file
    archive.*/
    let mut decoder = zstd::stream::Decoder::with_dictionary(
        comp_chunk_data.as_slice(), 
        &dictionary)?;

    // Decompress the data into a new vector.
    let mut decompressed_chunk_data = Vec::new();
    decoder.read_to_end(&mut decompressed_chunk_data)?;

    /*Return decompressed chunk data. */
    Ok(decompressed_chunk_data)
}