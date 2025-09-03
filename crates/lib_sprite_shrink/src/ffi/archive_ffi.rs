//! FFI layer for the archive building process.
//!
//! This module provides a C-compatible interface for creating, configuring,
//! and building sprite-shrink archives. It handles the conversion between
//! Rust and C data types, ensuring memory safety across the FFI boundary.

use std::{
    os::raw::c_void,
    slice,
};

use crate::archive::{ArchiveBuilder, ArchiveError};
use crate::ffi::FFIStatus;
use crate::ffi::ffi_structs::{
    FFIArchiveData, FFIChunkDataArray,
    FFIFileManifestParentU64, FFIFileManifestParentU128,
    FFIProgress
};
use crate::lib_error_handling::SpriteShrinkError;
use crate::processing::ProcessingError;
use crate::{FileManifestParent, SSAChunkMeta};

//Type alias for clarity in other functions
type BuilderHandle = *mut c_void;

trait ArchiveBuilderTraitU64 {
        fn set_compression_algorithm(&mut self, algorithm: u16);
        fn set_compression_level(&mut self, level: i32);
        fn set_dictionary_size(&mut self, size: u64);
        fn with_c_progress(&mut self, callback: extern "C" fn(FFIProgress, *mut c_void), user_data: *mut c_void);
        fn set_worker_threads(&mut self, threads: usize);
        fn set_optimize_dictionary(&mut self, optimize: bool);

        //This method consumes the builder
        fn build(self: Box<Self>) -> Result<Vec<u8>, SpriteShrinkError>;
}

impl<'a, R, W> ArchiveBuilderTraitU64 for ArchiveBuilder<'a, u64, R, W>
where
    R: Fn(&[u64]) -> Vec<Vec<u8>> + Send + Sync + 'static,
    W: FnMut(&[u8], bool) + Send + Sync + 'static,
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

trait ArchiveBuilderTraitU128 {
        fn set_compression_algorithm(&mut self, algorithm: u16);
        fn set_compression_level(&mut self, level: i32);
        fn set_dictionary_size(&mut self, size: u64);
        fn with_c_progress(&mut self, callback: extern "C" fn(FFIProgress, *mut c_void), user_data: *mut c_void);
        fn set_worker_threads(&mut self, threads: usize);
        fn set_optimize_dictionary(&mut self, optimize: bool);

        //This method consumes the builder
        fn build(self: Box<Self>) -> Result<Vec<u8>, SpriteShrinkError>;
}

