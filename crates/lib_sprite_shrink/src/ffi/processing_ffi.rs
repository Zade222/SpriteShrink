//! FFI-safe processing functions for sprite-shrink archives.
//!
//! This module exposes the core data processing logic (chunking, hashing,
//! verification) to C callers with robust error handling and memory 
//! management.

use std::collections::HashMap;
use std::ffi::CString;
use std::slice;

use dashmap::DashMap;
use fastcdc::v2020::Chunk;
use libc::c_char;

use crate::ffi::FFIStatus;
use crate::ffi::ffi_structs::{
    FFIChunk, FFIChunkIndexEntry, FFIFileManifestChunks, FFIDataStoreEntry, 
    FFIFileData, FFIFileManifestParent, FFIHashedChunkData, FFIProcessedFileData, 
    FFIVeriHashesEntry
};
use crate::ffi::FFISSAChunkMeta;
use crate::lib_error_handling::LibError;
use crate::lib_structs::{ChunkLocation, SSAChunkMeta};
use crate::processing::{
    create_file_manifest_and_chunks, process_file_in_memory, 
    rebuild_and_verify_single_file, test_compression};
use crate::{FileData, FileManifestParent};

/// Creates a file manifest and a list of hashed data chunks via an FFI-safe 
/// interface.
///
/// # Safety
/// The caller MUST ensure that all pointer arguments are valid for their 
/// specified lengths
/// and that `file_name_ptr` is a valid, null-terminated C string.
/// - `out_ptr` must be a valid, non-null pointer to a 
/// `*mut FFIFileManifestChunks`.
/// - On success, the pointer written to `out_ptr` is owned by the caller and
///   MUST be freed by passing it to `free_file_manifest_and_chunks_ffi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn create_file_manifest_and_chunks_ffi(
    file_name_ptr: *const c_char,
    file_data_array_ptr: *const u8,
    file_data_len: usize,
    chunks_array_ptr: *const FFIChunk,
    chunks_len: usize,
    out_ptr: *mut *mut FFIFileManifestChunks
) -> FFIStatus {
    if file_name_ptr.is_null() 
        || chunks_array_ptr.is_null() 
        || out_ptr.is_null() {
        return FFIStatus::NullArgument;
    }

    let (file_name, file_data, chunks) = unsafe {
        /*Prepare file_name, which is the first parameter of the original 
        create_file_manifest_and_chunks function, from C input.*/
        let file_name = std::ffi::CStr::from_ptr(file_name_ptr)
            .to_string_lossy()
            .into_owned();

        /*Prepare file_data, which is the second parameter of the original 
        create_file_manifest_and_chunks function, from C input.*/
        let file_data = slice::from_raw_parts(
            file_data_array_ptr, 
            file_data_len
        );

        /*Prepare chunks, which is the third parameter of the original 
        create_file_manifest_and_chunks function, from C input.*/
        let chunks = {
            let chunk_slice = 
                slice::from_raw_parts(
                    chunks_array_ptr, 
                    chunks_len
                );
            
            chunk_slice.iter()
                .map(|chunk| {
                    Chunk {
                        hash: chunk.hash,
                        offset: chunk.offset,
                        length: chunk.length,
                    }
                })
                .collect::<Vec<_>>()
        };

        (file_name, file_data, chunks)
    };

    /*Call original function to process data.*/
    let (fmp, hashed_chunks) = 
    create_file_manifest_and_chunks(&file_name, file_data, &chunks);

    let mut ffi_chunk_metadata: Vec<FFISSAChunkMeta> = fmp.chunk_metadata.into_iter()
        .map(|meta| FFISSAChunkMeta {
            hash: meta.hash,
            offset: meta.offset,
            length: meta.length
        }).collect();
    let fmp_meta_ptr = ffi_chunk_metadata.as_mut_ptr();
    std::mem::forget(ffi_chunk_metadata);

    let c_filename = match CString::new(fmp.filename) {
        Ok(s) => s.into_raw(),
        Err(_) => return FFIStatus::InvalidString,
    };

    let ffi_fmp = FFIFileManifestParent {
        filename: c_filename,
        chunk_metadata: fmp_meta_ptr,
        chunk_metadata_len: fmp.chunk_count as usize,
    };

    //Convert vector of hashed chunks
    let mut ffi_hashed_chunks: Vec<FFIHashedChunkData> = hashed_chunks.into_iter()
        .map(|(hash, mut data_vec)| {
            let data_ptr = data_vec.as_mut_ptr();
            let data_len = data_vec.len();
            std::mem::forget(data_vec); //Give up ownership of the inner Vec<u8>
            FFIHashedChunkData {
                hash,
                chunk_data: data_ptr,
                chunk_data_len: data_len
            }
        }).collect();
    
    let hashed_chunks_ptr = ffi_hashed_chunks.as_mut_ptr();
    let hashed_chunks_len = ffi_hashed_chunks.len();
    std::mem::forget(ffi_hashed_chunks);

    /*Prepare return data in struct.*/
    let output = Box::new(FFIFileManifestChunks{
        fmp: ffi_fmp,
        hashed_chunks: hashed_chunks_ptr,
        hashed_chunks_len,
    });

    unsafe {
        *out_ptr = Box::into_raw(output);
    };
    FFIStatus::Ok
}

