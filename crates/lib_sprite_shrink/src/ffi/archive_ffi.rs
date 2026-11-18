//! FFI layer for the archive building process.
//!
//! This module provides a C-compatible interface for creating, configuring,
//! and building sprite-shrink archives. It handles the conversion between
//! Rust and C data types, ensuring memory safety across the FFI boundary.

use std::{
    os::raw::c_void,
    slice,
};

use bitcode::Encode;

use crate::archive::{
    ArchiveBuilder, decompress_chunk, decompress_chunk_checked
};
use crate::ffi::ffi_error_handling::{
    FFIResult
};
use crate::ffi::ffi_ownership::copy_host_chunks;
use crate::ffi::ffi_types::{
    ArchiveBuilderU64, ArchiveBuilderU128,
    ArchiveBuilderArgsU64, ArchiveBuilderArgsU128,
    FFIChunkDataArray, FFICompData,
    FFIProgress
};
use crate::lib_error_handling::{
    IsCancelled, SpriteShrinkError
};
use crate::lib_structs::{CompressionResult};

//Type alias for clarity in other functions
trait FFIBuilderHandle {
    type HashType;
}

impl FFIBuilderHandle for ArchiveBuilderU64 {
    type HashType = u64;
}

impl FFIBuilderHandle for ArchiveBuilderU128 {
    type HashType = u128;
}

trait ArchiveBuilderTrait<H> {
    //fn set_compression_algorithm(&mut self, algorithm: u16);
    fn set_compression_level(&mut self, level: i32);
    fn set_dictionary_size(&mut self, size: u64);
    fn with_c_progress(
        &mut self,
        callback: extern "C" fn(FFIProgress, *mut c_void),
        user_data: *mut c_void
    );
    fn set_worker_threads(&mut self, threads: usize);
    fn set_optimize_dictionary(&mut self, optimize: bool);

    //This method consumes the builder
    fn build(self: Box<Self>) -> Result<CompressionResult, SpriteShrinkError>;
}

impl<'a, E, H, R, W> ArchiveBuilderTrait<H> for ArchiveBuilder<'a, E, H, R, W>
where
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    H: Copy + std::fmt::Debug + Encode + Eq + std::hash::Hash + serde::Serialize +
        Send + Sync + 'static + std::fmt::Display + Ord,
    R: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    W: FnMut(&[u8]) -> Result<(), E> + Send + Sync + 'static,
{
    /*fn set_compression_algorithm(&mut self, algorithm: u16) {
        self.compression_algorithm(algorithm);
    }*/
    fn set_compression_level(&mut self, level: i32) {
        self.compression_level(level);
    }
    fn set_dictionary_size(&mut self, size: u64) {
        self.dictionary_size(size);
    }
    fn set_worker_threads(&mut self, threads: usize) {
        self.worker_threads(threads);
    }
    fn set_optimize_dictionary(&mut self, optimize: bool) {
        self.optimize_dictionary(optimize);
    }
    fn with_c_progress(
        &mut self, callback: extern "C" fn(FFIProgress, *mut c_void),
        user_data: *mut c_void
    ) {
        self.with_c_progress(callback, user_data);
    }


    fn build(self: Box<Self>) -> Result<CompressionResult, SpriteShrinkError> {
        (*self).build()
    }
}

/// The buffers a host passes to the `decompress_chunk_*` functions.
#[derive(Clone, Copy)]
struct HostBuffers {
    comp: *const u8,
    comp_len: usize,
    dict: *const u8,
    dict_len: usize,
    out: *mut u8,
    out_len: usize,
}

