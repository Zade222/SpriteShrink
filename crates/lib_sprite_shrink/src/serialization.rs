//! Manages the serialization of archive data and metadata.
//!
//! This module provides the functions needed to convert in-memory data
//! structures into a binary format suitable for writing to a final
//! archive file. It handles the serialization of the file manifest, the
//! chunk index, and the compressed data store, ensuring all components
//! are correctly ordered and formatted.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
};

use bitcode::{Encode, encode};
use dashmap::DashMap;
use thiserror::Error;

use crate::lib_error_handling::SpriteShrinkError;
use crate::lib_structs::{
    ALGORITHM_ZSTD, SSMC_UID,
    CompressionResult, FileHeader, FileManifestParent,
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

/// Prepares and serializes all data necessary for the final archive assembly.
///
/// This function acts as a final preparation step before the archive is
/// constructed. It takes the collected file metadata and organizes it into a
/// consistent, serializable format. It only reads the manifests: no chunk
/// data is needed.
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
///
/// # Arguments
///
/// * `file_manifest`: A thread-safe `DashMap` where each key is a filename and
///   the value is the corresponding `FileManifestParent` struct containing all
///   of its metadata.
///
/// # Returns
///
/// The table of contents and file manifests, both sorted by filename, and
/// every chunk hash the files use, once each, in the order to lay the
/// chunks out in.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must be `Copy`, `Ord`, `Eq` and
///   `Hash`.
#[must_use]
pub fn serialize_uncompressed_data<H>(
    file_manifest: &DashMap<String, FileManifestParent<H>>,
) -> SerializedData<H>
where
    H: Copy + Ord + Eq + std::hash::Hash,
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

    let mut chunk_freq = HashMap::new();
    for fmp in &manifests {
        for chunk in &fmp.chunk_metadata{
            *chunk_freq.entry(chunk.hash).or_insert(0) += 1;
        }
    }

    let mut scored_manifests: Vec<(&FileManifestParent<H>, u32)> = manifests
        .iter()
        .map(|fmp| {
            let score: u32 = fmp
                .chunk_metadata
                .iter()
                .map(|chunk| chunk_freq.get(&chunk.hash).copied().unwrap_or(0))
                .sum();
            (fmp, score)
        }).collect();

    /*Sorts the manifests in descending order of the score: Reverse flips
    the key's ordering, so the highest score sorts first.*/
    scored_manifests.sort_by_key(|&(_, score)| Reverse(score));

    //chunk_freq has one entry per hash the files use.
    let mut sorted_hashes = Vec::with_capacity(chunk_freq.len());
    let mut seen_hashes = HashSet::with_capacity(chunk_freq.len());

    /*Store the order of the chunks in order of the file with the most shared
    chunks to the least.*/
    for (fmp, _score) in scored_manifests {
        for chunk in &fmp.chunk_metadata{
            if seen_hashes.insert(chunk.hash){
                sorted_hashes.push(chunk.hash);
            }
        }
    }

    SerializedData {
        ser_file_manifest: manifests,
        archive_toc: toc,
        sorted_hashes,
    }
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

    /// The files' manifests, keyed by name.
    fn manifests(
        files: &[(&str, FileManifestParent<u64>)],
    ) -> DashMap<String, FileManifestParent<u64>> {
        files
            .iter()
            .map(|(name, manifest)| ((*name).to_owned(), manifest.clone()))
            .collect()
    }

    #[test]
    fn the_toc_is_sorted_by_filename_and_lines_up_with_the_manifests() {
        let manifests = manifests(&[
            ("c.bin", manifest(&[(1, 10)])),
            ("a.bin", manifest(&[(2, 10), (3, 10)])),
            ("b.bin", manifest(&[(4, 10), (5, 10), (6, 10)])),
        ]);
        let data = serialize_uncompressed_data(&manifests);

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
        let manifests = manifests(&[("file.bin", shuffled)]);

        let data = serialize_uncompressed_data(&manifests);

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
        let manifests = manifests(&[("empty.bin", manifest(&[])), ("a.bin", manifest(&[(1, 5)]))]);
        let data = serialize_uncompressed_data(&manifests);
        assert_eq!(data.archive_toc[1].filename, "empty.bin");
        assert_eq!(data.archive_toc[1].uncompressed_size, 0);
    }

    #[test]
    fn sorted_hashes_holds_each_used_hash_once() {
        // Chunk 1 is in every file, and twice in a.bin.
        let manifests = manifests(&[
            ("a.bin", manifest(&[(1, 10), (2, 10), (1, 10)])),
            ("b.bin", manifest(&[(1, 10), (3, 10)])),
            ("c.bin", manifest(&[(4, 10), (1, 10)])),
        ]);
        let data = serialize_uncompressed_data(&manifests);

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
        let manifests = manifests(&[
            ("a.bin", manifest(&[(1, 10), (3, 10), (20, 10), (21, 10)])),
            ("m.bin", manifest(&[(1, 10), (2, 10), (30, 10), (31, 10)])),
            ("z.bin", manifest(&[(1, 10), (2, 10), (3, 10), (10, 10)])),
        ]);
        let data = serialize_uncompressed_data(&manifests);

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
