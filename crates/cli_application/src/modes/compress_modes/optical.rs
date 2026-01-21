use std::{
    cmp::min, collections::HashMap,
    ffi::OsStr,
    fmt::{Debug, Display},
    fs::{
        File, read_to_string
    },
    hash::Hasher, io::{
        self, BufWriter, Read, Seek, SeekFrom, Write
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, atomic::{
        AtomicBool, AtomicU64, Ordering,
        }
    },
    thread
};

use bitcode::{Encode, encode};
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
    SS_SEED,
    ArchiveBuilder, ChunkLocation, FileHeader,
    test_compression, dashmap_values_to_vec
};
use sprite_shrink_cd::{
    SSMD_UID,
    ContentBlock, CueSheet, DataChunkLayout, DiscManifest,
    FileSizeProvider, ExceptionRegistry, ExceptionInfo, MultiBinStream,
    ReconstructionError, SectorDataProvider, SectorRegionStream, SectorMap,
    SectorType, SpriteShrinkCDError, SSMDFormatData, SSMDTocEntry,
    SubheaderRegistry, UserDataStream,
    analyze_cue_sheets, analyze_and_map_disc, compress_audio_blocks,
    resolve_file_sector_counts, rle_encode_map, verify_disc_integrity
};
use xxhash_rust::xxh3::Xxh3;
use zerocopy::IntoBytes;

use crate::{
    arg_handling::Args,
    cli_types::{DiscCompleteData, TempFileGuard},
    storage_io::{append_data_to_file, write_final_archive},
};
use crate::cli_types::{
    APPIDENTIFIER, CacheTarget, OpticalChunkMessage
};
use crate::error_handling::CliError;
use crate::storage_io::{
    TempCache, get_cache_paths
};
use crate::utils::{process_in_memory_check, set_priority};

