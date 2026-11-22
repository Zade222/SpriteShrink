//! Handles the final assembly and serialization of the archive file.
//!
//! This module is responsible for taking all processed data and metadata
//! to construct the final, portable archive. It orchestrates the entire
//! process, from building the file header and training a compression
//! dictionary to compressing data chunks and serializing the complete
//! archive into a single byte vector ready for storage.

use std::{
    borrow::Cow,
    {collections::{
        VecDeque},
    },
    fmt::{Debug, Display},
    hash::Hash,
    io::{self, Read},
    marker::PhantomData,
    os::raw::c_void,
    thread
};

use bitcode::{
    Encode, encode
};
use thiserror::Error;
use serde::Serialize;


use crate::{IsCancelled, SpriteShrinkError,
    ffi::ffi_types::{
    FFIProgress, FFIProgressType, FFIUserData
    },
    lib_structs::{
        ChunkLocation, CompressionResult, Progress, SSAChunkMeta,
    }
};

use crate::processing::{
    MAX_CHUNK_LEN,
    Hashable, ProcessingError,
    build_train_samples, gen_zstd_opt_dict,
};

#[derive(Error, Debug)]
pub enum ArchiveError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("Compression failed: {0}")]
    CompressionError(String),

    #[error("Dictionary creation failed: {0}")]
    Dictionary(String),

    #[error("Thread pool creation failed: {0}")]
    ThreadPool(String),

    #[error("Failed to encode file manifest: {0}")]
    ManifestEncodeError(String),

    #[error("Worker error: {0}")]
    WorkerError(String),

    #[error("Failed to encode chunk index: {0}")]
    IndexEncodeError(String),

    #[error("A data processing step failed: {0}")]
    ProcessingError(#[from] ProcessingError),

    #[error("Zstd error: {0}")]
    ZstdError(String),

    #[error("Thread pool creation failed: {0}")]
    ThreadPoolError(String),

    #[error("An error occurred in an external callback: {0}")]
    External(String),

    #[error("Chunk failed its integrity check (expected hash {expected}, \
        got {actual}). The archive may be damaged.")]
    ChunkHashMismatch { expected: String, actual: String },

    #[error("Chunk decodes to more than the {limit} bytes allowed. The \
        archive may be damaged.")]
    ChunkTooLong { limit: usize },

    #[error("Chunk decodes to {actual} bytes, not the {expected} recorded \
        for it. The archive may be damaged.")]
    ChunkTooShort { expected: usize, actual: usize },

    #[error("Operation cancelled by user.")]
    Cancelled,
}

unsafe impl Send for FFIUserData {}
unsafe impl Sync for FFIUserData {}

//C style progress callback function pointer
type CProgressCallback = extern "C" fn(FFIProgress, *mut c_void);

pub struct ArchiveBuilder<'a, E, H, R, W>
where
    H: Clone,
{
    //Required parameters
    /// The chunks to compress, in write order. Borrowed from the caller, or
    /// owned when the caller had to build the list (the u128 FFI decodes it
    /// from bytes).
    ///
    /// Only ever read. Calling `to_mut` or `into_owned` on a borrowed list
    /// copies every hash, 8 or 16 bytes per unique chunk, and keeps the copy
    /// for the whole build. Take `&sorted_hashes` instead.
    sorted_hashes: Cow<'a, [H]>,
    total_size: u64,
    get_chunk_data: R,
    write_comp_data: W,

    //Optional parameters, will be set with default values.
    compression_level: i32,
    dictionary_size: u64,
    worker_threads: usize,
    opt_dict: bool,
    progress_callback: Option<Box<dyn Fn(Progress) + Sync + Send>>,
    c_progress_callback: Option<(CProgressCallback, FFIUserData)>,

    //Add this field
    _error_type: PhantomData<E>,
}

/// Creates the zstd compressor that chunks are compressed with.
///
/// The dictionary is loaded once per compressor, so one compressor should
/// be reused for many chunks. Loading the dictionary again for every chunk
/// took most of the time at low levels.
///
/// Frames leave out the decompressed size: the file manifest already
/// records each chunk's length.
///
/// # Errors
///
/// Returns `ArchiveError::Dictionary` if zstd can't use `dictionary`. An
/// empty dictionary is accepted and means no dictionary.
fn chunk_compressor(
    compression_level: i32,
    dictionary: &[u8],
) -> Result<zstd::bulk::Compressor<'static>, ArchiveError> {
    let rejected =
        |e: io::Error| ArchiveError::Dictionary(format!("zstd rejected it: {e}"));

    let mut compressor =
        zstd::bulk::Compressor::with_dictionary(compression_level, dictionary)
            .map_err(rejected)?;
    compressor.include_contentsize(false)?;

    /*zstd only copies the dictionary here and parses it when it compresses
    the first frame. A corrupt dictionary would then fail the first chunk,
    with an error that reads "Allocation error : not enough memory".
    Compressing an empty frame parses it now, so the error names the
    dictionary. The parsed dictionary is kept for later frames.*/
    compressor.compress(&[]).map_err(rejected)?;
    Ok(compressor)
}

/// The most chunks read per batch.
const PREFETCH_HIGH_THRESHOLD: usize = 500;

/// The most chunks handed to a decompression worker in one message. A
/// worker that has run out of work sleeps, and waking it costs a few
/// microseconds, about what decoding a 2 KiB chunk takes; a run of chunks
/// is one wake-up for all of them. Compression hands chunks over one at a
/// time: compressing one takes far longer than a wake-up, and single chunks
/// spread the work more evenly.
const DECODE_RUN_LEN: usize = 64;

/// A run ends once its data reaches this many bytes, so large items (as
/// compression with a large window has) still spread across the workers.
const RUN_BYTE_TARGET: usize = 128 * 1024;

/// A run of consecutive items for a worker: the first one's position in
/// `items`, and each one's data. A worker returns its results the same way.
type Run = (usize, Vec<Vec<u8>>);

/// How the coordinator splits a batch into runs for the workers.
#[derive(Clone, Copy)]
struct RunSplit {
    workers: usize,
    max_len: usize,
}

impl RunSplit {
    /// The most items per run for a batch of `batch` items: at most
    /// `max_len`, short enough that every worker gets one, and at least 1.
    fn len(self, batch: usize) -> usize {
        batch.div_ceil(self.workers).min(self.max_len).max(1)
    }
}

/// The most chunks the coordinator holds at once, from the next one to
/// write to the last one read: queued, being worked on, or done and
/// waiting for an earlier chunk. Two batches, so one can be read while the
/// one before is still being written.
const REORDER_WINDOW: usize = 2 * PREFETCH_HIGH_THRESHOLD;

/// The most chunk data the coordinator plans to hold at once, read or
/// produced, across every chunk in the window. A chunk larger than this
/// is read on its own once the window is empty.
///
/// At the default 2 KiB window, `REORDER_WINDOW` chunks are a few MiB, so
/// the count is the limit. With large chunks (up to 16 MiB), this is.
const PIPELINE_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// Maps an error from one of the pipeline's callbacks to an
/// `ArchiveError`, keeping cancellation distinct.
fn callback_error<E: std::error::Error + IsCancelled>(e: &E) -> ArchiveError {
    if e.is_cancelled() {
        ArchiveError::Cancelled
    } else {
        ArchiveError::External(e.to_string())
    }
}

/// Compresses chunks with Zstandard, in parallel, for an archive.
///
/// A wrapper around `compress_pipeline` that gives each worker a Zstandard
/// compressor with `dictionary` loaded.
///
/// # Arguments
///
/// * `worker_count`: The number of worker threads to compress with. `0`
///   uses the available parallelism.
/// * `dictionary`: The Zstandard dictionary to compress with. It may be
///   empty.
/// * `sorted_hashes`: The chunks to compress, in the order to write them.
/// * `compression_level`: The Zstandard compression level for the workers.
/// * `chunk_list_read_cb`: Reads the uncompressed data for a set of hashes.
/// * `chunk_write_cb`: Writes the ordered, compressed chunk data.
///
/// # Errors
///
/// - `ArchiveError::Dictionary` if zstd can't use `dictionary`. This is
///   checked before any chunk is read.
/// - Any error from `compress_pipeline`, or from compressing a chunk.
pub(crate) fn compress_chunks<ERead, EWrite, H, R, W>(
    worker_count: usize,
    dictionary: &[u8],
    sorted_hashes: &[H],
    compression_level: i32,
    chunk_list_read_cb: R,
    chunk_write_cb: W,
) -> Result<Vec<(H, ChunkLocation)>, ArchiveError>
where
    ERead: std::error::Error + IsCancelled,
    EWrite: std::error::Error + IsCancelled,
    H: Copy,
    R: Fn(&[H]) -> Result<Vec<Vec<u8>>, ERead>,
    W: FnMut(&[u8]) -> Result<(), EWrite>,
{
    /*Each worker gets its own compressor, so the dictionary is loaded once
    per worker rather than once per chunk.*/
    let new_compressor = || {
        let mut compressor = chunk_compressor(compression_level, dictionary)?;
        Ok(move |data: &[u8]| {
            compressor.compress(data).map_err(ArchiveError::from)
        })
    };

    compress_pipeline(
        worker_count,
        sorted_hashes,
        new_compressor,
        chunk_list_read_cb,
        chunk_write_cb,
    )
}