/// Creates and initializes a new `ArchiveBuilder` for u64 hashes.
///
/// This function serves as the entry point for the archive creation process
/// from a C interface. It configures a new builder with the necessary file
/// metadata, a list of all unique chunk hashes, and C callback functions for
/// data retrieval and writing.
///
/// On success, it returns a pointer to an opaque `ArchiveBuilderU64` handle
/// via the `out_ptr` parameter. This handle can then be used with other FFI
/// functions to configure and finally build the archive.
///
/// # Arguments
///
/// * `args`: Struct for all the necessary data for initializing the archive
///   creation process.
/// * `out_ptr`: A pointer to a `*mut ArchiveBuilderU64` where the handle to
///   the newly created builder will be written.
///
/// # Returns
///
/// * `FFIResult::StatusOk` on success, and `out_ptr` will be populated with a valid
///   handle.
/// * `FFIResult::NullArgument` if any of the essential pointer arguments
///   (`manifest_array_ptr`, `sorted_hashes_array_ptr`, `out_ptr`) are null.
///
/// # Safety
///
/// The caller is responsible for ensuring the following invariants:
/// * All pointer arguments (`manifest_array_ptr`, `sorted_hashes_array_ptr`,
///   `out_ptr`) in the `args` parameter must be non‑null and point to valid
///   memory.
/// * The `ArchiveBuilderU64` handle returned via `out_ptr` is owned by the
///   C caller and **must** be passed to either `archive_builder_build_u64`
///   to consume it and build the archive, or `archive_builder_free_u64` to
///   deallocate its memory if the build process is aborted.  Failure to do so
///   will result in a memory leak.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_new_u64(
    args: *const ArchiveBuilderArgsU64,
    out_ptr: *mut *mut ArchiveBuilderU64,
) -> FFIResult {
    if args.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let args_int = unsafe{&*args};

    if args_int.manifest_array_ptr.is_null() ||
        args_int.sorted_hashes_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    /*SAFETY: `sorted_hashes_array_ptr` was checked for null above, and
    `ArchiveBuilderArgsU64` requires it to point to `sorted_hashes_len`
    initialised `u64`s that stay valid for this call. The slice is copied
    before returning, so nothing reads the host's array afterwards.*/
    let host_hashes = unsafe{slice::from_raw_parts(
        args_int.sorted_hashes_array_ptr,
        args_int.sorted_hashes_len
    )};
    /*The builder keeps its own copy. The host may free its array once this
    function returns, but the builder reads the hashes again during the
    build.*/
    let sorted_hashes = host_hashes.to_vec();

    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = {
        let parent_get_cb = args_int.get_chunks_cb;
        let parent_free_cb = args_int.free_chunks_cb;
    move |
        hashes: &[u64]
    | -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let user_data = user_data_addr as *mut c_void;
        let get = |out_chunks: *mut FFIChunkDataArray| {
            /*SAFETY: `ArchiveBuilderArgsU64` requires `get_chunks_cb` and
            `user_data` to stay valid for the builder's whole lifetime, and
            this closure only runs while the builder builds. The hash pointer
            covers `hashes.len()` hashes until the callback returns, and
            `out_chunks` points to `copy_host_chunks`' local array, valid for
            writes.*/
            unsafe {
                parent_get_cb(
                    user_data,
                    hashes.as_ptr(),
                    hashes.len(),
                    out_chunks,
                )
            }
        };
        /*SAFETY: `ArchiveBuilderArgsU64` requires `get_chunks_cb` to fill the
        array with readable buffers that stay valid until `free_chunks_cb`
        takes them back, and `free_chunks_cb` to accept that array with
        `user_data`.*/
        unsafe {
            copy_host_chunks(hashes.len(), get, parent_free_cb, user_data)
        }
    }};

    let write_data_closure = {
        let parent_write_cb = args_int.write_comp_data_cb;
    move |data: &[u8]| -> Result<(), SpriteShrinkError> {
        /*SAFETY: `ArchiveBuilderArgsU64` requires the caller to keep
        `write_comp_data_cb` and `user_data` valid for the builder's whole
        lifetime, and this closure only runs while the builder builds.
        `user_data_addr` is that `user_data` pointer, stored as a usize so
        the closure is Send, and is passed back unchanged. `data` is a live
        slice for the length of the call, so its pointer and length describe
        `data.len()` initialised bytes, valid until the callback returns.*/
        let status = unsafe {
            parent_write_cb(
                user_data_addr as *mut c_void,
                data.as_ptr(),
                data.len(),
            )
        };

        Result::from(status)?;
        Ok(())
    }};

    let builder = ArchiveBuilder::new(
        //ser_file_manifest,
        sorted_hashes,
        args_int.total_size,
        get_chunks_closure,
        write_data_closure
    );

    let builder_trait_object: Box<dyn ArchiveBuilderTrait<u64>> = Box::new(
        builder
    );

    let handle = Box::new(builder_trait_object);

    unsafe {
        *out_ptr = Box::into_raw(handle) as *mut ArchiveBuilderU64;
    };

    FFIResult::StatusOk
}

