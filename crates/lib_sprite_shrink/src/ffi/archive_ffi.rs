//! FFI layer for the archive building process.
//!
//! This module provides a C-compatible interface for creating, configuring,
//! and building sprite-shrink archives. It handles the conversion between
//! Rust and C data types, ensuring memory safety across the FFI boundary.

use std::{
    collections::HashMap,
    os::raw::c_void,
    slice
};

use crate::archive::{ArchiveBuilder};
use crate::lib_error_handling::LibError;
use crate::lib_structs::{Progress};
use crate::ffi::FFIStatus;
use crate::ffi::ffi_structs::{
    FFIArchiveData, FFIFileManifestParentU64, FFIFileManifestParentU128,
    FFIDataStoreEntryU64, 
    FFIDataStoreEntryU128, FFIProgress
};
use crate::{FileManifestParent, SSAChunkMeta};

//Type alias for clarity in other functions
type BuilderHandle = *mut c_void;

type BuilderU64 = ArchiveBuilder<'static, fn(Progress), u64>;
type BuilderU128 = ArchiveBuilder<'static, fn(Progress), u128>;



/// Creates a new ArchiveBuilder and returns it via an out-parameter.
///
/// # Safety
/// - All input pointers must be valid and non-null.
/// - `out_builder` must be a valid pointer to a `BuilderHandle`.
/// - On success, the pointer written to `out_builder` is owned by the caller
///   and MUST be freed with `archive_builder_free` or consumed by 
///   `archive_builder_build`.
macro_rules! archive_builder_new{
    (
        $doc_l7:literal,
        $doc_l8:literal,
        $fn_name:ident,
        $manifest_array_type:ty,
        $data_store_array_type:ty,
        $hash_type:ty,
        $builder_type:ty,
        $hash_type_identifier:literal,
    ) => {
        /// Creates a new ArchiveBuilder and returns it via an out-parameter.
        ///
        /// # Safety
        /// - All input pointers must be valid and non-null.
        /// - `out_builder` must be a valid pointer to a `BuilderHandle`.
        /// - On success, the pointer written to `out_builder` is owned by the
        #[doc = $doc_l7]
        #[doc = $doc_l8]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            manifest_array_ptr: *const $manifest_array_type,
            manifest_len: usize,
            data_store_array_ptr: *const $data_store_array_type,
            data_store_len: usize,
            sorted_hashes_array_ptr: *const $hash_type,
            sorted_hashes_len: usize,
            file_count: u32,
            out_ptr: *mut BuilderHandle,
        ) -> FFIStatus {
            /*Fail early on null pointers to prevent dereferencing invalid 
            memory.*/
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
                /*Prepare ser_file_manifest, which is the first parameter of 
                the original ArchiveBuilder function, from C input.*/
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

                /*Prepare data_store, which is the second parameter of the 
                original ArchiveBuilder function, from C input.*/
                //Reconstruct the data_store HashMap via the following.
                let data_store: HashMap<$hash_type, Vec<u8>> = 
                    slice::from_raw_parts(
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

                /*Prepare sorted_hashes, which is the third parameter of the 
                original ArchiveBuilder function, from C input.*/
                let sorted_hashes = slice::from_raw_parts(
                    sorted_hashes_array_ptr, 
                    sorted_hashes_len
                );

                (ser_file_manifest, data_store, sorted_hashes)
            };

            let builder = <$builder_type>::new(
                ser_file_manifest,
                data_store,
                sorted_hashes.to_vec(),
                file_count,
                $hash_type_identifier
            );

            unsafe {
                *out_ptr = Box::into_raw(Box::new(builder)) as *mut c_void;
            };
            FFIStatus::Ok
        }
    };
}

archive_builder_new!(
    "  caller and MUST be freed with `archive_builder_free_u64` or consumed",
    "  by `archive_builder_build_u64`.",
    archive_builder_new_u64,
    FFIFileManifestParentU64,
    FFIDataStoreEntryU64,
    u64,
    BuilderU64,
    1, //Set type to xxhash3_64bit
);