/// Compresses chunks in parallel with a caller-supplied compressor, and
/// writes them in order.
///
/// This is the pipeline `ArchiveBuilder` compresses with, for hosts that
/// compress some chunks with another codec (e.g. FLAC for CD audio). It
/// reads chunks in batches through `chunk_list_read_cb`, compresses them on
/// a pool of worker threads, and writes the results through
/// `chunk_write_cb` in `sorted_hashes` order.
///
/// # Type Parameters
///
/// * `F`: Makes a compressor. It's called once per worker thread, so a
///   compressor can keep state (e.g. a loaded dictionary) between chunks.
/// * `C`: A compressor: takes a chunk's data and returns it compressed.
/// * `R`: The read callback. Given a slice of hashes, it returns the
///   uncompressed data of each, in the same order.
/// * `W`: The write callback, called once per chunk, in order, with the
///   compressed bytes.
///
/// # Arguments
///
/// * `worker_count`: The number of worker threads to compress with. `0`
///   uses the available parallelism. The calling thread coordinates
///   besides these, and mostly waits.
/// * `sorted_hashes`: The chunks to compress, in the order to write them. A
///   hash that appears twice is compressed and written twice.
/// * `new_compressor`: Makes each worker's compressor.
/// * `chunk_list_read_cb`: Reads the uncompressed data for a set of hashes.
/// * `chunk_write_cb`: Writes the ordered, compressed chunk data to its
///   final destination (e.g., a file or an in-memory buffer).
///
/// # Returns
///
/// The chunk index: each hash of `sorted_hashes`, in order, with the offset
/// and length of its compressed data in the written output.
///
/// # Errors
///
/// - `SpriteShrinkError::Cancelled` if either callback reports a
///   cancellation.
/// - `SpriteShrinkError::Archive` for every other error, which includes:
///   an error from either callback; a read callback that returns a
///   different number of chunks than it was asked for; an error from
///   `new_compressor` or a compressor (its message is kept); a worker
///   thread that panics; a compressed chunk of 4 GiB or more, which the
///   chunk index can't record; and a `worker_count` of 0 when the available
///   parallelism can't be determined.
///
/// `new_compressor` is called once before any chunk is read, so a
/// compressor that can't be made fails before the callbacks are used.
pub fn compress_chunks_with<C, EComp, ERead, EWrite, F, H, R, W>(
    worker_count: usize,
    sorted_hashes: &[H],
    new_compressor: F,
    chunk_list_read_cb: R,
    chunk_write_cb: W,
) -> Result<Vec<(H, ChunkLocation)>, SpriteShrinkError>
where
    C: FnMut(&[u8]) -> Result<Vec<u8>, EComp> + Send,
    EComp: std::error::Error,
    ERead: std::error::Error + IsCancelled,
    EWrite: std::error::Error + IsCancelled,
    F: Fn() -> Result<C, EComp> + Sync,
    H: Copy,
    R: Fn(&[H]) -> Result<Vec<Vec<u8>>, ERead>,
    W: FnMut(&[u8]) -> Result<(), EWrite>,
{
    /*ArchiveError isn't public, so the caller's error type can't convert
    into it. Its message is kept instead.*/
    let compression_error =
        |e: EComp| ArchiveError::CompressionError(e.to_string());

    /*Wraps each compressor so its errors are ArchiveErrors. The closure
    that maps them captures nothing, so it's Copy and each wrapper gets
    its own.*/
    let new_archive_compressor = || {
        new_compressor()
            .map(|mut compress| move |data: &[u8]| {
                compress(data).map_err(compression_error)
            })
            .map_err(compression_error)
    };

    compress_pipeline(
        worker_count,
        sorted_hashes,
        new_archive_compressor,
        chunk_list_read_cb,
        chunk_write_cb,
    ).map_err(SpriteShrinkError::from)
}

/// Compresses chunks in parallel and writes them in order, recording where
/// each one was written.
///
/// Runs `run_pipeline` with each worker's compressor, and builds the chunk
/// index from the compressed lengths as they're written. The order of
/// writes is what makes the index's offsets correct.
///
/// # Returns
///
/// The chunk index: each hash of `sorted_hashes`, in order, with the offset
/// and length of its compressed data in the written output.
///
/// # Errors
///
/// - `ArchiveError::CompressionError` if a compressed chunk is 4 GiB or
///   more, which the chunk index can't record. Nothing is written for it.
/// - Any error from `run_pipeline`.
fn compress_pipeline<C, ERead, EWrite, F, H, R, W>(
    worker_count: usize,
    sorted_hashes: &[H],
    new_compressor: F,
    chunk_list_read_cb: R,
    mut chunk_write_cb: W,
) -> Result<Vec<(H, ChunkLocation)>, ArchiveError>
where
    C: FnMut(&[u8]) -> Result<Vec<u8>, ArchiveError> + Send,
    ERead: std::error::Error + IsCancelled,
    EWrite: std::error::Error + IsCancelled,
    F: Fn() -> Result<C, ArchiveError> + Sync,
    H: Copy,
    R: Fn(&[H]) -> Result<Vec<Vec<u8>>, ERead>,
    W: FnMut(&[u8]) -> Result<(), EWrite>,
{
    let new_worker = || {
        new_compressor().map(|mut compress| {
            move |_position: usize, data: &[u8]| compress(data)
        })
    };

    /*Locations in write order, so the nth belongs to sorted_hashes[n]. They
    are paired with their hashes at the end, which needs no indexing.*/
    let mut locations = Vec::with_capacity(sorted_hashes.len());

    //Offset of the next chunk in the written data.
    let mut offset = 0u64;

    let write = |data: &[u8]| -> Result<(), ArchiveError> {
        /*The chunk index stores lengths as u32. A compressed chunk is a few
        KiB, so this only fails if the chunker's limits change. It's checked
        first so that a chunk the index can't describe is never written.*/
        let compressed_length = u32::try_from(data.len())
            .map_err(|_too_long| ArchiveError::CompressionError(format!(
                "a compressed chunk is {} bytes, more than the chunk index \
                can record",
                data.len()
            )))?;

        chunk_write_cb(data).map_err(|e| callback_error(&e))?;

        locations.push(ChunkLocation { offset, compressed_length });
        offset += u64::from(compressed_length);
        Ok(())
    };

    //Compression doesn't know a chunk's size until it has read it.
    run_pipeline(
        worker_count,
        1,
        sorted_hashes,
        |_: &H| None,
        new_worker,
        chunk_list_read_cb,
        write,
    )?;

    //run_pipeline wrote every chunk, so both sides have the same length.
    Ok(sorted_hashes.iter().copied().zip(locations).collect())
}

/// Decompresses an SSMC file's chunks in parallel and writes them in
/// order: the reverse of what `ArchiveBuilder` does when it compresses
/// them.
///
/// Reads stored chunks in batches through `chunk_list_read_cb`, decodes
/// them on a pool of worker threads, each with its own
/// [`ChunkDecompressor`], and writes the decoded bytes through
/// `chunk_write_cb` in `chunks` order. Each chunk is checked as
/// `decompress_chunk_checked` does: exactly its recorded length, and
/// hashing to the hash it's stored under. Reading runs ahead of writing,
/// so the workers don't wait, but holds at most 1,000 chunks or about
/// 64 MiB of chunk data at once, sized from the recorded lengths.
///
/// The library does no file I/O: the callbacks read the stored chunks and
/// write the output.
///
/// # Arguments
///
/// * `worker_count`: The number of worker threads to decode with. `0`
///   uses the available parallelism. The calling thread coordinates
///   besides these, and mostly waits.
/// * `dictionary`: The archive's Zstandard dictionary. It may be empty.
/// * `chunks`: The chunks to write, in order, as a file manifest lists them
///   (`FileManifestParent::chunk_metadata`). A chunk that appears twice is
///   read and decoded twice.
/// * `chunk_list_read_cb`: Given a run of `chunks`, returns each one's
///   stored (compressed) bytes, in the same order. A host looks each hash
///   up in the chunk index (`ParsedSSMC::chunk_index`) and reads that
///   location.
/// * `chunk_write_cb`: Called once per chunk, in order, with its decoded
///   bytes.
///
/// # Errors
///
/// - `SpriteShrinkError::Cancelled` if either callback reports a
///   cancellation.
/// - `SpriteShrinkError::Io` if zstd can't load `dictionary` (before any
///   chunk is read), or can't decode a chunk.
/// - `SpriteShrinkError::Archive` with `ArchiveError::ChunkTooLong`,
///   `ChunkTooShort` or `ChunkHashMismatch` for a chunk that fails its
///   checks, and for every other error: an error from either callback, a
///   read callback that returns a different number of chunks than it was
///   asked for, a worker thread that panics, or a `worker_count` of 0 when
///   the available parallelism can't be determined.
///
/// Nothing after a failed chunk is written.
pub fn decompress_chunks_with<ERead, EWrite, H, R, W>(
    worker_count: usize,
    dictionary: &[u8],
    chunks: &[SSAChunkMeta<H>],
    chunk_list_read_cb: R,
    mut chunk_write_cb: W,
) -> Result<(), SpriteShrinkError>
where
    ERead: std::error::Error + IsCancelled,
    EWrite: std::error::Error + IsCancelled,
    H: Hashable,
    R: Fn(&[SSAChunkMeta<H>]) -> Result<Vec<Vec<u8>>, ERead>,
    W: FnMut(&[u8]) -> Result<(), EWrite>,
{
    let new_worker = || {
        let mut decompressor =
            ChunkDecompressor::new(dictionary).map_err(into_archive_error)?;
        Ok(move |position: usize, data: &[u8]| {
            let chunk = chunks.get(position).ok_or_else(|| {
                ArchiveError::WorkerError(format!(
                    "a worker was given chunk {position}, which isn't in the \
                    list"
                ))
            })?;
            decompressor
                .decompress_checked(data, chunk.hash, chunk.length as usize)
                .map_err(into_archive_error)
        })
    };

    let write = |data: &[u8]| -> Result<(), ArchiveError> {
        chunk_write_cb(data).map_err(|e| callback_error(&e))
    };

    /*The manifest records each chunk's decoded length, and decoding stops
    there, so reading ahead is planned from it. The stored bytes are
    counted too once they're read.*/
    run_pipeline(
        worker_count,
        DECODE_RUN_LEN,
        chunks,
        |chunk: &SSAChunkMeta<H>| Some(chunk.length as usize),
        new_worker,
        chunk_list_read_cb,
        write,
    )
    .map_err(|e| match e {
        //As decompress_chunk reports them.
        ArchiveError::Io(e) => SpriteShrinkError::Io(e),
        e => SpriteShrinkError::from(e),
    })
}

/// A `SpriteShrinkError` from a `ChunkDecompressor` as an `ArchiveError`,
/// for the pipeline. `decompress_chunks_with` turns `Io` back into
/// `SpriteShrinkError::Io`, and `SpriteShrinkError::from` does the rest.
fn into_archive_error(e: SpriteShrinkError) -> ArchiveError {
    match e {
        SpriteShrinkError::Archive(e) => e,
        SpriteShrinkError::Io(e) => ArchiveError::Io(e),
        SpriteShrinkError::Cancelled => ArchiveError::Cancelled,
        e => ArchiveError::External(e.to_string()),
    }
}