/// Creates and initializes a new `ArchiveBuilder` for u128 hashes.
///
/// This function serves as the entry point for the archive creation process
/// from a C interface. It configures a new builder with the necessary file
/// metadata, a list of all unique chunk hashes, and C callback functions for
/// data retrieval and writing.
///
/// On success, it returns a pointer to an opaque `ArchiveBuilderU128` handle
/// via the `out_ptr` parameter. This handle can then be used with other FFI
/// functions to configure and finally build the archive.
///
/// # Arguments
///
/// * `args`: Struct for all the necessary data for initializing the archive
///   creation process.
/// * `out_ptr`: A pointer to a `*mut ArchiveBuilderU128` where the handle to
///   the newly created builder will be written.
///
/// # Returns
///
/// * `FFIResult::StatusOk` on success, and `out_ptr` will be populated with a valid
///   handle.
/// * `FFIResult::NullArgument` if any of the essential pointer arguments
///   (`manifest_array_ptr`, `sorted_hashes_array_ptr`, `out_ptr`) are null.
///
/// # Safety
///
/// The caller is responsible for ensuring the following invariants:
/// * All pointer arguments (`manifest_array_ptr`, `sorted_hashes_array_ptr`,
///   `out_ptr`) in the `args` parameter must be non‑null and point to valid
///   memory.
/// * The `ArchiveBuilderU128` handle returned via `out_ptr` is owned by the
///   C caller and **must** be passed to either `archive_builder_build_u128`
///   to consume it and build the archive, or `archive_builder_free_u128` to
///   deallocate its memory if the build process is aborted.  Failure to do so
///   will result in a memory leak.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_new_u128(
    args: *const ArchiveBuilderArgsU128,
    out_ptr: *mut *mut ArchiveBuilderU128,
) -> FFIResult {
    if args.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let args_int = unsafe{&*args};

    if args_int.manifest_array_ptr.is_null() ||
        args_int.sorted_hashes_array_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    let hash_byte_slice = unsafe{slice::from_raw_parts(
        args_int.sorted_hashes_array_ptr,
        args_int.sorted_hashes_len * 16
    )};

    let hashes_vec: Vec<u128> = hash_byte_slice
        .chunks_exact(16)
        .map(|byte_chunk| {
            u128::from_le_bytes(byte_chunk.try_into().unwrap())
        })
        .collect();

    let user_data_addr = args_int.user_data as usize;

    let get_chunks_closure = {
        let parent_get_cb = args_int.get_chunks_cb;
        let parent_free_cb = args_int.free_chunks_cb;
    move |
        hashes: &[u128]
    | -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let user_data = user_data_addr as *mut c_void;
        let ffi_hash_bytes: Vec<u8> = hashes
            .iter()
            .flat_map(|h| h.to_le_bytes())
            .collect();
        let get = |out_chunks: *mut FFIChunkDataArray| {
            /*SAFETY: `ArchiveBuilderArgsU128` requires `get_chunks_cb` and
            `user_data` to stay valid for the builder's whole lifetime, and
            this closure only runs while the builder builds. The hash pointer
            covers `hashes.len()` hashes until the callback returns, and
            `out_chunks` points to `copy_host_chunks`' local array, valid for
            writes.*/
            unsafe {
                parent_get_cb(
                    user_data,
                    ffi_hash_bytes.as_ptr(),
                    hashes.len(),
                    out_chunks,
                )
            }
        };
        /*SAFETY: `ArchiveBuilderArgsU128` requires `get_chunks_cb` to fill the
        array with readable buffers that stay valid until `free_chunks_cb`
        takes them back, and `free_chunks_cb` to accept that array with
        `user_data`.*/
        unsafe {
            copy_host_chunks(hashes.len(), get, parent_free_cb, user_data)
        }
    }};

    let write_data_closure = {
        let parent_write_cb = args_int.write_comp_data_cb;
    move |data: &[u8]| -> Result<(), SpriteShrinkError> {
        /*SAFETY: `ArchiveBuilderArgsU128` requires the caller to keep
        `write_comp_data_cb` and `user_data` valid for the builder's whole
        lifetime, and this closure only runs while the builder builds.
        `user_data_addr` is that `user_data` pointer, stored as a usize so
        the closure is Send, and is passed back unchanged. `data` is a live
        slice for the length of the call, so its pointer and length describe
        `data.len()` initialised bytes, valid until the callback returns.*/
        let status = unsafe {
            parent_write_cb(
                user_data_addr as *mut c_void,
                data.as_ptr(),
                data.len(),
            )
        };
        Result::from(status)?;
        Ok(())
    }};

    /*The builder takes ownership of the decoded hashes. They're a new list,
    not the host's array, so nothing else would keep them alive until the
    build.*/
    let builder = ArchiveBuilder::new(
        //ser_file_manifest,
        hashes_vec,
        args_int.total_size,
        get_chunks_closure,
        write_data_closure
    );

    let builder_trait_object: Box<dyn ArchiveBuilderTrait<u128>> = Box::new(
        builder
    );

    let handle = Box::new(builder_trait_object);

    unsafe {
        *out_ptr = Box::into_raw(handle) as *mut ArchiveBuilderU128;
    };

    FFIResult::StatusOk
}

/// Decompresses the chunk in `buffers` with `decompress` and copies the
/// result into the host's output buffer. Errors from `decompress` become
/// the status `on_error` returns.
///
/// # Safety
///
/// Each non-null pointer in `buffers` must point to at least its length in
/// bytes, readable for `comp` and `dict` and writable for `out`, for the
/// whole call. `out` must not overlap `comp` or `dict`.
unsafe fn decompress_into(
    buffers: HostBuffers,
    decompress: impl FnOnce(&[u8], &[u8]) -> Result<Vec<u8>, SpriteShrinkError>,
    on_error: impl FnOnce(SpriteShrinkError) -> FFIResult,
) -> FFIResult {
    if buffers.comp.is_null() || buffers.dict.is_null() || buffers.out.is_null()
    {
        return FFIResult::NullArgument;
    }

    // SAFETY: `comp` is non-null (checked above), and the caller guarantees
    // it points to `comp_len` readable bytes for the whole call. `u8` has
    // alignment 1, so any non-null pointer is aligned.
    let comp = unsafe { slice::from_raw_parts(buffers.comp, buffers.comp_len) };
    // SAFETY: `dict` is non-null (checked above), and the caller guarantees
    // it points to `dict_len` readable bytes for the whole call. `u8` has
    // alignment 1, so any non-null pointer is aligned.
    let dict = unsafe { slice::from_raw_parts(buffers.dict, buffers.dict_len) };

    let data = match decompress(comp, dict) {
        Ok(data) => data,
        Err(err) => return on_error(err),
    };

    // SAFETY: `out` is non-null (checked above), and the caller guarantees
    // it points to `out_len` writable bytes that don't overlap `comp` or
    // `dict`, so this is the only reference to them. `u8` has alignment 1.
    let out = unsafe { slice::from_raw_parts_mut(buffers.out, buffers.out_len) };
    let Some(target) = out.get_mut(..data.len()) else {
        return FFIResult::BufferTooSmall;
    };
    target.copy_from_slice(&data);
    FFIResult::StatusOk
}

