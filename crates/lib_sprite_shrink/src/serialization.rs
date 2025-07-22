//! Manages the serialization of archive data and metadata.
//!
//! This module provides the functions needed to convert in-memory data
//! structures into a binary format suitable for writing to a final
//! archive file. It handles the serialization of the file manifest, the
//! chunk index, and the compressed data store, ensuring all components
//! are correctly ordered and formatted.

use std::collections::HashMap;
use std::slice;

use dashmap::DashMap;

use crate::ffi_structs::{FFIDataStoreEntry, FFIFileManifestParent, FFISerializedOutput};
use crate::lib_structs::{ChunkLocation, FileManifestParent, SSAChunkMeta};

/// Extracts all values from a DashMap into a vector.
///
/// This utility function iterates over a `DashMap`, clones each value,
/// and collects them into a new `Vec`. This is a convenient way to get a
/// snapshot of all values in the map for further processing, such as
/// sorting or serialization.
///
/// # Arguments
///
/// * `input_dash`: A reference to the `DashMap` to extract values from.
///
/// # Returns
///
/// A `Vec` containing clones of all the values from the input map.
pub fn dashmap_values_to_vec<T, R>(
    input_dash: &DashMap<T, R>
) -> Vec<R> 
where
    T: Eq + std::hash::Hash,
    R: Clone,
{
    // .iter() creates an iterator over the DashMap's entries.
    // .map() iterates through each entry and extracts a clone of the value.
    // .collect() assembles the cloned values into a Vec.
    input_dash.iter().map(|entry| entry.value().clone()).collect()
}

/// Serializes a data store into a byte vector and an index.
///
/// This function transforms a map of unique data chunks into a single,
/// ordered byte vector. It also generates a `chunk_index` that maps each
/// chunk's hash to its precise offset and length within this new data
/// block. This is a crucial step in preparing data for final archival.
///
/// # Arguments
///
/// * `data_store`: A map from chunk hashes to their raw byte data.
/// * `sorted_hashes`: A slice of hashes, sorted to ensure deterministic
///   ordering of the data in the final byte vector.
///
/// # Returns
///
/// A tuple containing:
/// - A `Vec<u8>` with all chunk data concatenated in order.
/// - A `HashMap` that serves as the chunk index.
pub fn serialize_data_store(
    data_store: &HashMap<u64, Vec<u8>>,
    sorted_hashes: &[u64],
) -> (Vec<u8>, HashMap<u64, ChunkLocation>){
    let total_size = sorted_hashes
        .iter()
        .map(|hash| data_store.get(hash).map_or(0, |data| data.len()))
        .sum();
    
    let (data_store, chunk_index, _offset) = sorted_hashes.iter().fold(
        (
            Vec::with_capacity(total_size),
            HashMap::with_capacity(sorted_hashes.len()),
            0u64, //Current_offset
        ),
        |(mut data_vec, mut index_map, mut offset), hash| {
            if let Some(data_entry) = data_store.get(hash) {
                let data = data_entry;
                let data_len = data.len() as u64;

                index_map.insert(
                    *hash,
                    ChunkLocation {
                        offset,
                        length: data_len as u32,
                    },
                );

                data_vec.extend_from_slice(&data);
                offset += data_len;
            }
            (data_vec, index_map, offset)
        },
    );

    (data_store, chunk_index)
}

