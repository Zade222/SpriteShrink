//! Implements the compression mode for the command-line application.
//!
//! This module contains the primary logic for the file compression and
//! archiving process. It defines the `run_compression` function, which
//! orchestrates a multi-stage pipeline to read input files, process
//! them in parallel to find duplicate data chunks, and then serialize,
//! verify, and compress the unique data into a final archive file.

use std::{
    borrow::Borrow, 
    fmt::Display, 
    fs::{create_dir_all, File},
    path::PathBuf, 
    sync::{Arc, Mutex}, 
    thread, 
    time::{Duration, Instant}
};

use dashmap::DashMap;
use directories::ProjectDirs;
use indicatif::{ProgressBar, ProgressStyle};
use fastcdc::v2020::{StreamCDC, Normalization};
use flume::{bounded};
use redb::{backends::InMemoryBackend, Database, TableDefinition, Value};
use sprite_shrink::{
    serialize_uncompressed_data, test_compression, verify_single_file, 
    ArchiveBuilder, FileManifestParent, Hashable, Progress, SSAChunkMeta, 
    SS_SEED
};
use rayon::prelude::*;
use serde::Serialize;
use sha2::{Digest,Sha512};
use sysinfo::System;
use thread_priority::*;
use tracing::{
    debug,
    info,
    warn
};

use crate::{
    arg_handling::Args,
    cli_types::{
        ChunkMessage, DBInfo, FileCompleteData, TempDatabase, APPIDENTIFIER
    },
    db_transactions::{batch_insert, get_chunks, get_keys, get_tot_data_size},
    error_handling::CliError,
    storage_io::{
        append_data_to_file, calc_tot_input_size, write_final_archive
    }
};

