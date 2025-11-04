//! FFI-safe processing functions for sprite-shrink archives.
//!
//! This module exposes the core data processing logic (chunking, hashing,
//! verification) to C callers with robust error handling and memory
//! management.

use std::{
    ffi::CString,
    fmt::Debug,
    hash::Hash,
    os::raw::c_void,
    slice
};

use fastcdc::v2020::Chunk;
use libc::c_char;

use crate::ffi::ffi_error_handling::{
    FFICallbackStatus, FFIResult
};
use crate::ffi::ffi_types::{
    FFIChunk, FFIChunkDataArray,
    FFIFileManifestChunks,
    FFIFileManifestChunksU64, FFIFileManifestChunksU128,
    FFIFileData, FFIFileManifestParent,
    FFIFileManifestParentU64, FFIFileManifestParentU128,
    FFIHashedChunkData, FFIProcessedFileData,
    FFISeekChunkInfo, FFISeekInfoArray, FFISSAChunkMeta,
};
use crate::lib_error_handling::{
    IsCancelled, SpriteShrinkError
};
use crate::processing::{
    create_file_manifest_and_chunks, get_seek_chunks, process_file_in_memory,
    verify_single_file, test_compression, Hashable};
use crate::{FileData, FileManifestParent};

/// A generic helper to create a file manifest and hashed chunks from C data.
///
/// This internal function serves as the core implementation for the public
/// `create_file_manifest_and_chunks_ffi_*` functions. It orchestrates the
/// conversion of C-style data (raw pointers and lengths) into Rust-native
/// types, invokes the primary `create_file_manifest_and_chunks` function to
/// perform the main processing logic, and then converts the resulting Rust
/// structs back into a complex, FFI-safe structure.
///
/// The final FFI-safe struct, which contains multiple heap-allocated,
/// C-compatible data structures, is itself allocated on the heap, and a
/// pointer to it is returned via the `out_ptr` parameter, transferring
/// ownership to the caller.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) that must implement the
///   `Hashable` trait.
///
/// # Arguments
///
/// * `file_name_ptr`: A pointer to a null-terminated C string for the
///   filename.
/// * `file_data_array_ptr`: A pointer to the byte array of the file's content.
/// * `file_data_len`: The length of the `file_data_array_ptr`.
/// * `chunks_array_ptr`: A pointer to an array of `FFIChunk` structs.
/// * `chunks_len`: The number of elements in the `chunks_array_ptr`.
/// * `out_ptr`: A pointer to a location where the pointer to the newly created
///   `FFIFileManifestChunks` struct will be written.
///
/// # Returns
///
/// * `FFIStatus::Ok` on success, with `out_ptr` pointing to the output struct.
/// * `FFIStatus::InvalidString` if the `file_name_ptr` cannot be converted
///   into a valid `CString`.
///
/// # Safety
///
/// The public FFI function that calls this helper is responsible for ensuring:
/// - All input pointers (`file_name_ptr`, `file_data_array_ptr`,
///   `chunks_array_ptr`, `out_ptr`) are valid and non-null.
/// - The lengths (`file_data_len`, `chunks_len`) accurately correspond to
///   their respective arrays.
/// - The caller takes full ownership of the memory allocated for the
///   `FFIFileManifestChunks` struct and all the nested pointers it contains
///   (filenames, chunk metadata, hashed chunk data). This memory **must** be
///   deallocated by passing the pointer to the corresponding
///   `free_file_manifest_and_chunks_ffi_*` function to prevent significant
///   memory leaks.
fn create_file_manifest_and_chunks_internal<H>(
    file_name_ptr: *const c_char,
    file_data_array_ptr: *const u8,
    file_data_len: usize,
    chunks_array_ptr: *const FFIChunk,
    chunks_len: usize,
    out_ptr: *mut *mut FFIFileManifestChunks<H>
) -> FFIResult
where
    H: Hashable,
{
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
                    .collect::<
                        Vec<_>
                    >()
            };

            (file_name, file_data, chunks)
        };

        /*Call original function to process data.*/
        let (fmp, hashed_chunks) = create_file_manifest_and_chunks(
            &file_name, file_data,
            &chunks
        );

        let mut ffi_chunk_metadata: Vec<FFISSAChunkMeta<H>> = fmp.chunk_metadata
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

        let ffi_fmp = FFIFileManifestParent::<H> {
            filename: c_filename,
            chunk_metadata: fmp_meta_ptr,
            chunk_metadata_len: fmp_meta_len,
            chunk_metadata_cap: fmp_meta_cap,
        };

        //Convert vector of hashed chunks
        let mut ffi_hashed_chunks: Vec<FFIHashedChunkData<H>> = hashed_chunks
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
        FFIResult::Ok
}

