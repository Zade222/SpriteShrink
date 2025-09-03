//! FFI-safe parsing functions for sprite-shrink archives.
//!
//! This module exposes Rust's parsing logic to C callers, handling the
//! conversion of data types and memory management across the FFI boundary.

use std::ffi::CString;
use std::slice;

use crate::ffi::FFIStatus;
use crate::ffi::ffi_structs::{
    FFIChunkIndexEntryU64, FFIChunkIndexEntryU128, FFIChunkLocation, 
    FFIFileManifestParentU64, FFIFileManifestParentU128, 
    FFIParsedChunkIndexArrayU64, FFIParsedChunkIndexArrayU128,
    FFIParsedManifestArrayU64, FFIParsedManifestArrayU128,
    FFISSAChunkMetaU64, FFISSAChunkMetaU128
};
use crate::lib_error_handling::SpriteShrinkError;
use crate::lib_structs::{FileHeader, FileManifestParent};
use crate::parsing::{
    parse_file_chunk_index, parse_file_header, parse_file_metadata,
    ParsingError
};

macro_rules! parse_file_chunk_index_ffi_setter {
    (
        $doc_l10:literal,
        $doc_l13:literal,
        $fn_name:ident,
        $ffi_parsed_chunk_index_array:path,
        $ffi_chunk_index_entry:path,
    ) => {
        /// Parses the file chunk index from a raw byte slice.
        ///
        /// On success, returns `FFIStatus::Ok` and populates `out_ptr`.
        /// On failure, returns an appropriate error code.
        ///
        /// # Safety
        /// - `chunk_index_array_ptr` must point to a valid memory block of
        ///   `chunk_index_len` bytes.
        /// - `out_ptr` must be a valid pointer to a `*mut
        #[doc = $doc_l10]
        /// - On success, the pointer written to `out_ptr` is owned by the
        ///   caller and MUST be freed by passing it to
        #[doc = $doc_l13]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            chunk_index_array_ptr: *const u8,
            chunk_index_len: usize,
            out_ptr: *mut *mut $ffi_parsed_chunk_index_array,
        ) -> FFIStatus {
            if chunk_index_array_ptr.is_null() || out_ptr.is_null() {
                return FFIStatus::NullArgument;
            }
            
            let chunk_index_data = unsafe {slice::from_raw_parts(
                chunk_index_array_ptr, 
                chunk_index_len)
            };

            match parse_file_chunk_index(chunk_index_data){
                Ok(index_map) => {
                    let mut entries: Vec<$ffi_chunk_index_entry> = index_map
                        .into_iter()
                        .map(|(hash, location)| {
                            $ffi_chunk_index_entry {
                                hash,
                                data: FFIChunkLocation {
                                    offset: location.offset,
                                    length: location.length,
                                },
                            }
                        })
                        .collect();

                    let entries_ptr = entries.as_mut_ptr();
                    let entries_len = entries.len();

                    /*Give up ownership of the Vec so it doesn't get 
                    deallocated.*/
                    std::mem::forget(entries);

                    //Prepare return struct
                    let result = {$ffi_parsed_chunk_index_array {
                        entries: entries_ptr,
                        entries_len,
                    }};
                    
                    unsafe {
                        *out_ptr = Box::into_raw(Box::new(result));
                    };

                    FFIStatus::Ok
                }
                Err(_) => FFIStatus::ManifestDecodeError,
            }
        }
    };
}

parse_file_chunk_index_ffi_setter!(
    "  FFIParsedChunkIndexArrayU64`.",
    "  `free_parsed_chunk_index_ffi_u64`.",
    parse_file_chunk_index_ffi_u64,
    FFIParsedChunkIndexArrayU64,
    FFIChunkIndexEntryU64,

);

parse_file_chunk_index_ffi_setter!(
    "  FFIParsedChunkIndexArrayU128`.",
    "  `free_parsed_chunk_index_ffi_u128`.",
    parse_file_chunk_index_ffi_u128,
    FFIParsedChunkIndexArrayU128,
    FFIChunkIndexEntryU128,

);