/// Executes the file compression and archiving process.
///
/// This is the main entry point for the compression operation. It manages a
/// multi-stage pipeline that reads input files, processes them in parallel to
/// identify duplicate data chunks, and then serializes, verifies, and 
/// compresses the unique data into a final archive file.
///
/// A key feature of this process is its use of a temporary `redb` database to
/// store unique chunks. This approach minimizes memory usage, making it 
/// suitable for processing large files that might not fit into RAM.
///
/// The function can automatically tune chunking and compression parameters for
/// optimal size if `--auto-tune` is enabled. It also uses distinct thread 
/// configurations for I/O-bound and CPU-bound stages to enhance performance.
///
/// # Arguments
///
/// * `file_paths`: A vector of `PathBuf`s for all files to be included in the
///   archive.
/// * `args`: A reference to the `Args` struct, containing all user-provided
///   settings. The `--auto-tune` flag enables parameter optimization.
/// * `hash_type_id`: A `u8` identifier for the hash type being used 
///   (e.g., 1 for 64-bit, 2 for 128-bit).
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   hashing, serialization, and use as a `redb` database key.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the entire compression and writing process completes 
///   successfully.
/// - `Err(CliError)` if any part of the process fails, from reading input
///   files to writing the final archive.
///
/// # Errors
///
/// This function can return an error in several cases, including:
/// - `CliError::NoFilesError` if the `file_paths` vector is empty.
/// - `CliError::InternalError` if a thread pool cannot be created.
/// - Any error propagated from file I/O, database transactions, 
///   data processing, verification, or final archive writing stages.
pub fn run_compression<H>(
    file_paths: Vec<PathBuf>,
    args: &Args,
    hash_type_id: &u8,
) -> Result<(), CliError> 
where
    H: Hashable
        + Ord
        + Display
        + Serialize
        + for<'de> serde::Deserialize<'de>
        + redb::Key
        + for<'a> redb::Value<SelfType<'a> = H>,
{
    /*Verify if the list of files paths is empty, throw error if true. */
    if file_paths.is_empty() {
        return Err(CliError::NoFilesFound());
    }

    let out_dir = args.output
        .as_ref()
        .unwrap()
        .parent()
        .unwrap();

    if !out_dir.exists() && !args.force{
        return Err(CliError::InvalidOutputPath())
    } else {
        create_dir_all(out_dir)?;
    }

    /*Set OS process priority to be undertypical user facing application 
    priority.*/
    #[cfg(target_os = "linux")]
    {
        let priority = ThreadPriority::Crossplatform(
            30.try_into().unwrap()
        );

        set_current_thread_priority(priority)?;
    }

    #[cfg(target_os = "macos")]
    {
        let priority = ThreadPriority::Crossplatform(
            30.try_into().unwrap()
        );

        set_current_thread_priority(priority)?;
    }

    #[cfg(target_os = "windows")]
    {
        let priority = ThreadPriority::Os(
            WinAPIThreadPriority::BelowNormal.into()
        );

        set_current_thread_priority(priority)?;
    }

    let input_data_size = calc_tot_input_size(&file_paths)?;

    let process_in_memory = if args.low_memory {
        false
    } else {
        process_in_memory_check(input_data_size)
    };



    /*The following lines will likely eventually be moved to a function in 
    db_transactions.rs*/
    
    let file_stem = args.output
        .as_ref()
        .unwrap()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let proj_dirs = ProjectDirs::from(
        APPIDENTIFIER.qualifier, 
        APPIDENTIFIER.organization, 
        APPIDENTIFIER.application)
    .unwrap();

    let cache_dir = proj_dirs.cache_dir();
    create_dir_all(cache_dir).unwrap();
    let db_path = cache_dir.join(file_stem.clone()).with_extension("redb");

    //When the guard falls out of scope the scope will be deleted.
    let _temp_db_guard = TempDatabase { path: db_path.clone() };
    
    let db_data_store: TableDefinition<
        'static, 
        H, 
        Vec<u8>
    > = TableDefinition::new("data_store");

    let db = if process_in_memory {
        Database::builder().create_with_backend(InMemoryBackend::new()).unwrap()
    } else {
        Database::create(&db_path).unwrap()
    };

    let db_info = Arc::new(DBInfo {
        db,
        db_def: db_data_store
    });

    //End function

    /*Stores the chunk metadata for each file. 
    The key is the file name and the value is a FileManifestParent struct.*/
    let mut _file_manifest: DashMap<
        String, 
        FileManifestParent<H>
    > = DashMap::new();
    
    let data_store: Arc<DashMap<H, Vec<u8>>> = Arc::new(DashMap::new());

    /*Stores the SHA-512 hash for each file. 
    The String is the file name and the array is the 512 bit hash as a 64 byte 
    array.*/
    let mut _veri_hashes: DashMap<String, [u8; 64]> = DashMap::new();
    
    //Used for storing overall file size.
    let total_size: u64;

    /*Numerical compression level pulled from the args.*/
    let level: i32 = args.compression_level as i32;

    /*Stores the size of threads used by the parallel task of running 
    process_file_in_memory function.*/
    let mut _process_threads: usize = 0;

    //Callback for getting all keys (hashes) in database
    let data_store_for_key_ret = Arc::clone(&data_store);
    let key_ret_cb = 
        || -> Vec<H> {
        if process_in_memory {
            //let data_store = data_store_arc.clone();
            data_store_for_key_ret.iter().map(|entry| *entry.key()).collect()
        } else {
            //Get keys from db
            get_keys(&db_info).unwrap()
        }
    };

    //Callback for getting a list of chunks from the provided list of hashes
    let chunk_ret_cb = Arc::new({
        let db_info_clone = Arc::clone(&db_info);
        let data_store_for_chunk_ret = Arc::clone(&data_store);
        move |hashes: &[H]| -> Vec<Vec<u8>> {
            if process_in_memory {
                hashes.iter().map(|hash| {
                    data_store_for_chunk_ret.get(hash).unwrap().clone()
                }).collect()
            } else {
                //Get chunk data from db
                get_chunks(hashes, &db_info_clone).unwrap()
            }
        }
    });

    let db_info_ins_clone = Arc::clone(&data_store);
    let db_info_ret_clone = Arc::clone(&db_info);
    let insert_batch_cb = move | chunk_batch: &Vec<(H, Vec<u8>)> | {
        if process_in_memory {
            for (hash, data) in chunk_batch{
                db_info_ins_clone.entry(*hash).or_insert(data.to_vec());
            }
        } else {
            batch_insert(&db_info_ret_clone, chunk_batch).unwrap();
        }
    };

    /*Create read_pool to specify the amount of threads to be used by the 
    parallel process that follows it.*/
    let _process_pool = {
        let builder = rayon::ThreadPoolBuilder::new()
            .num_threads(_process_threads);
        
        builder.build()
            .map_err(|e| CliError::InternalError(
                format!("Failed to create thread pool: {e}")))?
    };

    //Sets the window size from cmd argument or default of 2kib
    let mut best_window_size = args.window.map_or(
        2 * 1024, |byte| byte.as_u64());

    //Sets the dictionary size from cmd argument or default of 16kib
    let mut best_dictionary_size = args.dictionary.map_or(
        16 * 1024, |byte| byte.as_u64());

    //Autotune if flag was provided.
    if args.auto_tune {
        let timeout_dur = args.autotune_timeout.
            map(Duration::from_secs);
        //Starting window size
        let mut current_window_size = 512;

        //Value used for testing each auto-tune step
        let mut last_compressed_size = usize::MAX;
        
        //If window size was not specified, find optimal window size.
        if args.window.is_none() {

            loop {
                debug!("Testing window size: {current_window_size}");

                /*Set starting time for determining the compressed size 
                using the current window size*/
                let start_time = Instant::now();

                /*Set temporary db path using task output file name.*/
                let at_stem = format!("auto_tune_{}", file_stem);
                let db_path = cache_dir.join(at_stem)
                    .with_extension("redb");

                //When the guard falls out of scope the scope will be deleted.
                let _auto_tune_db_guard = TempDatabase {
                    path: db_path.clone()
                };
                
                //Make database definition
                let db_at_data_store: TableDefinition<
                    'static, 
                    H, 
                    Vec<u8>
                > = TableDefinition::new("data_store");

                let at_db = if process_in_memory {
                    Database::builder().create_with_backend(
                        InMemoryBackend::new()
                    ).unwrap()
                } else {
                    Database::create(&db_path).unwrap()
                };

                //Create database and encapsulate it in an Arc
                let at_db_info = Arc::new(DBInfo {
                    db: at_db,
                    db_def: db_at_data_store
                });

                let tmp_data_store: Arc<DashMap<H, Vec<u8>>> = Arc::new(
                    DashMap::new()
                );

                let tmp_data_store_clone = Arc::clone(&tmp_data_store);
                let at_db_info_clone = Arc::clone(&at_db_info);

                let at_insert_batch_cb = move | 
                    chunk_batch: &Vec<(H, Vec<u8>)> 
                | {
                    if process_in_memory {
                        for (hash, data) in chunk_batch{
                            tmp_data_store_clone
                                .entry(*hash)
                                .or_insert(data.to_vec());
                        }
                    } else {
                        batch_insert(
                            &at_db_info_clone, 
                            chunk_batch
                        ).unwrap();
                    }
                };

                //Process files with the current window size
                let (_fm,
                    _vh) =
                        process_files(
                            &file_paths, 
                            current_window_size, 
                            at_insert_batch_cb
                        )?;

                /*Callback for getting all keys(hashes) from temporary 
                database*/
                let at_key_ret_cb = || -> Vec<H> {
                    //Get keys from db
                    if process_in_memory {
                        tmp_data_store.iter().map(|entry| 
                            *entry.key()
                        ).collect()
                    } else {
                        get_keys(&at_db_info).unwrap()
                    }
                };

                /*Callback for getting one or more chunks from the temporary 
                database*/
                let at_chunk_ret_cb = {
                    let at_db_info_clone = Arc::clone(&at_db_info);
                    let data_store_for_chunk_ret = Arc::clone(&tmp_data_store);
                    move |hashes: &[H]| -> Vec<Vec<u8>> {
                        if process_in_memory {
                            hashes.iter().map(|hash| {
                                data_store_for_chunk_ret
                                    .get(hash)
                                    .unwrap()
                                    .clone()
                            }).collect()
                        } else {
                            //Get chunk data from db
                            get_chunks(hashes, &at_db_info_clone).unwrap()
                        }
                    }
                };

                //Serialize temporary data
                let (_ser_file_manifest,
                _chunk_index, 
                sorted_hashes) = 
                serialize_uncompressed_data(
                    &_fm, 
                    &at_key_ret_cb,
                    &at_chunk_ret_cb
                )?;
                
                //Calc the sum of all chunks in auto-tune databse
                let total_data_size = if process_in_memory {
                    tmp_data_store
                        .iter()
                        .map(|entry| entry.value().len() as u64)
                        .sum()
                } else {
                    get_tot_data_size(&at_db_info)?
                };

                /*Compress the data and measure the size
                (dictionary size + compressed data size)*/
                let compressed_size = test_compression(
                    &sorted_hashes, 
                    total_data_size,
                    _process_threads,
                    8192,
                    at_chunk_ret_cb,
                )?;
                
                /*Measure the time taken for determining the compressed size 
                for the current window size*/
                let elapsed = start_time.elapsed();

                if compressed_size > last_compressed_size {
                    //Process passed the optimal point, stop.
                    debug!("Optimal window size found to be \
                        {best_window_size} bytes.");
                    break;
                }


                if let Some(timeout) = timeout_dur 
                    && elapsed > timeout
                {
                    warn!("Autotune for window size {current_window_size}\
                        took too long (>{timeout:?}).Using best \
                        result so far: {best_window_size}."
                    );
                }
                
                //This iteration was successful and an improvement
                last_compressed_size = compressed_size;
                best_window_size = current_window_size;

                //Double value for next loop
                current_window_size *= 2;
            }
        }

        /*Using either the autotune best window size or the user set value,
        process all files.*/
        (_file_manifest, _veri_hashes) =
            process_files(&file_paths, 
                best_window_size,
                insert_batch_cb
            )?;
        
        /*Serialize the data for later using it for verifying the files. */
        let (_ser_fm, 
            _ci, 
            sorted_hashes) =
            serialize_uncompressed_data(
                &_file_manifest, 
                &key_ret_cb,
                chunk_ret_cb.as_ref()
            )?;
        
        //Calc the sum of all chunks in databse
        total_size = if process_in_memory {
            data_store
                .iter()
                .map(|entry| entry.value().len() as u64)
                .sum()
        } else {
            get_tot_data_size(&db_info)?
        };

        //Set value back to max to prepare it for the next auto-tune step/
        last_compressed_size = usize::MAX;

        //Starting dictionary size, 8kb
        let mut current_dict_size: usize = 8192; 

        //If dicionary size was not specified, find optimal dictionary size.
        if args.dictionary.is_none() {
            loop {
                debug!("Testing dictionary size: {current_dict_size}");

                let get_chunk_data_for_test = {
                    //Clone the Arc, which is a cheap reference count bump
                    let cb = Arc::clone(&chunk_ret_cb);
                    //This new closure takes ownership of the cloned Arc
                    move |hashes: &[H]| cb(hashes)
                };

                
                /*Given the current dictionary size, determine the size of the
                data*/
                let compressed_size = test_compression(
                    &sorted_hashes,
                    total_size,
                    _process_threads,
                    current_dict_size,
                    get_chunk_data_for_test
                )?;
                

                if compressed_size > last_compressed_size {
                    //Process passed the optimal point, stop.
                    debug!("Optimal dictionary size found to be \
                        {best_dictionary_size} bytes."
                    );
                    break;
                }

                //This iteration was successful and an improvement
                last_compressed_size = compressed_size;
                best_dictionary_size = current_dict_size as u64;

                //Double value for next loop
                current_dict_size *= 2;

                /*Accept reasonable upper limit for dictionary size.
                This will stop it at an accepted value of 1024 * 1024*/
                if current_dict_size > 1024 * 1024 { 
                    break;
                }
            }
        }
    } else {
        /*If not autotuning window or dictionary size, just process files.*/
        (_file_manifest, _veri_hashes) = process_files(
            &file_paths, 
            best_window_size, 
            insert_batch_cb
        )?;

        //Calc the sum of all chunks in databse
        total_size = if process_in_memory {
            data_store
                .iter()
                .map(|entry| entry.value().len() as u64)
                .sum()
        } else {
            get_tot_data_size(&db_info)?
        };
    }

    debug!("All files processed. Verifying data...");

    /*Serialize and organize data. */

    /*ser_file_manifest: Serialized file manifest. 
    Sorted vector that contains:
        The file name as a string.
        How many chunks the file requires.
        A sorted vector contain chunk location data. Sorted offset*/
    
    /*chunk_index: Stores the index of the chunk data associated with a hash.
    Key is the hash, value is the index location.*/

    /*sorted_hashes: Sorted vector that stores all chunk hashes.*/
    
    /*Prepares and serializes all data for the final archive. See above for 
    each variable in the output tuple.*/
    let (ser_file_manifest, 
        chunk_index, 
        sorted_hashes) = serialize_uncompressed_data(
            &_file_manifest, 
            &key_ret_cb,
            chunk_ret_cb.as_ref()
        )?;

    /*Drop file_manifest since it is no longer needed and was only used for 
    verifying files.*/
    drop(_file_manifest);
    
    //Clone ser_file_manifest for use in verification loop
    let sfm_clone = ser_file_manifest.clone();

    //Vector used to track status of each thread.
    let mut thread_handles = vec![];

    /*Pulls each chunk for a file, generating a hash for the file, 
    and verifies the hash matches the hash of original file.*/
    for fmp in sfm_clone{
        let entry = _veri_hashes.get(&fmp.filename).unwrap();
        let veri_hash = *entry.value();

        let get_chunk_data_for_verify = {
            let cb = Arc::clone(&chunk_ret_cb);
            move |hashes: &[H]| cb(hashes)
        };

        let handle = thread::spawn(move ||{
            verify_single_file(&fmp, 
                &veri_hash, 
                get_chunk_data_for_verify
            )
        });
        thread_handles.push(handle);
    }

    /*Check whether each thread completed without error. */
    for handle in thread_handles {
        match handle.join().unwrap() {
            Ok(_) => (), //Verification succeeded for this file
            Err(e) => return Err(e.into()), //Propagate the error
        }
    }

    //At this point, verification is complete. We can free all related data.
    drop(_veri_hashes);
    
    debug!("File verification passed!");

    /*chunk index was storing non compressed data_store locations and is no 
    longer needed.*/
    drop(chunk_index);

    //Define progress bar to be used by callback
    let progress_bar = Arc::new(Mutex::new(None));

    /*Make separate vars for storing quiet and verbose arguments due to 
    closure using move.*/
    let is_quiet = args.quiet;
    
    /*Creates a callback function that updates the progress as milestones are
    met and prints a progress bar.
    If statement enables or disables callback messages depending on mode.*/
    let progress_bar_clone = Arc::clone(&progress_bar);
    let progress_callback = move |progress| {
        if is_quiet {
            return; //Do nothing in quiet mode
        }

        let mut bar_guard = progress_bar_clone.lock().unwrap();
        
        match progress {
            Progress::GeneratingDictionary => {
                debug!("Generating compression dictionary...");
            }
            Progress::DictionaryDone => {
                debug!("Dictionary created.");
            }
            Progress::Compressing { total_chunks } => {
                let new_bar = ProgressBar::new(total_chunks);
                new_bar.set_style(ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta})")
                    .unwrap()
                    .progress_chars("#>-"));
                new_bar.set_message("Compressing chunks...");
                
                //Lock the mutex and place the new bar inside the Option.
                *bar_guard = Some(new_bar);
                
            }
            Progress::ChunkCompressed => {
                //Verify if the progress bar exists, if yes increment it.
                if let Some(bar) = bar_guard.as_ref() {
                    bar.inc(1);
                }
            }
            Progress::Finalizing => {
                debug!("Finalizing archive...");
            }
        }
    };

    //Const for the max amount of chunks before writing to disk.
    const BUFFER_SIZE: usize = 1000;

    /*Write buffer holds chunk byte data prior to being written.
    Vector made with capacity to hold 1000 chunks * the maximum window size
    fastcdc has used. (avg x 4)*/
    let mut write_buffer: Vec<u8> = Vec::with_capacity(
        (1000 * (best_dictionary_size * 4)) as usize
    );

    //Holds the amount of chunks that have been stored in the buffer.
    let mut chunk_count = 0usize;

    //Set the temproary file path that will hold the compressed data.
    let tmp_output_path = args.output
        .as_ref()
        .unwrap()
        .with_extension(".tmp");

    let get_chunk_data_for_lib = {
            let cb = Arc::clone(&chunk_ret_cb);
            move |hashes: &[H]| cb(hashes)
        };

    /*Callback for storing and writing chunk data.
    Flush flag can be used to force the data to be written if the buffer has
    not reached the BUFFER_SIZE.*/
    let tmp_chunk_write_cb = move |chunk: &[u8], flush_flag: bool|{
        write_buffer.extend_from_slice(chunk);
        chunk_count += 1;

        if (chunk_count >= BUFFER_SIZE) || flush_flag {
            append_data_to_file(&tmp_output_path, &write_buffer).unwrap();
            write_buffer.clear();
            chunk_count = 0;
        }
    };

    /*Assembles the final archive header from its constituent parts, structures
    it according to the ssmc spec and returns the byte data ready to be
    written.*/
    let mut builder = ArchiveBuilder::new(
        ser_file_manifest, 
        &sorted_hashes, 
        file_paths.len() as u32,
        *hash_type_id,
        total_size,
        get_chunk_data_for_lib,
        tmp_chunk_write_cb
    );

    //Set optional paramters for the builder.
    builder.compression_level(level)
        .dictionary_size(best_dictionary_size)
        .optimize_dictionary(args.optimize_dictionary)
        .worker_threads(args.threads.unwrap_or(0));
        
        
    //Start the build process using the progress callback.
    let ssmc_data = builder.with_progress(progress_callback)
        .build()?;

    if let Some(bar) = progress_bar.lock().unwrap().take()
        && !args.quiet
    {
        bar.finish_with_message("Compression complete.");
    }
    
    /*Takes the specified output file destination and adds the ssmc file 
    extension if it's not present or replaces the specified extension with
    ssmc.*/
    let final_output_path = args.output
        .as_ref()
        .unwrap()
        .with_extension("ssmc");

    //Write ssmc header to disk and append it with compressed data.
    write_final_archive(&final_output_path, &ssmc_data)?;

    info!(
        "Successfully created sprite-shrink multicart archive at: \
            {final_output_path:?}"
    );

    Ok(())
}

