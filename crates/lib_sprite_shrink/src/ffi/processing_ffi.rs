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

use crate::ffi::ffi_error_handling::{
    FFIResult
};
use crate::ffi::ffi_types::{
    CreateFileManifestAndChunksArgs, FFIChunk, FFIChunkDataArray,
    ChunkIndexMap, FFIChunkLocation,
    FFIFileManifestChunks,
    FFIFileManifestChunksU64, FFIFileManifestChunksU128,
    FFIFileData, FFIFileManifestParent,
    FFIHashedChunkData,
    FFIFileManifestParentU64, FFIFileManifestParentU128,
    FFIProcessedFileData,
    FFISeekChunkInfo, FFISeekInfoArray,
    FFISeekInfoArrayU64, FFISeekInfoArrayU128,
    FFISSAChunkMeta,
    VerifySingleFileArgsU64, VerifySingleFileArgsU128,
    TestCompressionArgsU64, TestCompressionArgsU128
};
use crate::lib_error_handling::{
    SpriteShrinkError
};
use crate::processing::{
    create_file_manifest_and_chunks, get_seek_chunks, process_file_in_memory,
    verify_single_file, test_compression};
use crate::lib_structs::{
    FileData, FileManifestParent, SSAChunkMeta
};

/// Creates a file manifest and a list of data chunks via an FFI‑safe interface
/// for 64‑bit hash keys.
///
/// The function parses the supplied file name, data slice, and chunk
/// descriptors and produces an `FFIFileManifestChunksU64` that the C
/// caller can consume.  The returned object owns all memory needed for the
/// manifest and the chunks; the caller must free it with
/// `free_file_manifest_and_chunks_u64`.
///
/// # Arguments
///
/// * `args`: A POD container that bundles a C‑string file name, a raw data
///   slice, and an array of `FFIChunk` descriptors  and their lengths)
/// * `out_ptr`: non‑null pointer to a mutable `*mut
///   FFIFileManifestChunksU64` where the result will be stored.
///
/// # Returns
///
/// * `FFIResult::StatusOk` - `*out_ptr` now points to a valid
///   `FFIFileManifestChunksU64`
/// * `FFIResult::NullArgument` - any of the pointers above were `NULL`
///
/// # Safety
///
/// The caller must guarantee that:
/// * All pointers refer to valid, readable memory for the stated lengths.
/// * `out_ptr` is a valid, non‑null pointer that the caller will read after
///   the call.
/// * The returned struct and all nested data are allocated on the heap; the
///   caller is responsible for freeing it with
///   `free_file_manifest_and_chunks_u64` to avoid leaks.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn create_file_manifest_and_chunks_u64(
    args: *const CreateFileManifestAndChunksArgs,
    out_ptr: *mut *mut FFIFileManifestChunksU64
) -> FFIResult {
    if args.is_null() ||
        out_ptr.is_null() {
            return FFIResult::NullArgument;
    };

    let args_int = unsafe{&*args};

    if args_int.file_name_ptr.is_null() ||
        args_int.file_data_array_ptr.is_null() ||
        args_int.chunks_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    let (file_name, file_data, chunks) = unsafe {
        /*Prepare file_name, which is the first parameter of the original
        create_file_manifest_and_chunks function, from C input.*/
        let file_name = std::ffi::CStr::from_ptr(args_int.file_name_ptr)
            .to_string_lossy()
            .into_owned();

        /*Prepare file_data, which is the second parameter of the original
        create_file_manifest_and_chunks function, from C input.*/
        let file_data = slice::from_raw_parts(
            args_int.file_data_array_ptr,
            args_int.file_data_len
        );

        /*Prepare chunks, which is the third parameter of the original
        create_file_manifest_and_chunks function, from C input.*/
        let chunks = {
            let chunk_slice =
                slice::from_raw_parts(
                    args_int.chunks_array_ptr,
                    args_int.chunks_len
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
    let (fmp, hashed_chunks) = create_file_manifest_and_chunks(
        &file_name, file_data,
        &chunks
    );

    let mut ffi_chunk_metadata: Vec<FFISSAChunkMeta<u64>> = fmp.chunk_metadata
        .iter()
        .map(|meta| FFISSAChunkMeta::from(*meta))
        .collect();
    let fmp_meta_ptr = ffi_chunk_metadata.as_mut_ptr();
    let fmp_meta_len = ffi_chunk_metadata.len();
    let fmp_meta_cap = ffi_chunk_metadata.capacity();
    std::mem::forget(ffi_chunk_metadata);

    let c_filename = match CString::new(fmp.filename.clone()) {
        Ok(s) => s.into_raw(),
        Err(_) => return FFIResult::InvalidString,
    };

    let ffi_fmp = FFIFileManifestParent::<u64> {
        filename: c_filename,
        chunk_metadata: fmp_meta_ptr,
        chunk_metadata_len: fmp_meta_len,
        chunk_metadata_cap: fmp_meta_cap,
    };

    //Convert vector of hashed chunks
    let mut ffi_hashed_chunks: Vec<FFIHashedChunkData<u64>> = hashed_chunks
        .into_iter()
        .map(|(hash, mut data_vec)| {
            let data_ptr = data_vec.as_mut_ptr();
            let data_len = data_vec.len();
            let data_cap = data_vec.capacity();

            /*data_vec is now owned and must be prevented from being
            dropped at the end of this closure, as we are transferring its
            allocation to the FFI caller.*/
            std::mem::forget(data_vec);
            FFIHashedChunkData::from((
                hash,
                data_ptr,
                data_len,
                data_cap
            ))
        }).collect();

    let hashed_chunks_ptr = ffi_hashed_chunks.as_mut_ptr();
    let hashed_chunks_len = ffi_hashed_chunks.len();
    let hashed_chunks_cap = ffi_hashed_chunks.capacity();
    std::mem::forget(ffi_hashed_chunks);

    /*Prepare return data in struct.*/
    let output = Box::new(FFIFileManifestChunks::from((
        ffi_fmp,
        hashed_chunks_ptr,
        hashed_chunks_len,
        hashed_chunks_cap
    )));

    unsafe {
        *out_ptr = Box::into_raw(output);
    };
    FFIResult::StatusOk
}

/// Creates a file manifest and a list of data chunks via an FFI‑safe interface
/// for 128‑bit hash keys.
///
/// The arguments and semantics are identical to
/// `create_file_manifest_and_chunks_u64`, but the resulting struct is
/// `FFIFileManifestChunksU128`.  The caller must free the returned value
/// with `free_file_manifest_and_chunks_u128`.
///
/// # Arguments
///
/// * `args`: A POD container that bundles a C‑string file name, a raw data
///   slice, and an array of `FFIChunk` descriptors  and their lengths).
/// * `out_ptr` – non‑null pointer to a mutable `*mut
///   FFIFileManifestChunksU128` where the result will be stored.
///
/// # Returns
///
/// * `FFIResult::StatusOk` – `*out_ptr` now points to a valid
///   `FFIFileManifestChunksU128`
/// * `FFIResult::NullArgument` – any of the pointers above were `NULL`
///
/// # Safety
///
/// The caller must guarantee that:
/// * All pointers refer to valid, readable memory for the stated lengths.
/// * `out_ptr` is a valid, non‑null pointer that the caller will read after
///   the call.
/// * The returned struct and all nested data are allocated on the heap; the
///   caller is responsible for freeing it with
///   `free_file_manifest_and_chunks_u128` to avoid leaks.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn create_file_manifest_and_chunks_u128(
    args: *const CreateFileManifestAndChunksArgs,
    out_ptr: *mut *mut FFIFileManifestChunksU128
) -> FFIResult {
    if args.is_null() ||
        out_ptr.is_null() {
            return FFIResult::NullArgument;
    };

    let args_int = unsafe{&*args};

    if args_int.file_name_ptr.is_null() ||
        args_int.file_data_array_ptr.is_null() ||
        args_int.chunks_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    let (file_name, file_data, chunks) = unsafe {
        /*Prepare file_name, which is the first parameter of the original
        create_file_manifest_and_chunks function, from C input.*/
        let file_name = std::ffi::CStr::from_ptr(args_int.file_name_ptr)
            .to_string_lossy()
            .into_owned();

        /*Prepare file_data, which is the second parameter of the original
        create_file_manifest_and_chunks function, from C input.*/
        let file_data = slice::from_raw_parts(
            args_int.file_data_array_ptr,
            args_int.file_data_len
        );

        /*Prepare chunks, which is the third parameter of the original
        create_file_manifest_and_chunks function, from C input.*/
        let chunks = {
            let chunk_slice =
                slice::from_raw_parts(
                    args_int.chunks_array_ptr,
                    args_int.chunks_len
                );

            chunk_slice
                .iter()
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

    let (fmp, hashed_chunks) = create_file_manifest_and_chunks::<u128>(
        &file_name, file_data,
        &chunks
    );

    let mut ffi_chunk_metadata: Vec<FFISSAChunkMeta<[u8; 16]>> = fmp
        .chunk_metadata
        .iter()
        .map(|meta| FFISSAChunkMeta {
            hash: meta.hash.to_le_bytes(),
            offset: meta.offset,
            length: meta.length,
        })
        .collect();

    let fmp_meta_ptr = ffi_chunk_metadata.as_mut_ptr();
    let fmp_meta_len = ffi_chunk_metadata.len();
    let fmp_meta_cap = ffi_chunk_metadata.capacity();
    std::mem::forget(ffi_chunk_metadata);

    let c_filename = match CString::new(fmp.filename.clone()) {
        Ok(s) => s.into_raw(),
        Err(_) => return FFIResult::InvalidString,
    };

    let ffi_fmp = FFIFileManifestParent::<[u8; 16]> {
        filename: c_filename,
        chunk_metadata: fmp_meta_ptr,
        chunk_metadata_len: fmp_meta_len,
        chunk_metadata_cap: fmp_meta_cap,
    };

    let mut ffi_hashed_chunks: Vec<FFIHashedChunkData<[u8; 16]>> = hashed_chunks
        .into_iter()
        .map(|(hash, mut data_vec)| {
            let data_ptr = data_vec.as_mut_ptr();
            let data_len = data_vec.len();
            let data_cap = data_vec.capacity();

            /*data_vec is now owned and must be prevented from being
            dropped at the end of this closure, as the library is transferring its
            allocation to the FFI caller.*/
            std::mem::forget(data_vec);
            FFIHashedChunkData::from((
                hash.to_le_bytes(),
                data_ptr,
                data_len,
                data_cap
            ))
        }).collect();

    let hashed_chunks_ptr = ffi_hashed_chunks.as_mut_ptr();
    let hashed_chunks_len = ffi_hashed_chunks.len();
    let hashed_chunks_cap = ffi_hashed_chunks.capacity();
    std::mem::forget(ffi_hashed_chunks);

    /*Prepare return data in struct.*/
    let output = Box::new(FFIFileManifestChunks::from((
        ffi_fmp,
        hashed_chunks_ptr,
        hashed_chunks_len,
        hashed_chunks_cap
    )));

    unsafe {
        *out_ptr = Box::into_raw(output);
    };

    FFIResult::StatusOk
}

/// A generic helper to deallocate an `FFIFileManifestChunks` and its contents.
///
/// This internal function is responsible for the complex memory cleanup
/// required for
/// the data structure returned by `create_file_manifest_and_chunks_internal`.
/// It safely deallocates not only the top-level struct but also all the nested,
/// heap-allocated data associated with it.
///
/// This cleanup process involves:
/// 1. Reconstructing the `Box` for the main `FFIFileManifestChunks` struct.
/// 2. Reconstructing the `CString` for the `filename` to deallocate it.
/// 3. Reconstructing the `Vec` for the `chunk_metadata` array.
/// 4. Reconstructing the `Vec` for the `hashed_chunks` array.
/// 5. Iterating through the `hashed_chunks` and reconstructing the inner
///    `Vec<u8>` for each chunk's data to deallocate them individually.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used in the manifest's
///   chunk metadata, ensuring pointers are cast and deallocated correctly.
///
/// # Arguments
///
/// * `ptr`: The raw pointer to the `FFIFileManifestChunks` struct to be freed.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - The `ptr` is a valid, non-null pointer originally returned from a
///   successful call to a `create_file_manifest_and_chunks_*` function.
/// - The pointer has not already been freed.
/// - The pointer, and any pointers contained within its structures, will not
///   be used again after this function is called.
///
/// Failure to uphold these conditions will result in undefined behavior, such
/// as double-freeing memory or use-after-free vulnerabilities.
fn free_file_manifest_and_chunks_internal<H>(
    ptr: *mut FFIFileManifestChunks<H>
) {
    unsafe {
        /*Retake ownership of the main struct to deallocate it at the
        end.*/
        let output_box = Box::from_raw(ptr);

        //Deallocate contents of the FFIFileManifestParent
        let _ = CString::from_raw(output_box.fmp.filename);

        //Reclaim and drop the Vec for the chunk metadata.
        let _ = Vec::from_raw_parts(
            output_box.fmp.chunk_metadata as *mut FFISSAChunkMeta<H>,
            output_box.fmp.chunk_metadata_len,
            output_box.fmp.chunk_metadata_cap,
        );

        //Deallocate the array of FFIHashedChunk and their inner data
        let hashed_chunks_vec = Vec::from_raw_parts(
            output_box.hashed_chunks,
            output_box.hashed_chunks_len,
            output_box.hashed_chunks_cap,
        );

        /*Iterate through the chunks to deallocate the inner Vec<u8>
        data for each.*/
        for chunk in hashed_chunks_vec {
            let _ = Vec::from_raw_parts(
                chunk.chunk_data,
                chunk.chunk_data_len,
                chunk.chunk_data_cap);
        }
    }
}

/// Frees the memory allocated by `create_file_manifest_and_chunk_u64`
/// for a u64 hash.
///
/// # Safety
/// The caller MUST ensure that `ptr` is a valid pointer returned from
/// `create_file_manifest_and_chunks_u64` and that it is not used
/// after this call. This function should only be called once for any
/// given pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_manifest_and_chunks_u64(
    ptr: *mut FFIFileManifestChunksU64
) {
    if ptr.is_null() {
        return;
    }

    free_file_manifest_and_chunks_internal::<u64>(
        ptr
    );
}

/// Frees the memory allocated by `create_file_manifest_and_chunks_u128`
/// for a u128 hash.
///
/// # Safety
/// The caller MUST ensure that `ptr` is a valid pointer returned from
/// `create_file_manifest_and_chunks_u128` and that it is not used
/// after this call. This function should only be called once for any
/// given pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_manifest_and_chunks_u128(
    ptr: *mut FFIFileManifestChunksU128
) {
    if ptr.is_null() {
        return;
    }

    free_file_manifest_and_chunks_internal::<[u8; 16]>(
        ptr
    );
}

/// Processes a single file from memory via an FFI-safe interface.
///
/// On success, returns `FFIResult::StatusOk` and populates `out_ptr`.
///
/// # Safety
/// - `file_data` must be valid for reads. Its pointers must not be null.
/// - `out_ptr` must be a valid, non-null pointer to a
///   `*mut FFIProcessedFileData`.
/// - On success, the pointer written to `out_ptr` is owned by the caller and
///   MUST be freed by passing it to `free_processed_file_data_ffi`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn process_file_in_memory_ffi(
    file_data: FFIFileData,
    window_size: u64,
    out_ptr: *mut *mut FFIProcessedFileData,
) -> FFIResult {
    if file_data.filename.is_null()
        || file_data.file_data.is_null()
        || out_ptr.is_null() {
        return FFIResult::NullArgument;
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
        Some(data) => data,
        None => return FFIResult::NoDataToProcess,
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

    let file_data_ptr = file_data_vec.as_mut_ptr();
    let file_data_len = file_data_vec.len();
    let file_data_cap = file_data_vec.capacity();

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

    let chunks_ptr = chunks_vec.as_mut_ptr();
    let chunks_len = chunks_vec.len();
    let chunks_cap = chunks_vec.capacity();

    std::mem::forget(chunks_vec); //Give up ownership, no longer needed

    //Prepare and return FFIProcessedFileData
    let output = Box::new(FFIProcessedFileData {
        filename: filename_ptr,
        filename_len,
        veri_hash: veri_hash_ptr,
        chunks: chunks_ptr,
        chunks_len,
        chunks_cap,
        file_data: file_data_ptr,
        file_data_len,
        file_data_cap,
    });

    unsafe {
        *out_ptr = Box::into_raw(output);
    };

    FFIResult::StatusOk
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
            output_box.chunks_cap
        );
        let _ = Vec::from_raw_parts(
            output_box.file_data,
            output_box.file_data_len,
            output_box.file_data_cap
        );
    }
}

