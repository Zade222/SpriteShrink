//! Implements the extraction mode for the command-line application.
//!
//! This module contains the `run_extraction` function, which is the main
//! entry point for the file extraction process. It handles reading an
//! archive's metadata, including the header, manifest, and chunk index,
//! then proceeds to decompress and write the files requested by the user
//! to the designated output directory.

use std::{
    fmt::Display, 
    fs::{File, create_dir_all}, 
    hash::Hash, 
    io::{BufWriter, Write}, 
    path::Path,
};
use serde::{Deserialize, Serialize};
use tracing::{
    debug
};

use sprite_shrink::{Hashable, decompress_chunk};

use crate::{
    archive_parser::{
    get_chunk_index, get_file_header, get_file_manifest, 
    }, arg_handling::Args, error_handling::CliError, storage_io::read_file_data
};

/// Executes the file extraction process from an archive.
///
/// This is the main entry point for the extraction operation. It reads an
/// archive's metadata, including the header, manifest, and chunk index.
/// It then extracts the specific files requested by the user, identified
/// by their ROM indices. The extraction process is then performed sequentially
/// to avoid IO contention with multiple threads reading and writing to disk at
/// once.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the source archive file.
/// * `out_dir`: A `Path` for the directory where extracted files will
///   be written.
/// * `rom_indices`: A vector of `u8` indices specifying which files to
///   extract from the archive.
/// * `args`: A reference to the main `Args` struct, used here to configure
///   the number of worker threads for parallel processing.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if all requested files are extracted successfully.
/// - `Err(CliError)` if any part of the process fails.
///
/// # Errors
///
/// This function can return an error in several cases, including:
/// - `CliError` variants if reading the archive's header, manifest, or
///   chunk data fails.
/// - `CliError::InternalError` if a worker thread pool cannot be
///   created.
/// - `CliError::InvalidRomIndex` if a requested ROM index does not exist
///   in the archive.
/// - Any error propagated from the decompression or file writing stages.
pub fn run_extraction(
    file_path: &Path, 
    out_dir: &Path, 
    rom_indices: &Vec<u8>,
    args: &Args,
) -> Result<(), CliError> {
    /*Get and store the parsed header from the target archive file.*/
    let header = get_file_header(file_path)?;

    /*Get the identifier for the hash type stored in the archive.*/
    let hash_bit_length = header.hash_type;

    match hash_bit_length {
        1 => extract_data::<u64>(
            file_path, 
            out_dir, 
            rom_indices,  
            &header,
            args
        ),
        2 => extract_data::<u128>(
            file_path, 
            out_dir, 
            rom_indices, 
            &header,
            args
        ),
        _ => //Handle other cases or return an error for unsupported hash types
            Err(CliError::InternalError(
                "Unsupported hash type in archive header.".to_string()
            )),
           
    }
}

/// Generic implementation of the file extraction process.
///
/// This function is parameterized over the hash type `H` and contains the
/// core logic for reading the manifest and chunk index, decompressing chunks,
/// and writing the final files to disk.
fn extract_data<H>(
    file_path: &Path,
    out_dir: &Path,
    rom_indices: &Vec<u8>,
    header: &sprite_shrink::FileHeader,
    args: &Args,
) -> Result<(), CliError>
where
    H: Hashable + 
        Ord + 
        Display + 
        Serialize + 
        for<'de> Deserialize<'de> + 
        Eq + 
        Hash,
{
    /*Stores the length of the file_manifest from the header.*/
    let man_length = header.man_length as usize;

    /*Stores the length of the chunk index from the header.*/
    let chunk_length = header.chunk_index_length as usize;

    /*Stores the length of the dictionary from the header.*/
    let dictionary_length = header.dict_length as usize;

    /*Read and store the file manifest from the target file in memory in 
    the file_manifest variable*/
    let file_manifest = get_file_manifest::<H>(
        file_path, 
        &header.man_offset, 
        &man_length
    )?;

    /*Read and store the zstd compression dictionary from the target file in 
    memory in the dictionary variable.*/
    let dictionary = read_file_data(
        file_path, 
        &header.dict_offset, 
        &dictionary_length
    )?;

    /*Read and store the file zstd compression dictionary from the target 
    file in memory in the dictionary variable.*/
    let chunk_index = get_chunk_index::<H>(
        file_path, 
        &header.chunk_index_offset, 
        &chunk_length
    )?;

    if !out_dir.exists() && !args.force{
        return Err(CliError::InvalidOutputPath())
    } else {
        create_dir_all(out_dir)?;
    }

    for rom in rom_indices {
        let fmp = file_manifest
        .get((*rom - 1)  as usize)
        .ok_or_else(|| CliError::InvalidRomIndex(
            format!("ROM index {rom} is out of bounds.")
        ))?;

        let final_output_path = out_dir.join(
            &fmp.filename);

        let file = File::create(&final_output_path)?;

        let mut writer = BufWriter::new(file);

        fmp.chunk_metadata
            .iter()
            .try_for_each(|scm| -> Result<(), CliError> {
            /*Get the chunk location from the chunk index.*/
            let chunk_location = chunk_index.get(&scm.hash)
                .ok_or_else(|| {
                CliError::InternalError(format!(
                    "Decompression failed: Missing chunk with hash\
                    {}",
                    scm.hash
                ))
            })?;

            /*Store the chunk data offset within the compressed
            store, from the ChunkLocation struct.*/
            let absolute_offset = 
                chunk_location.offset + 
                header.data_offset;

            let comp_chunk_data = read_file_data(
                file_path, 
                &absolute_offset, 
                &(chunk_location.length as usize)
            )?;

            let decomp_chunk_data = decompress_chunk(
                    &comp_chunk_data, 
                    &dictionary
            )?;

            let decomp_chunk_hash = H::from_bytes_with_seed(
                &decomp_chunk_data
            );

            if scm.hash != decomp_chunk_hash {
                return Err(CliError::DataIntegrity(format!(
                    "chunk hash mismatch (expected: {}, calculated: {})",
                    scm.hash, decomp_chunk_hash
                )));
            };

            writer.write_all(&decomp_chunk_data)?;

            Ok(())
        })?;

        debug!("{} extracted successfully", &fmp.filename);
    }

    Ok(())
}