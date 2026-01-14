//! Implements the metadata retrieval mode for the command-line application.
//!
//! This module contains the `run_info` function, which reads the header
//! and file manifest from a sprite-shrink multicart archive. It then displays
//! a summary of the archive's contents, such as the names and indices of
//! all contained files, to the console.

use std::{
    path::Path,
};

use crate::archive_parser::{
    get_file_header, get_file_manifest, get_toc
};

use human_bytes::human_bytes;
use serde_json::{self, json, Value, to_string_pretty};

use sprite_shrink::{
    FileHeader, FileManifestParent, TocEntry
};

use crate::error_handling::CliError;

/// Displays metadata about the contents of an archive file.
///
/// This function serves as the entry point for the metadata retrieval
/// mode. It reads the archive's header to locate the file manifest, then
/// parses the manifest to get a list of all files contained within the
/// archive. Finally, it prints a formatted table to the console showing
/// the 1-based index and filename for each file.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the archive file whose
///   metadata is to be displayed.
/// * `list`: A user provided boolean flag that determines the plain text
///   output format. If `true`, the output will be a plain text table.
/// * `metadata`: A user provided boolean flag that determines the json
///   output format. If `true`, the output will be JSON.
///
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the metadata is successfully read and displayed.
/// - `Err(CliError)` if reading the file header or manifest fails.
///
/// # Panics
///
/// This function does not panic but prints directly to standard output.
pub fn run_info(
    file_path: &Path,
    list: bool,
    metadata: bool
) -> Result<(), CliError> {
    /*Get and store the parsed header from the target archive file.*/
    let header = get_file_header(file_path)?;

    let toc_offset = header.toc_offset;

    /*Stores the length of the table of contents from the header.*/
    let toc_length = header.toc_length as usize;

    let toc = get_toc(file_path, toc_offset, toc_length)?;

    dispatch_print_info(toc, metadata)?;

    Ok(())
}

/// Dispatches the print operation to the appropriate output format function.
///
/// This function acts as a router, taking the parsed file manifest and a
/// format specifier. Based on the `json` flag, it delegates the final
/// output to either a human-readable table format or a machine-readable
/// JSON format. This approach allows for easy extension with additional
/// output formats in the future.
///
/// # Arguments
///
/// * `file_manifest`: A vector of `FileManifestParent` structs, where each
///   struct contains the metadata for a single file in the archive.
/// * `metadata`: A user provided boolean flag that determines the json
///   output format. If `true`, the output will be JSON.
fn dispatch_print_info(
    toc: Vec<TocEntry>,
    metadata: bool
) -> Result<(), CliError>{
    if metadata {
        print_info_json(toc)?;
    } else {
        print_info_table(toc);
    }

    Ok(())
}

/// Prints a formatted table of the file manifest's contents.
///
/// # Arguments
///
/// * `file_manifest`: A vector of `FileManifestParent` structs to display.
///
/// # Examples
///
/// An example of the output format printed to the console:
///
/// ```text
/// Index   | Filesize      | Filename
/// ----------------------------------
/// 1       | 4.00 MiB      | file_a.rom
/// 2       | 8.00 MiB      | file_b.rom
/// 3       | 8.00 MiB      | file_c.rom
/// ```
fn print_info_table(
    file_manifest: Vec<TocEntry>
){
    println!("Index\t| Filesize\t| Filename");
    println!("----------------------------------");

    /*For each FileManifestParent in the file manifest print the index
    + 1 to be easily understood by the user and the filename of that index.*/
    file_manifest.iter().enumerate().for_each(|(index, toc_entry)|{
        let file_size = toc_entry.uncompressed_size;
        println!("{}\t| {}\t| {}",
            index + 1,
            human_bytes(file_size as f64),
            toc_entry.title
        );
    });

}

/// Prints a json formatted table of the file manifest's contents.
///
/// # Arguments
///
/// * `file_manifest`: A vector of `FileManifestParent` structs to display.
fn print_info_json(
    file_manifest: Vec<TocEntry>
) -> Result<(), CliError>{
    let files_json: Vec<Value> = file_manifest.iter()
        .enumerate()
        .map(|(index, toc_entry)| {
            let file_size = toc_entry.uncompressed_size;

            json!({
                "index": index + 1,
                "filename": &toc_entry.title,
                "size(bytes)": file_size
            })
        })
        .collect();


    let output = json!(files_json);

    let pretty_json = to_string_pretty(&output)
        .map_err(
            |e| CliError::InternalError(e.to_string())
        )?;

    println!("{}", pretty_json);

    Ok(())
}
