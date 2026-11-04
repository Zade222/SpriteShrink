//! Manages the serialization of archive data and metadata.
//!
//! This module provides the functions needed to convert in-memory data
//! structures into a binary format suitable for writing to a final
//! archive file. It handles the serialization of the file manifest, the
//! chunk index, and the compressed data store, ensuring all components
//! are correctly ordered and formatted.

use std::collections::{HashMap, HashSet};

use bitcode::{Encode, encode};
use dashmap::DashMap;
use thiserror::Error;

use crate::lib_error_handling::{
    IsCancelled, SpriteShrinkError
};
use crate::lib_structs::{
    ALGORITHM_ZSTD, SSMC_UID,
    ChunkLocation, CompressionResult, FileHeader, FileManifestParent,
    SerializedData, SSMCFormatData, SSMCTocEntry
};

#[derive(Error, Debug)]
pub enum SerializationError {
    #[error("Archive is too large for the format: {0}")]
    ArchiveTooLarge(String),

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
/// 1.  **Sorting the File Manifest**: The file manifest is sorted
///     alphabetically by filename. This provides a deterministic,
///     user friendly order for the logical file listing within the archive.
/// 2.  **Sorting Chunk Metadata**: For each file in the manifest, its
///     constituent chunks are sorted by their original byte offset. This is
///     critical for ensuring files can be correctly reconstructed during
///     extraction.
/// 3.  **Optimizing Chunk Layout**: The physical layout of chunks in the data
///     blob is optimized for fast extraction. This is achieved by:
///     a. Analyzing how many times each unique chunk is used across all files.
///     b. Scoring each file based on the frequency of its chunks. Files with
///     more shared chunks receive higher scores.
///     c. Building the final list of hashes (`sorted_hashes`) by processing
///     files in descending order of their score. This places the most
///     commonly shared data together at the start of the archive, reducing
///     disk seek time during extraction.
/// 4.  **Generating the Chunk Index**: It calls the `serialize_store` function
///     to create the final chunk index, which maps each hash from the
///     optimized layout to its location in the data blob.
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
/// - `Ok` on success, containing a struct with three fields:
///   - `Vec<FileManifestParent<H>>`: The file manifest, sorted by filename.
///   - `HashMap<H, ChunkLocation>`: The complete chunk index, mapping each
///     hash to its location.
///   - `Vec<H>`: A vector of all unique chunk hashes, sorted in a
///     deterministic order.
/// - `Err(SpriteShrinkError)` if any part of the serialization process fails,
///   such as a missing chunk in the data store.
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
) -> Result<SerializedData<H>, SpriteShrinkError>
where
    D: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    E: std::error::Error + IsCancelled + Send + Sync + 'static,
    H: Copy + Ord + Eq + std::hash::Hash + std::fmt::Display,
    K: Fn() -> Result<Vec<H>, E>
{
    let mut entries: Vec<(String, FileManifestParent<H>)> = file_manifest
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect();

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut toc = Vec::with_capacity(entries.len());
    let mut manifests = Vec::with_capacity(entries.len());

    for (filename, mut manifest) in entries {
        /*Put each file's chunks in order from the beginning of the file to
        the end, to aid in processing when rebuilding the file. The size is
        read after sorting: the last chunk by offset is the one that ends
        the file, whatever order the chunks arrived in.*/
        manifest.chunk_metadata.sort_by_key(|metadata| metadata.offset);

        let uncomp_size = manifest.chunk_metadata.last().map_or(0, |chunk| {
            chunk.offset + u64::from(chunk.length)
        });

        toc.push(SSMCTocEntry {
            filename,
            uncompressed_size: uncomp_size,
        });

        manifests.push(manifest);
    }

    let mut serialized_data = SerializedData::<H> {
        ser_file_manifest: manifests,
        archive_toc: toc,
        ..Default::default()
    };

    let mut chunk_freq = HashMap::new();
    for fmp in &serialized_data.ser_file_manifest{
        for chunk in &fmp.chunk_metadata{
            *chunk_freq.entry(chunk.hash).or_insert(0) += 1;
        }
    }

    let mut scored_indices: Vec<_> = serialized_data
        .ser_file_manifest
        .iter()
        .enumerate()
        .map(|(i, fmp)| {
            let score: u32 = fmp
                .chunk_metadata
                .iter()
                .map(|chunk| chunk_freq.get(&chunk.hash).copied().unwrap_or(0))
                .sum();
            (i, score)
        }).collect();

    /*Sorts each index, which points to the position in the FileManifestParent
    vector, in descending order of the score.*/
    scored_indices.sort_by(|a, b| b.1.cmp(&a.1));

    let total_hashes = match data_store_key_cb() {
        Ok(hashes) => hashes.len(),
        Err(e) => {
            if e.is_cancelled() {
                return Err(SpriteShrinkError::Cancelled);
            }
            return Err(SpriteShrinkError::External(Box::new(e)));
        }
    };
    let mut sorted_hashes = Vec::with_capacity(total_hashes);
    let mut seen_hashes = HashSet::with_capacity(total_hashes);

    /*Store the order of the chunks in order of the file with the most shared
    chunks to the least.*/
    for(index, _score) in scored_indices{
        let fmp = &serialized_data.ser_file_manifest[index];
        for chunk in &fmp.chunk_metadata{
            if seen_hashes.insert(chunk.hash){
                sorted_hashes.push(chunk.hash);
            }
        }
    }

    serialized_data.sorted_hashes = sorted_hashes;

    serialized_data.chunk_index = serialize_store(
        &serialized_data.sorted_hashes,
        data_store_get_chunk_cb
    )?;

    Ok(serialized_data)
}

