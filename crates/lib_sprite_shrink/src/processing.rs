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
use fastcdc::v2020::{Chunk, FastCDC, Normalization};
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

    #[error("Verification failed for file:  {0}")]
    HashMismatchError(String),

    #[error("An internal logic error occurred: {0}")]
    InternalError(String),

    #[error("Dictionary creation failed: {0}")]
    DictionaryError(String),

    #[error("Seek request is outside the file bounds: {0}")]
    SeekOutOfBounds(String),

    #[error("File Verification failed: {0}")]
    VerificationError(String),

    #[error("A thread panic occurred: {0}")]
    ThreadPanic(String),

    #[error("An error occurred in an external callback: {0}")]
    External(String),

    #[error("Operation cancelled by user.")]
    Cancelled,
}

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
/// * `window_size`:
///
/// # Returns
///
/// A `Result` which is:
/// - `Some(ProcessedFileData)` on successful processing.
/// - `None` if the input file data is empty.
pub fn process_file_in_memory(
    file_data: FileData,
    window_size: u64,
) -> Option<ProcessedFileData> {
    if file_data.file_data.is_empty() {
        return None;
    }

    let veri_hash = generate_sha_512(&file_data.file_data);
    let chunks = chunk_data(&file_data, SS_SEED, window_size);

    Some(ProcessedFileData {
        file_name: file_data.file_name,
        veri_hash,
        chunks,
        file_data: file_data.file_data,
    })
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
///
/// # Returns
///
/// A `Result` which is:
/// - `Vec<Chunk>` containing all the identified data chunks.
fn chunk_data(
    file_data: &FileData,
    seed: u64,
    window_size: u64
) -> Vec<Chunk> {
    let chunker = FastCDC::with_level_and_seed(
        &file_data.file_data,
        (window_size/4) as u32,
        window_size as u32,
        (window_size*4) as u32,
        Normalization::Level1, seed);

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
    size fail early.*/
    if seek_offset + seek_length > original_file_size {
        return Err(ProcessingError::SeekOutOfBounds(
            "Seek request is outside the bounds of the original file."
                .to_string(),
        ).into());
    }

    //Prepare the return vector
    let mut required_chunks: Vec<SeekMetadata<H>> = Vec::new();

    //Calculate the seek end for checking if each chunk is within that range.
    let seek_end = seek_offset + seek_length;

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
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug)]
    struct TestError { cancelled: bool }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "test error")
        }
    }

    impl std::error::Error for TestError {}

    impl IsCancelled for TestError {
        fn is_cancelled(&self) -> bool { self.cancelled }
    }

    fn make_manifest(entries: &[(u64, u32, u64)]) -> FileManifestParent<u64> {
        let chunk_metadata = entries.iter()
            .map(|&(offset, length, hash)| SSAChunkMeta { hash, offset, length })
            .collect::<Vec<_>>();
        FileManifestParent {
            chunk_count: chunk_metadata.len() as u64,
            chunk_metadata,
        }
    }

    // --- generate_sha_512 ---

    #[test]
    fn test_generate_sha_512_succeeds() {
        let data = b"Hello, world!".to_vec();
        let sha_512 = generate_sha_512(&data);
        let expected: [u8; 64] = [
            193, 82, 124, 216, 147, 193, 36, 119, 61, 129, 25, 17, 151, 12, 143, 230,
            232, 87, 214, 223, 93, 201, 34, 107, 216, 161, 96, 97, 76, 12, 217, 99,
            164, 221, 234, 43, 148, 187, 125, 54, 2, 30, 249, 216, 101, 213, 206, 162,
            148, 168, 45, 212, 154, 11, 178, 105, 245, 31, 110, 122, 87, 247, 148, 33,
        ];
        assert_eq!(sha_512, expected);
    }

    #[test]
    fn test_generate_sha_512_empty_input_has_known_hash() {
        let sha_512 = generate_sha_512(b"");
        assert_eq!(sha_512.len(), 64);
        // SHA-512 of empty input is a well-known constant; just verify it is not all zeros.
        assert_ne!(sha_512, [0u8; 64]);
    }

    #[test]
    fn test_generate_sha_512_different_inputs_give_different_hashes() {
        assert_ne!(generate_sha_512(b"hello"), generate_sha_512(b"world"));
    }

    // --- process_file_in_memory ---

    #[test]
    fn test_process_file_in_memory_succeeds() {
        let file_data = FileData {
            file_name: String::from("test.txt"),
            file_data: b"Hello, world!".to_vec()
        };

        let processed_file_data = process_file_in_memory(file_data, 1024);
        assert!(processed_file_data.is_some());

        let processed_file_data = processed_file_data.unwrap();
        assert_eq!(processed_file_data.file_name, "test.txt");
        assert_eq!(processed_file_data.file_data, b"Hello, world!".to_vec());
    }

    #[test]
    fn test_process_file_in_memory_returns_none_for_empty_file() {
        let file_data = FileData {
            file_name: String::from("test.txt"),
            file_data: Vec::new()
        };

        let processed_file_data = process_file_in_memory(file_data, 1024);
        assert!(processed_file_data.is_none(), "Expected None for empty file data");
    }

    #[test]
    fn test_process_file_in_memory_veri_hash_matches_data() {
        let data = b"some file content".to_vec();
        let file_data = FileData { file_name: "f.bin".into(), file_data: data.clone() };
        let processed = process_file_in_memory(file_data, 1024).unwrap();
        assert_eq!(processed.veri_hash, generate_sha_512(&data));
    }

    #[test]
    fn test_process_file_in_memory_chunks_cover_full_data() {
        let data: Vec<u8> = (0..2000u16).map(|i| i as u8).collect();
        let file_data = FileData { file_name: "f.bin".into(), file_data: data.clone() };
        let processed = process_file_in_memory(file_data, 512).unwrap();

        let total_chunk_bytes: usize = processed.chunks.iter().map(|c| c.length).sum();
        assert_eq!(total_chunk_bytes, data.len());
    }

    // --- Hashable ---

    #[test]
    fn test_hashable_u64_is_deterministic() {
        let data = b"determinism check";
        assert_eq!(
            u64::from_bytes_with_seed(data),
            u64::from_bytes_with_seed(data)
        );
    }

    #[test]
    fn test_hashable_u128_is_deterministic() {
        let data = b"determinism check";
        assert_eq!(
            u128::from_bytes_with_seed(data),
            u128::from_bytes_with_seed(data)
        );
    }

    #[test]
    fn test_hashable_different_inputs_give_different_hashes() {
        assert_ne!(
            u64::from_bytes_with_seed(b"alpha"),
            u64::from_bytes_with_seed(b"beta")
        );
        assert_ne!(
            u128::from_bytes_with_seed(b"alpha"),
            u128::from_bytes_with_seed(b"beta")
        );
    }

    // --- create_file_manifest_and_chunks ---

    #[test]
    fn test_create_file_manifest_chunk_count_matches() {
        let data = b"Hello, world!".to_vec();
        let fd = FileData { file_name: "t.txt".into(), file_data: data.clone() };
        let chunks = chunk_data(&fd, SS_SEED, 1024);

        let (manifest, chunk_list) = create_file_manifest_and_chunks::<u64>(&data, &chunks);

        assert_eq!(manifest.chunk_count, chunks.len() as u64);
        assert_eq!(manifest.chunk_metadata.len(), chunks.len());
        assert_eq!(chunk_list.len(), chunks.len());
    }

    #[test]
    fn test_create_file_manifest_chunks_reconstruct_original_data() {
        // window_size=512 → min=128, which satisfies FastCDC's MINIMUM_MIN=64
        let data: Vec<u8> = (0u8..200).collect();
        let fd = FileData { file_name: "t.bin".into(), file_data: data.clone() };
        let chunks = chunk_data(&fd, SS_SEED, 512);

        let (_manifest, chunk_list) = create_file_manifest_and_chunks::<u64>(&data, &chunks);

        let reassembled: Vec<u8> = chunk_list.into_iter().flat_map(|(_, d)| d).collect();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_create_file_manifest_hashes_are_consistent_across_calls() {
        let data = b"repeatable hashing".to_vec();
        let fd = FileData { file_name: "t.txt".into(), file_data: data.clone() };
        let chunks = chunk_data(&fd, SS_SEED, 1024);

        let (m1, _) = create_file_manifest_and_chunks::<u64>(&data, &chunks);
        let (m2, _) = create_file_manifest_and_chunks::<u64>(&data, &chunks);

        for (a, b) in m1.chunk_metadata.iter().zip(m2.chunk_metadata.iter()) {
            assert_eq!(a.hash, b.hash);
        }
    }

    #[test]
    fn test_create_file_manifest_u128_hashes() {
        let data = b"u128 hash test".to_vec();
        let fd = FileData { file_name: "t.txt".into(), file_data: data.clone() };
        let chunks = chunk_data(&fd, SS_SEED, 1024);

        let (manifest, chunk_list) = create_file_manifest_and_chunks::<u128>(&data, &chunks);

        assert_eq!(manifest.chunk_count, chunks.len() as u64);
        assert_eq!(chunk_list.len(), chunks.len());
    }

    // --- verify_single_file ---

    fn chunk_map_from(data: &[u8], window: u64) -> (FileManifestParent<u64>, [u8; 64], HashMap<u64, Vec<u8>>) {
        let fd = FileData { file_name: "t.bin".into(), file_data: data.to_vec() };
        let processed = process_file_in_memory(fd, window).unwrap();
        let (manifest, chunk_list) = create_file_manifest_and_chunks::<u64>(
            &processed.file_data, &processed.chunks
        );
        let map: HashMap<u64, Vec<u8>> = chunk_list.into_iter().collect();
        (manifest, processed.veri_hash, map)
    }

    #[test]
    fn test_verify_single_file_succeeds() {
        let (manifest, veri_hash, map) = chunk_map_from(b"Hello, world!", 1024);

        let result = verify_single_file(
            "t.bin".into(),
            &manifest,
            &veri_hash,
            move |hashes: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Ok(hashes.iter().map(|h| map[h].clone()).collect())
            },
            |_| {},
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_single_file_detects_hash_mismatch() {
        let (manifest, _, map) = chunk_map_from(b"Hello, world!", 1024);
        let wrong_hash = [0u8; 64];

        let result = verify_single_file(
            "t.bin".into(),
            &manifest,
            &wrong_hash,
            move |hashes: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Ok(hashes.iter().map(|h| map[h].clone()).collect())
            },
            |_| {},
        );

        assert!(matches!(result, Err(SpriteShrinkError::Processing(_))));
    }

    #[test]
    fn test_verify_single_file_propagates_callback_error() {
        let (manifest, veri_hash, _) = chunk_map_from(b"Hello, world!", 1024);

        let result = verify_single_file(
            "t.bin".into(),
            &manifest,
            &veri_hash,
            |_: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Err(TestError { cancelled: false })
            },
            |_| {},
        );

        assert!(matches!(result, Err(SpriteShrinkError::External(_))));
    }

    #[test]
    fn test_verify_single_file_propagates_cancellation() {
        let (manifest, veri_hash, _) = chunk_map_from(b"Hello, world!", 1024);

        let result = verify_single_file(
            "t.bin".into(),
            &manifest,
            &veri_hash,
            |_: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Err(TestError { cancelled: true })
            },
            |_| {},
        );

        assert!(matches!(result, Err(SpriteShrinkError::Cancelled)));
    }

    // --- build_train_samples ---

    #[test]
    fn test_build_train_samples_includes_all_chunks_under_limit() {
        let chunks: Vec<Vec<u8>> = vec![vec![1u8; 50], vec![2u8; 75], vec![3u8; 60]];
        let hashes: Vec<u64> = (0..chunks.len() as u64).collect();
        let total: u64 = chunks.iter().map(|c| c.len() as u64).sum();
        let chunks_clone = chunks.clone();

        let (_buffer, sizes) = build_train_samples(
            &hashes,
            total,
            1024,
            &|h: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Ok(h.iter().map(|&i| chunks_clone[i as usize].clone()).collect())
            },
        ).unwrap();

        assert_eq!(sizes.len(), chunks.len());
    }

    #[test]
    fn test_build_train_samples_sample_sizes_match_buffer_length() {
        let chunks: Vec<Vec<u8>> = vec![vec![0u8; 40], vec![1u8; 60]];
        let hashes: Vec<u64> = (0..chunks.len() as u64).collect();
        let total: u64 = chunks.iter().map(|c| c.len() as u64).sum();
        let chunks_clone = chunks.clone();

        let (buffer, sizes) = build_train_samples(
            &hashes,
            total,
            1024,
            &|h: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Ok(h.iter().map(|&i| chunks_clone[i as usize].clone()).collect())
            },
        ).unwrap();

        let total_from_sizes: usize = sizes.iter().sum();
        assert_eq!(buffer.len(), total_from_sizes);
    }

    #[test]
    fn test_build_train_samples_propagates_callback_error() {
        let hashes: Vec<u64> = vec![0];

        let result = build_train_samples(
            &hashes,
            100,
            1024,
            &|_: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Err(TestError { cancelled: false })
            },
        );

        assert!(matches!(result, Err(ProcessingError::External(_))));
    }

    #[test]
    fn test_build_train_samples_propagates_cancellation() {
        let hashes: Vec<u64> = vec![0];

        let result = build_train_samples(
            &hashes,
            100,
            1024,
            &|_: &[u64]| -> Result<Vec<Vec<u8>>, TestError> {
                Err(TestError { cancelled: true })
            },
        );

        assert!(matches!(result, Err(ProcessingError::Cancelled)));
    }

    // --- gen_zstd_opt_dict ---

    #[test]
    fn test_gen_zstd_opt_dict_produces_nonempty_dictionary() {
        // Feed 50 samples of 200 bytes each so the COVER algorithm has enough data.
        let base: Vec<u8> = (0u8..=255).cycle().take(10_000).collect();
        let sample_size = 200usize;
        let samples: Vec<Vec<u8>> = (0..50)
            .map(|i| base[i * 13 % 9800..][..sample_size].to_vec())
            .collect();
        let buffer: Vec<u8> = samples.iter().flatten().copied().collect();
        let sizes: Vec<usize> = samples.iter().map(|s| s.len()).collect();

        let result = gen_zstd_opt_dict(&buffer, &sizes, 4096, 1, 3);

        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        assert!(!result.unwrap().is_empty());
    }

    // --- get_seek_chunks ---

    #[test]
    fn test_get_seek_chunks_within_single_chunk() {
        let manifest = make_manifest(&[(0, 100, 42)]);

        let result = get_seek_chunks(&manifest, 10, 20).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hash, 42);
        assert_eq!(result[0].start_offset, 10);
        assert_eq!(result[0].end_offset, 30);
    }

    #[test]
    fn test_get_seek_chunks_at_file_start() {
        let manifest = make_manifest(&[(0, 100, 7)]);

        let result = get_seek_chunks(&manifest, 0, 10).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start_offset, 0);
        assert_eq!(result[0].end_offset, 10);
    }

    #[test]
    fn test_get_seek_chunks_at_file_end() {
        let manifest = make_manifest(&[(0, 100, 7)]);

        let result = get_seek_chunks(&manifest, 90, 10).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start_offset, 90);
        assert_eq!(result[0].end_offset, 100);
    }

    #[test]
    fn test_get_seek_chunks_spanning_two_chunks() {
        // Two 50-byte chunks; seek from byte 30 to byte 70 crosses the boundary.
        let manifest = make_manifest(&[(0, 50, 1), (50, 50, 2)]);

        let result = get_seek_chunks(&manifest, 30, 40).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].hash, 1);
        assert_eq!(result[0].start_offset, 30);
        assert_eq!(result[0].end_offset, 50);
        assert_eq!(result[1].hash, 2);
        assert_eq!(result[1].start_offset, 0);
        assert_eq!(result[1].end_offset, 20);
    }

    #[test]
    fn test_get_seek_chunks_covers_all_chunks() {
        let manifest = make_manifest(&[(0, 50, 1), (50, 50, 2), (100, 50, 3)]);

        let result = get_seek_chunks(&manifest, 0, 150).unwrap();

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].hash, 1);
        assert_eq!(result[1].hash, 2);
        assert_eq!(result[2].hash, 3);
    }

    #[test]
    fn test_get_seek_chunks_skips_non_overlapping_chunks() {
        // Seek only within the middle chunk.
        let manifest = make_manifest(&[(0, 50, 1), (50, 50, 2), (100, 50, 3)]);

        let result = get_seek_chunks(&manifest, 50, 50).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hash, 2);
    }

    #[test]
    fn test_get_seek_chunks_out_of_bounds_returns_error() {
        let manifest = make_manifest(&[(0, 100, 1)]);

        let result = get_seek_chunks(&manifest, 0, 101);

        assert!(matches!(result, Err(SpriteShrinkError::Processing(_))));
    }

    #[test]
    fn test_get_seek_chunks_empty_manifest_any_read_is_out_of_bounds() {
        let manifest: FileManifestParent<u64> = FileManifestParent {
            chunk_count: 0,
            chunk_metadata: vec![],
        };

        let result = get_seek_chunks(&manifest, 0, 1);

        assert!(matches!(result, Err(SpriteShrinkError::Processing(_))));
    }
}