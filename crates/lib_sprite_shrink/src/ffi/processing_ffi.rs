use std::collections::HashMap;
use std::slice;

use crate::ffi::ffi_structs::{FFIDataStoreEntry};
use crate::processing::{test_compression};

/// # Safety
///
/// The caller MUST ensure that `data_store_array_ptr` and 
/// `sorted_hashes_array_ptr` are valid for the given lengths.
///
/// # Returns
///
/// Returns the estimated compressed size in bytes. On an internal error (e.g.,
/// dictionary creation failure), this function returns `u64::MAX`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_compression_ffi(
    data_store_array_ptr: *const FFIDataStoreEntry,
    data_store_len: usize, 
    sorted_hashes_array_ptr: *const u64,
    sorted_hashes_len: usize,
    worker_count: usize,
    dictionary_size: usize
) -> u64 {
    let (data_store, sorted_hashes) = unsafe {
        //Convert C inputs to Rust types

        /*Prepare data_store, which is the first parameter of the original 
        test_compression function, from C input.*/
        let ffi_data_store = slice::from_raw_parts(
            data_store_array_ptr, 
            data_store_len
        );

        //Reconstruct the data_store HashMap via the following.
        let data_store: HashMap<u64, Vec<u8>> = ffi_data_store.iter().map(|entry| {
            let data_slice = std::slice::from_raw_parts(entry.data, entry.data_len);
            (entry.hash, data_slice.to_vec())
        }).collect();

        /*Prepare data_store, which is the first second of the original 
        test_compression function, from C input.*/
        let sorted_hashes_slice = slice::from_raw_parts(
            sorted_hashes_array_ptr, 
            sorted_hashes_len
        );

        (data_store, sorted_hashes_slice)
    };

    //Run test compression with received and converted data.
    match test_compression(
        &data_store, 
        &sorted_hashes, 
        worker_count, 
        dictionary_size) {
            Ok(compressed_size) => compressed_size as u64,
            Err(_) => {
                u64::MAX
            }
        }
}