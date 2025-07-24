//! FFI layer for the archive building process.
//!
//! This module provides a C-compatible interface for creating, configuring,
//! and building sprite-shrink archives. It handles the conversion between
//! Rust and C data types, ensuring memory safety across the FFI boundary.

use std::collections::HashMap;
use std::os::raw::{c_void};
use std::slice;

use crate::archive::{ArchiveBuilder};
use crate::lib_error_handling::LibError;
use crate::lib_structs::{Progress};
use crate::ffi::FFIStatus;
use crate::ffi::ffi_structs::{
    FFIArchiveData, FFIFileManifestParent, FFIDataStoreEntry, FFIProgress
};
use crate::{FileManifestParent, SSAChunkMeta};

//Type alias for clarity in other functions
type BuilderHandle = *mut ArchiveBuilder<'static, fn(Progress)>;

/// Creates a new ArchiveBuilder and returns it via an out-parameter.
///
/// # Safety
/// - All input pointers must be valid and non-null.
/// - `out_builder` must be a valid pointer to a `BuilderHandle`.
/// - On success, the pointer written to `out_builder` is owned by the caller
///   and MUST be freed with `archive_builder_free` or consumed by 
/// `archive_builder_build`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_new(
    manifest_array_ptr: *const FFIFileManifestParent,
    manifest_len: usize,
    data_store_array_ptr: *const FFIDataStoreEntry,
    data_store_len: usize,
    sorted_hashes_array_ptr: *const u64,
    sorted_hashes_len: usize,
    file_count: u32,
    out_ptr: *mut BuilderHandle,
) -> FFIStatus {
    //Fail early on null pointers to prevent dereferencing invalid memory.
    if manifest_array_ptr.is_null()
        || data_store_array_ptr.is_null()
        || sorted_hashes_array_ptr.is_null()
        || out_ptr.is_null()
    {
        return FFIStatus::NullArgument;
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
                fmp.chunk_metadata_len
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
                chunk_count: fmp.chunk_metadata_len as u64,
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

    unsafe {
        *out_ptr = Box::into_raw(Box::new(builder));
    };
    FFIStatus::Ok
}

/// Sets the compression level on the ArchiveBuilder.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from 
/// `archive_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_set_compression_level(
    builder_handle: BuilderHandle, 
    level: i32
) -> FFIStatus {
    unsafe {
        if let Some(builder) = 
            builder_handle.as_mut() {
                builder.compression_level(level);
                FFIStatus::Ok
        } else {
            FFIStatus::NullArgument
        }
    }
}

/// Sets the dictionary size on the ArchiveBuilder.
///
/// # Safety
/// The `builder_handle` must be a valid, non-null pointer from 
/// `archive_builder_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_set_dictionary_size(
    builder_handle: BuilderHandle, 
    size: u64
) -> FFIStatus {
    unsafe {
        if let Some(builder) = 
            builder_handle.as_mut() {
                builder.dictionary_size(size);
                FFIStatus::Ok
        } else {
            FFIStatus::NullArgument
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
) -> FFIStatus {
    unsafe {    
        if let Some(builder) = 
        builder_handle.as_mut() {
            builder.with_c_progress(callback, user_data);
            FFIStatus::Ok
        } else {
            FFIStatus::NullArgument
        }
    }
}

/// Consumes the builder, builds the archive, and returns the data via an 
/// out-parameter.
///
/// # Safety
/// - `builder_handle` must be a valid pointer from `archive_builder_new`.
/// - `out_data` must be a valid pointer to an `*mut FFIArchiveData`.
/// - This function consumes the builder; `builder_handle` is invalid after 
/// this call.
/// - The pointer returned via `out_data` must be freed with 
/// `archive_data_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn archive_builder_build(
    builder_handle: BuilderHandle,
    out_ptr: *mut *mut FFIArchiveData,
) -> FFIStatus {
    if builder_handle.is_null() 
        || out_ptr.is_null() {
        return FFIStatus::NullArgument;
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
            unsafe {
                *out_ptr = Box::into_raw(output);
            }
            FFIStatus::Ok
        }
        Err(e) => match e {
            LibError::DictionaryError(_) => FFIStatus::DictionaryError,
            LibError::CompressionError(_) => FFIStatus::CompressionError,
            LibError::ThreadPoolError(_) => FFIStatus::ThreadPoolError,
            _ => FFIStatus::InternalError,
        },
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