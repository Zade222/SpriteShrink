//! FFI-safe processing functions for sprite-shrink archives.
//!
//! This module exposes the core data processing logic (chunking, hashing,
//! verification) to C callers with robust error handling and memory 
//! management.

use std::{
    ffi::CString,
    os::raw::c_void,
    slice
};

use fastcdc::v2020::Chunk;
use libc::c_char;

use crate::ffi::FFIStatus;
use crate::ffi::ffi_structs::{
    FFIChunk, FFIChunkDataArray,
    FFIFileManifestChunksU64, FFIFileManifestChunksU128, 
    FFIFileData, 
    FFIFileManifestParentU64, FFIFileManifestParentU128, 
    FFIHashedChunkDataU64, FFIHashedChunkDataU128,
    FFIProcessedFileData, 
    FFISeekChunkInfoU64, FFISeekChunkInfoU128,
    FFISeekInfoArrayU64, FFISeekInfoArrayU128,
    FFISSAChunkMetaU64, FFISSAChunkMetaU128, 
};
use crate::lib_error_handling::SpriteShrinkError;
use crate::lib_structs::{SSAChunkMeta};
use crate::processing::{
    create_file_manifest_and_chunks, get_seek_chunks, process_file_in_memory, 
    verify_single_file, test_compression, ProcessingError};
use crate::{FileData, FileManifestParent};

macro_rules! create_file_manifest_and_chunks_ffi_setter {
    (
        $doc_l6:literal,
        $fn_name:ident,
        $ffi_file_man_chunks:path,
        $ffi_ssa_chunk_meta:path,
        $ffi_file_man_par:path,
        $ffi_hashed_chunk_data:path,
    ) => {
        /// Creates a file manifest and a list of hashed data chunks via an 
        /// FFI-safe interface.
        ///
        /// # Safety
        /// The caller MUST ensure `ptr` is a valid pointer previously returned
        #[doc = $doc_l6] 
        ///
        /// Passing a null pointer, a pointer that has already been freed, or a
        /// pointer from any other source will result in 
        /// **undefined behavior**.
        /// This function must only be called once per pointer.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            file_name_ptr: *const c_char,
            file_data_array_ptr: *const u8,
            file_data_len: usize,
            chunks_array_ptr: *const FFIChunk,
            chunks_len: usize,
            out_ptr: *mut *mut $ffi_file_man_chunks
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

            let mut ffi_chunk_metadata: Vec<$ffi_ssa_chunk_meta> = fmp.chunk_metadata.into_iter()
                .map(|meta| {
                    $ffi_ssa_chunk_meta {
                        hash: meta.hash,
                        offset: meta.offset,
                        length: meta.length
                    }
                }).collect();
            let fmp_meta_ptr = ffi_chunk_metadata.as_mut_ptr();
            std::mem::forget(ffi_chunk_metadata);

            let c_filename = match CString::new(fmp.filename) {
                Ok(s) => s.into_raw(),
                Err(_) => return FFIStatus::InvalidString,
            };

            let ffi_fmp = {
                    $ffi_file_man_par {
                        filename: c_filename,
                        chunk_metadata: fmp_meta_ptr,
                        chunk_metadata_len: fmp.chunk_count as usize,
                    }
            };

            //Convert vector of hashed chunks
            let mut ffi_hashed_chunks: Vec<$ffi_hashed_chunk_data> = hashed_chunks.into_iter()
                .map(|(hash, mut data_vec)| {
                    let data_ptr = data_vec.as_mut_ptr();
                    let data_len = data_vec.len();
                    std::mem::forget(data_vec); //Give up ownership of the inner Vec<u8>
                    $ffi_hashed_chunk_data {
                        hash,
                        chunk_data: data_ptr,
                        chunk_data_len: data_len
                    }
                }).collect();
            
            let hashed_chunks_ptr = ffi_hashed_chunks.as_mut_ptr();
            let hashed_chunks_len = ffi_hashed_chunks.len();
            std::mem::forget(ffi_hashed_chunks);

            /*Prepare return data in struct.*/
            let output = Box::new({$ffi_file_man_chunks{
                fmp: ffi_fmp,
                hashed_chunks: hashed_chunks_ptr,
                hashed_chunks_len,
            }});

            unsafe {
                *out_ptr = Box::into_raw(output);
            };
            FFIStatus::Ok
        }
    };
}

