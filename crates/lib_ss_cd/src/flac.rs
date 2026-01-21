use std::{
    collections::{VecDeque},
    io::{Cursor, Read, Write},
    sync::{Arc, Mutex},
    thread
};

use flac_codec::{
    byteorder::LittleEndian,
    decode::FlacByteReader,
    encode::{FlacByteWriter, Options},
};
use flume::{bounded, unbounded};
use thiserror::Error;

use crate::{
    lib_error_handling::SpriteShrinkCDError,
};

use sprite_shrink::{ChunkLocation, Hashable};


#[derive(Error, Debug)]
pub enum FlacError {
    #[error("Flac write buffer error.")]
    Buffer,

    #[error("Flac decode error: {0}")]
    Decode(String),

    #[error("An error occurred in an external callback: {0}")]
    External(String),

    #[error("Flac finalization error: {0}")]
    Finalization(String),

    #[error("Worker Error {0}")]
    Worker(String),

    #[error("Flac init error: {0}")]
    Init(String),
}


pub fn compress_audio_block(
    audio_block: &[u8],
    best_comp: bool
) -> Result<Vec<u8>, FlacError> {
    let mut flac_dest = Cursor::new(Vec::with_capacity(
        (audio_block.len() * 3) / 5
    ));

    let options = if best_comp {
        Options::best()
    } else {
        Options::fast()
    };

    let mut writer = FlacByteWriter::<_, LittleEndian>::new_cdda(
        &mut flac_dest,
        options,
        Some(audio_block.len() as u64)
    ).map_err(|e| FlacError::Init(e.to_string()))?;

    writer.write_all(audio_block).map_err(|_| FlacError::Buffer)?;
    writer.finalize().map_err(|e| FlacError::Finalization(e.to_string()))?;

    Ok(flac_dest.into_inner())
}


pub fn decompress_flac_block(
    flac_block: &[u8]
) -> Result<Vec<u8>, FlacError> {
    let mut reader = FlacByteReader::endian(
        flac_block,
        LittleEndian
    ).map_err(|e| FlacError::Init(e.to_string()))?;

    let mut audio_bytes = Vec::new();

    reader.read_to_end(&mut audio_bytes)
        .map_err(|e| FlacError::Decode(e.to_string()))?;

    Ok(audio_bytes)
}


