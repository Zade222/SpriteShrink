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
    FFIResult
};
use crate::ffi::ffi_types::{
    FFIChunkDataArray, FFIChunkIndexEntry,
    FFIFileManifestParent,
    FFIKeyArray, FFISerializedOutput,
    FFISerializedOutputU64, FFISerializedOutputU128,
    SerializeUncompressedDataArgsU64, SerializeUncompressedDataArgsU128,
    FFISSAChunkMeta,
};
use crate::lib_error_handling::SpriteShrinkError;
use crate::lib_structs::{
    ChunkLocation, FileManifestParent
};
use crate::serialization::{serialize_uncompressed_data};

/// Serialises uncompressed sprite data using 64bit hash keys.
///
/// Function for obtaining a `FFISerializedOutputU64` containing a manifest, a
/// chunk index and a sorted hash list. The function validates its arguments
/// and then forwards the work to the generic implementation
/// `serialize_uncompressed_data_internal::<u64>`.
///
/// # Parameters
///
/// * `args` – A pointer to a fully‑initialised
///   `SerializeUncompressedDataArgsU64` structure. The structure supplies the
///   manifest, user data and the two callbacks required for the serialisation
///   process.
/// * `out_ptr` – A mutable pointer that will be populated with a pointer to a
///   newly allocated `FFISerializedOutputU64` on success. The caller is
///   responsible for freeing this output via the matching
///   `free_serialized_output_u64` function.
///
/// # Safety
///
/// * Both `args` and `out_ptr` must be non‑null pointers. If either is null,
///   the function returns `FFIResult::NullArgument`.
/// * All pointer arguments **must** be non‑null and point to valid memory that
///   lives at least for the duration of the call.
/// * `args.manifest_array_ptr` must also be a valid, non‑null pointer to an
///   array of `FFIFileManifestParentU64` with `args.manifest_len` elements.
///   The caller is responsible for ensuring the array lives for the duration
///   of the call.
/// * The callbacks contained in `args` (`get_keys_cb` and `get_chunks_cb`) are
///   `unsafe extern "C"` functions; they must obey the contract described in
///   `SerializeUncompressedDataArgsU64`.
///
/// # Return value
///
/// Returns an `FFIResult` indicating the outcome:
///
/// * `FFIResult::StatusOk` – The operation succeeded and `*out_ptr` now points to a
///   valid `FFISerializedOutputU64`.
/// * `FFIResult::NullArgument` – Either `args` or `out_ptr` (or the manifest
///   pointer inside `args`) was null.
/// * Other error variants may be returned by the internal implementation if,
///   for example, a callback fails or memory allocation aborts.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn serialize_uncompressed_data_u64(
    args: *const SerializeUncompressedDataArgsU64,
    out_ptr: *mut *mut FFISerializedOutputU64
) -> FFIResult {
    if args.is_null()
        || out_ptr.is_null() {
            return FFIResult::NullArgument;
    };

    let args_int = unsafe{&*args};

    if args_int.manifest_array_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let file_manifest = unsafe {
        //Convert C inputs to Rust types

        /*Prepare file_manifest DashMap, which is the first parameter
        of the original serialize_uncompressed_data function, from C
        input.*/
        let ffi_manifests = slice::from_raw_parts(
            args_int.manifest_array_ptr,
            args_int.manifest_len
        );

        /*Since DashMap is not a supported type in C, the following
        reconstructs it for use by the library.*/

        /*Reconstruct the file_manifest DashMap via the following
        loop.*/
        let file_manifest: DashMap<String, FileManifestParent<u64>> = ffi_manifests
        .iter()
        .map(|ffi_fmp| {
            let rust_fmp = FileManifestParent::<u64>::from(ffi_fmp);
            (rust_fmp.filename.clone(), rust_fmp)
        })
        .collect();

        file_manifest
    };

    //Wrap C function pointers into Rust closures for the core library.
    let get_chunks_cb = args_int.get_chunks_cb;
    let get_keys_cb = args_int.get_keys_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_keys_closure = || -> Result<Vec<u64>, SpriteShrinkError> {
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

    let get_chunks_closure = move |hashes: &[u64]| -> Result<
        Vec<Vec<u8>>,
        SpriteShrinkError>
    {
        let mut ffi_chunks_array = FFIChunkDataArray {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0
        };
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
    let serialized_data = match serialize_uncompressed_data::<
        _,
        SpriteShrinkError,
        u64,
        _
    >(
        &file_manifest,
        &get_keys_closure,
        &get_chunks_closure
    ) {
        Ok(data) => data,
        Err(_) => return FFIResult::SerializationMissingChunk,
    };

    let mut ffi_manifests_vec: Vec<FFIFileManifestParent<u64>> =
           Vec::with_capacity(serialized_data.ser_file_manifest.len());

    for fmp in serialized_data.ser_file_manifest {
        let mut chunk_meta_vec: Vec<FFISSAChunkMeta<u64>> = fmp
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
                            fmp_to_free.chunk_metadata as *mut FFISSAChunkMeta<u64>,
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
            chunk_metadata: chunk_meta_ptr as *const FFISSAChunkMeta<u64>,
            chunk_metadata_len: fmp.chunk_count as usize,
            chunk_metadata_cap: chunk_meta_cap,
        });
    }

    let manifests_ptr = ffi_manifests_vec.as_mut_ptr();
    let manifests_len = ffi_manifests_vec.len();
    let manifests_cap = ffi_manifests_vec.capacity();
    //Give up ownership of the outer Vec
    std::mem::forget(ffi_manifests_vec);

    let mut entries: Vec<FFIChunkIndexEntry<u64>> = serialized_data.chunk_index
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
        serialized_data.sorted_hashes.as_ptr(),
        serialized_data.sorted_hashes.len(),
        serialized_data.sorted_hashes.capacity(),
    )));

    std::mem::forget(serialized_data.sorted_hashes);

    unsafe {
        *out_ptr = Box::into_raw(output);
    };
    FFIResult::StatusOk
}

