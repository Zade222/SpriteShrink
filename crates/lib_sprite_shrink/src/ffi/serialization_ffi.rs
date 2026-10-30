//! FFI-safe serialization functions for sprite-shrink archives.
//!
//! This module exposes Rust's serialization logic to C callers, handling the
//! complex conversion of data structures and memory management.

use std::{
    ffi::{CStr, CString},
    mem::ManuallyDrop,
    os::raw::c_void,
    slice
};

use bitcode::Encode;
use dashmap::DashMap;
use libc::c_char;

use crate::ffi::ffi_error_handling::{
    FFIResult
};
use crate::ffi::ffi_types::{
    FFIChunkDataArray, FFIChunkIndexEntry, FFICompData,
    FFIFileManifestParent, FFIFileManifestParentU64,
    FFIFileManifestParentU128,
    FFIKeyArray, FFISerializedOutput,
    FFISerializedOutputU64, FFISerializedOutputU128,
    SerializeUncompressedDataArgsU64, SerializeUncompressedDataArgsU128,
    FFISSAChunkMeta, FFITocEntry, FFIVecBytes, U128Bytes
};
use crate::lib_error_handling::SpriteShrinkError;
use crate::lib_structs::{
    HASH_TYPE_XXH3_64,HASH_TYPE_XXH3_128,
    CompressionResult, FileManifestParent, SerializedData, SSMCTocEntry,
};
use crate::serialization::{build_ssmc_prefix, serialize_uncompressed_data};

/// Splits a `Vec` into the pointer, length and capacity handed to C. The
/// matching free function rebuilds it with `Vec::from_raw_parts`.
fn vec_into_raw_parts<T>(vec: Vec<T>) -> (*mut T, usize, usize) {
    let mut vec = ManuallyDrop::new(vec);
    (vec.as_mut_ptr(), vec.len(), vec.capacity())
}

/// Pairs each manifest from C with its filename, keyed the way
/// `serialize_uncompressed_data` expects.
///
/// # Errors
///
/// * `NullArgument` if a filename pointer is null.
/// * `InvalidString` if a filename isn't valid UTF-8.
/// * `DuplicateFilename` if two manifests have the same name. Keying by
///   name would otherwise drop one of the files without an error.
///
/// # Safety
///
/// `manifests` and `filenames` must each point to `len` elements, and every
/// non-null filename must be a null-terminated string.
unsafe fn read_file_manifest<F, H>(
    manifests: *const FFIFileManifestParent<F>,
    filenames: *const *const c_char,
    len: usize,
) -> Result<DashMap<String, FileManifestParent<H>>, FFIResult>
where
    for<'a> FileManifestParent<H>: From<&'a FFIFileManifestParent<F>>,
{
    // SAFETY: the caller guarantees `manifests` holds `len` elements.
    let ffi_manifests = unsafe { slice::from_raw_parts(manifests, len) };
    // SAFETY: the caller guarantees `filenames` holds `len` elements.
    let name_ptrs = unsafe { slice::from_raw_parts(filenames, len) };

    let file_manifest = DashMap::with_capacity(len);

    for (ffi_fmp, &name_ptr) in ffi_manifests.iter().zip(name_ptrs) {
        if name_ptr.is_null() {
            return Err(FFIResult::NullArgument);
        }

        // SAFETY: `name_ptr` isn't null, and the caller guarantees it's
        // null-terminated.
        let Ok(filename) = unsafe { CStr::from_ptr(name_ptr) }.to_str() else {
            return Err(FFIResult::InvalidString);
        };

        if file_manifest
            .insert(filename.to_owned(), FileManifestParent::from(ffi_fmp))
            .is_some()
        {
            return Err(FFIResult::DuplicateFilename);
        }
    }

    Ok(file_manifest)
}

