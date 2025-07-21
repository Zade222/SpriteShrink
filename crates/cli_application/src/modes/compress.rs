//! Implements the compression mode for the command-line application.
//!
//! This module contains the primary logic for the file compression and
//! archiving process. It defines the `run_compression` function, which
//! orchestrates a multi-stage pipeline to read input files, process
//! them in parallel to find duplicate data chunks, and then serialize,
//! verify, and compress the unique data into a final archive file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use lib_sprite_shrink::{
    FileManifestParent, 
    create_file_manifest_and_chunks, finalize_archive, process_file_in_memory, 
    rebuild_and_verify_single_file, serialize_uncompressed_data, test_compression
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
/// multi-stage pipeline that reads input files, processes them in parallel,
/// and compresses them into a final archive file.
///
/// The function can automatically tune chunking and compression parameters
/// for optimal size if `--auto-tune` is enabled. It also uses distinct
/// thread configurations for I/O-bound and CPU-bound stages to enhance
/// performance.
///
/// # Arguments
///
/// * `file_paths`: A vector of `PathBuf`s for all files to be included
///   in the archive.
/// * `args`: A reference to the `Args` struct, containing all user-provided
///   settings. The `--auto-tune` flag enables parameter optimization. This
///   can be partially overridden by providing an explicit `--window` or
///   `--dictionary` value, which will skip the tuning for that specific
///   parameter.
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
    let mut _file_manifest: DashMap<String, FileManifestParent> = DashMap::new();
    
    /*Stores each chunk and it's hash. 
    The key is the hash and the value is the byte data of the chunk.*/
    let mut _data_store: HashMap<u64, Vec<u8>> = HashMap::new();

    /*Stores the SHA-512 hash for each file. 
    The String is the file name and the array is the 512 bit hash as a 64 byte 
    array.*/
    let mut _veri_hashes: DashMap<String, [u8; 64]> = DashMap::new();

    /*Numerical compression level pulled from the args.*/
    let level: i32 = args.compression_level as i32;

    /*Stores the size of threads used by the parallel task of running 
    process_file_in_memory function.*/
    let mut _process_threads: usize = 0;

    /*If low memory is set limit reads to one worker else set to the user 
    specified argument or let Rayon decide (which is the amount of threads the
    host system supports) */
    if args.low_memory {
        _process_threads = 1 as usize;
        println!("Low memory mode engaged.")
    } else {
        /*0 lets Rayon decide the optimal number when thread parameter isn't 
        used, otherwise set to thread parameter.*/
        _process_threads = args.threads.unwrap_or(0) as usize;
    };

    /*Create read_pool to specify the amount of threads to be used by the 
    parallel process that follows it.*/
    let _process_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(_process_threads)
        .build()
        .map_err(|e| CliError::InternalError(format!
            ("Failed to create thread pool: {}", e)))?;

    let mut best_window_size = args.window.map_or(
        2 * 1024, |b| b.as_u64());

    let mut best_dictionary_size = args.dictionary.map_or(
        16 * 1024, |b| b.as_u64());

    if args.auto_tune {
        let timeout_dur = args.autotune_timeout.
            map(Duration::from_secs);
        //Starting window size
        let mut current_window_size = 512;

        let mut last_compressed_size = usize::MAX;
        
        if args.window.is_none() {

            loop {
                println!("Testing window size: {}", current_window_size);

                /*Set starting time for determining the compressed size 
                using the current window size*/
                let start_time = Instant::now();

                //Process files with the current window size
                let (_fm, 
                    temp_data_store, 
                    _vh) =
                        process_files_with_window_size(
                            &file_paths, 
                            current_window_size, 
                            &_process_pool)?;

                //Serialize temporary data
                let (_ser_file_manifest,
                _ser_data_store, 
                _chunk_index, 
                sorted_hashes) = 
                serialize_uncompressed_data(&_fm, &temp_data_store);

                //Compress the data and measure the size
                let compressed_size = test_compression(
                    &temp_data_store, 
                    &sorted_hashes, 
                    _process_threads,
                8192)?;
                
                /*Measure the time taken for determining the compressed size 
                for the current window size*/
                let elapsed = start_time.elapsed();

                if compressed_size > last_compressed_size {
                    //Process passed the optimal point, stop.
                    println!("Optimal window size found to be {} bytes.", 
                        best_window_size);
                    break;
                }

                if let Some(timeout) = timeout_dur {
                    if elapsed > timeout {
                        println!(
                            "Autotune for window size {} took too long (>{:?}). \
                            Using best result so far: {}.",
                            current_window_size, timeout, best_window_size
                        );
                        break; // Exit the loop
                    }
                }
                
                //This iteration was successful and an improvement
                last_compressed_size = compressed_size;
                best_window_size = current_window_size;

                //Double value for next loop
                current_window_size *= 2;
            }
        }

        (_file_manifest, _data_store, _veri_hashes) =
            process_files_with_window_size(&file_paths, 
                best_window_size, 
                &_process_pool)?;
        
        let (_ser_fm, 
            _sds, 
            _ci, 
            sorted_hashes) =
            serialize_uncompressed_data(&_file_manifest, 
                &_data_store);


        last_compressed_size = usize::MAX;

        //Starting dictionary size, 8kb
        let mut current_dict_size: usize = 8192; 

        loop {
            if args.dictionary.is_none() {
                println!("Testing dictionary size: {}", current_dict_size);
                
                let compressed_size = test_compression(
                    &_data_store, 
                    &sorted_hashes,
                    _process_threads,
                    current_dict_size)?;
                
                if compressed_size > last_compressed_size {
                    //Process passed the optimal point, stop.
                    println!("Optimal dictionary size found to be {} bytes.", 
                        best_dictionary_size);
                    break;
                }
                //This iteration was successful and an improvement
                last_compressed_size = compressed_size;
                best_dictionary_size = current_dict_size as u64;

                //Double value for next loop
                current_dict_size *= 2;

                //Accept reasonable upper limit for dictionary size.
                //This will stop it at an accepted value of 1024 * 512
                if current_dict_size > 1024 * 1024 { 
                    break;
                }
            }
        } 
    } else {

        (_file_manifest, _data_store, _veri_hashes) =
            process_files_with_window_size(&file_paths, 
                best_window_size, 
                &_process_pool)?;
    }

    println!("{} unique chunks in data store.", _data_store.len());

    println!("All files processed. Verifying data...");

    /*If low memory is set limit reads to four workers else set to the user 
    specified argument or let Rayon decide (which is the amount of threads the
    host system supports) */
    /*if args.low_memory {
        compute_threads = 4;
    } else {
        /*0 lets Rayon decide the optimal number when thread parameter isn't 
        used, otherwise set to thread parameter.*/
        compute_threads = args.threads.unwrap_or(0) as usize;
    };*/

    let _compute_threads = 
        if args.low_memory { 4 } 
        else { args.threads.unwrap_or(0) };

    /*Create task_pool to specify the amount of threads to be used by the 
    rebuild_and_verify_single_file parallel process. */
    let _compute_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(_compute_threads)
        .build()
        .map_err(|e| CliError::InternalError(
            format!("Failed to create thread pool: {}", e)))?;

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
    let (ser_file_manifest, 
        ser_data_store, 
        chunk_index, 
        sorted_hashes) = 
        serialize_uncompressed_data(&_file_manifest, &_data_store);
    
    /*Rebuilds each file and checks the SHA-512 hash for each.*/
    _compute_pool.install(||{
        ser_file_manifest
            .par_iter()
            .try_for_each(|fmp| {
                rebuild_and_verify_single_file(
                    &fmp, &ser_data_store, &chunk_index, &_veri_hashes)
            })
    })?;
    println!("File verification passed! Compressing data and \
        finalizing archive...");

    /*chunk index was storing non compressed data_store locations and is no 
    longer needed.*/
    drop(chunk_index);

    //let dictionary_size = args.dictionary.as_u64();
    
    /*Assembles the final archive from its constituent parts, structures it 
    according to the ssmc spec and returns the byte data ready to be written.*/
    let ssmc_data = finalize_archive(
        &ser_file_manifest, 
        &_data_store, 
        &sorted_hashes, 
        file_paths.len() as u32, 
        level, 
        best_dictionary_size,
        _compute_threads,
        args.optimize_dictionary)?;

    println!("Total file size will be: {} bytes.", ssmc_data.len());

    /*Takes the specified output file destination and adds the ssmc file 
    extension if it's not present or replaces the specified extension with
    ssmc.*/
    let final_output_path = args.output
        .as_ref()
        .unwrap()
        .with_extension("ssmc");

    //Write ssmc archive to disk.
    write_final_archive(&final_output_path, &ssmc_data)?;

    Ok(())
}