/// Verify a single file by reconstructing it from stored chunks and checking its
/// verification hash.
///
/// This is the FFI‑exposed entry point for 64‑bit hash types. It expects a
/// pointer to a `VerifySingleFileArgsU64` struct that bundles all required
/// arguments and callbacks. The function is `unsafe` because it dereferences
/// raw pointers that cross the FFI boundary.
///
/// # Parameters
///
/// * `args` – Pointer to a fully‑initialised `VerifySingleFileArgsU64`
///   containing all required arguments.
///
/// # Safety
///
/// * All pointer arguments **must** be non‑null and point to valid memory that
///   lives at least for the duration of the call.
/// * The `get_chunks_cb` must not unwind across the FFI boundary and must return a
///   correctly‑populated `FFIChunkDataArray`.
///
/// If any pointer is null, the function returns `FFIResult::NullArgument`.
///
/// # Return Value
///
/// Returns an `FFIResult` indicating success (`FFIResult::StatusOk`) or the type of
/// failure (e.g., `NullArgument`, `InvalidArgument`, or an internal error).
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn verify_single_file_u64(
    args: *const VerifySingleFileArgsU64,
) -> FFIResult {
    //Immediately check for null pointers to fail early.
    if args.is_null() {
        return FFIResult::NullArgument;
    }

    let args_int = unsafe{&*args};

    if args_int.file_manifest_parent.is_null() ||
        args_int.veri_hash_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    let(fmp, veri_hash) = unsafe {
        /*Dereference the main struct pointer and convert using from.*/
        let fmp = FileManifestParent::<u64>::from(&*args_int.file_manifest_parent);

        /*Prepare and reconstruct veri_hash, which is the fourth
        parameter of the original verify_single_file
        function, from C input.*/
        let veri_hash: [u8; 64] = *(args_int.veri_hash_array_ptr as *const [u8; 64]);

        (fmp, veri_hash)
    };

    let get_chunks_cb = args_int.get_chunks_cb;
    let progress_cb = args_int.progress_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u64]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
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
            unsafe {Vec::from_raw_parts(c.ptr, c.len, c.cap)}
        }).collect();

        let _ = unsafe {Vec::from_raw_parts(
            ffi_chunks_array.ptr,
            ffi_chunks_array.len,
            ffi_chunks_array.cap)};
        Ok(chunks)
    };

    let progress_closure = move |bytes_processed: u64| {
        unsafe {
            progress_cb(user_data_addr as *mut c_void, bytes_processed);
        }
    };

    match verify_single_file(
        &fmp, &veri_hash, get_chunks_closure, progress_closure
    ){
        Ok(()) => FFIResult::StatusOk,
        Err(e) => e.into(),
    }
}