/// Converts the serializer's output into the struct handed to C,
/// converting each hash to its FFI form `F`.
///
/// # Errors
///
/// Returns `InvalidString` if a filename contains a null byte. Names read
/// from C strings can't, but it's checked rather than assumed.
fn into_ffi_output<H, F>(
    data: SerializedData<H>
) -> Result<FFISerializedOutput<F>, FFIResult>
where
    F: From<H>,
{
    /*The only fallible step comes first, so nothing has been handed out
    yet if it fails.*/
    let Ok(filenames) = data.archive_toc
        .iter()
        .map(|entry| CString::new(entry.filename.as_str()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return Err(FFIResult::InvalidString);
    };

    let toc: Vec<FFITocEntry> = filenames
        .into_iter()
        .zip(&data.archive_toc)
        .map(|(filename, entry)| FFITocEntry {
            filename: filename.into_raw(),
            uncompressed_size: entry.uncompressed_size,
        })
        .collect();

    let manifests: Vec<FFIFileManifestParent<F>> = data.ser_file_manifest
        .into_iter()
        .map(|fmp| {
            let chunk_metadata: Vec<FFISSAChunkMeta<F>> = fmp.chunk_metadata
                .into_iter()
                .map(|meta| FFISSAChunkMeta {
                    hash: meta.hash.into(),
                    offset: meta.offset,
                    length: meta.length,
                })
                .collect();
            let (ptr, len, cap) = vec_into_raw_parts(chunk_metadata);

            FFIFileManifestParent {
                chunk_metadata: ptr,
                chunk_metadata_len: len,
                chunk_metadata_cap: cap,
            }
        })
        .collect();

    let chunk_index: Vec<FFIChunkIndexEntry<F>> = data.chunk_index
        .into_iter()
        .map(|(hash, location)| {
            FFIChunkIndexEntry::from((hash.into(), location))
        })
        .collect();

    let sorted_hashes: Vec<F> = data.sorted_hashes
        .into_iter()
        .map(F::from)
        .collect();

    let (toc_ptr, toc_len, toc_cap) = vec_into_raw_parts(toc);
    let (ser_manifest_ptr, ser_manifest_len, ser_manifest_cap) =
        vec_into_raw_parts(manifests);
    let (ser_chunk_index_ptr, ser_chunk_index_len, ser_chunk_index_cap) =
        vec_into_raw_parts(chunk_index);
    let (sorted_hashes_ptr, sorted_hashes_len, sorted_hashes_cap) =
        vec_into_raw_parts(sorted_hashes);

    Ok(FFISerializedOutput {
        toc_ptr,
        toc_len,
        toc_cap,
        ser_manifest_ptr,
        ser_manifest_len,
        ser_manifest_cap,
        ser_chunk_index_ptr,
        ser_chunk_index_len,
        ser_chunk_index_cap,
        sorted_hashes_ptr,
        sorted_hashes_len,
        sorted_hashes_cap,
    })
}

/// Serialises uncompressed sprite data using 64bit hash keys.
///
/// Function for obtaining a `FFISerializedOutputU64` containing a table of
/// contents, a manifest, a chunk index and a sorted hash list. The function
/// validates its arguments and then forwards the work to
/// `serialize_uncompressed_data`.
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
/// * `args.filenames` must be a valid, non‑null pointer to an array of
///   `args.manifest_len` null-terminated strings, one per manifest.
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
/// * `FFIResult::NullArgument` – `args`, `out_ptr`, the manifest or
///   filename array inside `args`, or one of the filenames was null.
/// * `FFIResult::InvalidString` – A filename isn't valid UTF-8.
/// * `FFIResult::DuplicateFilename` – Two manifests have the same filename.
/// * `FFIResult::SerializationMissingChunk` – The chunk callback didn't
///   return data for a hash in the manifests.
/// * Other error variants if, for example, a callback fails.
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

    if args_int.manifest_array_ptr.is_null() ||
        args_int.filenames.is_null() {
            return FFIResult::NullArgument;
    }

    /*Pair each manifest with its filename in the DashMap the original
    serialize_uncompressed_data function takes.*/
    // SAFETY: the caller guarantees both arrays hold `manifest_len`
    // elements and every filename is null-terminated.
    let file_manifest = match unsafe {
        read_file_manifest::<_, u64>(
            args_int.manifest_array_ptr,
            args_int.filenames,
            args_int.manifest_len
        )
    } {
        Ok(file_manifest) => file_manifest,
        Err(status) => return status,
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
        Err(e) => return e.into(),
    };

    let output = match into_ffi_output::<u64, u64>(serialized_data) {
        Ok(output) => output,
        Err(status) => return status,
    };

    // SAFETY: `out_ptr` was checked for null above, and the caller
    // guarantees it's valid for writes.
    unsafe {
        *out_ptr = Box::into_raw(Box::new(output));
    };
    FFIResult::StatusOk
}