/// Processes a collection of files in parallel to produce core data structures.
///
/// This function reads multiple files from disk, chunks them using a
/// specified window size, and generates the necessary metadata for archiving.
/// It leverages a provided thread pool to perform I/O and processing
/// concurrently, which is efficient for handling many files.
///
/// The results are organized into three thread-safe collections: a manifest
/// of all files, a deduplicated data store of unique chunks, and a map of
/// verification hashes for each file.
///
/// # Arguments
///
/// * `file_paths`: A slice of `PathBuf`s pointing to the input files.
/// * `window_size`: A `u64` that defines the window size for the
///   content-defined chunking algorithm.
/// * `pool`: A reference to a configured `rayon::ThreadPool` that will be
///   used to execute the file processing in parallel.
///
/// # Returns
///
/// A `Result` containing a tuple of three data structures:
/// - A `DashMap` for the file manifest, mapping filenames to their metadata.
/// - A `HashMap` for the data store, mapping chunk hashes to raw byte data.
/// - A `DashMap` for verification hashes, mapping filenames to SHA-512 hashes.
///
/// On failure, it returns a `CliError`.
fn process_files_with_window_size(
    file_paths: &[PathBuf],
    window_size: u64,
    pool: &rayon::ThreadPool,
) -> Result<(DashMap<String, FileManifestParent>, //file_manifest
    HashMap<u64, Vec<u8>>, //data_store
    DashMap<String, [u8; 64]>), //veri_hashes
    CliError>
{
    //Read and process each file. Low memory mode limits this to 1 worker.
    let processed_files = pool.install(|| {
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

    let file_manifest: DashMap<String, FileManifestParent> = DashMap::new();
    let mut data_store: HashMap<u64, Vec<u8>> = HashMap::new();
    let veri_hashes: DashMap<String, [u8; 64]> = DashMap::new();

    //Data must be ordered sequentially.
    for processed in processed_files {
        let (fmp, 
            chunk_data_list) = 
            create_file_manifest_and_chunks(
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

    Ok((file_manifest, data_store, veri_hashes))
}