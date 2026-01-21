use std::{
    collections::{HashMap, VecDeque},
    fmt::Display,
    fs::File, thread,
    sync::{Arc, Mutex, Condvar},
    thread::{Scope, ScopedJoinHandle},
};

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

use crate::{
    flac::decompress_flac_block,
    lib_error_handling::SpriteShrinkCDError,
};

use flume::Sender;
use sprite_shrink::{
    ChunkLocation, Hashable,
    decompress_chunk
};
use thiserror::Error;


#[derive(Error, Debug)]
pub enum UtilError {
    #[error("Hash not found for {0} chunk")]
    MissingHash(String),


}


pub type SharedBuffer = Arc<(Mutex<(VecDeque<u8>, bool)>, Condvar)>;

struct WorkerGuard {
       shared_state: SharedBuffer,
   }


impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.shared_state;

        let mut state = match lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.1 = true;
        cvar.notify_all();
    }
}



pub fn spawn_data_fetcher<H, D, E>(
    hash_list: Vec<H>,
    get_data: D,
    tx: flume::Sender<Result<Vec<Vec<u8>>, SpriteShrinkCDError>>,
    batch_size: usize,
) -> thread::JoinHandle<()>
where
    H: Hashable,
    D: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    thread::spawn(move || {
        for hashes in hash_list.chunks(batch_size) {
            match get_data(hashes) {
                Ok(data) => {
                    if tx.send(Ok(data)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Data fetcher callback error: {}", e);
                    let _ = tx.send(Err(
                        SpriteShrinkCDError::External(Box::new(e))
                    ));
                    break;
                }
            }
        }
    })
}


pub fn spawn_audio_decomp_worker<'scope, 'env, H>(
    scope: &'scope Scope<'scope, 'env>,
    hash_list: Vec<H>,
    audio_index: &'env HashMap<H, ChunkLocation>,
    archive_file: &'env File,
    shared_state: SharedBuffer,
    max_buffer_size: usize,
    error_tx: Sender<SpriteShrinkCDError>,
) -> ScopedJoinHandle<'scope, ()>
where
    H: Hashable + Display + Send + Sync + 'static,
{
    let resume_threshold = max_buffer_size / 2;

    scope.spawn(move || {
        let _guard = WorkerGuard {
            shared_state: shared_state.clone(),
        };

        for hash in hash_list {

            let result = (|| -> Result<Vec<u8>, SpriteShrinkCDError> {
                let chunk_loc = audio_index.get(&hash)
                    .ok_or_else(|| UtilError::MissingHash(hash.to_string()))?;

                let mut comp_data = vec![0u8; chunk_loc.compressed_length as usize];

                #[cfg(unix)]
                archive_file.read_at(&mut comp_data, chunk_loc.offset)?;
                #[cfg(windows)]
                archive_file.seek_read(&mut comp_data, chunk_loc.offset)?;

                Ok(decompress_flac_block(&comp_data)?)
            })();


            match result {
                Ok(data) => {
                    let data_len = data.len();
                    let (lock, cvar) = &*shared_state;

                    let mut state = lock.lock().unwrap();

                    if state.0.len() >= max_buffer_size {
                        while state.0.len() > resume_threshold {
                            state = cvar.wait(state).unwrap();
                        }
                    }

                    while state.0.len() + data_len > max_buffer_size {
                        if state.0.is_empty() {
                            break;
                        }
                        state = cvar.wait(state).unwrap();

                    }

                    state.0.extend(data);
                    cvar.notify_all();
                }
                Err(e) => {
                    let _ = error_tx.send(e);
                    break;
                }
            }
        }
    })
}


pub fn spawn_chunk_decomp_worker<'scope, 'env, H>(
    scope: &'scope thread::Scope<'scope, 'env>,
    hash_list: Vec<H>,
    chunk_index: &'env HashMap<H, ChunkLocation>,
    dictionary: &'env [u8],
    archive_file: &'env File,
    shared_state: SharedBuffer,
    max_buffer_size: usize,
    error_tx: Sender<SpriteShrinkCDError>,
) -> ScopedJoinHandle<'scope, ()>
where
    H: Hashable + Display + Send + Sync + 'static,
{
    let resume_threshold = max_buffer_size / 2;

    scope.spawn(move || {
        let _guard = WorkerGuard {
            shared_state: shared_state.clone(),
        };

        for hash in hash_list {
            let result = (|| -> Result<Vec<u8>, SpriteShrinkCDError> {
                let chunk_loc = chunk_index.get(&hash)
                    .ok_or_else(|| UtilError::MissingHash(hash.to_string()))?;

                let mut comp_data = vec![0u8; chunk_loc.compressed_length as usize];

                #[cfg(unix)]
                if let Err(e) = archive_file.read_at(&mut comp_data, chunk_loc.offset) {
                    return Err(SpriteShrinkCDError::External(Box::new(e)));
                }
                #[cfg(windows)]
                if let Err(e) = archive_file.seek_read(&mut comp_data, chunk_loc.offset) {
                    return Err(SpriteShrinkCDError::External(Box::new(e)));
                }

                decompress_chunk(
                    &comp_data,
                    dictionary
                ).map_err(|e| SpriteShrinkCDError::External(Box::new(e)))
            })();


            match result {
                Ok(data) => {
                    let data_len = data.len();
                    let (lock, cvar) = &*shared_state;

                    let mut state = lock.lock().unwrap();

                    if state.0.len() >= max_buffer_size {
                        while state.0.len() > resume_threshold {
                            state = cvar.wait(state).unwrap();
                        }
                    }

                    while state.0.len() + data_len > max_buffer_size {
                        if state.0.is_empty() {
                            break;
                        }
                        state = cvar.wait(state).unwrap();
                    }

                    state.0.extend(data);
                    cvar.notify_all();
                }
                Err(e) => {
                    let _ = error_tx.send(e);
                    break;
                }
            }
        }
    })
}


pub fn pull_data(
    shared_state: &SharedBuffer,
    amount: usize,
    error_rx: &flume::Receiver<SpriteShrinkCDError>,
) -> Result<Vec<u8>, SpriteShrinkCDError>  {
    let (lock, cvar) = &**shared_state;
    let mut state = lock.lock().unwrap();

    while state.0.len() < amount && !state.1 {
        state = cvar.wait(state).unwrap();

        if let Ok(err) = error_rx.try_recv() {
            return Err(err);
        }
    }

    let to_pull = std::cmp::min(amount, state.0.len());
    let data: Vec<u8> = state.0.drain(..to_pull).collect();

    cvar.notify_all();

    Ok(data)
}