/// Reads a `u128` hash from the 16 little-endian bytes at `ptr`, as in
/// `U128Bytes`.
///
/// # Safety
///
/// `ptr` must be non-null and point to 16 readable bytes.
unsafe fn read_u128_hash(ptr: *const u8) -> u128 {
    // SAFETY: the caller guarantees `ptr` points to 16 readable bytes.
    // `[u8; 16]` has alignment 1, so any non-null pointer is aligned.
    let bytes = unsafe { ptr.cast::<[u8; 16]>().read() };
    u128::from_le_bytes(bytes)
}

/// Decompress a chunk of data using a zstd dictionary.
///
/// The output is not checked against the chunk's hash, and chunks are
/// written without a zstd checksum, so most damage to the compressed data
/// decodes to wrong bytes with no error. Use `decompress_chunk_checked_c_u64`
/// or `decompress_chunk_checked_c_u128` unless the host hashes the output
/// itself.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers supplied by
/// callers from other languages (e.g., C). The caller must guarantee:
///
/// * `comp_chunk_data_ptr` points to a valid buffer of at least
///   `comp_chunk_data_len` bytes.
/// * `dictionary_ptr` points to a valid buffer of at least `dictionary_len`
///   bytes.
/// * `data_out_ptr` points to a mutable buffer of at least `data_out_len`
///   bytes, which doesn't overlap the other two.
///
/// If any of the pointers are null, the function returns
/// `FFIResult::NullArgument`.
///
/// # Parameters
///
/// - `comp_chunk_data_ptr`: Pointer to the compressed chunk data.
/// - `comp_chunk_data_len`: Length of the compressed data buffer.
/// - `dictionary_ptr`: Pointer to the dictionary used for decompression.
/// - `dictionary_len`: Length of the dictionary buffer.
/// - `data_out_ptr`: Pointer to the buffer where decompressed data will be
///   written.
/// - `data_out_len`: Capacity of the output buffer. This is known prior to
///   calling this function as it is stored as part of the chunk's metadata.
///
/// # Return Value
///
/// The function returns an `FFIResult` indicating the outcome:
///
/// * `FFIResult::StatusOk`: Decompression succeeded and the output was
///   written.
/// * `FFIResult::NullArgument`: One or more supplied pointers were null.
/// * `FFIResult::BufferTooSmall`: The output buffer is not large enough to
///   hold the decompressed data.
/// * `FFIResult::InternalError`: Decompression failed for an internal reason
///   (e.g., invalid data).
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks a function SPRITE_SHRINK_MUST_USE in the C \
              header only if it has #[must_use]"
)]
#[must_use]
pub unsafe extern "C" fn decompress_chunk_c(
    comp_chunk_data_ptr: *const u8,
    comp_chunk_data_len: usize,
    dictionary_ptr: *const u8,
    dictionary_len: usize,
    data_out_ptr: *mut u8,
    data_out_len: usize
) -> FFIResult {
    let buffers = HostBuffers {
        comp: comp_chunk_data_ptr,
        comp_len: comp_chunk_data_len,
        dict: dictionary_ptr,
        dict_len: dictionary_len,
        out: data_out_ptr,
        out_len: data_out_len,
    };
    // SAFETY: the caller guarantees each non-null pointer points to its
    // length in bytes (readable, or writable for the output) and that the
    // output doesn't overlap the inputs, which is what `decompress_into`
    // needs. It checks for null itself.
    unsafe {
        decompress_into(buffers, decompress_chunk, |_err| {
            FFIResult::InternalError
        })
    }
}

/// Decompresses a chunk like `decompress_chunk_c`, then checks that the
/// output hashes to `expected_hash`, the `u64` hash the chunk is stored
/// under in the archive's chunk index.
///
/// # Safety
///
/// The same as `decompress_chunk_c`.
///
/// # Return Value
///
/// The statuses of `decompress_chunk_c`, except that a failed decompression
/// returns the status for its error (such as `FFIResult::IoError` for data
/// zstd can't decode), and:
///
/// * `FFIResult::VerificationHashMismatch`: The chunk decompressed, but not
///   to the bytes it was stored as. The archive is damaged. Nothing was
///   written to the output buffer.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks a function SPRITE_SHRINK_MUST_USE in the C \
              header only if it has #[must_use]"
)]
#[must_use]
pub unsafe extern "C" fn decompress_chunk_checked_c_u64(
    comp_chunk_data_ptr: *const u8,
    comp_chunk_data_len: usize,
    dictionary_ptr: *const u8,
    dictionary_len: usize,
    expected_hash: u64,
    data_out_ptr: *mut u8,
    data_out_len: usize
) -> FFIResult {
    let buffers = HostBuffers {
        comp: comp_chunk_data_ptr,
        comp_len: comp_chunk_data_len,
        dict: dictionary_ptr,
        dict_len: dictionary_len,
        out: data_out_ptr,
        out_len: data_out_len,
    };
    // SAFETY: the caller guarantees each non-null pointer points to its
    // length in bytes (readable, or writable for the output) and that the
    // output doesn't overlap the inputs, which is what `decompress_into`
    // needs. It checks for null itself.
    unsafe {
        decompress_into(
            buffers,
            |comp, dict| decompress_chunk_checked(comp, dict, expected_hash),
            FFIResult::from,
        )
    }
}