/// Builds every byte of an SSMC archive that comes before the compressed
/// data: the file header, table of contents, format data, file manifest,
/// dictionary and chunk index, in the order the specification lays them out.
///
/// The host writes the returned prefix and then appends the compressed chunk
/// data that [`ArchiveBuilder::build`] passed to its write callback. The
/// prefix is always `metadata_prefix_end()` bytes long, so the compressed
/// data starts exactly where the format data says it does.
///
/// # Arguments
///
/// * `toc`: One entry per file, in the order the files are numbered.
/// * `manifests`: One manifest per file, in the same order as `toc`.
/// * `comp`: The dictionary and encoded chunk index from
///   [`ArchiveBuilder::build`].
/// * `hash_type`: The hash used for the chunk hashes:
///   [`HASH_TYPE_XXH3_64`] or [`HASH_TYPE_XXH3_128`].
///
/// # Errors
///
/// Returns `SerializationError::ArchiveTooLarge` if there are more than
/// `u32::MAX` files, or the encoded table of contents is too large for the
/// header's 32-bit offsets.
///
/// [`ArchiveBuilder::build`]: crate::ArchiveBuilder::build
/// [`HASH_TYPE_XXH3_64`]: crate::HASH_TYPE_XXH3_64
/// [`HASH_TYPE_XXH3_128`]: crate::HASH_TYPE_XXH3_128
pub fn build_ssmc_prefix<H>(
    toc: &[SSMCTocEntry],
    manifests: &[FileManifestParent<H>],
    comp: &CompressionResult,
    hash_type: u8,
) -> Result<Vec<u8>, SpriteShrinkError>
where
    H: Encode,
{
    let enc_toc = encode(toc);
    let enc_manifest = encode(manifests);

    let file_header = FileHeader::build_file_header(
        toc.len(),
        ALGORITHM_ZSTD,
        hash_type,
        SSMC_UID,
        enc_toc.len(),
    )?;

    /*Offsets come from the lengths of the bytes actually written, so the
    format data always matches the prefix.*/
    let format_data = SSMCFormatData::build_format_data(
        file_header.format_data_offset as usize,
        enc_manifest.len(),
        comp.dictionary.len() as u64,
        comp.enc_chunk_index.len() as u64,
    );

    let mut prefix = Vec::with_capacity(
        size_of::<FileHeader>() + enc_toc.len() +
            size_of::<SSMCFormatData>() + enc_manifest.len() +
            comp.dictionary.len() + comp.enc_chunk_index.len()
    );

    prefix.extend_from_slice(&file_header.to_le_bytes());
    prefix.extend_from_slice(&enc_toc);
    prefix.extend_from_slice(&format_data.to_le_bytes());
    prefix.extend_from_slice(&enc_manifest);
    prefix.extend_from_slice(&comp.dictionary);
    prefix.extend_from_slice(&comp.enc_chunk_index);

    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib_structs::SSAChunkMeta;

    type ReadResult = Result<Vec<Vec<u8>>, SpriteShrinkError>;

    /// A manifest for `chunks`, given as (hash, length) in file order, with
    /// each chunk's offset where the previous one ended.
    fn manifest(chunks: &[(u64, u32)]) -> FileManifestParent<u64> {
        let mut offset = 0;
        let chunk_metadata = chunks
            .iter()
            .map(|&(hash, length)| {
                let meta = SSAChunkMeta { hash, offset, length };
                offset += u64::from(length);
                meta
            })
            .collect();
        FileManifestParent {
            chunk_count: chunks.len() as u64,
            chunk_metadata,
        }
    }

    /// The files' manifests by name, and a store holding `length` bytes of
    /// `hash as u8` for every chunk they use.
    fn archive_input(
        files: &[(&str, FileManifestParent<u64>)],
    ) -> (DashMap<String, FileManifestParent<u64>>, HashMap<u64, Vec<u8>>) {
        let mut store = HashMap::new();
        for (_name, manifest) in files {
            for chunk in &manifest.chunk_metadata {
                let byte = u8::try_from(chunk.hash % 256).unwrap();
                store.insert(chunk.hash, vec![byte; chunk.length as usize]);
            }
        }
        let manifests = files
            .iter()
            .map(|(name, manifest)| ((*name).to_owned(), manifest.clone()))
            .collect();
        (manifests, store)
    }

    /// A read callback that returns each requested chunk from `store`.
    fn read_from(
        store: HashMap<u64, Vec<u8>>,
    ) -> impl Fn(&[u64]) -> ReadResult + Send + Sync + 'static {
        move |hashes| Ok(hashes.iter().map(|hash| store[hash].clone()).collect())
    }

    /// Runs `serialize_uncompressed_data` with a key callback that lists
    /// every chunk in `store`.
    fn serialize(
        manifests: &DashMap<String, FileManifestParent<u64>>,
        store: HashMap<u64, Vec<u8>>,
    ) -> Result<SerializedData<u64>, SpriteShrinkError> {
        let keys: Vec<u64> = store.keys().copied().collect();
        serialize_uncompressed_data(
            manifests,
            &move || -> Result<Vec<u64>, SpriteShrinkError> { Ok(keys.clone()) },
            &read_from(store),
        )
    }

    #[test]
    fn the_toc_is_sorted_by_filename_and_lines_up_with_the_manifests() {
        let (manifests, store) = archive_input(&[
            ("c.bin", manifest(&[(1, 10)])),
            ("a.bin", manifest(&[(2, 10), (3, 10)])),
            ("b.bin", manifest(&[(4, 10), (5, 10), (6, 10)])),
        ]);
        let data = serialize(&manifests, store).unwrap();

        let names: Vec<&str> = data
            .archive_toc
            .iter()
            .map(|entry| entry.filename.as_str())
            .collect();
        assert_eq!(names, ["a.bin", "b.bin", "c.bin"]);

        // Each TOC entry's manifest is the one at the same index.
        for (entry, manifest) in data.archive_toc.iter().zip(&data.ser_file_manifest) {
            assert_eq!(
                manifest.chunk_count,
                manifests.get(&entry.filename).unwrap().chunk_count,
                "{}",
                entry.filename
            );
            assert_eq!(entry.uncompressed_size, manifest.chunk_count * 10);
        }
    }

    #[test]
    fn unordered_chunk_metadata_gets_the_right_size_and_order() {
        // The size used to be read from the last chunk before the chunks
        // were sorted, so it was only right if they arrived in order.
        let mut shuffled = manifest(&[(1, 100), (2, 30), (3, 7), (4, 50)]);
        shuffled.chunk_metadata.swap(0, 3);
        shuffled.chunk_metadata.swap(1, 2);
        let (manifests, store) = archive_input(&[("file.bin", shuffled)]);

        let data = serialize(&manifests, store).unwrap();

        assert_eq!(data.archive_toc[0].uncompressed_size, 187);
        let offsets: Vec<u64> = data.ser_file_manifest[0]
            .chunk_metadata
            .iter()
            .map(|chunk| chunk.offset)
            .collect();
        assert_eq!(offsets, [0, 100, 130, 137]);
    }

    #[test]
    fn an_empty_file_has_size_zero() {
        let (manifests, store) =
            archive_input(&[("empty.bin", manifest(&[])), ("a.bin", manifest(&[(1, 5)]))]);
        let data = serialize(&manifests, store).unwrap();
        assert_eq!(data.archive_toc[1].filename, "empty.bin");
        assert_eq!(data.archive_toc[1].uncompressed_size, 0);
    }

    #[test]
    fn sorted_hashes_holds_each_used_hash_once() {
        // Chunk 1 is in every file, and twice in a.bin.
        let (manifests, mut store) = archive_input(&[
            ("a.bin", manifest(&[(1, 10), (2, 10), (1, 10)])),
            ("b.bin", manifest(&[(1, 10), (3, 10)])),
            ("c.bin", manifest(&[(4, 10), (1, 10)])),
        ]);
        // A chunk in the store that no file uses isn't archived.
        store.insert(99, vec![0; 10]);

        let data = serialize(&manifests, store).unwrap();

        assert_eq!(data.sorted_hashes.len(), 4);
        let unique: HashSet<u64> = data.sorted_hashes.iter().copied().collect();
        assert_eq!(unique, HashSet::from([1, 2, 3, 4]));
    }

    #[test]
    fn the_file_sharing_the_most_chunks_is_laid_out_first() {
        // Similar files, as the archive is meant for. Chunks 1 to 3 are
        // shared: 1 by every file, 2 by z and m, 3 by z and a. z shares
        // the most, so its chunks come first, then each later file adds
        // only the chunks not yet laid out. a and m tie, and keep filename
        // order. Filename order alone would start with a.
        let (manifests, store) = archive_input(&[
            ("a.bin", manifest(&[(1, 10), (3, 10), (20, 10), (21, 10)])),
            ("m.bin", manifest(&[(1, 10), (2, 10), (30, 10), (31, 10)])),
            ("z.bin", manifest(&[(1, 10), (2, 10), (3, 10), (10, 10)])),
        ]);
        let data = serialize(&manifests, store).unwrap();

        assert_eq!(data.sorted_hashes, [1, 2, 3, 10, 20, 21, 30, 31]);
    }

    #[test]
    fn dashmap_values_to_vec_returns_every_value() {
        let map = DashMap::new();
        map.insert("a", 1);
        map.insert("b", 2);
        map.insert("c", 3);

        let mut values = dashmap_values_to_vec(&map);
        values.sort_unstable();
        assert_eq!(values, [1, 2, 3]);
    }
}
