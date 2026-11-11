use std::{
    fmt::{Debug, Display},
    sync::{Arc, atomic::{
        AtomicBool, Ordering
    }},
    time::{Duration, Instant}
};

use bitcode::Encode;
use indicatif::{ProgressBar, ProgressStyle};
use sprite_shrink::{
    MAX_WINDOW_SIZE,
    Hashable, SerializedData,
    serialize_uncompressed_data, test_compression
};
use tracing::{
    debug,
    warn
};

use crate::{
    arg_handling::Args,
    cli_types::FileData,
    error_handling::CliError,
    storage_io::TempCache
};

/// The largest dictionary size, in bytes, that auto-tune tries.
const MAX_TUNED_DICTIONARY_SIZE: u64 = 1024 * 1024;

/// Finds the size with the smallest `measure` result by doubling from
/// `start`, and returns it. `what` names the setting in log messages.
///
/// Stops at the first size whose result is larger than the one before, at
/// the last size not above `max`, or after a measurement that took longer
/// than `timeout`. A slow measurement still counts if it improved on the
/// best so far: it has already finished, so its result is real.
///
/// # Errors
///
/// Returns the first error from `measure`.
fn tune<E>(
    start: u64,
    max: u64,
    timeout: Option<Duration>,
    what: &str,
    mut measure: impl FnMut(u64) -> Result<usize, E>,
) -> Result<u64, E> {
    let mut size = start;
    let mut best_size = start;
    let mut best_result = usize::MAX;

    while size <= max {
        debug!("Testing {what} size: {size}");
        let start_time = Instant::now();
        let result = measure(size)?;
        let elapsed = start_time.elapsed();
        debug!("Compressed size = {result}, best so far = {best_result}");

        if result > best_result {
            //Past the optimal point.
            break;
        }
        best_size = size;
        best_result = result;

        if let Some(timeout) = timeout
            && elapsed > timeout
        {
            warn!(
                "Auto-tune for {what} size {size} took longer than \
                 {timeout:?}. Stopping with the best result so far: \
                 {best_size} bytes."
            );
            break;
        }

        let Some(next) = size.checked_mul(2) else { break };
        size = next;
    }

    debug!("Optimal {what} size found to be {best_size} bytes.");
    Ok(best_size)
}


pub fn auto_tune_dict<F, H>(
    args: &Args,
    chunk_sum: u64,
    ser_data: &SerializedData<H>,
    chunk_ret_cb: F,
) -> Result<u64, CliError>
where
    F: Fn(&[H]) -> Result<Vec<Vec<u8>>, CliError>
        + Clone
        + Send
        + Sync
        + 'static,
    H: Hashable + Debug + Encode
{
    let timeout_dur = args.autotune_timeout.
        map(Duration::from_secs);

    let dictionary_bar = if args.progress {
        let bar = ProgressBar::new_spinner();
        bar.enable_steady_tick(Duration::from_millis(500));
        bar.set_style(
            ProgressStyle::with_template("{msg} {spinner}")
                .unwrap()
                .tick_strings(&[
                    "   ",
                    ".  ",
                    ".. ",
                    "...",
                    " ..",
                    "  .",
                    "   ",
                ]),
        );
        bar.set_message("Tuning dictionary size");
        Some(bar)
    } else {
        None
    };

    let best_dict_size = tune::<CliError>(
        8192,
        MAX_TUNED_DICTIONARY_SIZE,
        timeout_dur,
        "dictionary",
        |dict_size| {
            let dict_size = usize::try_from(dict_size).map_err(|err| {
                CliError::InternalError(format!(
                    "Dictionary size {dict_size} doesn't fit in memory: {err}"
                ))
            })?;
            Ok(test_compression(
                &ser_data.sorted_hashes,
                chunk_sum,
                args.threads.unwrap_or(0),
                dict_size,
                i32::from(args.compression_level),
                chunk_ret_cb.clone()
            )?)
        },
    )?;

    if let Some(bar) = &dictionary_bar {
        bar.finish_with_message("Optimal dictionary size found.");
    }

    Ok(best_dict_size)
}


