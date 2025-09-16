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
use crate::ffi::{
    FFIResult
};
use crate::ffi::ffi_types::{
    ArchiveBuilderU64, ArchiveBuilderU128,
    FFIArchiveData, FFIChunkDataArray,
    FFIFileManifestParentU64, FFIFileManifestParentU128,
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

/// Creates Rust closures that wrap C function pointers for the ArchiveBuilder.
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
/// * `get_chunks_cb`: A C function pointer that the `ArchiveBuilder` will call
///   via the returned closure to request uncompressed chunk data.
/// * `write_comp_data_cb`: A C function pointer that the `ArchiveBuilder` will
///   call via the returned closure to write out the final compressed archive 
///   data.
///
/// # Returns
///
/// A tuple containing two closures:
/// 1. A `get_chunks_closure` that can be called with a slice of hashes 
///    (`&[H]`) and returns the corresponding chunk data (`Vec<Vec<u8>>`).
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
fn setup_builder_closures<E, H: 'static>(
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void, 
        hashes: *const H, 
        hashes_len: usize
    ) -> FFIChunkDataArray,
    write_comp_data_cb: unsafe extern "C" fn(
        user_data: *mut c_void, 
        data: *const u8, 
        data_len: usize, 
        is_flush: bool
    ),
) -> (
    impl Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    impl FnMut(&[u8], bool) -> Result<(), E> + Send + Sync + 'static,
) {
    let user_data_addr = user_data as usize;

    let get_chunks_closure = move |hashes: &[H]| -> Result<Vec<Vec<u8>>, E> {
        let ffi_chunks_array = unsafe {
            get_chunks_cb(
                user_data_addr as *mut c_void,
                hashes.as_ptr(),
                hashes.len(),
            )
        };
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
        Ok(chunks)
    };

    let write_data_closure = move |data: &[u8], flush: bool| -> Result<(), E> {
        unsafe {
            write_comp_data_cb(
                user_data_addr as *mut c_void,
                data.as_ptr(),
                data.len(),
                flush,
            );

        Ok(())
        }
    };

    (get_chunks_closure, write_data_closure)
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
/// * `manifest_array_ptr`: A pointer to the start of an array of
///   `FFIFileManifestParentU64` structs, representing the metadata for all 
///   files to be included in the archive.
/// * `manifest_len`: The number of elements in the `manifest_array_ptr` array.
/// * `sorted_hashes_array_ptr`: A pointer to a sorted array of all unique 
///   `u64` chunk hashes that make up the data for all files.
/// * `sorted_hashes_len`: The number of elements in the 
///   `sorted_hashes_array_ptr` array.
/// * `file_count`: The total number of files being added to the archive.
/// * `hash_type`: A numerical code representing the hash algorithm used 
///   (e.g., 1 for xxh3_64).
/// * `total_size`: The total combined size in bytes of all unique,
///   uncompressed chunks.
/// * `user_data`: An opaque `void` pointer that will be passed back to the C
///   callbacks, allowing the C side to maintain state.
/// * `get_chunks_cb`: A C function pointer that the builder will call to 
///   request the raw data for a given set of hashes.
/// * `write_comp_data_cb`: A C function pointer that the builder will call to
///   write out the compressed archive data as it is generated.
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
/// - All pointer arguments (`manifest_array_ptr`, `sorted_hashes_array_ptr`,
///   `out_ptr`) must be non-null and point to valid memory.
/// - The `manifest_len` and `sorted_hashes_len` arguments must accurately 
///   reflect the number of elements in their respective arrays.
/// - The `user_data` pointer and the C callback function pointers 
///   (`get_chunks_cb`, `write_comp_data_cb`) must be valid for the entire
///   lifetime of the builder, until it is consumed by 
///   `archive_builder_build_u64` or freed by `archive_builder_free_u64`.
/// - The `ArchiveBuilderU64` handle returned via `out_ptr` is owned by the C
///   caller and **must** be passed to either `archive_builder_build_u64` to
///   consume it and build the archive, or `archive_builder_free_u64` to
///   deallocate its memory if the build process is aborted. Failure to do so
///   will result in a memory leak.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_new_u64(
    manifest_array_ptr: *const FFIFileManifestParentU64,
    manifest_len: usize,
    sorted_hashes_array_ptr: *const u64,
    sorted_hashes_len: usize,
    file_count: u32,
    hash_type: u8,
    total_size: u64,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void, 
        hashes: *const u64, 
        hashes_len: usize
    ) -> FFIChunkDataArray,
    write_comp_data_cb: unsafe extern "C" fn(
        user_data: *mut c_void,
        data: *const u8, 
        data_len: usize, 
        is_flush: bool
    ),
    out_ptr: *mut *mut ArchiveBuilderU64,
) -> FFIResult {
    if manifest_array_ptr.is_null() || 
        sorted_hashes_array_ptr.is_null() || 
        out_ptr.is_null() {
            return FFIResult::NullArgument;
    }

    let sorted_hashes = unsafe{slice::from_raw_parts(
        sorted_hashes_array_ptr, 
        sorted_hashes_len
    )};

    let ffi_manifests = unsafe {
        slice::from_raw_parts(
            manifest_array_ptr, 
            manifest_len
        )
    };
    
    let ser_file_manifest: Vec<FileManifestParent<u64>> = ffi_manifests
    .iter()
    .map(FileManifestParent::<u64>::from) // Use the From trait
    .collect();

    let (get_chunks_closure, write_data_closure) = 
    setup_builder_closures::<SpriteShrinkError, u64>(
        user_data,
        get_chunks_cb,
        write_comp_data_cb
    );

    let builder = ArchiveBuilder::new(
        ser_file_manifest,
        sorted_hashes,
        file_count,
        hash_type, 
        total_size,
        get_chunks_closure,
        write_data_closure
    );

    let builder_trait_object: Box<dyn ArchiveBuilderTrait<u64>> = Box::new(
        builder
    );

    unsafe {
        *out_ptr = Box::into_raw(builder_trait_object) as *mut ArchiveBuilderU64;
    };
    
    FFIResult::Ok
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
/// * `manifest_array_ptr`: A pointer to the start of an array of
///   `FFIFileManifestParentU128` structs, representing the metadata for all 
///   files to be included in the archive.
/// * `manifest_len`: The number of elements in the `manifest_array_ptr` array.
/// * `sorted_hashes_array_ptr`: A pointer to a sorted array of all unique 
///   `u128` chunk hashes that make up the data for all files.
/// * `sorted_hashes_len`: The number of elements in the 
///   `sorted_hashes_array_ptr` array.
/// * `file_count`: The total number of files being added to the archive.
/// * `hash_type`: A numerical code representing the hash algorithm used 
///   (e.g., 1 for xxh3_64).
/// * `total_size`: The total combined size in bytes of all unique,
///   uncompressed chunks.
/// * `user_data`: An opaque `void` pointer that will be passed back to the C
///   callbacks, allowing the C side to maintain state.
/// * `get_chunks_cb`: A C function pointer that the builder will call to 
///   request the raw data for a given set of hashes.
/// * `write_comp_data_cb`: A C function pointer that the builder will call to
///   write out the compressed archive data as it is generated.
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
/// - All pointer arguments (`manifest_array_ptr`, `sorted_hashes_array_ptr`,
///   `out_ptr`) must be non-null and point to valid memory.
/// - The `manifest_len` and `sorted_hashes_len` arguments must accurately 
///   reflect the number of elements in their respective arrays.
/// - The `user_data` pointer and the C callback function pointers 
///   (`get_chunks_cb`, `write_comp_data_cb`) must be valid for the entire
///   lifetime of the builder, until it is consumed by 
///   `archive_builder_build_u128` or freed by `archive_builder_free_u128`.
/// - The `ArchiveBuilderU128` handle returned via `out_ptr` is owned by the C
///   caller and **must** be passed to either `archive_builder_build_u128` to
///   consume it and build the archive, or `archive_builder_free_u128` to
///   deallocate its memory if the build process is aborted. Failure to do so
///   will result in a memory leak.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_new_u128(
    manifest_array_ptr: *const FFIFileManifestParentU128,
    manifest_len: usize,
    sorted_hashes_array_ptr: *const u128,
    sorted_hashes_len: usize,
    file_count: u32,
    hash_type: u8,
    total_size: u64,
    user_data: *mut c_void,
    get_chunks_cb: unsafe extern "C" fn(
        user_data: *mut c_void, 
        hashes: *const u128, 
        hashes_len: usize
    ) -> FFIChunkDataArray,
    write_comp_data_cb: unsafe extern "C" fn(
        user_data: *mut c_void, 
        data: *const u8, 
        data_len: usize, 
        is_flush: bool
    ),
    out_ptr: *mut *mut ArchiveBuilderU128,
) -> FFIResult {
    if manifest_array_ptr.is_null() || 
        sorted_hashes_array_ptr.is_null() || 
        out_ptr.is_null() {
            return FFIResult::NullArgument;
    }
    
    let sorted_hashes = unsafe{slice::from_raw_parts(
        sorted_hashes_array_ptr, 
        sorted_hashes_len
    )};

    let ffi_manifests = unsafe {
        slice::from_raw_parts(
            manifest_array_ptr, 
            manifest_len
        )
    };
    
    let ser_file_manifest: Vec<FileManifestParent<u128>> = ffi_manifests
    .iter()
    .map(FileManifestParent::<u128>::from) // Use the From trait
    .collect();

    let (get_chunks_closure, write_data_closure) = 
    setup_builder_closures::<SpriteShrinkError, u128>(
        user_data,
        get_chunks_cb,
        write_comp_data_cb
    );

    let builder = ArchiveBuilder::new(
        ser_file_manifest,
        sorted_hashes,
        file_count,
        hash_type, 
        total_size,
        get_chunks_closure,
        write_data_closure
    );

    let builder_trait_object: Box<dyn ArchiveBuilderTrait<u128>> = Box::new(
        builder
    );

    unsafe {
        *out_ptr = Box::into_raw(builder_trait_object) as *mut ArchiveBuilderU128;
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


unsafe fn handle_build_result(
    result: Result<Vec<u8>, SpriteShrinkError>,
    out_ptr: *mut *mut FFIArchiveData,
) -> FFIResult {
    match result {
        Ok(mut archive_data) => {
            let data_ptr = archive_data.as_mut_ptr();
            let data_len = archive_data.len();
            std::mem::forget(archive_data); // Give ownership to C caller

            let output = Box::new(FFIArchiveData {
                data: data_ptr,
                data_len,
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

// A generic helper to deallocate an `ArchiveBuilder` from an FFI handle.
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
                archive_box.data_len
            );
        }
    }
}