create_file_manifest_and_chunks_ffi_setter!(
    "from `create_file_manifest_and_chunks_ffi_u64`.",
    create_file_manifest_and_chunks_ffi_u64,
    FFIFileManifestChunksU64,
    FFISSAChunkMetaU64,
    FFIFileManifestParentU64,
    FFIHashedChunkDataU64,
);

create_file_manifest_and_chunks_ffi_setter!(
    "from `create_file_manifest_and_chunks_ffi_u128`.",
    create_file_manifest_and_chunks_ffi_u128,
    FFIFileManifestChunksU128,
    FFISSAChunkMetaU128,
    FFIFileManifestParentU128,
    FFIHashedChunkDataU128,
);

macro_rules! free_file_manifest_and_chunks_ffi_setter {
    (
        $doc_l2:literal,
        $doc_l6:literal,
        $fn_name:ident,
        $ffi_parsed_man_chunks:ty,
        $ffi_ssa_chunk_meta:ty,
    ) => {
        /// Frees the memory allocated by 
        #[doc = $doc_l2]
        ///
        /// # Safety
        /// The caller MUST ensure that `ptr` is a valid pointer returned from
        #[doc = $doc_l6] 
        /// after this call. This function should only be called once for any
        /// given pointer.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            ptr: *mut $ffi_parsed_man_chunks
        ) {
            if ptr.is_null() {
                return;
            }

            unsafe {
                /*Retake ownership of the main struct to deallocate it at the
                end.*/
                let output_box = Box::from_raw(ptr);

                //Deallocate contents of the FFIFileManifestParent
                let _ = CString::from_raw(output_box.fmp.filename);

                //Reclaim and drop the Vec for the chunk metadata.
                let _ = Vec::from_raw_parts(
                    output_box.fmp.chunk_metadata as *mut $ffi_ssa_chunk_meta,
                    output_box.fmp.chunk_metadata_len,
                    output_box.fmp.chunk_metadata_len,
                );

                //Deallocate the array of FFIHashedChunk and their inner data
                let hashed_chunks_vec = Vec::from_raw_parts(
                    output_box.hashed_chunks,
                    output_box.hashed_chunks_len,
                    output_box.hashed_chunks_len,
                );

                /*Iterate through the chunks to deallocate the inner Vec<u8> 
                data for each.*/
                for chunk in hashed_chunks_vec {
                    let _ = Vec::from_raw_parts(
                        chunk.chunk_data, 
                        chunk.chunk_data_len, 
                        chunk.chunk_data_len);
                }
            }
        }
    };
}

free_file_manifest_and_chunks_ffi_setter!(
    "`create_file_manifest_and_chunks_ffi_u64` for a u64 hash.",
    "`create_file_manifest_and_chunks_ffi_u64` and that it is not used",
    free_file_manifest_and_chunks_ffi_u64,
    FFIFileManifestChunksU64,
    FFISSAChunkMetaU64,
);

free_file_manifest_and_chunks_ffi_setter!(
    "`create_file_manifest_and_chunks_ffi_u128` for a u128 hash.",
    "`create_file_manifest_and_chunks_ffi_u128` and that it is not used",
    free_file_manifest_and_chunks_ffi_u128,
    FFIFileManifestChunksU128,
    FFISSAChunkMetaU128,
);

/// Processes a single file from memory via an FFI-safe interface.
///
/// On success, returns `FFIStatus::Ok` and populates `out_ptr`.
///
/// # Safety
/// - `file_data` must be valid for reads. Its pointers must not be null.
/// - `out_ptr` must be a valid, non-null pointer to a 
///   `*mut FFIProcessedFileData`.
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
    file_data_vec.shrink_to_fit();
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
    chunks_vec.shrink_to_fit();
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
        let _ = Vec::from_raw_parts(
            output_box.chunks, 
            output_box.chunks_len, 
            output_box.chunks_len
        );
        let _ = Vec::from_raw_parts(
            output_box.file_data, 
            output_box.file_data_len, 
            output_box.file_data_len
        );
    }
}