/// Decompresses a chunk like `decompress_chunk_c`, then checks that the
/// output hashes to the `u128` hash the chunk is stored under in the
/// archive's chunk index.
///
/// `expected_hash_ptr` points to the hash's 16 bytes, little-endian as in
/// `U128Bytes`.
///
/// # Safety
///
/// The same as `decompress_chunk_c`, and `expected_hash_ptr` must point to
/// 16 readable bytes.
///
/// # Return Value
///
/// The same as `decompress_chunk_checked_c_u64`. A null
/// `expected_hash_ptr` returns `FFIResult::NullArgument`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks a function SPRITE_SHRINK_MUST_USE in the C \
              header only if it has #[must_use]"
)]
#[must_use]
pub unsafe extern "C" fn decompress_chunk_checked_c_u128(
    comp_chunk_data_ptr: *const u8,
    comp_chunk_data_len: usize,
    dictionary_ptr: *const u8,
    dictionary_len: usize,
    expected_hash_ptr: *const u8,
    data_out_ptr: *mut u8,
    data_out_len: usize
) -> FFIResult {
    if expected_hash_ptr.is_null() {
        return FFIResult::NullArgument;
    }
    // SAFETY: `expected_hash_ptr` is non-null (checked above), and the
    // caller guarantees it points to 16 readable bytes.
    let expected_hash = unsafe { read_u128_hash(expected_hash_ptr) };

    let buffers = HostBuffers {
        comp: comp_chunk_data_ptr,
        comp_len: comp_chunk_data_len,
        dict: dictionary_ptr,
        dict_len: dictionary_len,
        out: data_out_ptr,
        out_len: data_out_len,
    };
    // SAFETY: the caller guarantees each non-null pointer points to its
    // length in bytes (readable, or writable for the output) and that the
    // output doesn't overlap the inputs, which is what `decompress_into`
    // needs. It checks for null itself.
    unsafe {
        decompress_into(
            buffers,
            |comp, dict| decompress_chunk_checked(comp, dict, expected_hash),
            FFIResult::from,
        )
    }
}

/// A generic helper to safely update the internal state of an
/// `ArchiveBuilder`.
///
/// This private function centralizes the logic for modifying an
/// `ArchiveBuilder` instance that is managed behind an opaque FFI handle. It
/// takes a raw pointer to the handle, safely casts it back into a mutable
/// reference to the underlying Rust trait object, and then executes a provided
/// closure to perform the update.
///
/// This approach avoids duplicating the unsafe pointer casting and
/// null-checking logic in every public FFI setter function
/// (e.g., `set_compression_level`, `set_dictionary_size`, etc.).
///
/// # Type Parameters
///
/// * `T`: The opaque FFI handle type (e.g., `ArchiveBuilderU64`), which must
///   implement the `FfiBuilderHandle` trait to associate it with a hash type.
///
/// # Arguments
///
/// * `builder_handle`: The raw, opaque pointer to the `ArchiveBuilder` handle
///   that was originally created by `archive_builder_new_*` and passed from C.
/// * `update_fn`: A closure that takes a mutable reference to the builder's
///   trait object and applies a specific configuration change.
///
/// # Returns
///
/// * `FFIResult::StatusOk` if the `builder_handle` is valid and the update was
///   applied.
/// * `FFIResult::NullArgument` if the provided `builder_handle` is a null
///   pointer.
fn update_builder_internal<T>(
    builder_handle: *mut T,
    update_fn: impl FnOnce(&mut Box<dyn ArchiveBuilderTrait<T::HashType>>),
) -> FFIResult
where
    T: FFIBuilderHandle,
{
    let builder = unsafe{
        (builder_handle as *mut Box<dyn ArchiveBuilderTrait<T::HashType>>)
            .as_mut()
    };
    if let Some(builder) = builder {
        update_fn(builder);
        FFIResult::StatusOk
    } else {
        FFIResult::NullArgument
    }
}

/*
/// Sets the compression algorithm on the ArchiveBuilder for a u64 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u64`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_compression_algorithm_u64(
    builder_handle: *mut ArchiveBuilderU64,
    code: u16
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_compression_algorithm(code)
    )
}


/// Sets the compression algorithm on the ArchiveBuilder for a u128 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u128`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_compression_algorithm_u128(
    builder_handle: *mut ArchiveBuilderU128,
    code: u16
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_compression_algorithm(code)
    )
}
*/

/// Sets the compression level on the ArchiveBuilder for a u64 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u64`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_compression_level_u64(
    builder_handle: *mut ArchiveBuilderU64,
    level: i32,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_compression_level(level)
    )
}

/// Sets the compression level on the ArchiveBuilder for a u128 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u128`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_compression_level_u128(
    builder_handle: *mut ArchiveBuilderU128,
    level: i32,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_compression_level(level)
    )
}

/// Sets the dictionary size on the ArchiveBuilder for a u64 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u64`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_dictionary_size_u64(
    builder_handle: *mut ArchiveBuilderU64,
    size: u64,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_dictionary_size(size)
    )
}

/// Sets the dictionary size on the ArchiveBuilder for a u128 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u128`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_dictionary_size_u128(
    builder_handle: *mut ArchiveBuilderU128,
    size: u64,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_dictionary_size(size)
    )
}

/// Sets the worker count on the ArchiveBuilder for a u64 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u64`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_worker_count_u64(
    builder_handle: *mut ArchiveBuilderU64,
    threads: usize,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_worker_threads(threads)
    )
}

