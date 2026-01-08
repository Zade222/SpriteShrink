use std::{
    cmp::min,
};

use sysinfo::System;
use thread_priority::{ThreadPriority, set_current_thread_priority};
use tracing::{
    debug,
};

use crate::{
    error_handling::CliError
};

/// Determines whether the compression process should use an in-memory
/// cache or a temporary file-based cache on disk.
///
/// This function assesses the trade-off between performance and memory usage.
/// Using an in-memory cache is significantly faster but requires enough
/// available RAM to hold all unique chunk data. Using a file-based cache
/// is slower due to disk I/O but supports processing datasets much larger
/// than the available memory.
///
/// The decision is made based on a heuristic: it checks if 80% of the
/// system's currently available memory is greater than the total size of the
/// input data plus a conservative buffer. This buffer accounts for the
/// overhead of other data structures used during the compression pipeline,
/// such as the file manifest and various temporary collections.
///
/// # Arguments
///
/// * `input_data_size`: The total combined size in bytes of all files to be
///   processed.
///
/// # Returns
///
/// * `true` if there is likely sufficient memory to safely use an in-memory
///   cache.
/// * `false` if it is safer to use a temporary cache on disk to avoid
///   potential out-of-memory errors.
pub fn process_in_memory_check (
    input_data_size: u64,
) -> bool {
    let mut system_info = System::new_all();
    system_info.refresh_all();

    let free_mem = system_info.available_memory();

    if (0.8 * free_mem as f64) > (input_data_size +
        min(input_data_size, u32::MAX as u64) +
        ((u32::MAX / 4) as u64)) as f64
    {
        debug!("Processing in memory.");
        true
    } else {
        debug!("Processing via disk cache.");
        false
    }
}


pub fn set_priority() -> Result<(), CliError> {
    #[cfg(target_os = "linux")]
    {
        let priority = ThreadPriority::Crossplatform(
            30.try_into().unwrap()
        );

        set_current_thread_priority(priority)?;
    }

    #[cfg(target_os = "macos")]
    {
        let priority = ThreadPriority::Crossplatform(
            30.try_into().unwrap()
        );

        set_current_thread_priority(priority)?;
    }

    #[cfg(target_os = "windows")]
    {
        let priority = ThreadPriority::Os(
            WinAPIThreadPriority::BelowNormal.into()
        );

        set_current_thread_priority(priority)?;
    }

    Ok(())
}