/// Creates a file manifest and a list of u64 hashed data chunks via an
/// FFI-safe interface.
///
/// # Safety
/// The caller MUST ensure `ptr` is a valid pointer previously returned
/// from `create_file_manifest_and_chunks_ffi_u64`.
///
/// Passing a null pointer, a pointer that has already been freed, or a
/// pointer from any other source will result in
/// **undefined behavior**.
/// This function must only be called once per pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn create_file_manifest_and_chunks_ffi_u64(
    file_name_ptr: *const c_char,
    file_data_array_ptr: *const u8,
    file_data_len: usize,
    chunks_array_ptr: *const FFIChunk,
    chunks_len: usize,
    out_ptr: *mut *mut FFIFileManifestChunksU64
) -> FFIResult {
    if file_name_ptr.is_null() ||
        file_data_array_ptr.is_null() ||
        chunks_array_ptr.is_null() ||
        out_ptr.is_null() {
            return FFIResult::NullArgument;
    };

    create_file_manifest_and_chunks_internal::<u64>(
        file_name_ptr,
        file_data_array_ptr,
        file_data_len,
        chunks_array_ptr,
        chunks_len,
        out_ptr
    )
}

/// Creates a file manifest and a list of u128 hashed data chunks via an
/// FFI-safe interface.
///
/// # Safety
/// The caller MUST ensure `ptr` is a valid pointer previously returned
/// from `create_file_manifest_and_chunks_ffi_u128`.
///
/// Passing a null pointer, a pointer that has already been freed, or a
/// pointer from any other source will result in
/// **undefined behavior**.
/// This function must only be called once per pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn create_file_manifest_and_chunks_ffi_u128(
    file_name_ptr: *const c_char,
    file_data_array_ptr: *const u8,
    file_data_len: usize,
    chunks_array_ptr: *const FFIChunk,
    chunks_len: usize,
    out_ptr: *mut *mut FFIFileManifestChunksU128
) -> FFIResult {
    if file_name_ptr.is_null() ||
        file_data_array_ptr.is_null() ||
        chunks_array_ptr.is_null() ||
        out_ptr.is_null() {
            return FFIResult::NullArgument;
    };

    create_file_manifest_and_chunks_internal::<u128>(
        file_name_ptr,
        file_data_array_ptr,
        file_data_len,
        chunks_array_ptr,
        chunks_len,
        out_ptr
    )
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
///   successful call to a `create_file_manifest_and_chunks_ffi_*` function.
/// - The pointer has not already been freed.
/// - The pointer, and any pointers contained within its structures, will not
///   be used again after this function is called.
///
/// Failure to uphold these conditions will result in undefined behavior, such
/// as double-freeing memory or use-after-free vulnerabilities.
fn free_file_manifest_and_chunks_ffi_internal<H>(
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

/// Frees the memory allocated by `create_file_manifest_and_chunks_ffi_u64`
/// for a u64 hash.
///
/// # Safety
/// The caller MUST ensure that `ptr` is a valid pointer returned from
/// `create_file_manifest_and_chunks_ffi_u64` and that it is not used
/// after this call. This function should only be called once for any
/// given pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_manifest_and_chunks_ffi_u64(
    ptr: *mut FFIFileManifestChunksU64
) {
    if ptr.is_null() {
        return;
    }

    free_file_manifest_and_chunks_ffi_internal::<u64>(
        ptr
    );
}

/// Frees the memory allocated by `create_file_manifest_and_chunks_ffi_u128`
/// for a u128 hash.
///
/// # Safety
/// The caller MUST ensure that `ptr` is a valid pointer returned from
/// `create_file_manifest_and_chunks_ffi_u128` and that it is not used
/// after this call. This function should only be called once for any
/// given pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_manifest_and_chunks_ffi_u128(
    ptr: *mut FFIFileManifestChunksU128
) {
    if ptr.is_null() {
        return;
    }

    free_file_manifest_and_chunks_ffi_internal::<u128>(
        ptr
    );
}

