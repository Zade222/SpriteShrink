//! Implements the extraction mode for the command-line application.
//!
//! This module contains the `run_extraction` function, which is the main
//! entry point for the file extraction process. It handles reading an
//! archive's metadata, including the header, manifest, and chunk index,
//! then proceeds to decompress and write the files requested by the user
//! to the designated output directory.

use std::path::PathBuf;
use rayon::prelude::*;

use sprite_shrink::{decompress_chunk};

use crate::archive_parser::{
    get_chunk_index, get_file_header, get_file_manifest, 
};
use crate::arg_handling::Args;
use crate::decompression::{get_decomp_chunk};
use crate::error_handling::CliError;
use crate::storage_io::{
    read_file_data, write_file
};

/// Executes the file extraction process from an archive.
///
/// This is the main entry point for the extraction operation. It reads an
/// archive's metadata, including the header, manifest, and chunk index.
/// It then extracts the specific files requested by the user, identified
/// by their ROM indices. The process is parallelized using a thread pool
/// to decompress and write multiple files concurrently.
///
/// # Arguments
///
/// * `file_path`: A `PathBuf` pointing to the source archive file.
/// * `out_dir`: A `PathBuf` for the directory where extracted files will
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
pub fn run_extraction(file_path: &PathBuf, 
    out_dir: &PathBuf, 
    rom_indices: &Vec<u8>,
    args: &Args,
) -> Result<(), CliError> {
    /*Get and store the parsed header from the target archive file.*/
    let header = get_file_header(file_path)?;

    /*Stores the length of the file_manifest from the header.*/
    let man_length = header.man_length as usize;

    /*Stores the length of the chunk index from the header.*/
    let chunk_length = header.chunk_index_length as usize;

    /*Stores the length of the dictionary from the header.*/
    let dictionary_length = header.dict_length as usize;

    /*Read and store the file manifest from the target file in memory in 
    the file_manifest variable*/
    let file_manifest = get_file_manifest(
        file_path, 
        &header.man_offset, 
        &man_length)?;

    /*Read and store the zstd compression dictionary from the target file in 
    memory in the dictionary variable.*/
    let dictionary = read_file_data(
        file_path, 
        &header.dict_offset, 
        &dictionary_length)?;

    /*Read and store the file zstd compression dictionary from the target 
    file in memory in the dictionary variable.*/
    let chunk_index = get_chunk_index(
        file_path, 
        &header.chunk_index_offset, 
        &chunk_length)?;

    /*If low memory is set limit reads to four workers else set to the user 
    specified argument or let Rayon decide (which is the amount of threads the
    host system supports) */
    let num_threads = if args.low_memory {
        println!("Low memory mode engaged.");
        4
    } else {
        args.threads.unwrap_or(0)
    };

    /*Create worker_pool to specify the amount of threads to be used by the 
    parallel process that follows it.*/
    let worker_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .map_err(|e| CliError::InternalError(format!("Failed to create thread pool: {}", e)))?;

    /*In parallel, for each ROM file index provided, the below code
    accomplishes the following:
    - Get the FileManifestParent and store it in fmp.
    - For each SSAChunkMeta in the FileManifestParent, read and decompress
        each chunk from the compressed data store and assemble the file data 
        in order.
    - Prepare the output path by appending the file name to the provided 
        output directory.
    - Write the file to the destination.*/
    worker_pool.install(|| {
        rom_indices.par_iter().try_for_each(|rom| -> Result<(), CliError> {
            let fmp = file_manifest
                .get((*rom - 1)  as usize)
                .ok_or_else(|| CliError::InvalidRomIndex(
                    format!("ROM index {} is out of bounds.", rom)
                ))?;

            let file_data: Vec<Vec<u8>> = fmp
                .chunk_metadata
                .par_iter()
                .map(|scm| -> Result<Vec<u8>, CliError> {
                    /*Get the chunk location from the chunk index.*/
                        let chunk_location = chunk_index.get(&scm.hash).ok_or_else(|| {
                            CliError::InternalError(format!(
                                "Decompression failed: Missing chunk with hash {}",
                                scm.hash
                            ))
                        })?;
                        /*Store the chunk data offset within the compressed 
                        store, from the ChunkLocation struct.*/
                        let absolute_offset = 
                            chunk_location.offset + 
                            header.data_offset;
                        
                        let comp_chunk_data = read_file_data(
                        &file_path, 
                        &absolute_offset, 
                        &chunk_length
                        )?;

                        let decompressed_chunk_data = decompress_chunk(
                            &comp_chunk_data, 
                            &dictionary
                        )?;

                        Ok(decompressed_chunk_data)
                
                }).collect::<Result<_,_ >>()?;

            let final_output_path = out_dir.join(
                &fmp.filename);

            let file_data_ref: Vec<&[u8]> = file_data
                .iter()
                .map(Vec::as_slice)
                .collect();
            
            write_file(&final_output_path, file_data_ref)?;
            
            if args.verbose{
                println!("{} extracted successfully", &fmp.filename);
            }

            Ok(())
        })
    })?;
    Ok(())
}