pub fn compress_audio_blocks<A, E, H, W>(
    audio_block_hashes: Vec<H>,
    get_audio_blocks_cb: A,
    com_block_write_cb: W,
    worker_count: usize,
) -> Result<Vec<(H, ChunkLocation)>, SpriteShrinkCDError>
where
    A: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
    H: Hashable,
    W: FnMut(&[u8], bool) -> Result<(), E> + Send + Sync + 'static,
{
    const PREFETCH_LOW_THRESHOLD: usize = 100;
    const PREFETCH_HIGH_THRESHOLD: usize = 500;

    let total_blocks = audio_block_hashes.len();

    let audio_block_hashes_arc: Arc<[H]> = Arc::from(audio_block_hashes);

    let (to_compress_tx, to_compress_rx) =
        bounded::<(usize, H, Vec<u8>)>(PREFETCH_HIGH_THRESHOLD);

    let (from_compress_tx, from_compress_rx) =
        unbounded::<(usize, H, Vec<u8>)>();

    let (err_tx, err_rx) = std::sync::mpsc::channel::<FlacError>();

    let mut worker_handles = Vec::with_capacity(worker_count);

    let num_threads = if worker_count == 0 {
        std::thread::available_parallelism()?.get()
    } else {
        worker_count
    };

    // Resolve the number of threads to use. If 0, use available parallelism.
    let num_workers = (num_threads).saturating_sub(1).max(1);

    for _ in 0..num_workers {
        //Clone necessary data to be used safely.
        let to_compress_rx_clone = to_compress_rx.clone();
        let from_compress_tx_clone = from_compress_tx.clone();
        let err_tx_clone = err_tx.clone();

        let handle = thread::spawn(move ||{
            while let Ok((batch_idx, hash, block)) = to_compress_rx_clone.recv() {
                match compress_audio_block(
                    &block,
                    true
                ) {
                    Ok(compress_block) => {
                        if from_compress_tx_clone.send((
                            batch_idx,
                            hash,
                            compress_block
                        )).is_err() {
                            //The I/O thread has terminated; exit gracefully.
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = err_tx_clone.send(e);
                        break;
                    }
                }
            }
        });
        worker_handles.push(handle);
    }

    drop(to_compress_rx);

    drop(from_compress_tx);

    let abha_clone = Arc::clone(&audio_block_hashes_arc);

    let audio_block_index = Arc::new(Mutex::new(Vec::with_capacity(total_blocks)));

    let block_index_clone = Arc::clone(&audio_block_index);

    let io_handle = thread::spawn(move || -> Result<(), FlacError> {
        let mut next_needed_idx = 0usize;
        let mut read_cursor = 0usize;
        let mut reorder_buffer: VecDeque<Option<(H, Vec<u8>)>> =
            VecDeque::with_capacity(num_workers * 2);
        let mut offset = 0u64;

        let mut com_block_write_cb = com_block_write_cb;

        while next_needed_idx < total_blocks {
            if to_compress_tx.len() < PREFETCH_LOW_THRESHOLD &&
            read_cursor < total_blocks
            {
                let end = (read_cursor + PREFETCH_HIGH_THRESHOLD)
                    .min(total_blocks);

                let hashes_to_read = &abha_clone[read_cursor..end];

                let blocks = match get_audio_blocks_cb(hashes_to_read) {
                    Ok(blocks) => blocks,
                    Err(e) => {
                        //if canceled, return error canceled

                        return Err(FlacError::Worker(e.to_string()));
                    }
                };

                for (i, block) in blocks.into_iter().enumerate() {
                    let global_idx = read_cursor + i;
                    let hash = hashes_to_read[i];

                    if to_compress_tx.send((global_idx, hash, block)).is_err() {
                        return Err(FlacError::Finalization("Worker pool died".into()));
                    }
                }
                read_cursor = end;
            }

            match from_compress_rx.recv() {
                Ok((idx, hash, comp_data)) => {
                    if idx >= next_needed_idx {
                        let relative = idx - next_needed_idx;

                        while reorder_buffer.len() <= relative {
                            reorder_buffer.push_back(None);
                        }

                        reorder_buffer[relative] = Some((hash, comp_data));
                    }
                }
                Err(_) => {
                    if next_needed_idx < total_blocks {
                         return Err(FlacError::Worker(
                            "Worker pool finished but not all chunks \
                            were processed.".to_string()));
                    }
                    break;
                }
            }

            while let Some(Some(_)) = reorder_buffer.front() {
                if let Some((hash, comp_block)) = reorder_buffer.pop_front().unwrap() {
                    let comp_block_len = comp_block.len() as u64;
                    if let Err(e) = com_block_write_cb(
                        &comp_block,
                        false
                    ) {
                        return Err(FlacError::External(e.to_string()));
                    }


                    let mut bi_clone = block_index_clone
                        .lock()
                        .unwrap();

                    bi_clone.push((
                        hash,
                        ChunkLocation {
                            offset,
                            compressed_length: comp_block_len as u32,
                        }
                    ));
                    offset += comp_block_len;
                }

                next_needed_idx += 1;
            }

        }

        let _ = com_block_write_cb(&[], true);


        Ok(())
    });

    loop {
        if io_handle.is_finished() {
            break;
        }

        if let Ok(err) = err_rx.try_recv() {
            return Err(err.into());
        }
    }

    for handle in worker_handles {
        handle.join().expect("A compression worker thread panicked.");
    }

    io_handle.join().expect("The I/O thread panicked.")?;

    let audio_block_index = Arc::try_unwrap(audio_block_index)
        .expect("Arc should be uniquely owned here")
        .into_inner()
        .expect("Mutex should not be poisoned");

    Ok(audio_block_index)
}
