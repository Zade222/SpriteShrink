use std::{
    cmp::min, collections::HashMap,
    fmt::{Debug, Display},
    fs::{
        self, File, read_to_string
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

use bitcode::Encode;
use dashmap::DashMap;
use fastcdc::v2020::{StreamCDC, Normalization};
use flume::bounded;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use serde::{Serialize};
use sha2::{Digest};
use sprite_shrink::Hashable;
use tempfile::tempfile_in;
use indicatif::ProgressBar;
use tracing::{
    debug,
    info,
};
use sprite_shrink::{
    SS_SEED,
    ArchiveBuilder, ChunkLocation, HashType,
};
use sprite_shrink_cd::{
    NO_COLLECTION,
    ContentBlock, CueSheet, DataChunkLayout, DiscManifest,
    FileSizeProvider, ExceptionRegistry, MultiBinStream,
    ReconstructionError, SectorDataProvider, SectorRegionStream, SectorMap,
    SectorType, SpriteShrinkCDError, SSMDPrefix, SSMDTocEntry,
    SubheaderRegistry, UserDataStream,
    analyze_cue_sheets, analyze_and_map_disc, build_ssmd_prefix,
    compress_audio_blocks, disc_files, resolve_file_sector_counts,
    rle_encode_map,
    verify_disc_integrity
};
use xxhash_rust::xxh3::Xxh3;

use crate::{
    arg_handling::Args,
    cli_types::DiscCompleteData,
    progress::{
        AnalysisBars, CompressionProgress, byte_bar, compression_summary,
        count_bar, shows_progress
    },
    storage_io::{ExistingFile, parent_dir, write_archive},
};
use crate::cli_types::{
    CacheTarget, OpticalChunkMessage
};
use crate::error_handling::CliError;
use crate::storage_io::{
    TempCache, get_cache_paths, has_extension
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

/// A disc's BIN data that advances a progress bar as it's read.
///
/// The disc is read through several times (to hash it, to classify its
/// sectors and to chunk them), each counted on its own bar. Each read adds
/// exactly the disc's length to its bar, so the bars end at the total size
/// of the discs: bytes read past the length (a sector read twice) aren't
/// counted, and bytes a read skips are added when it ends.
struct CountingReader<R> {
    inner: R,
    /// The bar of the read in progress.
    bar: ProgressBar,
    /// Bytes added to `bar` by this read, at most `disc_len`.
    counted: u64,
    disc_len: u64,
}

impl<R> CountingReader<R> {
    fn new(inner: R, disc_len: u64) -> Self {
        Self { inner, bar: ProgressBar::hidden(), counted: 0, disc_len }
    }

    /// Ends the read in progress and counts the reads after it on `bar`.
    fn start_read(&mut self, bar: &ProgressBar) {
        self.finish_read();
        self.bar = bar.clone();
        self.counted = 0;
    }

    /// Ends the read in progress, adding what it skipped to its bar.
    fn finish_read(&mut self) {
        self.bar.inc(self.disc_len.saturating_sub(self.counted));
        self.counted = self.disc_len;
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        let counted = (read as u64).min(self.disc_len - self.counted);
        self.counted += counted;
        self.bar.inc(counted);
        Ok(read)
    }
}

impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}


pub(super) fn compress_optical_mode<H>(
    file_paths: Vec<PathBuf>,
    final_output_path: &Path,
    args: &Args,
    hash_type: HashType,
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

    let collection_map = playlist_groups(&file_paths)?;

    /*Sheets are decoded lossily. Ones saved as Shift-JIS or Windows-1252
    usually have non-ASCII text only in TITLE, PERFORMER or REM lines,
    which the parser drops. A FILE name that isn't UTF-8 decodes with
    U+FFFD and is then reported as a file that can't be found.*/
    let prepared_sheets = file_paths
        .into_iter()
        .filter(|path| has_extension(path, "cue"))
        .map(|path| {
            let bytes = fs::read(&path)
                .map_err(|err| CliError::CueRead(path.clone(), err))?;
            let content = String::from_utf8_lossy(&bytes).into_owned();
            Ok((path, content))
        })
        .collect::<Result<Vec<(PathBuf, String)>, CliError>>()?;

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

    let cache_info = get_cache_paths();

    let base_path = cache_info.cache_path.with_extension("");
    let stem = base_path.file_name().unwrap().to_str().unwrap();

    if prepared_sheets.is_empty() {
        return Err(CliError::NoCueFiles);
    }

    /*Each disc reads its BIN files from its own CUE file's folder.*/
    let discs = pair_with_cue_dirs(
        cue_analysis.cue_sheets,
        &prepared_sheets
    )?;


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

    /*Numerical compression level pulled from the args.*/
    let level: i32 = args.compression_level as i32;

    let process_threads = args.threads.map_or(0, |count| count);

    let window_size = args.window.map_or(
        8 * 1024, |byte| byte.as_u64());

    let dictionary_size = args.dictionary.map_or(
        128 * 1024, |byte| byte.as_u64());

    let show_progress = shows_progress(args);

    let analysis_bars = AnalysisBars::new(
        show_progress,
        cue_analysis.total_data_size
    );

    let proc_result = process_optical_discs::<H>(
        &discs,
        &data_temp_cache,
        &audio_temp_cache,
        &running,
        process_threads,
        window_size,
        &analysis_bars
    )?;
    analysis_bars.finish();

    let disc_manifiests = proc_result.manifests;
    veri_hashes = proc_result.veri_hashes;
    let subheader_table = proc_result.subheader_table;
    let exception_index = proc_result.exception_index;
    let exception_blob = proc_result.exception_blob;

    let audio_data_size = audio_temp_cache.get_tot_data_size();

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

        let collection_id = collection_map.get(&title)
            .copied()
            .unwrap_or(NO_COLLECTION);

        archive_toc.push(SSMDTocEntry {
            filename: title,
            collection_id,
            uncompressed_size: sector_count * 2352
        });

        disc_manifests.push(manifest);
    }

    let archive_toc_arc = Arc::new(&archive_toc);

    let verification_bar = byte_bar(
        show_progress,
        archive_toc.iter().map(|toc| toc.uncompressed_size).sum(),
        "Verifying discs..."
    );

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
            |bytes| verification_bar.inc(bytes),
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

    verification_bar.finish_with_message("Disc verification passed!");
    debug!("Disc verification passed!");

    //Compress audio
    let mut audio_block_index: Vec<(H, ChunkLocation)> = Vec::new();

    /*Holds the compressed audio and data until the prefix can be built.
    The file has no name, so the system deletes it however the run ends.*/
    let tmp_file = tempfile_in(parent_dir(final_output_path))?;

    let writer = Arc::new(
        Mutex::new(BufWriter::with_capacity(64 * 1024, tmp_file))
    );

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

        let audio_bar = count_bar(
            show_progress,
            audio_chunk_hashes.len() as u64,
            "Compressing audio blocks..."
        );

        /*Called once per compressed block, in order, so it also counts
        the blocks done.*/
        let tmp_audio_write_cb = {
            let blob_size_clone = Arc::clone(&audio_blob_size);
            let writer_clone = Arc::clone(&writer);
            let audio_bar = audio_bar.clone();
            move |data: &[u8]| -> Result<(), CliError>{
                blob_size_clone.fetch_add(data.len() as u64, Ordering::SeqCst);

                writer_clone.lock().unwrap().write_all(data)?;
                audio_bar.inc(1);
                Ok(())
            }
        };

        audio_block_index = compress_audio_blocks(
            &audio_chunk_hashes,
            get_audio_data_for_lib,
            tmp_audio_write_cb,
            process_threads
        )?;
        audio_bar.finish_with_message("Audio compression complete.");
    }

    let final_audio_blob_size = audio_blob_size.load(Ordering::SeqCst);

    let chunk_hashes = data_temp_cache.get_keys()?;
    let chunk_sum = data_temp_cache.get_tot_data_size();

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

    /*Writes each compressed chunk, in order. The builder only borrows the
    writer, so it's flushed below once the build is done.*/
    let tmp_chunk_write_cb = |chunk: &[u8]| -> Result<(), CliError> {
        writer.lock().unwrap().write_all(chunk)?;
        Ok(())
    };

    let mut comp_data_builder = ArchiveBuilder::new(
        &chunk_hashes,
        chunk_sum,
        get_chunk_data_for_lib,
        tmp_chunk_write_cb
    );

    let compression_progress = CompressionProgress::new(args);

    comp_data_builder.compression_level(level)
        .dictionary_size(dictionary_size)
        .optimize_dictionary(args.optimize_dictionary)
        .worker_threads(args.threads.unwrap_or(0));

    let comp_data = comp_data_builder
        .with_progress(compression_progress.callback())
        .build()?;
    compression_progress.finish();
    super::note_if_no_dictionary(&comp_data);

    /*Flush, then take a second handle to the file. It shares the first
    one's position, so write_archive's rewind applies to both.*/
    let mut tmp_file = {
        let mut writer = writer.lock().unwrap();
        writer.flush()?;
        writer.get_ref().try_clone()?
    };
    archive_toc.iter_mut().for_each(|toc|{
        toc.filename.push_str(".bin");
    });

    /*Builds every byte of the archive that comes before the compressed
    audio and data: header, TOC, format data, disc manifest, dictionary,
    indexes, subheader table and exception data.*/
    let final_data = build_ssmd_prefix(&SSMDPrefix {
        toc: &archive_toc,
        disc_manifests: &disc_manifests,
        comp: &comp_data,
        audio_block_index: &audio_block_index,
        exception_index: &exception_index,
        subheader_table: &subheader_table,
        exception_blob: &exception_blob,
        audio_blob_size: final_audio_blob_size,
        hash_type: hash_type.id(),
    })?;

    let existing = ExistingFile::from_force(args.force);
    write_archive(final_output_path, existing, &final_data, &mut tmp_file)?;

    let archive_size = fs::metadata(final_output_path)?.len();
    info!("{}", compression_summary(
        final_output_path,
        cue_analysis.total_data_size,
        archive_size
    ));

    Ok(())
}