struct OptProcResult<H> {
    manifests: DashMap<String, DiscManifest<H>>,
    veri_hashes: DashMap<String, [u8; 64]>,
    exception_index: Vec<u64>,
    exception_blob: Vec<u8>,
    subheader_table: Vec<[u8; 8]>
}

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

    let mut collection_map: HashMap<String, u8> = HashMap::new();

    let mut next_collection_id = 0u8;

    for path in &file_paths {
        if let Some(ext) = path.extension() &&
            (ext == "m3u" || ext == "m3u8") &&
            let Ok(content) = read_to_string(path)
        {
            let mut found_any = false;
            for line in content.lines() {
                let line = line.trim();

                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                let entry_path = Path::new(line);

                if let Some(stem) = entry_path.file_stem() {
                    collection_map.insert(
                        stem.to_string_lossy().to_string(),
                        next_collection_id
                    );
                    found_any = true;
                }
            }

            if found_any {
                if next_collection_id == 255 {
                    debug!("Max collection count reached, wrapping ids");
                }
                next_collection_id = next_collection_id.wrapping_add(1);
            }
        }
    }

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
    /*if args.auto_tune {
        if args.window.is_none() {

        }
    } //else { //Note: for now skip autotune logic to get the core logic sound.*/

    let proc_result = process_optical_discs::<H>(
        &cue_analysis.cue_sheets,
        &source_base_dir,
        &data_temp_cache,
        &audio_temp_cache,
        &running,
        process_threads,
        best_window_size
    )?;

    let disc_manifiests = proc_result.manifests;
    veri_hashes = proc_result.veri_hashes;
    let subheader_table = proc_result.subheader_table;
    let exception_index = proc_result.exception_index;
    let exception_blob = proc_result.exception_blob;

    let audio_data_size = audio_temp_cache.get_tot_data_size();

    let total_input_size = cue_analysis.total_data_size;

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

    let mut entries: Vec<(String, DiscManifest<H>)> = disc_manifiests
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect();

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut archive_toc = Vec::with_capacity(entries.len());
    let mut disc_manifests = Vec::with_capacity(entries.len());

    for (title, manifest) in entries {
        let sector_count: u64 = manifest.rle_sector_map.runs.iter()
            .map(|(run, _)| *run as u64)
            .sum();

        let collection_id = collection_map.get(&title).copied().unwrap_or(255);

        archive_toc.push(SSMDTocEntry {
            filename: title,
            collection_id,
            uncompressed_size: sector_count * 2352
        });

        disc_manifests.push(manifest);
    }

    let archive_toc_arc = Arc::new(&archive_toc);

    //This can be parallelized to accelerate the validation step.
    for (i, manifest) in disc_manifests.iter().enumerate() {
        let archive_toc_clone = archive_toc_arc.clone();

        let hashes_entry = veri_hashes
            .get(&archive_toc_clone[i].filename.clone())
            .ok_or_else(|| CliError::InternalError(format!(
            "Verification hash missing for file: {}",
            archive_toc_clone[i].filename.clone()
        )))?;
        let veri_hash = *hashes_entry.value();

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

        let result = verify_disc_integrity(
            manifest,
            &exception_index,
            &exception_blob,
            &subheader_table,
            &veri_hash,
            get_chunk_data_for_verify,
            get_chunk_audio_for_verify,
            //progress_cb
        );

        match result {
            Ok(_) => {},
            Err(SpriteShrinkCDError::Reconstruction(
                ReconstructionError::HashMismatchError { orig_hash, calc_hash }
            )) => {
                eprintln!(
                    "Integrity check failed for {}",
                    archive_toc_clone[i].filename
                );
                eprintln!("Expected: {}", orig_hash);
                eprintln!("Calculated: {}", calc_hash);

                return Err(CliError::InternalError(
                    "Disc verification failed".to_string()
                ));
            },
            Err(e) => return Err(CliError::from(e)),
        }
    }

    println!("Verification passed!");

    //Compress audio
    let mut audio_block_index: Vec<(H, ChunkLocation)> = Vec::new();

    let tmp_file_path = Arc::new(
        args.output
        .as_ref()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("{pid}"))
        .with_extension("tmp")
    );

    let tmp_file = File::create(tmp_file_path.as_path())?;

    let writer = Arc::new(
        Mutex::new(BufWriter::with_capacity(64 * 1024, tmp_file))
    );

    /*Set file guard to clean up tmp files on error. */
    let _tmp_guard = TempFileGuard::new(&tmp_file_path);
    let audio_blob_size = Arc::new(AtomicU64::new(0));

    if audio_data_size > 0 {
        let audio_chunk_hashes = audio_temp_cache.get_keys()?;

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
            let blob_size_clone = Arc::clone(&audio_blob_size);
            let writer_clone = Arc::clone(&writer);
            move |data: &[u8], flush_flag: bool| -> Result<(), CliError>{
                blob_size_clone.fetch_add(data.len() as u64, Ordering::SeqCst);

                writer_clone.lock().unwrap().write_all(data)?;
                Ok(())
            }
        };

        audio_block_index = compress_audio_blocks(
            audio_chunk_hashes,
            get_audio_data_for_lib,
            tmp_audio_write_cb,
            process_threads
        )?;
    }

    let final_audio_blob_size = audio_blob_size.load(Ordering::SeqCst);

    let chunk_hashes = data_temp_cache.get_keys()?;
    let chunk_sum = data_temp_cache.get_tot_data_size();

    /*Write buffer holds chunk byte data prior to being written.
    Vector made with capacity to hold 1000 chunks * the maximum window size
    fastcdc has used. (avg x 4)*/
    let mut write_buffer: Vec<u8> = Vec::with_capacity(
        BUFFER_SIZE * (best_window_size as usize * 4)
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

    /*Callback for storing and writing chunk data.
    Flush flag can be used to force the data to be written if the buffer has
    not reached the BUFFER_SIZE.*/
    let tmp_chunk_write_cb = {
        let writer_clone = Arc::clone(&writer);
        move |chunk: &[u8], flush_flag: bool| -> Result<(), CliError>{
            write_buffer.extend_from_slice(chunk);

            writer_clone.lock().unwrap().write_all(chunk)?;
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

    writer.lock().unwrap().flush()?;
    let blob_size = _tmp_guard.size();

    archive_toc.iter_mut().for_each(|toc|{
        toc.filename.push_str(".bin");
    });

    let enc_toc = encode(&archive_toc);

    let enc_disc_manifest = encode(&disc_manifests);

    let enc_audio_index = if !audio_block_index.is_empty() {
        encode(&audio_block_index)
    } else {
        vec![0u8; 0]
    };

    let enc_excep_index = encode(&exception_index);

    let file_header = FileHeader::build_file_header(
        archive_toc.len() as u32,
        98, //zstd
        *hash_type_id,
        SSMD_UID,
        enc_toc.len() as u32
    );

    let format_data = SSMDFormatData::build_format_data(
        FileHeader::HEADER_SIZE as usize + enc_toc.len(),
        enc_disc_manifest.len(),
        comp_data.dictionary_size,
        comp_data.enc_chunk_index_size,
        enc_audio_index.len() as u64,
        enc_excep_index.len() as u64,
        subheader_table.len() as u64 * 8 ,
        exception_blob.len() as u64,
        final_audio_blob_size
    );

    let mut final_data = Vec::with_capacity(
        FileHeader::HEADER_SIZE as usize + enc_toc.len() +
            SSMDFormatData::SIZE as usize + enc_disc_manifest.len() +
            comp_data.dictionary_size as usize +
            comp_data.enc_chunk_index_size as usize +
            subheader_table.len()
    );

    final_data.extend_from_slice(file_header.as_bytes());
    final_data.extend_from_slice(&enc_toc);
    final_data.extend_from_slice(format_data.as_bytes());
    final_data.extend_from_slice(&enc_disc_manifest);
    final_data.extend_from_slice(&comp_data.dictionary);
    final_data.extend_from_slice(&comp_data.enc_chunk_index);
    if !audio_block_index.is_empty() {
        final_data.extend_from_slice(&enc_audio_index);
    }
    final_data.extend_from_slice(&enc_excep_index);
    final_data.extend_from_slice(subheader_table.as_bytes());
    final_data.extend_from_slice(&exception_blob);

    println!("Estimated final data size with blob: {}", final_data.len() + blob_size as usize);

    let final_output_path = args.output
        .as_ref()
        .unwrap()
        .with_extension("ssmd");

    write_final_archive(
        &final_output_path,
        &tmp_file_path,
        &final_data
    )?;

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
) -> Result<OptProcResult<H>, CliError>
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

    let shared_subheader_registry = Arc::new(SubheaderRegistry::default());

    let shared_exception_registry = Arc::new(ExceptionRegistry::default());

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
    let worker_subheader_reg = shared_subheader_registry.clone();
    let worker_exception_reg = shared_exception_registry.clone();

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
                        sender_clone,
                        &worker_subheader_reg,
                        &worker_exception_reg
                    )?;

                    worker_veri_hashes.insert(
                        dcd.title.clone(),
                        dcd.verification_hash
                    );

                    let dm = DiscManifest {
                        lba_map: dcd.lba_map,
                        rle_sector_map: dcd.rle_sector_map,
                        audio_block_map: dcd.audio_block_map,
                        data_stream_layout: dcd.data_stream_layout,
                        subheader_index: dcd.subheader_index,
                        disc_exception_index: dcd.disc_exception_index,
                        integrity_hash: dcd.integrity_hash,
                    };

                    //Possibly need to come up with a way of sorting everything
                    // by sector, if needed, for faster file decompression.

                    worker_file_manifest.insert(dcd.title.clone(), dm);

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

    let subheader_table = shared_subheader_registry.generate_blob();

    let (exception_blob, exception_index) = shared_exception_registry
        .generate_blob_and_index();

    Ok(OptProcResult {
            manifests: final_disc_man,
            veri_hashes: final_hashes,
            exception_index,
            exception_blob,
            subheader_table
    })
}


