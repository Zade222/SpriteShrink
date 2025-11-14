//! FFI layer for the archive building process.
//!
//! This module provides a C-compatible interface for creating, configuring,
//! and building sprite-shrink archives. It handles the conversion between
//! Rust and C data types, ensuring memory safety across the FFI boundary.

use std::{
    os::raw::c_void,
    slice,
};

use crate::archive::{ArchiveBuilder};
use crate::ffi::Hash128;
use crate::ffi::ffi_error_handling::{
    FFICallbackStatus, FFIResult
};
use crate::ffi::ffi_types::{
    ArchiveBuilderU64, ArchiveBuilderU128,
    ArchiveBuilderArgsU64, ArchiveBuilderArgsU128,
    BuilderCallbacks, FFIArchiveData, FFIChunkDataArray,
    FFIProgress
};
use crate::lib_error_handling::{
    IsCancelled, SpriteShrinkError
};
use crate::lib_structs::{FileManifestParent};

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
    fn set_compression_algorithm(&mut self, algorithm: u16);
    fn set_compression_level(&mut self, level: i32);
    fn set_dictionary_size(&mut self, size: u64);
    fn with_c_progress(&mut self, callback: extern "C" fn(FFIProgress, *mut c_void), user_data: *mut c_void);
    fn set_worker_threads(&mut self, threads: usize);
    fn set_optimize_dictionary(&mut self, optimize: bool);

    //This method consumes the builder
    fn build(self: Box<Self>) -> Result<Vec<u8>, SpriteShrinkError>;
}

impl<'a, E, H, R, W> ArchiveBuilderTrait<H> for ArchiveBuilder<'a, E, H, R, W>
where
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    H: Copy + std::fmt::Debug + Eq + std::hash::Hash + serde::Serialize + Send + Sync + 'static + std::fmt::Display + Ord,
    R: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    W: FnMut(&[u8], bool) -> Result<(), E> + Send + Sync + 'static,
{
    fn set_compression_algorithm(&mut self, algorithm: u16) {
        self.compression_algorithm(algorithm);
    }
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
    fn with_c_progress(&mut self, callback: extern "C" fn(FFIProgress, *mut c_void), user_data: *mut c_void) {
        self.with_c_progress(callback, user_data);
    }


    fn build(self: Box<Self>) -> Result<Vec<u8>, SpriteShrinkError> {
        (*self).build()
    }
}