/// Transforms items' data in parallel and writes the results in order: the
/// pipeline under both `compress_pipeline` and `decompress_chunks_with`.
///
/// The calling thread coordinates: it reads data in batches through
/// `read`, queues it for a pool of worker threads, and writes the results
/// through `write` in `items` order. Workers can finish out of order, so
/// results are held by position until every earlier item has been written.
///
/// # Type Parameters
///
/// * `T`: An item: what the read callback is given to find a chunk's data.
/// * `S`: Gives an item's size, if it's known before reading: the most
///   bytes its data or its result will be. Used to plan reading ahead.
/// * `F`: Makes a worker's transform. Each worker calls it once.
/// * `C`: A transform: takes an item's position in `items` and its data,
///   and returns the bytes to write.
/// * `R`: The read callback. Given a run of `items`, it returns each one's
///   data, in the same order.
/// * `W`: Writes one item's result. Called in order.
///
/// # Arguments
///
/// * `worker_count`: The number of worker threads. `0` uses the available
///   parallelism. The calling thread coordinates besides these, and
///   mostly waits.
/// * `max_run_len`: The most items handed to a worker at once (at least 1).
///   Runs are also kept short enough that each batch reaches every worker.
/// * `items`: The items, in the order to write them.
/// * `item_size`: See `S`.
/// * `new_worker`: Makes each worker's transform. The first one is made on
///   the calling thread before anything is read.
/// * `read`: See `R`.
/// * `write`: See `W`.
///
/// # Errors
///
/// - `ArchiveError::Cancelled` if the read callback reports a
///   cancellation, and `ArchiveError::External` for any other error from
///   it.
/// - `ArchiveError::External` if the read callback returns a different
///   number of chunks than it was asked for.
/// - `ArchiveError::Io` if `worker_count` is 0 and the available
///   parallelism can't be determined.
/// - `ArchiveError::WorkerError` if a worker thread panics.
/// - Any error from `new_worker`, a transform, or `write`.
fn run_pipeline<C, ERead, F, R, S, T, W>(
    worker_count: usize,
    max_run_len: usize,
    items: &[T],
    item_size: S,
    new_worker: F,
    read: R,
    write: W,
) -> Result<(), ArchiveError>
where
    C: FnMut(usize, &[u8]) -> Result<Vec<u8>, ArchiveError> + Send,
    ERead: std::error::Error + IsCancelled,
    F: Fn() -> Result<C, ArchiveError> + Sync,
    R: Fn(&[T]) -> Result<Vec<Vec<u8>>, ERead>,
    S: Fn(&T) -> Option<usize>,
    W: FnMut(&[u8]) -> Result<(), ArchiveError>,
{
    let num_workers = if worker_count == 0 {
        thread::available_parallelism()?.get()
    } else {
        worker_count
    };

    /*Jobs are runs of items. The coordinator's window already limits how
    far reading runs ahead, and every run holds at least one item, so the
    queue never fills.*/
    let (to_workers_tx, to_workers_rx) = flume::bounded::<Run>(REORDER_WINDOW);

    //Results are runs of result data, or the error a worker hit.
    let (from_workers_tx, from_workers_rx) =
        flume::unbounded::<Result<Run, ArchiveError>>();

    /*Make one transform before reading anything or starting any threads,
    so a bad configuration (e.g. a dictionary zstd rejects) fails first.
    It isn't thrown away: the first worker uses it.*/
    let mut checked_worker = Some(new_worker()?);

    /*Each worker's closure is `move`, so it needs its own handle on the
    factory. A shared reference is Copy, and is Send because F is Sync.*/
    let new_worker = &new_worker;

    /*Scoped threads can borrow the factory and whatever it borrows (e.g.
    the dictionary), and are all joined before the scope returns, even when
    the coordinator stops early.*/
    thread::scope(|scope| {
        let workers: Vec<_> = (0..num_workers)
            .map(|_| {
                let to_workers_rx = to_workers_rx.clone();
                let from_workers_tx = from_workers_tx.clone();
                //Some for the first worker, None for the rest.
                let checked = checked_worker.take();
                scope.spawn(move || {
                    let mut transform = match checked
                        .map_or_else(new_worker, Ok)
                    {
                        Ok(transform) => transform,
                        Err(e) => {
                            /*A failed send means the coordinator has
                            already stopped and doesn't need the error.*/
                            let _ignored = from_workers_tx.send(Err(e));
                            return;
                        }
                    };

                    /*Workers loop until the queue is empty and the
                    coordinator has dropped its sender.*/
                    while let Ok((first, run)) = to_workers_rx.recv() {
                        let mut outputs = Vec::with_capacity(run.len());
                        let mut failure = None;
                        for (position, data) in (first..).zip(&run) {
                            match transform(position, data) {
                                Ok(output) => outputs.push(output),
                                Err(e) => {
                                    failure = Some(e);
                                    break;
                                }
                            }
                        }

                        /*Results before a failed item go first, so they're
                        written as they would be one item at a time. A
                        failed send means the coordinator has stopped, and
                        after an error there's nothing more to do.*/
                        if !outputs.is_empty() &&
                            from_workers_tx.send(Ok((first, outputs))).is_err()
                        {
                            break;
                        }
                        if let Some(e) = failure {
                            let _ignored = from_workers_tx.send(Err(e));
                            break;
                        }
                    }
                })
            })
            .collect();

        //Only the workers' clones remain, so the coordinator sees them stop.
        drop(to_workers_rx);
        drop(from_workers_tx);

        let result = coordinate_chunks(
            items,
            &item_size,
            &read,
            write,
            RunSplit { workers: num_workers, max_len: max_run_len },
            &to_workers_tx,
            &from_workers_rx,
        );

        /*Closing the queue ends each worker's loop once it's empty; without
        it, joining the workers would wait forever. Closing the results
        makes a worker stop at its next send after an early return, instead
        of working through the rest of the queue for nothing.*/
        drop(to_workers_tx);
        drop(from_workers_rx);

        for worker in workers {
            worker.join().map_err(|_panic| ArchiveError::WorkerError(
                "A pipeline worker thread panicked.".to_string()
            ))?;
        }

        result
    })
}

/// One item in the coordinator's window.
enum Slot {
    /// Read and with the workers. Holds the bytes counted for it: the
    /// larger of its data and its expected result.
    Pending(usize),
    /// Done, and waiting for every earlier item to be written.
    Done(Vec<u8>),
}

/// How many of `upcoming` to read next, keeping the window under
/// [`PIPELINE_BYTE_BUDGET`] as far as sizes are known.
///
/// Each item counts as its `item_size`, or, when that's unknown, as the
/// largest data or result seen so far (`largest_seen`). With neither,
/// one item is read to find out. `held` is what the window holds now.
///
/// Returns at least 1 when the window is empty (`window_empty`) and items
/// remain, even for an item larger than the budget, so a large item is
/// read on its own rather than never.
fn batch_len<T>(
    upcoming: &[T],
    item_size: impl Fn(&T) -> Option<usize>,
    largest_seen: usize,
    held: usize,
    window_empty: bool,
) -> usize {
    let mut planned = held;
    let mut count = 0;
    for item in upcoming.iter().take(PREFETCH_HIGH_THRESHOLD) {
        let Some(size) = item_size(item)
            .or_else(|| (largest_seen > 0).then_some(largest_seen))
        else {
            //Nothing to go on yet: read one item and learn its size.
            return count.max(1);
        };

        let next = planned.saturating_add(size);
        if next > PIPELINE_BYTE_BUDGET && !(window_empty && count == 0) {
            break;
        }
        planned = next;
        count += 1;
    }
    count
}

/// Runs the calling thread's side of `run_pipeline`: reads items' data and
/// queues it for the workers in runs, and writes the results in order.
fn coordinate_chunks<ERead, R, S, T, W>(
    items: &[T],
    item_size: &S,
    read: &R,
    mut write: W,
    split: RunSplit,
    to_workers_tx: &flume::Sender<Run>,
    from_workers_rx: &flume::Receiver<Result<Run, ArchiveError>>,
) -> Result<(), ArchiveError>
where
    ERead: std::error::Error + IsCancelled,
    R: Fn(&[T]) -> Result<Vec<Vec<u8>>, ERead>,
    S: Fn(&T) -> Option<usize>,
    W: FnMut(&[u8]) -> Result<(), ArchiveError>,
{
    let total_items = items.len();

    //Position of the next item to read and queue for the workers.
    let mut read_cursor = 0usize;

    //Position of the next item to write.
    let mut write_cursor = 0usize;

    /*A ring buffer with a slot for every item from write_cursor up to
    read_cursor. Items are written from the front as soon as the front slot
    is done. Reading stops while the window is full, so it never holds
    more than REORDER_WINDOW slots and allocates once.*/
    let mut window: VecDeque<Slot> = VecDeque::with_capacity(REORDER_WINDOW);

    //Bytes counted for the slots in the window.
    let mut held = 0usize;

    //The largest data or result seen, to size items whose size is unknown.
    let mut largest_seen = 0usize;

    /*The send fails only once every worker has stopped, and a worker stops
    early only after sending its error, so report that error rather than
    the send's.*/
    let send_run = |run: Run| {
        to_workers_tx.send(run).map_err(|_disconnected| {
            from_workers_rx
                .try_iter()
                .find_map(Result::err)
                .unwrap_or_else(|| ArchiveError::WorkerError(
                    "Worker pool terminated unexpectedly.".to_string()
                ))
        })
    };

    while write_cursor < total_items {
        /*Read another batch when the window has room for a whole one. While
        a slow item holds up writing and the window is full, the coordinator
        waits for results instead.*/
        let upcoming = items.get(read_cursor..).unwrap_or_default();
        if window.len() + upcoming.len().min(PREFETCH_HIGH_THRESHOLD)
            <= REORDER_WINDOW
        {
            let count = batch_len(
                upcoming,
                item_size,
                largest_seen,
                held,
                window.is_empty(),
            );
            if let Some(to_read) = upcoming.get(..count) &&
                !to_read.is_empty()
            {
                let data = read(to_read).map_err(|e| callback_error(&e))?;

                /*A missing item would never arrive, and the loop would
                wait for it forever.*/
                if data.len() != to_read.len() {
                    return Err(ArchiveError::External(format!(
                        "the read callback returned {} chunks for {} hashes",
                        data.len(),
                        to_read.len()
                    )));
                }

                //Queue the batch as runs.
                let run_len = split.len(to_read.len());
                let mut run_start = read_cursor;
                let mut run = Vec::with_capacity(run_len);
                let mut run_bytes = 0usize;

                for (item, data) in to_read.iter().zip(data) {
                    let counted = data.len().max(item_size(item).unwrap_or(0));
                    largest_seen = largest_seen.max(data.len());
                    held = held.saturating_add(counted);
                    window.push_back(Slot::Pending(counted));
                    read_cursor += 1;

                    run_bytes = run_bytes.saturating_add(data.len());
                    run.push(data);
                    if run.len() == run_len || run_bytes >= RUN_BYTE_TARGET {
                        let full = std::mem::replace(
                            &mut run,
                            Vec::with_capacity(run_len)
                        );
                        send_run((run_start, full))?;
                        run_start = read_cursor;
                        run_bytes = 0;
                    }
                }
                if !run.is_empty() {
                    send_run((run_start, run))?;
                }
            }
        }

        /*Every queued item is either still with a worker or already in the
        window, so the channel only disconnects if every worker has
        stopped.*/
        let (first, outputs) = from_workers_rx.recv()
            .map_err(|_disconnected| ArchiveError::WorkerError(
                "Worker pool finished but not all chunks were processed."
                .to_string()
            ))??;

        for (position, output) in (first..).zip(outputs) {
            /*The front slot is write_cursor's. Workers only return
            positions the coordinator queued, which are all in the window.*/
            let slot = position.checked_sub(write_cursor)
                .and_then(|index| window.get_mut(index))
                .filter(|slot| matches!(slot, Slot::Pending(_)))
                .ok_or_else(|| ArchiveError::WorkerError(format!(
                    "a worker returned chunk {position}, which wasn't \
                    waiting for a worker"
                )))?;
            if let Slot::Pending(counted) = *slot {
                held = held.saturating_sub(counted);
            }
            held = held.saturating_add(output.len());
            largest_seen = largest_seen.max(output.len());
            *slot = Slot::Done(output);
        }

        //Write every item that is now next in order.
        while matches!(window.front(), Some(Slot::Done(_))) {
            let Some(Slot::Done(output)) = window.pop_front() else {
                break;
            };
            write(&output)?;
            held = held.saturating_sub(output.len());
            write_cursor += 1;
        }
    }

    Ok(())
}