fn cue_sheet_worker<H>(
    cue_sheet: &CueSheet,
    base_dir: &Path,
    window_size: u64,
    send_channel: flume::Sender<OpticalChunkMessage<H>>,
    subheader_registry: &SubheaderRegistry,
    exception_registry: &ExceptionRegistry
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

    let mut sha_hasher = sha2::Sha512::new();
    let mut xxh3_hasher = Xxh3::new();
    let mut buffer = [0u8; 65536];
    source_stream.seek(SeekFrom::Start(0))?;
    loop {
        let count = source_stream.read(&mut buffer)?;
        if count == 0 { break; }
        sha_hasher.update(&buffer[..count]);
        xxh3_hasher.update(&buffer[..count]);
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
        &mut sector_provider,
        subheader_registry,
        exception_registry
    )?;

    let sector_map = &sector_result.sector_map;

    let subheader_index = sector_result.subheader_index;

    let disc_exception_index = sector_result.exception_index;

    //Personal note: I'm thinking that the building of the sector map,
    // processing of its result, the building of the exception index/blob and
    // generation of the rle_sector_map should happen before calling of the
    // process_optical_discs function. This will allow for more efficient
    // processing when autotune logic takes place. For now it will live here
    // but I think this is low hanging fruit for an optimization.
    let rle_sector_map = rle_encode_map(sector_map);

    let mut curr_sector_offset: u32 = 0;

    let mut data_stream_layout: Vec<DataChunkLayout<H>> = Vec::new();
    let mut audio_block_map: Vec<ContentBlock<H>> = Vec::new();

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
            SectorType::ZeroedMode1Data | SectorType::ZeroedMode2Data => {
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
                    audio_block_map.push(block_info);

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
        lba_map: sector_result.lba_map,
        rle_sector_map,
        audio_block_map,
        data_stream_layout,
        disc_exception_index,
        subheader_index,
        verification_hash: sha_hasher.finalize().into(),
        integrity_hash: xxh3_hasher.finish()
    })
}
