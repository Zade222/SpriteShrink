//! Handles the final assembly and serialization of the archive file.
//!
//! This module is responsible for taking all processed data and metadata
//! to construct the final, portable archive. It orchestrates the entire
//! process, from building the file header and training a compression
//! dictionary to compressing data chunks and serializing the complete
//! archive into a single byte vector ready for storage.

use std::{
    {collections::HashMap, io::Write},
    fmt::Display,
    hash::Hash,
    io::Read,
    mem,
    os::raw::c_void
};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use zerocopy::IntoBytes;
use dashmap::DashMap;
use serde::Serialize;

use crate::ffi::ffi_structs::{
    FFIUserData, FFIProgress, FFIProgressType
};

#[cfg(target_os = "macos")]
use libc::{
    pthread_self, pthread_set_qos_class_self_np
};

use crate::lib_error_handling::LibError;

use crate::lib_structs::{
    FileHeader, FileManifestParent, Progress
};  

use crate::parsing::{MAGIC_NUMBER, SUPPORTED_VERSION};

use crate::processing::gen_zstd_opt_dict;

use crate::serialization::{serialize_store, serialize_store_from_dashmap};

unsafe impl Send for FFIUserData {}
unsafe impl Sync for FFIUserData {}

//C style progress callback function pointer
type CProgressCallback = extern "C" fn(FFIProgress, *mut c_void);

