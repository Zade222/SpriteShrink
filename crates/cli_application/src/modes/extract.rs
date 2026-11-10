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
use serde::{Deserialize, Serialize};
use tracing::{
    debug
};

use sprite_shrink::{
    SSMC_UID,
    HashType,
    Hashable, FileHeader, ParsedSSMC,
    decompress_chunk
};

use sprite_shrink_cd::{
    SSMD_UID,
    ParsedSSMD,
    generate_cue_string, write_disc
};

use crate::{
    archive_parser::{get_file_header, get_ssmc_prefix, get_ssmd_prefix},
    arg_handling::Args,
    cli_types::TempFileGuard,
    error_handling::CliError,
    storage_io::read_file_data,
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
/// * `rom_indices`: Zero-based positions in the archive's table of contents
///   of the files to extract, as `parse_rom_indices` returns them.
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
    rom_indices: &[usize],
    args: &Args,
    running: Arc<AtomicBool>,
) -> Result<(), CliError> {
    /*Get and store the parsed header from the target archive file.*/
    let header = get_file_header(file_path)?;

    /*The header was already checked when it was parsed, so this only turns
    the byte into a `HashType`. The match has no catch-all arm, so a new
    hash type doesn't compile until it's handled here.*/
    match HashType::try_from(header.hash_type)? {
        HashType::Xxh3_64 => dispatch_extraction::<u64>(
            file_path,
            out_dir,
            &header,
            rom_indices,
            args,
            running
        ),
        HashType::Xxh3_128 => dispatch_extraction::<u128>(
            file_path,
            out_dir,
            &header,
            rom_indices,
            args,
            running
        ),
    }
}

fn dispatch_extraction<H>(
    file_path: &Path,
    out_dir: &Path,
    header: &FileHeader,
    rom_indices: &[usize],
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
    match header.format_id {
        SSMC_UID => {
            //Read and parse all metadata before the compressed data.
            let prefix = get_ssmc_prefix::<H>(file_path, header)?;

            extract_ssmc_data::<H>(
                file_path,
                out_dir,
                rom_indices,
                &prefix,
                args,
                running
            )?
        },
        SSMD_UID => {
            //Read and parse all metadata before the audio and data blobs.
            let prefix = get_ssmd_prefix::<H>(file_path, header)?;

            extract_ssmd_data::<H>(
                file_path,
                out_dir,
                rom_indices,
                prefix,
                args,
                running
            )?;

        }
        _ => {
            return Err(CliError::InvalidFormatID(header.format_id))
        }
    }

    Ok(())
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
/// * `rom_indices`: Zero-based positions in the table of contents of the
///   files to extract.
/// * `prefix`: The archive's parsed metadata: TOC, manifest, dictionary,
///   and a chunk index holding absolute file offsets.
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
    rom_indices: &[usize],
    prefix: &ParsedSSMC<H>,
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
    let ParsedSSMC {
        toc,
        manifests: file_manifest,
        dictionary,
        chunk_index,
        ..
    } = prefix;

    if !out_dir.exists() && !args.force{
        return Err(CliError::InvalidOutputPath())
    } else {
        create_dir_all(out_dir)?;
    }

    for &index in rom_indices {
        let (fmp, entry) = file_manifest
        .get(index)
        .zip(toc.get(index))
        .ok_or_else(|| CliError::InvalidRomIndex(
            format!("ROM index {} is out of bounds.", index + 1)
        ))?;

        let final_output_path = out_dir.join(&entry.filename);

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

            //The chunk index already holds absolute file offsets.
            let comp_chunk_data = read_file_data(
                file_path,
                chunk_location.offset,
                chunk_location.compressed_length as usize
            )?;

            let decomp_chunk_data = decompress_chunk(
                    &comp_chunk_data,
                    dictionary
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

        debug!("{} extracted successfully", entry.filename);
    }

    Ok(())
}


fn extract_ssmd_data<H> (
    file_path: &Path,
    out_dir: &Path,
    disc_indices: &[usize],
    prefix: ParsedSSMD<H>,
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
    let ParsedSSMD {
        toc,
        disc_manifests,
        data_dictionary: dictionary,
        indices,
        subheader_table,
        exception_blob,
        ..
    } = prefix;

    if !out_dir.exists() && !args.force{
        return Err(CliError::InvalidOutputPath())
    } else {
        create_dir_all(out_dir)?;
    }

    println!("Opening archive...");
    let archive_file = File::open(file_path)?;

    println!("Beginning extraction loop...");
    for &index in disc_indices {
        let (dm, entry) = disc_manifests
            .get(index)
            .zip(toc.get(index))
            .ok_or_else(|| CliError::InvalidRomIndex(
                format!("Disc index {} is out of bounds.", index + 1)
            ))?;

        let final_output_path = out_dir.join(&entry.filename);

        let tmp_output_path = final_output_path.with_extension("tmp");

        let _guard = TempFileGuard::new(&tmp_output_path);

        let file = File::create(&tmp_output_path)?;

        const WRITE_BUFFER_SIZE: usize = 2048 * 2352;
        let mut writer = BufWriter::with_capacity(WRITE_BUFFER_SIZE, file);

        write_disc(
            dm,
            &indices,
            &dictionary,
            &archive_file,
            &exception_blob,
            &subheader_table,
            &mut writer
        )?;

        let cue_content = generate_cue_string(dm, &entry.filename)
            .map_err(|e| CliError::InternalError(
                format!("Failed to generate CUE sheet: {}", e)
            ))?;

        let cue_path = final_output_path.with_extension("cue");

        rename(&tmp_output_path, final_output_path)?;

        let mut cue_file = File::create(&cue_path)?;
        cue_file.write_all(cue_content.as_bytes())?;

        debug!("{} extracted successfully", entry.filename);
    }

    Ok(())
}