/// Reads the M3U playlists among `file_paths` and returns the group each
/// listed disc belongs to, by CUE file stem.
///
/// Each playlist that lists at least one entry gets the next group ID, from
/// 0. Blank lines and `#` comments aren't entries. A playlist that can't be
/// read as UTF-8 is skipped.
///
/// # Errors
///
/// Returns `CliError::TooManyPlaylists` for a playlist with entries after
/// the 255th: the IDs left are taken by [`NO_COLLECTION`].
fn playlist_groups(
    file_paths: &[PathBuf],
) -> Result<HashMap<String, u8>, CliError> {
    let mut groups = HashMap::new();
    let mut next_id = 0u8;

    for path in file_paths {
        if (has_extension(path, "m3u") || has_extension(path, "m3u8")) &&
            let Ok(content) = read_to_string(path)
        {
            let stems: Vec<String> = content.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .filter_map(|line| Path::new(line).file_stem())
                .map(|stem| stem.to_string_lossy().into_owned())
                .collect();

            if stems.is_empty() {
                continue;
            }
            if next_id == NO_COLLECTION {
                return Err(CliError::TooManyPlaylists(path.clone()));
            }
            for stem in stems {
                groups.insert(stem, next_id);
            }
            next_id += 1;
        }
    }

    Ok(groups)
}