macro_rules! verify_single_file_ffi_setter {
    (
        $doc_l2:literal,
        $fn_name:ident,
        $ffi_file_man_par:ty,
        $ffi_chunk_index_entry:ty,
        $hash_type:ty,
    ) => {
        /// Rebuilds a single file from chunks and verifies its integrity via
        #[doc = $doc_l2]
        ///
        /// # Safety
        /// All pointer arguments must be non-null and valid for their 
        /// specified lengths.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            file_manifest_parent: *const $ffi_file_man_par,
            veri_hash_array_ptr: *const u8,
            veri_hash_len: usize,
            user_data: *mut c_void,
            get_chunks_cb: unsafe extern "C" fn(
                user_data: *mut c_void, 
                hashes: *const $hash_type, 
                hashes_len: usize
            ) -> FFIChunkDataArray,
        ) -> FFIStatus {
            //Immediately check for null pointers to fail early.
            if file_manifest_parent.is_null()
                || veri_hash_array_ptr.is_null()
            {
                return FFIStatus::NullArgument;
            }

            if veri_hash_len != 64 {
                return FFIStatus::IncorrectArrayLength
            }

            let(fmp, veri_hash) = unsafe {
                //Dereference the main struct pointer once.
                let ffi_fmp_ref = &*file_manifest_parent;
                
                /*Prepare data_store, which is the second parameter of the 
                original rebuild_and_verify_single_file function, from C 
                input.*/
                let ffi_meta = std::slice::from_raw_parts(
                    ffi_fmp_ref.chunk_metadata,
                    ffi_fmp_ref.chunk_metadata_len
                );

                let vec_chunk_metadata: Vec<SSAChunkMeta<$hash_type>> = ffi_meta
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

                /*Prepare and reconstruct veri_hash, which is the fourth 
                parameter of the original verify_single_file 
                function, from C input.*/
                let veri_hash_slice = slice::from_raw_parts(
                    veri_hash_array_ptr, 
                    64
                );

                let veri_hash: [u8; 64] = veri_hash_slice.try_into().unwrap();

                (fmp, veri_hash)
            };

            let user_data_addr = user_data as usize;

            let get_chunks_closure = move |hashes: &[$hash_type]| {
                let ffi_chunks_array = unsafe {
                    get_chunks_cb(
                        user_data_addr as *mut c_void, 
                        hashes.as_ptr(), 
                        hashes.len()
                    )};
                
                let ffi_chunks_slice = unsafe {slice::from_raw_parts(
                    ffi_chunks_array.ptr, 
                    ffi_chunks_array.len
                )};

                let chunks: Vec<Vec<u8>> = ffi_chunks_slice.iter().map(|c| {
                    unsafe {Vec::from_raw_parts(c.ptr, c.len, c.len)}
                }).collect();
                
                let _ = unsafe {Vec::from_raw_parts(
                    ffi_chunks_array.ptr, 
                    ffi_chunks_array.len, 
                    ffi_chunks_array.len)};
                chunks
            };

            match verify_single_file(
                &fmp, &veri_hash, get_chunks_closure
            ){
                Ok(()) => FFIStatus::Ok,
                Err(e) => match e {
                    SpriteShrinkError::Processing(processing_error) 
                        => match processing_error {
                            ProcessingError::HashMismatchError(_) => 
                                FFIStatus::VerificationHashMismatch,
                            ProcessingError::VerificationError(_) => 
                                FFIStatus::VerificationMissingChunk,
                            _ => FFIStatus::InternalError,
                        }
                    _ => FFIStatus::InternalError,
                },
            }
        }
    };
}

verify_single_file_ffi_setter!(
    "an FFI-safe interface for u64 hashes.",
    verify_single_file_ffi_u64,
    FFIFileManifestParentU64,
    FFIChunkIndexEntryU64,
    u64,
);

verify_single_file_ffi_setter!(
    "an FFI-safe interface for u128 hashes.",
    verify_single_file_ffi_u128,
    FFIFileManifestParentU128,
    FFIChunkIndexEntryU128,
    u128,
);

