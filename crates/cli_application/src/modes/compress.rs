//! Implements the compression mode for the command-line application.
//!
//! This module contains the primary logic for the file compression and
//! archiving process. It defines the `run_compression` function, which
//! orchestrates a multi-stage pipeline to read input files, process
//! them in parallel to find duplicate data chunks, and then serialize,
//! verify, and compress the unique data into a final archive file.

use std::collections::HashMap;
use std::path::PathBuf;

use dashmap::DashMap;
use lib_sprite_shrink::{
    FileManifestParent, ProcessedFileData, 
    create_file_manifest_and_chunks, finalize_archive, process_file_in_memory, 
    rebuild_and_verify_single_file, serialize_uncompressed_data
};
use rayon::prelude::*;

use crate::arg_handling::Args;
use crate::error_handling::CliError;
use crate::storage_io::{
    load_file, write_final_archive
};

/// Executes the file compression and archiving process.
///
/// This is the main entry point for the compression operation. It manages a
/// multi-stage pipeline that reads input files, processes them in parallel
/// to identify duplicate data chunks, and then serializes, verifies, and
/// compresses the unique data into a final archive. The level of
/// parallelism is configured based on the user's arguments.
///
/// # Arguments
///
/// * `file_paths`: A vector of `PathBuf`s for all files to be included
///   in the archive.
/// * `args`: A reference to the `Args` struct, containing all user-provided
///   settings for compression, such as level, thread count, and memory use.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the entire compression and writing process completes
///   successfully.
/// - `Err(CliError)` if any part of the process fails, from reading
///   input files to writing the final archive.
///
/// # Errors
///
/// This function can return an error in several cases, including:
/// - `CliError::NoFilesError` if the `file_paths` vector is empty.
/// - `CliError::InternalError` if a thread pool cannot be created.
/// - Any error propagated from file I/O, data processing, verification,
///   or final archive writing stages.
pub fn run_compression(
    file_paths: Vec<PathBuf>,
    args: &Args,
) -> Result<(), CliError> {
    
    /*Verify if the list of files paths is empty, throw error if true. */
    if file_paths.is_empty() {
        return Err(CliError::NoFilesError());
    }

    /*Stores the chunk metadata for each file. 
    The key is the file name and the value is a FileManifestParent struct.*/
    let file_manifest: DashMap<String, FileManifestParent> = DashMap::new();
    
    /*Stores each chunk and it's hash. 
    The key is the hash and the value is the byte data of the chunk.*/
    let mut data_store: HashMap<u64, Vec<u8>> = HashMap::new();

    /*Stores the SHA-512 hash for each file. 
    The String is the file name and the array is the 512 bit hash as a 64 byte 
    array.*/
    let veri_hashes: DashMap<String, [u8; 64]> = DashMap::new();

    /*Numerical compression level pulled from the args.*/
    let level: i32 = args.compression_level as i32;

    /*Stores the size of threads used by the parallel task of running 
    process_file_in_memory function.*/
    let mut _read_threads: usize = 0;

    /*If low memory is set limit reads to one worker else set to the user 
    specified argument or let Rayon decide (which is the amount of threads the
    host system supports) */
    if args.low_memory {
        _read_threads = 1 as usize;
        println!("Low memory mode engaged.")
    } else {
        /*0 lets Rayon decide the optimal number when thread parameter isn't 
        used, otherwise set to thread parameter.*/
        _read_threads = args.threads.unwrap_or(0) as usize;
    };

    /*Create read_pool to specify the amount of threads to be used by the 
    parallel process that follows it.*/
    let read_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(_read_threads)
        .build()
        .map_err(|e| CliError::InternalError(format!
            ("Failed to create thread pool: {}", e)))?;

    let window_size = args.window.as_u64();

    /*In parallel read and process each file. The resulting data is then stored
    in a ProcessedFileData struct and added to the processed_files vector.*/
    let processed_files: Vec<ProcessedFileData> = read_pool.install(|| {
        file_paths
            .par_iter()
            .map(|path| {
                //The application is responsible for I/O
                let file_data = load_file(path)?;
                //The library processes the in-memory data
                process_file_in_memory(
                    file_data, 
                    window_size).map_err(CliError::from)
            })
            .filter_map(|result| result.transpose())
            .collect::<Result<Vec<_>, _>>()
    })?;

    /*Sequentially process each ProcessedFileData struct and create a tuple 
    contain a FileManifestParent struct (fmp) and a vector containing 
    ProcessedFileData (chunk_data_list).
    The loop then does the following for each fmp and chunk_data list:
    - Adds the generated 512bit hash to the veri_hashes dash_map.
    - Adds the fmp to the file_manifest.
    - Adds the byte data as a vector to the data_store with the hash as the 
    key.*/
    for processed in processed_files {
        let (fmp, chunk_data_list) = create_file_manifest_and_chunks(
            &processed.file_name,
            &processed.file_data,
            &processed.chunks,
        );

        veri_hashes.insert(processed.file_name.clone(), processed.veri_hash);
        file_manifest.insert(processed.file_name, fmp);
        for (hash, data) in chunk_data_list {
            data_store.entry(hash).or_insert(data);
        }
    }

    println!("{} chunks in data store.", data_store.len());

    println!("All files processed. Verifying data...");

    /*If low memory is set limit reads to four workers else set to the user 
    specified argument or let Rayon decide (which is the amount of threads the
    host system supports) */
    let mut _task_threads: usize = 0;
    if args.low_memory {
        _task_threads = 4;
        println!("Low memory mode engaged.")
    } else {
        /*0 lets Rayon decide the optimal number when thread parameter isn't 
        used, otherwise set to thread parameter.*/
        _task_threads = args.threads.unwrap_or(0) as usize;
    };

    /*Create task_pool to specify the amount of threads to be used by the 
    rebuild_and_verify_single_file parallel process. */
    let task_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(_task_threads)
        .build()
        .map_err(|e| CliError::InternalError(format!("Failed to create thread pool: {}", e)))?;

    /*Serialize and organize data. */

    /*ser_file_manifest: Serialized file manifest. 
    Sorted vector that contains:
        The file name as a string.
        How many chunks the file requires.
        A sorted vector contain chunk location data. Sorted offset*/

    /*ser_data_store: Serialized data store is a sorted data blob of bytes.*/
    
    /*chunk_index: Stores the index of the chunk data associated with a hash.
    Key is the hash, value is the index location.*/

    /*sorted_hashes: Sorted vector that stores all hashes.*/
    
    /*Prepares and serializes all data for the final archive. See above for 
    each variable in the output tuple.*/
    let (ser_file_manifest, ser_data_store, chunk_index, sorted_hashes) = 
        serialize_uncompressed_data(&file_manifest, &data_store);
    
    /*Rebuilds each file and checks the SHA-512 hash for each.*/
    task_pool.install(||{
        ser_file_manifest
            .par_iter()
            .try_for_each(|fmp| {
                rebuild_and_verify_single_file(
                    &fmp, &ser_data_store, &chunk_index, &veri_hashes)
            })
    })?;
    println!("File verification passed! Compressing data and finalizing archive...");

    /*chunk index was storing non compressed data_store locations and is no 
    longer needed.*/
    drop(chunk_index);

    let dictionary_size = args.dictionary.as_u64();
    
    /*Assembles the final archive from its constituent parts, structures it 
    according to the ssmc spec and returns the byte data ready to be written.*/
    let ssmc_data = finalize_archive(
        &ser_file_manifest, 
        &data_store, 
        &sorted_hashes, 
        file_paths.len() as u32, 
        level, 
        dictionary_size,
        _task_threads,
        args.optimize_dictionary)?;

    println!("Total file size will be: {} bytes.", ssmc_data.len());

    /*Takes the specified output file destination and adds the ssmc file 
    extension if it's not present or replaces the specified extension with
    ssmc.*/
    let final_output_path = args.output.as_ref().unwrap().with_extension("ssmc");

    //Write ssmc archive to disk.
    write_final_archive(&final_output_path, &ssmc_data)?;

    Ok(())
}