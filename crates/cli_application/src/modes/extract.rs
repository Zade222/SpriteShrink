//! Implements the extraction mode for the command-line application.
//!
//! This module contains the `run_extraction` function, which is the main
//! entry point for the file extraction process. It handles reading an
//! archive's metadata, including the header, manifest, and chunk index,
//! then proceeds to decompress and write the files requested by the user
//! to the designated output directory.

use std::{
    fmt::Display,
    fs::{File, create_dir_all, rename},
    hash::Hash,
    io::{BufWriter, Write},
    path::Path,
    sync::{
        Arc, atomic::{AtomicBool, Ordering},
    },
};
use bitcode::Decode;
use flume::bounded;
use rayon::join;
use serde::{Deserialize, Serialize};
use tracing::{
    debug
};

use sprite_shrink::{
    Hashable, FileHeader, SSMCFormatData, SSMCTocEntry,
    decompress_chunk, parse_file_metadata, parse_file_chunk_index,
    parse_format_data
};

use sprite_shrink_cd::{
    SSMDFormatData, SSMDTocEntry,
    expand_exception_index, expand_sector_map, expand_subheader_map,
    decode_ssmd_format_data, decode_ssmd_meta_data
};

use crate::{
    archive_parser::{get_file_header, get_toc},
    arg_handling::Args,
    cli_types::TempFileGuard,
    error_handling::CliError,
    storage_io::{read_file_data, read_metadata_block},
};

/// Executes the file extraction process from an archive.
///
/// This is the main entry point for the extraction operation. It reads an
/// archive's metadata, including the header, manifest, and chunk index.
/// It then extracts the specific files requested by the user, identified
/// by their ROM indices. The extraction process is then performed sequentially
/// to avoid IO contention with multiple threads reading and writing to disk at
/// once.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the source archive file.
/// * `out_dir`: A `Path` for the directory where extracted files will
///   be written.
/// * `rom_indices`: A vector of `u8` indices specifying which files to
///   extract from the archive.
/// * `args`: A reference to the main `Args` struct, used here to configure
///   the number of worker threads for parallel processing.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if all requested files are extracted successfully.
/// - `Err(CliError)` if any part of the process fails.
///
/// # Errors
///
/// This function can return an error in several cases, including:
/// - `CliError` variants if reading the archive's header, manifest, or
///   chunk data fails.
/// - `CliError::InternalError` if a worker thread pool cannot be
///   created.
/// - `CliError::InvalidRomIndex` if a requested ROM index does not exist
///   in the archive.
/// - Any error propagated from the decompression or file writing stages.
pub fn run_extraction(
    file_path: &Path,
    out_dir: &Path,
    rom_indices: &Vec<u8>,
    args: &Args,
    running: Arc<AtomicBool>,
) -> Result<(), CliError> {
    /*Get and store the parsed header from the target archive file.*/
    let header = get_file_header(file_path)?;

    let bin_format_data = read_file_data(
        file_path,
        header.format_data_offset as u64,
        SSMCFormatData::SIZE as usize
    )?;

    let format_data = parse_format_data(&bin_format_data)?;

    let toc = get_toc(file_path, header.enc_toc_offset, header.enc_toc_length as usize)?;

    //Read all required metadata.
    let metadata_block = read_metadata_block(file_path, &format_data)?;

    /*Get the identifier for the hash type stored in the archive header.*/
    let hash_bit_length = header.hash_type;

    match hash_bit_length {
        1 => extract_ssmc_data::<u64>(
            file_path,
            out_dir,
            rom_indices,
            &format_data,
            &toc,
            &metadata_block,
            args,
            running,
        ),
        2 => extract_ssmc_data::<u128>(
            file_path,
            out_dir,
            rom_indices,
            &format_data,
            &toc,
            &metadata_block,
            args,
            running,
        ),
        _ => //Handle other cases or return an error for unsupported hash types
            Err(CliError::InternalError(
                "Unsupported hash type in archive header.".to_string()
            )),

    }
}