/// Pairs each parsed CUE sheet with the folder of the CUE file it was parsed
/// from. A sheet's `FILE` paths are relative to that folder.
///
/// `analyze_cue_sheets` returns one sheet per CUE file, in the order it was
/// given them, so `sheets[i]` came from `cue_files[i]`.
///
/// # Errors
///
/// Returns `CliError::InternalError` if the counts differ, so no disc is
/// dropped, or if a CUE path has no parent folder.
fn pair_with_cue_dirs(
    sheets: Vec<CueSheet>,
    cue_files: &[(PathBuf, String)],
) -> Result<Vec<(CueSheet, PathBuf)>, CliError> {
    if sheets.len() != cue_files.len() {
        return Err(CliError::InternalError(format!(
            "{} CUE sheets were parsed from {} CUE files",
            sheets.len(),
            cue_files.len()
        )));
    }

    sheets
        .into_iter()
        .zip(cue_files)
        .map(|(sheet, (cue_path, _))| {
            let cue_dir = cue_path.parent().ok_or_else(|| {
                CliError::InternalError(format!(
                    "Could not determine base directory from CUE path: {}",
                    cue_path.display()
                ))
            })?;
            Ok((sheet, cue_dir.to_path_buf()))
        })
        .collect()
}

/// `discs` pairs each CUE sheet with the folder its `FILE` paths are
/// relative to, as `pair_with_cue_dirs` returns them.
fn process_optical_discs<H>(
    discs: &[(CueSheet, PathBuf)],
    data_cache: &Arc<TempCache<H>>,
    audio_cache: &Arc<TempCache<H>>,
    running: &Arc<AtomicBool>,
    thread_count: usize,
    window_size: u64,
    bars: &AnalysisBars,
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

    let discs_owned = discs.to_owned();
    let worker_bars = bars.clone();

    let worker_handle = thread::spawn(move || -> Result<(), CliError>{
        worker_pool.install(|| -> Result<(), CliError> {
            discs_owned
                .par_iter()
                .try_for_each(|(cue_sheet, cue_dir)| -> Result<(), CliError> {
                    let sender_clone = chunk_sender.clone();
                    let dcd = cue_sheet_worker(
                        cue_sheet,
                        cue_dir,
                        window_size,
                        sender_clone,
                        &worker_subheader_reg,
                        &worker_exception_reg,
                        &worker_bars
                    )?;

                    worker_veri_hashes.insert(
                        dcd.title.clone(),
                        dcd.verification_hash
                    );

                    let dm = DiscManifest {
                        files: dcd.files,
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

    let final_disc_man = Arc::try_unwrap(shared_disc_manifest)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for disc manifest".to_string()
        ))?;
    let final_hashes = Arc::try_unwrap(shared_veri_hashes)
        .map_err(|_| CliError::InternalError(
            "Failed to unwrap Arc for verification hashes".to_string()
        ))?;

    // The discs registered subheaders and exceptions in thread order.
    // Sorting both tables and renumbering every disc against them makes the
    // archive the same on every run.
    let subheader_table = shared_subheader_registry.sorted_table()?;
    let exception_table = shared_exception_registry.sorted_table()?;

    for mut manifest in final_disc_man.iter_mut() {
        subheader_table.renumber(&mut manifest.subheader_index)?;
        exception_table.renumber(&mut manifest.disc_exception_index)?;
    }

    Ok(OptProcResult {
            manifests: final_disc_man,
            veri_hashes: final_hashes,
            exception_index: exception_table.index,
            exception_blob: exception_table.blob,
            subheader_table: subheader_table.entries
    })
}


fn cue_sheet_worker<H>(
    cue_sheet: &CueSheet,
    base_dir: &Path,
    window_size: u64,
    send_channel: flume::Sender<OpticalChunkMessage<H>>,
    subheader_registry: &SubheaderRegistry,
    exception_registry: &ExceptionRegistry,
    bars: &AnalysisBars,
) -> Result<DiscCompleteData<H>, CliError>
where
    H: Hashable + Send + Display
{
    let bin_paths: Vec<PathBuf> = cue_sheet.files
        .iter()
        .map(|f| base_dir.join(&f.name))
        .collect();
    let mut bin_stream: Box<dyn ReadSeekSend> = if bin_paths.len() == 1 {
        Box::new(File::open(&bin_paths[0])?)
    } else {
        //The library joins the parts; opening them is the application's job.
        let parts = bin_paths.iter().map(File::open).collect::<io::Result<_>>()?;
        Box::new(MultiBinStream::<File>::new(parts)?)
    };
    let disc_len = bin_stream.seek(SeekFrom::End(0))?;
    let mut source_stream = CountingReader::new(bin_stream, disc_len);

    let file_provider = AppFileSizeProvider {
        base_dir: base_dir.to_path_buf(),
    };

    let mut sha_hasher = sha2::Sha512::new();
    let mut xxh3_hasher = Xxh3::new();
    let mut buffer = [0u8; 65536];
    source_stream.seek(SeekFrom::Start(0))?;
    source_stream.start_read(&bars.hashing);
    loop {
        let count = source_stream.read(&mut buffer)?;
        if count == 0 { break; }
        sha_hasher.update(&buffer[..count]);
        xxh3_hasher.update(&buffer[..count]);
    }
    source_stream.seek(SeekFrom::Start(0))?;

    source_stream.start_read(&bars.classifying);
    let mut sector_provider = AppSectorDataProvider {
        source_stream: &mut source_stream,
    };

    let sector_count = resolve_file_sector_counts(
        cue_sheet,
        &file_provider
    )?;

    let files = disc_files(cue_sheet, &sector_count)
        .map_err(SpriteShrinkCDError::from)?;

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

    source_stream.start_read(&bars.chunking);

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
    source_stream.finish_read();

    //collection_id, exception_blob_offset and exception_index_length to be
    //  set elsewhere
    Ok(DiscCompleteData {
        title: cue_sheet.source_filename.clone(),
        files,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(title: &str) -> CueSheet {
        CueSheet { source_filename: title.to_string(), files: Vec::new() }
    }

    fn cue_file(path: &str) -> (PathBuf, String) {
        (PathBuf::from(path), String::new())
    }

    #[test]
    fn each_sheet_gets_its_own_cue_folder() {
        let discs = pair_with_cue_dirs(
            vec![sheet("one"), sheet("two")],
            &[cue_file("first/one.cue"), cue_file("second/two.cue")],
        )
        .unwrap();

        let paired: Vec<(&str, &Path)> = discs
            .iter()
            .map(|(sheet, dir)| (sheet.source_filename.as_str(), dir.as_path()))
            .collect();
        assert_eq!(
            paired,
            [("one", Path::new("first")), ("two", Path::new("second"))]
        );
    }

    #[test]
    fn a_count_mismatch_is_an_error() {
        let result = pair_with_cue_dirs(
            vec![sheet("one")],
            &[cue_file("first/one.cue"), cue_file("second/two.cue")],
        );
        assert!(matches!(result, Err(CliError::InternalError(_))));
    }

    /*Group IDs are a u8 and 255 means no group, so 255 playlists fit. The
    old code wrapped: the 256th got 255 and the 257th shared group 0.*/
    #[test]
    fn playlists_past_the_255th_are_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut paths = Vec::new();
        // Comments and blank lines aren't entries, so this takes no group.
        let comments = tmp.path().join("comments.m3u");
        fs::write(&comments, "#EXTM3U\n\n").unwrap();
        paths.push(comments);
        for set in 0..255 {
            let path = tmp.path().join(format!("set{set}.m3u"));
            fs::write(&path, format!("# set {set}\nset{set}.cue\n")).unwrap();
            paths.push(path);
        }

        let groups = playlist_groups(&paths).unwrap();
        assert_eq!(groups.len(), 255);
        for set in 0..255_u8 {
            assert_eq!(groups[&format!("set{set}")], set);
        }

        let extra = tmp.path().join("SET255.M3U8");
        fs::write(&extra, "set255.cue\n").unwrap();
        paths.push(extra.clone());
        assert!(matches!(
            playlist_groups(&paths),
            Err(CliError::TooManyPlaylists(path)) if path == extra
        ));
    }
}
