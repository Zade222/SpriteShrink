//! Handles the final assembly and serialization of the archive file.
//!
//! This module is responsible for taking all processed data and metadata
//! to construct the final, portable archive. It orchestrates the entire
//! process, from building the file header and training a compression
//! dictionary to compressing data chunks and serializing the complete
//! archive into a single byte vector ready for storage.

use std::{collections::HashMap, io::Write};
use std::mem;

use bincode;

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use zerocopy::IntoBytes;
use dashmap::DashMap;

use crate::lib_error_handling::LibError;

use crate::lib_structs::{
    FileHeader, FileManifestParent, Progress
};  

use crate::parsing::{MAGIC_NUMBER, SUPPORTED_VERSION};

use crate::processing::gen_zstd_opt_dict;

use crate::serialization::{serialize_compressed_store};

pub struct ArchiveBuilder<'a, F> {
    //Required parameters
    ser_file_manifest: &'a [FileManifestParent],
    data_store: &'a HashMap<u64, Vec<u8>>,
    sorted_hashes: &'a [u64],
    file_count: u32,

    //Optional parameters, will be set with default values.
    compression_level: i32,
    dictionary_size: u64,
    worker_threads: usize,
    opt_dict: bool,
    progress_callback: Option<&'a F>,
}

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
pub fn compress_with_dict(data_payload: &[u8], dict: &[u8], level: &i32) -> 
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

impl<'a, F> ArchiveBuilder<'a, F>
where
    F: Fn(Progress) + Sync + Send,
{
    pub fn new(
        ser_file_manifest: &'a [FileManifestParent],
        data_store: &'a HashMap<u64, Vec<u8>>,
        sorted_hashes: &'a [u64],
        file_count: u32
    ) -> Self {
        Self {
            ser_file_manifest,
            data_store,
            sorted_hashes,
            file_count,
            //Set default values for optional parameters
            compression_level: 19, 
            dictionary_size: 16 * 1024,
            worker_threads: 0, //Let Rayon decide
            opt_dict: false,
            progress_callback: None,
        }
    }

    //The following 4 functions set optional parameters.

    /// Sets the Zstandard compression level.
    ///
    /// The default value is `19`.
    pub fn compression_level(mut self, level: i32) -> Self {
        self.compression_level = level;
        self
    }

    /// Sets the target size for the compression dictionary in bytes.
    ///
    /// The default value is `16384` (16 KiB).
    pub fn dictionary_size(mut self, size: u64) -> Self {
        self.dictionary_size = size;
        self
    }

    /// Enables dictionary optimization.
    ///
    /// This can improve compression but is significantly slower.
    /// The default is `false`.
    pub fn optimize_dictionary(mut self, optimize: bool) -> Self {
        self.opt_dict = optimize;
        self
    }

    /// Sets a callback function for progress reporting.
    pub fn with_progress(mut self, callback: &'a F) -> Self {
        self.progress_callback = Some(callback);
        self
    }

    /// Consumes the builder and returns the final archive as a byte vector.
    ///
    /// This function orchestrates the final stage of the archiving
    /// process. It takes the processed file metadata, unique data chunks,
    /// and other parameters to construct the complete archive in memory.
    ///
    /// The process includes training a compression dictionary,
    /// compressing data chunks in parallel, serializing all metadata,
    /// and combining everything into a single, contiguous byte vector.
    ///
    /// # Returns
    ///
    /// A `Result` which is:
    /// - `Ok(Vec<u8>)` containing the complete binary data of the archive.
    /// - `Err(LibError)` if any step fails, such as dictionary training,
    ///   compression, or serialization.
    pub fn build(self) -> Result<Vec<u8>, LibError> {
        //Sort to prepare data to be analyzed to build dictionary.
        let samples_for_dict: Vec<&[u8]> = self.sorted_hashes
            .iter()
            .filter_map(|hash| self.data_store.get(hash).map(|data| data.as_slice()))
            .collect();

        //Make dictionary from sorted data.
        let mut _dictionary: Vec<u8> = Vec::new();

        //Report progress before starting a dictionary generation
        if let Some(callback) = self.progress_callback {
            callback(Progress::GeneratingDictionary);
        }
        
        if self.opt_dict{
            _dictionary = gen_zstd_opt_dict(
            samples_for_dict, 
            self.dictionary_size as usize, 
            self.worker_threads, 
            self.compression_level)?;
        } else {
            _dictionary = zstd::dict::from_samples(
            &samples_for_dict,
            self.dictionary_size as usize, // dictionary size in bytes
            ).map_err(|e| LibError::CompressionError(e.to_string()))?;
        }

        //Report progress after dictionary generation is done.
        if let Some(callback) = self.progress_callback {
            callback(Progress::DictionaryDone);
        }
        
        let task_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.worker_threads)
            .build()
            .map_err(|e| LibError::ThreadPoolError(format!("Failed to create thread pool: {}", e)))?;

        let compressed_dash: DashMap<u64, Vec<u8>> = DashMap::new();

        let comp_result: Result<(), LibError> = task_pool.install(|| {
            //Report that compression is starting
            if let Some(callback) = self.progress_callback {    
                callback(Progress::Compressing {
                    total_chunks: self.sorted_hashes.len() as u64 
                });
            }
            
            self.sorted_hashes.par_iter().try_for_each(|hash| {
                if let Some(data) = self.data_store.get(hash){
                    let compressed_chunk = compress_with_dict(
                        data.as_slice(), 
                        &_dictionary, 
                        &self.compression_level)
                        .map_err(|e| LibError::CompressionError(e.to_string()))?;

                    compressed_dash.insert(*hash, compressed_chunk);

                    if let Some(callback) = self.progress_callback {
                        callback(Progress::ChunkCompressed);
                    }
                }
                Ok(())
            })
        });

        //Check if any of the parallel operations failed.
        comp_result?;

        let (compressed_data_store, chunk_index) = 
            serialize_compressed_store(&compressed_dash, self.sorted_hashes);

        drop(compressed_dash);

        /*The following are now prepared:
        compressed_data store
        chunk_index
        dictionary*/

        //Report that process is in the final stage
        if let Some(callback) = self.progress_callback {
            callback(Progress::Finalizing);
        }

        let config = bincode::config::standard();

        let bin_file_manifest = bincode::serde::encode_to_vec(
        &self.ser_file_manifest, config
        ).map_err(|e| LibError::ManifestEncodeError(e.to_string()))?;

        let bin_chunk_index = bincode::serde::encode_to_vec(
        &chunk_index, config
        ).map_err(|e| LibError::IndexEncodeError(e.to_string()))?;

        //Build the file header
        let file_header = build_file_header(
            self.file_count,
            bin_file_manifest.len() as u64,
            _dictionary.len() as u64,
            bin_chunk_index.len() as u64,
        );

        let total_file_size = file_header.data_offset  as usize + 
            compressed_data_store.len();

        let mut final_data = Vec::with_capacity(total_file_size);

        final_data.extend_from_slice(file_header.as_bytes());
        final_data.extend_from_slice(&bin_file_manifest);
        final_data.extend_from_slice(&_dictionary);
        final_data.extend_from_slice(&bin_chunk_index);
        final_data.extend_from_slice(&compressed_data_store);

        Ok(final_data)
    }
}