/// Concurrently processes a list of files to chunk data and collect metadata.
///
/// This function orchestrates the parallel processing of input files. It 
/// spawns a pool of worker threads to handle the I/O-intensive task of 
/// reading and chunking files.
///
/// The main components of its operation are:
/// 1.  **Worker Pool**: A `rayon` thread pool is created to run 
///     `file_chunk_worker` on multiple files simultaneously.
/// 2.  **Communication Channels**: It uses `flume` channels to decouple the
///     file chunking process from the data storage process. Worker threads 
///     send chunks to a central receiver.
/// 3.  **Data Aggregation**: While the workers are running, the main thread 
///     receives `ChunkMessage`s and inserts the unique chunks into the 
///     `data_store`. It also collects the `FileManifestParent` and 
///     verification hashes for each file.
///
/// This concurrent design ensures that CPU-bound hashing and I/O-bound file 
/// reading can proceed efficiently without blocking each other.
///
/// # Arguments
///
/// * `file_paths`: A slice of `PathBuf`s representing all the input files to
///   be processed.
/// * `window_size`: The target average chunk size to be used by the chunking
///   algorithm.
/// * `db_info`: A `DBInfo` struct containing the database connection and table
///   definition for storing unique chunks.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   hashing, serialization, and use as a database key.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok((DashMap<String, FileManifestParent<H>>, DashMap<String, [u8; 64]>))`
///   on success. The tuple contains:
///   - A `DashMap` for the file manifest, mapping filenames to their metadata.
///   - A `DashMap` for verification hashes, mapping filenames to their SHA-512
///     hashes.
/// - `Err(CliError)` if any part of the parallel processing fails, such as a
///   worker thread panicking or a failure to unwrap the shared data 
///   structures.
fn process_files<H, W>(
    file_paths: &[PathBuf],
    window_size: u64,
    //db_info: &DBInfo<H, Vec<u8>>,
    mut insert_batch_cb: W
) -> Result<(
        DashMap<String, FileManifestParent<H>>, //file_manifest
        DashMap<String, [u8; 64]> //veri_hashes
    ), CliError>
