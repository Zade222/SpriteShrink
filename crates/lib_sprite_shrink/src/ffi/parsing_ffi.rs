//! FFI-safe parsing functions for sprite-shrink archives.
//!
//! This module exposes Rust's parsing logic to C callers, handling the
//! conversion of data types and memory management across the FFI boundary.

use std::ffi::{
    CString,
};
use std::{
    hash::Hash,
    slice
};

use crate::ffi::FFIResult;
use crate::ffi::ffi_types::{
    FFIChunkIndexEntry,
    FFIFileManifestParent,
    FFIParsedChunkIndexArray,
    FFIParsedChunkIndexArrayU64, FFIParsedChunkIndexArrayU128,
    FFIParsedManifestArray,
    FFIParsedManifestArrayU64, FFIParsedManifestArrayU128,
    FFISSAChunkMeta,
};
use crate::lib_structs::{
    FileHeader, FileManifestParent,
};
use crate::parsing::{
    parse_file_chunk_index, parse_file_header, parse_file_metadata,
};

/// A generic helper function to parse a chunk index and convert it to an
/// FFI-safe format.
///
/// This internal function serves as the core logic for the public
/// `parse_file_chunk_index_ffi_*` functions. It takes a raw byte slice
/// containing the serialized chunk index data, calls the primary
/// `parse_file_chunk_index` function to deserialize it into a Rust `HashMap`,
/// and then transforms the result into a C-compatible array structure.
///
/// The resulting FFI-safe array is allocated on the heap, and a pointer to it
/// is returned via the `out_ptr` parameter. Ownership of this memory is
/// transferred to the caller.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used in the chunk index.
///   It must be deserializable and support hashing and equality checks.
///
/// # Arguments
///
/// * `chunk_index_data`: A byte slice containing the bincode-serialized chunk
///   index.
/// * `out_ptr`: A raw pointer to a location where the pointer to the newly
///   allocated `FFIParsedChunkIndexArray` will be written.
///
/// # Returns
///
/// * `FFIResult::Ok` on success, with `out_ptr` pointing to the parsed data
///   structure.
/// * `FFIResult::ManifestDecodeError` if deserialization of the
///   `chunk_index_data` fails.
///
/// # Safety
///
/// The public FFI function that calls this helper is responsible for upholding
/// the following safety invariants:
/// - The `out_ptr` must be a valid, non-null pointer.
/// - The caller takes ownership of the memory allocated for the
///   `FFIParsedChunkIndexArray` and its internal `entries` array. This memory
///   **must** be deallocated by passing the pointer back to the corresponding
///   `free_parsed_chunk_index_ffi_*` function to prevent a memory leak.
fn parse_file_chunk_index_internal<H>(
    chunk_index_data: &[u8],
    out_ptr: *mut *mut FFIParsedChunkIndexArray<H>,
) -> FFIResult
where
    H: Eq + Hash,
    for<'de> H: serde::Deserialize<'de>
{
    match parse_file_chunk_index(chunk_index_data){
        Ok(index_map) => {
            let mut entries: Vec<FFIChunkIndexEntry<H>> = index_map
                .into_iter()
                .map(|(hash, location)| {
                    FFIChunkIndexEntry::<H>::from((hash, location))
                })
                .collect();

            let entries_ptr = entries.as_mut_ptr();
            let entries_len = entries.len();
            let entries_cap = entries.capacity();

            /*Give up ownership of the Vec so it doesn't get
            deallocated.*/
            std::mem::forget(entries);

            //Prepare return struct
            let result = FFIParsedChunkIndexArray::<H> {
                entries: entries_ptr,
                entries_len,
                entries_cap
            };

            unsafe {
                *out_ptr = Box::into_raw(Box::new(result));
            };

            FFIResult::Ok
        }
        Err(_) => FFIResult::ManifestDecodeError,
    }
}

