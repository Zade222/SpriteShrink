use std::ffi::CString;
use std::ptr;
use std::slice;

use crate::ffi::ffi_structs::{
    FFIChunkIndexEntry, FFIChunkLocation, FFIFileManifestParent, 
    FFIParsedChunkIndexArray, FFIParsedManifestArray, FFISSAChunkMeta
};
use crate::lib_structs::{FileHeader, FileManifestParent};
use crate::parsing::{
    parse_file_chunk_index, parse_file_header, parse_file_metadata
};

/// Parses the file chunk index from a raw byte slice.
///
/// On success, returns a pointer to an array of chunk index entries.
/// On failure, returns a null pointer.
///
/// # Safety
/// The `chunk_index_array_ptr` must be a valid pointer for the given length.
/// The returned pointer is owned by the caller and MUST be freed by passing
/// it to `free_parsed_chunk_index_ffi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_chunk_index_ffi(
    chunk_index_array_ptr: *const u8,
    chunk_index_len: usize
) -> *mut FFIParsedChunkIndexArray {
    if chunk_index_array_ptr.is_null() {
        return ptr::null_mut();
    }
    
    let chunk_index_data = unsafe {slice::from_raw_parts(
        chunk_index_array_ptr, 
        chunk_index_len)
    };

    match parse_file_chunk_index(chunk_index_data){
        Ok(index_map) => {
            

            let mut entries: Vec<FFIChunkIndexEntry> = index_map
                .into_iter()
                .map(|(hash, location)| FFIChunkIndexEntry {
                    hash,
                    data: FFIChunkLocation {
                        offset: location.offset,
                        length: location.length,
                    },
                })
                .collect();

            let entries_ptr = entries.as_mut_ptr();
            let entries_len = entries.len();

            //Give up ownership of the Vec so it doesn't get deallocated.
            std::mem::forget(entries);

            //Prepare return struct
            let result = FFIParsedChunkIndexArray {
                entries: entries_ptr,
                entries_len,
            };

            Box::into_raw(Box::new(result))
        }
        Err(_) => {
            ptr::null_mut()
        }
    }
}

/// Frees the memory allocated by `parse_file_chunk_index_ffi`.
///
/// # Safety
/// The `ptr` must be a non-null pointer from a successful call to
/// `parse_file_chunk_index_ffi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_chunk_index_ffi(
    ptr: *mut FFIParsedChunkIndexArray) 
{
    if ptr.is_null() {
        return;
    }

    unsafe {
        //Retake ownership of the main struct Box.
        let array_struct = Box::from_raw(ptr);

        /*Reconstruct the Vec from the raw parts, which allows
        memory manager to deallocate the array of entries.*/
        let _ = Vec::from_raw_parts(
            array_struct.entries,
            array_struct.entries_len,
            array_struct.entries_len,
            );
    };
}

/// Parses a file header from a byte slice.
///
/// On success, returns a pointer to a valid FileHeader.
/// On failure (e.g., invalid magic number, unsupported version), returns a 
/// null pointer.
///
/// # Safety
/// The `header_data_array_ptr` must be a valid pointer to readable memory of
/// at least `header_data_len` bytes. The pointer returned by this function is
/// owned by the caller and MUST be freed by passing it to 
/// `free_file_header_ffi` to prevent memory leaks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_header_ffi(
    header_data_array_ptr: *const u8,
    header_data_len: usize
) -> *mut FileHeader {
    /*Prepare header_data, which is the only parameter of the original 
    parse_file_header function, from C input.*/
    let header_data = unsafe {slice::from_raw_parts(
            header_data_array_ptr, 
            header_data_len)
    };

    /*Parse the file header data and return it. On error return null pointer.*/
    match parse_file_header(header_data){
        Ok(data) => {
            Box::into_raw(Box::new(data))
        }
        Err(_) => {
            ptr::null_mut()
        }
    }
}