impl<'a, E, H, R, W> ArchiveBuilder<'a, E, H, R, W>
where
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    H: Copy + Debug + Encode + Eq + Hash + Serialize + Send + Sync + 'static + Display
        + Ord,
    R: Fn(&[H]) -> Result<Vec<Vec<u8>>, E>,
    W: FnMut(&[u8]) -> Result<(), E>,
{
    /// Creates a builder with the default settings.
    ///
    /// # Arguments
    ///
    /// * `sorted_hashes`: The chunks to compress, in the order to write
    ///   them. A slice or `&Vec` is borrowed and never copied; a `Vec` is
    ///   moved in and freed with the builder.
    /// * `total_size`: The combined size in bytes of the uncompressed
    ///   chunks.
    /// * `get_chunk_data`: Returns the data of the chunks it's given, in the
    ///   same order.
    /// * `write_comp_data`: Receives each compressed chunk, in
    ///   `sorted_hashes` order.
    pub fn new(
        //ser_file_manifest: Vec<FileManifestParent<H>>,
        sorted_hashes: impl Into<Cow<'a, [H]>>,
        total_size: u64,
        get_chunk_data: R,
        write_comp_data: W,
    ) -> Self {
        Self {
            //ser_file_manifest,
            sorted_hashes: sorted_hashes.into(),
            total_size,
            get_chunk_data,
            write_comp_data,
            //Set default values for optional parameters
            compression_level: 19,
            dictionary_size: 16 * 1024,
            worker_threads: 0, //Use the available parallelism
            opt_dict: false,
            progress_callback: None,
            c_progress_callback: None,
            _error_type: PhantomData,
        }
    }

    //The following 5 functions set optional parameters.
    /*
    /// Sets the numerical compression code.
    ///
    /// The default value is `98` for zstd.

    pub fn compression_algorithm(&mut self, algorithm: u16) -> &mut Self {
        self.compression_algorithm = algorithm;
        self
    }*/

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

    /// Sets the number of threads that compress chunks.
    ///
    /// The default value is `0`, which uses the available parallelism.
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
    /// If no dictionary can be trained, as happens when the input has too
    /// little distinct data, the chunks are compressed without one and the
    /// returned dictionary is empty. Such archives extract like any other.
    ///
    /// The compressed chunks go to the write callback in `sorted_hashes`
    /// order, one call per chunk. Every chunk has been written when `build`
    /// returns, so a caller that buffers its writes should flush them then.
    /// Flushing on drop isn't enough for a `BufWriter`, which ignores errors
    /// when it's dropped.
    ///
    /// Progress is reported to both the Rust and the C progress callback:
    /// `GeneratingDictionary`, `DictionaryDone`, `Compressing`, then
    /// `ChunkCompressed` once per chunk as it's written, then `Finalizing`
    /// once every chunk is written.
    ///
    /// # Returns
    ///
    /// The dictionary and the encoded chunk index. The compressed chunks
    /// themselves have gone to the write callback.
    ///
    /// # Errors
    ///
    /// Returns a `SpriteShrinkError` if any step fails, such as reading
    /// chunks for the dictionary samples, compression, or a callback.
    /// `is_cancelled()` is true if a callback reported a cancellation.
    pub fn build(self) -> Result<CompressionResult, SpriteShrinkError> {
        //Destructure self into its fields
        let ArchiveBuilder {
            sorted_hashes,
            total_size,
            get_chunk_data,
            mut write_comp_data,
            compression_level,
            dictionary_size,
            worker_threads,
            opt_dict,
            progress_callback,
            c_progress_callback,
            _error_type
        } = self;

        /*Borrowed or owned, the build only reads the hashes, so borrow them
        as a slice rather than taking ownership, which could copy them.*/
        let sorted_hashes: &[H] = &sorted_hashes;

        //Every report goes to both progress callbacks, if they're set.
        let report_progress = {
            let progress_callback = progress_callback.as_ref();
            move |progress: Progress| {
                if let Some(callback) = progress_callback {
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

        //Report progress before starting a dictionary generation
        report_progress(Progress::GeneratingDictionary);

        let (samples_for_dict, sample_sizes) = build_train_samples(
            sorted_hashes,
            total_size,
            dictionary_size as usize,
            &get_chunk_data
        )?;

        /*Make dictionary from sorted data. Training fails when there's too
        little distinct data, such as an archive of one small file: zstd's
        trainers need at least seven unique chunks. A dictionary only makes
        the output smaller, and on that little data it costs more bytes
        than it saves, so the archive is built without one instead. An
        empty dictionary is stored with length 0, and zstd treats it as no
        dictionary when compressing and decompressing.*/
        let dictionary = if opt_dict {
            gen_zstd_opt_dict(
                &samples_for_dict,
                &sample_sizes,
                dictionary_size as usize,
                worker_threads,
                compression_level
            ).ok()
        } else {
            zstd::dict::from_continuous(
                &samples_for_dict,
                &sample_sizes,
                dictionary_size as usize, //Dictionary size in bytes
            ).ok()
        }.unwrap_or_default();

        //Samples are no longer needed.
        drop(samples_for_dict);
        drop(sample_sizes);

        //Report progress after dictionary generation is done.
        report_progress(Progress::DictionaryDone);

        /*compress_chunks calls this on this thread, so it can borrow the
        write callback.*/
        let write_chunk_data_cb = |chunk: &[u8]| -> Result<(), E> {
            write_comp_data(chunk)?;
            report_progress(Progress::ChunkCompressed);
            Ok(())
        };

        report_progress(Progress::Compressing {
            total_chunks: (
                sorted_hashes.len() as u64
            )
        });

        let chunk_index = compress_chunks(
            worker_threads,
            &dictionary,
            sorted_hashes,
            compression_level,
            &get_chunk_data,
            write_chunk_data_cb
        )?;

        report_progress(Progress::Finalizing);

        let enc_chunk_index = encode(&chunk_index);

        let dictionary_size = dictionary.len() as u64;
        let enc_chunk_index_size = enc_chunk_index.len() as u64;

        Ok(CompressionResult {
            dictionary,
            dictionary_size,
            enc_chunk_index,
            enc_chunk_index_size
        })
    }
}

/// Decompresses a compressed chunk using a ZSTD dictionary.
///
/// # Parameters
///
/// * `comp_chunk_data` – A byte slice containing the compressed chunk data.
/// * `dictionary` – A byte slice containing the ZSTD dictionary that was used
///   during compression.
/// * `max_len` – The most bytes the chunk may decode to, normally its
///   recorded length. Values above [`MAX_CHUNK_LEN`] (16 MiB) are lowered to
///   it, since no valid chunk is longer. Decoding stops one byte past the
///   limit, so a small chunk that expands without end can't use more memory
///   than this.
///
/// # Returns
///
/// Returns `Ok(Vec<u8>)` with the decompressed data on success, or a
/// `SpriteShrinkError` on failure.
///
/// # Errors
///
/// This function will return an error if:
/// * The ZSTD decoder cannot be created with the supplied dictionary,
/// * The decompression process fails while reading the compressed data, or
/// * The chunk decodes to more than `max_len` bytes, or more than
///   [`MAX_CHUNK_LEN`] (`ArchiveError::ChunkTooLong`, with the limit that
///   applied).
///
/// The output is not checked against the chunk's hash or its recorded
/// length. Chunks are written without a zstd checksum, so most damage to
/// `comp_chunk_data` decodes to wrong bytes with no error. Use
/// `decompress_chunk_checked` unless the caller checks the output itself.
///
/// This sets up a zstd context and loads `dictionary` for this one chunk.
/// To decompress many chunks, make one [`ChunkDecompressor`] and reuse it.
pub fn decompress_chunk(
    comp_chunk_data: &[u8],
    dictionary: &[u8],
    max_len: usize,
) -> Result<Vec<u8>, SpriteShrinkError> {
    ChunkDecompressor::new(dictionary)?.decompress(comp_chunk_data, max_len)
}

/// Decompresses a chunk like `decompress_chunk`, then checks that the
/// output is exactly `len` bytes and hashes to `expected`, the hash the
/// chunk is stored under.
///
/// `len` is the chunk's length as the archive records it
/// (`SSAChunkMeta::length`, or `DataChunkLayout::uncomp_len` in a disc
/// archive). Decoding stops just past it.
///
/// The chunk's key is the hash of its uncompressed bytes, so this catches
/// damage that zstd decodes without an error, including two frames that
/// decode as one chunk.
///
/// # Errors
///
/// Returns the errors of `decompress_chunk`, with `len` as its limit,
/// `ArchiveError::ChunkTooShort` if the output is shorter than `len`, or
/// `ArchiveError::ChunkHashMismatch` if it doesn't hash to `expected` (all
/// inside `SpriteShrinkError::Archive`).
///
/// Like `decompress_chunk`, this sets up zstd for one chunk; reuse a
/// [`ChunkDecompressor`] for many.
pub fn decompress_chunk_checked<H: Hashable>(
    comp_chunk_data: &[u8],
    dictionary: &[u8],
    expected: H,
    len: usize,
) -> Result<Vec<u8>, SpriteShrinkError> {
    ChunkDecompressor::new(dictionary)?
        .decompress_checked(comp_chunk_data, expected, len)
}

/// The largest zstd window a chunk may use, as a power of two: enough for
/// [`MAX_CHUNK_LEN`]. Chunks are compressed with their size known, so zstd
/// never gives one a window larger than the chunk.
const MAX_WINDOW_LOG: u32 = MAX_CHUNK_LEN.ilog2();

//MAX_WINDOW_LOG is exact only if MAX_CHUNK_LEN is a power of two.
const _: () = assert!(MAX_CHUNK_LEN.is_power_of_two());

/// Decompresses chunks one after another with a single zstd context and
/// dictionary.
///
/// Setting up zstd and loading the dictionary costs more than decoding a
/// small chunk (about 5.5 us against 2 us for a 2 KiB chunk), so a caller
/// with many chunks makes one of these and reuses it. Each thread needs its
/// own, since decompressing takes `&mut self`.
///
/// The context keeps buffers as large as the largest chunk's zstd window,
/// at most 16 MiB ([`MAX_CHUNK_LEN`]); frames asking for more are refused.
///
/// `decompress_chunk` and `decompress_chunk_checked` make one of these for
/// a single chunk.
pub struct ChunkDecompressor {
    context: zstd::zstd_safe::DCtx<'static>,
}

impl Debug for ChunkDecompressor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        //zstd's context has no Debug, and nothing in it is useful to print.
        f.debug_struct("ChunkDecompressor").finish_non_exhaustive()
    }
}

/// A zstd error code as an `io::Error`, as the `zstd` crate's own readers
/// report it.
fn zstd_error(code: usize) -> io::Error {
    io::Error::other(zstd::zstd_safe::get_error_name(code))
}

impl ChunkDecompressor {
    /// Makes a decompressor for chunks compressed with `dictionary`, which
    /// may be empty (an archive whose dictionary training failed).
    ///
    /// # Errors
    ///
    /// Returns `SpriteShrinkError::Io` if zstd can't load `dictionary`.
    pub fn new(dictionary: &[u8]) -> Result<Self, SpriteShrinkError> {
        let mut context = zstd::zstd_safe::DCtx::create();
        context.load_dictionary(dictionary).map_err(zstd_error)?;
        context
            .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(
                MAX_WINDOW_LOG,
            ))
            .map_err(zstd_error)?;
        Ok(Self { context })
    }

    /// Decompresses one chunk, decoding at most `max_len` bytes. See
    /// `decompress_chunk` for the limit and what isn't checked.
    ///
    /// A chunk that fails leaves the decompressor usable for the next one.
    ///
    /// # Errors
    ///
    /// The errors of `decompress_chunk`.
    pub fn decompress(
        &mut self,
        comp_chunk_data: &[u8],
        max_len: usize,
    ) -> Result<Vec<u8>, SpriteShrinkError> {
        /*Start a new frame. The last chunk may have stopped part way through
        one, on an error or at the length limit. The dictionary and the
        window limit are kept.*/
        self.context
            .reset(zstd::zstd_safe::ResetDirective::SessionOnly)
            .map_err(zstd_error)?;
        let decoder = zstd::stream::Decoder::with_context(
            comp_chunk_data,
            &mut self.context,
        );

        let max_len = max_len.min(MAX_CHUNK_LEN);

        /*Read one byte more than allowed, so output past `max_len` is seen
        without decoding the rest of it.*/
        let limit = u64::try_from(max_len).map_or(u64::MAX, |len| len.saturating_add(1));
        let mut decompressed_chunk_data = Vec::new();
        decoder.take(limit).read_to_end(&mut decompressed_chunk_data)?;

        if decompressed_chunk_data.len() > max_len {
            return Err(ArchiveError::ChunkTooLong { limit: max_len }.into());
        }

        Ok(decompressed_chunk_data)
    }

    /// Decompresses one chunk and checks it is exactly `len` bytes and
    /// hashes to `expected`. See `decompress_chunk_checked`.
    ///
    /// # Errors
    ///
    /// The errors of `decompress_chunk_checked`.
    pub fn decompress_checked<H: Hashable>(
        &mut self,
        comp_chunk_data: &[u8],
        expected: H,
        len: usize,
    ) -> Result<Vec<u8>, SpriteShrinkError> {
        let data = self.decompress(comp_chunk_data, len)?;
        if data.len() != len {
            return Err(ArchiveError::ChunkTooShort {
                expected: len,
                actual: data.len(),
            }
            .into());
        }
        check_chunk_hash(&data, expected)?;
        Ok(data)
    }
}