archive_builder_new!(
    "  caller and MUST be freed with `archive_builder_free_u128` or consumed",
    "  by `archive_builder_build_u128`.",
    archive_builder_new_u128,
    FFIFileManifestParentU128,
    FFIDataStoreEntryU128,
    u128,
    BuilderU128,
    2, //Set type to xxhash3_128bit
);


macro_rules! ffi_builder_setter {
    (
        /*Each of these lines defines a placeholder and its type.
        For example, `$fn_name:ident` means "expect an identifier (like a 
        function name) and name it $fn_name for use in the template body."*/
        $doc_l1:literal,
        $doc_l5:literal,
        $fn_name:ident,
        $builder_type:ty,
        $param_name:ident,
        $param_type:ty,
        $builder_method:ident
    ) => {
        /*This is the template body. The compiler will substitute the
        placeholders with the actual values you provide.*/

        #[doc = $doc_l1]
        ///
        /// # Safety
        /// The `builder_handle` must be a valid, non-null pointer from 
        #[doc = $doc_l5]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name( //Placeholder for function name
            builder_handle: BuilderHandle,
            $param_name: $param_type,//Placeholders for parameter name and type
        ) -> FFIStatus {
            /*Placeholder for the specific builder type (BuilderU64 or 
            BuilderU128)*/
            unsafe{
                let builder_mut = (builder_handle as *mut $builder_type)
                    .as_mut();
                
                if let Some(builder) = builder_mut {
                    //Placeholder for the builder's method to call
                    builder.$builder_method($param_name);
                    FFIStatus::Ok
                } else {
                    FFIStatus::NullArgument
                }
            }
        }
    };
}

ffi_builder_setter!(
    "Sets the numerical compression code on the ArchiveBuilder for \
    a u64 hash.",
    "`archive_builder_new_u64`.",
    archive_builder_set_compression_algorithm_u64,
    BuilderU64,
    code,
    u16,
    compression_algorithm
);

ffi_builder_setter!(
    "Sets the numerical compression code on the ArchiveBuilder for \
    a u64 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_compression_algorithm_u128,
    BuilderU128,
    code,
    u16,
    compression_algorithm
);

ffi_builder_setter!(
    "Sets the compression level on the ArchiveBuilder for a u64 hash.",
    "`archive_builder_new_u64`.",
    archive_builder_set_compression_level_u64,
    BuilderU64,
    level,
    i32,
    compression_level
);

ffi_builder_setter!(
    "Sets the compression level on the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_compression_level_u128,
    BuilderU128,
    level,
    i32,
    compression_level
);

ffi_builder_setter!(
    "Sets the dictionary size on the ArchiveBuilder for a u64 hash.",
    "`archive_builder_new_u64`.",
    archive_builder_set_dictionary_size_u64,
    BuilderU64,
    size,
    u64,
    dictionary_size
);

ffi_builder_setter!(
    "Sets the dictionary size on the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_dictionary_size_u128,
    BuilderU128,
    size,
    u64,
    dictionary_size
);

macro_rules! ffi_builder_progress_setter{
    (
        $doc_l1:literal,
        $doc_l5:literal,
        $fn_name:ident,
        $builder_type:ty,
    ) => {
        #[doc = $doc_l1]
        ///
        /// # Safety
        /// The `builder_handle` must be a valid pointer returned from 
        #[doc = $doc_l5]
        /// the lifetime of the builder.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            builder_handle: BuilderHandle,
            callback: extern "C" fn(FFIProgress, *mut c_void),
            user_data: *mut c_void,
        ) -> FFIStatus {
            unsafe {
                let builder_mut = (builder_handle as *mut $builder_type)
                    .as_mut();

                if let Some(builder) = 
                builder_mut {
                    builder.with_c_progress(callback, user_data);
                    FFIStatus::Ok
                } else {
                    FFIStatus::NullArgument
                }
            }
        }
    };
}