macro_rules! free_parsed_chunk_index_ffi_setter {
    (
        $doc_l1:literal,
        $doc_l5:literal,
        $fn_name:ident,
        $ffi_parsed_chunk_index_array:ty,
    ) => {
        #[doc = $doc_l1]
        ///
        /// # Safety
        /// The `ptr` must be a non-null pointer from a successful call to
        #[doc = $doc_l5]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            ptr: *mut $ffi_parsed_chunk_index_array) 
        {
            if ptr.is_null() {
                return;
            }

            unsafe {
                //Retake ownership of the main struct Box.
                let array_struct = Box::from_raw(ptr);

                /*Reconstruct the Vec from its raw parts. This allows Rust's
                memory manager to take ownership and correctly deallocate 
                the underlying array of entries when this new Vec goes out 
                of scope.*/
                let _ = Vec::from_raw_parts(
                    array_struct.entries,
                    array_struct.entries_len,
                    array_struct.entries_len,
                    );
            };
        }
    };
}

free_parsed_chunk_index_ffi_setter!(
    "Frees the memory allocated by `parse_file_chunk_index_ffi_u64`.",
    "`parse_file_chunk_index_ffi_u64`.",
    free_parsed_chunk_index_ffi_u64,
    FFIParsedChunkIndexArrayU64,
);

free_parsed_chunk_index_ffi_setter!(
    "Frees the memory allocated by `parse_file_chunk_index_ffi_u128`.",
    "`parse_file_chunk_index_ffi_u128`.",
    free_parsed_chunk_index_ffi_u128,
    FFIParsedChunkIndexArrayU128,
);

