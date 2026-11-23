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
use crate::ffi::ffi_ownership::copy_host_chunks;
use crate::ffi::ffi_types::{
    CheckedManifestU64, CheckedManifestU128,
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
    TestCompressionArgsU64, TestCompressionArgsU128,
    U128Bytes,
    VerifySingleFileArgsU64, VerifySingleFileArgsU128,
};
use crate::lib_error_handling::{
    SpriteShrinkError
};
use crate::parsing::CheckedManifest;
use crate::processing::{
    create_file_manifest_and_chunks, process_file_in_memory,
    verify_single_file, test_compression};
use crate::lib_structs::{
    FileData, FileManifestParent
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

    let (file_data, chunks) = unsafe {
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

        (file_data, chunks)
    };

    /*Call original function to process data.*/
    let (fmp, hashed_chunks) = create_file_manifest_and_chunks(
        file_data,
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

    let ffi_fmp = FFIFileManifestParent::<u64> {
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

    let (file_data, chunks) = unsafe {
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

        (file_data, chunks)
    };

    let (fmp, hashed_chunks) = create_file_manifest_and_chunks::<u128>(
        file_data,
        &chunks
    );

    let mut ffi_chunk_metadata: Vec<FFISSAChunkMeta<U128Bytes>> = fmp
        .chunk_metadata
        .iter()
        .map(|meta| FFISSAChunkMeta {
            hash: meta.hash.into(),
            offset: meta.offset,
            length: meta.length,
        })
        .collect();

    let fmp_meta_ptr = ffi_chunk_metadata.as_mut_ptr();
    let fmp_meta_len = ffi_chunk_metadata.len();
    let fmp_meta_cap = ffi_chunk_metadata.capacity();
    std::mem::forget(ffi_chunk_metadata);

    let ffi_fmp = FFIFileManifestParent::<U128Bytes> {
        chunk_metadata: fmp_meta_ptr,
        chunk_metadata_len: fmp_meta_len,
        chunk_metadata_cap: fmp_meta_cap,
    };

    let mut ffi_hashed_chunks: Vec<FFIHashedChunkData<U128Bytes>> = hashed_chunks
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
                hash.into(),
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

    free_file_manifest_and_chunks_internal::<U128Bytes>(
        ptr
    );
}

/// Processes a single file from memory via an FFI-safe interface.
///
/// On success, returns `FFIResult::StatusOk` and populates `out_ptr`.
/// Returns `FFIResult::NoDataToProcess` for an empty file, and
/// `FFIResult::InvalidWindowSize` if `window_size` is outside 256 to
/// 4194304 bytes.
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
        Ok(Some(data)) => data,
        Ok(None) => return FFIResult::NoDataToProcess,
        Err(e) => return e.into(),
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
/// * The callbacks must not unwind across the FFI boundary. `get_chunks_cb`
///   must fill `out_chunks` and `free_chunks_cb` must free it, as
///   `VerifySingleFileArgsU64` describes.
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

    if args_int.filename.is_null() ||
        args_int.file_manifest_parent.is_null() ||
        args_int.veri_hash_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    // SAFETY: `file_manifest_parent` was checked for null above, and the
    // caller guarantees it points to a valid manifest.
    let ffi_fmp = unsafe { &*args_int.file_manifest_parent };
    let fmp = match FileManifestParent::<u64>::try_from(ffi_fmp) {
        Ok(fmp) => fmp,
        Err(status) => return status,
    };

    /*Prepare and reconstruct veri_hash, which is the fourth parameter of the
    original verify_single_file function, from C input.*/
    // SAFETY: `veri_hash_array_ptr` was checked for null above, and the
    // caller guarantees it points to 64 readable bytes. `[u8; 64]` has an
    // alignment of 1, so any address is aligned for it.
    let veri_hash = unsafe {
        *args_int.veri_hash_array_ptr.cast::<[u8; 64]>()
    };

    // SAFETY: `filename` was checked for null above, and the caller
    // guarantees it's a null-terminated string.
    let filename = unsafe { std::ffi::CStr::from_ptr(args_int.filename) }
        .to_string_lossy()
        .into_owned();

    let get_chunks_cb = args_int.get_chunks_cb;
    let free_chunks_cb = args_int.free_chunks_cb;
    let progress_cb = args_int.progress_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u64]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let user_data = user_data_addr as *mut c_void;
        let get = |out_chunks: *mut FFIChunkDataArray| {
            /*SAFETY: `VerifySingleFileArgsU64` requires `get_chunks_cb` and
            `user_data` to stay valid for the whole call, and this closure
            only runs during it. The hash pointer covers `hashes.len()`
            hashes until the callback returns, and `out_chunks` points to
            `copy_host_chunks`' local array, valid for writes.*/
            unsafe {
                get_chunks_cb(
                    user_data,
                    hashes.as_ptr(),
                    hashes.len(),
                    out_chunks,
                )
            }
        };
        /*SAFETY: `VerifySingleFileArgsU64` requires `get_chunks_cb` to fill
        the array with one readable buffer per hash, all valid until
        `free_chunks_cb` takes them back, and `free_chunks_cb` to accept
        that array with `user_data`.*/
        unsafe {
            copy_host_chunks(hashes.len(), get, free_chunks_cb, user_data)
        }
    };

    let progress_closure = move |bytes_processed: u64| {
        unsafe {
            progress_cb(user_data_addr as *mut c_void, bytes_processed);
        }
    };

    match verify_single_file(
        filename, &fmp, &veri_hash, get_chunks_closure, progress_closure
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
/// * The callbacks must not unwind across the FFI boundary. `get_chunks_cb`
///   must fill `out_chunks` and `free_chunks_cb` must free it, as
///   `VerifySingleFileArgsU128` describes.
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

    if args_int.filename.is_null() ||
        args_int.file_manifest_parent.is_null() ||
        args_int.veri_hash_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    // SAFETY: `file_manifest_parent` was checked for null above, and the
    // caller guarantees it points to a valid manifest.
    let ffi_fmp = unsafe { &*args_int.file_manifest_parent };
    let fmp = match FileManifestParent::<u128>::try_from(ffi_fmp) {
        Ok(fmp) => fmp,
        Err(status) => return status,
    };

    /*Prepare and reconstruct veri_hash, which is the fourth parameter of the
    original verify_single_file function, from C input.*/
    // SAFETY: `veri_hash_array_ptr` was checked for null above, and the
    // caller guarantees it points to 64 readable bytes. `[u8; 64]` has an
    // alignment of 1, so any address is aligned for it.
    let veri_hash = unsafe {
        *args_int.veri_hash_array_ptr.cast::<[u8; 64]>()
    };

    // SAFETY: `filename` was checked for null above, and the caller
    // guarantees it's a null-terminated string.
    let filename = unsafe { std::ffi::CStr::from_ptr(args_int.filename) }
        .to_string_lossy()
        .into_owned();

    let get_chunks_cb = args_int.get_chunks_cb;
    let free_chunks_cb = args_int.free_chunks_cb;
    let progress_cb = args_int.progress_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u128]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let user_data = user_data_addr as *mut c_void;
        let ffi_hash_bytes: Vec<u8> = hashes
            .iter()
            .flat_map(|h| h.to_le_bytes())
            .collect();
        let get = |out_chunks: *mut FFIChunkDataArray| {
            /*SAFETY: `VerifySingleFileArgsU128` requires `get_chunks_cb` and
            `user_data` to stay valid for the whole call, and this closure
            only runs during it. The hash pointer covers `hashes.len()`
            hashes until the callback returns, and `out_chunks` points to
            `copy_host_chunks`' local array, valid for writes.*/
            unsafe {
                get_chunks_cb(
                    user_data,
                    ffi_hash_bytes.as_ptr(),
                    hashes.len(),
                    out_chunks,
                )
            }
        };
        /*SAFETY: `VerifySingleFileArgsU128` requires `get_chunks_cb` to fill
        the array with one readable buffer per hash, all valid until
        `free_chunks_cb` takes them back, and `free_chunks_cb` to accept
        that array with `user_data`.*/
        unsafe {
            copy_host_chunks(hashes.len(), get, free_chunks_cb, user_data)
        }
    };

    let progress_closure = move |bytes_processed: u64| {
        unsafe {
            (progress_cb)(user_data_addr as *mut c_void, bytes_processed);
        }
    };

    match verify_single_file(
        filename, &fmp, &veri_hash, get_chunks_closure, progress_closure
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
    let free_chunks_cb = args_int.free_chunks_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u64]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let user_data = user_data_addr as *mut c_void;
        let get = |out_chunks: *mut FFIChunkDataArray| {
            /*SAFETY: `TestCompressionArgsU64` requires `get_chunks_cb` and
            `user_data` to stay valid for the whole call, and this closure
            only runs during it. The hash pointer covers `hashes.len()`
            hashes until the callback returns, and `out_chunks` points to
            `copy_host_chunks`' local array, valid for writes.*/
            unsafe {
                get_chunks_cb(
                    user_data,
                    hashes.as_ptr(),
                    hashes.len(),
                    out_chunks,
                )
            }
        };
        /*SAFETY: `TestCompressionArgsU64` requires `get_chunks_cb` to fill
        the array with one readable buffer per hash, all valid until
        `free_chunks_cb` takes them back, and `free_chunks_cb` to accept
        that array with `user_data`.*/
        unsafe {
            copy_host_chunks(hashes.len(), get, free_chunks_cb, user_data)
        }
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
    let free_chunks_cb = args_int.free_chunks_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = move |hashes: &[u128]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let user_data = user_data_addr as *mut c_void;
        let ffi_hash_bytes: Vec<u8> = hashes
            .iter()
            .flat_map(|h| h.to_le_bytes())
            .collect();
        let get = |out_chunks: *mut FFIChunkDataArray| {
            /*SAFETY: `TestCompressionArgsU128` requires `get_chunks_cb` and
            `user_data` to stay valid for the whole call, and this closure
            only runs during it. The hash pointer covers `hashes.len()`
            hashes until the callback returns, and `out_chunks` points to
            `copy_host_chunks`' local array, valid for writes.*/
            unsafe {
                get_chunks_cb(
                    user_data,
                    ffi_hash_bytes.as_ptr(),
                    hashes.len(),
                    out_chunks,
                )
            }
        };
        /*SAFETY: `TestCompressionArgsU128` requires `get_chunks_cb` to fill
        the array with one readable buffer per hash, all valid until
        `free_chunks_cb` takes them back, and `free_chunks_cb` to accept
        that array with `user_data`.*/
        unsafe {
            copy_host_chunks(hashes.len(), get, free_chunks_cb, user_data)
        }
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