/// Frees the memory allocated by `create_file_manifest_and_chunks_ffi`.
///
/// # Safety
/// The caller MUST ensure that `ptr` is a valid pointer returned from
/// `create_file_manifest_and_chunks_ffi` and that it is not used after this 
/// call. This function should only be called once for any given pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_manifest_and_chunks_ffi(ptr: *mut FFIFileManifestChunks) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        //Retake ownership of the main struct to deallocate it at the end.
        let output_box = Box::from_raw(ptr);

        //Deallocate contents of the FFIFileManifestParent
        let _ = CString::from_raw(output_box.fmp.filename);

        //Reclaim and drop the Vec for the chunk metadata.
        let _ = Vec::from_raw_parts(
            output_box.fmp.chunk_metadata as *mut FFISSAChunkMeta,
            output_box.fmp.chunk_metadata_len as usize,
            output_box.fmp.chunk_metadata_len as usize,
        );

        //Deallocate the array of FFIHashedChunk and their inner data
        let hashed_chunks_vec = Vec::from_raw_parts(
            output_box.hashed_chunks,
            output_box.hashed_chunks_len,
            output_box.hashed_chunks_len,
        );

        /*Iterate through the chunks to deallocate the inner Vec<u8> data for
        each.*/
        for chunk in hashed_chunks_vec {
            let _ = Vec::from_raw_parts(
                chunk.chunk_data, 
                chunk.chunk_data_len, 
                chunk.chunk_data_len);
        }
    }
}

/// Processes a single file from memory via an FFI-safe interface.
///
/// On success, returns `FFIStatus::Ok` and populates `out_ptr`.
///
/// # Safety
/// - `file_data` must be valid for reads. Its pointers must not be null.
/// - `out_ptr` must be a valid, non-null pointer to a 
/// `*mut FFIProcessedFileData`.
/// - On success, the pointer written to `out_ptr` is owned by the caller and
///   MUST be freed by passing it to `free_processed_file_data_ffi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_file_in_memory_ffi(
    file_data: FFIFileData,
    window_size: u64,
    out_ptr: *mut *mut FFIProcessedFileData,
) -> FFIStatus {
    if file_data.filename.is_null() 
        || file_data.file_data.is_null() 
        || out_ptr.is_null() {
        return FFIStatus::NullArgument;
    }
    
    let native_file_data = unsafe{
        FileData {
            file_name: std::ffi::CStr::from_ptr(file_data.filename)
                .to_string_lossy()
                .into_owned(),
            file_data: slice::from_raw_parts(
                file_data.file_data, 
                file_data.file_data_len
            ).to_vec()
        }
    };

    
    let processed_data = match process_file_in_memory(
        native_file_data, 
        window_size){
        Ok(Some(data)) => data,
        Ok(None) => return FFIStatus::Ok, //No error, but no data to return
        Err(_) => return FFIStatus::InternalError,
    };

    /*Convert filename String to *mut c_char to be compatible with return 
    struct.*/
    let filename_cstring = CString::new(processed_data.file_name).unwrap();
    let filename_len = filename_cstring.as_bytes().len();
    let filename_ptr = filename_cstring.into_raw();
        
    /*Convert veri_hash [u8; 64] to *mut [u8; 64] to be compatible with return 
    struct.*/
    let veri_hash_ptr = Box::into_raw(Box::new(processed_data.veri_hash));

    /*Convert file_data Vec<u8> to *mut u8 to be compatible with return 
    struct.*/
    let mut file_data_vec = processed_data.file_data;
    let file_data_len = file_data_vec.len();
    let file_data_ptr = file_data_vec.as_mut_ptr();
    std::mem::forget(file_data_vec); //Give up ownership, no longer needed

    /*Convert chunks Vec<Chunk> to *mut FFIChunk to be compatible with return
    struct.*/
    let mut chunks_vec: Vec<FFIChunk> = processed_data.chunks
        .into_iter()
        .map(|c: Chunk| FFIChunk {
            hash: c.hash,
            offset: c.offset,
            length: c.length,
        })
        .collect();
    let chunks_len = chunks_vec.len();
    let chunks_ptr = chunks_vec.as_mut_ptr();
    std::mem::forget(chunks_vec); //Give up ownership, no longer needed

    //Prepare and return FFIProcessedFileData
    let output = Box::new(FFIProcessedFileData {
        filename: filename_ptr,
        filename_len,
        veri_hash: veri_hash_ptr,
        chunks: chunks_ptr,
        chunks_len,
        file_data: file_data_ptr,
        file_data_len,
    });

    unsafe {
        *out_ptr = Box::into_raw(output);
    };

    FFIStatus::Ok
}