/// Serialises uncompressed sprite data using 128bit hash keys.
///
/// Function for obtaining a `FFISerializedOutputU128` containing a table of
/// contents, a manifest, a chunk index and a sorted hash list. The function
/// validates its arguments and then forwards the work to
/// `serialize_uncompressed_data`.
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
/// * `args.filenames` must be a valid, non‑null pointer to an array of
///   `args.manifest_len` null-terminated strings, one per manifest.
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
/// * `FFIResult::NullArgument` – `args`, `out_ptr`, the manifest or
///   filename array inside `args`, or one of the filenames was null.
/// * `FFIResult::InvalidString` – A filename isn't valid UTF-8.
/// * `FFIResult::DuplicateFilename` – Two manifests have the same filename.
/// * `FFIResult::SerializationMissingChunk` – The chunk callback didn't
///   return data for a hash in the manifests.
/// * Other error variants if, for example, a callback fails.
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

    if args_int.manifest_array_ptr.is_null() ||
        args_int.filenames.is_null() {
            return FFIResult::NullArgument;
    }

    /*Pair each manifest with its filename in the DashMap the original
    serialize_uncompressed_data function takes.*/
    // SAFETY: the caller guarantees both arrays hold `manifest_len`
    // elements and every filename is null-terminated.
    let file_manifest = match unsafe {
        read_file_manifest::<_, u128>(
            args_int.manifest_array_ptr,
            args_int.filenames,
            args_int.manifest_len
        )
    } {
        Ok(file_manifest) => file_manifest,
        Err(status) => return status,
    };

    //Wrap C function pointers into Rust closures for the core library.
    let get_chunks_cb = args_int.get_chunks_cb;
    let get_keys_cb = args_int.get_keys_cb;
    let user_data_addr = args_int.user_data as usize;

    let get_keys_closure = || -> Result<Vec<u128>, SpriteShrinkError> {
        let mut ffi_keys_array = FFIKeyArray::<U128Bytes> {
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
            .map(|bytes| (*bytes).into())
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

        let status = unsafe {
            (get_chunks_cb)(
                user_data_addr as *mut c_void,
                ffi_hash_bytes.as_ptr(),
                hashes.len(),
                & mut ffi_chunks_array
            )};
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
        Err(e) => return e.into(),
    };

    let output = match into_ffi_output::<u128, U128Bytes>(serialized_data) {
        Ok(output) => output,
        Err(status) => return status,
    };

    // SAFETY: `out_ptr` was checked for null above, and the caller
    // guarantees it's valid for writes.
    unsafe {
        *out_ptr = Box::into_raw(Box::new(output));
    };
    FFIResult::StatusOk
}

/// Reconstructs each vector that was originally created in
/// `into_ffi_output`:
/// * The top‑level `FFISerializedOutput` box is recovered with
///   `Box::from_raw(ptr)`.
/// * The arrays for the table of contents, file manifests, chunk indices
///   and sorted hashes are reconstructed with `Vec::from_raw_parts` and then
///   dropped, which releases the underlying memory.
/// * Each `FFITocEntry` has its `filename` (`CString`) freed, and each
///   `FFIFileManifestParent` has its `chunk_metadata`
///   (`Vec<FFISSAChunkMeta>`) freed.
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
///   that was allocated by `serialize_uncompressed_data_*`
///   (via `Box::into_raw`).  Passing a null or invalid pointer
///   results in undefined behaviour.
/// * The function assumes that all inner pointers (`toc_ptr`,
///   `ser_manifest_ptr`, `ser_chunk_index_ptr`, `sorted_hashes_ptr`) and
///   the embedded C
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
        let toc = Vec::from_raw_parts(
            output.toc_ptr,
            output.toc_len,
            output.toc_cap,
        );

        for entry in toc {
            //Deallocate the CString for the filename.
            let _ = CString::from_raw(entry.filename);
        }

        let ffi_manifests = Vec::from_raw_parts(
            output.ser_manifest_ptr,
            output.ser_manifest_len,
            output.ser_manifest_cap,
        );

        for fmp in ffi_manifests {
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

    unsafe {free_serialized_output_internal::<U128Bytes>(
        ptr,
    )}
}

/// Copies the table of contents from C.
///
/// # Errors
///
/// * `NullArgument` if a filename pointer is null.
/// * `InvalidString` if a filename isn't valid UTF-8.
///
/// # Safety
///
/// `toc` must point to `len` elements, and every non-null filename must be
/// a null-terminated string.
unsafe fn read_toc(
    toc: *const FFITocEntry,
    len: usize,
) -> Result<Vec<SSMCTocEntry>, FFIResult> {
    // SAFETY: the caller guarantees `toc` holds `len` elements.
    let entries = unsafe { slice::from_raw_parts(toc, len) };

    entries
        .iter()
        .map(|entry| {
            if entry.filename.is_null() {
                return Err(FFIResult::NullArgument);
            }

            // SAFETY: `filename` isn't null, and the caller guarantees it's
            // null-terminated.
            let Ok(filename) = unsafe { CStr::from_ptr(entry.filename) }
                .to_str()
            else {
                return Err(FFIResult::InvalidString);
            };

            Ok(SSMCTocEntry {
                filename: filename.to_owned(),
                uncompressed_size: entry.uncompressed_size,
            })
        })
        .collect()
}

