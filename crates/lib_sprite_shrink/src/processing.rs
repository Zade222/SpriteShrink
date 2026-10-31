//! Handles the in-memory processing and chunking of files.
//!
//! This module provides the core logic for taking raw file data,
//! breaking it down into content-defined chunks, and generating the
//! necessary metadata for deduplication and archival. It includes
//! functions for chunking and hashing, as well as for rebuilding and
//! verifying files from their chunks.

use std::{
    ffi::c_void,
    fmt::Debug,
    hash::Hash,
    sync::{Arc, Mutex},
    thread
};

use bitcode::Encode;
use fastcdc::v2020::{AVERAGE_MAX, AVERAGE_MIN, Chunk, FastCDC, Normalization};
use thiserror::Error;
use sha2::{Digest,Sha512};
use xxhash_rust::xxh3::{xxh3_64_with_seed, xxh3_128_with_seed};
use zstd_sys::{self, ZDICT_params_t};

use crate::archive::{compress_chunks};
use crate::lib_error_handling::{
    IsCancelled, SpriteShrinkError
};
use crate::lib_structs::{
    FileData, FileManifestParent, ProcessedFileData, SeekMetadata, SSAChunkMeta
};
use crate::parsing::{SS_SEED};

#[derive(Error, Debug)]
pub enum ProcessingError {
    #[error("Failed to generate dictionary samples")]
    SampleGeneration,

    #[error("Verification failed for file: {0}")]
    HashMismatchError(String),

    #[error("An internal logic error occurred: {0}")]
    InternalError(String),

    #[error("Dictionary creation failed: {0}")]
    DictionaryError(String),

    #[error("Seek request is outside the file bounds: {0}")]
    SeekOutOfBounds(String),

    #[error(
        "Window size {0} is outside the supported range of {min} to {max} \
         bytes",
        min = MIN_WINDOW_SIZE,
        max = MAX_WINDOW_SIZE
    )]
    InvalidWindowSize(u64),

    #[error("File Verification failed: {0}")]
    VerificationError(String),

    #[error("A thread panic occurred: {0}")]
    ThreadPanic(String),

    #[error("An error occurred in an external callback: {0}")]
    External(String),

    #[error("Operation cancelled by user.")]
    Cancelled,
}

/// The smallest window size, in bytes, that the chunker accepts.
///
/// The chunker cuts chunks between a quarter of the window size and four
/// times it. `FastCDC`'s limits on those three sizes all reduce to its limits
/// on the average size, which is the window size itself.
pub const MIN_WINDOW_SIZE: u64 = AVERAGE_MIN as u64;

/// The largest window size, in bytes, that the chunker accepts. See
/// [`MIN_WINDOW_SIZE`].
pub const MAX_WINDOW_SIZE: u64 = AVERAGE_MAX as u64;

pub trait Hashable:
    //Specifies what capabilities H must have.
    Copy + Clone + Debug + Eq + std::hash::Hash + serde::Serialize + Send + Sync +
        'static
{
    /// Create a new hash from a byte slice using the library's seed.
    fn from_bytes_with_seed(bytes: &[u8]) -> Self;
}

/// Implements the Hashable trait for 64-bit unsigned integers.
impl Hashable for u64 {
    fn from_bytes_with_seed(bytes: &[u8]) -> Self {
        xxh3_64_with_seed(bytes, SS_SEED)
    }
}

/// Implements the Hashable trait for 128-bit unsigned integers.
impl Hashable for u128 {
    fn from_bytes_with_seed(bytes: &[u8]) -> Self {
        xxh3_128_with_seed(bytes, SS_SEED)
    }
}