/// Checks a host's manifest once and returns a handle to a copy of it (see
/// [`CheckedManifest`]).
///
/// # Returns
///
/// * `FFIResult::StatusOk`, with the handle written to `out_handle`.
/// * `FFIResult::NullArgument` if `manifest` or `out_handle` is null, or
///   the manifest's chunk pointer is null with a nonzero length.
/// * `FFIResult::ManifestDecodeError` if the manifest's chunks don't lie
///   end to end from offset 0, or its `chunk_count` is wrong.
///
/// # Safety
///
/// - `manifest` must point to a valid manifest whose `chunk_metadata`
///   points to `chunk_metadata_len` chunk records.
/// - `out_handle` must point to writable memory for one pointer.
/// - The handle written to `out_handle` is owned by the caller and must be
///   passed to `checked_manifest_free_u64` exactly once.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks a function SPRITE_SHRINK_MUST_USE in the C \
              header only if it has #[must_use]"
)]
#[must_use]
pub unsafe extern "C" fn checked_manifest_new_u64(
    manifest: *const FFIFileManifestParentU64,
    out_handle: *mut *mut CheckedManifestU64,
) -> FFIResult {
    // SAFETY: the caller guarantees both pointers are valid when non-null,
    // which is what `checked_manifest_new_internal` needs. It checks for
    // null itself.
    unsafe { checked_manifest_new_internal::<u64, u64, _>(manifest, out_handle) }
}

