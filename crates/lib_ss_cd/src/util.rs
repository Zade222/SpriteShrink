use std::{
    collections::{HashMap, VecDeque},
    fmt::Display,
    fs::File, thread,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
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


/// What a decompression worker and `write_disc`'s sector loop share.
pub struct BufferState {
    /// Decompressed bytes the worker has produced and the loop hasn't
    /// consumed yet.
    pub data: VecDeque<u8>,
    /// The worker has stopped producing: it finished, failed or was told
    /// the loop stopped.
    pub producer_done: bool,
    /// The loop has stopped consuming. A worker waiting for room would
    /// otherwise wait forever, so it stops instead.
    pub consumer_stopped: bool,
}

impl BufferState {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity),
            producer_done: false,
            consumer_stopped: false,
        }
    }
}

pub type SharedBuffer = Arc<(Mutex<BufferState>, Condvar)>;

/// Locks a buffer's state, recovering it if another thread panicked while
/// holding the lock.
///
/// Recovering is sound because no invariant spans the state's fields: the
/// deque is still a valid deque if a panic interrupts an `extend` or a
/// `drain`, and the flags are plain stores. The panic itself isn't lost: a
/// worker that panicked is reported as `ThreadPanic` when `write_disc` joins
/// it.
fn lock_state(lock: &Mutex<BufferState>) -> MutexGuard<'_, BufferState> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits on `cvar`, recovering the state as [`lock_state`] does.
fn wait_state<'a>(
    cvar: &Condvar,
    state: MutexGuard<'a, BufferState>,
) -> MutexGuard<'a, BufferState> {
    cvar.wait(state).unwrap_or_else(PoisonError::into_inner)
}

struct WorkerGuard {
       shared_state: SharedBuffer,
   }


impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.shared_state;

        let mut state = lock_state(lock);
        state.producer_done = true;
        cvar.notify_all();
    }
}

/// Tells the workers feeding `buffers` that the consumer has stopped, when
/// dropped.
///
/// `write_disc` creates one inside its thread scope, so it drops on every
/// return from the sector loop, early or not, before the scope joins the
/// workers. Without it, a worker waiting for buffer room after the loop
/// returned early would never wake, and the join would hang.
pub struct ConsumerGuard<'a> {
    pub buffers: [&'a SharedBuffer; 2],
}

impl Drop for ConsumerGuard<'_> {
    fn drop(&mut self) {
        for buffer in self.buffers {
            let (lock, cvar) = &**buffer;
            let mut state = lock_state(lock);
            state.consumer_stopped = true;
            cvar.notify_all();
        }
    }
}

/// Waits until the buffer has room for `data`, then appends it.
///
/// The buffer may hold up to `max_buffer_size` bytes, or one larger piece of
/// data when it's empty. Once it's full, the worker waits until the consumer
/// has drained it to half. Returns `false`, without appending, if the
/// consumer has stopped, so the worker can stop too.
fn push_data(
    shared_state: &SharedBuffer,
    data: Vec<u8>,
    max_buffer_size: usize,
) -> bool {
    let resume_threshold = max_buffer_size / 2;
    let (lock, cvar) = &**shared_state;
    let mut state = lock_state(lock);

    if state.data.len() >= max_buffer_size {
        while state.data.len() > resume_threshold && !state.consumer_stopped {
            state = wait_state(cvar, state);
        }
    }

    while state.data.len() + data.len() > max_buffer_size
        && !state.data.is_empty()
        && !state.consumer_stopped
    {
        state = wait_state(cvar, state);
    }

    if state.consumer_stopped {
        return false;
    }

    state.data.extend(data);
    cvar.notify_all();
    true
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
                    if !push_data(&shared_state, data, max_buffer_size) {
                        break;
                    }
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
                    if !push_data(&shared_state, data, max_buffer_size) {
                        break;
                    }
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
    let mut state = lock_state(lock);

    while state.data.len() < amount && !state.producer_done {
        state = wait_state(cvar, state);

        if let Ok(err) = error_rx.try_recv() {
            return Err(err);
        }
    }

    let to_pull = std::cmp::min(amount, state.data.len());
    let data: Vec<u8> = state.data.drain(..to_pull).collect();

    cvar.notify_all();

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer whose lock was poisoned by a thread that panicked while
    /// holding it, after appending `data`.
    fn poisoned_buffer(data: &[u8]) -> SharedBuffer {
        let buffer: SharedBuffer =
            Arc::new((Mutex::new(BufferState::with_capacity(0)), Condvar::new()));
        let shared = Arc::clone(&buffer);
        let data = data.to_vec();
        let result = thread::spawn(move || {
            let mut state = shared.0.lock().unwrap();
            state.data.extend(data);
            panic!("worker panicked while holding the buffer lock");
        })
        .join();
        assert!(result.is_err());
        assert!(buffer.0.is_poisoned());
        buffer
    }

    #[test]
    fn a_poisoned_buffer_can_still_be_read_and_written() {
        let buffer = poisoned_buffer(b"abc");
        let (_error_tx, error_rx) = flume::unbounded();

        assert!(push_data(&buffer, b"de".to_vec(), 1024));
        assert_eq!(pull_data(&buffer, 5, &error_rx).unwrap(), b"abcde");
    }

    #[test]
    fn a_poisoned_buffer_still_reports_a_stopped_consumer() {
        let buffer = poisoned_buffer(b"");
        drop(ConsumerGuard { buffers: [&buffer, &buffer] });

        assert!(!push_data(&buffer, b"x".to_vec(), 1024));
    }
}