/// Creates Rust closures that wrap C function pointers for the ArchiveBuilder
/// for u64 hahes.
///
/// This function acts as a bridge between the C-style callback mechanism
/// (function pointers and an opaque `user_data` pointer) and the idiomatic
/// Rust API of `ArchiveBuilder`, which expects closures. It captures the C
/// callbacks and the `user_data` pointer into two closures that can be passed
/// to the builder.
///
/// # Arguments
///
/// * `user_data`: An opaque pointer provided by the C caller, which is passed
///   back to the C callbacks to allow them to access their own context or
///   state.
/// * `get_chunks_cb`: callback that receives the user data, an array of hash
///   values, the length of that array, and an output pointer that will be
///   populated with a `FFIChunkDataArray`.
/// * `free_chunks_cb` – callback to free the array returned by
///   `get_chunks_cb`. It receives the same `user_data` and the
///   `FFIChunkDataArray` that was just returned.
/// * `write_comp_data_cb`: A C function pointer that the `ArchiveBuilder` will
///   call via the returned closure to write out the final compressed archive
///   data.
///
/// # Returns
///
/// A tuple containing two closures:
/// 1. A `get_chunks_closure` that can be called with a slice of hashes
///    (`&[u64]`) and returns the corresponding chunk data (`Vec<Vec<u8>>`).
/// 2. A `write_data_closure` that can be called with a slice of bytes
///    (`&[u8]`) and a boolean flush flag to write data.
///
/// # Safety
///
/// The closures created by this function contain `unsafe` blocks that
/// dereference raw pointers and call C functions. The caller of the public FFI
/// function that uses this helper must guarantee that:
/// - The `user_data` pointer is valid for the entire lifetime of the
///   `ArchiveBuilder`.
/// - The `get_chunks_cb` and `write_comp_data_cb` function pointers are valid
///   and point to functions with the correct signatures.
fn setup_builder_closures_u64(
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const u64,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
    free_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        chunks: FFIChunkDataArray
    ),
    write_comp_data_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        data: *const u8,
        data_len: usize,
        is_flush: bool
    ) -> FFICallbackStatus,
) -> BuilderCallbacks<u64> {
    let user_data_addr = user_data as usize;

    let get_chunks_closure = move |
        hashes: &[u64]
    | -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let mut ffi_chunks_array = FFIChunkDataArray {
            ptr: std::ptr::null_mut(), len: 0, cap: 0
        };
        let status = unsafe {
            get_chunks_cb(
                user_data_addr as *mut c_void,
                hashes.as_ptr(),
                hashes.len(),
                &mut ffi_chunks_array
            )
        };
        Result::from(status)?;

        if ffi_chunks_array.ptr.is_null() {
            return Ok(Vec::new());
        }

        let chunks = unsafe{
            let ffi_chunks_slice = slice::from_raw_parts(
                ffi_chunks_array.ptr,
                ffi_chunks_array.len
            );

            let rust_chunks: Vec<Vec<u8>> = ffi_chunks_slice.iter().map(|c| {
                let c_chunk_slice = slice::from_raw_parts(c.ptr, c.len);
                c_chunk_slice.to_vec()
            }).collect();

            free_chunks_cb(user_data_addr as *mut c_void, ffi_chunks_array);

            rust_chunks
        };

        Ok(chunks)
    };

    let write_data_closure = move |
        data: &[u8],
        flush: bool
    | -> Result<(), SpriteShrinkError> {
        let status = unsafe {
            write_comp_data_cb(
                user_data_addr as *mut c_void,
                data.as_ptr(),
                data.len(),
                flush,
            )
        };

        Result::from(status)?;
        Ok(())
    };

    BuilderCallbacks {
        get_chunks: Box::new(get_chunks_closure),
        write_data: Box::new(write_data_closure),
    }
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
/// * `FFIResult::Ok` on success, and `out_ptr` will be populated with a valid
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

    let sorted_hashes = unsafe{slice::from_raw_parts(
        args_int.sorted_hashes_array_ptr,
        args_int.sorted_hashes_len
    )};

    let ffi_manifests = unsafe {
        slice::from_raw_parts(
            args_int.manifest_array_ptr,
            args_int.manifest_len
        )
    };

    let ser_file_manifest: Vec<FileManifestParent<u64>> = ffi_manifests
    .iter()
    .map(FileManifestParent::<u64>::from)
    .collect();

    let callbacks = setup_builder_closures_u64(
        args_int.user_data,
        args_int.get_chunks_cb,
        args_int.free_chunks_cb,
        args_int.write_comp_data_cb
    );

    let builder = ArchiveBuilder::new(
        ser_file_manifest,
        sorted_hashes,
        args_int.file_count,
        1,
        args_int.total_size,
        callbacks.get_chunks,
        callbacks.write_data
    );

    let builder_trait_object: Box<dyn ArchiveBuilderTrait<u64>> = Box::new(
        builder
    );

    let handle = Box::new(builder_trait_object);

    unsafe {
        *out_ptr = Box::into_raw(handle) as *mut ArchiveBuilderU64;
    };

    FFIResult::Ok
}

