use std::{
    cmp::min,
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fmt::{Debug, Display},
    fs::{
        File, read_to_string
    },
    io::{
        self, Read, Seek, SeekFrom
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, atomic::{
        AtomicBool, Ordering,
    }},
    thread,
};

use bitcode::Encode;
use dashmap::DashMap;
use directories::ProjectDirs;
use fastcdc::v2020::{StreamCDC, Normalization};
use flume::bounded;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use serde::{Serialize};
use sha2::{Digest};
use sprite_shrink::Hashable;
use tracing::{
    debug,
};
use sprite_shrink::{
    SS_SEED, ArchiveBuilder, ChunkLocation, FileManifestParent,
    test_compression, dashmap_values_to_vec
};
use sprite_shrink_cd::{
    ContentBlock, CueSheet, DataChunkLayout, DiscManifest,
    FileSizeProvider, ExceptionInfo, MultiBinStream,
    SectorDataProvider, SectorRegionStream, SectorMap, SectorType,
    UserDataStream, analyze_cue_sheets, analyze_and_map_disc,
    compress_audio_blocks, resolve_file_sector_counts, rle_encode_map,
    verify_disc_integrity
};

use crate::{
    arg_handling::Args,
    cli_types::{DiscCompleteData, DiscExceptionBlob, TempFileGuard},
    storage_io::append_data_to_file,
};
use crate::cli_types::{
    APPIDENTIFIER, CacheTarget, OpticalChunkMessage
};
use crate::error_handling::CliError;
use crate::storage_io::{
    TempCache, get_cache_paths
};
use crate::utils::{process_in_memory_check, set_priority};

type CueProcessingResult<H> = (
    Vec<FileManifestParent<H>>,
    HashMap<String, [u8; 64]>
);

struct AppFileSizeProvider {
    base_dir: PathBuf,
}

struct AppSectorDataProvider<'a, R: io::Read + io::Seek> {
    source_stream: &'a mut R,
}

impl FileSizeProvider for AppFileSizeProvider {
    fn get_file_size_bytes(
        &self,
        filename: &str
    ) -> Result<u64, std::io::Error> {
        let full_path = self.base_dir.join(filename);

        let metadata = std::fs::metadata(full_path)?;
        Ok(metadata.len())
    }
}

impl<'a, R: io::Read + io::Seek> SectorDataProvider for AppSectorDataProvider<'a, R> {
    fn read_sector(
        &mut self,
        sector_index: u32
    ) -> Result<[u8; 2352], std::io::Error> {
        let start_byte = sector_index as u64 * 2352;

        self.source_stream.seek(SeekFrom::Start(start_byte))?;

        let mut sector_buffer = [0u8; 2352];

        self.source_stream.read_exact(&mut sector_buffer)?;

        Ok(sector_buffer)
    }
}

trait ReadSeekSend: io::Read + io::Seek + Send {}

impl<T: Read + Seek + Send> ReadSeekSend for T {}


