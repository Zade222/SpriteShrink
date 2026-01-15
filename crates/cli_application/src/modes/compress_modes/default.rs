//! Implements the compression mode for the command-line application.
//!
//! This module contains the primary logic for the file compression and
//! archiving process. It defines the `run_compression` function, which
//! orchestrates a multi-stage pipeline to read input files, process
//! them in parallel to find duplicate data chunks, and then serialize,
//! verify, and compress the unique data into a final archive file.

use std::{
    fmt::{Debug, Display},
    fs::{File, create_dir_all},
    path::PathBuf,
    sync::{Arc, Mutex, atomic::{
        AtomicBool, Ordering
    }},
    thread, time::{Duration}
};

use bitcode::{Encode, encode};
use dashmap::DashMap;
use directories::ProjectDirs;
use indicatif::{ProgressBar, ProgressStyle};
use fastcdc::v2020::{StreamCDC, Normalization};
use flume::{bounded};
use sprite_shrink::{
    SS_SEED, SSMC_UID,
    ArchiveBuilder, FileHeader, FileManifestParent, Hashable, Progress,
    SSAChunkMeta, SSMCFormatData,
    serialize_uncompressed_data, verify_single_file
};
use rayon::prelude::*;
use serde::Serialize;
use sha2::{Digest,Sha512};
use tracing::{
    debug,
    info,
};
use zerocopy::IntoBytes;

use crate::{
    arg_handling::Args,
    auto_tune::{auto_tune_dict, auto_tune_win},
    cli_types::{
        ChunkMessage, FileData, FileCompleteData,
        TempFileGuard, APPIDENTIFIER
    },
    error_handling::CliError,
    storage_io::{
        append_data_to_file, calc_tot_input_size, get_cache_paths,
        write_final_archive, TempCache
    },
    utils::{process_in_memory_check, set_priority},
};

