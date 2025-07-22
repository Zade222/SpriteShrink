use std::collections::HashMap;
use std::slice;

use dashmap::DashMap;

use crate::ffi::ffi_structs::{
    FFIChunkIndexEntry, FFIDataStoreEntry, FFIVeriHashesEntry
};
use crate::ffi::{FFIFileManifestParent};
use crate::lib_error_handling::LibError;
use crate::lib_structs::{ChunkLocation, SSAChunkMeta};
use crate::processing::{rebuild_and_verify_single_file, test_compression};
use crate::FileManifestParent;

/// Rebuilds a single file from chunks and verifies its integrity via an 
/// FFI-safe interface.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers passed from C.
/// The caller MUST ensure that:
/// - All pointer arguments (`ffi_fmp`, `ser_data_store_array_ptr`, etc.) are 
/// non-null and valid.
/// - The `filename` and `chunk_metadata` pointers within `ffi_fmp` are valid.
/// - `ffi_fmp.chunk_metadata` points to an array of `FFISSAChunkMeta` with at 
/// least `ffi_fmp.chunk_count` elements.
/// - `ser_data_store_array_ptr` points to a valid memory block of 
/// `ser_data_store_len` bytes.
/// - `chunk_index_array_ptr` points to a valid array of `FFIChunkIndexEntry` 
/// with `chunk_index_len` elements.
/// - `veri_hashes_array_ptr` points to a valid array of `FFIVeriHashesEntry` 
/// with `veri_hashes_len` elements.
/// - All C-style strings are properly null-terminated.
///
/// # Returns
///
/// Returns an `i8` status code:
/// - `0`: Success. The file was rebuilt and the verification hash matched.
/// - `-1`: Hash Mismatch. The file was rebuilt, but its hash did not match 
/// the expected hash.
/// - `-2`: Verification Error. A chunk was missing from the index or another 
/// internal error occurred.
/// - `-3`: Internal Logic Error. Could not find the original verification 
/// hash for the file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebuild_and_verify_single_file_ffi(
    ffi_fmp: *const FFIFileManifestParent,
    ser_data_store_array_ptr: *const u8,
    ser_data_store_len: usize,
    chunk_index_array_ptr: *const FFIChunkIndexEntry,
    chunk_index_len: usize,
    veri_hashes_array_ptr: *const FFIVeriHashesEntry,
    veri_hashes_len: usize
) -> i8 {
    //Immediately check for null pointers to fail early.
    if ffi_fmp.is_null()
        || ser_data_store_array_ptr.is_null()
        || chunk_index_array_ptr.is_null()
        || veri_hashes_array_ptr.is_null()
    {
        return -2; //Verification Error
    }

    let(fmp, ser_data_store, chunk_index, veri_hashes) = unsafe {
        //Dereference the main struct pointer once.
        let ffi_fmp_ref = &*ffi_fmp;
        
        /*Prepare data_store, which is the second parameter of the original 
        rebuild_and_verify_single_file function, from C input.*/
        let ffi_meta = std::slice::from_raw_parts(
            ffi_fmp_ref.chunk_metadata,
            ffi_fmp_ref.chunk_count as usize
        );

        let vec_chunk_metadata: Vec<SSAChunkMeta> = ffi_meta
        .iter()
        .map(|meta| SSAChunkMeta {
            hash: meta.hash,
            offset: meta.offset,
            length: meta.length
        })
        .collect();

        let fmp = FileManifestParent {
            filename: std::ffi::CStr::from_ptr(ffi_fmp_ref.filename)
                .to_string_lossy()
                .into_owned(),
            chunk_count: ffi_fmp_ref.chunk_count,
            chunk_metadata: vec_chunk_metadata
        };
        
        /*Prepare data_store, which is the second parameter of the original 
        rebuild_and_verify_single_file function, from C input.*/
        let ser_data_store = slice::from_raw_parts(
            ser_data_store_array_ptr, 
            ser_data_store_len
        );

        /*Prepare and reconstruct chunk_index, which is the third parameter of
        the original rebuild_and_verify_single_file function, from C input.*/
        let chunk_index: HashMap<u64, ChunkLocation> = {
            let ffi_chunk_index_slice = 
                slice::from_raw_parts(
                    chunk_index_array_ptr, 
                    chunk_index_len
                );
            ffi_chunk_index_slice
                .iter()
                .map(|entry| {
                    (entry.hash, ChunkLocation {
                        offset: entry.data.offset,
                        length: entry.data.length,
                    })
                })
                .collect()
        };

        /*Prepare and reconstruct veri_hashes, which is the fourth parameter 
        of the original rebuild_and_verify_single_file function, from C input.*/
        let veri_hashes: DashMap<String, [u8; 64]> = {
            let ffi_veri_hashes_slice = 
                slice::from_raw_parts(
                    veri_hashes_array_ptr, 
                    veri_hashes_len
                );

            let map = DashMap::with_capacity(
                ffi_veri_hashes_slice.len());
            
            for entry in ffi_veri_hashes_slice{
                let filename = std::ffi::CStr::from_ptr(entry.key)
                .to_string_lossy()
                .into_owned();

                let hash_array = *entry.value;
                map.insert(filename, hash_array);
            }
            map
        };

        (fmp, ser_data_store, chunk_index, veri_hashes)
    };

    match rebuild_and_verify_single_file(
        &fmp, ser_data_store, &chunk_index, &veri_hashes
    ){
        Ok(()) => 0,
        Err(e) => match e {
            LibError::HashMismatchError(_) => -1,
            LibError::VerificationError(_) => -2,
            _ => -3, //Catch the rest of other errors.
        }
    }
}

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

        /*Prepare data_store, which is the second parameter of the original 
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