/// Verify a single file by reconstructing it from stored chunks and checking
/// its verification hash.
///
/// This is the FFI‑exposed entry point for 128‑bit hash types. It expects a
/// pointer to a `VerifySingleFileArgsU128` struct (see `ffi_types.rs`) that
/// bundles all required arguments and callbacks. The function is `unsafe`
/// because it dereferences raw pointers that cross the FFI boundary.
///
/// # Parameters
///
/// * `args` – Pointer to a fully‑initialised `VerifySingleFileArgsU128`
///   containing all required arguments.
///
/// # Safety
///
/// * All pointer arguments **must** be non‑null and point to valid memory that
///   lives at least for the duration of the call.
/// * The `get_chunks_cb` must not unwind across the FFI boundary and must
///   return a correctly‑populated `FFIChunkDataArray`.
///
/// If any pointer is null, the function returns `FFIResult::NullArgument`.
///
/// # Return Value
///
/// Returns an `FFIResult` indicating success (`FFIResult::StatusOk`) or the type of
/// failure (e.g., `NullArgument`, `InvalidArgument`, or an internal error).
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn verify_single_file_u128(
    args: *const VerifySingleFileArgsU128,
) -> FFIResult {
    //Immediately check for null pointers to fail early.
    if args.is_null() {
        return FFIResult::NullArgument;
    }

    let args_int = unsafe{&*args};

    if args_int.file_manifest_parent.is_null() ||
        args_int.veri_hash_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    let(fmp, veri_hash) = unsafe {
        /*Dereference the main struct pointer and convert using from.*/
        let ffi_fmp = &*args_int.file_manifest_parent;
        let filename = std::ffi::CStr::from_ptr(ffi_fmp.filename)
            .to_string_lossy()
            .into_owned();

        let chunk_metadata_slice = slice::from_raw_parts(
            ffi_fmp.chunk_metadata,
            ffi_fmp.chunk_metadata_len
        );

        let chunk_metadata = chunk_metadata_slice
            .iter()
            .map(|meta| SSAChunkMeta {
                hash: u128::from_le_bytes(meta.hash),
                offset: meta.offset,
                length: meta.length,
            })
            .collect();

        let fmp = FileManifestParent {
            filename,
            chunk_metadata,
            chunk_count: ffi_fmp.chunk_metadata_len as u64,
        };

        /*Prepare and reconstruct veri_hash, which is the fourth
        parameter of the original verify_single_file
        function, from C input.*/
        let veri_hash: [u8; 64] = *(args_int.veri_hash_array_ptr as *const [u8; 64]);

        (fmp, veri_hash)
    };

    let get_chunks_cb = args_int.get_chunks_cb;
    let progress_cb = args_int.progress_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u128]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let ffi_hash_bytes: Vec<u8> = hashes
            .iter()
            .flat_map(|h| h.to_le_bytes())
            .collect();

        let ffi_chunks_array = unsafe {
            (get_chunks_cb)(
                user_data_addr as *mut c_void,
                ffi_hash_bytes.as_ptr(),
                hashes.len()
            )};

        let ffi_chunks_slice = unsafe {slice::from_raw_parts(
            ffi_chunks_array.ptr,
            ffi_chunks_array.len
        )};

        let chunks: Vec<Vec<u8>> = ffi_chunks_slice.iter().map(|c| {
            unsafe {Vec::from_raw_parts(c.ptr, c.len, c.cap)}
        }).collect();

        let _ = unsafe {Vec::from_raw_parts(
            ffi_chunks_array.ptr,
            ffi_chunks_array.len,
            ffi_chunks_array.cap)};
        Ok(chunks)
    };

    let progress_closure = move |bytes_processed: u64| {
        unsafe {
            (progress_cb)(user_data_addr as *mut c_void, bytes_processed);
        }
    };

    match verify_single_file(
        &fmp, &veri_hash, get_chunks_closure, progress_closure
    ){
        Ok(()) => FFIResult::StatusOk,
        Err(e) => e.into(),
    }
}