/// Processes a single file that has been loaded into memory.
///
/// This function takes a `FileData` struct, containing the file's name
/// and its contents. It then performs content-defined chunking on the
/// data and generates a SHA-512 verification hash. The results are
/// bundled into a `ProcessedFileData` struct, ready for the next stage
/// of archiving. Empty files are skipped.
///
/// # Arguments
///
/// * `file_data`: A `FileData` struct with the in-memory file data.
/// * `window_size`: The target average chunk size in bytes, from
///   [`MIN_WINDOW_SIZE`] to [`MAX_WINDOW_SIZE`].
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Some(ProcessedFileData))` on successful processing.
/// - `Ok(None)` if the input file data is empty.
///
/// # Errors
///
/// Returns `ProcessingError::InvalidWindowSize` if `window_size` is outside
/// the supported range, even when the file is empty.
pub fn process_file_in_memory(
    file_data: FileData,
    window_size: u64,
) -> Result<Option<ProcessedFileData>, SpriteShrinkError> {
    let chunker_sizes = chunker_sizes(window_size)?;

    if file_data.file_data.is_empty() {
        return Ok(None);
    }

    let veri_hash = generate_sha_512(&file_data.file_data);
    let chunks = chunk_data(&file_data, SS_SEED, chunker_sizes);

    Ok(Some(ProcessedFileData {
        file_name: file_data.file_name,
        veri_hash,
        chunks,
        file_data: file_data.file_data,
    }))
}

/// The minimum, average and maximum chunk sizes for `window_size`, as
/// `FastCDC` takes them.
///
/// # Errors
///
/// Returns `ProcessingError::InvalidWindowSize` if `window_size` is outside
/// [`MIN_WINDOW_SIZE`] to [`MAX_WINDOW_SIZE`]. `FastCDC` panics on sizes
/// outside its limits, and a window size above `u32::MAX` could otherwise
/// be truncated into the range and chunk with a different size than asked.
fn chunker_sizes(window_size: u64) -> Result<(u32, u32, u32), ProcessingError> {
    let avg_size = u32::try_from(window_size)
        .ok()
        .filter(|size| (AVERAGE_MIN..=AVERAGE_MAX).contains(size))
        .ok_or(ProcessingError::InvalidWindowSize(window_size))?;

    // At most 4 MiB, so four times it still fits in a u32.
    Ok((avg_size / 4, avg_size, avg_size * 4))
}

/// Generates a SHA-512 hash for a given data slice.
///
/// This function computes a cryptographic hash of the input data, which
/// is used as a robust verification checksum. This allows the system to
/// confirm that a file has been reconstructed correctly by comparing the
/// hash of the rebuilt file with the original.
///
/// # Arguments
///
/// * `data`: A byte slice (`&[u8]`) of the data to be hashed.
///
/// # Returns
///
/// A 64 byte array representing the computed SHA-512 hash.
fn generate_sha_512(data: &[u8]) -> [u8; 64]{
    let mut hasher = Sha512::new();

    hasher.update(data);

    hasher.finalize().into()
}

/// Splits file data into content-defined chunks using FastCDC.
///
/// This function applies a content-defined chunking algorithm to a byte
/// slice of file data. It identifies boundaries based on the content
/// itself, which is effective for deduplication, as identical data
/// segments will produce identical chunks.
///
/// # Arguments
///
/// * `file_data`: A reference to the `FileData` struct to be chunked.
/// * `seed`: A `u64` value to initialize the chunking algorithm.
/// * `(min_size, avg_size, max_size)`: The chunk size bounds, from
///   [`chunker_sizes`].
///
/// The size bounds and normalization level match the CLI's `StreamCDC`
/// chunker, so a file is cut at the same points whether it's chunked in
/// memory or streamed. Level 0 produced smaller archives than level 1.
///
/// # Returns
///
/// A `Result` which is:
/// - `Vec<Chunk>` containing all the identified data chunks.
fn chunk_data(
    file_data: &FileData,
    seed: u64,
    (min_size, avg_size, max_size): (u32, u32, u32),
) -> Vec<Chunk> {
    let chunker = FastCDC::with_level_and_seed(
        &file_data.file_data,
        min_size,
        avg_size,
        max_size,
        Normalization::Level0, seed);

    chunker.collect()
}

/// Creates a file manifest and a list of data chunks.
///
/// This function takes the results of the chunking process and generates
/// two key outputs: a `FileManifestParent` struct, which contains all
/// metadata needed to reconstruct the file, and a vector of tuples,
/// where each tuple holds a unique chunk hash and its raw data.
///
/// # Arguments
///
/// * `file_name`: The name of the file being processed.
/// * `file_data`: A byte slice of the complete, original file data.
/// * `chunks`: A slice of `Chunk` structs that describe the file divisions.
///
/// # Returns
///
/// A tuple containing:
/// - A `FileManifestParent` for the processed file.
/// - A `Vec` where each element is a tuple of a chunk's hash and its data.
pub fn create_file_manifest_and_chunks<H: Hashable>(
    file_data: &[u8],
    chunks: &[Chunk],
) -> (FileManifestParent<H>, Vec<(H, Vec<u8>)>) {
    let mut chunk_metadata = Vec::with_capacity(chunks.len());
    let mut chunk_data_list = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        let chunk_data_slice = &file_data[chunk.offset..chunk.offset +
            chunk.length];
        let data_hash = H::from_bytes_with_seed(chunk_data_slice);

        //Add the chunk's data to our list for returning
        chunk_data_list.push((data_hash, chunk_data_slice.to_vec()));

        //Populate the metadata for the manifest
        chunk_metadata.push(SSAChunkMeta {
            hash: data_hash,
            offset: chunk.offset as u64,
            length: chunk.length as u32,
        });
    }

    //Build file_manifest
    let file_manifest = FileManifestParent {
        chunk_count: chunks.len() as u64,
        chunk_metadata,
    };

    (file_manifest, chunk_data_list)
}

/// Verifies the integrity of a single file.
///
/// This function requests each chunk of a file by fetching each of its
/// data chunks in order, feeding each chunk to a sha-512 hasing algorithm,
/// and then computing a final verification hash. To optimize performance,
/// it uses a producer-consumer pattern:
///
/// 1.  **Producer Thread**: A dedicated fetcher thread reads chunk hashes and
///     retrieves their corresponding data in batches using the provided
///     `get_chunk_data` callback.
/// 2.  **Consumer (Main) Thread**: The main thread receives these batches of
///     chunk data and continuously updates a SHA-512 hasher.
///
/// This concurrent approach allows data fetching (I/O-bound) to happen in
/// parallel with data hashing (CPU-bound), significantly speeding up the
/// verification process.
///
/// # Arguments
///
/// * `fmp`: A reference to the `FileManifestParent` for the file.
/// * `veri_hash`: The original SHA-512 hash of the file, used for the final
///   integrity check.
/// * `get_chunk_data`: A callback function that takes a slice of chunk hashes
///   and each corresponding chunks byte data.
/// * `progress_cb`: A callback function for reporting the amount of bytes
///   processed back to the host application.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the file is successfully rebuilt and its computed hash
///   matches the `veri_hash`.
/// - `Err(SpriteShrinkError)` if the computed hash does not match, or if a
///   thread panics during execution.
///
/// # Type Parameters
///
/// * `F`: The type of the `get_chunk_data` callback, which must be a closure
///   that is `Send`, `Sync`, and has a `'static` lifetime.
/// * `H`: The generic hash type, which must be `Eq`, `Hash`, `Display`,
///   `Clone`, `Send`, `Sync`, and have a `'static` lifetime.
pub fn verify_single_file<D, E, H, P>(
    title: String,
    fmp: &FileManifestParent<H>,
    veri_hash: &[u8; 64],
    get_chunk_data: D,
    mut progress_cb: P
) -> Result<(), SpriteShrinkError>
where
    D: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    H: Eq + std::hash::Hash + Clone + Send + Sync + 'static,
    P: FnMut(u64) + Sync + Send + 'static,
{
    /*A batch size of 64 provides a good balance between reducing the overhead
    of the get_chunk_data callback and keeping memory usage low per
    verification task.*/
    const BATCH_SIZE: usize = 64;

    //flume tx and rx channels, limited\bounded to 4 messages.
    let (to_hash_tx, to_hash_rx) = flume::bounded(4);

    /*Puts the list of all hashes for a file in an arc array.*/
    let all_hashes: Arc<[H]> = fmp.chunk_metadata.iter().map(|meta|
        meta.hash.clone()).collect();

    /*Encapsulates the get_chunk_data callback to be safely used by multiple
    threads.*/
    let get_chunk_data_arc = Arc::new(get_chunk_data);

    //Start the fetch handle so it can begin getting the first batch of data.
    let fetch_handle = {
        let hashes_clone = Arc::clone(&all_hashes);
        thread::spawn(move || {
            for batch in hashes_clone.chunks(BATCH_SIZE) {
            match (get_chunk_data_arc)(batch) {
                Ok(chunk_data_batch) => {
                    /*If sending fails, the receiver has hung up, so we can
                    stop.*/
                    if to_hash_tx.send(Ok(chunk_data_batch)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    /*Check if the error from the callback indicates
                    cancellation.*/
                    if e.is_cancelled() {
                        let _ = to_hash_tx.send(
                            Err(SpriteShrinkError::Cancelled)
                        );
                    } else {
                        //Otherwise, treat it as a generic external error.
                        let _ = to_hash_tx.send(Err(
                            SpriteShrinkError::External(Box::new(e))
                        ));
                    }
                    break;
                }
            }
        }
        })
    };

    //Start the SHA-512 hasher
    let mut hasher = Sha512::new();

    /*Until all chunks have been fed to the hasher, while loop will receive
    chunk data and feed it to the hasher.*/
    while let Ok(result) = to_hash_rx.recv() {
        match result {
            //Process the batch of chunks received from the fetcher thread.
            Ok(chunk_batch) => {
                for chunk_data in chunk_batch {
                    //Report progress before updating the hasher.
                    progress_cb(chunk_data.len() as u64);
                    //Update the hash.
                    hasher.update(&chunk_data);
                }
            }
            Err(e) => {
                //If we receive an error, we stop processing and return it.
                return Err(e);
            }
        }
    }

    //Finalize and store sha-512 hash
    let calculated_hash: [u8; 64] = hasher.finalize().into();

    //Clean up threads and propogate errors if needed.
    fetch_handle.join().map_err(|_| ProcessingError::ThreadPanic(
        "Chunk fetching thread panicked.".to_string()))?;

    /*Compare the generated hash with the pregenerated hash and return OK if
    they match.*/
    if calculated_hash.as_slice() == veri_hash {
        Ok(())
    } else {
        Err(ProcessingError::HashMismatchError(title).into())
    }
}


/// Generates a Zstandard compression dictionary from sample data and optimizes
/// the dictionary.
///
/// This function uses the `zstd-sys` crate to train a dictionary that can
/// improve compression ratios for similar data blocks. It can operate
/// in a multi-threaded fashion to speed up the dictionary generation
/// process on large sets of sample data. The COVER algorithm is used for
/// training, which is generally a good default.
///
/// # Arguments
///
/// * `samples_buffer`: A shared slice reference, that stores a continuous
///   array of bytes sampled from the input data.
/// * `max_dict_size`: The maximum desired size for the generated
///   dictionary in bytes. The actual size may be smaller.
/// * `workers`: The number of threads to utilize for dictionary
///   training. A value of 0 will let zstd decide.
/// * `compression_level`: The zstd compression level to target during
///   training. This should match the level used for compression.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the generated dictionary data.
/// - `Err(ProcessingError::DictionaryError)` if the underlying zstd library
///   encounters an error during the dictionary training process.
///
/// # Safety
///
/// This function is safe because it upholds the contracts of the zstd-sys C
/// functions:
/// 1. `dict_buffer` is allocated with sufficient capacity (`max_dict_size`).
/// 2. The pointers and lengths for `samples_buffer` and `sample_sizes` are
///    valid as they are derived directly from Rust-managed Vecs and slices.
/// 3. All potential errors from the C function are checked with
///    `ZDICT_isError`.
pub fn gen_zstd_opt_dict(
    samples_buffer: &[u8],
    sample_sizes: &[usize],
    max_dict_size: usize,
    workers: usize,
    compression_level: i32
) -> Result<Vec<u8>, ProcessingError> {
    /*Prepares the samples for the zstd_sys function.*/

    /*This buffer will store the dictionary to be returned.*/
    let mut dict_buffer = Vec::with_capacity(max_dict_size);

    /*Set and store the ZDICT parameters to be used by the cover algorithm.
    Any values marked as default are either not used by
    ZDICT_optimizeTrainFromBuffer_cover or is the default value that is
    used for the vast majority of applications.*/
    let z_params = ZDICT_params_t {
        compressionLevel: compression_level,
        notificationLevel: 0, //Default
        dictID: 0 //Default
    };
    /*params is declared mutable as the function tunes values as it
    processes.*/
    let mut params = zstd_sys::ZDICT_cover_params_t {
        k:          0, //Dynamically set by below function.
        d:          0, //Dynamically set by below function.
        steps:      4,
        nbThreads:  workers as u32,
        splitPoint: 0.0, //Default
        shrinkDict: 0, //Default, not used
        shrinkDictMaxRegression: 0, //Default, not used
        zParams: z_params
    };

    //Run the C function with the defined paramters and prepared vectors.
    let dict_size = unsafe {
        zstd_sys::ZDICT_optimizeTrainFromBuffer_cover(
            dict_buffer.as_mut_ptr() as *mut c_void,
            max_dict_size,
            samples_buffer.as_ptr() as *const c_void,
            sample_sizes.as_ptr(),
            sample_sizes.len() as u32,
            &mut params
        )
    };

    //Verify if any errors occured in the unsafe function.
    if unsafe { zstd_sys::ZDICT_isError(dict_size) } != 0 {
        let e = unsafe {
            std::ffi::CStr::from_ptr(zstd_sys::ZDICT_getErrorName(
                dict_size)).to_string_lossy()
        };
        return Err(ProcessingError::DictionaryError(e.to_string()))
    }

    /*Set final length of buffer so that Rust knows how much data is within
    it as the unsafe C function from zstd has written to it.*/
    unsafe {
        dict_buffer.set_len(dict_size);
    }

    /*Return the dictionary buffer.*/
    Ok(dict_buffer)
}

/// Prepares a vector of data samples for Zstandard dictionary training.
///
/// The zstd compression algorithm can achieve better compression ratios if
/// it's "trained" on a dictionary built from data similar to what it will
/// compress. This function selects an optimal set of data chunks to be used
/// as this training data.
///
/// To ensure the dictionary training process is does not cause an overflow,
/// zstd requires a sample data size of no more than 4 GB. This function
/// implements two strategies:
///
/// 1.  If the total size of all unique chunks (`total_data_size`) is within
///     the 4 GB limit, all chunks are used.
/// 2.  If the total size exceeds the limit, a representative subset of chunks
///     is selected by stepping through the sorted list of hashes. This ensures
///     the sample includes data from across the entire dataset while
///     respecting the size limit.
///
/// # Type Parameters
///
/// * `F`: A closure that takes a chunk hash and returns its corresponding byte
///   data.
/// * `H`: The hash type, which must be copyable and usable as a hash key.
///
/// # Arguments
///
/// * `sorted_hashes`: A sorted slice of all unique chunk hashes to process.
/// * `total_data_size`: The total combined size in bytes of all unique chunks.
/// * `get_chunk_data`: A callback function for retrieving a chunk's data by
///   its hash.
///
/// # Returns
///
/// A `Result` containing a `Vec<Vec<u8>>`, where each inner vector is the byte
/// data of a selected sample chunk.
pub fn build_train_samples<E, F, H>(
    sorted_hashes: &[H],
    total_data_size: u64,
    target_dictionary_size: usize,
    get_chunk_data: &F,
) -> Result<(Vec<u8>, Vec<usize>), ProcessingError>
where
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    F: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + ?Sized,
    H: Copy + Eq + Hash + Send + Sync + 'static,
{
    /*Max samples is 4.0 gigabytes. This is delibrately set to prevent an
    overflow in the zstd library.*/
    const MAX_SAMPLES_SIZE: usize = 4 * 1024 * 1024 * 1024;

    //Get and store the number of chunks in sorted hashes.
    let number_of_chunks = sorted_hashes.len();

    /*If the total data size is less than MAX_SAMPLES_SIZE, use all data to
    generate the dictionary and return it.*/
    if total_data_size as usize <= MAX_SAMPLES_SIZE {
        //Reserve capacity to avoid reallocations.
        let mut samples_buffer: Vec<u8> = Vec::with_capacity(
            target_dictionary_size
        );
        let mut sample_sizes: Vec<usize> = Vec::with_capacity(
            number_of_chunks
        );
        for hash in sorted_hashes {
            let sample = &get_chunk_data(&[*hash])
            .map_err(|e| {
                if e.is_cancelled() {
                    ProcessingError::Cancelled
                } else {
                    ProcessingError::External(e.to_string())
                }
            })?
            .remove(0);

            sample_sizes.push(sample.len());
            samples_buffer.extend_from_slice(sample);
        }

        let mut total_sizes: usize = 0;

        sample_sizes.iter().for_each(|size|{
            total_sizes += size ;
        });

        return Ok((samples_buffer, sample_sizes));
    }

    //If the data size exceeds the limit, sample the chunks.

    //Reserve capacity to avoid reallocations.
    let mut samples_buffer: Vec<u8> = Vec::with_capacity(
        target_dictionary_size
    );
    let mut sample_sizes: Vec<usize> = Vec::with_capacity(
        number_of_chunks
    );

    //Variable for tracking sum of the samples.
    let mut current_samples_size: usize = 0;

    /*Calculate a step value to sample evenly across all chunks.
    This ensures we select chunks from the beginning, middle, and
    end.*/
    let step = (total_data_size as f64 / MAX_SAMPLES_SIZE as f64)
        .ceil() as u64;
    let step = step.max(1) as usize;


    for i in (0..number_of_chunks).step_by(step) {
        if let Some(hash) = sorted_hashes.get(i) {
            //Get a sample using the callback.
            let sample = &get_chunk_data(&[*hash])
            .map_err(|e| {
                if e.is_cancelled() {
                    ProcessingError::Cancelled
                } else {
                    ProcessingError::External(e.to_string())
                }
            })?
            .remove(0);
            let sample_size = sample.len();

            //Check if adding the next sample would exceed the limit.
            if current_samples_size + (sample_size) >
                MAX_SAMPLES_SIZE {
                    break; //Stop if the size limit is exceeded.
                }

            //Increment total sample size.
            current_samples_size += sample_size;

            //Add chunk to sample vectors.
            sample_sizes.push(sample_size);
            samples_buffer.extend_from_slice(sample);
        }
    }

    Ok((samples_buffer, sample_sizes))
}

/// Estimates the total compressed size of the data store.
///
/// This function simulates a full compression cycle to provide a size
/// estimate for a given set of parameters. It first builds a temporary
/// Zstandard dictionary from the provided data chunks. It then compresses
/// all chunks in parallel using this dictionary.
///
/// The final returned size is the sum of the dictionary's size and the
/// size of all compressed chunks. This is used during the auto-tuning
/// process to evaluate different parameters without creating a full archive.
///
/// # Arguments
///
/// * `sorted_hashes`: A sorted slice of all unique chunk hashes to process.
/// * `total_data_size`: The total combined size in bytes of all unique chunks.
/// * `worker_count`: The number of threads to use for parallel compression.
/// * `dictionary_size`: The target size for the temporary dictionary.
/// * `compression_level`: The Zstandard compression level to be used by the
///   worker threads. When testing a maximum value of 7 is used.
/// * `get_chunk_data`: A callback function for retrieving a chunk's data by
///   its hash.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(usize)` containing the total estimated compressed size in bytes.
/// - `Err(SpriteShrinkError)` if dictionary creation or compression fails.
pub fn test_compression<E, F, H>(
    sorted_hashes: &[H],
    total_data_size: u64,
    worker_count: usize,
    dictionary_size: usize,
    compression_level: i32,
    get_chunk_data: F,
) -> Result<usize, SpriteShrinkError>
where
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    F: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    H: Copy + Debug + Eq + Encode + Hash + Send + Sync + 'static,
{
    //Prepare the samples for dictionary generation.
    let (samples_for_dict, sample_sizes) = build_train_samples(
        sorted_hashes,
        total_data_size,
        dictionary_size,
        &get_chunk_data
    )?;

    //Build dictionary
    let dictionary = zstd::dict::from_continuous(
        &samples_for_dict,
        &sample_sizes,
        dictionary_size, //Max dictionary size in bytes
        ).map_err(|e| ProcessingError::DictionaryError(e.to_string()))?;

    //Callback wrapper for getting chunk data from host application.
    let get_chunk_data_wrapper = move |hashes: &[H]| ->
        Result<Vec<(H, Vec<u8>)>, E> {
            let ret_chunks = get_chunk_data(hashes)?;
            let mut chunk_pairs: Vec<(H, Vec<u8>)> =
                Vec::with_capacity(ret_chunks.len());

            for (index, chunk) in ret_chunks.iter().enumerate(){
                chunk_pairs.push((hashes[index], chunk.clone()));
            };

            Ok(chunk_pairs)
    };

    //Arc(Mutex) for storing the total data size.
    let final_size = Arc::new(Mutex::new(0usize));

    /*Callback that doesn't store but adds the size of the chunk to
    final_size.*/
    let write_chunk_data_cb = {
        let final_size_clone = Arc::clone(&final_size);
        move |chunk: &[u8]| -> Result<(), E> {
            let mut size = final_size_clone.lock().unwrap();
            *size += chunk.len();
            Ok(())
        }
    };

    //Run compression for data for test data.
    let _chunk_index = compress_chunks(
        worker_count,
        &dictionary,
        sorted_hashes,
        compression_level.max(7),
        get_chunk_data_wrapper,
        write_chunk_data_cb
    )?;

    //Pull the finalize size from the Arc(Mutex)
    let final_size_val = *final_size.lock().unwrap();

    Ok((dictionary.len() as usize) + final_size_val)
}

/// Calculates which chunks and what parts of them are needed to satisfy a
/// seek request.
///
/// # Arguments
/// * `manifest`: The `FileManifestParent` for the specific file being read.
/// * `seek_offset`: The starting byte offset within the original,
///   uncompressed file.
/// * `seek_length`: The number of bytes to read from the `seek_offset`.
///
/// # Returns
/// A `Result` containing a `Vec` where each entry is a SeekMetadata which
/// contains the following:
/// - The `hash` of a required chunk.
/// - The `start` and `end` byte range to copy from that chunk *after* it has
///   been decompressed.
///
/// A `seek_length` of 0 returns an empty `Vec`.
///
/// # Errors
///
/// Returns `ProcessingError::SeekOutOfBounds` if the requested range ends
/// past the end of the file, including when `seek_offset + seek_length`
/// overflows a `u64`.
pub fn get_seek_chunks<H: Copy + Eq + Hash>(
    manifest: &FileManifestParent<H>,
    seek_offset: u64,
    seek_length: u64,
) -> Result<Vec<SeekMetadata<H>>, SpriteShrinkError> {
    //Calculate original file size.
    let original_file_size: u64 = manifest.chunk_metadata
        .iter()
        .map(|c| c.length as u64)
        .sum();

    /*Using the calculated file size, if the seek request is beyond the file
    size fail early. A request whose end doesn't fit in a u64 is beyond it
    too.*/
    let seek_end = seek_offset
        .checked_add(seek_length)
        .filter(|&end| end <= original_file_size)
        .ok_or_else(|| ProcessingError::SeekOutOfBounds(format!(
            "{seek_length} bytes at offset {seek_offset} in a \
             {original_file_size}-byte file"
        )))?;

    /*A zero-length read needs no chunks. Without this, a request inside a
    chunk would return that chunk with an empty range.*/
    if seek_length == 0 {
        return Ok(Vec::new());
    }

    //Prepare the return vector
    let mut required_chunks: Vec<SeekMetadata<H>> = Vec::new();

    for chunk_meta in &manifest.chunk_metadata {
        //Calculate start offset of the chunk within the virtual file.
        let chunk_start_offset = chunk_meta.offset;
        //Calculate end offset of the chunk within the virtual file.
        let chunk_end_offset = chunk_meta.offset + chunk_meta.length as u64;

        //Check if a chunk is found within the desired range.
        if (chunk_start_offset < seek_end) && (chunk_end_offset > seek_offset){
            /*Calculate and record the start offset within the chunk of the
            desired data.
            If the seek offset is before the chunk offset it will be 0.*/
            let read_start_in_chunk = seek_offset
                .saturating_sub(chunk_start_offset);

            /*Calculate and record the end offset within the chunk of the
            desired data.
            If the seek offset is after the end of the chunk, it will be the
            data length of the chunk.*/
            let read_end_in_chunk = (seek_end - chunk_start_offset)
                .min(chunk_meta.length
                    .into()
                );

            /*Add the gathered data to the return vector. */
            required_chunks.push(SeekMetadata::new(
                chunk_meta.hash,
                read_start_in_chunk,
                read_end_in_chunk
            ))
        }
        /*If a chunk is found beyond the end of the seek request, end the
        loop.*/
        else if chunk_start_offset >= seek_end {
            break;
        }
    }

    Ok(required_chunks)
}


#[cfg(test)]
mod tests {
    use std::{fmt::Write, io::Cursor};

    use fastcdc::v2020::StreamCDC;
    use proptest::{prelude::*, sample::Index};
    use sprite_shrink_test_support::tiled_bytes;

    use super::*;

    /// Pseudo-random bytes from a fixed-seed xorshift generator.
    fn test_bytes(len: usize) -> Vec<u8> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_be_bytes()[0]
            })
            .collect()
    }

    // The CLI streams files through StreamCDC with these settings
    // (`default.rs` and `optical.rs`). If they drift apart, the same file is
    // cut differently depending on which path compressed it.
    #[test]
    fn chunk_data_matches_cli_stream_chunker() {
        let window_size: u64 = 4096;
        let file_data = FileData {
            file_name: "test.bin".to_string(),
            file_data: test_bytes(1 << 20),
        };

        let sizes = chunker_sizes(window_size).unwrap();
        let in_memory: Vec<(u64, usize)> =
            chunk_data(&file_data, SS_SEED, sizes)
                .iter()
                .map(|c| (u64::try_from(c.offset).unwrap(), c.length))
                .collect();

        let streamed: Vec<(u64, usize)> = StreamCDC::with_level_and_seed(
            Cursor::new(&file_data.file_data),
            u32::try_from(window_size / 4).unwrap(),
            u32::try_from(window_size).unwrap(),
            u32::try_from(window_size * 4).unwrap(),
            Normalization::Level0,
            SS_SEED,
        )
        .map(|c| {
            let c = c.unwrap();
            (c.offset, c.length)
        })
        .collect();

        assert!(in_memory.len() > 100, "too few chunks to compare");
        assert_eq!(in_memory, streamed);
    }

    fn file(file_data: Vec<u8>) -> FileData {
        FileData { file_name: "test.bin".to_string(), file_data }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").unwrap();
            hex
        })
    }

    #[test]
    fn window_size_must_be_within_fastcdc_limits() {
        for window_size in [MIN_WINDOW_SIZE, 2048, MAX_WINDOW_SIZE] {
            let processed = process_file_in_memory(
                file(test_bytes(4096)),
                window_size,
            );
            assert!(
                matches!(processed, Ok(Some(_))),
                "window size {window_size}: {:?}",
                processed.err()
            );
        }

        // 2^34 + 2048 used to be truncated to a 2048-byte window.
        let rejected = [
            0,
            MIN_WINDOW_SIZE - 1,
            MAX_WINDOW_SIZE + 1,
            u64::from(u32::MAX) + 1,
            (1 << 34) + 2048,
            u64::MAX,
        ];
        for window_size in rejected {
            for len in [4096, 0] {
                let processed =
                    process_file_in_memory(file(test_bytes(len)), window_size);
                assert!(
                    matches!(
                        processed,
                        Err(SpriteShrinkError::Processing(
                            ProcessingError::InvalidWindowSize(size)
                        )) if size == window_size
                    ),
                    "window size {window_size}, {len} bytes: {:?}",
                    processed.map(|data| data.is_some())
                );
            }
        }
    }

    #[test]
    fn invalid_window_size_message_names_the_value_and_range() {
        assert_eq!(
            ProcessingError::InvalidWindowSize(100).to_string(),
            "Window size 100 is outside the supported range of 256 to \
             4194304 bytes"
        );
    }

    #[test]
    fn empty_file_is_skipped() {
        let processed = process_file_in_memory(file(Vec::new()), 2048);
        assert!(
            matches!(processed, Ok(None)),
            "{:?}",
            processed.map(|data| data.is_some())
        );
    }

    #[test]
    fn verification_hash_is_sha_512() {
        // The FIPS 180-2 test vectors.
        assert_eq!(
            hex(&generate_sha_512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );

        let processed =
            process_file_in_memory(file(b"abc".to_vec()), 2048).unwrap();
        assert_eq!(
            hex(&processed.unwrap().veri_hash),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    // Archives store these hashes, and new data only deduplicates against
    // an existing archive if it hashes and chunks the same way. A failure
    // here after a dependency upgrade means xxhash or FastCDC changed their
    // output.
    #[test]
    fn chunk_hashes_are_pinned() {
        assert_eq!(u64::from_bytes_with_seed(b""), 0xa63f_e93c_61f7_8d72);
        assert_eq!(u64::from_bytes_with_seed(b"abc"), 0x8e4f_aaf4_2353_5806);
        assert_eq!(
            u128::from_bytes_with_seed(b""),
            0xd8d4_5ad7_f57e_c110_9b90_b1dd_4d15_9b1c
        );
        assert_eq!(
            u128::from_bytes_with_seed(b"abc"),
            0xef42_d376_4298_be2f_8e4f_aaf4_2353_5806
        );
    }

    #[test]
    fn chunk_boundaries_are_pinned() {
        let processed =
            process_file_in_memory(file(test_bytes(16 * 1024)), 1024)
                .unwrap()
                .unwrap();
        let lengths: Vec<usize> =
            processed.chunks.iter().map(|chunk| chunk.length).collect();

        assert_eq!(
            lengths,
            [
                464, 725, 1078, 886, 654, 2545, 1000, 1182, 1058, 2618, 1254,
                333, 675, 318, 1594
            ]
        );
    }

    /// Checks that `processed`'s chunks tile the file in order, within the
    /// window's size bounds, and that the manifest built from them hashes
    /// and locates each chunk's bytes.
    fn assert_chunks_tile_the_file<H: Hashable>(
        processed: &ProcessedFileData,
        window_size: u64,
    ) -> Result<(), TestCaseError> {
        let data = &processed.file_data;
        let (min_size, _, max_size) = chunker_sizes(window_size)
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        // FastCDC tests two bytes per step from `min_size / 2 * 2`, so an
        // odd minimum can give a chunk one byte shorter.
        let min_size = min_size as usize / 2 * 2;
        let max_size = max_size as usize;

        let mut offset = 0;
        for (index, chunk) in processed.chunks.iter().enumerate() {
            prop_assert_eq!(chunk.offset, offset);
            prop_assert!(chunk.length <= max_size, "{chunk:?}");
            // Only the last chunk may end short, at the end of the file.
            if index + 1 < processed.chunks.len() {
                prop_assert!(chunk.length >= min_size, "{chunk:?}");
            }
            offset += chunk.length;
        }
        prop_assert_eq!(offset, data.len());

        let (manifest, chunk_list) =
            create_file_manifest_and_chunks::<H>(data, &processed.chunks);
        prop_assert_eq!(manifest.chunk_count, processed.chunks.len() as u64);
        prop_assert_eq!(manifest.chunk_metadata.len(), processed.chunks.len());
        prop_assert_eq!(chunk_list.len(), processed.chunks.len());

        let entries = processed
            .chunks
            .iter()
            .zip(&manifest.chunk_metadata)
            .zip(&chunk_list);
        for ((chunk, meta), (hash, bytes)) in entries {
            let expected = &data[chunk.offset..chunk.offset + chunk.length];
            prop_assert_eq!(meta.offset, chunk.offset as u64);
            prop_assert_eq!(meta.length as usize, chunk.length);
            prop_assert_eq!(meta.hash, H::from_bytes_with_seed(expected));
            prop_assert_eq!(*hash, meta.hash);
            prop_assert_eq!(bytes.as_slice(), expected);
        }
        Ok(())
    }

    /// A manifest of contiguous chunks with the given lengths, each hashed
    /// as its index.
    fn manifest_of(lengths: &[u32]) -> FileManifestParent<u64> {
        let mut offset = 0;
        let chunk_metadata: Vec<SSAChunkMeta<u64>> = lengths
            .iter()
            .zip(0..)
            .map(|(&length, hash)| {
                let meta = SSAChunkMeta { hash, offset, length };
                offset += u64::from(length);
                meta
            })
            .collect();

        FileManifestParent {
            chunk_count: chunk_metadata.len() as u64,
            chunk_metadata,
        }
    }

    fn is_out_of_bounds<T>(result: &Result<T, SpriteShrinkError>) -> bool {
        matches!(
            result,
            Err(SpriteShrinkError::Processing(
                ProcessingError::SeekOutOfBounds(_)
            ))
        )
    }

    #[test]
    fn seek_may_end_at_the_end_of_the_file_but_not_past_it() {
        let manifest = manifest_of(&[10, 20, 30]);

        assert_eq!(get_seek_chunks(&manifest, 0, 60).unwrap().len(), 3);
        assert_eq!(
            get_seek_chunks(&manifest, 50, 10).unwrap(),
            [SeekMetadata::new(2, 20, 30)]
        );
        assert!(is_out_of_bounds(&get_seek_chunks(&manifest, 0, 61)));
        assert!(is_out_of_bounds(&get_seek_chunks(&manifest, 60, 1)));
        assert!(is_out_of_bounds(&get_seek_chunks(&manifest, 61, 0)));
    }

    #[test]
    fn seek_whose_end_overflows_is_out_of_bounds() {
        let manifest = manifest_of(&[10, 20, 30]);

        let overflowing = [(u64::MAX, 1), (1, u64::MAX), (5, u64::MAX - 2)];
        for (offset, length) in overflowing {
            assert!(
                is_out_of_bounds(&get_seek_chunks(&manifest, offset, length)),
                "{length} bytes at {offset}"
            );
        }
    }

    #[test]
    fn zero_length_seek_needs_no_chunks() {
        let manifest = manifest_of(&[10, 20, 30]);

        // Inside a chunk, on a boundary, and at the end of the file.
        for offset in [0, 5, 10, 59, 60] {
            assert_eq!(
                get_seek_chunks(&manifest, offset, 0).unwrap(),
                [],
                "offset {offset}"
            );
        }
        assert_eq!(get_seek_chunks(&manifest_of(&[]), 0, 0).unwrap(), []);
    }

    proptest! {
        #[test]
        fn chunks_tile_the_file_and_hash_their_bytes(
            seed in 1..=u64::MAX,
            len in 1..=64 * 1024_usize,
            window_size in MIN_WINDOW_SIZE..=8 * 1024,
        ) {
            let processed = process_file_in_memory(
                file(tiled_bytes(seed, len)),
                window_size,
            )
            .unwrap()
            .unwrap();

            assert_chunks_tile_the_file::<u64>(&processed, window_size)?;
            assert_chunks_tile_the_file::<u128>(&processed, window_size)?;
        }

        /// Joining the ranges `get_seek_chunks` returns reads exactly the
        /// requested bytes, and every range it returns is needed.
        #[test]
        fn seek_chunks_cover_exactly_the_requested_range(
            lengths in proptest::collection::vec(1..=64_u32, 0..16),
            start in any::<Index>(),
            len in any::<Index>(),
        ) {
            let manifest = manifest_of(&lengths);
            let data = test_bytes(lengths.iter().map(|&l| l as usize).sum());
            let offset = start.index(data.len() + 1);
            let length = len.index(data.len() - offset + 1);

            let seeks = get_seek_chunks(
                &manifest,
                offset as u64,
                length as u64,
            )
            .unwrap();

            let at = |n: u64| usize::try_from(n).unwrap();
            let mut read = Vec::new();
            for seek in &seeks {
                prop_assert!(seek.start_offset < seek.end_offset, "{seek:?}");
                let chunk_start =
                    at(manifest.chunk_metadata[at(seek.hash)].offset);
                read.extend_from_slice(
                    &data[chunk_start + at(seek.start_offset)
                        ..chunk_start + at(seek.end_offset)],
                );
            }
            prop_assert_eq!(read, &data[offset..offset + length]);
        }
    }
}
