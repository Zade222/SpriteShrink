use std::collections::HashMap;
use std::slice;

use dashmap::DashMap;

use crate::ffi::ffi_structs::{FFIDataStoreEntry, FFIFileManifestParent, FFISerializedOutput};
use crate::lib_structs::{FileManifestParent, SSAChunkMeta};
use crate::serialization::{serialize_uncompressed_data};

/// # Safety
///
/// This function is unsafe because it dereferences raw pointers passed from C.
/// The caller MUST ensure that:
/// - `manifest_array_ptr` points to a valid array of `FFIFileManifestParent`
/// of `manifest_len`.
/// - `data_store_array_ptr` points to a valid array of `FFIDataStoreEntry` 
/// of `data_store_len`.
/// - All pointers within these structs are valid for the specified lengths.
/// - The returned pointer from this function must be passed to 
/// `free_serialized_output` to avoid memory leaks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn serialize_uncompressed_data_ffi(
    manifest_array_ptr: *const FFIFileManifestParent,
    manifest_len: usize,
    data_store_array_ptr: *const FFIDataStoreEntry,
    data_store_len: usize,
) -> *mut FFISerializedOutput {
    let (file_manifest, data_store) = unsafe {
        //Convert C inputs to Rust types

        /*Prepare file_manifest DashMap, which is the first parameter of the 
        original serialize_uncompressed_data function, from C input.*/
        let ffi_manifests = slice::from_raw_parts(
            manifest_array_ptr, 
            manifest_len
        );
        
        /*Prepare data_store, which is the second parameter of the original 
        serialize_uncompressed_data function, from C input.*/
        let ffi_data_store = slice::from_raw_parts(
            data_store_array_ptr, 
            data_store_len
        );

        /*Since DashMap is not a supported type in C, the following reconstructs 
        it for use by the library.*/

        //Reconstruct the file_manifest DashMap via the following and loop.
        let file_manifest: DashMap<String, FileManifestParent> = DashMap::new();
        for ffi_fmp in ffi_manifests{
            let filename = std::ffi::CStr::from_ptr(ffi_fmp.filename)
                .to_string_lossy()
                .into_owned();

            let ffi_chunks = std::slice::from_raw_parts(
                ffi_fmp.chunk_metadata, 
                ffi_fmp.chunk_count as usize
            );

            let chunk_metadata = ffi_chunks.iter().map(|c| SSAChunkMeta {
                hash: c.hash,
                offset: c.offset,
                length: c.length,
            }).collect();

            file_manifest.insert(filename.clone(), FileManifestParent {
                filename,
                chunk_count: ffi_fmp.chunk_count,
                chunk_metadata,
            });
        }

        //Reconstruct the data_store HashMap via the following.
        let data_store: HashMap<u64, Vec<u8>> = ffi_data_store.iter().map(|entry| {
            let data_slice = std::slice::from_raw_parts(entry.data, entry.data_len);
            (entry.hash, data_slice.to_vec())
        }).collect();

        (file_manifest, data_store)
    };

    let (ser_file_manifest,
        ser_data_store,
        chunk_index,
        sorted_hashes) = 
        serialize_uncompressed_data(&file_manifest, &data_store);

    let config = bincode::config::standard();
    let ser_manifest_bytes = bincode::serde::encode_to_vec(
        &ser_file_manifest, config).unwrap();
    let ser_chunk_index_bytes = bincode::serde::encode_to_vec(
        &chunk_index, config).unwrap();

    let output = Box::new(FFISerializedOutput {
        ser_manifest_ptr: ser_manifest_bytes.as_ptr(),
        ser_manifest_len: ser_manifest_bytes.len(),
        ser_data_store_ptr: ser_data_store.as_ptr(),
        ser_data_store_len: ser_data_store.len(),
        ser_chunk_index_ptr: ser_chunk_index_bytes.as_ptr(),
        ser_chunk_index_len: ser_chunk_index_bytes.len(),
        sorted_hashes_ptr: sorted_hashes.as_ptr(),
        sorted_hashes_len: sorted_hashes.len(),
    });

    std::mem::forget(ser_manifest_bytes);
    std::mem::forget(ser_data_store);
    std::mem::forget(ser_chunk_index_bytes);
    std::mem::forget(sorted_hashes);

    Box::into_raw(output)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_serialized_output(ptr: *mut FFISerializedOutput) {
    if ptr.is_null() {
        return;
    }
    // Re-take ownership of the Box to deallocate it and its contents.
    unsafe {
        let output = Box::from_raw(ptr);
        // Deallocate all the vectors whose memory was passed to C
        let _ = Vec::from_raw_parts(
            output.ser_manifest_ptr as *mut u8, 
            output.ser_manifest_len, 
            output.ser_manifest_len
        );
        let _ = Vec::from_raw_parts(
            output.ser_data_store_ptr as *mut u8, 
            output.ser_data_store_len, 
            output.ser_data_store_len
        );
        let _ = Vec::from_raw_parts(
            output.ser_chunk_index_ptr as *mut u8, 
            output.ser_chunk_index_len, 
            output.ser_chunk_index_len);
        let _ = Vec::from_raw_parts(
            output.sorted_hashes_ptr as *mut u64, 
            output.sorted_hashes_len, 
            output.sorted_hashes_len
        );
    }
}