/// Parses a file header from a byte slice.
///
/// On success, returns `FFIStatus::Ok` and populates `out_ptr`.
/// On failure, returns an appropriate error code.
///
/// # Safety
/// - `header_data_array_ptr` must point to valid memory of at least 
///   `header_data_len` bytes.
/// - `out_ptr` must be a valid pointer to a `*mut FileHeader`.
/// - The pointer returned via `out_ptr` is owned by the caller and MUST be 
///   freed by passing it to `free_file_header_ffi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_header_ffi(
    header_data_array_ptr: *const u8,
    header_data_len: usize,
    out_ptr: *mut *mut FileHeader,
) -> FFIStatus {
    if header_data_array_ptr.is_null() || out_ptr.is_null() {
        return FFIStatus::NullArgument;
    }
    
    /*Prepare header_data, which is the only parameter of the original 
    parse_file_header function, from C input.*/
    let header_data = unsafe {slice::from_raw_parts(
            header_data_array_ptr, 
            header_data_len)
    };

    /*Parse the file header data and return it. On error return null pointer.*/
    match parse_file_header(header_data){
        Ok(data) => {
            unsafe{
                *out_ptr = Box::into_raw(Box::new(data));
            };
            FFIStatus::Ok
        }
        Err(e) => match e {
            SpriteShrinkError::Parsing(parsing_error) => match parsing_error {
                ParsingError::InvalidHeader(_) => FFIStatus::InvalidHeader,
                ParsingError::InvalidFileVersion() => FFIStatus::UnsupportedVersion,
                _ => FFIStatus::InternalError,
            },
            _ => FFIStatus::InternalError,
        },
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

macro_rules! parse_file_metadata_ffi_setter {
    (
        $doc_l1:literal,
        $doc_l10:literal,
        $doc_l12:literal,
        $fn_name:ident,
        $ffi_parsed_man_arr:path,
        $hash_type:ty,
        $ffi_file_man_par:path,
        $ffi_ssa_chunk_meta:path,
    ) => {
        #[doc = $doc_l1]
        ///
        /// On success, returns `FFIStatus::Ok` and populates `out_ptr`.
        /// On failure, returns an appropriate error code.
        ///
        /// # Safety
        /// - `manifest_data_array_ptr` must point to valid memory of at least
        ///   `manifest_data_len` bytes.
        /// - `out_ptr` must be a valid pointer to a `*mut 
        #[doc = $doc_l10]
        /// - The pointer returned via `out_ptr` is owned by the caller and
        #[doc = $doc_l12]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            manifest_data_array_ptr: *const u8,
            manifest_data_len: usize, 
            out_ptr: *mut *mut $ffi_parsed_man_arr
        ) -> FFIStatus {
            if manifest_data_array_ptr.is_null() || out_ptr.is_null() {
                return FFIStatus::NullArgument;
            }
            
            let manifest_data = unsafe {
                slice::from_raw_parts(
                    manifest_data_array_ptr, 
                    manifest_data_len
                )
            };

            let file_manifest: Vec<FileManifestParent<$hash_type>> = 
                match parse_file_metadata(manifest_data) {
                    Ok(data) => data,
                    //Return null on error
                    Err(_) => return FFIStatus::ManifestDecodeError,
                };

            /*Convert the Vec<FileManifestParent> to a 
            Vec<$ffi_file_man_par>*/
            let mut ffi_manifests: Vec<$ffi_file_man_par> = 
                match file_manifest
                .into_iter()
                .map(|fmp| {
                    //Convert the nested Vec<SSAChunkMeta>
                    let mut chunk_meta_vec: Vec<$ffi_ssa_chunk_meta> = 
                        fmp.chunk_metadata
                            .into_iter()
                            .map(|meta| 
                                {$ffi_ssa_chunk_meta {
                                    hash: meta.hash,
                                    offset: meta.offset,
                                    length: meta.length,
                                }
                            })
                            .collect();
                    
                    let chunk_meta_ptr = chunk_meta_vec.as_mut_ptr();
                    //Give up ownership so Rust doesn't deallocate it
                    std::mem::forget(chunk_meta_vec);

                    let c_filename = match CString::new(fmp.filename) {
                        Ok(s) => s.into_raw(),
                        Err(_) => return Err(FFIStatus::InvalidString),
                    };

                    Ok({$ffi_file_man_par {
                        filename: c_filename, 
                        chunk_metadata: chunk_meta_ptr,
                        chunk_metadata_len: fmp.chunk_count as usize,
                    }})
                })
                .collect() {
                    Ok(v) => v,
                    Err(status) => return status, //Propagate the error status
                };

            let manifests_ptr = ffi_manifests.as_mut_ptr();
            let manifests_len = ffi_manifests.len();

            //Give up ownership of the outer Vec
            std::mem::forget(ffi_manifests);

            //Create the final struct to be returned
            let result = {
                $ffi_parsed_man_arr {
                    manifests: manifests_ptr,
                    manifests_len,
                }
            };

            //Allocate the result struct on the heap and return a raw pointer
            unsafe {
                *out_ptr = Box::into_raw(Box::new(result));
            };
            FFIStatus::Ok
        }
    };
}

parse_file_metadata_ffi_setter!(
    "Parses the file manifest from a raw byte slice for a u64 hash.",
    "  FFIParsedManifestArrayU64`.",
    "  MUST be freed by passing it to `free_parsed_manifest_ffi_u64`.",
    parse_file_metadata_ffi_u64,
    FFIParsedManifestArrayU64,
    u64,
    FFIFileManifestParentU64,
    FFISSAChunkMetaU64,
);

parse_file_metadata_ffi_setter!(
    "Parses the file manifest from a raw byte slice for a u128 hash.",
    "  FFIParsedManifestArrayU128`.",
    "  MUST be freed by passing it to `free_parsed_manifest_ffi_u128`.",
    parse_file_metadata_ffi_u128,
    FFIParsedManifestArrayU128,
    u128,
    FFIFileManifestParentU128,
    FFISSAChunkMetaU128,
);

macro_rules! free_parsed_manifest_ffi_setter {
    (
        $doc_l1:literal,
        $doc_l5:literal,
        $fn_name:ident,
        $ffi_parsed_man_arr:ty,
        $ffi_ssa_chunk_meta:ty,
    ) => {
        #[doc = $doc_l1]
        ///
        /// # Safety
        /// The `ptr` must be a non-null pointer returned from a successful
        #[doc = $doc_l5]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            ptr: *mut $ffi_parsed_man_arr
        ) {
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
                        fmp.chunk_metadata as *mut $ffi_ssa_chunk_meta,
                        fmp.chunk_metadata_len,
                        fmp.chunk_metadata_len,
                    );
                }
            }
        }
    };
}

free_parsed_manifest_ffi_setter!(
    "Frees the memory allocated by `parse_file_metadata_ffi_u64`.",
    "call to `parse_file_metadata_ffi_u64`.",
    free_parsed_manifest_ffi_u64,
    FFIParsedManifestArrayU64,
    FFISSAChunkMetaU64,
);

free_parsed_manifest_ffi_setter!(
    "Frees the memory allocated by `parse_file_metadata_ffi_u128`.",
    "call to `parse_file_metadata_ffi_u128`.",
    free_parsed_manifest_ffi_u128,
    FFIParsedManifestArrayU128,
    FFISSAChunkMetaU128,
);