macro_rules! test_compression_ffi_setter {
    (
        $doc_l2:literal,
        $fn_name:ident,
        $ffi_data_store_entry:ty,
        $hash_type:ty,
    ) => {
        /// Estimates the compressed size of a data store when using 
        #[doc = $doc_l2]
        /// 
        /// On success, returns `FFIStatus::Ok` and populates `out_size`.
        ///
        /// # Safety
        /// - All input pointers must be non-null and valid for their specified
        /// lengths.
        /// - `out_size` must be a valid, non-null pointer to a `u64`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            total_data_size: u64,
            sorted_hashes_array_ptr: *const $hash_type,
            sorted_hashes_len: usize,
            worker_count: usize,
            dictionary_size: usize,
            out_size: *mut u64,
            user_data: *mut c_void,
            get_chunks_cb: unsafe extern "C" fn(
                user_data: *mut c_void, 
                hashes: *const $hash_type, 
                hashes_len: usize
            ) -> FFIChunkDataArray,
        ) -> FFIStatus {
            let sorted_hashes = unsafe {
                //Convert C inputs to Rust types

                /*Prepare data_store, which is the second parameter of the 
                original test_compression function, from C input.*/
                let sorted_hashes_slice = slice::from_raw_parts(
                    sorted_hashes_array_ptr, 
                    sorted_hashes_len
                );

                (sorted_hashes_slice)
            };

            let user_data_addr = user_data as usize;

            let get_chunks_closure = move |hashes: &[$hash_type]| {
                let ffi_chunks_array = unsafe {
                    get_chunks_cb(
                        user_data_addr as *mut c_void, 
                        hashes.as_ptr(), 
                        hashes.len()
                    )};
                
                let ffi_chunks_slice = unsafe {slice::from_raw_parts(
                    ffi_chunks_array.ptr, 
                    ffi_chunks_array.len
                )};

                let chunks: Vec<Vec<u8>> = ffi_chunks_slice.iter().map(|c| {
                    unsafe {Vec::from_raw_parts(c.ptr, c.len, c.len)}
                }).collect();
                
                let _ = unsafe {Vec::from_raw_parts(
                    ffi_chunks_array.ptr, 
                    ffi_chunks_array.len, 
                    ffi_chunks_array.len)};
                chunks
            };

            //Run test compression with received and converted data.
            match test_compression(
                sorted_hashes, 
                total_data_size,
                worker_count, 
                dictionary_size,
                get_chunks_closure
            ) {
                Ok(compressed_size) => {
                    unsafe {
                        *out_size = compressed_size as u64;
                    }
                    
                    FFIStatus::Ok
                }
                Err(e) => match e {
                    SpriteShrinkError::Processing(ProcessingError::DictionaryError(_)) => {
                        FFIStatus::DictionaryError
                    }
                    _ => FFIStatus::InternalError,
                }
            }
        }
    };
}

test_compression_ffi_setter!(
    "u64 hashes.",
    test_compression_ffi_u64,
    FFIDataStoreEntryU64,
    u64,
);

test_compression_ffi_setter!(
    "u128 hashes.",
    test_compression_ffi_u128,
    FFIDataStoreEntryU128,
    u128,
);