/// Estimates the compressed size of a data store when using
/// u64 hashes.
///
/// On success, returns `FFIResult::StatusOk` and populates `out_size`.
///
/// # Safety
/// - All input pointers must be non-null and valid for their specified
///   lengths.
/// - `out_size` must be a valid, non-null pointer to a `u64`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn test_compression_u64(
    args: *const TestCompressionArgsU64,
    out_size: *mut u64
) -> FFIResult {
    if args.is_null() ||
        out_size.is_null() {
            return FFIResult::NullArgument;
    };

    let args_int = unsafe{&*args};

    if args_int.sorted_hashes_array_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let sorted_hashes = unsafe {
        //Convert C inputs to Rust types

        /*Prepare data_store, which is the second parameter of the
        original test_compression function, from C input.*/
        slice::from_raw_parts(
            args_int.sorted_hashes_array_ptr,
            args_int.sorted_hashes_len
        )
    };

    let get_chunks_cb = args_int.get_chunks_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u64]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let mut ffi_chunks_array = FFIChunkDataArray { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
        let status = unsafe {
            (get_chunks_cb)(
                user_data_addr as *mut c_void,
                hashes.as_ptr(),
                hashes.len(),
                & mut ffi_chunks_array
            )
        };
        Result::from(status)?;
        let ffi_chunks_slice = unsafe {slice::from_raw_parts(
            ffi_chunks_array.ptr,
            ffi_chunks_array.len
        )};

        let chunks: Vec<Vec<u8>> = ffi_chunks_slice.iter().map(|c| {
            unsafe {Vec::from_raw_parts(c.ptr, c.len, c.cap)}
        }).collect();

        let _ = unsafe {Vec::from_raw_parts(
            ffi_chunks_array.ptr,
            ffi_chunks_array.len,
            ffi_chunks_array.cap)};
        Ok(chunks)
    };

    match test_compression(
        sorted_hashes,
        args_int.total_data_size,
        args_int.worker_count,
        args_int.dictionary_size,
        args_int.compression_level,
        get_chunks_closure
    ) {
        Ok(compressed_size) => {
            unsafe {
                *out_size = compressed_size as u64;
            }

            FFIResult::StatusOk
        }
        Err(e) => e.into()
    }
}

