//! Handles the final assembly and serialization of the archive file.
//!
//! This module is responsible for taking all processed data and metadata
//! to construct the final, portable archive. It orchestrates the entire
//! process, from building the file header and training a compression
//! dictionary to compressing data chunks and serializing the complete
//! archive into a single byte vector ready for storage.

use std::{
    {collections::{
        HashMap}, 
    },
    fmt::{Debug, Display},
    hash::Hash,
    io::{Read, Write},
    mem,
    os::raw::c_void,
    sync::{Arc, Mutex},
    thread
};

use serde::Serialize;
use zerocopy::IntoBytes;


use crate::ffi::ffi_structs::{
    FFIUserData, FFIProgress, FFIProgressType
};


use crate::lib_error_handling::LibError;

use crate::lib_structs::{
    ChunkLocation, FileHeader, FileManifestParent, Progress
};  

use crate::parsing::{MAGIC_NUMBER, SUPPORTED_VERSION};

use crate::processing::{
    build_train_samples, gen_zstd_opt_dict
};

unsafe impl Send for FFIUserData {}
unsafe impl Sync for FFIUserData {}

//C style progress callback function pointer
type CProgressCallback = extern "C" fn(FFIProgress, *mut c_void);

pub struct ArchiveBuilder<'a, H, R, W> {
    //Required parameters
    ser_file_manifest: Vec<FileManifestParent<H>>,
    sorted_hashes: &'a [H],
    file_count: u32,
    hash_type: u8,
    total_size: u64,
    get_chunk_data: R,
    write_comp_data: W,

    //Optional parameters, will be set with default values.
    compression_algorithm: u16,
    compression_level: i32,
    dictionary_size: u64,
    worker_threads: usize,
    opt_dict: bool,
    progress_callback: Option<Box<dyn Fn(Progress) + Sync + Send>>,
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
/// * `level`: A signed integer specifying the desired compression level.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the compressed data as a byte vector.
/// - `Err` if the compression process fails.
pub fn compress_with_dict(
    data_payload: &[u8], 
    dict: &[u8], 
    level: &i32
) -> Result<Vec<u8>, LibError> {
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

/// Compresses a collection of data chunks in parallel using a 
/// producer-consumer model.
///
/// This function orchestrates a high-performance, multi-threaded compression
/// pipeline designed to handle large sets of unique data chunks efficiently. 
/// It separates the I/O-bound task of reading and writing from the CPU-bound
/// task of compression.
///
/// The core architecture consists of:
/// 1.  **An I/O Coordinator Thread**: This thread is responsible for reading
///     uncompressed chunk data (via the `chunk_list_read_cb`), sending it to
///     the worker pool, receiving compressed data, and writing it to the final
///     destination in the correct order (via `chunk_write_cb`).
/// 2.  **A Worker Pool**: A configurable number of threads that receive
///     uncompressed chunks, compress them using the provided Zstandard
///     dictionary, and send them back to the I/O thread.
///
/// To handle out-of-order completion from the worker threads, the I/O thread
/// uses a `reordering_buffer`. This ensures that the final compressed data is
/// written sequentially according to the initial `sorted_hashes` order, which
/// is critical for generating a correct chunk index.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must be `Copy`, `Debug`, `Eq`, `Hash`,
///   `Send`, `Sync`, and have a `'static` lifetime.
/// * `R`: The type of the `chunk_list_read_cb` callback, which must be a
///   closure that takes a slice of hashes and returns a `Vec` of tuples, where
///   each tuple contains a hash and its corresponding uncompressed byte data.
/// * `W`: The type of the `chunk_write_cb` callback, which must be a closure
///   that takes a slice of compressed bytes and handles writing it to the
///   final destination.
///
/// # Arguments
///
/// * `worker_count`: The number of threads to spawn for the compression worker
///   pool. If `0`, it defaults to the number of available logical cores minus 
///   one.
/// * `dictionary`: A byte slice containing the pre-trained Zstandard
///   compression dictionary.
/// * `sorted_hashes`: A slice of all unique chunk hashes, sorted to ensure a
///   deterministic write order.
/// * `compression_level`: The Zstandard compression level to be used by the
///   worker threads.
/// * `chunk_list_read_cb`: A callback function that the I/O thread uses to
///   fetch the uncompressed data for a given set of hashes.
/// * `chunk_write_cb`: A callback function that the I/O thread uses to write
///   the ordered, compressed chunk data to its final destination (e.g., a file
///   or an in-memory buffer).
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<(H, ChunkLocation)>)` on success, containing the final chunk
///   index. The index is a vector of tuples, where each tuple maps a chunk's
///   hash to its `ChunkLocation` (offset and length) in the final compressed
///   data blob.
/// - `Err(LibError)` if any part of the process fails, such as a worker thread
///   panicking, a failure in a callback, or an internal logic error.
///
/// # Errors
///
/// This function can return an error in the following cases:
/// - `LibError::InternalLibError` if a worker thread pool terminates
///   unexpectedly or if not all chunks are processed correctly.
/// - Any error propagated from the compression logic within a worker thread.
pub fn compress_chunks<H, R, W>(
    worker_count: usize,
    dictionary: &[u8],
    sorted_hashes: &[H],
    compression_level: i32,
    chunk_list_read_cb: R,
    chunk_write_cb: W,
) -> Result<Vec<(H, ChunkLocation)>, LibError>
where
    H: Copy + Debug + Eq + Hash + Send + Sync + 'static,
    R: Fn(&[H]) -> Vec<(H, Vec<u8>)> + Send + Sync + 'static,
    W: Fn(&[u8]) + Send + Sync + 'static,
{
    //Set lower and upper thresholds for prefetcher.
    const PREFETCH_LOW_THRESHOLD: usize = 100;
    const PREFETCH_HIGH_THRESHOLD: usize = 500;

    //Calculate total chunks from the amount of hashes.
    let total_chunks = sorted_hashes.len();

    //Create Arc for sorted hashes for safely sharing.
    let sorted_hashes_arc: Arc<[H]> = Arc::from(sorted_hashes);
    
    /*Create channels for sending/receiving to compressor info limited by
    PREFETCH_HIGH_THRESHOLD.
    This acts as the FIFO job buffer.*/
    let (to_compress_tx, to_compress_rx) = 
        flume::bounded::<(H, Vec<u8>)>(PREFETCH_HIGH_THRESHOLD);

    //Create channels for sending/receiving from compressor.
    let (from_compress_tx, from_compress_rx) = 
        flume::unbounded::<(H, Vec<u8>)>();

    //Error channels
    let (err_tx, err_rx) = std::sync::mpsc::channel::<LibError>();

    //Create arc for safely sharing dictionary.
    let dictionary_arc = Arc::new(dictionary.to_vec());

    //Create vector to contain all worker handles.
    let mut worker_handles = Vec::with_capacity(worker_count);

    let num_threads = if worker_count == 0 {
        std::thread::available_parallelism().unwrap().get()
    } else {
        worker_count
    };

    

    // Resolve the number of threads to use. If 0, use available parallelism.
    let num_workers = (num_threads).saturating_sub(1).max(1);

    //Spawn workers according to the specified worker count.
    for _ in 0..(num_workers) {
        //Clone necessary data to be used safely.
        let to_compress_rx_clone = to_compress_rx.clone();
        let from_compress_tx_clone = from_compress_tx.clone();
        let err_tx_clone = err_tx.clone();
        let dictionary_clone = Arc::clone(&dictionary_arc);

        //Create handle/thread for worker.
        let handle = thread::spawn(move || {
            /*Workers loop until the to_compress channel is empty and 
            disconnected.*/
            while let Ok((hash, data)) = to_compress_rx_clone.recv() {
                match compress_with_dict(
                    &data, 
                    &dictionary_clone, 
                    &compression_level
                ) {
                    Ok(compressed_data) => {
                        if from_compress_tx_clone.send((
                            hash, 
                            compressed_data
                        )).is_err() {
                            //The I/O thread has terminated; exit gracefully.
                            break;
                        }
                    }
                    Err(e) => {
                        //Report the error and stop processing.
                        let _ = err_tx_clone.send(e);
                        break;
                    }
                }
            }
        });
        worker_handles.push(handle);
    }
    //Drop the original receiver clone so the loop terminates correctly.
    drop(to_compress_rx);

    drop(from_compress_tx);

    //Clone sorted_hashes to be used safely.
    let sha_clone = Arc::clone(&sorted_hashes_arc);

    //Create chunk_index vector encapsulated in an Arc.
    let chunk_index = Arc::new(Mutex::new(Vec::with_capacity(total_chunks)));

    //Make clone for safe use in separate thread.
    let chunk_index_clone = Arc::clone(&chunk_index);

    //Create handle/thread for IO coordinator.
    let io_handle = thread::spawn(move || -> Result<usize, LibError> {
        /*Tracks which chunks have been read from the source and sent to the 
        workers for compression. */
        let mut read_cursor = 0usize;

        /*Tracks which chunks have been written to the destination in the 
        correct, sequential order.*/
        let mut write_cursor = 0usize;

        /*Tracks the offset into the final data blob of the archive to be 
        stored by the chunk index.*/
        let mut offset = 0u64;
        
        /*Temporary buffer for holding compressed data if received out of 
        order.*/
        let mut reordering_buffer: HashMap<H, Vec<u8>> = HashMap::new();

        let mut max_buffer_size = 0usize;

        /*Loop that runs until all chunks have been sent to the host 
        application to be written.*/
        while write_cursor < total_chunks {
            
            //If the to_compress_tx job buffer is below the threshold.
            if to_compress_tx.len() < PREFETCH_LOW_THRESHOLD && 
                //and if the read_cursor is below total chunks
                read_cursor < total_chunks {
                    //Calculate how much chunk data to get.
                    let end = (read_cursor + PREFETCH_HIGH_THRESHOLD)
                        .min(total_chunks);
                    //Generate list of hashes to retrieve.
                    let hashes_to_read = &sha_clone[read_cursor..end];
                    
                    //Use callback to receive chunk data.
                    for chunk in chunk_list_read_cb(hashes_to_read) {
                        if to_compress_tx.send(chunk).is_err() {
                            //State if error occurred.
                            return Err(LibError::InternalLibError(
                                "Worker pool terminated unexpectedly."
                                .to_string()));
                        }
                        read_cursor = end;
                    }
            }

            //Match for receiving compressed data.
            match from_compress_rx.recv() {
                //For a received hash and compressed chunk data
                Ok((hash, compressed_data)) => {
                    //Add to reordering buffer.
                    reordering_buffer.insert(hash, compressed_data);
                    let buf_cap = reordering_buffer.capacity();
                    if max_buffer_size < buf_cap{
                        max_buffer_size = buf_cap;
                    };
                }
                //Else catch error.
                Err(_) => {
                    if write_cursor < total_chunks {
                         return Err(LibError::InternalLibError(
                            "Worker pool finished but not all chunks \
                            were processed.".to_string()));
                    }
                    break;
                }
            }

            //While write_cursor is less than total_chunks
            while write_cursor < total_chunks {
                //Set next hash to write
                let next_hash_to_write = sha_clone[write_cursor];

                //If there is data in the reordering buffer
                if let Some(data_to_write) = reordering_buffer
                    //And if the next hash is found, remove the key and data
                    .remove(&next_hash_to_write) {
                        //Calculate the length of the compressed data
                        let data_len = data_to_write.len() as u64;

                        let mut ci_clone = chunk_index_clone
                            .lock()
                            .unwrap();

                        /*Send the compressed data to be written by the host 
                        application*/
                        chunk_write_cb(&data_to_write);

                        //Incrment write cursor.
                        write_cursor += 1;

                        //Add chunk location data to chunk index.
                        ci_clone.push((
                            next_hash_to_write, 
                            ChunkLocation {
                                offset,
                                length: data_len as u32,
                            }
                        ));
                        //Increment offset
                        offset += data_len;
                    } else {
                    //If the next chunk is not available, break loop.
                    break;
                }
            }
        }
        Ok(max_buffer_size)
    });

    //Monitor I/O thread for completion and monitor for errors.
    loop {
        if io_handle.is_finished() {
            break;
        }
        
        if let Ok(err) = err_rx.try_recv() {
            return Err(err);
        }
    }

    //Join all threads and propogate any errors.
    for handle in worker_handles {
        handle.join().expect("A compression worker thread panicked.");
    }

    //The final result is the result from our I/O thread, propogate if needed.
    io_handle.join().expect("The I/O thread panicked.")?;

    //Extract the Vec from the Arc<Mutex<>>
    let final_chunk_index = Arc::try_unwrap(chunk_index)
        .expect("Arc should be uniquely owned here")
        .into_inner()
        .expect("Mutex should not be poisoned");

    //println!("reordering_buffer reached {max_buffer_size} key-value pairs.");

    Ok(final_chunk_index)
}

impl<'a, H, R, W> ArchiveBuilder<'a, H, R, W>
where
    H: Copy + Debug + Eq + Hash + Serialize + Send + Sync + 'static + Display + Ord,
    R: Fn(&[H]) -> Vec<Vec<u8>> + Send + Sync + 'static,
    W: FnMut(&[u8], bool) + Send + Sync + 'static,
{
    pub fn new(
        ser_file_manifest: Vec<FileManifestParent<H>>,
        sorted_hashes: &'a [H],
        file_count: u32,
        hash_type: u8,
        total_size: u64,
        get_chunk_data: R,
        write_comp_data: W,
    ) -> Self {
        Self {
            ser_file_manifest,
            sorted_hashes,
            file_count,
            hash_type,
            total_size,
            get_chunk_data,
            write_comp_data,
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
    pub fn with_progress<F>(mut self, callback: F) -> Self 
    where
        F: Fn(Progress) + Sync + Send + 'static,
    {
        self.progress_callback = Some(Box::new(callback));
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
    /// - `Ok(Vec<u8>)` containing the complete binary data of the archive 
    ///   header.
    /// - `Err(LibError)` if any step fails, such as dictionary training,
    ///   compression, or serialization.
    pub fn build(self) -> Result<Vec<u8>, LibError> {
        //Destructure self into its fields
        let ArchiveBuilder {
            ser_file_manifest,
            sorted_hashes,
            file_count,
            hash_type,
            total_size,
            get_chunk_data,
            write_comp_data,
            compression_algorithm,
            compression_level,
            dictionary_size,
            worker_threads,
            opt_dict,
            progress_callback,
            c_progress_callback,
        } = self;

        let get_chunk_data_arc = Arc::new(get_chunk_data);
        let write_comp_data_arc = Arc::new(Mutex::new(write_comp_data));

        let progress_callback_arc = progress_callback.map(Arc::new);

        let report_progress = {
            let progress_callback_clone = progress_callback_arc.clone();
            move |progress: Progress| {
                if let Some(callback) = &progress_callback_clone {
                    callback(progress.clone());
                }
                if let Some((callback, user_data)) = c_progress_callback {
                    let ffi_progress = match progress {
                        Progress::GeneratingDictionary => FFIProgress { 
                            ty: FFIProgressType::GeneratingDictionary, 
                            total_chunks: 0 
                        },
                        Progress::DictionaryDone => FFIProgress { 
                            ty: FFIProgressType::DictionaryDone, 
                            total_chunks: 0 
                        },
                        Progress::Compressing {total_chunks} => FFIProgress {
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
        };

        //println!("Chunked data total size is: {}", self.total_size);
        
        let (samples_for_dict, sample_sizes) = build_train_samples(
            sorted_hashes, 
            total_size, 
            dictionary_size as usize,
            get_chunk_data_arc.as_ref()
        )?;

        //Make dictionary from sorted data.
        let mut _dictionary: Vec<u8> = Vec::new();

        //Report progress before starting a dictionary generation
        report_progress(Progress::GeneratingDictionary);
        
        if opt_dict{
            //println!("Generating optimized dictionary.");
            _dictionary = gen_zstd_opt_dict(
            &samples_for_dict, 
            &sample_sizes,
            dictionary_size as usize, 
            worker_threads, 
            compression_level)?;
        } else {
            //println!("Generating regular dictionary.");
            _dictionary = zstd::dict::from_continuous(
            &samples_for_dict,
            &sample_sizes,
            dictionary_size as usize, //Dictionary size in bytes
            ).map_err(|e| LibError::DictionaryError(e.to_string()))?;
        }

        //Samples are no longer needed.
        drop(samples_for_dict);
        drop(sample_sizes);

        //println!("Dictionary generated. Now compressing.");

        //Report progress after dictionary generation is done.
        report_progress(Progress::DictionaryDone);

        let get_chunk_data_cb = {
            move |hashes: &[H]| -> Vec<(H, Vec<u8>)> {
                let ret_chunks = (get_chunk_data_arc)(hashes);
                let mut chunk_pairs: Vec<(H, Vec<u8>)> = 
                    Vec::with_capacity(ret_chunks.len());

                for (index, chunk) in ret_chunks.iter().enumerate(){
                    chunk_pairs.push((hashes[index], chunk.clone()));
                };

                chunk_pairs
            }
        };

        let write_chunk_data_cb = {
            let progress_callback_clone = progress_callback_arc.clone();
            let write_comp_data_arc_clone = Arc::clone(&write_comp_data_arc);
            move |chunk: &[u8]| {
                let mut writer = write_comp_data_arc_clone.lock().unwrap();
                (*writer)(chunk, false);

                if let Some(callback) = &progress_callback_clone {
                    callback(Progress::ChunkCompressed);
                }
            }
            
        };

        report_progress(Progress::Compressing { 
            total_chunks: (
                sorted_hashes.len() as u64 
            )
        });
        
        let chunk_index = compress_chunks(
            worker_threads, 
            &_dictionary, 
            sorted_hashes, 
            compression_level, 
            get_chunk_data_cb, 
            write_chunk_data_cb
        )?;

        //Flush buffer to disk
        let empty_data: [u8; 0] = [];
        let mut writer = write_comp_data_arc.lock().unwrap();
        (*writer)(&empty_data, true);

        /*let task_pool = {
            let builder = rayon::ThreadPoolBuilder::new()
                .num_threads(worker_threads);
            
            builder.build()
                .map_err(|e| LibError::ThreadPoolError(
                    format!("Failed to create thread pool: {e}"))
                )?
        };*/
        /*
        let compressed_dash: DashMap<H, Vec<u8>> = DashMap::new();

        let comp_result: Result<(), LibError> = task_pool.install(|| {
            //Report that compression is starting
            /*self.report_progress(Progress::Compressing { 
                total_chunks: (
                    self.sorted_hashes.len() as u64 
                )
            });*/
            
            self.sorted_hashes.par_iter().try_for_each(|hash| {
                let data = (get_chunk_data_arc.as_ref())(&[*hash]);

                let compressed_chunk = compress_with_dict(
                    data[0].as_slice(), 
                    &_dictionary, 
                    &self.compression_level)
                    .map_err(|e| LibError::CompressionError(
                        e.to_string()
                    ))?;

                compressed_dash.insert(*hash, compressed_chunk);

                    //self.report_progress(Progress::ChunkCompressed);
                
                Ok(())
            })
        });

        //Check if any of the parallel operations failed.
        comp_result?;
        /*
        let (compressed_data_store, chunk_index) = 
            serialize_store(&compressed_dash, &self.sorted_hashes)?;*/
        */

        //drop(compressed_dash);

        /*println!("Serializing data");
        let (compressed_data_store, chunk_index) = 
            serialize_store_from_dashmap(compressed_dash, self.sorted_hashes)?;*/

        /*The following are now prepared:
        compressed_data store
        chunk_index
        dictionary*/

        let config = bincode::config::standard();

        //println!("Converting ser_file_manifest.");
        let bin_file_manifest = bincode::serde::encode_to_vec(
        &ser_file_manifest, config
        ).map_err(|e| LibError::ManifestEncodeError(e.to_string()))?;
        
        drop(ser_file_manifest);

        //println!("Converting chunk_index.");
        let bin_chunk_index = bincode::serde::encode_to_vec(
        &chunk_index, config
        ).map_err(|e| LibError::IndexEncodeError(e.to_string()))?;

        drop(chunk_index);

        //println!("Building file header.");
        //Build the file header
        let file_header = build_file_header(
            file_count,
            compression_algorithm,
            hash_type,
            bin_file_manifest.len() as u64,
            _dictionary.len() as u64,
            bin_chunk_index.len() as u64,
        );

        //let data_store_size = compressed_data_store.len();

        //let total_file_size = file_header.data_offset as usize + 
        //    data_store_size;
        
        //println!("Compressed data store is {} bytes.", data_store_size);

        //println!("Preparing to return final_data.");
        let mut final_data = Vec::with_capacity(
            file_header.data_offset as usize + bin_file_manifest.len() as usize + 
            _dictionary.len() + bin_chunk_index.len() as usize
        );

        final_data.extend_from_slice(file_header.as_bytes());
        final_data.extend_from_slice(&bin_file_manifest);
        final_data.extend_from_slice(&_dictionary);
        final_data.extend_from_slice(&bin_chunk_index);
        //final_data.extend_from_slice(&compressed_data_store);

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

/*
pub fn compress_chunks<H, R, W>(
    worker_count: usize,
    dictionary: &[u8],
    sorted_hashes: &[H],
    compression_level: i32,
    chunk_list_read_cb: R,
    chunk_write_cb: W,
) -> Result<Vec<(H, ChunkLocation)>, LibError>
where
    H: Copy + Debug + Eq + Hash + Send + Sync + 'static,
    R: Fn(&[H]) -> Vec<(H, Vec<u8>)> + Send + Sync + 'static,
    W: Fn(&[u8]) + Send + Sync + 'static,
{
    //Set lower and upper thresholds for prefetcher.
    const PREFETCH_LOW_THRESHOLD: usize = 100;
    const PREFETCH_HIGH_THRESHOLD: usize = 500;

    //Calculate total chunks from the amount of hashes.
    let total_chunks = sorted_hashes.len();

    //Create Arc for sorted hashes for safely sharing.
    let sorted_hashes_arc: Arc<[H]> = Arc::from(sorted_hashes);
    
    /*Create channels for sending/receiving to compressor info limited by
    PREFETCH_HIGH_THRESHOLD.
    This acts as the FIFO job buffer.*/
    let (to_compress_tx, to_compress_rx) = 
        flume::bounded::<(H, Vec<u8>)>(PREFETCH_HIGH_THRESHOLD);

    //Create channels for sending/receiving from compressor.
    let (from_compress_tx, from_compress_rx) = 
        flume::unbounded::<(H, Vec<u8>)>();

    //Error channels
    let (err_tx, err_rx) = std::sync::mpsc::channel::<LibError>();

    //Create arc for safely sharing dictionary.
    let dictionary_arc = Arc::new(dictionary.to_vec());

    //Create vector to contain all worker handles.
    let mut worker_handles = Vec::with_capacity(worker_count);

    let num_threads = if worker_count == 0 {
        std::thread::available_parallelism().unwrap().get()
    } else {
        worker_count
    };

    // Resolve the number of threads to use. If 0, use available parallelism.
    let num_workers = (num_threads).saturating_sub(1).max(1);

    //Spawn workers according to the specified worker count.
    for _ in 0..(num_workers) {
        //Clone necessary data to be used safely.
        let to_compress_rx_clone = to_compress_rx.clone();
        let from_compress_tx_clone = from_compress_tx.clone();
        let err_tx_clone = err_tx.clone();
        let dictionary_clone = Arc::clone(&dictionary_arc);

        //Create handle/thread for worker.
        let handle = thread::spawn(move || {
            /*Workers loop until the to_compress channel is empty and 
            disconnected.*/
            while let Ok((hash, data)) = to_compress_rx_clone.recv() {
                match compress_with_dict(
                    &data, 
                    &dictionary_clone, 
                    &compression_level
                ) {
                    Ok(compressed_data) => {
                        if from_compress_tx_clone.send((
                            hash, 
                            compressed_data
                        )).is_err() {
                            //The I/O thread has terminated; exit gracefully.
                            break;
                        }
                    }
                    Err(e) => {
                        //Report the error and stop processing.
                        let _ = err_tx_clone.send(e);
                        break;
                    }
                }
            }
        });
        worker_handles.push(handle);
    }
    //Drop the original receiver clone so the loop terminates correctly.
    drop(to_compress_rx);

    drop(from_compress_tx);

    //Clone sorted_hashes to be used safely.
    let sha_clone = Arc::clone(&sorted_hashes_arc);

    //Create chunk_index vector.
    //let mut chunk_index: Vec<(H, ChunkLocation)> = 
    //        Vec::with_capacity(sorted_hashes.len());
    let chunk_index = Arc::new(Mutex::new(Vec::with_capacity(total_chunks)));

    let chunk_index_clone = Arc::clone(&chunk_index);

    //Create handle/thread for IO coordinator.
    let io_handle = thread::spawn(move || -> Result<usize, LibError> {
        /*Tracks which chunks have been read from the source and sent to the 
        workers for compression. */
        let mut read_cursor = 0usize;

        /*Tracks which chunks have been written to the destination in the 
        correct, sequential order.*/
        let mut write_cursor = 0usize;

        /*Tracks the offset into the final data blob of the archive to be 
        stored by the chunk index.*/
        let mut offset = 0u64;
        
        /*Temporary buffer for holding compressed data if received out of 
        order.*/
        let mut reordering_buffer: HashMap<H, Vec<u8>> = HashMap::new();

        let mut max_buffer_size = 0usize;

        /*Loop that runs until all chunks have been sent to the host 
        application to be written.*/
        while write_cursor < total_chunks {
            
            //If the to_compress_tx job buffer is below the threshold.
            if to_compress_tx.len() < PREFETCH_LOW_THRESHOLD && 
                //and if the read_cursor is below total chunks
                read_cursor < total_chunks {
                    //Calculate how much chunk data to get.
                    let end = (read_cursor + PREFETCH_HIGH_THRESHOLD)
                        .min(total_chunks);
                    //Generate list of hashes to retrieve.
                    let hashes_to_read = &sha_clone[read_cursor..end];
                    
                    //Use callback to request and recceive chunk data.
                    /*match chunk_list_read_cb(hashes_to_read) {
                        //With a valid response
                        Ok(chunks) => {
                            //For each chunk
                            for chunk in chunks {
                                //Add hash and chunk data to job queue
                                if to_compress_tx.send(chunk).is_err() {
                                    //State if error occurred.
                                    return Err(LibError::InternalLibError(
                                        "Worker pool terminated unexpectedly."
                                        .to_string()));
                                }
                            }
                            //Set read_cursor to current index.
                            read_cursor = end;
                        }
                        Err(e) => return Err(e), //Catch all.
                    }*/

                    for chunk in chunk_list_read_cb(hashes_to_read) {
                        if to_compress_tx.send(chunk).is_err() {
                            //State if error occurred.
                            return Err(LibError::InternalLibError(
                                "Worker pool terminated unexpectedly."
                                .to_string()));
                        }
                        read_cursor = end;
                    }
            }

            //Match for receiving compressed data.
            match from_compress_rx.recv() {
                //For a received hash and compressed chunk data
                Ok((hash, compressed_data)) => {
                    //Add to reordering buffer.
                    reordering_buffer.insert(hash, compressed_data);
                    let buf_cap = reordering_buffer.capacity();
                    if max_buffer_size < buf_cap{
                        max_buffer_size = buf_cap;
                    };
                }
                //Else catch error.
                Err(_) => {
                    if write_cursor < total_chunks {
                         return Err(LibError::InternalLibError(
                            "Worker pool finished but not all chunks \
                            were processed.".to_string()));
                    }
                    break;
                }
            }

            //While write_cursor is less than total_chunks
            while write_cursor < total_chunks {
                //Set next hash to write
                let next_hash_to_write = sha_clone[write_cursor];

                //If there is data in the reordering buffer
                if let Some(data_to_write) = reordering_buffer
                    //And if the next hash is found, remove the key and data
                    .remove(&next_hash_to_write) {
                        //Calculate the length of the compressed data
                        let data_len = data_to_write.len() as u64;

                        let mut ci_clone = chunk_index_clone
                            .lock()
                            .unwrap();

                        /*Send the compressed data to be written by the host 
                        application*/
                        chunk_write_cb(&data_to_write);

                        //Incrment write cursor.
                        write_cursor += 1;

                        //Add chunk location data to chunk index.
                        ci_clone.push((
                            next_hash_to_write, 
                            ChunkLocation {
                                offset,
                                length: data_len as u32,
                            }
                        ));
                        //Increment offset
                        offset += data_len;
                    } else {
                    //If the next chunk is not available, break loop.
                    break;
                }
            }
        }
        Ok(max_buffer_size)
    });

    //Monitor I/O thread for completion and monitor for errors.
    loop {
        if io_handle.is_finished() {
            break;
        }
        
        if let Ok(err) = err_rx.try_recv() {

            return Err(err);
        }
    }

    //Join all threads and propogate any errors.
    for handle in worker_handles {
        handle.join().expect("A compression worker thread panicked.");
    }

    //The final result is the result from our I/O thread, propogate if needed.
    let max_buffer_size = io_handle.join().expect("The I/O thread panicked.")?;

    //Extract the Vec from the Arc<Mutex<>>
    let final_chunk_index = Arc::try_unwrap(chunk_index)
        .expect("Arc should be uniquely owned here")
        .into_inner()
        .expect("Mutex should not be poisoned");

    println!("reordering_buffer reached {max_buffer_size} key-value pairs.");

    Ok(final_chunk_index)
}*/