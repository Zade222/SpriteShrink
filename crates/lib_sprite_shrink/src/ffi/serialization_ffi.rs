//! FFI-safe serialization functions for sprite-shrink archives.
//!
//! This module exposes Rust's serialization logic to C callers, handling the
//! complex conversion of data structures and memory management.

use std::{
    ffi::CString,
    os::raw::c_void,
    slice
};

use dashmap::DashMap;

use crate::ffi::ffi_error_handling::{
    FFICallbackStatus, FFIResult
};
use crate::ffi::ffi_types::{
    FFIChunkDataArray, FFIChunkIndexEntry,
    FFIFileManifestParent,
    FFIFileManifestParentU64, FFIFileManifestParentU128,
    FFIKeyArray, FFIKeyArrayU64, FFIKeyArrayU128,
    FFISerializedOutput,
    FFISerializedOutputU64, FFISerializedOutputU128,
    FFISSAChunkMeta,
};
use crate::lib_error_handling::SpriteShrinkError;
use crate::lib_structs::{
    ChunkLocation, FileManifestParent
};
use crate::serialization::{serialize_uncompressed_data};

/// A generic helper to serialize archive metadata into an FFI-safe structure.
///
/// This internal function serves as the core implementation for the public
/// `serialize_uncompressed_data_*` functions. It orchestrates a complex
/// data transformation process, preparing in-memory Rust data structures for
/// the final archive-building step by converting them into a set of
/// C-compatible, heap-allocated arrays.
///
/// The process involves:
/// 1.  Reconstructing a `DashMap` of file manifests from the C array input.
/// 2.  Wrapping the C callback functions (`get_keys_cb`, `get_chunks_cb`) into
///     Rust closures.
/// 3.  Calling the primary `serialize_uncompressed_data` function to sort the
///     manifests, generate a chunk index, and get a sorted list of all unique
///     hashes.
/// 4.  Performing a deep conversion of the results into FFI-safe structures,
///     allocating memory for C-strings, metadata arrays, and the chunk index.
/// 5.  Bundling all resulting pointers and lengths into the
///     `FFISerializedOutput` struct, which is itself boxed and returned via
///     the `out_ptr`.
///
/// # Type Parameters
///
/// * `H`: The generic hash type (e.g., `u64`, `u128`), which must be
///   displayable, sortable, copyable, and hashable.
///
/// # Arguments
///
/// * `manifest_array_ptr`: A pointer to an array of `FFIFileManifestParent`
///   structs provided by the C caller.
/// * `manifest_len`: The number of elements in the `manifest_array_ptr`.
/// * `user_data`: An opaque `void` pointer that is passed back to the C
///   callbacks.
/// * `get_keys_cb`: A C function pointer that returns a complete list of all
///   unique chunk hashes from the C-side data store.
/// * `get_chunks_cb`: A C function pointer that returns the raw data for a
///   given set of chunk hashes.
/// * `out_ptr`: A pointer to a location where the pointer to the newly created
///   `FFISerializedOutput` struct will be written.
///
/// # Returns
///
/// * `FFIResult::Ok` on success, with `out_ptr` pointing to the output struct.
/// * `FFIResult::SerializationMissingChunk` if a chunk required by the
///   manifest is not available from the `get_chunks_cb`.
/// * `FFIResult::InvalidString` if a filename in the manifest cannot be
///   converted to a C-string.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - All input pointers are valid and non-null for the duration of this call.
/// - The `manifest_len` accurately reflects the number of elements in the
///   array.
/// - The C callback function pointers are valid and point to functions with
///   the correct signatures.
/// - The caller takes full ownership of the memory allocated for the
///   `FFISerializedOutput` struct and all the nested pointers it contains.
///   This memory **must** be deallocated by passing the pointer to the
///   corresponding `free_serialized_output_*` function to prevent significant
///   memory leaks.
fn serialize_uncompressed_data_internal<H>(
    manifest_array_ptr: *const FFIFileManifestParent<H>,
    manifest_len: usize,
    user_data: *mut c_void,
    get_keys_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        out_keys: *mut FFIKeyArray<H>,
    ) -> FFICallbackStatus,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const H,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
    out_ptr: *mut *mut FFISerializedOutput<H>
) -> FFIResult
where
    H: std::fmt::Display + std::cmp::Ord + std::marker::Copy + std::hash::Hash + 'static,
{
    //Convert C inputs into Rust-native data structures.
    let file_manifest = unsafe {
        //Convert C inputs to Rust types

        /*Prepare file_manifest DashMap, which is the first parameter
        of the original serialize_uncompressed_data function, from C
        input.*/
        let ffi_manifests = slice::from_raw_parts(
            manifest_array_ptr,
            manifest_len
        );

        /*Since DashMap is not a supported type in C, the following
        reconstructs it for use by the library.*/

        /*Reconstruct the file_manifest DashMap via the following
        loop.*/

        let file_manifest: DashMap<String, FileManifestParent<H>> = ffi_manifests
        .iter()
        .map(|ffi_fmp| {
            let rust_fmp = FileManifestParent::<H>::from(ffi_fmp);
            (rust_fmp.filename.clone(), rust_fmp)
        })
        .collect();

        file_manifest
    };
    //Wrap C function pointers into Rust closures for the core library.
    let user_data_addr = user_data as usize;

    let get_keys_closure = || -> Result<Vec<H>, SpriteShrinkError> {
        let mut ffi_keys_array = FFIKeyArray {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        };

        let status = unsafe {
            get_keys_cb(
                user_data_addr as *mut c_void,
                &mut ffi_keys_array, //Pass a pointer to our struct
            )
        };
        Result::from(status)?;
        //Take ownership of the C-allocated array
        let keys = unsafe {
            Vec::from_raw_parts(
                ffi_keys_array.ptr,
                ffi_keys_array.len,
                ffi_keys_array.cap
            )
        };

        Ok(keys)
    };

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

    //Call the core Rust serialization logic.
    let (
        ser_file_manifest,
        chunk_index,
        sorted_hashes
    ) = match serialize_uncompressed_data::<_, SpriteShrinkError, H, _>(
        &file_manifest,
        &get_keys_closure,
        &get_chunks_closure
    ) {
        Ok(data) => data,
        Err(_) => return FFIResult::SerializationMissingChunk,
    };

    //Convert the Rust results back into FFI-safe, heap-allocated C structures.
    /*let mut ffi_manifests: Vec<FFIFileManifestParent<H>> =
        match ser_file_manifest
        .into_iter()
        .map(|fmp|  {
            let mut chunk_meta_vec: Vec<FFISSAChunkMeta<H>> =
                fmp.chunk_metadata
                .iter()
                .map(|meta| FFISSAChunkMeta::from(*meta))
                .collect();

            let chunk_meta_ptr = chunk_meta_vec
                .as_mut_ptr();
            let chunk_meta_cap = chunk_meta_vec.capacity();

            //Give up ownership so it doesn't deallocate it
            std::mem::forget(chunk_meta_vec);

            let c_filename = match CString::new(fmp.filename.clone()) {
                Ok(s) => s.into_raw(),
                Err(_) => return Err(FFIResult::InvalidString),
            };

            Ok(FFIFileManifestParent {
                filename: c_filename,
                chunk_metadata: chunk_meta_ptr,
                chunk_metadata_len: fmp.chunk_count as usize,
                chunk_metadata_cap: chunk_meta_cap,
            })
        })
        .collect() {
            Ok(v) => v,
            Err(status) => return status, //Propagate the error status
        };*/

    let mut ffi_manifests_vec: Vec<FFIFileManifestParent<H>> =
           Vec::with_capacity(ser_file_manifest.len());

    for fmp in ser_file_manifest {
        let mut chunk_meta_vec: Vec<FFISSAChunkMeta<H>> = fmp
            .chunk_metadata
            .iter()
            .map(|meta| FFISSAChunkMeta::from(*meta))
            .collect();

        let chunk_meta_ptr = chunk_meta_vec.as_mut_ptr();
        let chunk_meta_cap = chunk_meta_vec.capacity();
        std::mem::forget(chunk_meta_vec);

        let c_filename = match CString::new(fmp.filename.clone()) {
            Ok(s) => s.into_raw(),
            Err(_) => {
                unsafe {
                    let _ = Vec::from_raw_parts(
                        chunk_meta_ptr,
                        fmp.chunk_count as usize,
                        chunk_meta_cap,
                    );
                }

                for fmp_to_free in ffi_manifests_vec {
                    unsafe {
                        let _ = CString::from_raw(fmp_to_free.filename);
                        let _ = Vec::from_raw_parts(
                            fmp_to_free.chunk_metadata as *mut FFISSAChunkMeta<H>,
                            fmp_to_free.chunk_metadata_len,
                            fmp_to_free.chunk_metadata_cap,
                        );
                    }
                }

                return FFIResult::InvalidString;
            }
        };

        ffi_manifests_vec.push(FFIFileManifestParent {
            filename: c_filename,
            chunk_metadata: chunk_meta_ptr as *const FFISSAChunkMeta<H>,
            chunk_metadata_len: fmp.chunk_count as usize,
            chunk_metadata_cap: chunk_meta_cap,
        });
    }

    let manifests_ptr = ffi_manifests_vec.as_mut_ptr();
    let manifests_len = ffi_manifests_vec.len();
    let manifests_cap = ffi_manifests_vec.capacity();
    //Give up ownership of the outer Vec
    std::mem::forget(ffi_manifests_vec);

    let mut entries: Vec<FFIChunkIndexEntry<H>> = chunk_index
        .into_iter()
        .map(|(hash, location)| {
            FFIChunkIndexEntry::from((
                hash,
                ChunkLocation {
                    offset: location.offset,
                    compressed_length: location.compressed_length,
                },
            ))
        })
        .collect();

    let entries_ptr = entries.as_mut_ptr();
    let entries_len = entries.len();
    let entries_cap = entries.capacity();

    //Give up ownership of the Vec so it doesn't get deallocated.
    std::mem::forget(entries);

    let output = Box::new(FFISerializedOutput::from((
        manifests_ptr,
        manifests_len,
        manifests_cap,
        entries_ptr,
        entries_len,
        entries_cap,
        sorted_hashes.as_ptr(),
        sorted_hashes.len(),
        sorted_hashes.capacity(),
    )));

    std::mem::forget(sorted_hashes);

    unsafe {
        *out_ptr = Box::into_raw(output);
    };
    FFIResult::Ok
}