/// Checks that `data` hashes to `expected`, the hash its chunk is stored
/// under.
///
/// # Errors
///
/// Returns `ArchiveError::ChunkHashMismatch` if it doesn't.
pub(crate) fn check_chunk_hash<H: Hashable>(
    data: &[u8],
    expected: H,
) -> Result<(), ArchiveError> {
    let actual = H::from_bytes_with_seed(data);
    if actual != expected {
        return Err(ArchiveError::ChunkHashMismatch {
            expected: format!("{expected:?}"),
            actual: format!("{actual:?}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::HashMap,
        io::ErrorKind,
        sync::{
            Arc, Mutex, OnceLock, mpsc,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
        time::Duration,
    };

    use proptest::{collection::vec, prelude::*};
    use sprite_shrink_test_support::tiled_bytes;

    use super::*;
    use crate::parsing::parse_file_chunk_index;

    /// A dictionary trained on 64 KiB of ROM-like data cut into 1 KiB
    /// samples, trained once and shared by every test.
    fn dictionary() -> &'static [u8] {
        static DICTIONARY: OnceLock<Vec<u8>> = OnceLock::new();
        DICTIONARY.get_or_init(|| {
            let samples = tiled_bytes(0x5EED_0D1C, 64 * 1024);
            zstd::dict::from_continuous(&samples, &[1024; 64], 4096).unwrap()
        })
    }

    /// Compresses `payload` the way `compress_chunks`' workers do.
    fn compress(
        payload: &[u8],
        dict: &[u8],
        level: i32,
    ) -> Result<Vec<u8>, ArchiveError> {
        Ok(chunk_compressor(level, dict)?.compress(payload)?)
    }

    /// 2000 bytes of data like the dictionary's, compressed with it at
    /// level 19.
    fn compressed_payload() -> (Vec<u8>, Vec<u8>) {
        let payload = tiled_bytes(0x5EED_0D1C, 2000);
        let compressed = compress(&payload, dictionary(), 19).unwrap();
        (payload, compressed)
    }

    /// The `io::Error` inside `result`, or a panic naming what was returned
    /// instead.
    fn io_error(result: Result<Vec<u8>, SpriteShrinkError>) -> io::Error {
        match result {
            Err(SpriteShrinkError::Io(e)) => e,
            other => panic!("expected an I/O error, got {other:?}"),
        }
    }

    /// Payloads for the round trip: random bytes, which barely compress,
    /// and ROM-like bytes, which the dictionary helps with.
    fn payloads() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            vec(any::<u8>(), 0..4096),
            // `tiled_bytes` needs a non-zero seed.
            (0..4096_usize, 1..=u64::MAX)
                .prop_map(|(len, seed)| tiled_bytes(seed, len)),
        ]
    }

    proptest! {
        #[test]
        fn chunks_decompress_to_what_was_compressed(
            payload in payloads(),
            level in -7..=22_i32,
            use_dictionary: bool,
        ) {
            // An empty dictionary is what `build` stores when training
            // fails.
            let dict = if use_dictionary { dictionary() } else { &[] };

            let compressed = compress(&payload, dict, level)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let decompressed = decompress_chunk(&compressed, dict, payload.len())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(decompressed, payload);
        }
    }

    #[test]
    fn an_empty_payload_round_trips() {
        for dict in [dictionary(), &[]] {
            let compressed = compress(&[], dict, 3).unwrap();
            assert!(!compressed.is_empty(), "no zstd frame was written");
            assert_eq!(
                decompress_chunk(&compressed, dict, 0).unwrap(),
                Vec::<u8>::new()
            );
        }
    }

    #[test]
    fn levels_outside_zstds_range_are_clamped() {
        let payload = tiled_bytes(0x5EED_0D1C, 2000);
        for level in [i32::MIN, 23, i32::MAX] {
            let compressed =
                compress(&payload, dictionary(), level).unwrap();
            assert_eq!(
                decompress_chunk(&compressed, dictionary(), payload.len())
                    .unwrap(),
                payload,
                "level {level}"
            );
        }
    }

    #[test]
    fn decompressing_without_the_same_dictionary_fails() {
        let (payload, compressed) = compressed_payload();
        let other = zstd::dict::from_continuous(
            &tiled_bytes(0x0BAD_D1C7, 64 * 1024),
            &[1024; 64],
            4096,
        )
        .unwrap();

        // The frame records the dictionary's ID, so zstd notices.
        for dict in [&other[..], &[]] {
            let e = io_error(decompress_chunk(&compressed, dict, payload.len()));
            assert_eq!(e.to_string(), "Dictionary mismatch");
        }
    }

    #[test]
    fn a_truncated_chunk_fails() {
        let (payload, compressed) = compressed_payload();
        for len in 0..compressed.len() {
            let e = io_error(decompress_chunk(
                &compressed[..len],
                dictionary(),
                payload.len(),
            ));
            assert_eq!(e.kind(), ErrorKind::UnexpectedEof, "{len} bytes");
        }
    }

    #[test]
    fn bytes_after_the_frame_fail() {
        let (payload, mut compressed) = compressed_payload();
        compressed.extend_from_slice(b"junk");

        let e =
            io_error(decompress_chunk(&compressed, dictionary(), payload.len()));
        assert_eq!(e.to_string(), "Unknown frame descriptor");
    }

    /// Whether `result` is the error for a chunk that failed its hash.
    fn is_hash_mismatch(result: &Result<Vec<u8>, SpriteShrinkError>) -> bool {
        matches!(
            result,
            Err(SpriteShrinkError::Archive(
                ArchiveError::ChunkHashMismatch { .. }
            ))
        )
    }

    #[test]
    fn a_checked_chunk_returns_its_bytes() {
        let (payload, compressed) = compressed_payload();
        let key = u64::from_bytes_with_seed(&payload);
        assert_eq!(
            decompress_chunk_checked(&compressed, dictionary(), key, payload.len())
                .unwrap(),
            payload
        );

        let key = u128::from_bytes_with_seed(&payload);
        assert_eq!(
            decompress_chunk_checked(&compressed, dictionary(), key, payload.len())
                .unwrap(),
            payload
        );
    }

    #[test]
    fn a_checked_chunk_fails_under_the_wrong_key() {
        let (payload, compressed) = compressed_payload();
        let key = u64::from_bytes_with_seed(&payload) ^ 1;
        let result =
            decompress_chunk_checked(&compressed, dictionary(), key, payload.len());
        assert!(is_hash_mismatch(&result), "{result:?}");
    }

    #[test]
    fn every_flipped_bit_fails_a_checked_chunk() {
        // Unlike `decompress_chunk`, no flip decodes to wrong bytes without
        // an error: zstd rejects it, or the hash doesn't match. A flip that
        // still decodes to the right bytes is fine.
        let (payload, compressed) = compressed_payload();
        let key = u64::from_bytes_with_seed(&payload);
        let mut mismatches = 0;
        for index in 0..compressed.len() {
            for bit in 0..8 {
                let mut corrupted = compressed.clone();
                corrupted[index] ^= 1 << bit;
                let result = decompress_chunk_checked(
                    &corrupted,
                    dictionary(),
                    key,
                    payload.len(),
                );
                if let Ok(data) = &result {
                    assert_eq!(data, &payload, "byte {index} bit {bit}");
                }
                mismatches += usize::from(is_hash_mismatch(&result));
            }
        }
        assert!(mismatches > 0, "no flip reached the hash check");
    }

    #[test]
    fn two_frames_fail_a_checked_chunk() {
        // zstd decodes two frames back to back as one stream, twice the
        // chunk's length.
        let (payload, compressed) = compressed_payload();
        let key = u64::from_bytes_with_seed(&payload);
        let doubled = [compressed.as_slice(), &compressed].concat();
        let result =
            decompress_chunk_checked(&doubled, dictionary(), key, payload.len());
        assert!(
            matches!(
                result,
                Err(SpriteShrinkError::Archive(ArchiveError::ChunkTooLong {
                    limit
                })) if limit == payload.len()
            ),
            "{result:?}"
        );
    }

    #[test]
    fn a_chunk_that_decodes_short_fails_a_checked_chunk() {
        let (payload, compressed) = compressed_payload();
        let key = u64::from_bytes_with_seed(&payload);
        let len = payload.len() + 1;
        let result = decompress_chunk_checked(&compressed, dictionary(), key, len);
        assert!(
            matches!(
                result,
                Err(SpriteShrinkError::Archive(ArchiveError::ChunkTooShort {
                    expected,
                    actual,
                })) if expected == len && actual == payload.len()
            ),
            "{result:?}"
        );
    }

    #[test]
    fn decoding_stops_just_past_max_len() {
        // 4096 frames of 1 MiB of zeros decode as one 4 GiB stream from
        // under 200 KB. Without the limit this would run out of memory.
        let frame = compress(&vec![0; 1024 * 1024], &[], 1).unwrap();
        let bomb = frame.repeat(4096);
        assert!(bomb.len() < 200 * 1024, "{} bytes", bomb.len());

        let result = decompress_chunk(&bomb, &[], 4096);
        assert!(
            matches!(
                result,
                Err(SpriteShrinkError::Archive(ArchiveError::ChunkTooLong {
                    limit: 4096
                }))
            ),
            "{result:?}"
        );

        // A caller asking for more still stops at the longest valid chunk.
        let result = decompress_chunk(&bomb, &[], usize::MAX);
        assert!(
            matches!(
                result,
                Err(SpriteShrinkError::Archive(ArchiveError::ChunkTooLong {
                    limit: MAX_CHUNK_LEN
                }))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn one_decompressor_decodes_chunk_after_chunk_and_after_a_failure() {
        let (payload, compressed) = compressed_payload();
        let key = u64::from_bytes_with_seed(&payload);
        let other = tiled_bytes(0x0BAD_D1C7, 3000);
        let other_compressed = compress(&other, dictionary(), 3).unwrap();
        let mut decompressor = ChunkDecompressor::new(dictionary()).unwrap();

        // Stopped mid-frame at the length limit, cut short, and rejected.
        let half = compressed.len() / 2;
        let failures: [(&[u8], usize); 3] = [
            (&compressed, payload.len() - 1),
            (&compressed[..half], payload.len()),
            (b"junk", payload.len()),
        ];
        for (failing, len) in failures {
            assert_eq!(
                decompressor.decompress_checked(&compressed, key, payload.len())
                    .unwrap(),
                payload
            );
            decompressor.decompress(failing, len).unwrap_err();
            assert_eq!(
                decompressor.decompress(&other_compressed, other.len()).unwrap(),
                other
            );
        }
    }

    #[test]
    fn a_frame_with_a_window_larger_than_any_chunk_is_refused() {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
        encoder
            .set_parameter(zstd::zstd_safe::CParameter::WindowLog(
                MAX_WINDOW_LOG + 1,
            ))
            .unwrap();
        io::Write::write_all(&mut encoder, &[7; 100]).unwrap();
        let frame = encoder.finish().unwrap();

        let e = io_error(decompress_chunk(&frame, &[], 100));
        assert_eq!(
            e.to_string(),
            "Frame requires too much memory for decoding"
        );
    }

    type ReadResult = Result<Vec<Vec<u8>>, SpriteShrinkError>;
    type ChunkIndex = Vec<(u64, ChunkLocation)>;

    /// `count` distinct ROM-like chunks of 200 to 499 bytes, stored under
    /// the hashes `0..count`.
    fn chunk_store(count: u64) -> (Vec<u64>, HashMap<u64, Vec<u8>>) {
        let chunks = (0..count)
            .map(|i| {
                let len = 200 + usize::try_from(i % 300).unwrap();
                (i, tiled_bytes(0x5EED_C000 + i, len))
            })
            .collect();
        ((0..count).collect(), chunks)
    }

    /// A read callback that returns each requested chunk from `chunks`.
    fn read_from(
        chunks: HashMap<u64, Vec<u8>>,
    ) -> impl Fn(&[u64]) -> ReadResult + Send + 'static {
        move |hashes| {
            Ok(hashes.iter().map(|hash| chunks[hash].clone()).collect())
        }
    }

    /// Runs `f` on another thread and returns its result, failing the test
    /// if it takes longer than 30 seconds. The hang cases deadlocked
    /// forever, so the test reports them instead of never finishing.
    fn within_timeout<T: Send + 'static>(
        f: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            /*The send fails only if the receiver timed out and was
            dropped, and that already failed the test.*/
            let _ignored = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(30))
            .expect("compress_chunks didn't return within 30 seconds")
    }

    /// Compresses `hashes` with `workers` workers and the shared dictionary,
    /// reading through `read`. Returns the result and each chunk passed to
    /// the write callback.
    fn compress_with(
        workers: usize,
        hashes: Vec<u64>,
        read: impl Fn(&[u64]) -> ReadResult + Send + 'static,
    ) -> (Result<ChunkIndex, ArchiveError>, Vec<Vec<u8>>) {
        within_timeout(move || {
            let mut written = Vec::new();
            let result = compress_chunks(
                workers,
                dictionary(),
                &hashes,
                3,
                read,
                |chunk: &[u8]| -> Result<(), SpriteShrinkError> {
                    written.push(chunk.to_vec());
                    Ok(())
                },
            );
            (result, written)
        })
    }

    #[test]
    fn chunks_are_written_in_order_with_contiguous_offsets() {
        // More than two 500-chunk batches, so the queue is refilled.
        let (hashes, chunks) = chunk_store(1200);
        let (result, written) =
            compress_with(4, hashes.clone(), read_from(chunks.clone()));
        let index = result.unwrap();

        let indexed: Vec<u64> = index.iter().map(|(hash, _)| *hash).collect();
        assert_eq!(indexed, hashes);
        assert_eq!(written.len(), hashes.len());

        let mut offset = 0;
        for ((hash, location), compressed) in index.iter().zip(&written) {
            assert_eq!(location.offset, offset, "chunk {hash}");
            assert_eq!(
                location.compressed_length as usize,
                compressed.len(),
                "chunk {hash}"
            );
            offset += u64::from(location.compressed_length);
            assert_eq!(
                decompress_chunk(compressed, dictionary(), chunks[hash].len())
                    .unwrap(),
                chunks[hash],
                "chunk {hash}"
            );
        }
    }

    /// `index` as plain tuples, since `ChunkLocation` isn't `PartialEq`.
    fn locations(index: &ChunkIndex) -> Vec<(u64, u64, u32)> {
        index
            .iter()
            .map(|(hash, loc)| (*hash, loc.offset, loc.compressed_length))
            .collect()
    }

    #[test]
    fn output_does_not_depend_on_the_worker_count() {
        let (hashes, chunks) = chunk_store(700);
        let (expected, expected_written) =
            compress_with(1, hashes.clone(), read_from(chunks.clone()));
        let expected = locations(&expected.unwrap());

        // Several runs of the multi-worker cases, to give ordering races a
        // chance to show.
        for workers in [0, 2, 2, 2, 8, 8, 8] {
            let (result, written) = compress_with(
                workers,
                hashes.clone(),
                read_from(chunks.clone()),
            );
            assert_eq!(
                locations(&result.unwrap()),
                expected,
                "{workers} workers"
            );
            assert!(written == expected_written, "{workers} workers");
        }
    }

    /// Runs `compress_chunks_with` on `hashes` from `chunks`, with
    /// `workers` workers and the compressors `new_compressor` makes.
    /// Returns the result and each chunk passed to the write callback.
    fn compress_with_custom<C, F>(
        workers: usize,
        hashes: Vec<u64>,
        chunks: HashMap<u64, Vec<u8>>,
        new_compressor: F,
    ) -> (Result<ChunkIndex, SpriteShrinkError>, Vec<Vec<u8>>)
    where
        C: FnMut(&[u8]) -> Result<Vec<u8>, io::Error> + Send,
        F: Fn() -> Result<C, io::Error> + Send + Sync + 'static,
    {
        within_timeout(move || {
            let mut written = Vec::new();
            let result = compress_chunks_with(
                workers,
                &hashes,
                new_compressor,
                read_from(chunks),
                |chunk: &[u8]| -> Result<(), SpriteShrinkError> {
                    written.push(chunk.to_vec());
                    Ok(())
                },
            );
            (result, written)
        })
    }

    #[test]
    fn a_custom_compressor_is_made_once_per_worker() {
        let (hashes, chunks) = chunk_store(600);
        let made = Arc::new(AtomicUsize::new(0));
        let made_in_factory = Arc::clone(&made);

        // Reverses each chunk, so the output shows the compressor ran.
        let (result, written) = compress_with_custom(
            3,
            hashes.clone(),
            chunks.clone(),
            move || {
                made_in_factory.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(|data: &[u8]| Ok(data.iter().rev().copied().collect()))
            },
        );
        let index = result.unwrap();

        assert_eq!(made.load(AtomicOrdering::SeqCst), 3);
        let mut offset = 0;
        for (((hash, location), compressed), expected_hash) in
            index.iter().zip(&written).zip(&hashes)
        {
            assert_eq!(hash, expected_hash);
            assert_eq!(location.offset, offset, "chunk {hash}");
            offset += u64::from(location.compressed_length);
            let reversed: Vec<u8> = chunks[hash].iter().rev().copied().collect();
            assert_eq!(compressed, &reversed, "chunk {hash}");
        }
    }

    /// A compressor type for a factory that never makes one.
    type Compressor = fn(&[u8]) -> io::Result<Vec<u8>>;

    #[test]
    fn a_compressor_that_cannot_be_made_fails_before_anything_is_read() {
        let result = within_timeout(|| {
            compress_chunks_with(
                2,
                &[1u64, 2, 3],
                || -> io::Result<Compressor> {
                    Err(io::Error::other("no codec available"))
                },
                |_: &[u64]| -> ReadResult { panic!("nothing should be read") },
                |_: &[u8]| -> Result<(), SpriteShrinkError> {
                    panic!("nothing should be written")
                },
            )
        });

        assert!(
            matches!(
                &result,
                Err(SpriteShrinkError::Archive(ArchiveError::CompressionError(
                    message
                ))) if message == "no codec available"
            ),
            "{result:?}"
        );
    }

    #[test]
    fn a_compressor_error_keeps_its_message() {
        let (hashes, chunks) = chunk_store(50);
        // Fails on the one chunk that is 230 bytes long.
        let (result, _written) = compress_with_custom(
            2,
            hashes,
            chunks,
            || Ok(|data: &[u8]| {
                if data.len() == 230 {
                    Err(io::Error::other("block 30 is malformed"))
                } else {
                    Ok(data.to_vec())
                }
            }),
        );

        assert!(
            matches!(
                &result,
                Err(SpriteShrinkError::Archive(ArchiveError::CompressionError(
                    message
                ))) if message == "block 30 is malformed"
            ),
            "{result:?}"
        );
    }

    #[test]
    fn no_chunks_compress_to_nothing() {
        let (result, written) = compress_with(
            2,
            Vec::new(),
            |_: &[u64]| -> ReadResult { panic!("nothing should be read") },
        );
        assert_eq!(result.unwrap().len(), 0);
        assert_eq!(written.len(), 0);
    }

    #[test]
    fn a_repeated_hash_is_compressed_each_time() {
        // Results used to be matched up by hash, so a repeat hung.
        let (_, chunks) = chunk_store(3);
        let hashes = vec![0, 1, 1, 2, 1];
        let (result, written) =
            compress_with(4, hashes.clone(), read_from(chunks.clone()));

        let index = result.unwrap();
        let indexed: Vec<u64> = index.iter().map(|(hash, _)| *hash).collect();
        assert_eq!(indexed, hashes);
        for (hash, compressed) in hashes.iter().zip(&written) {
            assert_eq!(
                decompress_chunk(compressed, dictionary(), chunks[hash].len())
                    .unwrap(),
                chunks[hash]
            );
        }
    }

    #[test]
    fn a_read_returning_the_wrong_number_of_chunks_fails() {
        // Fewer chunks than asked for used to hang, waiting for the rest.
        for extra in [-1_isize, 1, -500] {
            let (hashes, chunks) = chunk_store(1200);
            let asked = Arc::new(AtomicUsize::new(0));
            let read = {
                let asked = Arc::clone(&asked);
                move |requested: &[u64]| -> ReadResult {
                    asked.store(requested.len(), AtomicOrdering::Relaxed);
                    let mut batch: Vec<Vec<u8>> = requested
                        .iter()
                        .map(|hash| chunks[hash].clone())
                        .collect();
                    if extra < 0 {
                        batch.truncate(
                            batch.len().saturating_sub(extra.unsigned_abs()),
                        );
                    } else {
                        batch.push(vec![0; 10]);
                    }
                    Ok(batch)
                }
            };

            let (result, _) = compress_with(4, hashes, read);
            let asked = asked.load(AtomicOrdering::Relaxed);
            let returned = asked.saturating_add_signed(extra);
            let Err(ArchiveError::External(message)) = result else {
                panic!("{returned} chunks for {asked} hashes: {result:?}");
            };
            assert_eq!(
                message,
                format!("the read callback returned {returned} chunks for \
                         {asked} hashes")
            );
        }
    }

    #[test]
    fn read_errors_stop_compression() {
        let (result, written) = compress_with(
            4,
            (0..100).collect(),
            |_: &[u64]| -> ReadResult { Err(SpriteShrinkError::Cancelled) },
        );
        assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
        assert_eq!(written.len(), 0);

        let (result, _) =
            compress_with(4, (0..100).collect(), |_: &[u64]| -> ReadResult {
                Err(io::Error::other("disk read failed").into())
            });
        let Err(ArchiveError::External(message)) = result else {
            panic!("{result:?}");
        };
        assert!(message.contains("disk read failed"), "{message}");
    }

    #[test]
    fn write_errors_stop_compression() {
        // Fail partway through, while the workers still have queued chunks.
        // Cancellation from the write callback used to come back as
        // External.
        let errors: [fn() -> SpriteShrinkError; 2] = [
            || SpriteShrinkError::Cancelled,
            || io::Error::other("disk full").into(),
        ];
        for make_error in errors {
            let (hashes, chunks) = chunk_store(1200);
            let result = within_timeout(move || {
                let mut writes = 0;
                compress_chunks(
                    4,
                    dictionary(),
                    &hashes,
                    3,
                    read_from(chunks),
                    |_: &[u8]| -> Result<(), SpriteShrinkError> {
                        writes += 1;
                        if writes == 10 { Err(make_error()) } else { Ok(()) }
                    },
                )
            });

            match (make_error(), result) {
                (
                    SpriteShrinkError::Cancelled,
                    Err(ArchiveError::Cancelled),
                ) => {}
                (_, Err(ArchiveError::External(message)))
                    if message.contains("disk full") => {}
                (expected, result) => panic!("{expected}: {result:?}"),
            }
        }
    }

    #[test]
    fn a_corrupt_dictionary_fails_before_any_chunk_is_read() {
        // zstd accepts a dictionary with its magic number and a corrupt
        // body until it compresses the first frame. That used to fail every
        // worker on its first chunk with an allocation error.
        let mut corrupt = vec![0x37, 0xA4, 0x30, 0xEC];
        corrupt.extend_from_slice(&[0xFF; 60]);

        let result = within_timeout(move || {
            compress_chunks(
                4,
                &corrupt,
                &[0u64, 1, 2],
                3,
                |_: &[u64]| -> ReadResult {
                    panic!("nothing should be read")
                },
                |_: &[u8]| -> Result<(), SpriteShrinkError> {
                    panic!("nothing should be written")
                },
            )
        });
        assert!(
            matches!(result, Err(ArchiveError::Dictionary(_))),
            "{result:?}"
        );
    }

    #[test]
    fn a_worker_error_is_returned_when_every_worker_has_stopped() {
        // The coordinator used to wait forever for chunks from workers that
        // had stopped, and then hid their error behind "Worker pool
        // terminated unexpectedly". zstd has no input that fails once the
        // dictionary has loaded, so stand-in workers fail every job.
        let (hashes, chunks) = chunk_store(1200);
        let result = within_timeout(move || {
            let (to_compress_tx, to_compress_rx) =
                flume::bounded(REORDER_WINDOW);
            let (from_compress_tx, from_compress_rx) = flume::unbounded();

            let workers: Vec<_> = (0..4)
                .map(|_| {
                    let to_compress_rx: flume::Receiver<Run> =
                        to_compress_rx.clone();
                    let from_compress_tx = from_compress_tx.clone();
                    std::thread::spawn(move || {
                        let _job = to_compress_rx.recv();
                        let _ignored = from_compress_tx.send(Err(
                            ArchiveError::CompressionError("job failed".into()),
                        ));
                    })
                })
                .collect();
            drop(to_compress_rx);
            drop(from_compress_tx);

            let result = coordinate_chunks(
                &hashes,
                &|_: &u64| None,
                &read_from(chunks),
                |_: &[u8]| -> Result<(), ArchiveError> {
                    panic!("nothing should be written")
                },
                RunSplit { workers: 4, max_len: 1 },
                &to_compress_tx,
                &from_compress_rx,
            );
            drop(to_compress_tx);
            for worker in workers {
                worker.join().unwrap();
            }
            result
        });
        assert!(
            matches!(
                &result,
                Err(ArchiveError::CompressionError(message))
                    if message == "job failed"
            ),
            "{result:?}"
        );
    }

    #[test]
    fn a_slow_chunk_does_not_let_reading_run_ahead() {
        // Reading only waited for the workers' queue to run low. While one
        // slow chunk held up writing, the other workers kept emptying the
        // queue, so the coordinator kept reading and holding everything
        // after the slow chunk in memory.
        let (hashes, mut chunks) = chunk_store(6000);
        chunks.insert(0, tiled_bytes(0x5EED_5100, 4 * 1024 * 1024));

        let max_read_ahead = within_timeout(move || {
            let read = Cell::new(0usize);
            let written = Cell::new(0usize);
            let max_read_ahead = Cell::new(0usize);

            compress_chunks(
                4,
                dictionary(),
                &hashes,
                19,
                |requested: &[u64]| -> ReadResult {
                    read.set(read.get() + requested.len());
                    max_read_ahead
                        .set(max_read_ahead.get().max(read.get() - written.get()));
                    Ok(requested.iter().map(|hash| chunks[hash].clone()).collect())
                },
                |_: &[u8]| -> Result<(), SpriteShrinkError> {
                    written.set(written.get() + 1);
                    Ok(())
                },
            )
            .unwrap();
            max_read_ahead.get()
        });
        assert!(
            max_read_ahead <= REORDER_WINDOW,
            "read {max_read_ahead} chunks ahead of writing"
        );
    }

    #[test]
    fn a_batch_is_sized_from_known_sizes_or_the_largest_seen() {
        const MIB: usize = 1024 * 1024;
        let budget = PIPELINE_BYTE_BUDGET;
        let known = |size: &usize| Some(*size);
        let unknown = |_: &usize| None;

        // Known sizes: as many as fit beside what the window holds.
        assert_eq!(batch_len(&[MIB; 600], known, 0, 0, true), 64);
        assert_eq!(batch_len(&[MIB; 600], known, 0, budget - 3 * MIB, false), 3);
        // Small items: the batch limit, not the budget.
        assert_eq!(batch_len(&[2048; 600], known, 0, 0, true),
            PREFETCH_HIGH_THRESHOLD);
        // An item over the budget is read alone, and only into an empty
        // window.
        assert_eq!(batch_len(&[budget + 1; 2], known, 0, 0, true), 1);
        assert_eq!(batch_len(&[budget + 1; 2], known, 0, 0, false), 0);
        assert_eq!(batch_len(&[budget + 1; 2], known, 0, MIB, false), 0);

        // Unknown sizes: one item to learn, then the largest seen.
        assert_eq!(batch_len(&[0; 600], unknown, 0, 0, true), 1);
        assert_eq!(batch_len(&[0; 600], unknown, 4 * MIB, 0, true), 16);
    }

    /// What `stored_chunks` returns: the chunk list, the stored bytes, where
    /// each chunk is stored (start and length, by position), and the
    /// expected output.
    type StoredChunks =
        (Vec<SSAChunkMeta<u64>>, Vec<u8>, Vec<(usize, usize)>, Vec<u8>);

    /// A chunk list for `decompress_chunks_with`: `sizes.len()` ROM-like
    /// chunks of those sizes, each compressed with the shared dictionary
    /// and stored one after another.
    fn stored_chunks(sizes: &[usize]) -> StoredChunks {
        let mut metas = Vec::new();
        let mut blob = Vec::new();
        let mut locations = Vec::new();
        let mut expected = Vec::new();
        for (i, &size) in sizes.iter().enumerate() {
            let data = tiled_bytes(0x5EED_D000 + i as u64, size);
            let compressed = compress(&data, dictionary(), 1).unwrap();
            locations.push((blob.len(), compressed.len()));
            blob.extend_from_slice(&compressed);
            metas.push(SSAChunkMeta {
                hash: u64::from_bytes_with_seed(&data),
                offset: expected.len() as u64,
                length: u32::try_from(size).unwrap(),
            });
            expected.extend_from_slice(&data);
        }
        (metas, blob, locations, expected)
    }

    /// Decompresses `metas` with `workers` workers, reading each chunk at
    /// its position's location in `blob`. Returns the result and what was
    /// written, joined.
    fn decompress_with(
        workers: usize,
        metas: &[SSAChunkMeta<u64>],
        blob: &[u8],
        locations: &[(usize, usize)],
    ) -> (Result<(), SpriteShrinkError>, Vec<u8>) {
        // Chunk positions by hash, as a host's chunk index finds them.
        let by_hash: HashMap<u64, (usize, usize)> = metas
            .iter()
            .zip(locations)
            .map(|(meta, &location)| (meta.hash, location))
            .collect();
        let mut written = Vec::new();
        let result = decompress_chunks_with(
            workers,
            dictionary(),
            metas,
            |requested: &[SSAChunkMeta<u64>]| -> ReadResult {
                Ok(requested
                    .iter()
                    .map(|meta| {
                        let (start, len) = by_hash[&meta.hash];
                        blob[start..start + len].to_vec()
                    })
                    .collect())
            },
            |data: &[u8]| -> Result<(), SpriteShrinkError> {
                written.extend_from_slice(data);
                Ok(())
            },
        );
        (result, written)
    }

    #[test]
    fn chunks_decompress_in_order_with_any_worker_count() {
        // Uneven sizes so workers finish out of order; more than two
        // batches; one chunk repeated.
        let sizes: Vec<usize> =
            (0..1200).map(|i| 1 + (i * 7919) % 5000).collect();
        let (mut metas, blob, mut locations, mut expected) =
            stored_chunks(&sizes);
        metas.push(metas[3]);
        locations.push(locations[3]);
        expected.extend_from_within(
            usize::try_from(metas[3].offset).unwrap()..
                usize::try_from(metas[4].offset).unwrap(),
        );

        for workers in [1, 4] {
            let (result, written) =
                decompress_with(workers, &metas, &blob, &locations);
            result.unwrap();
            assert!(written == expected, "{workers} workers");
        }
    }

    #[test]
    fn a_damaged_chunk_stops_decompression_before_it_is_written() {
        let (metas, mut blob, locations, expected) =
            stored_chunks(&[3000; 600]);
        // Flip a byte in chunk 400's stored data until it no longer
        // decodes to the right bytes (zstd error or hash mismatch).
        let (start, len) = locations[400];
        blob[start + len / 2] ^= 0xFF;

        let (result, written) = decompress_with(4, &metas, &blob, &locations);
        assert!(
            matches!(
                result,
                Err(SpriteShrinkError::Io(_)
                    | SpriteShrinkError::Archive(
                        ArchiveError::ChunkHashMismatch { .. }
                        | ArchiveError::ChunkTooShort { .. }
                    ))
            ),
            "{result:?}"
        );
        assert!(written.len() <= 400 * 3000, "{} bytes", written.len());
        assert!(written == expected[..written.len()]);
    }

    #[test]
    fn a_cancelled_read_cancels_decompression() {
        let (metas, ..) = stored_chunks(&[100; 3]);
        let result = decompress_chunks_with(
            2,
            dictionary(),
            &metas,
            |_: &[SSAChunkMeta<u64>]| -> ReadResult {
                Err(SpriteShrinkError::Cancelled)
            },
            |_: &[u8]| -> Result<(), SpriteShrinkError> {
                panic!("nothing should be written")
            },
        );
        assert!(matches!(result, Err(SpriteShrinkError::Cancelled)), "{result:?}");
    }

    #[test]
    fn decompression_reads_ahead_no_more_than_the_byte_budget() {
        // 200 chunks of 512 KiB: the 1,000-chunk window alone would allow
        // reading 100 MiB ahead.
        let (metas, blob, locations, _) = stored_chunks(&[512 * 1024; 200]);
        let by_hash: HashMap<u64, (usize, usize)> = metas
            .iter()
            .zip(&locations)
            .map(|(meta, &location)| (meta.hash, location))
            .collect();

        let read = Cell::new(0usize);
        let written = Cell::new(0usize);
        let max_ahead = Cell::new(0usize);
        decompress_chunks_with(
            4,
            dictionary(),
            &metas,
            |requested: &[SSAChunkMeta<u64>]| -> ReadResult {
                read.set(read.get() + requested.len() * 512 * 1024);
                max_ahead.set(max_ahead.get().max(read.get() - written.get()));
                Ok(requested
                    .iter()
                    .map(|meta| {
                        let (start, len) = by_hash[&meta.hash];
                        blob[start..start + len].to_vec()
                    })
                    .collect())
            },
            |data: &[u8]| -> Result<(), SpriteShrinkError> {
                written.set(written.get() + data.len());
                Ok(())
            },
        )
        .unwrap();
        assert!(
            max_ahead.get() <= PIPELINE_BYTE_BUDGET,
            "read {} bytes ahead of writing",
            max_ahead.get()
        );
    }

    /// What `ArchiveBuilder::build` returned and passed to its callbacks.
    struct Built {
        result: Result<CompressionResult, SpriteShrinkError>,
        /// The data of every call to the write callback.
        writes: Vec<Vec<u8>>,
        /// Every report to the Rust progress callback, as its Debug text.
        progress: Vec<String>,
    }

    /// Builds `hashes` through `ArchiveBuilder` with 4 workers at level 19,
    /// reading through `read`. `c_progress` also sets the C progress
    /// callback, which records into `C_PROGRESS`.
    fn build_with(
        hashes: &[u64],
        total_size: u64,
        read: impl Fn(&[u64]) -> ReadResult,
        optimize: bool,
        c_progress: bool,
    ) -> Built {
        let mut writes = Vec::new();
        // The progress callback must be 'static, so it can't borrow.
        let progress = Arc::new(Mutex::new(Vec::new()));

        let mut builder = {
            let progress = Arc::clone(&progress);
            ArchiveBuilder::new(
                hashes,
                total_size,
                read,
                |data: &[u8]| -> Result<(), SpriteShrinkError> {
                    writes.push(data.to_vec());
                    Ok(())
                },
            )
            .with_progress(move |report| {
                progress.lock().unwrap().push(format!("{report:?}"));
            })
        };
        builder
            .compression_level(19)
            .worker_threads(4)
            .optimize_dictionary(optimize);
        if c_progress {
            builder.with_c_progress(record_c_progress, c_progress_user_data());
        }
        let result = builder.build();

        let progress = std::mem::take(&mut *progress.lock().unwrap());
        Built { result, writes, progress }
    }

    /// `count` chunks from `chunk_store`, with their total size.
    fn builder_input(count: u64) -> (Vec<u64>, HashMap<u64, Vec<u8>>, u64) {
        let (hashes, chunks) = chunk_store(count);
        let total_size = chunks.values().map(|chunk| chunk.len() as u64).sum();
        (hashes, chunks, total_size)
    }

    /// Checks that `built` succeeded and that every chunk in `chunks` can
    /// be found through the chunk index and decompressed with the
    /// dictionary.
    fn assert_round_trips(built: &Built, chunks: &HashMap<u64, Vec<u8>>) {
        let result = built.result.as_ref().unwrap();
        assert_eq!(result.dictionary_size, result.dictionary.len() as u64);
        assert_eq!(
            result.enc_chunk_index_size,
            result.enc_chunk_index.len() as u64
        );

        let blob: Vec<u8> = built
            .writes
            .iter()
            .flat_map(|data| data.iter().copied())
            .collect();
        let index =
            parse_file_chunk_index::<u64>(&result.enc_chunk_index).unwrap();
        assert_eq!(index.len(), chunks.len());

        for (hash, chunk) in chunks {
            let location = &index[hash];
            let start = usize::try_from(location.offset).unwrap();
            let end = start + location.compressed_length as usize;
            let decompressed = decompress_chunk(
                &blob[start..end],
                &result.dictionary,
                chunk.len(),
            )
            .unwrap();
            assert!(decompressed == *chunk, "chunk {hash}");
        }
    }

    #[test]
    fn build_round_trips_every_chunk_through_the_chunk_index() {
        let (hashes, chunks, total_size) = builder_input(300);
        let built =
            build_with(&hashes, total_size, read_from(chunks.clone()), false, false);

        assert_round_trips(&built, &chunks);
        // The input has enough distinct data to train on.
        assert_ne!(built.result.unwrap().dictionary.len(), 0);
    }

    #[test]
    fn an_optimized_dictionary_also_round_trips() {
        let (hashes, chunks, total_size) = builder_input(300);
        let built =
            build_with(&hashes, total_size, read_from(chunks.clone()), true, false);

        assert_round_trips(&built, &chunks);
        assert_ne!(built.result.unwrap().dictionary.len(), 0);
    }

    #[test]
    fn every_write_is_one_compressed_chunk() {
        // There used to be one more, empty write at the end, flagged as a
        // flush.
        let (hashes, chunks, total_size) = builder_input(300);
        let built =
            build_with(&hashes, total_size, read_from(chunks), false, false);
        built.result.unwrap();

        assert_eq!(built.writes.len(), hashes.len());
        assert!(built.writes.iter().all(|data| !data.is_empty()));
    }

    #[test]
    fn progress_is_reported_in_order_with_one_report_per_chunk() {
        let (hashes, chunks, total_size) = builder_input(300);
        let built =
            build_with(&hashes, total_size, read_from(chunks), false, false);
        built.result.unwrap();

        let mut expected = vec![
            "GeneratingDictionary".to_owned(),
            "DictionaryDone".to_owned(),
            "Compressing { total_chunks: 300 }".to_owned(),
        ];
        expected.extend(std::iter::repeat_n("ChunkCompressed".to_owned(), 300));
        expected.push("Finalizing".to_owned());
        assert_eq!(built.progress, expected);
    }

    /// Reports to `record_c_progress`: each one's type as a number, its
    /// `total_chunks` and the user data pointer it came with.
    static C_PROGRESS: Mutex<Vec<(u8, u64, usize)>> = Mutex::new(Vec::new());

    /// The address passed to the C progress callback as its user data. It's
    /// only compared, never dereferenced.
    static C_PROGRESS_MARKER: u8 = 0;

    fn c_progress_user_data() -> *mut c_void {
        (&raw const C_PROGRESS_MARKER).cast_mut().cast()
    }

    /// A C progress callback that records what it's given. It must not
    /// panic, since a panic can't unwind out of an `extern "C"` function,
    /// so a poisoned lock is recovered instead of unwrapped.
    extern "C" fn record_c_progress(progress: FFIProgress, user_data: *mut c_void) {
        C_PROGRESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((progress.ty as u8, progress.total_chunks, user_data as usize));
    }

    #[test]
    fn the_c_progress_callback_gets_every_report_too() {
        // It used to get every report but ChunkCompressed, so C hosts
        // couldn't show per-chunk progress.
        let (hashes, chunks, total_size) = builder_input(300);
        let built =
            build_with(&hashes, total_size, read_from(chunks), false, true);
        built.result.unwrap();

        let user_data = c_progress_user_data() as usize;
        let report = |ty: FFIProgressType, total_chunks| {
            (ty as u8, total_chunks, user_data)
        };
        let mut expected = vec![
            report(FFIProgressType::GeneratingDictionary, 0),
            report(FFIProgressType::DictionaryDone, 0),
            report(FFIProgressType::Compressing, 300),
        ];
        expected.extend(
            (0..300).map(|_| report(FFIProgressType::ChunkCompressed, 0)),
        );
        expected.push(report(FFIProgressType::Finalizing, 0));
        assert_eq!(*C_PROGRESS.lock().unwrap(), expected);
        // The Rust callback still gets the same reports.
        assert_eq!(built.progress.len(), expected.len());
    }

    #[test]
    fn a_cancelled_read_cancels_the_build() {
        // The dictionary samples are read one chunk at a time, and the
        // chunks to compress in batches, so the request size tells the
        // two phases apart.
        for (phase, cancel_when) in [
            ("sampling", (|len| len == 1) as fn(usize) -> bool),
            ("compression", |len| len > 1),
        ] {
            let (hashes, chunks, total_size) = builder_input(300);
            let read = read_from(chunks);
            let built = build_with(
                &hashes,
                total_size,
                move |requested| {
                    if cancel_when(requested.len()) {
                        return Err(SpriteShrinkError::Cancelled);
                    }
                    read(requested)
                },
                false,
                false,
            );
            let error = built.result.unwrap_err();
            assert!(error.is_cancelled(), "{phase}: {error:?}");
        }
    }

    #[test]
    fn a_cancelled_write_cancels_the_build() {
        let (hashes, chunks, total_size) = builder_input(300);
        let result = ArchiveBuilder::new(
            &hashes,
            total_size,
            read_from(chunks),
            |_: &[u8]| -> Result<(), SpriteShrinkError> {
                Err(SpriteShrinkError::Cancelled)
            },
        )
        .build();
        assert!(result.unwrap_err().is_cancelled());
    }
}