/// Estimates the compressed size of a data store when using
/// u128 hashes.
///
/// On success, returns `FFIResult::StatusOk` and populates `out_size`.
///
/// # Safety
/// - All input pointers must be non-null and valid for their specified
///   lengths.
/// - `out_size` must be a valid, non-null pointer to a `u64`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn test_compression_u128(
    args: *const TestCompressionArgsU128,
    out_size: *mut u64,
) -> FFIResult {
    if args.is_null() ||
        out_size.is_null() {
            return FFIResult::NullArgument;
    };

    let args_int = unsafe{&*args};

    if args_int.sorted_hashes_array_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let sorted_hashes_bytes = unsafe {
        //Convert C inputs to Rust types

        /*Prepare data_store, which is the second parameter of the
        original test_compression function, from C input.*/
        slice::from_raw_parts(
            args_int.sorted_hashes_array_ptr,
            args_int.sorted_hashes_len * 16
        )
    };

    let sorted_hashes: Vec<u128> = sorted_hashes_bytes
        .chunks_exact(16)
        .map(|chunk| u128::from_le_bytes(chunk.try_into().unwrap()))
        .collect();

    let get_chunks_cb = args_int.get_chunks_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u128]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let ffi_hash_bytes: Vec<u8> = hashes.iter().flat_map(|h| h.to_le_bytes()).collect();

        let mut ffi_chunks_array = FFIChunkDataArray {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0
        };

        let status = unsafe {
            (get_chunks_cb)(
                user_data_addr as *mut c_void,
                ffi_hash_bytes.as_ptr(),
                hashes.len(),
                & mut ffi_chunks_array
            )
        };
        Result::from(status)?;
        let ffi_chunks_slice = unsafe {slice::from_raw_parts(
            ffi_chunks_array.ptr,
            ffi_chunks_array.len
        )};

        let chunks: Vec<Vec<u8>> = ffi_chunks_slice.iter().map(|c| {
            unsafe {Vec::from_raw_parts(c.ptr, c.len, c.cap)}
        }).collect();

        let _ = unsafe {Vec::from_raw_parts(
            ffi_chunks_array.ptr,
            ffi_chunks_array.len,
            ffi_chunks_array.cap)};
        Ok(chunks)
    };

    match test_compression(
        &sorted_hashes,
        args_int.total_data_size,
        args_int.worker_count,
        args_int.dictionary_size,
        args_int.compression_level,
        get_chunks_closure
    ) {
        Ok(compressed_size) => {
            unsafe {
                *out_size = compressed_size as u64;
            }

            FFIResult::StatusOk
        }
        Err(e) => e.into()
    }
}

