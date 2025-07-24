use std::collections::HashMap;
use std::ffi::CString;
use std::slice;

use dashmap::DashMap;

use crate::ffi::ffi_structs::{
    FFIChunkLocation, FFIDataStoreEntry, FFIFileManifestParent, 
    FFISerializedOutput, FFISSAChunkMeta
};
use crate::ffi::FFIChunkIndexEntry;
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

    let mut ffi_manifests: Vec<FFIFileManifestParent> = ser_file_manifest
        .into_iter()
        .map(|fmp| {
            let mut chunk_meta_vec: Vec<FFISSAChunkMeta> = fmp.chunk_metadata
                .into_iter()
                .map(|meta| FFISSAChunkMeta {
                    hash: meta.hash,
                    offset: meta.offset,
                    length: meta.length,
                })
                .collect();

            let chunk_meta_ptr = chunk_meta_vec
                .as_mut_ptr();
            //Give up ownership so Rust doesn't deallocate it
            std::mem::forget(chunk_meta_vec);

            FFIFileManifestParent {
                filename: CString::new(fmp.filename).unwrap_or_default()
                    .into_raw(),
                chunk_count: fmp.chunk_count,
                chunk_metadata: chunk_meta_ptr
            }
        })
        .collect::<Vec<_>>();
    
    let manifests_ptr = ffi_manifests.as_mut_ptr();
    let manifests_len = ffi_manifests.len();
    std::mem::forget(ffi_manifests);//Give up ownership of the outer Vec

    let mut entries: Vec<FFIChunkIndexEntry> = chunk_index
        .into_iter()
        .map(|(hash, location)| FFIChunkIndexEntry {
            hash,
            data: FFIChunkLocation {
                offset: location.offset,
                length: location.length,
            },
        })
        .collect();

    let entries_ptr = entries.as_mut_ptr();
    let entries_len = entries.len();

    //Give up ownership of the Vec so it doesn't get deallocated.
    std::mem::forget(entries);

    let output = Box::new(FFISerializedOutput {
        ser_manifest_ptr: manifests_ptr,
        ser_manifest_len: manifests_len,
        ser_data_store_ptr: ser_data_store.as_ptr(),
        ser_data_store_len: ser_data_store.len(),
        ser_chunk_index_ptr: entries_ptr,
        ser_chunk_index_len: entries_len,
        sorted_hashes_ptr: sorted_hashes.as_ptr(),
        sorted_hashes_len: sorted_hashes.len(),
    });

    std::mem::forget(ser_data_store);
    std::mem::forget(sorted_hashes);

    Box::into_raw(output)
}

/// Frees the memory allocated by `serialize_uncompressed_data_ffi`.
///
/// This function is responsible for deallocating the `FFISerializedOutput`
/// struct and all the memory blocks it points to. This includes the
/// serialized manifest, data store, chunk index, and sorted hashes.
///
/// # Safety
///
/// The caller MUST ensure that `ptr` is a valid pointer returned from a
/// successful call to `serialize_uncompressed_data_ffi`. Passing a null
/// pointer or a pointer that has already been freed will lead to
/// undefined behavior. This function must only be called once per pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_serialized_output(ptr: *mut FFISerializedOutput) {
    if ptr.is_null() {
        return;
    }
    //Re-take ownership of the Box to deallocate it and its contents.
    unsafe {
        let output = Box::from_raw(ptr);
        //Deallocate all the vectors whose memory was passed to C
        let ffi_manifests = Vec::from_raw_parts(
            output.ser_manifest_ptr,
            output.ser_manifest_len,
            output.ser_manifest_len,
        );

        for fmp in ffi_manifests {
            //Deallocate the CString for the filename.
            let _ = CString::from_raw(fmp.filename);
            //Reconstruct and deallocate the Vec for the chunk metadata.
            let _ = Vec::from_raw_parts(
                fmp.chunk_metadata as *mut FFISSAChunkMeta,
                fmp.chunk_count as usize,
                fmp.chunk_count as usize,
            );
        }

        let _ = Vec::from_raw_parts(
            output.ser_data_store_ptr as *mut u8, 
            output.ser_data_store_len, 
            output.ser_data_store_len
        );
        let _ = Vec::from_raw_parts(
            output.ser_chunk_index_ptr as *mut FFIChunkIndexEntry, 
            output.ser_chunk_index_len, 
            output.ser_chunk_index_len);
        let _ = Vec::from_raw_parts(
            output.sorted_hashes_ptr as *mut u64, 
            output.sorted_hashes_len, 
            output.sorted_hashes_len
        );
    }
}