/// Serializes a compressed data store into a contiguous byte vector.
///
/// This function is analogous to `serialize_data_store` but operates on
/// data that has already been compressed. It takes a map of chunk hashes
/// to their compressed byte data and serializes them into a single,
/// ordered `Vec<u8>`. It also creates an index mapping each hash to its
/// `ChunkLocation`.
///
/// # Arguments
///
/// * `compressed_store`: A map from chunk hashes to compressed byte data.
/// * `sorted_hashes`: A slice of hashes, sorted to ensure deterministic
///   ordering of the data in the final byte vector.
///
/// # Returns
///
/// A tuple containing:
/// - A `Vec<u8>` with all compressed chunk data concatenated.
/// - A `HashMap` that serves as the chunk index for the compressed data.
pub fn serialize_compressed_store(
    compressed_store: &DashMap<u64, Vec<u8>>,
    sorted_hashes: &[u64],
) -> (Vec<u8>, HashMap<u64, ChunkLocation>){
    let total_size = sorted_hashes
        .iter()
        .map(|hash| compressed_store.get(hash).map_or(0, |data| data.len()))
        .sum();
    
    let (data_store, chunk_index, _offset) = sorted_hashes.iter().fold(
        (
            Vec::with_capacity(total_size),
            HashMap::with_capacity(sorted_hashes.len()),
            0u64, //Current_offset
        ),
        |(mut data_vec, mut index_map, mut offset), hash| {
            if let Some(data_entry) = compressed_store.get(hash) {
                let data = data_entry;
                let data_len = data.len() as u64;

                index_map.insert(
                    *hash,
                    ChunkLocation {
                        offset,
                        length: data_len as u32,
                    },
                );

                data_vec.extend_from_slice(&data);
                offset += data_len;
            }
            (data_vec, index_map, offset)
        },
    );

    (data_store, chunk_index)
}

/// Prepares and serializes all data for the final archive.
///
/// This function orchestrates the serialization of the primary data
/// structures. It sorts the file manifests for consistent ordering,
/// then serializes the data store into a single byte vector and creates
/// a corresponding chunk index. The result is a tuple containing all
/// components needed for the final archival step.
///
/// # Arguments
///
/// * `file_manifest`: A map of filenames to their manifest data.
/// * `data_store`: A map of chunk hashes to their raw byte data.
///
/// # Returns
///
/// A tuple containing:
/// - A `Vec` of `FileManifestParent` sorted by filename.
/// - A `Vec<u8>` with all chunk data concatenated in order.
/// - A `HashMap` that serves as the chunk index.
/// - A sorted `Vec` of all unique chunk hashes.
pub fn serialize_uncompressed_data(file_manifest: &DashMap<String, FileManifestParent>,
    data_store: &HashMap<u64, Vec<u8>>) -> 
    (Vec<FileManifestParent>/*ser_file_manifest */, 
        Vec<u8> /*ser_data_store */, 
        HashMap<u64, ChunkLocation> /*chunk_index */,
        Vec<u64> /*sorted_hashes */)
{
    let mut ser_file_manifest = dashmap_values_to_vec(file_manifest);

    ser_file_manifest.sort_by(|a, b| a.filename.cmp(&b.filename));

    /*Put each files chunks in order from the beginning of the file to the end
    for easier processing when rebuilding file. */
    ser_file_manifest.iter_mut().for_each(|fmp| {
        fmp.chunk_metadata.sort_by_key(|metadata| metadata.offset);
    });

    let mut sorted_hashes: Vec<u64> = data_store.keys().copied().collect();
    
    sorted_hashes.sort_unstable();

    let (ser_data_store, chunk_index) = serialize_data_store(data_store, &sorted_hashes);
    
    (
        ser_file_manifest, 
        ser_data_store, 
        chunk_index, 
        sorted_hashes
    )
}

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
        let _ = Vec::from_raw_parts(output.ser_manifest_ptr as *mut u8, output.ser_manifest_len, output.ser_manifest_len);
        let _ = Vec::from_raw_parts(output.ser_data_store_ptr as *mut u8, output.ser_data_store_len, output.ser_data_store_len);
        let _ = Vec::from_raw_parts(output.ser_chunk_index_ptr as *mut u8, output.ser_chunk_index_len, output.ser_chunk_index_len);
        let _ = Vec::from_raw_parts(output.sorted_hashes_ptr as *mut u64, output.sorted_hashes_len, output.sorted_hashes_len);
    }
}
