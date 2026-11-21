//! Implements the extraction mode for the command-line application.
//!
//! This module contains the `run_extraction` function, which is the main
//! entry point for the file extraction process. It handles reading an
//! archive's metadata, including the header, manifest, and chunk index,
//! then proceeds to decompress and write the files requested by the user
//! to the designated output directory.

use std::{
    cell::Cell,
    collections::HashMap,
    fmt::Display,
    fs::{create_dir_all, symlink_metadata},
    hash::Hash,
    io::Write,
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
    ChunkLocation, FileHeader, FileManifestParent, Hashable, ParsedSSMC,
    IsCancelled, SSAChunkMeta, SSMCTocEntry, SpriteShrinkError,
    check_entry_name, decompress_chunks_with
};

use sprite_shrink_cd::{
    SSMD_UID,
    ParsedSSMD,
    generate_cue_string, write_disc
};

use crate::{
    archive_parser::{get_file_header, get_ssmc_prefix, get_ssmd_prefix},
    arg_handling::Args,
    error_handling::CliError,
    storage_io::{
        ArchiveFile, ExistingFile,
        has_extension, write_via_temp
    },
};

/// Write buffer for a file from an SSMC archive: 8 KiB, the size
/// `BufWriter::new` uses.
const FILE_WRITE_BUFFER_SIZE: usize = 8 * 1024;

/// Write buffer for a disc image: 2048 raw sectors of 2352 bytes (about
/// 4.6 MiB), so a disc is written in a few large writes.
const DISC_WRITE_BUFFER_SIZE: usize = 2048 * 2352;

/// What extracting one archive entry writes, checked by [`check_plan`]
/// before anything is written.
struct EntryPlan {
    /// The entry's number, from 1, as `-l` and `-e` show it.
    number: usize,
    /// The names of the files it writes in the output folder.
    names: Vec<String>,
    /// The bytes it writes, from its manifest.
    size: u64,
}

impl EntryPlan {
    /// Plans entry `index` (from 0), named `name` in the table of
    /// contents, which lists it as `listed` bytes. `size` is the size its
    /// manifest gives.
    ///
    /// # Errors
    ///
    /// Returns `CliError::DataIntegrity` if `size` isn't `listed`: the
    /// archive contradicts itself, so it's damaged.
    fn new(
        index: usize,
        names: Vec<String>,
        size: u64,
        listed: u64,
    ) -> Result<Self, CliError> {
        let number = index + 1;
        if size != listed {
            return Err(CliError::DataIntegrity(format!(
                "entry {number} ({}) is listed as {listed} bytes, but its \
                manifest gives {size}",
                names.join(", ")
            )));
        }
        Ok(Self { number, names, size })
    }
}

/// The name of the CUE sheet written beside a disc's BIN: the BIN's name
/// with a `.bin` extension, in any case, replaced by `.cue`, or with
/// `.cue` appended to any other name. The two never share a name.
fn cue_name(bin_name: &str) -> String {
    if has_extension(Path::new(bin_name), "bin") {
        Path::new(bin_name)
            .with_extension("cue")
            .to_string_lossy()
            .into_owned()
    } else {
        format!("{bin_name}.cue")
    }
}

/// Checks everything extraction will write before it writes anything, and
/// prints the total size.
///
/// Every name must be usable as a file name, and no two may be the same,
/// even ignoring case, or one file would replace another on Windows and
/// macOS. That holds even with `--force`, which permits replacing the
/// user's files, not the archive's own. With [`ExistingFile::Refuse`], no
/// name may already be in `out_dir`.
///
/// # Errors
///
/// Returns `ParsingError::InvalidEntryName` (inside
/// `CliError::SpriteShrinkError`) for an unusable name,
/// `CliError::ClashingNames` for two entries writing one name, or
/// `CliError::FilesExist` listing every name already in `out_dir`.
fn check_plan(
    out_dir: &Path,
    plans: &[EntryPlan],
    existing: ExistingFile,
) -> Result<(), CliError> {
    let mut seen: HashMap<String, (usize, &str)> = HashMap::new();
    for plan in plans {
        for name in &plan.names {
            check_entry_name(name).map_err(SpriteShrinkError::from)?;
            if let Some((other, other_name)) =
                seen.insert(name.to_lowercase(), (plan.number, name))
            {
                return Err(CliError::ClashingNames(format!(
                    "entry {other} writes {other_name:?} and entry {} \
                    writes {name:?}",
                    plan.number
                )));
            }
        }
    }

    if existing == ExistingFile::Refuse {
        let present: Vec<String> = plans
            .iter()
            .flat_map(|plan| &plan.names)
            .map(|name| out_dir.join(name))
            .filter(|path| symlink_metadata(path).is_ok())
            .map(|path| format!("  {}", path.display()))
            .collect();
        if !present.is_empty() {
            return Err(CliError::FilesExist(present.join("\n")));
        }
    }

    let total = plans
        .iter()
        .fold(0_u64, |total, plan| total.saturating_add(plan.size));
    println!("Extracting {} entries, {total} bytes.", plans.len());
    Ok(())
}