/// [`checked_manifest_new_u64`] for `u128` hashes.
///
/// # Safety
///
/// The same as `checked_manifest_new_u64`; the handle must be passed to
/// `checked_manifest_free_u128` exactly once.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks a function SPRITE_SHRINK_MUST_USE in the C \
              header only if it has #[must_use]"
)]
#[must_use]
pub unsafe extern "C" fn checked_manifest_new_u128(
    manifest: *const FFIFileManifestParentU128,
    out_handle: *mut *mut CheckedManifestU128,
) -> FFIResult {
    // SAFETY: the caller guarantees both pointers are valid when non-null,
    // which is what `checked_manifest_new_internal` needs. It checks for
    // null itself.
    unsafe {
        checked_manifest_new_internal::<u128, U128Bytes, _>(manifest, out_handle)
    }
}

/// Copies and checks the host's manifest and writes a handle to it.
///
/// # Safety
///
/// Each non-null pointer must be valid: `manifest` for reading a manifest
/// and its chunk records, `out_handle` for writing one pointer.
unsafe fn checked_manifest_new_internal<H, F, Handle>(
    manifest: *const FFIFileManifestParent<F>,
    out_handle: *mut *mut Handle,
) -> FFIResult
where
    for<'a> FileManifestParent<H>: TryFrom<&'a FFIFileManifestParent<F>, Error = FFIResult>,
{
    if manifest.is_null() || out_handle.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: `manifest` is non-null (checked above), and the caller
    // guarantees it points to a valid, initialized manifest for the whole
    // call.
    let ffi_manifest = unsafe { &*manifest };
    let manifest = match FileManifestParent::<H>::try_from(ffi_manifest) {
        Ok(manifest) => manifest,
        Err(status) => return status,
    };
    let checked = match CheckedManifest::new(manifest) {
        Ok(checked) => checked,
        Err(e) => return e.into(),
    };

    let handle = Box::into_raw(Box::new(checked)).cast::<Handle>();
    // SAFETY: `out_handle` is non-null (checked above), and the caller
    // guarantees it points to writable memory for one pointer.
    unsafe { out_handle.write(handle) };
    FFIResult::StatusOk
}