/// Copies the dictionary and encoded chunk index out of a build result.
///
/// # Errors
///
/// Returns `NullArgument` if either buffer pointer is null.
///
/// # Safety
///
/// Each non-null buffer in `comp` must hold its recorded number of bytes.
unsafe fn read_comp_data(
    comp: &FFICompData
) -> Result<CompressionResult, FFIResult> {
    if comp.dictionary_data_ptr.is_null() ||
        comp.enc_chunk_index_data_ptr.is_null()
    {
        return Err(FFIResult::NullArgument);
    }

    // SAFETY: the pointer isn't null, and the caller guarantees it holds
    // `dictionary_data_len` bytes.
    let dictionary = unsafe {
        slice::from_raw_parts(
            comp.dictionary_data_ptr,
            comp.dictionary_data_len
        )
    }.to_vec();
    // SAFETY: as above, for `enc_chunk_index_data_len` bytes.
    let enc_chunk_index = unsafe {
        slice::from_raw_parts(
            comp.enc_chunk_index_data_ptr,
            comp.enc_chunk_index_data_len
        )
    }.to_vec();

    Ok(CompressionResult {
        dictionary_size: dictionary.len() as u64,
        dictionary,
        enc_chunk_index_size: enc_chunk_index.len() as u64,
        enc_chunk_index,
    })
}

/// Shared body of `build_ssmc_prefix_u64` and `build_ssmc_prefix_u128`,
/// converting each manifest's hashes from their FFI form `F` to `H`.
///
/// # Safety
///
/// Every non-null pointer must meet the contract of the public functions.
unsafe fn build_ssmc_prefix_internal<F, H>(
    toc: *const FFITocEntry,
    manifests: *const FFIFileManifestParent<F>,
    file_count: usize,
    comp_data: *const FFICompData,
    hash_type: u8,
) -> Result<FFIVecBytes, FFIResult>
where
    for<'a> FileManifestParent<H>: From<&'a FFIFileManifestParent<F>>,
    H: Encode,
{
    if toc.is_null() || manifests.is_null() || comp_data.is_null() {
        return Err(FFIResult::NullArgument);
    }

    // SAFETY: `toc` isn't null, and the caller guarantees it holds
    // `file_count` entries with null-terminated names.
    let toc = unsafe { read_toc(toc, file_count) }?;

    // SAFETY: `manifests` isn't null, and the caller guarantees it holds
    // `file_count` elements.
    let manifests: Vec<FileManifestParent<H>> = unsafe {
        slice::from_raw_parts(manifests, file_count)
    }
        .iter()
        .map(FileManifestParent::from)
        .collect();

    // SAFETY: `comp_data` isn't null, and the caller guarantees it points to
    // a valid `FFICompData`.
    let comp_data = unsafe { &*comp_data };
    // SAFETY: the caller guarantees each buffer holds its recorded length.
    let comp = unsafe { read_comp_data(comp_data) }?;

    let prefix = build_ssmc_prefix(&toc, &manifests, &comp, hash_type)?;
    let (ptr, len, cap) = vec_into_raw_parts(prefix);

    Ok(FFIVecBytes { ptr, len, cap })
}