/// Parses the file chunk index from a raw byte slice.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
/// On failure, returns an appropriate error code.
///
/// # Safety
/// - `chunk_index_array_ptr` must point to a valid, readable memory block of
///   `chunk_index_len` bytes.
/// - `out_ptr` must be a valid, non-null pointer to a `*mut
///   FFIParsedChunkIndexArrayU64`.
/// - On success, the pointer written to `out_ptr` is owned by the C
///   caller and MUST be freed by passing it to
///   `free_parsed_chunk_index_ffi_u64` to avoid memory leaks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_chunk_index_ffi_u64(
    chunk_index_array_ptr: *const u8,
    chunk_index_len: usize,
    out_ptr: *mut *mut FFIParsedChunkIndexArrayU64,
) -> FFIResult {
    if chunk_index_array_ptr.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let chunk_index_data = unsafe {slice::from_raw_parts(
        chunk_index_array_ptr,
        chunk_index_len)
    };

    parse_file_chunk_index_internal::<u64>(
        chunk_index_data,
        out_ptr
    )
}

/// Parses the file chunk index from a raw byte slice.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
/// On failure, returns an appropriate error code.
///
/// # Safety
/// - `chunk_index_array_ptr` must point to a valid, readable memory block of
///   `chunk_index_len` bytes.
/// - `out_ptr` must be a valid, non-null pointer to a `*mut
///   FFIParsedChunkIndexArrayU128`.
/// - On success, the pointer written to `out_ptr` is owned by the C
///   caller and MUST be freed by passing it to
///   `free_parsed_chunk_index_ffi_u128` to avoid memory leaks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_chunk_index_ffi_u128(
    chunk_index_array_ptr: *const u8,
    chunk_index_len: usize,
    out_ptr: *mut *mut FFIParsedChunkIndexArrayU128,
) -> FFIResult {
    if chunk_index_array_ptr.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let chunk_index_data = unsafe {slice::from_raw_parts(
        chunk_index_array_ptr,
        chunk_index_len)
    };

    parse_file_chunk_index_internal::<u128>(
        chunk_index_data,
        out_ptr
    )
}


fn free_parsed_chunk_index_ffi_internal<H>(
    ptr: *mut FFIParsedChunkIndexArray<H>
) {
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
            array_struct.entries_cap,
            );
    }
}

/// Frees the memory allocated by `parse_file_chunk_index_ffi_u64`.
///
/// # Safety
/// The `ptr` must be a non-null pointer from a successful call to
/// `parse_file_chunk_index_ffi_u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_chunk_index_ffi_u64(
    ptr: *mut FFIParsedChunkIndexArrayU64
) {
    if ptr.is_null() {
        return;
    }

    free_parsed_chunk_index_ffi_internal::<u64>(
        ptr
    );
}

/// Frees the memory allocated by `parse_file_chunk_index_ffi_u128`.
///
/// # Safety
/// The `ptr` must be a non-null pointer from a successful call to
/// `parse_file_chunk_index_ffi_u128`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_chunk_index_ffi_u128(
    ptr: *mut FFIParsedChunkIndexArrayU128
) {
    if ptr.is_null() {
        return;
    }

    free_parsed_chunk_index_ffi_internal::<u128>(
        ptr
    );
}

/// Parses a file header from a byte slice.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
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
) -> FFIResult {
    if header_data_array_ptr.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
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
            FFIResult::Ok
        }
        Err(e) => e.into(),
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

