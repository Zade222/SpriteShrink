//! FFI layer for the archive building process.
//!
//! This module provides a C-compatible interface for creating, configuring,
//! and building sprite-shrink archives. It handles the conversion between
//! Rust and C data types, ensuring memory safety across the FFI boundary.

use std::collections::HashMap;
use std::os::raw::{c_void};
use std::ptr;
use std::slice;

use crate::archive::{ArchiveBuilder};
use crate::lib_structs::{Progress};
use crate::ffi::ffi_structs::{
    FFIArchiveData, FFIFileManifestParent, FFIDataStoreEntry, FFIProgress
};
use crate::{FileManifestParent, SSAChunkMeta};

//Type alias for clarity in other functions
type BuilderHandle = *mut ArchiveBuilder<'static, fn(Progress)>;

/// Creates a new ArchiveBuilder and returns an opaque pointer to it.
///
/// # Safety
/// All pointer arguments must be valid and point to initialized data for their
/// specified lengths. The returned pointer must be freed with 
/// `archive_builder_free` or consumed by `archive_builder_build` to prevent
/// memory leaks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_new(
    manifest_array_ptr: *const FFIFileManifestParent,
    manifest_len: usize,
    data_store_array_ptr: *const FFIDataStoreEntry,
    data_store_len: usize,
    sorted_hashes_array_ptr: *const u64,
    sorted_hashes_len: usize,
    file_count: u32,
) -> BuilderHandle {
    //Fail early on null pointers to prevent dereferencing invalid memory.
    if manifest_array_ptr.is_null() 
        || data_store_array_ptr.is_null() 
        || sorted_hashes_array_ptr.is_null() {
        return ptr::null_mut();
    }
    
    let (ser_file_manifest, 
        data_store, 
        sorted_hashes
    ) = unsafe {
        /*Prepare ser_file_manifest, which is the first parameter of the 
        original ArchiveBuilder function, from C input.*/
        let ser_file_manifest = slice::from_raw_parts(
            manifest_array_ptr, 
            manifest_len
        )
        .iter()
        .map(|fmp| {
            let file_name = std::ffi::CStr::from_ptr(fmp.filename)
                .to_string_lossy()
                .into_owned();

            let chunk_metadata = slice::from_raw_parts(
                fmp.chunk_metadata, 
                fmp.chunk_count as usize
            )
                .iter()
                .map(|meta|{
                    SSAChunkMeta{
                        hash: meta.hash,
                        offset: meta.offset,
                        length: meta.length,
                    }
                }).collect::<Vec<_>>();
            
            FileManifestParent {
                filename: file_name,
                chunk_count: fmp.chunk_count,
                chunk_metadata,
            }
        }).collect::<Vec<_>>();

        /*Prepare data_store, which is the second parameter of the original 
        ArchiveBuilder function, from C input.*/
        //Reconstruct the data_store HashMap via the following.
        let data_store: HashMap<u64, Vec<u8>> = slice::from_raw_parts(
            data_store_array_ptr, 
            data_store_len
        )
            .iter()
            .map(|entry| {
                let data_slice = std::slice::from_raw_parts(
                    entry.data, 
                    entry.data_len
                );
                (entry.hash, data_slice.to_vec())
        }).collect();

        /*Prepare sorted_hashes, which is the third parameter of the original 
        ArchiveBuilder function, from C input.*/
        let sorted_hashes = slice::from_raw_parts(
            sorted_hashes_array_ptr, 
            sorted_hashes_len
        );

        (ser_file_manifest, data_store, sorted_hashes)
    };

    let builder = ArchiveBuilder::new(
        ser_file_manifest,
        data_store,
        sorted_hashes.to_vec(),
        file_count,
    );

    Box::into_raw(Box::new(builder))
}

/// Sets the compression level on the ArchiveBuilder.
///
/// # Safety
/// The `builder_handle` must be a valid pointer returned from 
/// `archive_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_set_compression_level(
    builder_handle: BuilderHandle, level: i32
) {
    unsafe {
        if let Some(
            builder
        ) = (builder_handle).as_mut() {
            builder.compression_level(level);
        }
    };
    
}

/// Sets the dictionary size on the ArchiveBuilder.
///
/// # Safety
/// The `builder_handle` must be a valid pointer returned from .
/// `archive_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_set_dictionary_size(
    builder_handle: BuilderHandle, size: u64) 
{
    unsafe {
        if !builder_handle.is_null() {
            if let Some(
                builder
            ) = (builder_handle).as_mut() {
                builder.dictionary_size(size);
            }
        }
    }
}

/// Sets the progress callback for the ArchiveBuilder.
///
/// # Safety
/// The `builder_handle` must be a valid pointer returned from 
/// `archive_builder_new`. The `callback` function pointer must be valid for 
/// the lifetime of the builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_set_c_progress(
    builder_handle: BuilderHandle,
    callback: extern "C" fn(FFIProgress, *mut c_void),
    user_data: *mut c_void,
) {
    unsafe {    
        if !builder_handle.is_null() {
            if let Some(
                builder
            ) = (builder_handle).as_mut() {
                builder.with_c_progress(callback, user_data);
            }
        }
    }
}

/// Consumes the builder, builds the archive, and returns the data.
///
/// On failure, this function returns a null pointer.
///
/// # Safety
/// The `builder_handle` must be a valid pointer from `archive_builder_new`.
/// This function consumes the builder, so `builder_handle` is invalid after 
/// this call. The returned pointer must be freed with `archive_data_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_build(
    builder_handle: BuilderHandle
) -> *mut FFIArchiveData {
    if builder_handle.is_null() {
        return std::ptr::null_mut();
    }
    
    // Retake ownership of the builder from the raw pointer
    let builder = unsafe {
        Box::from_raw(builder_handle)
    };
    
    match builder.build() {
        Ok(mut archive_data) => {
            let data_ptr = archive_data.as_mut_ptr();
            let data_len = archive_data.len();
            std::mem::forget(archive_data); // Give ownership to C caller

            let output = Box::new(FFIArchiveData {
                data: data_ptr,
                data_len,
            });
            Box::into_raw(output)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// Frees the ArchiveBuilder if the build is never run.
///
/// # Safety
/// The `builder_handle` must be a valid pointer from `archive_builder_new` that
/// has not been passed to `archive_builder_build`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_free(builder_handle: BuilderHandle) {
    unsafe{
        if !builder_handle.is_null() {
            let _ = Box::from_raw(builder_handle);
        }
    }
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