/// Executes the file extraction process from an archive.
///
/// This is the main entry point for the extraction operation. It reads an
/// archive's metadata, including the header, manifest, and chunk index.
/// It then extracts the specific files requested by the user, identified
/// by their ROM indices. Files are extracted one after another. Within an
/// SSMC file, chunks are decoded on several threads and written in order.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the source archive file.
/// * `out_dir`: A `Path` for the directory where extracted files will
///   be written.
/// * `rom_indices`: Zero-based positions in the archive's table of contents
///   of the files to extract, as `parse_rom_indices` returns them.
/// * `args`: A reference to the main `Args` struct: `force`, and `threads`
///   for the number of threads decoding SSMC chunks (0 or unset uses every
///   core).
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

/// Entry `index` (from 0) of an SSMC archive: its manifest and its table
/// of contents entry.
///
/// # Errors
///
/// Returns `CliError::InvalidRomIndex` if the archive has no entry
/// `index`.
fn ssmc_entry<'a, H>(
    manifests: &'a [FileManifestParent<H>],
    toc: &'a [SSMCTocEntry],
    index: usize,
) -> Result<(&'a FileManifestParent<H>, &'a SSMCTocEntry), CliError> {
    manifests.get(index).zip(toc.get(index)).ok_or_else(|| {
        CliError::InvalidRomIndex(format!(
            "ROM index {} is out of bounds.",
            index + 1
        ))
    })
}