/// Frees the memory allocated by `process_file_in_memory_ffi`.
///
/// # Safety
/// The caller MUST ensure that `ptr` is a valid pointer returned from
/// `process_file_in_memory_ffi` and that it is not used after this call.
/// This function should only be called once for any given pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_processed_file_data_ffi(
    ptr: *mut FFIProcessedFileData) {
    if ptr.is_null() {
        return;
    }

    /*Retake ownership of the main struct Box to deallocate it and retake 
    ownership of each pointer within the struct to deallocate them*/
    unsafe{
        let output_box = Box::from_raw(ptr);

        let _ = CString::from_raw(output_box.filename);
        let _ = Box::from_raw(output_box.veri_hash);
        let _ = Vec::from_raw_parts(output_box.chunks, output_box.chunks_len, output_box.chunks_len);
        let _ = Vec::from_raw_parts(output_box.file_data, output_box.file_data_len, output_box.file_data_len);
    }
}

/// Rebuilds a single file from chunks and verifies its integrity via an 
/// FFI-safe interface.
///
/// # Safety
/// All pointer arguments must be non-null and valid for their specified lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebuild_and_verify_single_file_ffi(
    file_manifest_parent: *const FFIFileManifestParent,
    ser_data_store_array_ptr: *const u8,
    ser_data_store_len: usize,
    chunk_index_array_ptr: *const FFIChunkIndexEntry,
    chunk_index_len: usize,
    veri_hashes_array_ptr: *const FFIVeriHashesEntry,
    veri_hashes_len: usize
) -> FFIStatus {
    //Immediately check for null pointers to fail early.
    if file_manifest_parent.is_null()
        || ser_data_store_array_ptr.is_null()
        || chunk_index_array_ptr.is_null()
        || veri_hashes_array_ptr.is_null()
    {
        return FFIStatus::NullArgument;
    }

    let(fmp, ser_data_store, chunk_index, veri_hashes) = unsafe {
        //Dereference the main struct pointer once.
        let ffi_fmp_ref = &*file_manifest_parent;
        
        /*Prepare data_store, which is the second parameter of the original 
        rebuild_and_verify_single_file function, from C input.*/
        let ffi_meta = std::slice::from_raw_parts(
            ffi_fmp_ref.chunk_metadata,
            ffi_fmp_ref.chunk_metadata_len
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
            chunk_count: ffi_fmp_ref.chunk_metadata_len as u64,
            chunk_metadata: vec_chunk_metadata
        };
        
        /*Prepare ser_data_store, which is the second parameter of the original 
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
        Ok(()) => FFIStatus::Ok,
        Err(LibError::HashMismatchError(_)) => FFIStatus::VerificationHashMismatch,
        Err(LibError::VerificationError(_)) => FFIStatus::VerificationMissingChunk,
        _ => FFIStatus::InternalError,
    }
}

/// Estimates the compressed size of a data store.
///
/// On success, returns `FFIStatus::Ok` and populates `out_size`.
///
/// # Safety
/// - All input pointers must be non-null and valid for their specified lengths.
/// - `out_size` must be a valid, non-null pointer to a `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_compression_ffi(
    data_store_array_ptr: *const FFIDataStoreEntry,
    data_store_len: usize, 
    sorted_hashes_array_ptr: *const u64,
    sorted_hashes_len: usize,
    worker_count: usize,
    dictionary_size: usize,
    out_size: *mut u64,
) -> FFIStatus {
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
        dictionary_size
    ) {
        Ok(compressed_size) => {
            unsafe {
                *out_size = compressed_size as u64;
            }
            
            FFIStatus::Ok
        }
        Err(LibError::DictionaryError(_)) => FFIStatus::DictionaryError,
        _ => FFIStatus::InternalError,
    }
}