/// Executes the file compression and archiving process.
///
/// This is the main entry point for the compression operation. It manages a
/// multi-stage pipeline that reads input files, processes them in parallel to
/// identify duplicate data chunks, and then serializes, verifies, and
/// compresses the unique data into a final archive file.
///
/// A key feature of this process is its use of a temporary on disk cache to
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
/// * `running`: An `Arc<AtomicBool>` used for graceful cancellation of the
///   operation.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   hashing and serialization
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
/// - Any error propagated from file I/O transactions,
///   data processing, verification, or final archive writing stages.
pub fn default_compression<H>(
    file_paths: Vec<PathBuf>,
    args: &Args,
    hash_type_id: &u8,
    running: Arc<AtomicBool>
) -> Result<(), CliError>
where
    H: Hashable
        + Debug
        + Display
        + Encode
        + Ord
        + Serialize
        + for<'de> serde::Deserialize<'de>
{
    debug!("Running default compression mode.");

    set_priority()?;

    let input_data_size = calc_tot_input_size(&file_paths)?;

    let process_in_memory = if args.low_memory {
        false
    } else {
        process_in_memory_check(input_data_size)
    };

    let proj_dirs = ProjectDirs::from(
        APPIDENTIFIER.qualifier,
        APPIDENTIFIER.organization,
        APPIDENTIFIER.application)
    .unwrap();

    let cache_dir = proj_dirs.cache_dir();
    create_dir_all(cache_dir)?;

    let cache_info = get_cache_paths();

    let pid = cache_info.id;

    let temp_cache = Arc::new(
        TempCache::<H>::new(cache_info.cache_path, process_in_memory)?
    );

    /*Stores the chunk metadata for each file.
    The key is the file name and the value is a FileManifestParent struct.*/
    let file_manifest: DashMap<
        String,
        FileManifestParent<H>
    >;

    /*Stores the SHA-512 hash for each file.
    The String is the file name and the array is the 512 bit hash as a 64 byte
    array.*/
    let veri_hashes: DashMap<String, [u8; 64]>;

    //Used for storing overall file size.
    let chunk_sum: u64;

    /*Numerical compression level pulled from the args.*/
    let level: i32 = args.compression_level as i32;

    /*Stores the amount of threads used by the parallel task of running
    process_file_in_memory function.*/
    let process_threads = args.threads.map_or(0, |count| count);

    //Callback for getting all keys (hashes) in cache
    let key_ret_cb = {
        let cache_clone = Arc::clone(&temp_cache);
        let running_clone = Arc::clone(&running);
        move || -> Result<Vec<H>, CliError> {
            if !running_clone.load(Ordering::SeqCst) {
                return Err(CliError::Cancelled);
            }

            cache_clone.get_keys()
        }
    };

    //Callback for getting a list of chunks from the provided list of hashes
    let chunk_ret_cb = Arc::new({
        let cache_clone = Arc::clone(&temp_cache);
        let running_clone = Arc::clone(&running);
        move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
            if !running_clone.load(Ordering::SeqCst) {
                return Err(CliError::Cancelled);
            }

            cache_clone.get_chunks(hashes)
        }
    });

    let insert_batch_cb = {
        let cache_clone = Arc::clone(&temp_cache);
        let running_clone = Arc::clone(&running);
        move |
            chunk_batch: &[(H, Vec<u8>)]
        | -> Result<(), CliError> {
            if !running_clone.load(Ordering::SeqCst) {
                return Err(CliError::Cancelled);
            }

            cache_clone.insert_batch(chunk_batch)?;
            Ok(())
        }
    };

    /*Create read_pool to specify the amount of threads to be used by the
    parallel process that follows it.*/
    let _process_pool = {
        let builder = rayon::ThreadPoolBuilder::new()
            .num_threads(process_threads);

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
        //If window size was not specified, find optimal window size.
        if args.window.is_none() {
            let processor = |
                    window_size: u64
            | -> Result<(FileData<H>, Arc<TempCache<H>>), CliError> {
                let at_temp_cache = Arc::new(
                    TempCache::<H>::new(
                        cache_info.at_cache_path.clone(),
                        process_in_memory
                    ).unwrap()
                );

                let at_insert_batch_cb = {
                    let cache_clone = Arc::clone(&at_temp_cache);
                    let running_clone = Arc::clone(&running);
                    move |
                        chunk_batch: &[(H, Vec<u8>)]
                    | -> Result<(), CliError> {
                        if !running_clone.load(Ordering::SeqCst) {
                            return Err(CliError::Cancelled);
                        }

                        cache_clone.insert_batch(chunk_batch)?;
                        Ok(())
                    }
                };

                let file_data = process_files(
                    &file_paths,
                    window_size,
                    &input_data_size,
                    false,
                    at_insert_batch_cb,
                    &running
                )?;

                Ok((file_data, at_temp_cache))
            };


            best_window_size = auto_tune_win(args, &running, processor)?;

        }

        /*Using either the autotune best window size or the user set value,
        process all files.*/
        let _temp_data = process_files(
            &file_paths,
            best_window_size,
            &input_data_size,
            args.progress,
            insert_batch_cb,
            &running
        )?;

        file_manifest = _temp_data.file_manifest;
        veri_hashes = _temp_data.veri_hashes;

        /*Serialize the data for later using it for verifying the files. */
        let temp_serialized_data = serialize_uncompressed_data(
                &file_manifest,
                &key_ret_cb,
                chunk_ret_cb.as_ref()
            )?;

        //Calc the sum of all chunks in cache
        chunk_sum = temp_cache.get_tot_data_size();

        //If dicionary size was not specified, find optimal dictionary size.
        if args.dictionary.is_none() {
            let get_chunk_data_for_test = {
                let cb = Arc::clone(&chunk_ret_cb);
                move |hashes: &[H]| cb(hashes)
            };

            best_dictionary_size = auto_tune_dict(
                args,
                chunk_sum,
                &temp_serialized_data,
                get_chunk_data_for_test
            )?;
        }
    } else {
        /*If not autotuning window or dictionary size, just process files.*/
        let temp_data = process_files(
            &file_paths,
            best_window_size,
            &input_data_size,
            args.progress,
            insert_batch_cb,
            &running,
        )?;

        file_manifest = temp_data.file_manifest;
        veri_hashes = temp_data.veri_hashes;

        //Calc the sum of all chunks in databse
        chunk_sum = temp_cache.get_tot_data_size();
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
    let serialized_data = serialize_uncompressed_data(
            &file_manifest,
            &key_ret_cb,
            chunk_ret_cb.as_ref()
        )?;

    /*Drop file_manifest since it is no longer needed and was only used for
    verifying files.*/
    drop(file_manifest);

    let archive_toc = serialized_data.archive_toc;

    let verification_pb = if args.progress {
        let bar = ProgressBar::new(input_data_size);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bytes}/{total_bytes} ({eta}) {msg}\n[{bar:40}]")
                .unwrap()
                .progress_chars("#>-"),
        );
        bar.set_message("Verifying files...");
        Some(bar)
    } else {
        None
    };

    let verification_pb_arc = Arc::new(verification_pb);

    //Vector used to track status of each thread.
    let mut thread_handles = vec![];

    /*Pulls each chunk for a file, generating a hash for the file,
    and verifies the hash matches the hash of original file.*/
    for (i, fmp) in serialized_data.ser_file_manifest.iter().enumerate() {
        let file_title = archive_toc[i].filename.clone();
        let fmp_owned = fmp.clone();

        let entry = veri_hashes.get(&file_title)
            .ok_or_else(|| CliError::InternalError(format!(
            "Verification hash missing for file: {}",
            file_title
        )))?;

        let veri_hash = *entry.value();

        let get_chunk_data_for_verify = {
            let cb = Arc::clone(&chunk_ret_cb);
            let verify_running_clone = Arc::clone(&running);
            move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
                if !verify_running_clone.load(Ordering::SeqCst) {
                    return Err(CliError::Cancelled);
                }
                cb(hashes)
            }
        };

        let pb_clone = Arc::clone(&verification_pb_arc);

        let progress_closure = move |bytes_processed: u64| {
            if let Some(bar) = &*pb_clone {
                bar.inc(bytes_processed);
            }
        };

        let handle = thread::spawn(move ||{
            verify_single_file(
                file_title,
                &fmp_owned,
                &veri_hash,
                get_chunk_data_for_verify,
                progress_closure
            )
        });
        thread_handles.push(handle);
    }

    /*Check whether each thread completed without error. */
    for handle in thread_handles {
        match handle.join()
            .expect("A verification thread panicked.") {
            Ok(_) => (), //Verification succeeded for this file
            Err(e) => return Err(e.into()), //Propagate the error
        }
    }

    if let Some(bar) = &*verification_pb_arc {
        bar.set_position(input_data_size);

        bar.finish_with_message("File verification passed!");
    }
    //At this point, verification is complete. We can free all related data.
    drop(veri_hashes);

    debug!("File verification passed!");

    /*chunk index was storing non compressed data_store locations and is no
    longer needed.*/
    drop(serialized_data.chunk_index);

    //Define progress bar to be used by callback
    let progress_bar = Arc::new(Mutex::new(None));

    /*Make separate vars for storing quiet and verbose arguments due to
    closure using move.*/
    let is_quiet = args.quiet;
    let is_progress = args.progress;

    /*Creates a callback function that updates the progress as milestones are
    met and prints a progress bar.*/
    let progress_bar_clone = Arc::clone(&progress_bar);
    let progress_callback = move |progress| {
        if is_quiet || !is_progress{
            return; //Do nothing in quiet mode
        }

        let mut guard = progress_bar_clone.lock().unwrap();

        match progress {
            Progress::GeneratingDictionary => {
                debug!("Generating compression dictionary...");

                let dict_spin = ProgressBar::new_spinner();
                dict_spin.enable_steady_tick(Duration::from_millis(500));
                dict_spin.set_style(
                    ProgressStyle::with_template("{msg} {spinner}")
                        .unwrap()
                        .tick_strings(&[
                            "   ",
                            ".  ",
                            ".. ",
                            "...",
                            " ..",
                            "  .",
                            "   ",
                        ]),
                );
                dict_spin.set_message("Generating compression dictionary");

                *guard = Some(dict_spin);
            }
            Progress::DictionaryDone => {
                if let Some(dict_spin) = guard.as_ref() {
                    dict_spin.finish_with_message("Dictionary created.");
                }

                *guard = None;

                debug!("Dictionary created.");
            }
            Progress::Compressing { total_chunks } => {
                let new_bar = ProgressBar::new(total_chunks);
                new_bar.set_style(ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] {pos}/{len} (eta {eta}) {msg}\n[{bar:40}]")
                    .unwrap()
                    .progress_chars("#>-"));
                new_bar.set_message("Compressing chunks...");

                //Lock the mutex and place the new bar inside the Option.
                *guard = Some(new_bar);

            }
            Progress::ChunkCompressed => {
                //Verify if the progress bar exists, if yes increment it.
                if let Some(bar) = guard.as_ref() {
                    bar.inc(1);
                }
            }
            Progress::Finalizing => {
                debug!("Finalizing archive...");
            }
        }
    };

    //Const for the max amount of chunks to cache before writing to disk.
    const BUFFER_SIZE: usize = 1000;

    /*Write buffer holds chunk byte data prior to being written.
    Vector made with capacity to hold 1000 chunks * the maximum window size
    fastcdc has used. (avg x 4)*/
    let mut write_buffer: Vec<u8> = Vec::with_capacity(
        BUFFER_SIZE * (best_window_size as usize * 4)
    );

    //Holds the amount of chunks that have been stored in the buffer.
    let mut chunk_count = 0usize;

    //Set the temproary file path that will hold the compressed data.
    let tmp_file_path = Arc::new(
        args.output
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("{pid}"))
        .with_extension("tmp")
    );

    let get_chunk_data_for_lib = {
        let cb = Arc::clone(&chunk_ret_cb);
        let lib_running_clone = Arc::clone(&running);
        move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
            if !lib_running_clone.load(Ordering::SeqCst) {
                return Err(CliError::Cancelled);
            }

            let ret_chunks = cb(hashes)?;

            Ok(ret_chunks)
        }
    };

    /*Set file guard to clean up tmp files on error. */
    let _tmp_guard = TempFileGuard::new(&tmp_file_path);

    /*Callback for storing and writing chunk data.
    Flush flag can be used to force the data to be written if the buffer has
    not reached the BUFFER_SIZE.*/
    let tmp_chunk_write_cb = {
        let tfp_clone = Arc::clone(&tmp_file_path);
        move |chunk: &[u8], flush_flag: bool| -> Result<(), CliError>{
            write_buffer.extend_from_slice(chunk);
            chunk_count += 1;

            if (chunk_count >= BUFFER_SIZE) || flush_flag {
                append_data_to_file(&tfp_clone, &write_buffer)?;
                write_buffer.clear();
                chunk_count = 0;
            };

            Ok(())
        }
    };

    /*Assembles the final archive header from its constituent parts, structures
    it according to the ssmc spec and returns the byte data ready to be
    written.*/
    let mut builder = ArchiveBuilder::new(
        &serialized_data.sorted_hashes,
        chunk_sum,
        get_chunk_data_for_lib,
        tmp_chunk_write_cb
    );

    //Set optional paramters for the builder.
    builder.compression_level(level)
        .dictionary_size(best_dictionary_size)
        .optimize_dictionary(args.optimize_dictionary)
        .worker_threads(args.threads.unwrap_or(0));

    //Start the build process using the progress callback.
    let comp_data = builder.with_progress(progress_callback)
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

    let enc_toc = encode(&archive_toc);

    let enc_file_manifest = encode(&serialized_data.ser_file_manifest);

    let file_header = FileHeader::build_file_header(
        file_paths.len() as u32,
        98, //zstd
        *hash_type_id,
        SSMC_UID,
        enc_toc.len() as u32,
    );

    let format_data = SSMCFormatData::build_format_data(
        enc_toc.len() + FileHeader::HEADER_SIZE as usize,
        enc_file_manifest.len(),
        comp_data.dictionary_size,
        comp_data.enc_chunk_index_size
    );

    let mut final_data = Vec::with_capacity(
        FileHeader::HEADER_SIZE as usize + enc_toc.len() +
            SSMCFormatData::SIZE as usize + enc_file_manifest.len() +
            comp_data.dictionary.len() + comp_data.enc_chunk_index.len()
    );

    final_data.extend_from_slice(file_header.as_bytes());
    final_data.extend_from_slice(&enc_toc);
    final_data.extend_from_slice(format_data.as_bytes());
    final_data.extend_from_slice(&enc_file_manifest);
    final_data.extend_from_slice(&comp_data.dictionary);
    final_data.extend_from_slice(&comp_data.enc_chunk_index);

    //Write ssmc header to disk and append it with compressed data.
    write_final_archive(
        &final_output_path,
        &tmp_file_path,
        &final_data
    )?;

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
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   hashing and serialization.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(FileData<H>)`on success. The struct contains:
///   - A `DashMap` for the file manifest, mapping filenames to their metadata.
///   - A `DashMap` for verification hashes, mapping filenames to their SHA-512
///     hashes.
/// - `Err(CliError)` if any part of the parallel processing fails, such as a
///   worker thread panicking or a failure to unwrap the shared data
///   structures.
fn process_files<H, W>(
    file_paths: &[PathBuf],
    window_size: u64,
    total_input_size: &u64,
    print_progress: bool,
    mut insert_batch_cb: W,
    running: &Arc<AtomicBool>,
) -> Result<FileData<H>, CliError>
where
    H: Hashable
        + Ord
        + Display
        + Serialize
        + for<'de> serde::Deserialize<'de>
        + Send
        + 'static,
    W: FnMut(&[(H, Vec<u8>)]) -> Result<(), CliError> + Send + 'static,

{
    let progress_bar = if print_progress {
        let bar = ProgressBar::new(*total_input_size);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bytes}/{total_bytes} ({eta}) {msg}\n[{bar:40}]")
                .unwrap()
                .progress_chars("#>-"),
        );
        bar.set_message("Chunking files...");
        Some(bar)
    } else {
        None
    };

    const BATCH_SIZE: usize = 1000;

    //Initialize flume channels for inter thread communication
    let (sender, receiver) = bounded(BATCH_SIZE);

    let (batch_sender, batch_receiver) = bounded::<Vec<(H, Vec<u8>)>>(100);

    let writer_handle = thread::spawn(move || -> Result<(), CliError> {
        while let Ok(batch) = batch_receiver.recv() {
            insert_batch_cb(&batch)?;
        };

        Ok(())
    });

    let shared_file_manifest: Arc<DashMap<
        String,
        FileManifestParent<H>
    >> = Arc::new(DashMap::new());

    let shared_veri_hashes: Arc<
        DashMap<String,
        [u8; 64]
    >> = Arc::new(DashMap::new());

    let thread_count = thread::available_parallelism()?.get();

    //Reserve one thread for the mainprocessing/aggregation logic.
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

    let worker_handle = thread::spawn(move || -> Result<(), CliError> {
        worker_pool.install(|| -> Result<(), CliError>{
            file_paths_owned
                .par_iter()
                .try_for_each(|path| -> Result<(), CliError> {
                    let sender_clone = sender.clone();
                    let fcd = file_chunk_worker(
                        path,
                        window_size,
                        sender_clone
                    )?;

                    worker_veri_hashes.insert(
                        fcd.file_name.clone(),
                        fcd.verification_hash
                    );

                    let mut fmp = FileManifestParent{
                        chunk_count: fcd.chunk_count,
                        chunk_metadata: fcd.chunk_meta
                    };

                    fmp.chunk_metadata.sort_by_key(|metadata|
                        metadata.offset
                    );

                    worker_file_manifest.insert(
                        fcd.file_name.clone(),
                        fmp
                    );

                    Ok(())
                })?;
            Ok(())
        })?;
        Ok(())
    });

    let mut chunk_batch: Vec<(H, Vec<u8>)> = Vec::with_capacity(BATCH_SIZE);

    //While workers are processing files, loop until done.
    while let Ok(message) = receiver.recv() {
        if !running.load(Ordering::SeqCst) {
            //Early exit on cancellation
            return Err(CliError::Cancelled);
        }

        //For every message received, store in chunk batch.
        chunk_batch.push((message.chunk_hash, message.chunk_data));

        if chunk_batch.len() >= BATCH_SIZE {
            //When the batch is full, write it to the cache
            batch_sender.send(std::mem::take(&mut chunk_batch))
                .map_err(|e| CliError::FlumeSendError(e.to_string()))?;
            chunk_batch.reserve(BATCH_SIZE);
        }

        //For every received chunk, increment progress bar.
        if let Some(bar) = &progress_bar {
            bar.inc(message.chunk_size as u64);
        }
    };

    //Write any remaining chunks in the batch
    if !chunk_batch.is_empty() {
        batch_sender.send(chunk_batch)
            .map_err(|e| CliError::FlumeSendError(e.to_string()))?;
    };

    //Batch sender is done so drop it and clean up any related threads.
    drop(batch_sender);
    writer_handle.join().expect("The writer thread panicked.")?;

    //Check for any worker errors.
    worker_handle.join().expect("A file chunk worker thread panicked.")?;

    //Finish the progress bar
    if let Some(bar) = progress_bar {
        bar.finish_with_message("File chunking complete.");
    }

    //Pull manifest and hashes from their Arcs.
    let final_manifest = Arc::try_unwrap(shared_file_manifest)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for file manifest".to_string()))?;
    let final_hashes = Arc::try_unwrap(shared_veri_hashes)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for verification hashes".to_string()))?;

    let final_data = FileData{
        file_manifest: final_manifest,
        veri_hashes: final_hashes,
    };

    Ok(final_data)
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
        Normalization::Level0,
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
            chunk_data: chunk.data,
            chunk_size: chunk.length,
        };

        if send_channel.send(message).is_err() {
            break;
        }

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