/// Processes a single file from memory via an FFI-safe interface.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
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

    FFIResult::Ok
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

/// A generic helper to verify a file's integrity using C callbacks for data
/// retrieval.
///
/// This internal function serves as the core implementation for the public
/// `verify_single_file_ffi_*` functions. It bridges the FFI boundary by
/// converting C-style inputs into Rust-native types and wrapping C function
/// pointers into Rust closures.
///
/// It orchestrates the file verification process by:
/// 1.  Reconstructing the Rust `FileManifestParent` and verification hash from
///     raw pointers.
/// 2.  Creating closures around the `get_chunks_cb` and `progress_cb` C
///     functions, capturing the `user_data` pointer to maintain state across
///     the FFI boundary.
/// 3.  Invoking the primary `verify_single_file` function with the prepared
///     data and closures.
/// 4.  Translating the `Result` from the core logic into an FFI-safe
///     `FFIResult` code.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`), which must be hashable,
///   equatable, copyable, and thread-safe.
///
/// # Arguments
///
/// * `file_manifest_parent`: A pointer to the `FFIFileManifestParent` struct
///   for the file to be verified.
/// * `veri_hash_array_ptr`: A pointer to a 64-byte array containing the
///   original SHA-512 verification hash of the file.
/// * `user_data`: An opaque `void` pointer provided by the C caller, which is
///   passed back to the C callbacks.
/// * `get_chunks_cb`: A C function pointer that the verification logic will
///   call to request the data for a set of chunk hashes.
/// * `progress_cb`: A C function pointer that is called periodically to report
///   the number of bytes processed.
///
/// # Returns
///
/// * `FFIResult::Ok` if the file is successfully reconstructed and its
///   computed hash matches the expected hash.
/// * `FFIResult::VerificationHashMismatch` if the computed hash does not
///   match.
/// * `FFIResult::VerificationMissingChunk` if a required chunk could not be
///   retrieved.
/// * `FFIResult::InternalError` for any other unexpected errors during the
///   process.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - All pointers (`file_manifest_parent`, `veri_hash_array_ptr`, `user_data`)
///   are valid, non-null, and point to memory that is readable for the
///   duration of this call.
/// - The C callback function pointers (`get_chunks_cb`, `progress_cb`) are
///   valid and point to functions with the correct signatures.
fn verify_single_file_ffi_internal<E, H>(
    file_manifest_parent: *const FFIFileManifestParent<H>,
    veri_hash_array_ptr: *const u8,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const H,
        hashes_len: usize
    ) -> FFIChunkDataArray,
    progress_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        bytes_processed: u64
    ),
) -> FFIResult
where
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    H: Eq + std::cmp::Eq+ std::hash::Hash + Copy + Clone + Send + Sync + 'static,
{
    let(fmp, veri_hash) = unsafe {
        /*Dereference the main struct pointer and convert using from.*/
        let fmp = FileManifestParent::<H>::from(&*file_manifest_parent);

        /*Prepare and reconstruct veri_hash, which is the fourth
        parameter of the original verify_single_file
        function, from C input.*/
        let veri_hash: [u8; 64] = *(veri_hash_array_ptr as *const [u8; 64]);

        (fmp, veri_hash)
    };

    let user_data_addr = user_data as usize;

    let get_chunks_closure = move |hashes: &[H]| -> Result<Vec<Vec<u8>>, E> {
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
        Ok(()) => FFIResult::Ok,
        Err(e) => e.into(),
    }
}