/// Works out which chunks, and which bytes of each, a read of
/// `seek_length` bytes from `seek_offset` needs (see
/// `CheckedManifest::seek_chunks`).
///
/// # Returns
///
/// * `FFIResult::StatusOk`, with the chunk list written to `out_ptr`.
/// * `FFIResult::NullArgument` if `handle` or `out_ptr` is null.
/// * `FFIResult::SeekOutOfBounds` if the range ends past the end of the
///   file.
///
/// # Safety
///
/// - `handle` must come from `checked_manifest_new_u64` and not have been
///   freed.
/// - `out_ptr` must point to writable memory for one pointer.
/// - The array written to `out_ptr` is owned by the caller and must be
///   freed with `free_seek_chunks_u64`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks a function SPRITE_SHRINK_MUST_USE in the C \
              header only if it has #[must_use]"
)]
#[must_use]
pub unsafe extern "C" fn checked_manifest_seek_chunks_u64(
    handle: *const CheckedManifestU64,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArrayU64,
) -> FFIResult {
    // SAFETY: the caller guarantees `handle` came from
    // `checked_manifest_new_u64`, which boxed a `CheckedManifest<u64>`, and
    // that `out_ptr` is valid when non-null, which is what
    // `seek_chunks_internal` needs. It checks for null itself.
    unsafe {
        seek_chunks_internal::<u64, u64, _>(handle, seek_offset, seek_length, out_ptr)
    }
}

/// [`checked_manifest_seek_chunks_u64`] for `u128` hashes.
///
/// # Safety
///
/// The same as `checked_manifest_seek_chunks_u64`, with a handle from
/// `checked_manifest_new_u128`; the array must be freed with
/// `free_seek_chunks_u128`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks a function SPRITE_SHRINK_MUST_USE in the C \
              header only if it has #[must_use]"
)]
#[must_use]
pub unsafe extern "C" fn checked_manifest_seek_chunks_u128(
    handle: *const CheckedManifestU128,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArrayU128,
) -> FFIResult {
    // SAFETY: the caller guarantees `handle` came from
    // `checked_manifest_new_u128`, which boxed a `CheckedManifest<u128>`,
    // and that `out_ptr` is valid when non-null, which is what
    // `seek_chunks_internal` needs. It checks for null itself.
    unsafe {
        seek_chunks_internal::<u128, U128Bytes, _>(
            handle, seek_offset, seek_length, out_ptr
        )
    }
}

/// Plans a seek with the `CheckedManifest<H>` behind `handle` and writes
/// the chunk list, its hashes converted to `F`, to `out_ptr`.
///
/// # Safety
///
/// A non-null `handle` must point to a live `CheckedManifest<H>` (a handle
/// from the matching `checked_manifest_new_*`), and a non-null `out_ptr`
/// to writable memory for one pointer.
unsafe fn seek_chunks_internal<H, F, Handle>(
    handle: *const Handle,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArray<F>,
) -> FFIResult
where
    H: Copy,
    F: From<H>,
{
    if handle.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: `handle` is non-null (checked above), and the caller
    // guarantees it points to a live `CheckedManifest<H>`, boxed by
    // `checked_manifest_new_internal` and not yet freed. Seeking only reads
    // it, so a shared reference is enough.
    let checked = unsafe { &*handle.cast::<CheckedManifest<H>>() };
    let seek_info = match checked.seek_chunks(seek_offset, seek_length) {
        Ok(seek_info) => seek_info,
        Err(e) => return e.into(),
    };

    let ffi_chunks: Vec<FFISeekChunkInfo<F>> = seek_info
        .into_iter()
        .map(|metadata| FFISeekChunkInfo {
            hash: metadata.hash.into(),
            read_start: metadata.start_offset,
            read_end: metadata.end_offset,
        })
        .collect();
    let mut ffi_chunks = std::mem::ManuallyDrop::new(ffi_chunks);
    let output = Box::new(FFISeekInfoArray {
        chunks: ffi_chunks.as_mut_ptr(),
        chunks_len: ffi_chunks.len(),
        chunks_cap: ffi_chunks.capacity(),
    });

    // SAFETY: `out_ptr` is non-null (checked above), and the caller
    // guarantees it points to writable memory for one pointer.
    unsafe { out_ptr.write(Box::into_raw(output)) };
    FFIResult::StatusOk
}