/// Serialises uncompressed sprite data using 128bit hash keys.
///
/// Function for obtaining a `FFISerializedOutputU128` containing a manifest, a
/// chunk index and a sorted hash list. The function validates its arguments
/// and then forwards the work to the generic implementation
/// `serialize_uncompressed_data_internal::<u128>`.
///
/// # Parameters
///
/// * `args`: A pointer to a fully‑initialised
///   `SerializeUncompressedDataArgsU128` structure. The structure supplies the
///   manifest, user data and the two callbacks required for the serialisation
///   process.
/// * `out_ptr`: A mutable pointer that will be populated with a pointer to a
///   newly allocated `FFISerializedOutputU128` on success. The caller is
///   responsible for freeing this output via the matching
///   `free_serialized_output_u128` function.
///
/// # Safety
///
/// * Both `args` and `out_ptr` must be non‑null pointers. If either is null,
///   the function returns `FFIResult::NullArgument`.
/// * All pointer arguments **must** be non‑null and point to valid memory that
///   lives at least for the duration of the call.
/// * `args.manifest_array_ptr` must also be a valid, non‑null pointer to an
///   array of `FFIFileManifestParentU128` with `args.manifest_len` elements.
///   The caller is responsible for ensuring the array lives for the duration
///   of the call.
/// * The callbacks contained in `args` (`get_keys_cb` and `get_chunks_cb`) are
///   `unsafe extern "C"` functions; they must obey the contract described in
///   `SerializeUncompressedDataArgsU128`.
///
/// # Return value
///
/// Returns an `FFIResult` indicating the outcome:
///
/// * `FFIResult::StatusOk` – The operation succeeded and `*out_ptr` now points to a
///   valid `FFISerializedOutputU128`.
/// * `FFIResult::NullArgument` – Either `args` or `out_ptr` (or the manifest
///   pointer inside `args`) was null.
/// * Other error variants may be returned by the internal implementation if,
///   for example, a callback fails or memory allocation aborts.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn serialize_uncompressed_data_u128(
    args: *const SerializeUncompressedDataArgsU128,
    out_ptr: *mut *mut FFISerializedOutputU128
) -> FFIResult {
    if args.is_null()
        || out_ptr.is_null() {
            return FFIResult::NullArgument;
    };

    let args_int = unsafe{&*args};

    if args_int.manifest_array_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let file_manifest = unsafe {
        //Convert C inputs to Rust types

        /*Prepare file_manifest DashMap, which is the first parameter
        of the original serialize_uncompressed_data function, from C
        input.*/
        let ffi_manifests = slice::from_raw_parts(
            args_int.manifest_array_ptr,
            args_int.manifest_len
        );

        /*Since DashMap is not a supported type in C, the following
        reconstructs it for use by the library.*/

        /*Reconstruct the file_manifest DashMap via the following
        loop.*/

        let file_manifest: DashMap<String, FileManifestParent<u128>> =
            ffi_manifests.iter()
            .map(|ffi_fmp| {
                let rust_fmp = FileManifestParent::<u128>::from(ffi_fmp);
                (rust_fmp.filename.clone(), rust_fmp)
            })
            .collect();

        file_manifest
    };

    //Wrap C function pointers into Rust closures for the core library.
    let get_chunks_cb = args_int.get_chunks_cb;
    let get_keys_cb = args_int.get_keys_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_keys_closure = || -> Result<Vec<u128>, SpriteShrinkError> {
        let mut ffi_keys_array = FFIKeyArray::<[u8; 16]> {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        };

        let status = unsafe {
            get_keys_cb(
                user_data_addr as *mut c_void,
                &mut ffi_keys_array, //Pass a pointer to the struct
            )
        };
        Result::from(status)?;

        let keys_bytes = unsafe {
            Vec::from_raw_parts(
                ffi_keys_array.ptr,
                ffi_keys_array.len,
                ffi_keys_array.cap)
        };

        let keys = keys_bytes
            .iter()
            .map(|bytes| u128::from_le_bytes(*bytes))
            .collect();

        Ok(keys)
    };

    let get_chunks_closure = move |
        hashes: &[u128]
    | -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let mut ffi_chunks_array = FFIChunkDataArray {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0
        };
        let ffi_hash_bytes: Vec<u8> = hashes
            .iter()
            .flat_map(|h| h.to_le_bytes())
            .collect();

        unsafe {
            (get_chunks_cb)(
                user_data_addr as *mut c_void,
                ffi_hash_bytes.as_ptr(),
                hashes.len(),
                & mut ffi_chunks_array
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

    //Pass converted data to internal library Rust logic.
    let serialized_data = match serialize_uncompressed_data::<
        _,
        SpriteShrinkError,
        u128,
        _
    >(
        &file_manifest,
        &get_keys_closure,
        &get_chunks_closure
    ) {
        Ok(data) => data,
        Err(_) => return FFIResult::SerializationMissingChunk,
    };

    let mut ffi_manifests_vec: Vec<FFIFileManifestParent<[u8; 16]>> =
        Vec::with_capacity(serialized_data.ser_file_manifest.len());

    for fmp in serialized_data.ser_file_manifest{
        let mut chunk_meta_vec: Vec<FFISSAChunkMeta<[u8; 16]>> = fmp
            .chunk_metadata
            .iter()
            .map(|meta| FFISSAChunkMeta{
                hash: meta.hash.to_le_bytes(),
                offset: meta.offset,
                length: meta.length,
            }).collect();

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
                        chunk_meta_cap
                    );
                }
                for fmp_to_free in ffi_manifests_vec {
                    unsafe {
                        let _ = CString::from_raw(fmp_to_free.filename);
                        let _ = Vec::from_raw_parts(
                            fmp_to_free.chunk_metadata as *mut FFISSAChunkMeta<
                                [u8; 16]
                            >,
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
            chunk_metadata: chunk_meta_ptr,
            chunk_metadata_len: fmp.chunk_count as usize,
            chunk_metadata_cap: chunk_meta_cap,
        });
    }

    let manifests_ptr = ffi_manifests_vec.as_mut_ptr();
    let manifests_len = ffi_manifests_vec.len();
    let manifests_cap = ffi_manifests_vec.capacity();
    //Give up ownership of the outer Vec
    std::mem::forget(ffi_manifests_vec);

    let mut entries: Vec<FFIChunkIndexEntry<[u8; 16]>> = serialized_data
        .chunk_index
        .into_iter()
        .map(|
            (hash, location)
        | FFIChunkIndexEntry::from((hash.to_le_bytes(), location)))
        .collect();

    let entries_ptr = entries.as_mut_ptr();
    let entries_len = entries.len();
    let entries_cap = entries.capacity();

    //Give up ownership of the Vec so it doesn't get deallocated.
    std::mem::forget(entries);

    let sorted_hashes_bytes: Vec<[u8; 16]> = serialized_data
        .sorted_hashes
        .into_iter()
        .map(|h| h.to_le_bytes())
        .collect();

    let sorted_hashes_ptr = sorted_hashes_bytes.as_ptr();
    let sorted_hashes_len = sorted_hashes_bytes.len();
    let sorted_hashes_cap = sorted_hashes_bytes.capacity();

    //Give up ownership of the Vec so it doesn't get deallocated.
    std::mem::forget(sorted_hashes_bytes);

    let output = Box::new(FFISerializedOutput::from((
        manifests_ptr,
        manifests_len,
        manifests_cap,
        entries_ptr,
        entries_len,
        entries_cap,
        sorted_hashes_ptr,
        sorted_hashes_len,
        sorted_hashes_cap,
    )));

    unsafe {
        *out_ptr = Box::into_raw(output);
    };
    FFIResult::StatusOk
}

/// Reconstructs each vector that was originally created in
/// `serialize_uncompressed_data_internal`:
/// * The top‑level `FFISerializedOutput` box is recovered with
///   `Box::from_raw(ptr)`.
/// * The arrays for file manifests, chunk indices and sorted hashes are
///   reconstructed with `Vec::from_raw_parts` and then dropped, which
///   releases the underlying memory.
/// * Each `FFIFileManifestParent` inside the manifest array has its
///   `filename` (`CString`) and `chunk_metadata` (`Vec<FFISSAChunkMeta>`)
///   freed as well.
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

    unsafe {free_serialized_output_internal::<[u8; 16]>(
        ptr,
    )}
}