pub(super) fn compress_optical_mode<H>(
    file_paths: Vec<PathBuf>,
    args: &Args,
    hash_type_id: &u8,
    running: Arc<AtomicBool>,
) -> Result<(), CliError>
where
    H:Hashable +
        Debug +
        Display +
        Encode +
        Ord +
        Serialize +
        for<'de> serde::Deserialize<'de>
{
    debug!("Running optical bin/cue compression mode.");

    set_priority()?;

    let prepared_sheets: Vec<(PathBuf, String)> = file_paths
        .into_iter()
        .filter(|path| path.extension() == Some(OsStr::new("cue")))
        .map(|path| {
            let content = read_to_string(&path).unwrap();
            (path, content)
        })
        .collect();

    let size_resolver = |path: &Path| -> Result<u64, io::Error> {
        let metadata = std::fs::metadata(path)?;
        Ok(metadata.len())
    };

    let cue_analysis = analyze_cue_sheets(
        prepared_sheets.as_slice(),
        size_resolver
    )?;

    let process_in_memory = if args.low_memory {
        false
    } else {
        process_in_memory_check(cue_analysis.total_data_size)
    };

    let proj_dirs = ProjectDirs::from(
        APPIDENTIFIER.qualifier,
        APPIDENTIFIER.organization,
        APPIDENTIFIER.application)
    .unwrap();

    let cache_info = get_cache_paths();

    let pid = cache_info.id;

    //Const for the max amount of chunks to cache before writing to disk.
    const BUFFER_SIZE: usize = 1000;

    let base_path = cache_info.cache_path.with_extension("");
    let stem = base_path.file_name().unwrap().to_str().unwrap();

    let first_cue_path = prepared_sheets.first().ok_or(
        CliError::InternalError("No input CUE files provided.".to_string())
    )?;

    let source_base_dir = first_cue_path.0.parent()
        .ok_or(CliError::InternalError(
            format!("Could not determine base directory from CUE path: {}", first_cue_path.0.display())
        ))?
        .to_path_buf();


    let audio_cache_path = base_path.with_file_name(
        format!("{}_audio.tmp", stem)
    );
    let data_cache_path = base_path.with_file_name(
        format!("{}_data.tmp", stem)
    );

    let audio_temp_cache = Arc::new(
        TempCache::<H>::new(audio_cache_path, process_in_memory)?
    );

    let data_temp_cache = Arc::new(
        TempCache::<H>::new(data_cache_path, process_in_memory)?
    );

    /*Stores the SHA-512 hash for each file.
    The String is the file name and the array is the 512 bit hash as a 64 byte
    array.*/
    let veri_hashes: DashMap<String, [u8; 64]>;

    //Used for storing overall file size.
    let mut data_sum = 0u64;

    /*Numerical compression level pulled from the args.*/
    let level: i32 = args.compression_level as i32;

    let process_threads = args.threads.map_or(0, |count| count);

    /*Create read_pool to specify the amount of threads to be used by the
    parallel process that follows it.*/
    let process_pool = {
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

    //autotune logic
    if args.auto_tune {
        if args.window.is_none() {

        }
    } //else { //Note: for now skip autotune logic to get the core logic sound.

    let temp_data = process_optical_discs::<H>(
        &cue_analysis.cue_sheets,
        &source_base_dir,
        &data_temp_cache,
        &audio_temp_cache,
        &running,
        process_threads,
        best_window_size
    )?;

    veri_hashes = temp_data.1;

    let exceptions = temp_data.2;

    let disc_manifiests = temp_data.0;

    let tot_exception_data: u64 = exceptions
        .iter()
        .map(|deb| deb.disc_blob.len() as u64)
        .sum();

    data_sum = data_temp_cache.get_tot_data_size() +
        audio_temp_cache.get_tot_data_size() +
        tot_exception_data;

    let audio_data_size = audio_temp_cache.get_tot_data_size();

    let total_input_size = cue_analysis.total_data_size;

    println!("Total input data size = {total_input_size}");

    println!("Reg data size = {}", audio_data_size);
    println!("Audio data size = {}", audio_temp_cache.get_tot_data_size());
    println!("Exception data size = {}", tot_exception_data);
    println!("Processed data size = {data_sum}");

    let all_data_hashes = collect_all_data_hashes(&disc_manifiests);

    let chunk_ret_cb = Arc::new({
        let cache_clone = Arc::clone(&data_temp_cache);
        let running_clone = Arc::clone(&running);
        move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
            if !running_clone.load(Ordering::SeqCst) {
                return Err(CliError::Cancelled);
            }

            cache_clone.get_chunks(hashes)
        }
    });

    let audio_ret_cb = Arc::new({
        let cache_clone = Arc::clone(&audio_temp_cache);
        let running_clone = Arc::clone(&running);
        move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
            if !running_clone.load(Ordering::SeqCst) {
                return Err(CliError::Cancelled);
            }

            cache_clone.get_chunks(hashes)
        }
    });

    /*let get_chunk_data_for_test = {
        let cb = Arc::clone(&chunk_ret_cb);
        move |hashes: &[H]| cb(hashes)
    };

    //}

    let compressed_size = test_compression(
        &all_data_hashes,
        data_temp_cache.get_tot_data_size(),
        0,
        131072,
        args.compression_level as i32,
        get_chunk_data_for_test
    )?;

    let size = tot_exception_data + compressed_size as u64;

    println!("Compressed data size = {compressed_size}");
    println!("Compressed data + exception data = {size}");*/

    let mut ser_man = dashmap_values_to_vec(&disc_manifiests);

    ser_man.sort_by(|a, b| a.title.cmp(&b.title));


    //This can be parallelized to accelerate the validation step.
    for manifest in ser_man {
        println!("Verifying {}", manifest.title);
        let arc_man = Arc::new(manifest);

        let hashes_entry = veri_hashes.get(&arc_man.title.clone())
            .ok_or_else(|| CliError::InternalError(format!(
            "Verification hash missing for file: {}",
            arc_man.title
        )))?;
        let veri_hash = *hashes_entry.value();

        let exceptions_entry = exceptions.get(&arc_man.title)
            .ok_or_else(|| CliError::InternalError(format!(
            "Exception blob missing for file: {}",
            arc_man.title
        )))?;
        let disc_exception_blob = exceptions_entry.value();

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

        let get_chunk_audio_for_verify = {
            let cb = Arc::clone(&audio_ret_cb);
            let verify_running_clone = Arc::clone(&running);
            move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
                if !verify_running_clone.load(Ordering::SeqCst) {
                    return Err(CliError::Cancelled);
                }
                cb(hashes)
            }
        };

        /*let original_bin_path = source_base_dir
            .join(&arc_man.title.clone())
            .with_extension("bin");

        let bin_paths: Vec<PathBuf> = cue_analysis.cue_sheets[0].files
            .iter()
            .map(|f| source_base_dir.join(&f.name))
            .collect();

        let mut source_stream: Box<dyn ReadSeekSend> = if bin_paths.len() == 1 {
            Box::new(File::open(&bin_paths[0])?)
        } else {
            Box::new(MultiBinStream::new(bin_paths)?)
        };*/

        verify_disc_integrity(
            arc_man,
            &disc_exception_blob.exception_index,
            &disc_exception_blob.disc_blob,
            &veri_hash,
            get_chunk_data_for_verify,
            get_chunk_audio_for_verify,
            //progress_cb
            //&mut source_stream
        )?;
    }

    println!("Verification passed!");

    //Compress audio
    let audio_block_index: Vec<(H, ChunkLocation)> = Vec::new();

    let audio_tmp_file_path = Arc::new(
        args.output
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("audio_{pid}"))
        .with_extension("tmp")
    );

    let _audio_tmp_guard = TempFileGuard::new(&audio_tmp_file_path);

    if audio_data_size > 0 {
        let audio_chunk_hashes = audio_temp_cache.get_keys()?;

        let mut audio_write_buffer: Vec<u8> = Vec::with_capacity(
            BUFFER_SIZE * 2352 * 16
        );

        let mut audio_block_count = 0usize;

        let get_audio_data_for_lib = {
            let cb = Arc::clone(&audio_ret_cb);
            let lib_running_clone = Arc::clone(&running);
            move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
                if !lib_running_clone.load(Ordering::SeqCst) {
                    return Err(CliError::Cancelled);
                }

                let ret_chunks = cb(hashes)?;

                Ok(ret_chunks)
            }
        };

        let tmp_audio_write_cb = {
            let tfp_clone = Arc::clone(&audio_tmp_file_path);
            move |chunk: &[u8], flush_flag: bool| -> Result<(), CliError>{
                audio_write_buffer.extend_from_slice(chunk);
                audio_block_count += 1;

                if (audio_block_count >= BUFFER_SIZE) || flush_flag {
                    append_data_to_file(&tfp_clone, &audio_write_buffer)?;
                    audio_write_buffer.clear();
                    audio_block_count = 0;
                };

                Ok(())
            }
        };

        println!("Audio block hash count = {}", audio_chunk_hashes.len());

        let audio_block_index = compress_audio_blocks(
            audio_chunk_hashes,
            get_audio_data_for_lib,
            tmp_audio_write_cb,
            process_threads
        )?;

        let audio_blob_size = audio_block_index.last().unwrap().1.compressed_length as u64 +
            audio_block_index.last().unwrap().1.offset;

        println!("Audio blob size = {}", audio_blob_size);
    }

    let chunk_hashes = data_temp_cache.get_keys()?;
    let chunk_sum = data_temp_cache.get_tot_data_size();

    /*Write buffer holds chunk byte data prior to being written.
    Vector made with capacity to hold 1000 chunks * the maximum window size
    fastcdc has used. (avg x 4)*/
    let mut write_buffer: Vec<u8> = Vec::with_capacity(
        BUFFER_SIZE * (best_window_size as usize * 4)
    );

    //Holds the amount of chunks that have been stored in the buffer.
    let mut chunk_count = 0usize;

    let chunk_tmp_file_path = Arc::new(
        args.output
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("chunk_{pid}"))
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
    let _tmp_guard = TempFileGuard::new(&chunk_tmp_file_path);

    /*Callback for storing and writing chunk data.
    Flush flag can be used to force the data to be written if the buffer has
    not reached the BUFFER_SIZE.*/
    let tmp_chunk_write_cb = {
        let tfp_clone = Arc::clone(&chunk_tmp_file_path);
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

    let mut comp_data_builder = ArchiveBuilder::new(
        &chunk_hashes,
        chunk_sum,
        get_chunk_data_for_lib,
        tmp_chunk_write_cb
    );

    comp_data_builder.compression_level(level)
        .dictionary_size(best_dictionary_size)
        .optimize_dictionary(args.optimize_dictionary)
        .worker_threads(args.threads.unwrap_or(0));

    let comp_data = comp_data_builder.build()?;

    println!("Dictionary size = {}", comp_data.dictionary_size);
    println!("Encoded Chunk Index size = {}", comp_data.enc_chunk_index_size);

    Ok(())
}