pub fn auto_tune_win<H, F>(
    args: &Args,
    running: &Arc<AtomicBool>,
    mut processor_fn: F,
) -> Result<u64, CliError>
where
    H: Hashable
        + Debug
        + Display
        + Ord
        + Encode,
    F: FnMut(u64) -> Result<(FileData<H>, Arc<TempCache<H>>), CliError>,
{
    let timeout_dur = args.autotune_timeout.
        map(Duration::from_secs);

    let window_bar = if args.progress {
        let bar = ProgressBar::new_spinner();
        bar.enable_steady_tick(Duration::from_millis(500));
        bar.set_style(
            ProgressStyle::with_template("{msg} {spinner}")
                .unwrap()
                .tick_strings(&[
                    "   ",
                    ".  ",
                    ".. ",
                    "...",
                    " ..",
                    "  .",
                    "   ",
                ]),
        );
        bar.set_message("Tuning window size");
        Some(bar)
    } else {
        None
    };

    /*FastCDC panics on a window above `MAX_WINDOW_SIZE`, so tuning stops
    there.*/
    let best_window_size = tune::<CliError>(
        512,
        MAX_WINDOW_SIZE,
        timeout_dur,
        "window",
        |window_size| {
            let (file_data, temp_cache) = processor_fn(window_size)?;

            /*Callback for getting one or more chunks from the temporary
            cache*/
            let chunk_ret_cb = Arc::new({
                let cache_clone = Arc::clone(&temp_cache);
                let running_clone = Arc::clone(running);
                move |hashes: &[H]| -> Result<Vec<Vec<u8>>, CliError> {
                    if !running_clone.load(Ordering::SeqCst) {
                        return Err(CliError::Cancelled);
                    }

                    cache_clone.get_chunks(hashes)
                }
            });

            let serialized_data =
                serialize_uncompressed_data(&file_data.file_manifest);

            let total_data_size = temp_cache.get_tot_data_size();

            let get_chunk_data_for_test = {
                let cb = Arc::clone(&chunk_ret_cb);
                move |hashes: &[H]| cb(hashes)
            };

            /*Compress the data and measure the size
            (dictionary size + compressed data size)*/
            Ok(test_compression(
                &serialized_data.sorted_hashes,
                total_data_size,
                args.threads.unwrap_or(0),
                8192,
                i32::from(args.compression_level),
                get_chunk_data_for_test,
            )?)
        },
    )?;

    if let Some(bar) = &window_bar {
        bar.finish_with_message("Optimal window size found.");
    }

    Ok(best_window_size)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, thread};

    use super::*;

    /// Runs `tune` from 512 with `results` as the measurements, in order,
    /// and returns the best size and the sizes that were measured.
    fn run_tune(
        max: u64,
        timeout: Option<Duration>,
        delay: Duration,
        results: &[usize],
    ) -> (u64, Vec<u64>) {
        let measured = RefCell::new(Vec::new());
        let best = tune(512, max, timeout, "test", |size| {
            thread::sleep(delay);
            let mut measured = measured.borrow_mut();
            measured.push(size);
            Ok::<_, CliError>(results[measured.len() - 1])
        })
        .unwrap();
        (best, measured.into_inner())
    }

    #[test]
    fn tuning_stops_at_the_first_worse_result() {
        let (best, measured) =
            run_tune(1 << 20, None, Duration::ZERO, &[100, 90, 95, 10]);
        assert_eq!(best, 1024);
        assert_eq!(measured, [512, 1024, 2048]);
    }

    #[test]
    fn tuning_never_passes_the_maximum() {
        let (best, measured) =
            run_tune(4096, None, Duration::ZERO, &[40, 30, 20, 10, 0]);
        assert_eq!(best, 4096);
        assert_eq!(measured, [512, 1024, 2048, 4096]);

        // A maximum between two steps stops at the step below it.
        let (best, measured) = run_tune(1000, None, Duration::ZERO, &[40, 30]);
        assert_eq!(best, 512);
        assert_eq!(measured, [512]);
    }

    #[test]
    fn tuning_stops_after_a_slow_measurement_and_keeps_its_result() {
        let (best, measured) = run_tune(
            1 << 20,
            Some(Duration::from_millis(1)),
            Duration::from_millis(5),
            &[40, 30],
        );
        assert_eq!(best, 512);
        assert_eq!(measured, [512]);
    }

    #[test]
    fn a_measurement_error_stops_tuning() {
        let result = tune(512, 1 << 20, None, "test", |_| {
            Err::<usize, _>(CliError::Cancelled)
        });
        assert!(matches!(result, Err(CliError::Cancelled)));
    }
}