/// Generic implementation of the file extraction process.
///
/// This function is parameterized over the hash type `H` and contains the
/// core logic for reading the manifest and chunk index, decompressing chunks,
/// and writing the final files to disk. It handles the creation of the
/// output directory, validates ROM indices against the manifest, and ensures
/// data integrity through hash verification. Each file is first written to
/// a temporary file (`.tmp` extension) and then renamed to its final name
/// upon successful completion to ensure atomicity and prevent corrupted files
/// in case of an error or cancellation.
///
/// # Type Parameters
///
/// * `H`: A type parameter that must implement `Hashable`, `Ord`, `Display`,
///   `Serialize`, `Deserialize<'de>`, `Eq`, and `Hash`. This generic parameter
///   represents the hash type used within the archive (e.g., `u64` or `u128`).
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the source archive file from which
///   data will be extracted.
/// * `out_dir`: A `Path` for the directory where extracted files will be
///   written. If the directory does not exist, it will be created, unless
///   `args.force` is false and the directory does not exist, in which case
///   an `InvalidOutputPath` error is returned.
/// * `rom_indices`: A vector of `u8` indices specifying which files to
///   extract from the archive. These indices are 1-based.
/// * `header`: A reference to the `sprite_shrink::FileHeader` containing
///   metadata about the archive.
/// * `metadata_block`: A slice of `u8` containing the raw metadata block
///   (manifest, dictionary, and chunk index) read from the archive.
/// * `args`: A reference to the main `Args` struct, used to check
///   configuration options like `force`.
/// * `running`: An `Arc<AtomicBool>` used to signal and check for
///   cancellation of the extraction process.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if all requested files are extracted successfully.
/// - `Err(CliError)` if any part of the process fails.
///
/// # Errors
///
/// This function can return an error in several cases, including:
/// - `CliError::from` for errors originating from parsing the file manifest
///   or chunk index.
/// - `CliError::InvalidOutputPath` if `out_dir` does not exist and `args.force`
///   is false.
/// - `CliError::InvalidRomIndex` if a requested ROM index is out of bounds
///   of the file manifest.
/// - `CliError::InternalError` if a chunk hash is found in the manifest but
///   is missing from the chunk index, or if an unsupported hash type is
///   encountered.
/// - `CliError::DataIntegrity` if a decompressed chunk's hash does not match
///   the expected hash from the manifest.
/// - `CliError::Cancelled` if the `running` flag is set to `false` during
///   the extraction process.
/// - Any `std::io::Error` that occurs during file creation, writing, or
///   renaming operations.
fn extract_ssmc_data<H>(
    file_path: &Path,
    out_dir: &Path,
    rom_indices: &Vec<u8>,
    format_data: &SSMCFormatData,
    toc: &[SSMCTocEntry],
    metadata_block: &[u8],
    args: &Args,
    running: Arc<AtomicBool>,
) -> Result<(), CliError>
where
    H: Hashable +
        Display +
        Eq +
        Hash +
        Ord +
        Serialize +
        for<'de> Deserialize<'de> +
        for<'de> Decode<'de>,
{
    /*Stores the length of each piece of the metadata from the archive as
     provided by the header.*/
    let man_length = format_data.enc_manifest.length as usize;
    let dict_length = format_data.data_dictionary.length as usize;

    let manifest_slice = &metadata_block[0..man_length];
    let dict_slice = &metadata_block[man_length..man_length + dict_length];
    let index_slice = &metadata_block[man_length + dict_length..];

    let (manifest_result, index_result) = join(
        || parse_file_metadata::<H>(manifest_slice).map_err(CliError::from),
        || parse_file_chunk_index::<H>(index_slice).map_err(CliError::from),
    );

    let file_manifest = manifest_result?;
    let chunk_index = index_result?;

    if !out_dir.exists() && !args.force{
        return Err(CliError::InvalidOutputPath())
    } else {
        create_dir_all(out_dir)?;
    }

    for rom in rom_indices {
        let fmp = file_manifest
        .get((*rom - 1)  as usize)
        .ok_or_else(|| CliError::InvalidRomIndex(
            format!("ROM index {rom} is out of bounds.")
        ))?;

        let final_output_path = out_dir.join(
            toc[(*rom as usize) - 1].filename.clone());

        let tmp_output_path = final_output_path.with_extension("tmp");

        let _guard = TempFileGuard::new(&tmp_output_path);

        let file = File::create(&tmp_output_path)?;
        let mut writer = BufWriter::new(file);

        fmp.chunk_metadata
            .iter()
            .try_for_each(|scm| -> Result<(), CliError> {
            /*Get the chunk location from the chunk index.*/
            let chunk_location = chunk_index.get(&scm.hash)
                .ok_or_else(|| {
                CliError::InternalError(format!(
                    "Decompression failed: Missing chunk with hash\
                    {}",
                    scm.hash
                ))
            })?;

            /*Store the chunk data offset within the compressed
            store, from the ChunkLocation struct.*/
            let absolute_offset =
                chunk_location.offset +
                format_data.data_offset;

            let comp_chunk_data = read_file_data(
                file_path,
                absolute_offset,
                chunk_location.compressed_length as usize
            )?;

            let decomp_chunk_data = decompress_chunk(
                    &comp_chunk_data,
                    dict_slice
            )?;

            let decomp_chunk_hash = H::from_bytes_with_seed(
                &decomp_chunk_data
            );

            if scm.hash != decomp_chunk_hash {
                return Err(CliError::DataIntegrity(format!(
                    "chunk hash mismatch (expected: {}, calculated: {})",
                    scm.hash, decomp_chunk_hash
                )));
            };

            //Check if the user has cancelled the operation
            if !running.load(Ordering::SeqCst) {
                return Err(CliError::Cancelled);
            }

            writer.write_all(&decomp_chunk_data)?;

            Ok(())
        })?;

        //The file is fully written, rename it to final name.
        rename(&tmp_output_path, final_output_path)?;

        debug!("{} extracted successfully", toc[(*rom as usize) - 1].filename);
    }

    Ok(())
}