fn process_optical_discs<H>(
    cue_sheets: &[CueSheet],
    base_dir: &PathBuf,
    data_cache: &Arc<TempCache<H>>,
    audio_cache: &Arc<TempCache<H>>,
    running: &Arc<AtomicBool>,
    thread_count: usize,
    window_size: u64,
) -> Result<(
    DashMap<String, DiscManifest<H>>, //The list of manifests
    DashMap<String, [u8; 64]>, //Associated veri hashes
    DashMap<String, DiscExceptionBlob> //Final DiscExceptionBlob
), CliError>
where
    H: Hashable
        + Ord
        + Display
        + for<'de> serde::Deserialize<'de>
        + Send
        + 'static,
{
    const BATCH_SIZE: usize = 1000;

    let (chunk_sender, chunk_receiver) = flume::bounded::<
        OpticalChunkMessage<H>
    >(BATCH_SIZE);

    let (batch_sender, batch_receiver) = bounded::<(
        CacheTarget,
        Vec<(H, Vec<u8>)>
    )>(BATCH_SIZE);

    let data_cache_clone = data_cache.clone();
    let audio_cache_clone = audio_cache.clone();

    let cache_handle = thread::spawn(move || -> Result<(), CliError> {
        while let Ok((target, batch)) = batch_receiver.recv() {
            match target {
                CacheTarget::Data => data_cache_clone.insert_batch(&batch)?,
                CacheTarget::Audio => audio_cache_clone.insert_batch(&batch)?,
            }
        };

        Ok(())
    });

    let shared_disc_manifest: Arc<DashMap<
        String,
        DiscManifest<H>
    >> = Arc::new(DashMap::new());

    let shared_veri_hashes: Arc<
        DashMap<String,
        [u8; 64]
    >> = Arc::new(DashMap::new());

    /*let shared_exception_blob: Arc<Mutex<
        Vec<DiscExceptionBlob>
    >> = Arc::new(Mutex::new(Vec::new()));*/

    let shared_exception_blobs: Arc<
        DashMap<String,
        DiscExceptionBlob
    >> = Arc::new(DashMap::new());

    let system_thread_count = thread::available_parallelism()?.get();

    let task_thread_count = if thread_count != 0 {
        std::cmp::max(
            1,
            thread_count.saturating_sub(1)
        )
    } else {
        std::cmp::max(
            1,
            system_thread_count.saturating_sub(1)
        )
    };

    let worker_pool = {
        let builder = rayon::ThreadPoolBuilder::new()
            .num_threads(task_thread_count);

        builder.build()
            .map_err(|e| CliError::InternalError(
                format!("Failed to create thread pool: {e}")))?
    };

    let worker_file_manifest = shared_disc_manifest.clone();
    let worker_veri_hashes = shared_veri_hashes.clone();
    let worker_excep_blob = shared_exception_blobs.clone();

    let cue_sheets_owned = cue_sheets.to_owned();
    let base_dir_owned = base_dir.to_owned();

    let worker_handle = thread::spawn(move || -> Result<(), CliError>{
        worker_pool.install(|| -> Result<(), CliError> {
            cue_sheets_owned
                .par_iter()
                .try_for_each(|cue_sheet| -> Result<(), CliError> {
                    let sender_clone = chunk_sender.clone();
                    let dcd = cue_sheet_worker(
                        cue_sheet,
                        &base_dir_owned,
                        window_size,
                        sender_clone
                    )?;

                    worker_veri_hashes.insert(
                        dcd.title.clone(),
                        dcd.verification_hash
                    );

                    //collection_id, exception_blob_offset and
                    // exception_index_length to be set elsewhere
                    let dm = DiscManifest {
                        title: dcd.title.clone(),
                        collection_id: 255,
                        normalized_cue_sheet: dcd.normalized_cue_sheet,
                        lba_map: dcd.lba_map,
                        rle_sector_map: dcd.rle_sector_map,
                        block_map: dcd.block_map,
                        data_stream_layout: dcd.data_stream_layout,
                        exception_blob_offset: 0,
                        exception_index_length: 0,
                        subheader_map: dcd.subheader_map
                    };

                    let deb = DiscExceptionBlob {
                        title: dcd.title.clone(),
                        exception_index: dcd.exception_index,
                        disc_blob: dcd.exception_blob
                    };

                    worker_excep_blob.insert(dcd.title, deb);

                    //Possibly need to come up with a way of sorting everything
                    // by sector, if needed, for faster file decompression.

                    worker_file_manifest.insert(dm.title.clone(), dm);

                    Ok(())
                })?;
            Ok(())
        })?;
        Ok(())
    });

    let mut data_chunk_batch: Vec<(H, Vec<u8>)> = Vec::with_capacity(BATCH_SIZE);
    let mut audio_chunk_batch: Vec<(H, Vec<u8>)> = Vec::with_capacity(BATCH_SIZE);

    while let Ok(message) = chunk_receiver.recv() {
        if !running.load(Ordering::SeqCst) {
            //Early exit on cancellation
            return Err(CliError::Cancelled);
        }

        match message.cache_target {
            CacheTarget::Data => {
                data_chunk_batch.push((
                    message.chunk_hash,
                    message.chunk_data
                ));
            }
            CacheTarget::Audio => {
                audio_chunk_batch.push((
                    message.chunk_hash,
                    message.chunk_data
                ));
            }
        }

        if data_chunk_batch.len() >= BATCH_SIZE {
            batch_sender.send((
                CacheTarget::Data,
                std::mem::take(&mut data_chunk_batch
            ))).map_err(|e| CliError::FlumeSendError(e.to_string()))?;
            data_chunk_batch.reserve(BATCH_SIZE);
        }

        if audio_chunk_batch.len() >= BATCH_SIZE {
            batch_sender.send((
                CacheTarget::Audio,
                std::mem::take(&mut audio_chunk_batch
            ))).map_err(|e| CliError::FlumeSendError(e.to_string()))?;
            audio_chunk_batch.reserve(BATCH_SIZE);
        }

        //Need to implement progress logic. I'm thinking sector count would be
        // best.
    };

    if !data_chunk_batch.is_empty() {
        batch_sender.send((CacheTarget::Data, data_chunk_batch))
            .map_err(|e| CliError::FlumeSendError(e.to_string()))?;
    };

    if !audio_chunk_batch.is_empty() {
        batch_sender.send((CacheTarget::Audio, audio_chunk_batch))
            .map_err(|e| CliError::FlumeSendError(e.to_string()))?;
    };

    drop(batch_sender);
    cache_handle.join().expect("The cache writer thread panicked.")?;
    worker_handle.join().expect("A file chunk worker thread panicked.")?;

    //Finish progress bar here

    let final_disc_man = Arc::try_unwrap(shared_disc_manifest)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for disc manifest".to_string()
        ))?;
    let final_hashes = Arc::try_unwrap(shared_veri_hashes)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for verification hashes".to_string()
        ))?;

    let final_exception_blob = Arc::try_unwrap(shared_exception_blobs)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for exception blob".to_string()
        ))?;


    Ok((final_disc_man, final_hashes, final_exception_blob))
}