/// Rebuilds a single file from chunks and verifies its integrity via
/// an FFI-safe interface for u64 hashes.
///
/// # Safety
/// All pointer arguments must be non-null and valid for their
/// specified lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verify_single_file_ffi_u64(
    file_manifest_parent: *const FFIFileManifestParentU64,
    veri_hash_array_ptr: *const u8,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const u64,
        hashes_len: usize
    ) -> FFIChunkDataArray,
    progress_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        bytes_processed: u64
    ),
) -> FFIResult {
     //Immediately check for null pointers to fail early.
    if file_manifest_parent.is_null()
        || veri_hash_array_ptr.is_null()
    {
        return FFIResult::NullArgument;
    }

    verify_single_file_ffi_internal::<SpriteShrinkError, u64>(
        file_manifest_parent,
        veri_hash_array_ptr,
        user_data,
        get_chunks_cb,
        progress_cb,
    )
}

/// Rebuilds a single file from chunks and verifies its integrity via
/// an FFI-safe interface for u128 hashes.
///
/// # Safety
/// All pointer arguments must be non-null and valid for their
/// specified lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verify_single_file_ffi_u128(
    file_manifest_parent: *const FFIFileManifestParentU128,
    veri_hash_array_ptr: *const u8,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const u128,
        hashes_len: usize
    ) -> FFIChunkDataArray,
    progress_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        bytes_processed: u64
    ),
) -> FFIResult {
     //Immediately check for null pointers to fail early.
    if file_manifest_parent.is_null()
        || veri_hash_array_ptr.is_null()
    {
        return FFIResult::NullArgument;
    }

    verify_single_file_ffi_internal::<SpriteShrinkError, u128>(
        file_manifest_parent,
        veri_hash_array_ptr,
        user_data,
        get_chunks_cb,
        progress_cb,
    )
}