/// Frees the memory for a FileHeader that was allocated by 
/// `parse_file_header_ffi`.
///
/// # Safety
/// The `ptr` must be a pointer returned from a successful call to
/// `parse_file_header_ffi`. Calling this function with a null pointer or a
/// pointer that has already been freed will lead to undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_header_ffi(ptr: *mut FileHeader) {
    unsafe{
        if !ptr.is_null() {
            /*Retake ownership of the pointer from C and drop it, freeing the
            memory.*/
            let _ = Box::from_raw(ptr);
        }
    }
}

/// Parses the file manifest from a raw byte slice.
///
/// On success, returns a pointer to an FFIParsedManifestArray.
/// On failure (e.g., decoding error), returns a null pointer.
///
/// # Safety
/// The `manifest_data_array_ptr` must be a valid pointer for the given 
/// length. The returned pointer is owned by the caller and MUST be 
/// freed by passing it to `free_parsed_manifest_ffi` to prevent memory 
/// leaks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_metadata_ffi(
    manifest_data_array_ptr: *const u8,
    manifest_data_len: usize
) -> *mut FFIParsedManifestArray {
    let manifest_data = unsafe {
        slice::from_raw_parts(
            manifest_data_array_ptr, 
            manifest_data_len
        )
    };

    let file_manifest: Vec<FileManifestParent> = 
        match parse_file_metadata(manifest_data) {
            Ok(data) => data,
            Err(_) => return ptr::null_mut(), //Return null on error
        };

    //Convert the Vec<FileManifestParent> to a Vec<FFIFileManifestParent>
    let mut ffi_manifests: Vec<FFIFileManifestParent> = file_manifest
        .into_iter()
        .map(|fmp| {
            //Convert the nested Vec<SSAChunkMeta>
            let mut chunk_meta_vec: Vec<FFISSAChunkMeta> = fmp.chunk_metadata
                .into_iter()
                .map(|meta| FFISSAChunkMeta {
                    hash: meta.hash,
                    offset: meta.offset,
                    length: meta.length,
                })
                .collect();
            
            let chunk_meta_ptr = chunk_meta_vec.as_mut_ptr();
            //Give up ownership so Rust doesn't deallocate it
            std::mem::forget(chunk_meta_vec);

            FFIFileManifestParent {
                //Convert filename String to a C string
                filename: CString::new(fmp.filename).unwrap_or_default().into_raw(),
                chunk_count: fmp.chunk_count,
                chunk_metadata: chunk_meta_ptr,
            }
        })
        .collect();

    let manifests_ptr = ffi_manifests.as_mut_ptr();
    let manifests_len = ffi_manifests.len();
    std::mem::forget(ffi_manifests);//Give up ownership of the outer Vec

    //Create the final struct to be returned
    let result = FFIParsedManifestArray {
        manifests: manifests_ptr,
        manifests_len,
    };

    //Allocate the result struct on the heap and return a raw pointer
    Box::into_raw(Box::new(result))
}

/// Frees the memory allocated by `parse_file_metadata_ffi`.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful call
/// to `parse_file_metadata_ffi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_manifest_ffi(ptr: *mut FFIParsedManifestArray) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        //Retake ownership of the main struct Box to deallocate it
        let manifest_array = Box::from_raw(ptr);

        //Retake ownership of the vector
        let ffi_manifests = Vec::from_raw_parts(
            manifest_array.manifests,
            manifest_array.manifests_len,
            manifest_array.manifests_len,
        );

        /*Iterate through and deallocate the contents of each 
        FFIFileManifestParent*/
        for fmp in ffi_manifests {
            //Deallocate the CString for the filename
            let _ = CString::from_raw(fmp.filename);
            //Deallocate the Vec for the chunk metadata
            let _ = Vec::from_raw_parts(
                fmp.chunk_metadata as *mut FFISSAChunkMeta,
                fmp.chunk_count as usize,
                fmp.chunk_count as usize,
            );
        }
    }
}