where
    H: Hashable
        + Ord
        + Display
        + Serialize
        + for<'de> serde::Deserialize<'de>
        + redb::Key
        + Send // Ensure H is Send
        + 'static,
    for<'a> H: Borrow<<H as Value>::SelfType<'a>>,
    W: FnMut(&Vec<(H, Vec<u8>)>) + Send + 'static,
{
    //Initialize flume channels for inter thread communication
    let (sender, receiver) = bounded(1000);

    let (batch_sender, batch_receiver) = bounded::<Vec<(H, Vec<u8>)>>(100);

    let writer_handle = thread::spawn(move || {
        while let Ok(batch) = batch_receiver.recv() {
            insert_batch_cb(&batch);
        }
    });

    let shared_file_manifest: Arc<DashMap<
        String,
        FileManifestParent<H>>
    > = Arc::new(DashMap::new());

    let shared_veri_hashes: Arc<
        DashMap<String, 
        [u8; 64]>
    > = Arc::new(DashMap::new());

    let thread_count = thread::available_parallelism().unwrap().get();

    let task_thread_count = std::cmp::max(
        1,
        thread_count.saturating_sub(1)
    );

    let worker_pool = {
        let builder = rayon::ThreadPoolBuilder::new()
            .num_threads(task_thread_count);

        builder.build()
            .map_err(|e| CliError::InternalError(
                format!("Failed to create thread pool: {e}")))?
    };

    let worker_file_manifest = shared_file_manifest.clone();
    let worker_veri_hashes = shared_veri_hashes.clone();

    let file_paths_owned = file_paths.to_owned();

    let worker_handle = thread::spawn(move || {
        worker_pool.install(||{
            file_paths_owned
                .par_iter()
                .for_each(|path| {
                    let sender_clone = sender.clone();
                    let fcd = file_chunk_worker(
                        path, 
                        window_size, 
                        sender_clone
                    ).unwrap();

                    worker_veri_hashes.insert(
                        fcd.file_name.clone(), 
                        fcd.verification_hash
                    );

                    let mut fmp = FileManifestParent{
                        filename: fcd.file_name, 
                        chunk_count: fcd.chunk_count,
                        chunk_metadata: fcd.chunk_meta
                    };

                    fmp.chunk_metadata.sort_by_key(|metadata| 
                        metadata.offset
                    );

                    worker_file_manifest.insert(
                        fmp.filename.clone(), 
                        fmp
                    );
                })
        });
    });

    const BATCH_SIZE: usize = 1000;
    let mut chunk_batch: Vec<(H, Vec<u8>)> = Vec::with_capacity(BATCH_SIZE);
    
    //While workers are processing files, loop until done.
    while let Ok(message) = receiver.recv() {
        //For every message received, store in chunk batch.
        chunk_batch.push((message.chunk_hash, message.chunk_data));

        if chunk_batch.len() >= BATCH_SIZE {
            //When the batch is full, write it to the database or data_store
            batch_sender.send(std::mem::take(&mut chunk_batch)).unwrap();
            chunk_batch.reserve(BATCH_SIZE);
        }
    };

    //Write any remaining chunks in the batch
    if !chunk_batch.is_empty() {
        batch_sender.send(chunk_batch).unwrap();
    };

    //Batch sender is done so drop it and clean up any related threads.
    drop(batch_sender);
    writer_handle.join().unwrap();

    //Check for any worker errors.
    worker_handle.join().map_err(|_| {
        CliError::InternalError(
            "File processing worker thread panicked.".to_string()
        )
    })?;

    //Pull manifest and hashes from their Arcs.
    let final_manifest = Arc::try_unwrap(shared_file_manifest)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for file manifest".to_string()))?;
    let final_hashes = Arc::try_unwrap(shared_veri_hashes)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for verification hashes".to_string()))?;

    Ok((final_manifest, final_hashes))
}