impl<'a, R, W> ArchiveBuilderTraitU128 for ArchiveBuilder<'a, u128, R, W>
where
    R: Fn(&[u128]) -> Vec<Vec<u8>> + Send + Sync + 'static,
    W: FnMut(&[u8], bool) + Send + Sync + 'static,
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
        $hash_type:path,
        $builder_type:path,
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
            sorted_hashes_array_ptr: *const $hash_type,
            sorted_hashes_len: usize,
            file_count: u32,
            hash_type: u8,
            total_size: u64,
            user_data: *mut c_void,
            get_chunks_cb: unsafe extern "C" fn(
                user_data: *mut c_void, 
                hashes: *const $hash_type, 
                hashes_len: usize
            ) -> FFIChunkDataArray,
            write_comp_data_cb: unsafe extern "C" fn(
                user_data: *mut c_void, 
                data: *const u8, 
                data_len: usize, 
                is_flush: bool
            ),
            out_ptr: *mut BuilderHandle,
        ) -> FFIStatus {
            /*Fail early on null pointers to prevent dereferencing invalid 
            memory.*/
            if manifest_array_ptr.is_null()
                || sorted_hashes_array_ptr.is_null()
                || out_ptr.is_null()
            {
                return FFIStatus::NullArgument;
            }
            
            let (ser_file_manifest, sorted_hashes) = unsafe {
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

                /*Prepare sorted_hashes, which is the third parameter of the 
                original ArchiveBuilder function, from C input.*/
                let sorted_hashes = slice::from_raw_parts(
                    sorted_hashes_array_ptr, 
                    sorted_hashes_len
                );

                (ser_file_manifest, sorted_hashes)
            };

            let user_data_addr = user_data as usize;

            let get_chunks_closure = move |hashes: &[$hash_type]| {
                let ffi_chunks_array = unsafe {
                    get_chunks_cb(
                        user_data_addr as *mut c_void, 
                        hashes.as_ptr(), 
                        hashes.len()
                    )};
                
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
                chunks
            };

            let write_data_closure = move |data: &[u8], flush: bool| {
                unsafe {
                    write_comp_data_cb(
                        user_data_addr as *mut c_void, 
                        data.as_ptr(), 
                        data.len(), 
                        flush
                    );
                }
            };

            let builder = ArchiveBuilder::new(
                ser_file_manifest,
                sorted_hashes,
                file_count,
                hash_type, 
                total_size,
                get_chunks_closure,
                write_data_closure
            );

            let builder_trait_object: Box<dyn $builder_type>
                = Box::new(builder);

            let handle_box = Box::new(builder_trait_object);

            unsafe {
                *out_ptr = Box::into_raw(handle_box) as *mut c_void;
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
    ArchiveBuilderTraitU64,
);

archive_builder_new!(
    "  caller and MUST be freed with `archive_builder_free_u128` or consumed",
    "  by `archive_builder_build_u128`.",
    archive_builder_new_u128,
    FFIFileManifestParentU128,
    FFIDataStoreEntryU128,
    u128,
    ArchiveBuilderTraitU128,
);


macro_rules! ffi_builder_setter {
    (
        /*Each of these lines defines a placeholder and its type.
        For example, `$fn_name:ident` means "expect an identifier (like a 
        function name) and name it $fn_name for use in the template body."*/
        $doc_l1:literal,
        $doc_l5:literal,
        $fn_name:ident,
        $builder_type:path,
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
                let handle_box_ptr = builder_handle as *mut Box<dyn $builder_type>;

                if let Some(builder_box) = handle_box_ptr.as_mut() {
                    let builder = builder_box.as_mut();
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
    ArchiveBuilderTraitU64,
    code,
    u16,
    set_compression_algorithm
);

ffi_builder_setter!(
    "Sets the numerical compression code on the ArchiveBuilder for \
    a u64 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_compression_algorithm_u128,
    ArchiveBuilderTraitU128,
    code,
    u16,
    set_compression_algorithm
);

ffi_builder_setter!(
    "Sets the compression level on the ArchiveBuilder for a u64 hash.",
    "`archive_builder_new_u64`.",
    archive_builder_set_compression_level_u64,
    ArchiveBuilderTraitU64,
    level,
    i32,
    set_compression_level
);

ffi_builder_setter!(
    "Sets the compression level on the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_compression_level_u128,
    ArchiveBuilderTraitU128,
    level,
    i32,
    set_compression_level
);

ffi_builder_setter!(
    "Sets the dictionary size on the ArchiveBuilder for a u64 hash.",
    "`archive_builder_new_u64`.",
    archive_builder_set_dictionary_size_u64,
    ArchiveBuilderTraitU64,
    size,
    u64,
    set_dictionary_size
);

ffi_builder_setter!(
    "Sets the dictionary size on the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_dictionary_size_u128,
    ArchiveBuilderTraitU128,
    size,
    u64,
    set_dictionary_size
);

ffi_builder_setter!(
    "Sets the amount of worker threads on the ArchiveBuilder for a u64 hash.",
    "`archive_builder_new_u64`.",
    archive_builder_set_worker_count_size_u64,
    ArchiveBuilderTraitU64,
    threads,
    usize,
    set_worker_threads
);

ffi_builder_setter!(
    "Sets the amount of worker threads on the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_worker_count_size_u128,
    ArchiveBuilderTraitU128,
    threads,
    usize,
    set_worker_threads
);

ffi_builder_setter!(
    "Sets the amount of worker threads on the ArchiveBuilder for a u64 hash.",
    "`archive_builder_new_u64`.",
    archive_builder_set_optimize_dictionary_flag_size_u64,
    ArchiveBuilderTraitU64,
    optimize,
    bool,
    set_optimize_dictionary
);

ffi_builder_setter!(
    "Sets the amount of worker threads on the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`.",
    archive_builder_set_optimize_dictionary_flag_size_u128,
    ArchiveBuilderTraitU128,
    optimize,
    bool,
    set_optimize_dictionary
);

macro_rules! ffi_builder_progress_setter{
    (
        $doc_l1:literal,
        $doc_l5:literal,
        $fn_name:ident,
        $builder_type:path,
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
                let handle_box_ptr = builder_handle as *mut Box<
                    dyn $builder_type
                >;

                if let Some(builder_box) = handle_box_ptr.as_mut() {
                    let builder = builder_box.as_mut();
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
    ArchiveBuilderTraitU64,
);

ffi_builder_progress_setter!(
    "Sets the progress callback for the ArchiveBuilder for a u128 hash.",
    "`archive_builder_new_u128`. The `callback` function pointer must be \
    valid for",
    archive_builder_set_c_progress_u128,
    ArchiveBuilderTraitU128,
);

macro_rules! ffi_builder_build_setter{
    (
        $doc_l1:literal,
        $doc_l2:literal,
        $doc_l6:literal,
        $doc_l11:literal,
        $fn_name:ident,
        $builder_type:path,

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
            
            //Retake ownership of the outer Box from the raw pointer.
            let handle_box = unsafe {
                Box::from_raw(builder_handle as *mut Box<dyn $builder_type>)
            };
            /*Dereference the outer box, moving the inner `Box<dyn Trait>` out,
            giving ownership of it.*/
            let builder = *handle_box;
            
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
                    SpriteShrinkError::Archive(
                        ArchiveError::Dictionary(_)
                    ) => FFIStatus::DictionaryError,
                    SpriteShrinkError::Archive(
                        ArchiveError::CompressionError(_)
                    ) => FFIStatus::CompressionError,
                    SpriteShrinkError::Archive(
                        ArchiveError::ThreadPool(_)
                    ) => FFIStatus::ThreadPoolError,
                    SpriteShrinkError::Processing(
                        ProcessingError::SampleGeneration
                    ) => FFIStatus::DictionaryError,
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
    ArchiveBuilderTraitU64,
);

ffi_builder_build_setter!(
    "Consumes the u128 builder, builds the archive, and returns the data via \
    an",
    "out-parameter for a u128 hash.",
    "    `archive_builder_new_u128`.",
    "    `archive_data_free_u128`",
    archive_builder_build_u128,
    ArchiveBuilderTraitU128,
);

macro_rules! archive_builder_free_setter {
    (
        $doc_l1:literal,
        $doc_l5:literal,
        $doc_l6:literal,
        $fn_name:ident,
        $builder_trait:path,
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
                        builder_handle as *mut Box<dyn $builder_trait>
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
    ArchiveBuilderTraitU64,
);

archive_builder_free_setter!(
    "Frees the ArchiveBuilder if the build is never run for u128 hashes.",
    "`archive_builder_new_u128` that has not been passed to",
    "`archive_builder_build_u128`.",
    archive_builder_free_u128,
    ArchiveBuilderTraitU128,
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