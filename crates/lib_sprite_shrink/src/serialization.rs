//! Manages the serialization of archive data and metadata.
//!
//! This module provides the functions needed to convert in-memory data
//! structures into a binary format suitable for writing to a final
//! archive file. It handles the serialization of the file manifest, the
//! chunk index, and the compressed data store, ensuring all components
//! are correctly ordered and formatted.

use std::collections::HashMap;

use dashmap::DashMap;
use thiserror::Error;

use crate::lib_error_handling::{
    IsCancelled, SpriteShrinkError
};
use crate::lib_structs::{ChunkLocation, FileManifestParent};

#[derive(Error, Debug)]
pub enum SerializationError {
    #[error("Chunk missing: {0}")]
    MissingChunk(String),

    #[error("An error occurred in an external callback: {0}")]
    External(String),

    #[error("Operation cancelled by user.")]
    Cancelled,
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

/// Serializes a chunk store and generates a corresponding index.
///
/// This function is responsible for creating a `chunk_index`, which maps each
/// unique data chunk's hash to its precise offset and length within a
/// conceptual, serialized data block. Rather than building the entire data
/// block in memory, this function calculates the layout and returns the index,
/// making it memory-efficient for large datasets.
///
/// The function operates on data retrieved via a callback
/// (`data_store_get_chunk_cb`). This design decouples the serialization logic
/// from the underlying storage mechanism, allowing the caller to supply chunk
/// data from various sources, such as an in-memory `HashMap`, a thread-safe
/// `DashMap`, or a persistent key-value store like a database.
///
/// # Arguments
///
/// * `sorted_hashes`: A slice of hashes that have been sorted into a
///   deterministic order. This consistent ordering is critical for ensuring
///   that the generated chunk index and its offsets are correct and 
///   reproducible.
/// * `data_store_get_chunk_cb`: A callback function that the serializer uses 
///   to fetch the raw byte data for a given set of hashes. It must return a
///   `Vec<Vec<u8>>` where the data for each chunk is in the same order as the
///   input hashes.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(HashMap<H, ChunkLocation>)` on success. The `HashMap` is the complete
///   chunk index, where each key is a chunk's hash and the value is its
///   `ChunkLocation` (offset and length) in the final serialized data blob.
/// - `Err(SerializationError::SerializationMissingChunkError)` if the
///   `data_store_get_chunk_cb` returns empty data for any requested hash,
///   which indicates that a required chunk is missing from the data store.
///
/// # Type Parameters
///
/// * `D`: The type of the `data_store_get_chunk_cb` callback. It must be a
///   closure that implements `Fn(&[H]) -> Vec<Vec<u8>>`.
/// * `H`: The generic hash type used for identifying chunks. It must be 
///   `Copy`,
///   `Eq`, `Hash`, and `Display`.
pub fn serialize_store<D, E, H>(
    sorted_hashes: &[H],
    data_store_get_chunk_cb: &D
) -> Result<HashMap<H, ChunkLocation>, SerializationError>
where
    D: Fn(&[H]) -> Result<Vec<Vec<u8>>, E>,
    E: std::error::Error + Send + Sync + 'static,
    H: Copy + Eq + std::hash::Hash + std::fmt::Display,
{
    
    let (chunk_index, _offset) = sorted_hashes.iter().try_fold(
        (
            HashMap::with_capacity(sorted_hashes.len()),
            0u64, //Current_offset
        ),
        |(mut index_map, mut offset), hash| {
            let data_entry = &data_store_get_chunk_cb(&[*hash])
            .map_err(|e| SerializationError::External(e.to_string()))?
            .remove(0);
            
            if !data_entry.is_empty() {
                let data = data_entry;
                let data_len = data.len() as u64;

                index_map.insert(
                    *hash,
                    ChunkLocation {
                        offset,
                        compressed_length: data_len as u32,
                    },
                );

                offset += data_len;
                
                Ok((index_map, offset))
            } else {
                //If a chunk is missing, return an error
                Err(SerializationError::MissingChunk(hash.to_string()))
            }
        },
    )?;

    Ok(chunk_index)
}

/// Prepares and serializes all data necessary for the final archive assembly.
///
/// This function acts as a final preparation step before the archive is
/// constructed. It takes the collected file metadata and the unique data 
/// chunks and organizes them into a consistent, serializable format.
///
/// The key operations performed are:
/// 1.  **Sorting the File Manifest**: It converts the file manifest from a
///     `DashMap` into a `Vec` and sorts it alphabetically by filename. This
///     ensures that the file listing in the final archive is deterministic.
/// 2.  **Sorting Chunk Metadata**: For each file in the manifest, it sorts the
///     chunk metadata by the original byte offset. This is critical for 
///     ensuring that files can be correctly reconstructed in order during 
///     extraction.
/// 3.  **Collecting and Sorting Hashes**: It retrieves all unique chunk hashes
///     from the data store (via the `data_store_key_cb`) and sorts them. This
///     provides a canonical order for both the chunk index and the compressed
///     data blob.
/// 4.  **Generating the Chunk Index**: It calls the `serialize_store` function
///     to create the final chunk index, which maps each sorted hash to its
///     location in the conceptual data blob.
///
/// # Arguments
///
/// * `file_manifest`: A thread-safe `DashMap` where each key is a filename and
///   the value is the corresponding `FileManifestParent` struct containing all
///   of its metadata.
/// * `data_store_key_cb`: A callback function that, when called, returns a
///   complete `Vec` of all unique chunk hashes from the data store.
/// * `data_store_get_chunk_cb`: A callback function that is passed to
///   `serialize_store` to retrieve the byte data for a given set of hashes.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok` on success, containing a tuple with four elements:
///   - `Vec<FileManifestParent<H>>`: The file manifest, sorted by filename.
///   - `HashMap<H, ChunkLocation>`: The complete chunk index, mapping each
///     hash to its location.
///   - `Vec<H>`: A vector of all unique chunk hashes, sorted in a
///     deterministic order.
/// - `Err(SpriteShrinkError)` if any part of the serialization process fails, such as a
///   missing chunk in the data store.
///
/// # Type Parameters
///
/// * `D`: The type of the `data_store_get_chunk_cb` callback.
/// * `H`: The generic hash type, which must be `Copy`, `Ord`, `Eq`, `Hash`,
///   and `Display`.
/// * `K`: The type of the `data_store_key_cb` callback.
pub fn serialize_uncompressed_data<D, E, H, K>(
    file_manifest: &DashMap<String, FileManifestParent<H>>,
    data_store_key_cb: &K,
    data_store_get_chunk_cb: &D
) -> Result<(
    Vec<FileManifestParent<H>>/*ser_file_manifest */, 
    HashMap<H, ChunkLocation> /*chunk_index */,
    Vec<H> /*sorted_hashes */
), SpriteShrinkError>
where
    D: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    H: Copy + Ord + Eq + std::hash::Hash + std::fmt::Display,
    K: Fn() -> Result<Vec<H>, E>
{
    let mut ser_file_manifest = dashmap_values_to_vec(file_manifest);

    ser_file_manifest.sort_by(|a, b| a.filename.cmp(&b.filename));

    /*Put each files chunks in order from the beginning of the file to the end
    for easier processing when rebuilding file. */
    ser_file_manifest.iter_mut().for_each(|fmp| {
        fmp.chunk_metadata.sort_by_key(|metadata| metadata.offset);
    });

    let mut sorted_hashes: Vec<H> = match data_store_key_cb() {
        Ok(hashes) => hashes,
        Err(e) => {
            if e.is_cancelled() {
                return Err(SpriteShrinkError::Cancelled);
            }
            return Err(SpriteShrinkError::External(Box::new(e)));
        }
    };
    
    
    sorted_hashes.sort_unstable();

    let chunk_index: HashMap<H, ChunkLocation> = 
        serialize_store(&sorted_hashes, data_store_get_chunk_cb)?;
    
    Ok((
        ser_file_manifest, 
        chunk_index, 
        sorted_hashes
    ))
}