macro_rules! get_seek_chunks_ffi_setter {
    (
        $doc_l10:literal,
        $fn_name:ident,
        $ffi_file_man_par:ty,
        $ffi_seek_info_array:path,
        $hash_type:ty,
        $ffi_seek_chunk_info:path,
        $ffi_ssa_chunk_meta:ty
    ) => {
        /// Calculates which chunks are needed for a seek operation via an 
        /// FFI-safe interface.
        ///
        /// # Safety
        /// - `file_manifest_parent` must be a valid, non-null pointer.
        /// - `out_ptr` must be a valid pointer to a 
        ///   `*mut FFISeekInfoArray...`.
        /// - On success, the pointer written to `out_ptr` is owned by the C 
        ///   caller and MUST be freed by passing it to the 
        #[doc = $doc_l10]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            file_manifest_parent: *const $ffi_file_man_par,
            seek_offset: u64,
            seek_length: u64,
            out_ptr: *mut *mut $ffi_seek_info_array,
        ) -> FFIStatus {
            if file_manifest_parent.is_null() || out_ptr.is_null() {
                return FFIStatus::NullArgument;
            }

            let fmp = unsafe{
                //Dereference the main struct pointer once.
                let ffi_fmp_ref = &*file_manifest_parent;
                
                /*Prepare data_store, which is the second parameter of the 
                original rebuild_and_verify_single_file function, from C 
                input.*/
                let ffi_meta = std::slice::from_raw_parts(
                    ffi_fmp_ref.chunk_metadata,
                    ffi_fmp_ref.chunk_metadata_len
                );

                let vec_chunk_metadata: Vec<SSAChunkMeta<$hash_type>> = ffi_meta
                .iter()
                .map(|meta| SSAChunkMeta {
                    hash: meta.hash,
                    offset: meta.offset,
                    length: meta.length
                })
                .collect();

                FileManifestParent {
                    filename: std::ffi::CStr::from_ptr(ffi_fmp_ref.filename)
                        .to_string_lossy()
                        .into_owned(),
                    chunk_count: ffi_fmp_ref.chunk_metadata_len as u64,
                    chunk_metadata: vec_chunk_metadata
                }
            };

            match get_seek_chunks::<$hash_type>(
                &fmp, 
                seek_offset, 
                seek_length
            ){
                Ok(seek_info_vec) => {
                    let mut ffi_chunks: Vec<$ffi_seek_chunk_info> = 
                        seek_info_vec
                            .into_iter()
                            .map(|(hash, (start, end))| 
                                {$ffi_seek_chunk_info {
                                    hash,
                                    read_start: start,
                                    read_end: end,
                                }
                            })
                            .collect();

                    let chunks_ptr = ffi_chunks.as_mut_ptr();
                    let chunks_len = ffi_chunks.len();

                    // Give up ownership so memory isn't freed here
                    std::mem::forget(ffi_chunks); 

                    /*Create the final struct that holds the array pointer 
                    and length*/
                    let output = Box::new({
                            $ffi_seek_info_array {
                                chunks: chunks_ptr,
                                chunks_len,
                            }
                    });

                    unsafe {
                        *out_ptr = Box::into_raw(output);
                    }

                    FFIStatus::Ok
                }
                Err(e) => match e {
                    SpriteShrinkError::Processing(ProcessingError::SeekOutOfBounds(_)) => {
                        FFIStatus::SeekOutOfBounds
                    }
                    _ => FFIStatus::InternalError,
                }
            }
        }
    };
}

get_seek_chunks_ffi_setter!(
    "  `free_seek_chunks_ffi_u64` function.",
    get_seek_chunks_ffi_64,
    FFIFileManifestParentU64,
    FFISeekInfoArrayU64,
    u64,
    FFISeekChunkInfoU64,
    FFISSAChunkMetaU64
);

get_seek_chunks_ffi_setter!(
    "  `free_seek_chunks_ffi_u128` function.",
    get_seek_chunks_ffi_128,
    FFIFileManifestParentU128,
    FFISeekInfoArrayU128,
    u128,
    FFISeekChunkInfoU128,
    FFISSAChunkMetaU128
);

macro_rules! free_seek_chunks_ffi_setter {
    (
        $doc_l5:literal,
        $fn_name:ident,
        $ffi_seek_info_array:ty,
        $ffi_seek_chunk_info:ty
    ) => {
        /// Frees the memory allocated by a `get_seek_chunks_ffi` function.
        ///
        /// # Safety
        /// The `ptr` must be a non-null pointer returned from a successful 
        #[doc = $doc_l5]
        /// Calling this with a null pointer or a pointer that has already 
        /// been freed will lead to undefined behavior.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            ptr: *mut $ffi_seek_info_array
        ) {
            if ptr.is_null() {
                return;
            }

            unsafe{
                /*Retake ownership of the main struct to deallocate it at the
                end.*/
                let output_box = Box::from_raw(ptr);

                let _ = Vec::from_raw_parts(
                    output_box.chunks,
                    output_box.chunks_len,
                    output_box.chunks_len,
                );
            }
        }
    };
}

free_seek_chunks_ffi_setter!(
    "call to the `get_seek_chunks_ffi_u64` function.",
    free_seek_chunks_ffi_u64,
    FFISeekInfoArrayU64,
    FFISeekChunkInfoU64
);

free_seek_chunks_ffi_setter!(
    "call to the `get_seek_chunks_ffi_u128` function.",
    free_seek_chunks_ffi_u128,
    FFISeekInfoArrayU128,
    FFISeekChunkInfoU128
);