/// Calculates which chunks are needed for a seek operation via an
/// FFI-safe interface.
///
/// # Safety
/// - `file_manifest_parent` must be a valid, non-null pointer.
/// - `out_ptr` must be a valid pointer to a
///   `*mut FFISeekInfoArray...`.
/// - On success, the pointer written to `out_ptr` is owned by the C
///   caller and MUST be freed by passing it to the
///   `free_seek_chunks_u64` function.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn get_seek_chunks_u64(
    file_manifest_parent: *const FFIFileManifestParentU64,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArrayU64,
) -> FFIResult {
    if file_manifest_parent.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    };

    let fmp = unsafe{
        /*Dereference the main struct pointer and convert using from.*/
        FileManifestParent::<u64>::from(&*file_manifest_parent)
    };

    match get_seek_chunks::<u64>(
        &fmp,
        seek_offset,
        seek_length
    ){
        Ok(seek_info_vec) => {
            let mut ffi_chunks: Vec<FFISeekChunkInfo<u64>> =
                seek_info_vec
                    .into_iter()
                    .map(|metadata| {
                        FFISeekChunkInfo::from(metadata)
                    })
                    .collect();

            let chunks_ptr = ffi_chunks.as_mut_ptr();
            let chunks_len = ffi_chunks.len();
            let chunks_cap = ffi_chunks.capacity();
            // Give up ownership so memory isn't freed here
            std::mem::forget(ffi_chunks);

            /*Create the final struct that holds the array pointer
            and length*/
            let output = Box::new({
                FFISeekInfoArray::from((
                    chunks_ptr,
                    chunks_len,
                    chunks_cap,
                ))
            });

            unsafe {
                *out_ptr = Box::into_raw(output);
            }

            FFIResult::StatusOk
        }
        Err(e) => e.into()
    }
}