/// A generic helper to deallocate an `FFIParsedChunkIndexArray` from an FFI
/// handle.
///
/// This internal function provides the core logic for safely freeing the
/// memory that was allocated by `parse_file_chunk_index_internal` and passed
/// to a C caller. It takes the raw pointer, reconstructs the `Box` for the
/// outer struct, and then reconstructs the `Vec` for the inner array of
/// entries. This allows Rust's memory manager to properly take back ownership
/// and deallocate all associated memory.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) to ensure the pointer is
///   cast and deallocated correctly.
///
/// # Arguments
///
/// * `ptr`: The raw pointer to the `FFIParsedChunkIndexArray` struct to be
///   freed.
///
/// # Safety
///
/// The public FFI function that calls this helper is responsible for ensuring
/// that:
/// - The `ptr` is a valid, non-null pointer that was originally returned from
///   a successful call to a `parse_file_chunk_index_ffi_*` function.
/// - The pointer has not already been freed.
/// - The pointer will not be used again after this function is called.
///
/// Passing a null, invalid, or double-freed pointer will result in undefined
/// behavior.
fn parse_file_metadata_internal<H>(
    manifest_data: &[u8],
    out_ptr: *mut *mut FFIParsedManifestArray<H>,
) -> FFIResult
where
    H: Copy + for<'de> serde::Deserialize<'de>
{
    let file_manifest: Vec<FileManifestParent<H>> =
        match parse_file_metadata(manifest_data) {
            Ok(data) => data,
            //Return null on error
            Err(_) => return FFIResult::ManifestDecodeError,
        };

    /*Convert the Vec<FileManifestParent> to a
    Vec<FFIFileManifestParent>*/
    let ffi_manifests: Result<Vec<FFIFileManifestParent<H>>, FFIResult> =
        file_manifest
        .into_iter()
        .map(|fmp| {
            //Convert the nested Vec<SSAChunkMeta>
            let mut chunk_meta_vec: Vec<FFISSAChunkMeta<H>> =
            fmp.chunk_metadata
                .iter()
                .map(|meta| FFISSAChunkMeta::<H>::from(*meta))
                .collect();

            let chunk_meta_ptr = chunk_meta_vec.as_mut_ptr();
            let chunk_meta_len = chunk_meta_vec.len();
            let chunk_meta_cap = chunk_meta_vec.capacity();
            //Give up ownership so Rust doesn't deallocate it
            std::mem::forget(chunk_meta_vec);

            let c_filename = match CString::new(fmp.filename.clone()) {
                Ok(s) => s.into_raw(),
                Err(_) => return Err(FFIResult::InvalidString),
            };

            Ok(FFIFileManifestParent::<H>{
                filename: c_filename,
                chunk_metadata: chunk_meta_ptr as *const FFISSAChunkMeta<H>,
                chunk_metadata_len: chunk_meta_len,
                chunk_metadata_cap: chunk_meta_cap,
            })
        })
        .collect();

    let mut ffi_manifests = match ffi_manifests {
        Ok(v) => v,
        Err(status) => return status,
    };

    let manifests_ptr = ffi_manifests.as_mut_ptr();
    let manifests_len = ffi_manifests.len();
    let manifests_cap = ffi_manifests.capacity();
    std::mem::forget(ffi_manifests);

    let result = FFIParsedManifestArray::<H> {
        manifests: manifests_ptr,
        manifests_len,
        manifests_cap
    };

    //Allocate the result struct on the heap and return a raw pointer
    unsafe {
        *out_ptr = Box::into_raw(Box::new(result));
    }
    FFIResult::Ok
}

/// Parses the file manifest from a raw byte slice for a u64 hash.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
/// On failure, returns an appropriate error code.
///
/// # Safety
/// - `manifest_data_array_ptr` must point to valid memory of at least
///   `manifest_data_len` bytes.
/// - `out_ptr` must be a valid pointer to a `*mut
///   FFIParsedManifestArrayU64`.
/// - The pointer returned via `out_ptr` is owned by the caller and
///   MUST be freed by passing it to `free_parsed_manifest_ffi_u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_metadata_ffi_u64(
    manifest_data_array_ptr: *const u8,
    manifest_data_len: usize,
    out_ptr: *mut *mut FFIParsedManifestArrayU64
) -> FFIResult {
    if manifest_data_array_ptr.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let manifest_data = unsafe {
        slice::from_raw_parts(
            manifest_data_array_ptr,
            manifest_data_len
        )
    };

    parse_file_metadata_internal::<u64>(manifest_data, out_ptr)
}