ffi_builder_progress_setter!(
    "Sets the progress callback for the ArchiveBuilder for a u64 hash.",
    "`archive_builder_new_u64`. The `callback` function pointer must be \
    valid for",
    archive_builder_set_c_progress_u64,
    BuilderU64,
);

ffi_builder_progress_setter!(
    "Sets the progress callback for the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`. The `callback` function pointer must be \
    valid for",
    archive_builder_set_c_progress_u128,
    BuilderU128,
);

macro_rules! ffi_builder_build_setter{
    (
        $doc_l1:literal,
        $doc_l2:literal,
        $doc_l6:literal,
        $doc_l11:literal,
        $fn_name:ident,
        $builder_type:ty,

    ) => {
        #[doc = $doc_l1]
        #[doc = $doc_l2]
        ///
        /// # Safety
        /// - `builder_handle` must be a valid pointer from 
        #[doc = $doc_l6]
        /// - `out_data` must be a valid pointer to an `*mut FFIArchiveData`.
        /// - This function consumes the builder; `builder_handle` is invalid 
        ///   after this call.
        /// - The pointer returned via `out_data` must be freed with 
        #[doc = $doc_l11]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(
            builder_handle: BuilderHandle,
            out_ptr: *mut *mut FFIArchiveData,
        ) -> FFIStatus {
            if builder_handle.is_null() 
                || out_ptr.is_null() {
                return FFIStatus::NullArgument;
            }
            
            // Retake ownership of the builder from the raw pointer
            let builder = unsafe {
                Box::from_raw(builder_handle as *mut $builder_type)
            };
            
            match builder.build() {
                Ok(mut archive_data) => {
                    let data_ptr = archive_data.as_mut_ptr();
                    let data_len = archive_data.len();
                    //Give ownership to C caller
                    std::mem::forget(archive_data);

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
                    LibError::DictionaryError(_) =>
                        FFIStatus::DictionaryError,
                    LibError::CompressionError(_) =>
                        FFIStatus::CompressionError,
                    LibError::ThreadPoolError(_) => 
                        FFIStatus::ThreadPoolError,
                    _ => FFIStatus::InternalError,
                },
            }
        }
    };
}

ffi_builder_build_setter!(
    "Consumes the u64 builder, builds the archive, and returns the data via \
    an",
    "out-parameter for a u64 hash.",
    "    `archive_builder_new_u64`.",
    "    `archive_data_free_u64`",
    archive_builder_build_u64,
    BuilderU64,
);

ffi_builder_build_setter!(
    "Consumes the u128 builder, builds the archive, and returns the data via \
    an",
    "out-parameter for a u128 hash.",
    "    `archive_builder_new_u128`.",
    "    `archive_data_free_u128`",
    archive_builder_build_u128,
    BuilderU128,
);

macro_rules! archive_builder_free_setter {
    (
        $doc_l1:literal,
        $doc_l5:literal,
        $doc_l6:literal,
        $fn_name:ident,
        $builder_type:ty,
    ) => {
        #[doc = $doc_l1]
        ///
        /// # Safety
        /// The `builder_handle` must be a valid pointer from 
        #[doc = $doc_l5] 
        #[doc = $doc_l6]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn_name(builder_handle: BuilderHandle) {
            unsafe{
                if !builder_handle.is_null() {
                    let _ = Box::from_raw(
                        builder_handle as *mut $builder_type
                    );
                }
            }
        }
    };
}

archive_builder_free_setter!(
    "Frees the ArchiveBuilder if the build is never run for u64 hashes.",
    "`archive_builder_new_u64` that has not been passed to",
    "`archive_builder_build_u64`.",
    archive_builder_free_u64,
    BuilderU64,
);

archive_builder_free_setter!(
    "Frees the ArchiveBuilder if the build is never run for u128 hashes.",
    "`archive_builder_new_u128` that has not been passed to",
    "`archive_builder_build_u128`.",
    archive_builder_free_u128,
    BuilderU128,
);

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