/// Calculates which chunks are needed for a seek operation via an
/// FFI-safe interface.
///
/// # Safety
/// - `file_manifest_parent` must be a valid, non-null pointer.
/// - `out_ptr` must be a valid pointer to a
///   `*mut FFISeekInfoArray...`.
/// - On success, the pointer written to `out_ptr` is owned by the C
///   caller and MUST be freed by passing it to the
///   `free_seek_chunks_u128` function.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn get_seek_chunks_u128(
    file_manifest_parent: *const FFIFileManifestParentU128,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArrayU128,
) -> FFIResult {
    if file_manifest_parent.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    };

    let fmp: FileManifestParent::<u128> = unsafe{
        /*Dereference the main struct pointer and convert using from.*/
        FileManifestParent::<u128>::from(&*file_manifest_parent)
    };

    match get_seek_chunks::<u128>(
        &fmp,
        seek_offset,
        seek_length
    ){
        Ok(seek_info_vec) => {
            let mut ffi_chunks: Vec<FFISeekChunkInfo<[u8; 16]>> =
                seek_info_vec
                    .into_iter()
                    .map(|metadata| {
                        FFISeekChunkInfo {
                            hash: metadata.hash.to_le_bytes(),
                            read_start: metadata.start_offset,
                            read_end: metadata.end_offset,
                        }
                    })
                    .collect();

            let chunks_ptr = ffi_chunks.as_mut_ptr();
            let chunks_len = ffi_chunks.len();
            let chunks_cap = ffi_chunks.capacity();
            std::mem::forget(ffi_chunks); // Give up ownership

            let output = Box::new({
                FFISeekInfoArray::from((
                    chunks_ptr,
                    chunks_len,
                    chunks_cap,
                ))
            });

            unsafe {
                *out_ptr = Box::into_raw(output);
            }

            FFIResult::StatusOk
        }
        Err(e) => e.into()
    }
}

/// A generic helper to deallocate an `FFISeekInfoArray` from an FFI handle.
///
/// This internal function provides the core logic for safely freeing the
/// memory that was allocated by `get_seek_chunks_internal` and passed to a
/// C caller. It takes the raw pointer, reconstructs the `Box` for the outer
/// struct, and then reconstructs the `Vec` for the inner array of
/// `FFISeekChunkInfo` entries. This allows Rust's memory manager to properly
/// take back ownership and deallocate all associated memory.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`), ensuring the pointer is
///   cast and deallocated correctly.
///
/// # Arguments
///
/// * `ptr`: The raw pointer to the `FFISeekInfoArray` struct to be freed.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - The `ptr` is a valid, non-null pointer that was originally returned from
///   a successful call to a `get_seek_chunks_*` function.
/// - The pointer has not already been freed.
/// - The pointer will not be used again after this function is called.
///
/// Passing a null, invalid, or double-freed pointer will result in undefined
/// behavior.
fn free_seek_chunks_internal<H>(
    ptr: *mut FFISeekInfoArray<H>
) {
    unsafe{
            /*Retake ownership of the main struct to deallocate it at the
            end.*/
            let output_box = Box::from_raw(ptr);

            let _ = Vec::from_raw_parts(
                output_box.chunks,
                output_box.chunks_len,
                output_box.chunks_cap,
            );
        }
}