/// Sets the worker count on the ArchiveBuilder for a u128 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u128`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_worker_count_u128(
    builder_handle: *mut ArchiveBuilderU128,
    threads: usize,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_worker_threads(threads)
    )
}

/// Sets the optimize dictionary flag on the ArchiveBuilder for a u64 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u64`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_optimize_dictionary_u64(
    builder_handle: *mut ArchiveBuilderU64,
    optimize: bool,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_optimize_dictionary(optimize)
    )
}

/// Sets the optimize dictionary flag on the ArchiveBuilder for a u128 hash.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from
/// `archive_builder_new_u128`.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_optimize_dictionary_u128(
    builder_handle: *mut ArchiveBuilderU128,
    optimize: bool,
) -> FFIResult {
    update_builder_internal(
        builder_handle,
        |b| b.set_optimize_dictionary(optimize)
    )
}

/// A generic helper to attach a C progress callback to an `ArchiveBuilder`.
///
/// This internal function provides the logic for setting a C-style progress
/// reporting callback on an `ArchiveBuilder` instance that is managed behind
/// an opaque FFI handle. It safely casts the handle back to a mutable Rust
/// trait object and calls the builder's `with_c_progress` method to register
/// the C function pointer and `user_data` context.
///
/// # Type Parameters
///
/// * `T`: The opaque FFI handle type (e.g., `ArchiveBuilderU64`), which must
///   implement the `FfiBuilderHandle` trait.
///
/// # Arguments
///
/// * `builder_handle`: The raw, opaque pointer to the `ArchiveBuilder` handle
///   that was originally created by `archive_builder_new_*`.
/// * `callback`: A C function pointer that the builder will call to report
///   progress updates during the archiving process.
/// * `user_data`: An opaque `void` pointer that will be passed back to the
///   `callback` function, allowing the C side to maintain state.
///
/// # Returns
///
/// * `FFIResult::StatusOk` if the `builder_handle` is valid and the callback was set.
/// * `FFIResult::NullArgument` if the provided `builder_handle` is a null
///   pointer.
///
/// # Safety
///
/// The public FFI function that calls this helper must guarantee that:
/// - `builder_handle` is a valid, non-null pointer.
/// - The `callback` function pointer is valid for the lifetime of the builder.
/// - The `user_data` pointer is valid for the lifetime of the builder and any
///   invocations of the `callback`.
fn ffi_builder_progress_internal<T>(
    builder_handle: *mut T,
    callback: extern "C" fn(FFIProgress, *mut c_void),
    user_data: *mut c_void,
) -> FFIResult
where
    T: FFIBuilderHandle,
{
    unsafe {
        let handle_box_ptr = builder_handle as *mut Box<
            dyn ArchiveBuilderTrait<T::HashType>
        >;

        if let Some(builder_box) = handle_box_ptr.as_mut() {
            let builder = builder_box.as_mut();
            builder.with_c_progress(callback, user_data);
            FFIResult::StatusOk
        } else {
            FFIResult::NullArgument
        }
    }
}

/// Sets the progress callback for the ArchiveBuilder for a u64 hash.
///
/// # Safety
/// The `builder_handle` must be a valid pointer returned from
/// `archive_builder_new_u64`. The `callback` function pointer must be
/// valid for the lifetime of the builder.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_c_progress_u64(
    builder_handle: *mut ArchiveBuilderU64,
    callback: extern "C" fn(FFIProgress, *mut c_void),
    user_data: *mut c_void,
) -> FFIResult {
    ffi_builder_progress_internal(
        builder_handle,
        callback,
        user_data
    )
}

/// Sets the progress callback for the ArchiveBuilder for a u128 hash.
///
/// # Safety
/// The `builder_handle` must be a valid pointer returned from
/// `archive_builder_new_u128`. The `callback` function pointer must be
/// valid for the lifetime of the builder.
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_set_c_progress_u128(
    builder_handle: *mut ArchiveBuilderU128,
    callback: extern "C" fn(FFIProgress, *mut c_void),
    user_data: *mut c_void,
) -> FFIResult {
    ffi_builder_progress_internal(
        builder_handle,
        callback,
        user_data
    )
}

