use std::{
    error::Error as StdError,
    thread,
    thread::{Scope, ScopedJoinHandle},
};

use crate::lib_error_handling::SpriteShrinkCDError;

use sprite_shrink::{ChunkLocation, Hashable};
use thiserror::Error;


#[derive(Error, Debug)]
pub enum UtilError {
    #[error("Hash not found for {0} chunk")]
    MissingHash(String),

    #[error("Reading the {expected} bytes stored at offset {offset} \
        returned {actual} bytes.")]
    WrongReadLength { offset: u64, expected: u32, actual: usize },

    #[error("A decoding thread stopped before sending its data.")]
    DecoderStopped,
}


/// Calls the host's `read_stored` for the bytes at `location` and checks
/// it returned all of them. An error from the host is kept as
/// `SpriteShrinkCDError::External`.
///
/// # Errors
///
/// Returns the host's error, or `UtilError::WrongReadLength` if it
/// returned more or fewer bytes than `location` records.
pub fn read_stored_at<R, E>(
    read_stored: &R,
    location: ChunkLocation,
) -> Result<Vec<u8>, SpriteShrinkCDError>
where
    R: Fn(ChunkLocation) -> Result<Vec<u8>, E>,
    E: StdError + Send + Sync + 'static,
{
    let data = read_stored(location)
        .map_err(|e| SpriteShrinkCDError::External(Box::new(e)))?;
    if data.len() != location.compressed_length as usize {
        return Err(UtilError::WrongReadLength {
            offset: location.offset,
            expected: location.compressed_length,
            actual: data.len(),
        }.into());
    }
    Ok(data)
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


/// A run of data chunks ends at this many chunks. A chunk decodes in about
/// 2 µs, about what handing it to another thread costs, so workers get
/// chunks in runs.
pub const DATA_RUN_ITEMS: usize = 64;

/// A run of audio blocks is one block: decoding a block takes far longer
/// than handing it over, and single blocks spread the work most evenly.
pub const AUDIO_RUN_ITEMS: usize = 1;

/// A run also ends once its decoded length reaches this many bytes, so
/// large chunks still spread across the workers.
const RUN_BYTE_TARGET: usize = 128 * 1024;

/// How far ahead of the reader the data chunk decoders work, in decoded
/// bytes, shared among them. zstd decodes much faster than the sector loop
/// rebuilds sectors, so a few runs refill quickly; decoding further ahead
/// only keeps more cores busy, which lowers the clock and the cache hits
/// of the sector loop.
pub const DATA_READ_AHEAD: usize = 2 * 1024 * 1024;

/// How far ahead the audio block decoders work. FLAC decodes slower than
/// the sector loop consumes audio, so blocks decoded while the loop is in
/// a data region are time saved when it reaches the audio.
pub const AUDIO_READ_AHEAD: usize = 16 * 1024 * 1024;

/// The most decoded bytes the workers of one stream may hold, whatever
/// the read-ahead: the worker count is lowered so that two runs per worker
/// (one decoding, one waiting) fit, which only matters for large chunks.
const IN_FLIGHT_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// The items for one worker's turn: each item and its decoded length.
type Run<T> = Vec<(T, usize)>;

/// What a worker sends back for a run: each item's decoded bytes, or the
/// error that stopped it.
type RunResult = Result<Vec<Vec<u8>>, SpriteShrinkCDError>;

/// Splits `items` into runs of at most `max_items` items, ending a run
/// early once its decoded length reaches `RUN_BYTE_TARGET`. Every run
/// holds at least one item.
fn split_runs<T>(items: Vec<(T, usize)>, max_items: usize) -> Vec<Run<T>> {
    let mut runs = Vec::new();
    let mut run = Vec::new();
    let mut run_bytes = 0usize;
    for (item, len) in items {
        run_bytes = run_bytes.saturating_add(len);
        run.push((item, len));
        if run.len() >= max_items || run_bytes >= RUN_BYTE_TARGET {
            runs.push(std::mem::take(&mut run));
            run_bytes = 0;
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs
}

/// The decoded length of the longest run in `runs`.
fn largest_run<T>(runs: &[Run<T>]) -> usize {
    runs.iter()
        .map(|run| run.iter().fold(0usize, |total, (_, len)| total.saturating_add(*len)))
        .max()
        .unwrap_or(0)
}

/// How many workers to start for `runs`: `requested` (at least 1), no
/// more than there are runs, and few enough that two runs per worker fit
/// in `IN_FLIGHT_BYTE_BUDGET`.
fn worker_count_for<T>(runs: &[Run<T>], requested: usize) -> usize {
    let affordable = IN_FLIGHT_BYTE_BUDGET / largest_run(runs).saturating_mul(2).max(1);
    requested.min(affordable).min(runs.len()).max(1)
}

/// How many runs each of `workers` workers' channels holds: each worker's
/// share of `read_ahead`, in runs, less the one it's decoding. At least 1.
fn channel_depth_for<T>(runs: &[Run<T>], workers: usize, read_ahead: usize) -> usize {
    let per_worker = read_ahead / workers.max(1);
    (per_worker / largest_run(runs).max(1)).saturating_sub(1).max(1)
}

/// Decodes `items` on several worker threads, and returns the stream the
/// caller reads the decoded bytes from, in item order, with the workers'
/// handles.
///
/// `items` holds each item and its decoded length, in the order the bytes
/// are needed. They're split into runs (see `split_runs`); run `r` goes to
/// worker `r % workers`, and each worker sends its runs back on its own
/// channel, sized by `channel_depth_for`. Reading the channels in turn
/// returns the runs in order without a reorder buffer.
///
/// Each worker makes its state with `new_state` once, when it decodes its
/// first item, and passes it to every `decode` call. An error, from
/// either, replaces its whole run and reaches the reader at that run's
/// turn: the bytes before it are those of the earlier runs, in order.
/// Dropping the stream closes the channels, so a worker waiting to send
/// stops; join the handles after that.
pub fn spawn_ordered_decoders<'scope, 'env, T, S, N, D>(
    scope: &'scope Scope<'scope, 'env>,
    items: Vec<(T, usize)>,
    max_run_items: usize,
    read_ahead: usize,
    worker_count: usize,
    new_state: &'env N,
    decode: &'env D,
) -> (OrderedStream, Vec<ScopedJoinHandle<'scope, ()>>)
where
    T: Send + 'scope,
    N: Fn() -> Result<S, SpriteShrinkCDError> + Sync,
    D: Fn(&mut S, &T, usize) -> Result<Vec<u8>, SpriteShrinkCDError> + Sync,
{
    let runs = split_runs(items, max_run_items);
    let run_count = runs.len();
    let workers = worker_count_for(&runs, worker_count);
    let depth = channel_depth_for(&runs, workers, read_ahead);

    let mut assigned: Vec<Vec<Run<T>>> = (0..workers).map(|_| Vec::new()).collect();
    for (run, slot) in runs.into_iter().zip((0..workers).cycle()) {
        if let Some(worker_runs) = assigned.get_mut(slot) {
            worker_runs.push(run);
        }
    }

    let mut receivers = Vec::with_capacity(workers);
    let mut handles = Vec::with_capacity(workers);
    for worker_runs in assigned {
        let (tx, rx) = flume::bounded::<RunResult>(depth);
        receivers.push(rx);
        handles.push(scope.spawn(move || {
            let mut state = None;
            for run in worker_runs {
                let result = (|| -> RunResult {
                    let state = match &mut state {
                        Some(state) => state,
                        None => state.insert(new_state()?),
                    };
                    run.iter().map(|(item, len)| decode(state, item, *len)).collect()
                })();
                let failed = result.is_err();
                /*A failed send means the reader has stopped, and after an
                error there's nothing more to do.*/
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
        }));
    }

    let stream = OrderedStream {
        receivers,
        next_run: 0,
        run_count,
        buffer: Vec::new(),
        start: 0,
    };
    (stream, handles)
}

/// The decoded bytes of one stream, in order, from the workers
/// `spawn_ordered_decoders` started.
///
/// It keeps the bytes of at most one run beyond what's been taken, so the
/// workers' channels and runs bound its memory.
#[derive(Debug)]
pub struct OrderedStream {
    receivers: Vec<flume::Receiver<RunResult>>,
    /// The run to receive next; it comes from worker `next_run % workers`.
    next_run: usize,
    run_count: usize,
    /// Received bytes; those before `start` have been taken.
    buffer: Vec<u8>,
    start: usize,
}

impl OrderedStream {
    /// Takes the next `amount` bytes, receiving runs as needed, and lends
    /// them until the next call. Returns fewer only if the stream has
    /// ended.
    ///
    /// # Errors
    ///
    /// The error a worker hit, at its run's turn, or
    /// `UtilError::DecoderStopped` if a worker stopped without sending its
    /// run (it panicked; joining it raises the panic).
    pub fn take(&mut self, amount: usize) -> Result<&[u8], SpriteShrinkCDError> {
        while self.buffer.len() - self.start < amount && self.next_run < self.run_count {
            let decoded = self.receivers
                .get(self.next_run % self.receivers.len().max(1))
                .ok_or(UtilError::DecoderStopped)?
                .recv()
                .map_err(|_disconnected| UtilError::DecoderStopped)??;
            self.next_run += 1;

            /*Drop the bytes already taken before adding more, so the
            buffer holds one run plus a partial sector, not the whole
            stream.*/
            self.buffer.drain(..self.start);
            self.start = 0;
            for data in decoded {
                self.buffer.extend_from_slice(&data);
            }
        }

        let start = self.start;
        let end = self.buffer.len().min(start.saturating_add(amount));
        self.start = end;
        Ok(self.buffer.get(start..end).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn a_stored_read_must_return_every_byte_it_asks_for() {
        let location = ChunkLocation { offset: 8, compressed_length: 3 };
        let returning = |len: usize| {
            move |_: ChunkLocation| Ok::<_, io::Error>(vec![0; len])
        };

        assert_eq!(read_stored_at(&returning(3), location).unwrap(), [0; 3]);

        for len in [2, 4] {
            let Err(SpriteShrinkCDError::Util(UtilError::WrongReadLength {
                offset: 8, expected: 3, actual,
            })) = read_stored_at(&returning(len), location) else {
                panic!("a read returning {len} of 3 bytes was accepted");
            };
            assert_eq!(actual, len);
        }

        let failing = |_| Err(io::Error::other("host read failed"));
        let err = read_stored_at(&failing, location).unwrap_err();
        assert!(err.to_string().contains("host read failed"), "{err}");
    }

    /// Decodes `items` with `workers` workers in runs of `run_items` and
    /// takes the stream `step` bytes at a time until it ends or fails.
    /// Each item decodes to its value repeated `len` times; item 13 fails.
    /// Odd items take longer, so workers finish out of turn.
    fn take_all(
        items: Vec<(u8, usize)>,
        run_items: usize,
        workers: usize,
        step: usize,
    ) -> (Vec<u8>, Option<SpriteShrinkCDError>) {
        let new_state = || Ok(0_usize);
        let decode = |decoded: &mut usize, item: &u8, len: usize| {
            if *item == 13 {
                return Err(UtilError::MissingHash("13".to_string()).into());
            }
            if item % 2 == 1 {
                thread::sleep(std::time::Duration::from_micros(200));
            }
            *decoded += 1;
            Ok(vec![*item; len])
        };
        thread::scope(|scope| {
            let (mut stream, handles) = spawn_ordered_decoders(
                scope, items, run_items, AUDIO_READ_AHEAD, workers, &new_state, &decode,
            );
            let mut taken = Vec::new();
            let mut error = None;
            loop {
                match stream.take(step) {
                    Ok([]) => break,
                    Ok(bytes) => taken.extend_from_slice(bytes),
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                }
            }
            drop(stream);
            for handle in handles {
                handle.join().unwrap();
            }
            (taken, error)
        })
    }

    #[test]
    fn decoded_bytes_come_out_in_item_order_with_any_worker_count() {
        let items: Vec<(u8, usize)> = (0..12).chain(14..60)
            .map(|item| (item, usize::from(item % 5) * 700 + 1))
            .collect();
        let expected: Vec<u8> = items.iter()
            .flat_map(|&(item, len)| vec![item; len])
            .collect();

        for (run_items, workers, step) in [(1, 1, 2352), (1, 4, 2048), (64, 4, 2352), (3, 7, 1)] {
            let (taken, error) = take_all(items.clone(), run_items, workers, step);
            assert!(error.is_none(), "{error:?}");
            assert!(taken == expected, "runs of {run_items}, {workers} workers");
        }
    }

    #[test]
    fn an_error_comes_at_its_runs_turn_after_the_runs_before_it() {
        // Runs of two: item 13 fails, so its run (items 12 and 13) is
        // replaced by the error. Items 0 to 11 come first, in order.
        let items: Vec<(u8, usize)> = (0..30).map(|item| (item, 100)).collect();
        let before: Vec<u8> = (0..12).flat_map(|item| vec![item; 100]).collect();

        let (taken, error) = take_all(items, 2, 4, 64);

        assert!(before.starts_with(&taken), "{} bytes taken", taken.len());
        assert!(taken.len() > before.len() - 64, "{} bytes taken", taken.len());
        assert!(
            matches!(error, Some(SpriteShrinkCDError::Util(UtilError::MissingHash(_)))),
            "{error:?}"
        );
    }

    #[test]
    fn dropping_the_stream_early_stops_the_workers() {
        // No read-ahead, so each channel holds one run, and each worker has
        // more runs than that: all of them are waiting to send when the
        // stream is dropped, and the joins would hang if a closed channel
        // didn't stop them.
        let new_state = || Ok(());
        let decode = |(): &mut (), item: &u8, len: usize| Ok(vec![*item; len]);
        let items: Vec<(u8, usize)> = (0..=255).map(|item| (item, 64)).collect();

        thread::scope(|scope| {
            let (mut stream, handles) =
                spawn_ordered_decoders(scope, items, 1, 0, 4, &new_state, &decode);
            assert_eq!(stream.take(64).unwrap(), [0; 64]);
            drop(stream);
            for handle in handles {
                handle.join().unwrap();
            }
        });
    }

    #[test]
    fn runs_end_at_their_item_or_byte_limit_and_cap_the_workers() {
        let small: Vec<((), usize)> = vec![((), 2048); 200];
        let runs = split_runs(small, DATA_RUN_ITEMS);
        let lengths: Vec<usize> = runs.iter().map(Vec::len).collect();
        // 64 chunks of 2 KiB reach the 128 KiB target together.
        assert_eq!(lengths, [64, 64, 64, 8]);
        assert_eq!(worker_count_for(&runs, 23), 4);

        // 16 MiB items: one per run, and two runs per worker in 64 MiB.
        let large: Vec<((), usize)> = vec![((), 16 << 20); 10];
        let runs = split_runs(large, DATA_RUN_ITEMS);
        assert_eq!(runs.len(), 10);
        assert_eq!(worker_count_for(&runs, 23), 2);
        assert_eq!(worker_count_for::<()>(&[], 23), 1);
    }
}