/// Serializes archive data into an FFI-safe structure.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
///
/// # Safety
/// - All pointer arguments must be non-null and valid for their
///   specified lengths.
/// - `out_ptr` must be a valid, non-null pointer.
/// - The memory allocated for `out_keys` in the `get_keys_cb` callback and for
///   `out_chunks` in the `get_chunks_cb` callback is owned by Rust upon return.
///   The C caller MUST NOT free this memory.
/// - On success, the pointer written to `out_ptr` is owned by the C
///   caller and MUST be freed by passing it to
///   `free_serialized_output_u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn serialize_uncompressed_data_u64(
    manifest_array_ptr: *const FFIFileManifestParentU64,
    manifest_len: usize,
    user_data: *mut c_void,
    get_keys_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        out_keys: *mut FFIKeyArrayU64,
    ) -> FFICallbackStatus,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const u64,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
    out_ptr: *mut *mut FFISerializedOutputU64
) -> FFIResult {
    if manifest_array_ptr.is_null()
        || out_ptr.is_null() {
        return FFIResult::NullArgument;
    };

    serialize_uncompressed_data_internal::<u64>(
        manifest_array_ptr,
        manifest_len,
        user_data,
        get_keys_cb,
        get_chunks_cb,
        out_ptr
    )
}

/// Serializes archive data into an FFI-safe structure.
///
/// On success, returns `FFIResult::Ok` and populates `out_ptr`.
///
/// # Safety
/// - All pointer arguments must be non-null and valid for their
///   specified lengths.
/// - `out_ptr` must be a valid, non-null pointer.
/// - The memory allocated for `out_keys` in the `get_keys_cb` callback and for
///   `out_chunks` in the `get_chunks_cb` callback is owned by Rust upon return.
///   The C caller MUST NOT free this memory.
/// - On success, the pointer written to `out_ptr` is owned by the C
///   caller and MUST be freed by passing it to
///   `free_serialized_output_u128`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn serialize_uncompressed_data_u128(
    manifest_array_ptr: *const FFIFileManifestParentU128,
    manifest_len: usize,
    user_data: *mut c_void,
    get_keys_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        out_keys: *mut FFIKeyArrayU128,
    ) -> FFICallbackStatus,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const u128,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
    out_ptr: *mut *mut FFISerializedOutputU128
) -> FFIResult {
    if manifest_array_ptr.is_null()
        || out_ptr.is_null() {
        return FFIResult::NullArgument;
    };

    serialize_uncompressed_data_internal::<u128>(
        manifest_array_ptr,
        manifest_len,
        user_data,
        get_keys_cb,
        get_chunks_cb,
        out_ptr
    )
}