/// Builds every byte of an SSMC archive that comes before the compressed
/// data, for archives whose chunks are identified by `u64` hashes.
///
/// The host writes the returned bytes and then appends the compressed chunk
/// data that `archive_builder_build_u64` passed to its write callback.
///
/// # Arguments
///
/// * `toc`: One entry per file, in the order the files are numbered. The
///   `toc_ptr` array from `serialize_uncompressed_data_u64` can be passed
///   as is.
/// * `manifests`: One manifest per file, in the same order as `toc`.
/// * `file_count`: The number of elements in both `toc` and `manifests`.
/// * `comp_data`: The result of `archive_builder_build_u64`.
/// * `out_ptr`: Receives the prefix on success.
///
/// # Returns
///
/// * `FFIResult::StatusOk` on success.
/// * `FFIResult::NullArgument` if any pointer argument, a filename in
///   `toc`, or a buffer in `comp_data` is null.
/// * `FFIResult::InvalidString` if a filename isn't valid UTF-8.
/// * `FFIResult::InternalError` if the archive is too large for the
///   header's 32-bit fields.
///
/// # Safety
///
/// - `toc` and `manifests` must each point to `file_count` elements, and
///   every filename must be a null-terminated string. Nothing passed in is
///   freed or kept; the caller still owns it.
/// - `comp_data` must point to a valid `FFICompData`.
/// - On success, the pointer written to `out_ptr` must be freed with
///   `free_ssmc_prefix`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks only #[must_use] functions SPRITE_SHRINK_MUST_USE"
)]
#[must_use]
pub unsafe extern "C" fn build_ssmc_prefix_u64(
    toc: *const FFITocEntry,
    manifests: *const FFIFileManifestParentU64,
    file_count: usize,
    comp_data: *const FFICompData,
    out_ptr: *mut *mut FFIVecBytes,
) -> FFIResult {
    if out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: the caller upholds this function's contract, which is the
    // helper's.
    match unsafe {
        build_ssmc_prefix_internal::<_, u64>(
            toc,
            manifests,
            file_count,
            comp_data,
            HASH_TYPE_XXH3_64,
        )
    } {
        Ok(prefix) => {
            // SAFETY: `out_ptr` was checked for null above and the caller
            // guarantees it is valid for writes.
            unsafe { *out_ptr = Box::into_raw(Box::new(prefix)) };
            FFIResult::StatusOk
        }
        Err(status) => status,
    }
}

/// Builds every byte of an SSMC archive that comes before the compressed
/// data, for archives whose chunks are identified by `u128` hashes.
///
/// The host writes the returned bytes and then appends the compressed chunk
/// data that `archive_builder_build_u128` passed to its write callback.
///
/// # Arguments
///
/// * `toc`: One entry per file, in the order the files are numbered. The
///   `toc_ptr` array from `serialize_uncompressed_data_u128` can be passed
///   as is.
/// * `manifests`: One manifest per file, in the same order as `toc`.
/// * `file_count`: The number of elements in both `toc` and `manifests`.
/// * `comp_data`: The result of `archive_builder_build_u128`.
/// * `out_ptr`: Receives the prefix on success.
///
/// # Returns
///
/// * `FFIResult::StatusOk` on success.
/// * `FFIResult::NullArgument` if any pointer argument, a filename in
///   `toc`, or a buffer in `comp_data` is null.
/// * `FFIResult::InvalidString` if a filename isn't valid UTF-8.
/// * `FFIResult::InternalError` if the archive is too large for the
///   header's 32-bit fields.
///
/// # Safety
///
/// - `toc` and `manifests` must each point to `file_count` elements, and
///   every filename must be a null-terminated string. Nothing passed in is
///   freed or kept; the caller still owns it.
/// - `comp_data` must point to a valid `FFICompData`.
/// - On success, the pointer written to `out_ptr` must be freed with
///   `free_ssmc_prefix`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks only #[must_use] functions SPRITE_SHRINK_MUST_USE"
)]
#[must_use]
pub unsafe extern "C" fn build_ssmc_prefix_u128(
    toc: *const FFITocEntry,
    manifests: *const FFIFileManifestParentU128,
    file_count: usize,
    comp_data: *const FFICompData,
    out_ptr: *mut *mut FFIVecBytes,
) -> FFIResult {
    if out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: the caller upholds this function's contract, which is the
    // helper's.
    match unsafe {
        build_ssmc_prefix_internal::<_, u128>(
            toc,
            manifests,
            file_count,
            comp_data,
            HASH_TYPE_XXH3_128,
        )
    } {
        Ok(prefix) => {
            // SAFETY: `out_ptr` was checked for null above and the caller
            // guarantees it is valid for writes.
            unsafe { *out_ptr = Box::into_raw(Box::new(prefix)) };
            FFIResult::StatusOk
        }
        Err(status) => status,
    }
}

/// Frees a prefix returned by `build_ssmc_prefix_u64` or
/// `build_ssmc_prefix_u128`.
///
/// Passing a null pointer does nothing.
///
/// # Safety
///
/// `ptr` must be null or a pointer written by one of those functions that
/// has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_ssmc_prefix(ptr: *mut FFIVecBytes) {
    if ptr.is_null() {
        return;
    }

    // SAFETY: `ptr` is non-null and, per the caller's guarantee, came from
    // `Box::into_raw` in a `build_ssmc_prefix_*` function.
    let prefix = unsafe { Box::from_raw(ptr) };
    // SAFETY: the parts came from `vec_into_raw_parts` and are unchanged.
    drop(unsafe { Vec::from_raw_parts(prefix.ptr, prefix.len, prefix.cap) });
}