/// Parses the file manifest from a raw byte slice for a u128 hash.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
/// On failure, returns an appropriate error code.
///
/// # Safety
/// - `manifest_data_array_ptr` must point to valid memory of at least
///   `manifest_data_len` bytes.
/// - `out_ptr` must be a valid pointer to a `*mut
///   FFIParsedManifestArrayU128`.
/// - The pointer returned via `out_ptr` is owned by the caller and
///   MUST be freed by passing it to `free_parsed_manifest_ffi_u128`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_metadata_ffi_u128(
    manifest_data_array_ptr: *const u8,
    manifest_data_len: usize,
    out_ptr: *mut *mut FFIParsedManifestArrayU128
) -> FFIResult {
    if manifest_data_array_ptr.is_null() ||
        out_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    let manifest_data = unsafe {
        slice::from_raw_parts(
            manifest_data_array_ptr,
            manifest_data_len
        )
    };

    parse_file_metadata_internal::<u128>(manifest_data, out_ptr)
}

/// A generic helper to deallocate an `FFIParsedManifestArray` and its
/// contents.
///
/// This internal function is responsible for the complex memory cleanup
/// required for the data structure returned by `parse_file_metadata_internal`.
/// It safely deallocates not only the top-level array of manifests but also
/// all the nested, heap-allocated data associated with each manifest,
/// including each C-string `filename` and each `chunk_metadata` array.
///
/// It works by systematically reconstructing the Rust-managed types
/// (`Box`, `Vec`, `CString`) from the raw pointers, allowing Rust's drop
/// checker to handle the deallocation correctly.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`) used in the manifest's
///   chunk metadata, ensuring pointers are cast and deallocated correctly.
///
/// # Arguments
///
/// * `ptr`: The raw pointer to the `FFIParsedManifestArray` struct to be
///   freed.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - The `ptr` is a valid, non-null pointer that was originally returned from
///   a successful call to a `parse_file_metadata_ffi_*` function.
/// - The pointer has not already been freed.
/// - The pointer, and any pointers contained within its structures, will not
///   be used again after this function is called.
///
/// Failure to uphold these conditions will result in undefined behavior, such
/// as double-freeing memory or use-after-free vulnerabilities.
fn free_parsed_manifest_ffi_internal<H>(
    ptr: *mut FFIParsedManifestArray<H>
) {
    unsafe {
        //Retake ownership of the main struct Box to deallocate it
        let manifest_array = Box::from_raw(ptr);

        //Retake ownership of the vector
        let ffi_manifests = Vec::from_raw_parts(
            manifest_array.manifests,
            manifest_array.manifests_len,
            manifest_array.manifests_cap,
        );

        /*Iterate through and deallocate the contents of each
        FFIFileManifestParent*/
        for fmp in ffi_manifests {
            //Deallocate the CString for the filename
            let _ = CString::from_raw(fmp.filename);
            //Deallocate the Vec for the chunk metadata
            let _ = Vec::from_raw_parts(
                fmp.chunk_metadata as *mut FFISSAChunkMeta<H>,
                fmp.chunk_metadata_len,
                fmp.chunk_metadata_cap,
            );
        }
    }
}

/// Frees the memory allocated by `parse_file_metadata_ffi_u64`.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to `parse_file_metadata_ffi_u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_manifest_ffi_u64(
    ptr: *mut FFIParsedManifestArrayU64
) {
    if ptr.is_null() {
        return;
    }

    free_parsed_manifest_ffi_internal::<u64>(
        ptr
    );
}

/// Frees the memory allocated by `parse_file_metadata_ffi_u128`.
///
/// # Safety
/// The `ptr` must be a non-null pointer returned from a successful
/// call to `parse_file_metadata_ffi_u128`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_manifest_ffi_u128(
    ptr: *mut FFIParsedManifestArrayU128
) {
    if ptr.is_null() {
        return;
    }

    free_parsed_manifest_ffi_internal::<u128>(
        ptr
    );
}