pub struct ArchiveBuilder<'cb, F, H> {
    //Required parameters
    ser_file_manifest: Vec<FileManifestParent<H>>,
    data_store: HashMap<H, Vec<u8>>,
    sorted_hashes: Vec<H>,
    file_count: u32,
    hash_type: u8,

    //Optional parameters, will be set with default values.
    compression_algorithm: u16,
    compression_level: i32,
    dictionary_size: u64,
    worker_threads: usize,
    opt_dict: bool,
    progress_callback: Option<&'cb F>,
    c_progress_callback: Option<(CProgressCallback, FFIUserData)>,
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
/// * `algorithm_code`: Numerical value representing the compression used on 
///   the compressed data.
/// * `hash_type`: Numerical value representing the hash type used when 
///   the data was hashed.
/// * `man_length`: The total length in bytes of the file manifest.
/// * `dict_length`: The total length in bytes of the dictionary.
/// * `chunk_index_length`: The total length of the chunk index.
///
/// # Returns
///
/// Returns a `FileHeader` struct populated with the provided metadata.
fn build_file_header(
    file_count: u32,
    algorithm_code: u16,
    hash_type: u8,
    man_length: u64,
    dict_length: u64,
    chunk_index_length: u64,
) -> FileHeader {
    //Get the size of the current FileHeader struct.
    let header_size = mem::size_of::<FileHeader>() as u64;

    //Build, file and return a FileHeader struct with data.
    FileHeader {
        magic_num:      MAGIC_NUMBER,
        file_version:   SUPPORTED_VERSION,
        file_count,
        algorithm:      algorithm_code,
        hash_type,
        pad:            [0, 0, 0, 0, 0],
        man_offset:     header_size,
        man_length,
        dict_offset:    header_size + man_length,
        dict_length,
        chunk_index_offset: header_size + man_length + dict_length,
        chunk_index_length,
        data_offset: header_size + man_length + dict_length + 
            chunk_index_length
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

impl<'cb, F, H> ArchiveBuilder<'cb, F, H>
where
    F: Fn(Progress) + Sync + Send,
    H: Copy + Eq + Hash + Serialize + Send + Sync + 'static + Display,
{
    pub fn new(
        ser_file_manifest: Vec<FileManifestParent<H>>,
        data_store: HashMap<H, Vec<u8>>,
        sorted_hashes: Vec<H>,
        file_count: u32,
        hash_type: u8,
    ) -> Self {
        Self {
            ser_file_manifest,
            data_store,
            sorted_hashes,
            file_count,
            hash_type,
            //Set default values for optional parameters
            compression_algorithm: 98, //Default to zstd numerical code.
            compression_level: 19, 
            dictionary_size: 16 * 1024,
            worker_threads: 0, //Let Rayon decide
            opt_dict: false,
            progress_callback: None,
            c_progress_callback: None,
        }
    }

    //The following 5 functions set optional parameters.

    /// Sets the numerical compression code.
    ///
    /// The default value is `98` for zstd.
    pub fn compression_algorithm(&mut self, algorithm: u16) -> &mut Self {
        self.compression_algorithm = algorithm;
        self
    }

    /// Sets the compression level.
    ///
    /// The default value is `19` for zstd.
    pub fn compression_level(&mut self, level: i32) -> &mut Self {
        self.compression_level = level;
        self
    }

    /// Sets the target size for the compression dictionary in bytes.
    ///
    /// The default value is `16384` (16 KiB).
    pub fn dictionary_size(&mut self, size: u64) -> &mut Self {
        self.dictionary_size = size;
        self
    }

    pub fn worker_threads(&mut self, threads: usize) -> &mut Self {
        self.worker_threads = threads;
        self
    }

    /// Enables dictionary optimization.
    ///
    /// This can improve compression but is significantly slower.
    /// The default is `false`.
    pub fn optimize_dictionary(&mut self, optimize: bool) -> &mut Self {
        self.opt_dict = optimize;
        self
    }

    /// Sets a callback function for progress reporting.
    pub fn with_progress(mut self, callback: &'cb F) -> Self {
        self.progress_callback = Some(callback);
        self
    }

    /// Sets a callback function for progress reporting when called via C.
    pub fn with_c_progress(
            &mut self, 
            callback: CProgressCallback, 
            user_data: *mut c_void
        ) -> &mut Self {
            self.c_progress_callback = Some((
                callback, 
                FFIUserData(user_data)
            ));
            self
    }

    fn report_progress(&self, progress: Progress) {
    if let Some(callback) = self.progress_callback {
        callback(progress.clone());
    }

    if let Some((callback, user_data)) = self.c_progress_callback {
        let ffi_progress = match progress {
            Progress::GeneratingDictionary => FFIProgress {
                ty: FFIProgressType::GeneratingDictionary, 
                total_chunks: 0
            },
            Progress::DictionaryDone => FFIProgress {
                ty: FFIProgressType::DictionaryDone, 
                total_chunks: 0
            },
            Progress::Compressing { total_chunks } => FFIProgress { 
                ty: FFIProgressType::Compressing, 
                total_chunks
            },
            Progress::ChunkCompressed => FFIProgress {
                ty: FFIProgressType::ChunkCompressed, 
                total_chunks: 0
            },
            Progress::Finalizing => FFIProgress {
                ty: FFIProgressType::Finalizing,
                total_chunks: 0
            },
        };
        callback(ffi_progress, user_data.0);
    }
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
        /*let samples_for_dict: Vec<&[u8]> = self.sorted_hashes
            .iter()
            .filter_map(|hash| self.data_store
                .get(hash)
                .map(|data| data.as_slice()))
            .collect();*/

        //Max samples is approximately 3.9 gigabytes.
        const MAX_SAMPLES_SIZE: u64 = 4 * 1024 * 1024 * 922;

        let mut samples_for_dict: Vec<&[u8]> = Vec::new();
        let mut current_samples_size: u64 = 0;

        let total_chunks = self.sorted_hashes.len();
        if total_chunks > 0 {
            /*Calculate a step value to sample evenly across all chunks.
            This ensures we select chunks from the beginning, middle, and 
            end.*/
            let step = (total_chunks as u64 * 100 / 
                MAX_SAMPLES_SIZE.min(self.data_store
                    .values()
                    .map(|v| v.len() as u64)
                    .sum()))
                .max(1) as usize;

            for i in (0..total_chunks).step_by(step) {
                if let Some(hash) = self.sorted_hashes.get(i) {
                    if let Some(data) = self.data_store.get(hash) {
                        // Check if adding the next sample would exceed the limit.
                        if current_samples_size + (data.len() as u64) > MAX_SAMPLES_SIZE {
                            break; // Stop if we hit the size limit.
                        }
                        samples_for_dict.push(data.as_slice());
                        current_samples_size += data.len() as u64;
                    }
                }
            }
        }
        //If the dataset is small enough, this will just use all the chunks.
        if samples_for_dict.is_empty() && total_chunks > 0 {
            /*Fallback for very small datasets where the step calculation 
            might be off.*/
            for hash in &self.sorted_hashes {
                if let Some(data) = self.data_store.get(hash) {
                    if current_samples_size + (data.len() as u64) > 
                        MAX_SAMPLES_SIZE { break; }
                    samples_for_dict.push(data.as_slice());
                    current_samples_size += data.len() as u64;
                }
            }
        }

        //Make dictionary from sorted data.
        let mut _dictionary: Vec<u8> = Vec::new();

        //Report progress before starting a dictionary generation
        self.report_progress(Progress::GeneratingDictionary);
        
        if self.opt_dict{
            _dictionary = gen_zstd_opt_dict(
            samples_for_dict, 
            self.dictionary_size as usize, 
            self.worker_threads, 
            self.compression_level)?;
        } else {
            _dictionary = zstd::dict::from_samples(
            &samples_for_dict,
            self.dictionary_size as usize, //Dictionary size in bytes
            ).map_err(|e| LibError::CompressionError(e.to_string()))?;
        }

        //Report progress after dictionary generation is done.
        self.report_progress(Progress::DictionaryDone);
        
        let task_pool = {
            let builder = rayon::ThreadPoolBuilder::new()
                .num_threads(self.worker_threads);
            
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
            //Report that compression is starting
            self.report_progress(Progress::Compressing { 
                total_chunks: (
                    self.sorted_hashes.len() as u64 
                )
            });
            
            self.sorted_hashes.par_iter().try_for_each(|hash| {
                if let Some(data) = self.data_store.get(hash){
                    let compressed_chunk = compress_with_dict(
                        data.as_slice(), 
                        &_dictionary, 
                        &self.compression_level)
                        .map_err(|e| LibError::CompressionError(
                            e.to_string()
                        ))?;

                    compressed_dash.insert(*hash, compressed_chunk);

                    self.report_progress(Progress::ChunkCompressed);
                }
                Ok(())
            })
        });

        //Check if any of the parallel operations failed.
        comp_result?;
        /*
        let (compressed_data_store, chunk_index) = 
            serialize_store(&compressed_dash, &self.sorted_hashes)?;*/
        

        //drop(compressed_dash);

        let (compressed_data_store, chunk_index) = 
            serialize_store_from_dashmap(compressed_dash, &self.sorted_hashes)?;

        /*The following are now prepared:
        compressed_data store
        chunk_index
        dictionary*/

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
            self.compression_algorithm,
            self.hash_type,
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

pub fn decompress_chunk(
    comp_chunk_data: &[u8],
    dictionary: &[u8]
) -> Result<Vec<u8>, LibError> {
    /*Create a zstd decoder with the prepared dictionary from the file
    archive.*/
    let mut decoder = zstd::stream::Decoder::with_dictionary(
        comp_chunk_data, 
        dictionary)?;

    //Decompress the data into a new vector.
    let mut decompressed_chunk_data = Vec::new();
    decoder.read_to_end(&mut decompressed_chunk_data)?;

    Ok(decompressed_chunk_data)
}