/// A generic helper to estimate the compressed size of a data store via FFI.
///
/// This internal function serves as the core implementation for the public
/// `test_compression_ffi_*` functions. It provides a way to run a simulated
/// compression cycle on a set of unique data chunks to estimate the final
/// archive size without writing any data to disk.
///
/// It works by:
/// 1.  Converting C-style inputs into Rust-native types.
/// 2.  Wrapping the `get_chunks_cb` C function pointer in a Rust closure.
/// 3.  Calling the primary `test_compression` function to build a temporary
///     dictionary and calculate the total compressed size.
/// 4.  Translating the result or error into an FFI-safe format.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`), which must be
///   debuggable, equatable, hashable, and thread-safe.
///
/// # Arguments
///
/// * `total_data_size`: The total combined size in bytes of all unique,
///   uncompressed chunks.
/// * `sorted_hashes_array_ptr`: A pointer to a sorted array of all unique
///   chunk hashes.
/// * `sorted_hashes_len`: The number of elements in the
///   `sorted_hashes_array_ptr`.
/// * `worker_count`: The number of threads to use for the parallel compression
///   test.
/// * `dictionary_size`: The target size in bytes for the temporary dictionary.
/// * `compression_level`: The Zstandard compression level to be used by the
///   worker threads. When testing a maximum value of 7 is used.
/// * `out_size`: A pointer to a `u64` where the estimated compressed size in
///   bytes will be written on success.
/// * `user_data`: An opaque `void` pointer that will be passed back to the
///   `get_chunks_cb` callback.
/// * `get_chunks_cb`: A C function pointer that the function will call to
///   request the raw data for a given set of hashes.
///
/// # Returns
///
/// * `FFIResult::Ok` on success, with `out_size` populated with the estimated
///   size.
/// * `FFIResult::DictionaryError` if creating the temporary dictionary fails.
/// * `FFIResult::InternalError` for any other unexpected errors.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - All pointers (`sorted_hashes_array_ptr`, `out_size`, `user_data`) are
///   valid and non-null for the duration of this call.
/// - The `sorted_hashes_len` accurately reflects the number of elements in the
///   array.
/// - The `get_chunks_cb` function pointer is valid and points to a function
///   with the correct signature.
fn test_compression_ffi_internal<H>(
    total_data_size: u64,
    sorted_hashes_array_ptr: *const H,
    sorted_hashes_len: usize,
    worker_count: usize,
    dictionary_size: usize,
    compression_level: i32,
    out_size: *mut u64,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const H,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
) -> FFIResult
where
    H: Copy + Debug + Eq + Hash + Send + Sync + 'static,
{
    let sorted_hashes = unsafe {
        //Convert C inputs to Rust types

        /*Prepare data_store, which is the second parameter of the
        original test_compression function, from C input.*/
        slice::from_raw_parts(
            sorted_hashes_array_ptr,
            sorted_hashes_len
        )
    };

    let user_data_addr = user_data as usize;

    let get_chunks_closure = move |hashes: &[H]| -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let mut ffi_chunks_array = FFIChunkDataArray { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
        let status = unsafe {
            get_chunks_cb(
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

    //Run test compression with received and converted data.
    match test_compression(
        sorted_hashes,
        total_data_size,
        worker_count,
        dictionary_size,
        compression_level,
        get_chunks_closure
    ) {
        Ok(compressed_size) => {
            unsafe {
                *out_size = compressed_size as u64;
            }

            FFIResult::Ok
        }
        Err(e) => e.into()
    }
}


/// Estimates the compressed size of a data store when using
/// u64 hashes.
///
/// On success, returns `FFIResult::Ok` and populates `out_size`.
///
/// # Safety
/// - All input pointers must be non-null and valid for their specified
///   lengths.
/// - `out_size` must be a valid, non-null pointer to a `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_compression_ffi_u64(
    total_data_size: u64,
    sorted_hashes_array_ptr: *const u64,
    sorted_hashes_len: usize,
    worker_count: usize,
    dictionary_size: usize,
    compression_level: i32,
    out_size: *mut u64,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const u64,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
) -> FFIResult {
    if sorted_hashes_array_ptr.is_null() ||
        out_size.is_null() {
            return FFIResult::NullArgument;
    };

    test_compression_ffi_internal::<u64>(
        total_data_size,
        sorted_hashes_array_ptr,
        sorted_hashes_len,
        worker_count,
        dictionary_size,
        compression_level,
        out_size,
        user_data,
        get_chunks_cb
    )
}

/// Estimates the compressed size of a data store when using
/// u128 hashes.
///
/// On success, returns `FFIResult::Ok` and populates `out_size`.
///
/// # Safety
/// - All input pointers must be non-null and valid for their specified
///   lengths.
/// - `out_size` must be a valid, non-null pointer to a `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_compression_ffi_u128(
    total_data_size: u64,
    sorted_hashes_array_ptr: *const u128,
    sorted_hashes_len: usize,
    worker_count: usize,
    dictionary_size: usize,
    compression_level: i32,
    out_size: *mut u64,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const u128,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
) -> FFIResult {
    if sorted_hashes_array_ptr.is_null() ||
        out_size.is_null() {
            return FFIResult::NullArgument;
    };

    test_compression_ffi_internal::<u128>(
        total_data_size,
        sorted_hashes_array_ptr,
        sorted_hashes_len,
        worker_count,
        dictionary_size,
        compression_level,
        out_size,
        user_data,
        get_chunks_cb
    )
}

/// A generic helper to calculate the chunks required for a file seek
/// operation.
///
/// This internal function serves as the core implementation for the public
/// `get_seek_chunks_ffi_*` functions. It takes an FFI-safe file manifest and
/// seek parameters (offset and length), converts the manifest into a
/// Rust-native type, and then calls the primary `get_seek_chunks` function to
/// determine which chunks are needed to fulfill the read request.
///
/// The result is a heap-allocated, FFI-safe array of `FFISeekChunkInfo`
/// structs, with ownership transferred to the caller via the `out_ptr`
/// parameter. Each struct in the array identifies a required chunk and the
/// specific byte range to read from it after decompression.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`), which must be hashable,
///   equatable, and copyable.
///
/// # Arguments
///
/// * `file_manifest_parent`: A pointer to the `FFIFileManifestParent` struct
///   for the file being accessed.
/// * `seek_offset`: The starting byte offset of the desired data within the
///   original, uncompressed file.
/// * `seek_length`: The number of bytes to read starting from the
///   `seek_offset`.
/// * `out_ptr`: A pointer to a location where the pointer to the newly created
///   `FFISeekInfoArray` will be written.
///
/// # Returns
///
/// * `FFIResult::Ok` on success, with `out_ptr` pointing to the output array.
/// * `FFIResult::SeekOutOfBounds` if the requested seek range extends beyond
///   the original file's boundaries.
/// * `FFIResult::InternalError` for any other unexpected errors.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - The `file_manifest_parent` and `out_ptr` pointers are valid and non-null.
/// - The caller takes full ownership of the memory allocated for the
///   `FFISeekInfoArray` struct and its internal `chunks` array. This memory
///   **must** be deallocated by passing the pointer to the corresponding
///   `free_seek_chunks_ffi_*` function to prevent a memory leak.
fn get_seek_chunks_ffi_internal<H>(
    file_manifest_parent: *const FFIFileManifestParent<H>,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArray<H>,
) -> FFIResult
where
    H: Copy + Eq + Hash + Hashable,
{
    let fmp = unsafe{
        /*Dereference the main struct pointer and convert using from.*/
        FileManifestParent::<H>::from(&*file_manifest_parent)
    };

    match get_seek_chunks::<H>(
        &fmp,
        seek_offset,
        seek_length
    ){
        Ok(seek_info_vec) => {
            let mut ffi_chunks: Vec<FFISeekChunkInfo<H>> =
                seek_info_vec
                    .into_iter()
                    .map(|(hash, (start, end))| {
                            FFISeekChunkInfo::from((
                            hash,
                            (start,
                            end)
                        ))
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

            FFIResult::Ok
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
///   `free_seek_chunks_ffi_u64` function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_seek_chunks_ffi_u64(
    file_manifest_parent: *const FFIFileManifestParent<u64>,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArray<u64>,
) -> FFIResult {
    if file_manifest_parent.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    };

    get_seek_chunks_ffi_internal::<u64>(
        file_manifest_parent,
        seek_offset,
        seek_length,
        out_ptr
    )
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
///   `free_seek_chunks_ffi_u128` function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_seek_chunks_ffi_u128(
    file_manifest_parent: *const FFIFileManifestParent<u128>,
    seek_offset: u64,
    seek_length: u64,
    out_ptr: *mut *mut FFISeekInfoArray<u128>,
) -> FFIResult {
    if file_manifest_parent.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    };

    get_seek_chunks_ffi_internal::<u128>(
        file_manifest_parent,
        seek_offset,
        seek_length,
        out_ptr
    )
}

/// A generic helper to deallocate an `FFISeekInfoArray` from an FFI handle.
///
/// This internal function provides the core logic for safely freeing the
/// memory that was allocated by `get_seek_chunks_ffi_internal` and passed to a
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
///   a successful call to a `get_seek_chunks_ffi_*` function.
/// - The pointer has not already been freed.
/// - The pointer will not be used again after this function is called.
///
/// Passing a null, invalid, or double-freed pointer will result in undefined
/// behavior.
fn free_seek_chunks_ffi_internal<H>(
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

/// Frees the memory allocated by a `get_seek_chunks_ffi` function.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to the `get_seek_chunks_ffi_u64` function.
/// Calling this with a null pointer or a pointer that has already
/// been freed will lead to undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_seek_chunks_ffi_u64(
    ptr: *mut FFISeekInfoArray<u64>
) {
    if ptr.is_null() {
        return;
    };

    free_seek_chunks_ffi_internal::<u64>(ptr);
}

/// Frees the memory allocated by a `get_seek_chunks_ffi` function.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to the `get_seek_chunks_ffi_u128` function.
/// Calling this with a null pointer or a pointer that has already
/// been freed will lead to undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_seek_chunks_ffi_u128(
    ptr: *mut FFISeekInfoArray<u128>
) {
    if ptr.is_null() {
        return;
    };

    free_seek_chunks_ffi_internal::<u128>(ptr);
}