fn cue_sheet_worker<H>(
    cue_sheet: &CueSheet,
    base_dir: &Path,
    window_size: u64,
    send_channel: flume::Sender<OpticalChunkMessage<H>>
) -> Result<DiscCompleteData<H>, CliError>
where
    H: Hashable + Send + Display
{
    let bin_paths: Vec<PathBuf> = cue_sheet.files
        .iter()
        .map(|f| base_dir.join(&f.name))
        .collect();
    let mut source_stream: Box<dyn ReadSeekSend> = if bin_paths.len() == 1 {
        Box::new(File::open(&bin_paths[0])?)
    } else {
        Box::new(MultiBinStream::new(bin_paths)?)
    };

    let file_provider = AppFileSizeProvider {
        base_dir: base_dir.to_path_buf(),
    };

    let mut hasher = sha2::Sha512::new();
    let mut buffer = [0u8; 65536];
    source_stream.seek(SeekFrom::Start(0))?;
    loop {
        let count = source_stream.read(&mut buffer)?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
    }
    source_stream.seek(SeekFrom::Start(0))?;

    let mut sector_provider = AppSectorDataProvider {
        source_stream: &mut source_stream,
    };

    let sector_count = resolve_file_sector_counts(
        cue_sheet,
        &file_provider
    )?;

    let sector_result = analyze_and_map_disc(
        cue_sheet,
        &sector_count,
        &mut sector_provider
    )?;

    let sector_map = &sector_result.sector_map;

    let subheader_map = sector_result.subheader_map;

    //Personal note: I'm thinking that the building of the sector map,
    // processing of its result, the building of the exception index/blob and
    // generation of the rle_sector_map should happen before calling of the
    // process_optical_discs function. This will allow for more efficient
    // processing when autotune logic takes place. For now it will live here
    // but I think this is low hanging fruit for an optimization.
    let mut exception_index: HashMap<u64, ExceptionInfo> = HashMap::new();
    let mut exception_blob: Vec<u8> = Vec::new();
    let mut blob_offset = 0u32;

    for exception in sector_result.exception_metadata {
        let sector_num = exception.0;
        let exception_type = exception.1;
        let sector_metadata = exception.2;

        exception_blob.extend_from_slice(&sector_metadata);

        exception_index.insert(
            sector_num,
            ExceptionInfo {
                exception_type,
                data_offset: blob_offset
            }
        );
        blob_offset += exception_type.metadata_size()
    }

    let rle_sector_map = rle_encode_map(sector_map);

    let mut curr_sector_offset: u32 = 0;

    let mut data_stream_layout: Vec<DataChunkLayout<H>> = Vec::new();
    let mut block_map: Vec<ContentBlock<H>> = Vec::new();

    for (run_length, sector_type) in &rle_sector_map.runs {
        let mut run_stream = SectorRegionStream::new(
            &mut source_stream,
            curr_sector_offset,
            *run_length
        );

        match sector_type {
            SectorType::Mode1 | SectorType::Mode1Exception |
            SectorType::Mode2Form1 | SectorType::Mode2Form1Exception |
            SectorType::Mode2Form2 | SectorType::Mode2Form2Exception |
            SectorType::PregapMode1 | SectorType::PregapMode1Exception |
            SectorType::PregapMode2 | SectorType::PregapMode2Exception |
            SectorType::ZeroedData => {
                let run_map = SectorMap {
                    sectors: vec![*sector_type; *run_length as usize],
                };

                let user_data_stream = UserDataStream::new(
                    &mut run_stream,
                    &run_map
                );

                let stream_chunker = StreamCDC::with_level_and_seed(
                    user_data_stream,
                    (window_size / 4) as u32,
                    window_size as u32,
                    (window_size * 4) as u32,
                    Normalization::Level0,
                    SS_SEED
                );

                for result in stream_chunker {
                    let chunk = result?;

                    let chunk_hash = H::from_bytes_with_seed(
                        chunk.data.as_slice()
                    );

                    let message = OpticalChunkMessage::<H>{
                        cache_target: CacheTarget::Data,
                        chunk_hash,
                        chunk_data: chunk.data,
                        chunk_size: chunk.length
                    };

                    if send_channel.send(message).is_err() {
                        break;
                    }

                    let chunk_layout = DataChunkLayout {
                        hash: chunk_hash,
                        uncomp_len: chunk.length as u32,
                    };

                    data_stream_layout.push(chunk_layout);
                }
            }
            SectorType::Audio | SectorType::PregapAudio |
            SectorType::ZeroedAudio => {
                const SECTOR_BLOCK: u32 = 16;
                const BLOCK_SIZE_BYTES: usize = (SECTOR_BLOCK * 2352) as usize;

                let mut block_buffer = vec![0u8; BLOCK_SIZE_BYTES];
                let mut sectors_proc_in_run = 0u32;

                while sectors_proc_in_run < *run_length {
                    let sectors_to_read = min(
                        SECTOR_BLOCK,
                        *run_length - sectors_proc_in_run
                    );
                    let bytes_to_read = (sectors_to_read * 2352) as usize;

                    let curr_block_slice = &mut block_buffer[..bytes_to_read];

                    run_stream.read_exact(curr_block_slice)?;

                    let block_hash = H::from_bytes_with_seed(
                        curr_block_slice
                    );

                    let message = OpticalChunkMessage::<H> {
                        cache_target: CacheTarget::Audio,
                        chunk_hash: block_hash,
                        chunk_data: curr_block_slice.to_vec(),
                        chunk_size: bytes_to_read,
                    };

                    if send_channel.send(message).is_err() {
                        break;
                    }

                    let block_info = ContentBlock {
                        start_sector: curr_sector_offset + sectors_proc_in_run,
                        sector_count: sectors_to_read,
                        content_hash: block_hash,
                        sector_type: *sector_type,
                    };
                    block_map.push(block_info);

                    sectors_proc_in_run += sectors_to_read;
                }
            }
            _ => {
                //Ignore anything else.
            }
        }
        curr_sector_offset += run_length;
    }

    //collection_id, exception_blob_offset and exception_index_length to be
    //  set elsewhere
    Ok(DiscCompleteData {
        title: cue_sheet.source_filename.clone(),
        normalized_cue_sheet: sector_result.normalized_cue_sheet.to_string(),
        rle_sector_map,
        block_map,
        data_stream_layout,
        exception_index,
        exception_blob,
        subheader_map,
        verification_hash: hasher.finalize().into(),
        lba_map: sector_result.lba_map
    })
}


fn collect_all_data_hashes<H>(
       manifests: &DashMap<String, DiscManifest<H>>
   ) -> Vec<H>
   where
       H: Hashable + Eq + Copy,
   {
       let mut unique_hashes = HashSet::new();

       // Iterate through all the manifests in the DashMap
       for entry in manifests.iter() {
           let manifest = entry.value();

           // For each manifest, iterate through its data chunk layout
           for chunk_layout in &manifest.data_stream_layout {
               // Insert the hash into the HashSet.
               // HashSet automatically handles ensuring uniqueness.
               unique_hashes.insert(chunk_layout.hash);
           }
       }

       // Convert the HashSet of unique hashes into a Vec to return
       unique_hashes.into_iter().collect()
   }
