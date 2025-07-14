//! Implements the metadata retrieval mode for the command-line application.
//!
//! This module contains the `run_info` function, which reads the header
//! and file manifest from a sprite-shrink multicart archive. It then displays 
//! a summary of the archive's contents, such as the names and indices of
//! all contained files, to the console.

use std::path::PathBuf;

use crate::archive_parser::{
    get_file_header, get_file_manifest
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
/// * `file_path`: A `PathBuf` pointing to the archive file whose
///   metadata is to be displayed.
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
///
/// # Examples
///
/// An example of the output format printed to the console:
///
/// ```text
/// Index   | Filename
/// ------------------
/// 1       | file_a.rom
/// 2       | file_b.rom
/// 3       | file_c.rom
/// ```
pub fn run_info(
    file_path: &PathBuf
) -> Result<(), CliError> {
    /*Get and store the parsed header from the target archive file.*/
    let header = get_file_header(file_path)?;

    /*Stores the length of the file_manifest from the header.*/
    let man_length = header.man_length as usize;

    /*Read and store the file manifest from the target file in memory in 
    the file_manifest variable*/
    let file_manifest = get_file_manifest(
        file_path, 
        &header.man_offset, 
        &man_length
    )?;


    println!("Index\t| Filename");
    println!("------------------");

    /*For each FileManifestParent in the file manifest print the index
    + 1 to be easily understood by the user and the filename of that index.*/
    file_manifest.iter().enumerate().for_each(|(index, fmp)|{
        println!("{}\t| {}", index + 1, fmp.filename);
    });
    
    Ok(())
}