/// Creates Rust closures that wrap C function pointers for the ArchiveBuilder
/// for u128 hahes.
///
/// This function acts as a bridge between the C-style callback mechanism
/// (function pointers and an opaque `user_data` pointer) and the idiomatic
/// Rust API of `ArchiveBuilder`, which expects closures. It captures the C
/// callbacks and the `user_data` pointer into two closures that can be passed
/// to the builder.
///
/// # Arguments
///
/// * `user_data`: An opaque pointer provided by the C caller, which is passed
///   back to the C callbacks to allow them to access their own context or
///   state.
/// * `get_chunks_cb`: callback that receives the user data, an array of hash
///   values, the length of that array, and an output pointer that will be
///   populated with a `FFIChunkDataArray`.
/// * `free_chunks_cb` – callback to free the array returned by
///   `get_chunks_cb`. It receives the same `user_data` and the
///   `FFIChunkDataArray` that was just returned.
/// * `write_comp_data_cb`: A C function pointer that the `ArchiveBuilder` will
///   call via the returned closure to write out the final compressed archive
///   data.
///
/// # Returns
///
/// A tuple containing two closures:
/// 1. A `get_chunks_closure` that can be called with a slice of hashes
///    (`&[u128]`) and returns the corresponding chunk data (`Vec<Vec<u8>>`).
/// 2. A `write_data_closure` that can be called with a slice of bytes
///    (`&[u8]`) and a boolean flush flag to write data.
///
/// # Safety
///
/// The closures created by this function contain `unsafe` blocks that
/// dereference raw pointers and call C functions. The caller of the public FFI
/// function that uses this helper must guarantee that:
/// - The `user_data` pointer is valid for the entire lifetime of the
///   `ArchiveBuilder`.
/// - The `get_chunks_cb` and `write_comp_data_cb` function pointers are valid
///   and point to functions with the correct signatures.
fn setup_builder_closures_u128(
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        hashes: *const Hash128,
        hashes_len: usize,
        out_chunks: *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
    free_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        chunks: FFIChunkDataArray
    ),
    write_comp_data_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        data: *const u8,
        data_len: usize,
        is_flush: bool
    ) -> FFICallbackStatus,
) -> BuilderCallbacks<u128> {
    let user_data_addr = user_data as usize;

    let get_chunks_closure = move |
        hashes: &[u128]
    | -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let ffi_hashes: Vec<Hash128> = hashes.iter().map(|&h| h.into()).collect();

        let mut ffi_chunks_array = FFIChunkDataArray {
            ptr: std::ptr::null_mut(), len: 0, cap: 0
        };

        let status = unsafe {
            get_chunks_cb(
                user_data_addr as *mut c_void,
                ffi_hashes.as_ptr(),
                ffi_hashes.len(),
                &mut ffi_chunks_array
            )
        };
        Result::from(status)?;

        if ffi_chunks_array.ptr.is_null() {
            return Ok(Vec::new());
        }

        let chunks = unsafe {
            let ffi_chunks_slice = slice::from_raw_parts(
                ffi_chunks_array.ptr,
                ffi_chunks_array.len
            );

            let rust_chunks: Vec<Vec<u8>> = ffi_chunks_slice.iter().map(|c| {
                let c_chunk_slice = slice::from_raw_parts(c.ptr, c.len);
                c_chunk_slice.to_vec()
            }).collect();

            free_chunks_cb(user_data_addr as *mut c_void, ffi_chunks_array);

            rust_chunks
        };

        Ok(chunks)
    };

    let write_data_closure = move |
        data: &[u8],
        flush: bool
    | -> Result<(), SpriteShrinkError> {
        let status = unsafe {
            write_comp_data_cb(
                user_data_addr as *mut c_void,
                data.as_ptr(),
                data.len(),
                flush,
            )
        };
        Result::from(status)?;
        Ok(())
    };

    BuilderCallbacks {
        get_chunks: Box::new(get_chunks_closure),
        write_data: Box::new(write_data_closure),
    }
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
/// * `FFIResult::Ok` on success, and `out_ptr` will be populated with a valid
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

    /*The use of the struct along with the vector conversion allows for
     * portability between both little and big endian systems.
     */
    let sorted_hashes_ffi = unsafe{slice::from_raw_parts(
        args_int.sorted_hashes_array_ptr,
        args_int.sorted_hashes_len
    )};
    let sorted_hashes: Vec<u128> = sorted_hashes_ffi
        .iter()
        .map(|&ffi_hash| u128::from(ffi_hash))
        .collect();

    let ffi_manifests = unsafe {
        slice::from_raw_parts(
            args_int.manifest_array_ptr,
            args_int.manifest_len
        )
    };

    let ser_file_manifest: Vec<FileManifestParent<u128>> = ffi_manifests
        .iter()
        .map(FileManifestParent::<u128>::from)
        .collect();

    let callbacks = setup_builder_closures_u128(
        args_int.user_data,
        args_int.get_chunks_cb,
        args_int.free_chunks_cb,
        args_int.write_comp_data_cb
    );

    let builder = ArchiveBuilder::new(
        ser_file_manifest,
        sorted_hashes.as_slice(),
        args_int.file_count,
        2,
        args_int.total_size,
        callbacks.get_chunks,
        callbacks.write_data
    );

    let builder_trait_object: Box<dyn ArchiveBuilderTrait<u128>> = Box::new(
        builder
    );

    let handle = Box::new(builder_trait_object);

    unsafe {
        *out_ptr = Box::into_raw(handle) as *mut ArchiveBuilderU128;
    };

    FFIResult::Ok
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
/// * `FFIResult::Ok` if the `builder_handle` is valid and the update was
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
        FFIResult::Ok
    } else {
        FFIResult::NullArgument
    }
}

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
/// reporting callback on an `ArchiveBuilder` instance that is managed behind an
/// opaque FFI handle. It safely casts the handle back to a mutable Rust trait
/// object and calls the builder's `with_c_progress` method to register the
/// C function pointer and `user_data` context.
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
/// * `FFIResult::Ok` if the `builder_handle` is valid and the callback was set.
/// * `FFIResult::NullArgument` if the provided `builder_handle` is a null pointer.
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
            FFIResult::Ok
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
/// * `FFIResult::Ok` on success, with `out_ptr` pointing to the
///   `FFIArchiveData` struct.
/// * An FFI-safe error code corresponding to the `SpriteShrinkError` on
///   failure.
///
/// # Safety
///
/// The caller must uphold the following safety invariants:
/// - The `out_ptr` must be a valid, non-null pointer.
/// - If the function returns `FFIResult::Ok`, the C caller takes ownership of
///   the `FFIArchiveData` pointer written to `out_ptr`. This pointer **must**
///   be passed to `archive_data_free` to deallocate the struct and the
///   underlying byte buffer, preventing a memory leak.
unsafe fn handle_build_result(
    result: Result<Vec<u8>, SpriteShrinkError>,
    out_ptr: *mut *mut FFIArchiveData,
) -> FFIResult {
    match result {
        Ok(mut archive_data) => {
            let data_ptr = archive_data.as_mut_ptr();
            let data_len = archive_data.len();
            let data_cap = archive_data.capacity();
            std::mem::forget(archive_data); // Give ownership to C caller

            let output = Box::new(FFIArchiveData {
                data: data_ptr,
                data_len,
                data_cap,
            });
            unsafe {
                    *out_ptr = Box::into_raw(output);
                }
            FFIResult::Ok
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
///   `archive_data_free_u64`
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_build_u64(
    builder_handle: *mut ArchiveBuilderU64,
    out_ptr: *mut *mut FFIArchiveData,
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
///   `archive_data_free_u128`
#[unsafe(no_mangle)]
#[allow(clippy::double_must_use)]
#[must_use]
pub unsafe extern "C" fn archive_builder_build_u128(
    builder_handle: *mut ArchiveBuilderU128,
    out_ptr: *mut *mut FFIArchiveData,
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
    archive_data_ptr: *mut FFIArchiveData)
{
    unsafe{
        if !archive_data_ptr.is_null() {
            let archive_box = Box::from_raw(archive_data_ptr);
            let _ = Vec::from_raw_parts(
                archive_box.data,
                archive_box.data_len,
                archive_box.data_cap,
            );
        }
    }
}