fn extract_ssmd_data<H> (
    file_path: &Path,
    header: &FileHeader,
    out_dir: &Path,
    disc_indices: &[u8],
    format_data: &SSMDFormatData,
    toc: &[SSMDTocEntry],
    args: &Args,
    running: Arc<AtomicBool>,
) -> Result<(), CliError>
where
    H: Hashable +
        Display +
        Eq +
        Hash +
        Ord +
        Serialize +
        for<'de> Deserialize<'de> +
        for<'de> Decode<'de>,
{
    const PREFETCH_LOW_THRESHOLD: usize = 50;
    const PREFETCH_HIGH_THRESHOLD: usize = 250;

    let bin_format_data = read_file_data(
        file_path,
        header.format_data_offset as u64,
        SSMDFormatData::SIZE as usize
    )?;

    let format_data = decode_ssmd_format_data(&bin_format_data)?;

    let encoded_metadata = read_file_data(
        file_path,
        format_data.enc_disc_manifest.offset,
        format_data.exception_data.offset as usize
    )?;

    let result = decode_ssmd_meta_data::<H>(&format_data, &encoded_metadata)?;

    let disc_manifests = result.disc_manifests;
    let dictionary = result.data_dictionary;
    let chunk_index = result.chunk_index;
    let audio_block_index = result.audio_block_index.unwrap_or_default();
    let exception_index = result.exception_index.unwrap_or_default();
    let subheader_table = result.subheader_table;

    for disc in disc_indices {
        let dm = disc_manifests
            .get((*disc - 1) as usize)
            .ok_or_else(|| CliError::InvalidRomIndex(
                format!("Disc index {disc} is out of bounds.")
            ))?;

        let final_output_path = out_dir.join(
            toc[(*disc as usize) - 1].filename.clone()
        );

        let tmp_output_path = final_output_path.with_extension("tmp");

        let _guard = TempFileGuard::new(&tmp_output_path);

        let file = File::create(&tmp_output_path)?;
        let mut writer = BufWriter::new(file);

        let chunk_hashes: Vec<H> = dm.data_stream_layout.iter().map(|data| {
            data.hash
        }).collect();

        let audio_hashes: Vec<H> = dm.audio_block_map.iter().map(|block| {
            block.content_hash
        }).collect();

        let expanded_sector_types = expand_sector_map(&dm.rle_sector_map);
        let total_sectors = expanded_sector_types.len();

        let subheader_map = expand_subheader_map(
            &dm.subheader_index,
            total_sectors
        );
        let expand_disc_excep_idx = expand_exception_index(
            &dm.disc_exception_index,
            total_sectors
        );


    }

    Ok(())
}