/// Frees a handle from `checked_manifest_new_u64`. A null handle is
/// ignored.
///
/// # Safety
///
/// A non-null `handle` must come from `checked_manifest_new_u64` and not
/// have been freed already. It must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn checked_manifest_free_u64(handle: *mut CheckedManifestU64) {
    // SAFETY: the caller guarantees a non-null `handle` came from
    // `checked_manifest_new_u64`, which boxed a `CheckedManifest<u64>`, and
    // isn't used again.
    unsafe { checked_manifest_free_internal::<u64, _>(handle) }
}

/// Frees a handle from `checked_manifest_new_u128`. A null handle is
/// ignored.
///
/// # Safety
///
/// A non-null `handle` must come from `checked_manifest_new_u128` and not
/// have been freed already. It must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn checked_manifest_free_u128(handle: *mut CheckedManifestU128) {
    // SAFETY: the caller guarantees a non-null `handle` came from
    // `checked_manifest_new_u128`, which boxed a `CheckedManifest<u128>`,
    // and isn't used again.
    unsafe { checked_manifest_free_internal::<u128, _>(handle) }
}

/// Drops the `CheckedManifest<H>` behind `handle`.
///
/// # Safety
///
/// A non-null `handle` must be a pointer `checked_manifest_new_internal`
/// made from a boxed `CheckedManifest<H>`, not yet freed and not used
/// afterwards.
unsafe fn checked_manifest_free_internal<H, Handle>(handle: *mut Handle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` is non-null (checked above), and the caller
    // guarantees it came from `Box::into_raw` of a `CheckedManifest<H>`
    // that hasn't been freed, so taking the box back is sound and frees it
    // once.
    drop(unsafe { Box::from_raw(handle.cast::<CheckedManifest<H>>()) });
}

/// A generic helper to deallocate an `FFISeekInfoArray` from an FFI handle.
///
/// This internal function provides the core logic for safely freeing the
/// memory that was allocated by `seek_chunks_internal` and passed to a
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
///   a successful call to a `checked_manifest_seek_chunks_*` function.
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

/// Frees a chunk list from `checked_manifest_seek_chunks_u64`.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to the `checked_manifest_seek_chunks_u64` function.
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

/// Frees a chunk list from `checked_manifest_seek_chunks_u128`.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to the `checked_manifest_seek_chunks_u128` function.
/// Calling this with a null pointer or a pointer that has already
/// been freed will lead to undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_seek_chunks_u128(
    ptr: *mut FFISeekInfoArrayU128
) {
    if ptr.is_null() {
        return;
    };

    free_seek_chunks_internal::<U128Bytes>(ptr);
}


/// Looks up the location of a data chunk in the chunk index using a u64 hash.
///
/// This function uses an opaque handle to the `HashMap` (the `chunk_index`
/// field of a `parse_ssmc_prefix_u64` result) to find the
/// `FFIChunkLocation` corresponding to the provided hash. Its `offset` is a
/// position in the archive file.
///
/// On success (`FFIResult::StatusOk`), the `chunk_location` out-parameter is
/// populated with the result. If the hash is not found, the function returns
/// `FFIResult::HashNotFound`.
///
/// # Arguments
///
/// * `map_ptr`: An opaque handle to the chunk index `HashMap`, from the
///   `chunk_index` field of a `parse_ssmc_prefix_u64` result.
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
/// This function uses an opaque handle to the `HashMap` (the `chunk_index`
/// field of a `parse_ssmc_prefix_u128` result) to find the
/// `FFIChunkLocation` corresponding to the provided hash. Its `offset` is a
/// position in the archive file.
///
/// On success (`FFIResult::StatusOk`), the `chunk_location` out-parameter is
/// populated with the result. If the hash is not found, the function returns
/// `FFIResult::HashNotFound`.
///
/// # Arguments
///
/// * `map_ptr`: An opaque handle to the chunk index `HashMap`, from the
///   `chunk_index` field of a `parse_ssmc_prefix_u128` result.
/// * `key_hash_ptr`: A pointer to the 16 bytes of the `u128` hash of the
///   chunk to look up, little-endian as in `U128Bytes`.
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
    if map_ptr.is_null() || key_hash_ptr.is_null() ||
        chunk_location.is_null()
    {
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
