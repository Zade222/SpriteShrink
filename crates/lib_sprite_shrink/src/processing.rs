//! Handles the in-memory processing and chunking of files.
//!
//! This module provides the core logic for taking raw file data,
//! breaking it down into content-defined chunks, and generating the
//! necessary metadata for deduplication and archival. It includes
//! functions for chunking and hashing, as well as for rebuilding and
//! verifying files from their chunks.

use std::collections::HashMap;
use std::ffi::c_void;

use dashmap::DashMap;
use fastcdc::v2020::{Chunk, FastCDC, Normalization};
#[cfg(target_os = "macos")]
use libc::{
    pthread_self, pthread_set_qos_class_self_np
};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use sha2::{Digest,Sha512};
use xxhash_rust::xxh3::{xxh3_64_with_seed, xxh3_128_with_seed};
use zstd_sys::{self, ZDICT_params_t};

use crate::archive::compress_with_dict;
use crate::lib_error_handling::LibError;
use crate::lib_structs::{
    ChunkLocation, FileData, FileManifestParent, ProcessedFileData, 
    SSAChunkMeta
};
use crate::parsing::{SS_SEED};

pub trait Hashable:
    //Specifies what capabilities H must have.
    Copy + Clone + Eq + std::hash::Hash + serde::Serialize + Send + Sync + 'static
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
/// - `Ok(Some(ProcessedFileData))` on successful processing.
/// - `Ok(None)` if the input file data is empty.
/// - `Err(LibError)` if chunking or other processing fails.
pub fn process_file_in_memory(
    file_data: FileData,
    window_size: u64,
) -> Result<Option<ProcessedFileData>, LibError> {
    if file_data.file_data.is_empty() {
        return Ok(None);
    }

    let veri_hash = generate_sha_512(&file_data.file_data);
    let chunks = chunk_data(&file_data, SS_SEED, window_size)?;

    Ok(Some(ProcessedFileData {
        file_name: file_data.file_name,
        veri_hash,
        chunks,
        file_data: file_data.file_data,
    }))
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
/// - `Ok(Vec<Chunk>)` containing all the identified data chunks.
/// - `Err(LibError)` if any internal error occurs during chunking.
fn chunk_data(file_data: &FileData, seed: u64, window_size: u64) -> 
    Result<Vec<Chunk>, LibError>
{
    let chunker = FastCDC::with_level_and_seed(
        &file_data.file_data, 
        (window_size/4) as u32, 
        window_size as u32, 
        (window_size*4) as u32, 
        Normalization::Level1, seed);

    let chunks: Vec<Chunk> = chunker.collect();

    Ok(chunks)
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
    file_name: &str,
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
            offset: chunk.offset as u32,
            length: chunk.length as u32,
        });
    }

    //Build file_manifest
    let file_manifest = FileManifestParent {
        filename: file_name.to_string(),
        chunk_count: chunks.len() as u64,
        chunk_metadata,
    };

    (file_manifest, chunk_data_list)
}