/// Processes a single file to chunk its data and send the chunks through a
/// channel.
///
/// This function is designed to run in a worker thread. It reads a file from
/// the provided `file_path`, processes it as a stream, and breaks it into 
/// content-defined chunks using `StreamCDC`.
///
/// For each chunk, it performs the following actions:
/// 1.  Calculates a hash of the chunk's data.
/// 2.  Sends the chunk's hash and its binary data to the main processing 
///     thread via the `send_channel`.
/// 3.  Collects metadata (`SSAChunkMeta`) for the chunk, including its hash, 
///     original offset, and length.
///
/// Additionally, it computes a single SHA-512 verification hash for the 
/// entire file to ensure data integrity during later stages.
///
/// # Arguments
///
/// * `file_path`: A `PathBuf` pointing to the input file to be processed.
/// * `window_size`: The target average chunk size for the content-defined 
///   chunking algorithm.
/// * `send_channel`: A `flume::Sender` used to send `ChunkMessage`s 
///   (containing the chunk hash and data) to a receiver on another thread for
///   storage and further processing.
///
/// # Type Parameters
///
/// * `H`: A generic type that must implement the `Hashable` and `Send` traits.
///   This allows the function to work with different hash types 
///   (e.g., `u64`, `u128`).
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(FileCompleteData<H>)` on success, containing the file's name, its
///   complete verification hash, and metadata for all of its chunks.
/// - `Err(CliError)` if any I/O operation fails or if chunking the file 
///   encounters an error.
fn file_chunk_worker<H>(
    file_path: &PathBuf,
    window_size: u64,
    send_channel: flume::Sender<ChunkMessage<H>>
) -> Result<FileCompleteData<H>, CliError>
where
    H: Hashable + Send,
{
    let mut hasher = Sha512::new();

    let mut chunk_meta: Vec<SSAChunkMeta<H>> = Vec::new(); 

    let file = File::open(file_path)?;
    let file_name = file_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let mut chunk_count: u64 = 0;

    let stream_chunker = StreamCDC::with_level_and_seed(
        file, 
        (window_size / 4) as u32, 
        window_size as u32, 
        (window_size * 4) as u32,
        Normalization::Level1,
        SS_SEED
    );

    for result in stream_chunker {
        let chunk = result?;

        hasher.update(chunk.data.as_slice());

        let chunk_hash = H::from_bytes_with_seed(
            chunk.data.as_slice()
        );

        let message = ChunkMessage::<H>{
            chunk_hash,
            chunk_data: chunk.data
        };

        send_channel.send(message).unwrap();
        chunk_count += 1;
        chunk_meta.push(SSAChunkMeta{
            hash: chunk_hash,
            offset: chunk.offset,
            length: chunk.length as u32
        });
    };

    Ok(FileCompleteData {
        file_name,
        verification_hash: hasher.finalize().into(),
        chunk_count,
        chunk_meta
    })
}

/// Determines whether the compression process should use an in-memory 
/// database or a temporary file-based database on disk.
///
/// This function assesses the trade-off between performance and memory usage.
/// Using an in-memory database is significantly faster but requires enough
/// available RAM to hold all unique chunk data. Using a file-based database
/// is slower due to disk I/O but supports processing datasets much larger
/// than the available memory.
///
/// The decision is made based on a heuristic: it checks if 80% of the
/// system's currently free memory is greater than the total size of the
/// input data plus a conservative buffer. This buffer accounts for the
/// overhead of other data structures used during the compression pipeline,
/// such as the file manifest and various temporary collections.
///
/// # Arguments
///
/// * `input_data_size`: The total combined size in bytes of all files to be
///   processed.
///
/// # Returns
///
/// * `true` if there is likely sufficient memory to safely use an in-memory
///   database.
/// * `false` if it is safer to use a temporary database on disk to avoid
///   potential out-of-memory errors.
fn process_in_memory_check (
    input_data_size: u64,
) -> bool {
    let mut system_info = System::new_all();
    system_info.refresh_all();

    let free_mem = system_info.free_memory();

    if (0.8 * free_mem as f64) > (input_data_size + 
        (u32::MAX as u64) + 
        ((u32::MAX / 4) as u64)) as f64 {
            debug!("Processing in memory.");
            true
        } else {
            debug!("Processing in disk cache.");
            false
        }
}