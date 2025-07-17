//! Handles the in-memory processing and chunking of files.
//!
//! This module provides the core logic for taking raw file data,
//! breaking it down into content-defined chunks, and generating the
//! necessary metadata for deduplication and archival. It includes
//! functions for chunking and hashing, as well as for rebuilding and
//! verifying files from their chunks.

use std::collections::HashMap;

use dashmap::DashMap;
use fastcdc::v2020::{Chunk, FastCDC, Normalization};
use sha2::{Digest,Sha512};
use xxhash_rust::xxh3::xxh3_64_with_seed;


use crate::lib_error_handling::LibError;
use crate::lib_structs::{
    ChunkLocation, FileData, FileManifestParent, ProcessedFileData, 
    SSAChunkMeta
};
use crate::parsing::{SS_SEED};


/// Processes a single file that has been loaded into memory.
///
/// This function takes a `FileData` struct, containing the file's name
/// and its contents. It then performs content-defined chunking on the
/// data and generates a SHA-512 verification hash. The results are
/// bundled into a `ProcessedFileData` struct, ready for the next stage
/// of archiving. Empty files are skipped.
///
/// # Arguments
///
/// * `file_data`: A `FileData` struct with the in-memory file data.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Some(ProcessedFileData))` on successful processing.
/// - `Ok(None)` if the input file data is empty.
/// - `Err(LibError)` if chunking or other processing fails.
pub fn process_file_in_memory(
    file_data: FileData,
    window_size: u64,
) -> Result<Option<ProcessedFileData>, LibError> {
    if file_data.file_data.is_empty() {
        return Ok(None);
    }

    let veri_hash = generate_sha_512(&file_data.file_data);
    let chunks = chunk_data(&file_data, SS_SEED, window_size)?;

    Ok(Some(ProcessedFileData {
        file_name: file_data.file_name,
        veri_hash,
        chunks,
        file_data: file_data.file_data,
    }))
}

/// Generates a SHA-512 hash for a given data slice.
///
/// This function computes a cryptographic hash of the input data, which
/// is used as a robust verification checksum. This allows the system to
/// confirm that a file has been reconstructed correctly by comparing the
/// hash of the rebuilt file with the original.
///
/// # Arguments
///
/// * `data`: A byte slice (`&[u8]`) of the data to be hashed.
///
/// # Returns
///
/// A 64 byte array representing the computed SHA-512 hash.
fn generate_sha_512(data: &[u8]) -> [u8; 64]{
    let mut hasher = Sha512::new();

    hasher.update(data);

    hasher.finalize().into()
}

/// Splits file data into content-defined chunks using FastCDC.
///
/// This function applies a content-defined chunking algorithm to a byte
/// slice of file data. It identifies boundaries based on the content
/// itself, which is effective for deduplication, as identical data
/// segments will produce identical chunks.
///
/// # Arguments
///
/// * `file_data`: A reference to the `FileData` struct to be chunked.
/// * `seed`: A `u64` value to initialize the chunking algorithm.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<Chunk>)` containing all the identified data chunks.
/// - `Err(LibError)` if any internal error occurs during chunking.
fn chunk_data(file_data: &FileData, seed: u64, window_size: u64) -> 
    Result<Vec<Chunk>, LibError>
{
    let chunker = FastCDC::with_level_and_seed(
        &file_data.file_data, 
        (window_size/4) as u32, 
        window_size as u32, 
        (window_size*4) as u32, 
        Normalization::Level1, seed);

    let chunks: Vec<Chunk> = chunker.collect();

    Ok(chunks)
}

/// Creates a file manifest and a list of data chunks.
///
/// This function takes the results of the chunking process and generates
/// two key outputs: a `FileManifestParent` struct, which contains all
/// metadata needed to reconstruct the file, and a vector of tuples,
/// where each tuple holds a unique chunk hash and its raw data.
///
/// # Arguments
///
/// * `file_name`: The name of the file being processed.
/// * `file_data`: A byte slice of the complete, original file data.
/// * `chunks`: A slice of `Chunk` structs that describe the file divisions.
///
/// # Returns
///
/// A tuple containing:
/// - A `FileManifestParent` for the processed file.
/// - A `Vec` where each element is a tuple of a chunk's hash and its data.
pub fn create_file_manifest_and_chunks(
    file_name: &str,
    file_data: &[u8],
    chunks: &[Chunk],
) -> (FileManifestParent, Vec<(u64, Vec<u8>)>) {
    let mut chunk_metadata = Vec::with_capacity(chunks.len());
    let mut chunk_data_list = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        let chunk_data_slice = &file_data[chunk.offset..chunk.offset + chunk.length];
        let data_hash = xxh3_64_with_seed(chunk_data_slice, SS_SEED);

        //Add the chunk's data to our list for returning
        chunk_data_list.push((data_hash, chunk_data_slice.to_vec()));

        //Populate the metadata for the manifest
        chunk_metadata.push(SSAChunkMeta {
            hash: data_hash,
            offset: chunk.offset as u32,
            length: chunk.length as u32,
        });
    }

    //Build file_manifest
    let file_manifest = FileManifestParent {
        filename: file_name.to_string(),
        chunk_count: chunks.len() as u64,
        chunk_metadata,
    };

    (file_manifest, chunk_data_list)
}

/// Rebuilds a single file from chunks and verifies its integrity.
///
/// This function reconstructs a file in memory by fetching each of its
/// data chunks from the serialized data store, using the chunk index for
/// lookups. After reassembling the file, it computes a SHA-512 hash of
/// the result and compares it against the original file's hash to ensure
/// data integrity.
///
/// # Arguments
///
/// * `fmp`: A reference to the `FileManifestParent` for the file.
/// * `ser_data_store`: A byte slice of the complete data store.
/// * `chunk_index`: A map from chunk hashes to their locations.
/// * `veri_hashes`: A map from filenames to their original SHA-512 hashes.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the file is rebuilt and its hash matches.
/// - `Err(LibError)` if a chunk is missing or the hash mismatches.
pub fn rebuild_and_verify_single_file(
    fmp: &FileManifestParent,
    ser_data_store: &[u8],
    chunk_index: &HashMap<u64, ChunkLocation>,
    veri_hashes: &DashMap<String, [u8; 64]>,
) -> Result<(), LibError> {
    //Calculate the total size of the original file to pre-allocate memory.
    let file_size: usize = fmp.chunk_metadata.iter().map(|scm|{
        scm.length as usize}).sum();
    let mut data_vec: Vec<u8> = Vec::with_capacity(file_size);

    //Rebuild file
    for scm in &fmp.chunk_metadata {
        if let Some(location) = chunk_index.get(&scm.hash) {
            let start = location.offset as usize;
            let end = start + location.length as usize;
            data_vec.extend_from_slice(&ser_data_store[start..end]);
        } else {
            return Err(LibError::InternalLibError(format!(
                "Verification failed: Missing chunk with hash {} for file '{}'",
                scm.hash, fmp.filename
            )));
        } 
    }

    //Verify hydrated file hash aginst original
    if let Some(expected_hash) = veri_hashes.get(&fmp.filename) {
        let built_file_hash = generate_sha_512(&data_vec);
        if *expected_hash.value() != built_file_hash {
            return Err(LibError::HashMismatchError(fmp.filename.clone()));
        }
    } else {
        // This case would also indicate an internal inconsistency.
        return Err(LibError::InternalLibError(format!(
            "Verification failed: Missing original hash for file '{}'",
            fmp.filename
        )));
    }

    Ok(())
}