/// Frees the memory allocated by the `get_seek_chunks_u64` function.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to the `get_seek_chunks_u64` function.
/// Calling this with a null pointer or a pointer that has already
/// been freed will lead to undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_seek_chunks_u64(
    ptr: *mut FFISeekInfoArrayU64
) {
    if ptr.is_null() {
        return;
    };

    free_seek_chunks_internal::<u64>(ptr);
}

/// Frees the memory allocated by a `get_seek_chunks_u128` function.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to the `get_seek_chunks_u128` function.
/// Calling this with a null pointer or a pointer that has already
/// been freed will lead to undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_seek_chunks_u128(
    ptr: *mut FFISeekInfoArrayU128
) {
    if ptr.is_null() {
        return;
    };

    free_seek_chunks_internal::<[u8; 16]>(ptr);
}


/// Looks up the location of a data chunk in the chunk index using a u64 hash.
///
/// This function uses an opaque handle to the `HashMap` (created by
/// `prepare_chunk_index_u64`) to find the `FFIChunkLocation` corresponding
/// to the provided hash.
///
/// On success (`FFIResult::StatusOk`), the `chunk_location` out-parameter is
/// populated with the result. If the hash is not found, the function returns
/// `FFIResult::HashNotFound`.
///
/// # Arguments
///
/// * `map_ptr`: An opaque handle to the chunk index `HashMap`, originally
///   obtained from `prepare_chunk_index_u64`.
/// * `key_hash`: The `u64` hash of the chunk to look up.
/// * `chunk_location`: An out-parameter; a pointer to an `FFIChunkLocation`
///   struct where the result will be written.
///
/// # Safety
///
/// - `map_ptr` must be a valid, non-null handle that has not yet been freed.
/// - `chunk_location` must be a valid, non-null pointer to a writable memory
///   location capable of holding an `FFIChunkLocation`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn lookup_chunk_location_u64(
    map_ptr: *const c_void,
    key_hash: u64,
    chunk_location: *mut FFIChunkLocation
) -> FFIResult {
    if map_ptr.is_null() ||chunk_location.is_null() {
        return FFIResult::NullArgument
    }

    let hash_map_ref = unsafe {&*(map_ptr as *const ChunkIndexMap<u64>)};

    match hash_map_ref.get(&key_hash){
        Some(chunk_loc) => {
            unsafe {
                *chunk_location = FFIChunkLocation::from(*chunk_loc);
            }

            FFIResult::StatusOk
        }
        None => {
            FFIResult::HashNotFound
        }
    }
}


/// Looks up the location of a data chunk in the chunk index using a u128 hash.
///
/// This function uses an opaque handle to the `HashMap` (created by
/// `prepare_chunk_index_u128`) to find the `FFIChunkLocation` corresponding
/// to the provided hash.
///
/// On success (`FFIResult::StatusOk`), the `chunk_location` out-parameter is
/// populated with the result. If the hash is not found, the function returns
/// `FFIResult::HashNotFound`.
///
/// # Arguments
///
/// * `map_ptr`: An opaque handle to the chunk index `HashMap`, originally
///   obtained from `prepare_chunk_index_u128`.
/// * `key_hash`: A 16-byte array representing the `u128` hash of the chunk
///   to look up.
/// * `chunk_location`: An out-parameter; a pointer to an `FFIChunkLocation`
///   struct where the result will be written.
///
/// # Safety
///
/// - `map_ptr` must be a valid, non-null handle that has not yet been freed.
/// - `chunk_location` must be a valid, non-null pointer to a writable memory
///   location capable of holding an `FFIChunkLocation`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn lookup_chunk_location_u128(
    map_ptr: *const c_void,
    key_hash_ptr: *const u8,
    chunk_location: *mut FFIChunkLocation
) -> FFIResult {
    if map_ptr.is_null() ||chunk_location.is_null() {
        return FFIResult::NullArgument
    }

    let hash_map_ref = unsafe {&*(map_ptr as *const ChunkIndexMap<u128>)};

    let key_hash_array = unsafe {*(key_hash_ptr as *const [u8; 16])};
    let hash_u128 = u128::from_le_bytes(key_hash_array);

    match hash_map_ref.get(&hash_u128){
        Some(chunk_loc) => {
            unsafe {
                *chunk_location = FFIChunkLocation::from(*chunk_loc);
            }

            FFIResult::StatusOk
        }
        None => {
            FFIResult::HashNotFound
        }
    }
}