/// A generic helper to process the result of an `ArchiveBuilder::build` call.
///
/// This internal function takes the `Result` from a build operation and
/// transforms it into a C-compatible format.
///
/// If the build was successful (`Ok`), it takes the `Vec<u8>` containing the
/// archive data, allocates an `FFIArchiveData` struct on the heap to hold a
/// pointer to this data, and transfers ownership of both the struct and the
/// underlying byte buffer to the C caller via the `out_ptr`.
///
/// If the build failed (`Err`), it converts the `SpriteShrinkError` into an
/// appropriate `FFIResult` status code to be returned to the caller.
///
/// # Arguments
///
/// * `result`: The `Result` returned from the `ArchiveBuilder::build()`
///   method.
/// * `out_ptr`: A raw pointer to a location where the pointer to the newly
///   allocated `FFIArchiveData` struct will be written on success.
///
/// # Returns
///
/// * `FFIResult::StatusOk` on success, with `out_ptr` pointing to the
///   `FFIArchiveData` struct.
/// * An FFI-safe error code corresponding to the `SpriteShrinkError` on
///   failure.
///
/// # Safety
///
/// The caller must uphold the following safety invariants:
/// - The `out_ptr` must be a valid, non-null pointer.
/// - If the function returns `FFIResult::StatusOk`, the C caller takes ownership of
///   the `FFIArchiveData` pointer written to `out_ptr`. This pointer must
///   be passed to `archive_data_free` to deallocate the struct and the
///   underlying byte buffer, preventing a memory leak.
unsafe fn handle_build_result(
    result: Result<CompressionResult, SpriteShrinkError>,
    out_ptr: *mut *mut FFICompData,
) -> FFIResult {
    match result {
        Ok(mut comp_data) => {
            let dictionary_data_ptr = comp_data.dictionary.as_mut_ptr();
            let dictionary_data_len = comp_data.dictionary_size as usize;
            let dictionary_data_cap = comp_data.dictionary.capacity();

            let enc_chunk_index_data_ptr = comp_data.enc_chunk_index
                .as_mut_ptr();
            let enc_chunk_index_data_len = comp_data.enc_chunk_index_size as usize;
            let enc_chunk_index_data_cap = comp_data.enc_chunk_index
                .capacity();

            // Give ownership to C parent application
            std::mem::forget(comp_data);

            let output = Box::new(FFICompData {
                dictionary_data_ptr,
                dictionary_data_len,
                dictionary_data_cap,
                enc_chunk_index_data_ptr,
                enc_chunk_index_data_len,
                enc_chunk_index_data_cap
            });
            unsafe {
                    *out_ptr = Box::into_raw(output);
                }
            FFIResult::StatusOk
        }
        Err(e) => e.into(),
    }
}

/// Consumes the u64 builder, builds the archive, and returns the data via
/// an out-parameter for a u64 hash.
///
/// # Safety
/// - `builder_handle` must be a valid pointer from
///   `archive_builder_new_u64`.
/// - `out_data` must be a valid pointer to an `*mut FFIArchiveData`.
/// - This function consumes the builder; `builder_handle` is invalid
///   after this call.
/// - The pointer returned via `out_data` must be freed with
///   `archive_data_free`
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_build_u64(
    builder_handle: *mut ArchiveBuilderU64,
    out_ptr: *mut *mut FFICompData,
) -> FFIResult {
    if builder_handle.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let handle_box = unsafe {
        Box::from_raw(builder_handle as *mut Box<
            dyn ArchiveBuilderTrait<u64>
        >)
    };
    let builder = *handle_box;
    let result = builder.build();

    unsafe {
        handle_build_result(result, out_ptr)
    }
}

/// Consumes the u128 builder, builds the archive, and returns the data via
/// an out-parameter for a u128 hash.
///
/// # Safety
/// - `builder_handle` must be a valid pointer from
///   `archive_builder_new_u128`.
/// - `out_data` must be a valid pointer to an `*mut FFIArchiveData`.
/// - This function consumes the builder; `builder_handle` is invalid
///   after this call.
/// - The pointer returned via `out_data` must be freed with
///   `archive_data_free`
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_build_u128(
    builder_handle: *mut ArchiveBuilderU128,
    out_ptr: *mut *mut FFICompData,
) -> FFIResult {
    if builder_handle.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    let handle_box = unsafe {
        Box::from_raw(builder_handle as *mut Box<
            dyn ArchiveBuilderTrait<u128>
        >)
    };
    let builder = *handle_box;
    let result = builder.build();

    unsafe {
        handle_build_result(result, out_ptr)
    }
}

/// A generic helper to deallocate an `ArchiveBuilder` from an FFI handle.
///
/// This private function contains the core logic for safely freeing the memory
/// of an `ArchiveBuilder` instance that is owned by the C caller. It
/// reconstructs a `Box` from the raw pointer and allows Rust's memory manager
/// to drop it, preventing a memory leak.
///
/// This function is intended to be called when the archive creation process is
/// aborted and the builder will not be consumed by a `build` function.
///
/// # Type Parameters
///
/// * `T`: The opaque FFI handle type (e.g., `ArchiveBuilderU64`), which must
///   implement the `FfiBuilderHandle` trait.
///
/// # Arguments
///
/// * `builder_handle`: The raw, opaque pointer to the `ArchiveBuilder` handle
///   to be freed.
///
/// # Safety
///
/// The caller of this function must guarantee that:
/// - The `builder_handle` is a valid pointer that was originally returned from
///   a successful call to `archive_builder_new_*`.
/// - The handle has not already been freed or passed to an
///   `archive_builder_build_*` function, as both actions invalidate the
///   pointer.
/// - The handle must not be used again after this function is called.
///
/// Passing a null, previously freed, or otherwise invalid pointer will result
/// in undefined behavior.
fn archive_builder_free_internal<T>(
    builder_handle: *mut T
)
where
    T: FFIBuilderHandle,
{
    unsafe{
        if !builder_handle.is_null() {
            let _ = Box::from_raw(
                builder_handle as *mut Box<dyn ArchiveBuilderTrait<
                T::HashType
                >>
            );
        }
    }
}

/// Frees the ArchiveBuilder if the build is never run for u64 hashes.
///
/// # Safety
/// The `builder_handle` must be a valid pointer from
/// `archive_builder_new_u64` that has not been passed to
/// `archive_builder_build_u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_free_u64(
    builder_handle: *mut ArchiveBuilderU64
) {
    archive_builder_free_internal(
        builder_handle
    )
}

