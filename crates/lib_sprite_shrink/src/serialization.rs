//! Manages the serialization of archive data and metadata.
//!
//! This module provides the functions needed to convert in-memory data
//! structures into a binary format suitable for writing to a final
//! archive file. It handles the serialization of the file manifest, the
//! chunk index, and the compressed data store, ensuring all components
//! are correctly ordered and formatted.

use std::collections::HashMap;

use dashmap::DashMap;

use crate::lib_error_handling::LibError;
use crate::lib_structs::{ChunkLocation, FileManifestParent};

/// A trait abstracting access to a key-value store of chunk data.
///
/// This trait provides a generic interface for retrieving chunk data from
/// different underlying map implementations, such as `HashMap` or `DashMap`.
/// It is used to allow serialization logic to operate on either a
/// thread-safe or non-thread-safe data store without duplication.
pub trait ChunkStore<H: Eq + std::hash::Hash> {
    /// Retrieves a reference to a chunk's data by its hash.
    ///
    /// # Arguments
    ///
    /// * `hash`: The unique `u64` hash of the chunk to retrieve.
    ///
    /// # Returns
    ///
    /// An `Option` containing a reference to the chunk's byte `Vec`.
    /// The reference is wrapped in a type that dereferences to `Vec<u8>`.
    /// Returns `None` if no chunk with the specified hash is found.
    fn get_chunk(&self, hash: &H) -> 
        Option<impl std::ops::Deref<Target = Vec<u8>>>;
}

/// Implements `ChunkStore` for a standard, single-threaded `HashMap`.
///
/// This implementation allows the generic serialization functions to
/// operate on a `HashMap` that maps chunk hashes to their byte data.
impl <H: Eq + std::hash::Hash> ChunkStore<H> for HashMap<H, Vec<u8>> {
    fn get_chunk(&self, hash: &H) -> Option<impl std::ops::Deref<Target = Vec<u8>>> {
        self.get(hash)
    }
}

/// Implements `ChunkStore` for a thread-safe `DashMap`.
///
/// This implementation allows the generic serialization functions to
/// operate on a `DashMap`. It provides read access to the map in a
/// concurrent context, returning a read guard that dereferences to the
/// chunk's data.
impl <H: Eq + std::hash::Hash + Clone> ChunkStore<H> for DashMap<H, Vec<u8>> {
    fn get_chunk(&self, hash: &H) -> Option<impl std::ops::Deref<Target = Vec<u8>>> {
        self.get(hash)
    }
}

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

/// Serializes a chunk store into a byte vector and generates an index.
///
/// This function transforms a map of unique data chunks into a single,
/// ordered byte vector. It also generates a `chunk_index` that maps each
/// chunk's hash to its precise offset and length within this new data
/// block. It is generic over any type that implements `ChunkStore`.
///
/// # Arguments
///
/// * `store`: A reference to a type implementing `ChunkStore`, which
///   contains the chunk data mapped by hash.
/// * `sorted_hashes`: A slice of `u64` hashes, sorted to ensure a
///   deterministic order for the serialized data.
///
/// # Returns
///
/// A tuple containing:
/// - A `Vec<u8>` with all chunk data concatenated in order.
/// - A `HashMap<u64, ChunkLocation>` that serves as the chunk index,
///   mapping each hash to its location in the returned byte vector.
pub fn serialize_store<H, S>(
    store: &S,
    sorted_hashes: &[H],
) -> Result<(Vec<u8>, HashMap<H, ChunkLocation>), LibError>
where
    H: Copy + Eq + std::hash::Hash + std::fmt::Display,
    S: ChunkStore<H>,
{
    let total_size = sorted_hashes
        .iter()
        .map(|hash| store.get_chunk(hash).map_or(0, |data| data.len()))
        .sum();
    
    let (data_store, chunk_index, _offset) = sorted_hashes.iter().try_fold(
        (
            Vec::with_capacity(total_size),
            HashMap::with_capacity(sorted_hashes.len()),
            0u64, //Current_offset
        ),
        |(mut data_vec, mut index_map, mut offset), hash| {
            if let Some(data_entry) = store.get_chunk(hash) {
                let data = &*data_entry;
                let data_len = data.len() as u64;

                index_map.insert(
                    *hash,
                    ChunkLocation {
                        offset,
                        length: data_len as u32,
                    },
                );

                data_vec.extend_from_slice(data);
                offset += data_len;
                
                Ok((data_vec, index_map, offset))
            } else {
                //If a chunk is missing, return an error
                Err(LibError::SerializationMissingChunkError(hash.to_string()))
            }
        },
    )?;

    Ok((data_store, chunk_index))
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
pub fn serialize_uncompressed_data<H>(
    file_manifest: &DashMap<String, FileManifestParent<H>>,
    data_store: &HashMap<H, Vec<u8>>
) -> Result<(
    Vec<FileManifestParent<H>>/*ser_file_manifest */, 
    Vec<u8> /*ser_data_store */, 
    HashMap<H, ChunkLocation> /*chunk_index */,
    Vec<H> /*sorted_hashes */
), LibError>
where
    H: Copy + Ord + Eq + std::hash::Hash + std::fmt::Display,
{
    let mut ser_file_manifest = dashmap_values_to_vec(file_manifest);

    ser_file_manifest.sort_by(|a, b| a.filename.cmp(&b.filename));

    /*Put each files chunks in order from the beginning of the file to the end
    for easier processing when rebuilding file. */
    ser_file_manifest.iter_mut().for_each(|fmp| {
        fmp.chunk_metadata.sort_by_key(|metadata| metadata.offset);
    });

    let mut sorted_hashes: Vec<H> = data_store.keys().copied().collect();
    
    sorted_hashes.sort_unstable();

    let (ser_data_store, chunk_index) = serialize_store(data_store, &sorted_hashes)?;
    
    Ok((
        ser_file_manifest, 
        ser_data_store, 
        chunk_index, 
        sorted_hashes
    ))
}