/// Rebuilds a single file from chunks and verifies its integrity.
///
/// This function reconstructs a file in memory by fetching each of its
/// data chunks from the serialized data store, using the chunk index for
/// lookups. After reassembling the file, it computes a SHA-512 hash of
/// the result and compares it against the original file's hash to ensure
/// data integrity.
///
/// # Arguments
///
/// * `fmp`: A reference to the `FileManifestParent` for the file.
/// * `ser_data_store`: A byte slice of the complete data store.
/// * `chunk_index`: A map from chunk hashes to their locations.
/// * `veri_hashes`: A map from filenames to their original SHA-512 hashes.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the file is rebuilt and its hash matches.
/// - `Err(LibError)` if a chunk is missing or the hash mismatches.
pub fn rebuild_and_verify_single_file<H>(
    fmp: &FileManifestParent<H>,
    ser_data_store: &[u8],
    chunk_index: &HashMap<H, ChunkLocation>,
    veri_hashes: &DashMap<String, [u8; 64]>,
) -> Result<(), LibError> 
where
    //Required for H: being a hash key and displayable in errors.
    H: Eq + std::hash::Hash + std::fmt::Display,
{
    //Calculate the total size of the original file to pre-allocate memory.
    let file_size: usize = fmp.chunk_metadata.iter().map(|scm|{
        scm.length as usize}).sum();
    let mut data_vec: Vec<u8> = Vec::with_capacity(file_size);

    //Rebuild file
    for scm in &fmp.chunk_metadata {
        if let Some(location) = chunk_index.get(&scm.hash) {
            let start = location.offset as usize;
            let end = start + location.length as usize;
            data_vec.extend_from_slice(&ser_data_store[start..end]);
        } else {
            return Err(LibError::VerificationError(format!(
                "Verification failed: Missing chunk with hash {} for file '{}'",
                scm.hash, fmp.filename
            )));
        } 
    }

    //Verify hydrated file hash aginst original
    if let Some(expected_hash) = veri_hashes.get(&fmp.filename) {
        let built_file_hash = generate_sha_512(&data_vec);
        if *expected_hash.value() != built_file_hash {
            return Err(LibError::HashMismatchError(fmp.filename.clone()));
        }
    } else {
        // This case would also indicate an internal inconsistency.
        return Err(LibError::VerificationError(format!(
            "Verification failed: Missing original hash for file '{}'",
            fmp.filename
        )));
    }

    Ok(())
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
/// * `samples`: A vector of byte slices, where each slice is a sample
///   of data to be used for training the dictionary.
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
/// - `Err(LibError::DictionaryError)` if the underlying zstd library
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
pub fn gen_zstd_opt_dict(samples: Vec<&[u8]>,
    max_dict_size: usize,
    workers: usize,
    compression_level: i32
) -> Result<Vec<u8>, LibError> {
    /*Prepares the samples for the zstd_sys function. */
    let samples_buffer: Vec<u8> = samples.concat();

    /*This vector will store the size of each sample. */
    let sample_sizes: Vec<usize> = samples.iter().map(|s| s.len()).collect();

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
        return Err(LibError::DictionaryError(e.to_string()))
    }

    /*Set final length of buffer so that Rust knows how much data is within 
    it as the unsafe C function from zstd has written to it.*/
    unsafe {
        dict_buffer.set_len(dict_size);
    }

    /*Return the dictionary buffer.*/
    Ok(dict_buffer)
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
/// * `data_store`: A map of chunk hashes to their raw byte data.
/// * `sorted_hashes`: A sorted slice of all unique chunk hashes to process.
/// * `worker_count`: The number of threads to use for parallel compression.
/// * `dictionary_size`: The target size for the temporary dictionary.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(usize)` containing the total estimated compressed size in bytes.
/// - `Err(LibError)` if dictionary creation or compression fails.
pub fn test_compression<H>(
    data_store: &HashMap<H, Vec<u8>>,
    sorted_hashes: &[H],
    worker_count: usize,
    dictionary_size: usize,
) -> Result<usize, LibError>
where
    H: Copy + Eq + std::hash::Hash + Send + Sync,
{
    let samples_for_dict: Vec<&[u8]> = sorted_hashes
        .iter()
        .filter_map(|hash| data_store.get(hash).map(|data| data.as_slice()))
        .collect();

    let dictionary = zstd::dict::from_samples(
        &samples_for_dict,
        dictionary_size, // dictionary size in bytes
        ).map_err(|e| LibError::DictionaryError(e.to_string()))?;
    
        let task_pool = {
        let builder = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count);
        
        #[cfg(target_os = "macos")]
        {
            builder = builder.spawn_handler(|thread| {
                let mut b = thread;
                //Create a place for the new thread's stack
                let mut stack = Vec::new(); 
                mem::swap(b.stack_size_mut(), &mut stack);

                b.spawn(move || {
                    //Inside the new Rayon thread, set its QoS.
                    unsafe {
                        pthread_set_qos_class_self_np(
                            libc::QOS_CLASS_UTILITY, 0
                        );
                    }
                });
            });
        }
        
        builder.build()
            .map_err(|e| LibError::ThreadPoolError(
                format!("Failed to create thread pool: {e}"))
            )?
    };

    let compressed_dash: DashMap<H, Vec<u8>> = DashMap::new();

    let comp_result: Result<(), LibError> = task_pool.install(|| {
        sorted_hashes.par_iter().try_for_each(|hash| {
            if let Some(data) = data_store.get(hash){
                let compressed_chunk = compress_with_dict(
                    data.as_slice(), 
                    &dictionary, 
                    &7)
                    .map_err(|e| LibError::CompressionError(e.to_string()))?;

                compressed_dash.insert(*hash, compressed_chunk);
            }
            Ok(())
        })
    });

    //Check if any of the parallel operations failed.
    comp_result?;

    let compressed_size:usize = compressed_dash.iter()
        //Get the length of each Vec
        .map(|entry| entry.value().len()) 
        .sum(); //Add everything up.
    
    Ok((dictionary.len() as usize) + compressed_size)
}