/// Frees the ArchiveBuilder if the build is never run for u128 hashes.
///
/// # Safety
/// The `builder_handle` must be a valid pointer from
/// `archive_builder_new_u128` that has not been passed to
/// `archive_builder_build_u128`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_free_u128(
    builder_handle: *mut ArchiveBuilderU128
) {
    archive_builder_free_internal(
        builder_handle
    )
}

/// Frees the data returned by a successful build.
///
/// # Safety
/// The `archive_data_ptr` must be a valid pointer from
/// `archive_builder_build`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_data_free(
    comp_data_ptr: *mut FFICompData)
{
    unsafe{
        if !comp_data_ptr.is_null() {
            let comp_box = Box::from_raw(comp_data_ptr);
            let _dict = Vec::from_raw_parts(
                comp_box.dictionary_data_ptr,
                comp_box.dictionary_data_len,
                comp_box.dictionary_data_cap
            );

            let _enc_chunk_index = Vec::from_raw_parts(
                comp_box.enc_chunk_index_data_ptr,
                comp_box.enc_chunk_index_data_len,
                comp_box.enc_chunk_index_data_cap
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;

    /// Buffers over `comp`, `dict` and `out`.
    fn buffers(comp: &[u8], dict: &[u8], out: &mut [u8]) -> HostBuffers {
        HostBuffers {
            comp: comp.as_ptr(),
            comp_len: comp.len(),
            dict: dict.as_ptr(),
            dict_len: dict.len(),
            out: out.as_mut_ptr(),
            out_len: out.len(),
        }
    }

    /// A stand-in for zstd, which Miri can't run: returns the compressed
    /// bytes followed by the dictionary.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "it stands in for a decompress function, which returns a \
                  Result"
    )]
    fn concat(comp: &[u8], dict: &[u8]) -> Result<Vec<u8>, SpriteShrinkError> {
        Ok([comp, dict].concat())
    }

    #[test]
    fn decompressed_bytes_are_copied_to_the_front_of_the_output() {
        let mut out = [0xEE_u8; 6];
        // SAFETY: every pointer comes from a live local slice of its stated
        // length, and `out` is a separate array.
        let status = unsafe {
            decompress_into(
                buffers(b"abc", b"d", &mut out),
                concat,
                |_err| FFIResult::InternalError,
            )
        };
        assert!(matches!(status, FFIResult::StatusOk));
        assert_eq!(&out, b"abcd\xEE\xEE");
    }

    #[test]
    fn an_output_buffer_too_small_is_left_untouched() {
        let mut out = [0xEE_u8; 3];
        // SAFETY: every pointer comes from a live local slice of its stated
        // length, and `out` is a separate array.
        let status = unsafe {
            decompress_into(
                buffers(b"abc", b"d", &mut out),
                concat,
                |_err| FFIResult::InternalError,
            )
        };
        assert!(matches!(status, FFIResult::BufferTooSmall));
        assert_eq!(out, [0xEE; 3]);
    }

    #[test]
    fn a_failed_decompress_returns_its_status_and_writes_nothing() {
        let mut out = [0xEE_u8; 4];
        // SAFETY: every pointer comes from a live local slice of its stated
        // length, and `out` is a separate array.
        let status = unsafe {
            decompress_into(
                buffers(b"abc", b"d", &mut out),
                |_comp, _dict| Err(SpriteShrinkError::Cancelled),
                FFIResult::from,
            )
        };
        assert!(matches!(status, FFIResult::StatusCancelled));
        assert_eq!(out, [0xEE; 4]);
    }

    #[test]
    fn empty_buffers_are_read_as_empty_slices() {
        // An archive without a dictionary has an empty one, and an empty
        // chunk decompresses to nothing.
        let mut out = [0xEE_u8; 0];
        // SAFETY: every pointer comes from a live local slice; all three
        // are empty, so no bytes are read or written.
        let status = unsafe {
            decompress_into(buffers(b"", b"", &mut out), concat, |_err| {
                FFIResult::InternalError
            })
        };
        assert!(matches!(status, FFIResult::StatusOk));
    }

    #[test]
    fn each_null_buffer_is_rejected_before_decompressing() {
        let mut out = [0_u8; 4];
        let valid = buffers(b"abc", b"d", &mut out);
        let nulled = [
            HostBuffers { comp: ptr::null(), ..valid },
            HostBuffers { dict: ptr::null(), ..valid },
            HostBuffers { out: ptr::null_mut(), ..valid },
        ];
        for buffers in nulled {
            // SAFETY: one pointer is null, which `decompress_into` checks
            // before reading anything; the others point into live locals.
            let status = unsafe {
                decompress_into(
                    buffers,
                    |_comp, _dict| panic!("decompressed with a null buffer"),
                    |_err| FFIResult::InternalError,
                )
            };
            assert!(matches!(status, FFIResult::NullArgument));
        }
    }

    #[test]
    fn a_u128_hash_is_read_little_endian_from_any_alignment() {
        let hash = 0x0011_2233_4455_6677_8899_AABB_CCDD_EEFF_u128;
        // One byte in front, so the hash starts at an odd address.
        let mut bytes = [0_u8; 17];
        bytes[1..].copy_from_slice(&hash.to_le_bytes());
        // SAFETY: `bytes[1..]` is 16 readable bytes in a live local.
        let read = unsafe { read_u128_hash(bytes[1..].as_ptr()) };
        assert_eq!(read, hash);
    }
}