/// Generic implementation of the file extraction process.
///
/// This function is parameterized over the hash type `H` and contains the
/// core logic for reading the manifest and chunk index, decompressing chunks,
/// and writing the final files to disk. It handles the creation of the
/// output directory, validates ROM indices against the manifest, and ensures
/// data integrity through hash verification. Each file is first written to
/// a temporary file in `out_dir` and renamed to its final name once
/// complete and flushed, so an error or cancellation leaves no partial file
/// (see [`write_via_temp`]).
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
/// * `args`: A reference to the main `Args` struct: `force`, and `threads`
///   for the number of decoding threads (0 or unset uses every core).
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
/// - `CliError::DataIntegrity`, `CliError::ClashingNames` or
///   `CliError::FilesExist` from the checks [`check_plan`] and
///   [`EntryPlan::new`] make before anything is written.
/// - `CliError::InvalidOutputPath` if `out_dir` does not exist and `args.force`
///   is false.
/// - `CliError::InvalidRomIndex` if a requested ROM index is out of bounds
///   of the file manifest.
/// - `CliError::InternalError` if a chunk hash is found in the manifest but
///   is missing from the chunk index, or if an unsupported hash type is
///   encountered.
/// - `CliError::SpriteShrinkError` if a chunk doesn't decompress, or doesn't
///   decompress to the bytes its hash says, or if a decoding thread can't be
///   started or panics.
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

    let existing = ExistingFile::from_force(args.force);
    let plans = rom_indices
        .iter()
        .map(|&index| {
            let (fmp, entry) = ssmc_entry(file_manifest, toc, index)?;
            let size = fmp.chunk_metadata.iter().fold(0_u64, |total, chunk| {
                total.saturating_add(chunk.length.into())
            });
            EntryPlan::new(
                index,
                vec![entry.filename.clone()],
                size,
                entry.uncompressed_size,
            )
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    check_plan(out_dir, &plans, existing)?;

    if !out_dir.exists() && !args.force{
        return Err(CliError::InvalidOutputPath())
    } else {
        create_dir_all(out_dir)?;
    }

    /*One handle serves every read. Reads give their own offsets, so the
    file has no shared position to keep in step.*/
    let archive = ArchiveFile::open(file_path)?;
    let worker_count = args.threads.unwrap_or(0);

    for &index in rom_indices {
        let (fmp, entry) = ssmc_entry(file_manifest, toc, index)?;

        /*The prefix parser has checked every name already. Check again
        here, where a bad name would write outside out_dir.*/
        check_entry_name(&entry.filename).map_err(SpriteShrinkError::from)?;
        let final_output_path = out_dir.join(&entry.filename);

        /*Each file is written to a temp file and renamed once every chunk
        has passed its hash check, so a failed or cancelled run leaves no
        partial file.*/
        let buffer_size = FILE_WRITE_BUFFER_SIZE;
        write_via_temp(&final_output_path, existing, buffer_size, |writer| {
            /*The library passes on a callback's error as its message only.
            Keep the error itself, so the user sees it as it was raised (an
            I/O error stays an I/O error).*/
            let callback_error = Cell::new(None);
            let keep = |e: CliError| {
                let stand_in = if e.is_cancelled() {
                    CliError::Cancelled
                } else {
                    CliError::InternalError(e.to_string())
                };
                callback_error.set(Some(e));
                stand_in
            };

            let read_chunks = |chunks: &[SSAChunkMeta<H>]| {
                if !running.load(Ordering::SeqCst) {
                    return Err(keep(CliError::Cancelled));
                }

                chunks.iter().map(|scm| {
                    let location = chunk_index.get(&scm.hash)
                        .ok_or_else(|| CliError::InternalError(format!(
                            "Decompression failed: Missing chunk with hash \
                            {}",
                            scm.hash
                        )))?;

                    //The chunk index already holds absolute file offsets.
                    archive.read(
                        location.offset,
                        location.compressed_length as usize
                    )
                }).collect::<Result<_, _>>().map_err(keep)
            };

            let write_chunk = |data: &[u8]| {
                if !running.load(Ordering::SeqCst) {
                    return Err(keep(CliError::Cancelled));
                }
                writer.write_all(data).map_err(|e| keep(e.into()))
            };

            decompress_chunks_with(
                worker_count,
                dictionary,
                &fmp.chunk_metadata,
                read_chunks,
                write_chunk,
            ).map_err(|e| callback_error.take()
                .unwrap_or_else(|| CliError::from(e)))
        })?;

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

    let disc_entry = |index: usize| {
        disc_manifests.get(index).zip(toc.get(index)).ok_or_else(|| {
            CliError::InvalidRomIndex(format!(
                "Disc index {} is out of bounds.",
                index + 1
            ))
        })
    };
    let existing = ExistingFile::from_force(args.force);
    let plans = disc_indices
        .iter()
        .map(|&index| {
            let (dm, entry) = disc_entry(index)?;
            let sectors = dm.rle_sector_map.runs.iter().fold(0_u64, |total, run| {
                total.saturating_add(run.0.into())
            });
            EntryPlan::new(
                index,
                vec![entry.filename.clone(), cue_name(&entry.filename)],
                sectors.saturating_mul(2352),
                entry.uncompressed_size,
            )
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    check_plan(out_dir, &plans, existing)?;

    if !out_dir.exists() && !args.force{
        return Err(CliError::InvalidOutputPath())
    } else {
        create_dir_all(out_dir)?;
    }

    println!("Opening archive...");
    let archive_file = ArchiveFile::open(file_path)?;
    let read_stored = |location: ChunkLocation| {
        archive_file.read(
            location.offset,
            location.compressed_length as usize
        )
    };

    println!("Beginning extraction loop...");
    for &index in disc_indices {
        let (dm, entry) = disc_entry(index)?;

        /*The prefix parser has checked every name already. Check again
        here, where a bad name would write outside out_dir.*/
        check_entry_name(&entry.filename).map_err(SpriteShrinkError::from)?;
        let final_output_path = out_dir.join(&entry.filename);

        let cue_content = generate_cue_string(dm, &entry.filename)
            .map_err(|e| CliError::InternalError(
                format!("Failed to generate CUE sheet: {}", e)
            ))?;

        /*The BIN and then the CUE sheet are each written to a temp file
        and renamed when complete, so a failed or cancelled run leaves no
        partial file.*/
        let buffer_size = DISC_WRITE_BUFFER_SIZE;
        write_via_temp(&final_output_path, existing, buffer_size, |writer| {
            write_disc(
                dm,
                &indices,
                &dictionary,
                read_stored,
                &exception_blob,
                &subheader_table,
                writer,
                || !running.load(Ordering::SeqCst)
            )
            .map_err(CliError::from)
        })?;

        let cue_path = out_dir.join(cue_name(&entry.filename));
        write_via_temp(&cue_path, existing, cue_content.len(), |writer| {
            writer.write_all(cue_content.as_bytes()).map_err(CliError::from)
        })?;

        debug!("{} extracted successfully", entry.filename);
    }

    Ok(())
}


#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn plan(number: usize, names: &[&str]) -> EntryPlan {
        EntryPlan {
            number,
            names: names.iter().map(ToString::to_string).collect(),
            size: 0,
        }
    }

    #[test]
    fn a_cue_sheet_is_named_after_its_bin() {
        for (bin, cue) in [
            ("disc.bin", "disc.cue"),
            ("DISC.BIN", "DISC.cue"),
            ("x.cue", "x.cue.cue"),
            ("x.iso", "x.iso.cue"),
            ("disc", "disc.cue"),
        ] {
            assert_eq!(cue_name(bin), cue, "{bin}");
        }
    }

    #[test]
    fn an_entry_whose_manifest_and_toc_disagree_is_refused() {
        EntryPlan::new(0, vec!["a.bin".to_string()], 10, 10).unwrap();

        let result = EntryPlan::new(1, vec!["b.bin".to_string()], 10, 11);
        assert!(
            matches!(&result, Err(CliError::DataIntegrity(m)) if m.contains("entry 2 (b.bin)")),
            "{:?}",
            result.err()
        );
    }

    /*Names that one folder can't hold apart are refused, with --force too:
    the same name, the same ignoring case, and one disc's BIN on another's
    CUE.*/
    #[test]
    fn entries_writing_one_name_are_refused_even_with_force() {
        let tmp = TempDir::new().unwrap();
        for plans in [
            [plan(1, &["a.bin"]), plan(2, &["a.bin"])],
            [plan(1, &["a.bin"]), plan(2, &["A.BIN"])],
            [plan(1, &["x.bin", "x.cue"]), plan(2, &["x.cue", "x.cue.cue"])],
        ] {
            let result = check_plan(tmp.path(), &plans, ExistingFile::Replace);
            assert!(
                matches!(&result, Err(CliError::ClashingNames(m)) if m.contains("entry 1")),
                "{:?}: {result:?}",
                plans.map(|p| p.names)
            );
        }

        let distinct = [plan(1, &["a.bin"]), plan(2, &["b.bin", "b.cue"])];
        check_plan(tmp.path(), &distinct, ExistingFile::Refuse).unwrap();
    }

    #[test]
    fn every_name_already_in_the_folder_is_listed() {
        let tmp = TempDir::new().unwrap();
        fs_write(tmp.path(), "a.bin");
        fs_write(tmp.path(), "b.cue");
        let plans = [plan(1, &["a.bin"]), plan(2, &["b.bin", "b.cue"])];

        let result = check_plan(tmp.path(), &plans, ExistingFile::Refuse);
        let Err(CliError::FilesExist(listed)) = &result else {
            panic!("{result:?}");
        };
        assert!(listed.contains("a.bin") && listed.contains("b.cue"), "{listed}");
        assert!(!listed.contains("b.bin"), "{listed}");

        check_plan(tmp.path(), &plans, ExistingFile::Replace).unwrap();
    }

    fn fs_write(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
}
