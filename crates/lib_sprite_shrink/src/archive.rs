//! Handles the final assembly and serialization of the archive file.
//!
//! This module is responsible for taking all processed data and metadata
//! to construct the final, portable archive. It orchestrates the entire
//! process, from building the file header and training a compression
//! dictionary to compressing data chunks and serializing the complete
//! archive into a single byte vector ready for storage.

use std::{collections::HashMap, io::Write};
use std::mem;
//use std::time::Instant;

use bincode;

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use zerocopy::IntoBytes;
use dashmap::DashMap;

use crate::lib_error_handling::LibError;

use crate::lib_structs::{
    FileHeader, FileManifestParent,
};  

use crate::parsing::{MAGIC_NUMBER, SUPPORTED_VERSION};

use crate::serialization::{serialize_compressed_store};

/// Constructs the file header for a new archive.
///
/// This function assembles a `FileHeader` struct, which is the starting
/// block of an archive. It contains critical metadata, including magic
/// numbers, versioning info, and the precise offsets and lengths for all
/// major data sections. This header is essential for allowing extraction
/// logic to properly navigate and decompress the archive.
///
/// # Arguments
///
/// * `file_count`: The total number of files to be included.
/// * `man_length`: The total length in bytes of the file manifest.
/// * `dict_length`: The total length in bytes of the dictionary.
/// * `chunk_index_length`: The total length of the chunk index.
///
/// # Returns
///
/// Returns a `FileHeader` struct populated with the provided metadata.
fn build_file_header(file_count: u32,
    man_length: u64,
    dict_length: u64,
    chunk_index_length: u64,
) -> FileHeader {
    //Get the size of the current FileHeader struct.
    let header_size = mem::size_of::<FileHeader>() as u64;

    //Build, file and return a FileHeader struct with data.
    FileHeader {
        magic_num:      *MAGIC_NUMBER,
        file_version:   SUPPORTED_VERSION,
        file_count:     file_count,
        man_offset:     header_size,
        man_length:     man_length,
        dict_offset:    header_size + man_length,
        dict_length:    dict_length,
        chunk_index_offset: header_size + man_length + dict_length,
        chunk_index_length: chunk_index_length,
        data_offset: header_size + man_length + dict_length + chunk_index_length
    }
}

/// Compresses a data slice using a shared dictionary.
///
/// This function applies Zstandard compression to a given byte slice. It
/// leverages a pre-computed dictionary to achieve higher compression
/// ratios, which is effective when compressing many small, similar files.
/// The compression level can be adjusted to balance speed and size.
///
/// # Arguments
///
/// * `data_payload`: A slice of bytes representing the original data.
/// * `dict`: A byte slice of the shared compression dictionary.
/// * `level`: An integer specifying the desired compression level.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the compressed data as a byte vector.
/// - `Err` if the compression process fails.
fn compress_with_dict(data_payload: &[u8], dict: &[u8], level: &i32) -> 
    Result<Vec<u8>, LibError>
{
    //Create and initiate vector for storing compressed data bytes.
    let mut compressed_data = Vec::new();

    //Create encoder that will process data.
    let mut encoder = zstd::stream::Encoder::with_dictionary(
        &mut compressed_data, 
        *level, 
        dict)?;
    
    //Use encoder to compress the data payload.
    encoder.write_all(data_payload)?;

    //Finalize the compression.
    encoder.finish()?;

    //Return compress data.
    Ok(compressed_data)
}

/// Assembles the final archive from its constituent parts.
///
/// This function orchestrates the final stage of the archiving process.
/// It takes the processed file metadata, unique data chunks, and other
/// parameters to construct the complete archive in memory. The process
/// includes training a compression dictionary, compressing data chunks in
/// parallel, serializing all metadata, and combining everything into a
/// single, contiguous byte vector.
///
/// # Arguments
///
/// * `ser_file_manifest`: A reference to the file manifest data.
/// * `data_store`: A `HashMap` of all unique, uncompressed data chunks.
/// * `sorted_hashes`: A sorted vector of all unique chunk hashes.
/// * `file_count`: The total number of files being added.
/// * `level`: The Zstandard compression level to be used.
/// * `worker_threads`: The number of threads for parallel compression.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the complete binary data of the archive.
/// - `Err(LibError)` if any step fails, such as dictionary training,
///   compression, or serialization.
pub fn finalize_archive(
    ser_file_manifest: &[FileManifestParent],
    data_store: &HashMap<u64, Vec<u8>>,
    sorted_hashes: &[u64],
    file_count: u32,
    compression_level: i32,
    dictionary_size: u64,
    worker_threads: usize
) -> Result<Vec<u8>, LibError> {
    //Sort to prepare data to be analyzed to build dictionary.
    let samples_for_dict: Vec<&[u8]> = sorted_hashes
        .iter()
        .filter_map(|hash| data_store.get(hash).map(|data| data.as_slice()))
        .collect();
    
    //let dict_start_time = Instant::now();

    //Make dictionary from sorted data.
    let dictionary = zstd::dict::from_samples(
        &samples_for_dict,
        dictionary_size as usize, // dictionary size in bytes
    ).map_err(|e| LibError::CompressionError(e.to_string()))?;

    //let dict_elapsed_time = dict_start_time.elapsed();

    //println!("The building dictionary took: {:?}", dict_elapsed_time);
    
    let task_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads)
        .build()
        .map_err(|e| LibError::InternalLibError(format!("Failed to create thread pool: {}", e)))?;

    let compressed_dash: DashMap<u64, Vec<u8>> = DashMap::new();

    //let comp_start_time = Instant::now();

    let comp_result: Result<(), LibError> = task_pool.install(|| {
        sorted_hashes.par_iter().try_for_each(|hash| {
            if let Some(data) = data_store.get(hash){
                let compressed_chunk = compress_with_dict(
                    data.as_slice(), 
                    &dictionary, 
                    &compression_level)
                    .map_err(|e| LibError::CompressionError(e.to_string()))?;

                compressed_dash.insert(*hash, compressed_chunk);
            }
            Ok(())
        })
    });

    //let elapsed_time = comp_start_time.elapsed();

    //println!("The compression function took: {:?}", elapsed_time);

    //Check if any of the parallel operations failed.
    comp_result?;

    let (compressed_data_store, chunk_index) = 
        serialize_compressed_store(&compressed_dash, sorted_hashes);

    drop(compressed_dash);

    /*The following are now prepared:
    compressed_data store
    chunk_index
    dictionary*/

    let config = bincode::config::standard();

    let bin_file_manifest = bincode::serde::encode_to_vec(
    &ser_file_manifest, config
    ).map_err(|e| LibError::ManifestEncodeError(e.to_string()))?;

    let bin_chunk_index = bincode::serde::encode_to_vec(
    &chunk_index, config
    ).map_err(|e| LibError::IndexEncodeError(e.to_string()))?;

    //Build the file header
    let file_header = build_file_header(
        file_count,
        bin_file_manifest.len() as u64,
        dictionary.len() as u64,
        bin_chunk_index.len() as u64,
    );

    let total_file_size = file_header.data_offset  as usize + 
        compressed_data_store.len();

    let mut final_data = Vec::with_capacity(total_file_size);

    final_data.extend_from_slice(file_header.as_bytes());
    final_data.extend_from_slice(&bin_file_manifest);
    final_data.extend_from_slice(&dictionary);
    final_data.extend_from_slice(&bin_chunk_index);
    final_data.extend_from_slice(&compressed_data_store);

    Ok(final_data)
}