/// Reconstructs each vector that was originally created in
/// `serialize_uncompressed_data_internal`:
/// * The top‑level `FFISerializedOutput` box is recovered with
///   `Box::from_raw(ptr)`.
/// * The arrays for file manifests, chunk indices and sorted hashes are
///   reconstructed with `Vec::from_raw_parts` and then dropped, which
///   releases the underlying memory.
/// * Each `FFIFileManifestParent` inside the manifest array has its
///   `filename` (`CString`) and `chunk_metadata` (`Vec<FFISSAChunkMeta>`) freed
///   as well.
/// * No further action is required by the caller; the function guarantees
///   that every allocation performed by the serialization routine is
///   returned to Rust’s allocator.
/// # Arguments
///
/// * `ptr` – pointer to an `FFISerializedOutput<H>` that was returned by
///   `serialize_uncompressed_data_*`.  The pointer is opaque to the
///   caller and must not be used after this function returns.
///
/// # Returns
///
/// No value is returned; the function performs in‑place deallocation.
///
/// # Safety
///
/// * `ptr` must be a non‑null pointer to a valid `FFISerializedOutput<H>`
///   that was allocated by `serialize_uncompressed_data_internal`
///   (via `Box::into_raw`).  Passing a null or invalid pointer
///   results in undefined behaviour.
/// * The function assumes that all inner pointers (`ser_manifest_ptr`,
///   `ser_chunk_index_ptr`, `sorted_hashes_ptr`) and the embedded C
///   strings and `FFISSAChunkMeta` arrays were allocated by the
///   serialization routine and have not yet been freed.
/// * After the function returns, the caller may no longer use the
///   `FFISerializedOutput` pointer.  All memory owned by that struct
///   has been reclaimed.
unsafe fn free_serialized_output_internal<H>(
    ptr: *mut FFISerializedOutput<H>
) {
    unsafe {
        let output = Box::from_raw(ptr);
        //Deallocate all the vectors whose memory was passed to C
        let ffi_manifests = Vec::from_raw_parts(
            output.ser_manifest_ptr,
            output.ser_manifest_len,
            output.ser_manifest_cap,
        );

        for fmp in ffi_manifests {
            //Deallocate the CString for the filename.
            let _ = CString::from_raw(fmp.filename);
            /*Reconstruct and deallocate the Vec for the chunk
            metadata.*/
            let _ = Vec::from_raw_parts(
                fmp.chunk_metadata as *mut FFISSAChunkMeta<H>,
                fmp.chunk_metadata_len,
                fmp.chunk_metadata_cap,
            );
        }

        let _ = Vec::from_raw_parts(
            output.ser_chunk_index_ptr,
            output.ser_chunk_index_len,
            output.ser_chunk_index_cap);
        let _ = Vec::from_raw_parts(
            output.sorted_hashes_ptr as *mut H,
            output.sorted_hashes_len,
            output.sorted_hashes_cap
        );
    }
}

/// Frees the memory allocated by `serialize_uncompressed_data_u64`.
///
/// This function is responsible for deallocating the
/// `FFISerializedOutput` struct and all the memory blocks it points
/// to. This includes the serialized manifest, data store,
/// chunk index, and sorted hashes.
///
/// # Safety
///
/// The `ptr` must be a non-null pointer returned from a successful
/// to `serialize_uncompressed_data_u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_serialized_output_u64(
    ptr: *mut FFISerializedOutputU64
) {
    if ptr.is_null() {
        return;
    }

    unsafe {free_serialized_output_internal::<u64>(
        ptr,
    )}
}

/// Frees the memory allocated by `serialize_uncompressed_data_u128`.
///
/// This function is responsible for deallocating the
/// `FFISerializedOutput` struct and all the memory blocks it points
/// to. This includes the serialized manifest, data store,
/// chunk index, and sorted hashes.
///
/// # Safety
///
/// The `ptr` must be a non-null pointer returned from a successful
/// to `serialize_uncompressed_data_u128`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_serialized_output_u128(
    ptr: *mut FFISerializedOutputU128
) {
    if ptr.is_null() {
        return;
    }

    unsafe {free_serialized_output_internal::<u128>(
        ptr,
    )}
}
