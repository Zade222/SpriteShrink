use std::{
    fmt::{Debug, Display},
    sync::{Arc, atomic::{
        AtomicBool, Ordering
    }},
    time::{Duration, Instant}
};

use indicatif::{ProgressBar, ProgressStyle};
use sprite_shrink::{
    Hashable, SerializedData, serialize_uncompressed_data, test_compression
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
    H: Hashable + Debug
{
    let timeout_dur = args.autotune_timeout.
        map(Duration::from_secs);

    let mut current_dict_size: usize = 8192;
    let mut best_dict_size = 8192;
    let mut last_compressed_size = usize::MAX;

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

    loop{
        debug!("Testing dictionary size: {current_dict_size}");

        let start_time = Instant::now();

        let compressed_size = test_compression(
            &ser_data.sorted_hashes,
            chunk_sum,
            args.threads.unwrap(),
            current_dict_size,
            args.compression_level as i32,
            chunk_ret_cb.clone()
        )?;

        let elapsed = start_time.elapsed();

        debug!("Compressed size = {compressed_size}");
        debug!("Last compressed size = {last_compressed_size}");

        if compressed_size > last_compressed_size {
            //Process passed the optimal point, stop.
            debug!("Optimal dictionary size found to be \
                {best_dict_size} bytes."
            );
            break;
        }

        if let Some(timeout) = timeout_dur
            && elapsed > timeout
        {
            warn!("Autotune for dicitonary size {current_dict_size}\
                took too long (>{timeout:?}).Using best \
                result so far: {best_dict_size}."
            );
        }

        //This iteration was successful and an improvement
        last_compressed_size = compressed_size;
        best_dict_size = current_dict_size as u64;

        //Double value for next loop
        current_dict_size *= 2;

        /*Accept reasonable upper limit for dictionary size.
        This will stop it at an accepted value of 1024 * 1024*/
        if current_dict_size > 1024 * 1024 {
            break;
        }
    }

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
        + Ord,
    F: FnMut(u64) -> Result<(FileData<H>, Arc<TempCache<H>>), CliError>,
{
    let timeout_dur = args.autotune_timeout.
        map(Duration::from_secs);

    //Starting values
    let mut current_window_size = 512;
    let mut best_window_size = 512;
    let mut last_compressed_size = usize::MAX;

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

    loop {
        debug!("Testing window size: {current_window_size}");

        /*Set starting time for determining the compressed size
        using the current window size*/
        let start_time = Instant::now();

        let (file_data, temp_cache) = processor_fn(current_window_size)?;

        /*Callback for getting all keys(hashes) from temporary
        cache*/
        let key_ret_cb = {
            let cache_clone = Arc::clone(&temp_cache);
            let running_clone = Arc::clone(running);
            move || -> Result<Vec<H>, CliError> {
                if !running_clone.load(Ordering::SeqCst) {
                    return Err(CliError::Cancelled);
                }

                cache_clone.get_keys()
            }
        };

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

        let serialized_data = serialize_uncompressed_data(
            &file_data.file_manifest,
            &key_ret_cb,
            chunk_ret_cb.as_ref()
        )?;

        let total_data_size = temp_cache.get_tot_data_size();

        let get_chunk_data_for_test = {
            let cb = Arc::clone(&chunk_ret_cb);
            move |hashes: &[H]| cb(hashes)
        };

        /*Compress the data and measure the size
        (dictionary size + compressed data size)*/
        let compressed_size = test_compression(
            &serialized_data.sorted_hashes,
            total_data_size,
            args.threads.unwrap(),
            8192,
            args.compression_level as i32,
            get_chunk_data_for_test,
        )?;

        /*Measure the time taken for determining the compressed size
        for the current window size*/
        let elapsed = start_time.elapsed();

        debug!("Compressed size = {compressed_size}");
        debug!("Last compressed size = {last_compressed_size}");

        if compressed_size > last_compressed_size {
            debug!("Optimal window size found to be \
                {best_window_size} bytes.");
            break;
        }

        if let Some(timeout) = timeout_dur
            && elapsed > timeout
        {
            warn!("Autotune for window size {current_window_size}\
                took too long (>{timeout:?}).Using best \
                result so far: {best_window_size}."
            );
        }

        last_compressed_size = compressed_size;
        best_window_size = current_window_size;
        current_window_size *= 2;
    }

    if let Some(bar) = &window_bar {
        bar.finish_with_message("Optimal window size found.");
    }

    Ok(best_window_size)
}
