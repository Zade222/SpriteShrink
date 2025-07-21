//! Provides all file system input/output operations for the application.
//!
//! This module abstracts the complexities of interacting with the file
//! system. It includes functions for reading from and writing to files,
//! checking for file and directory existence, organizing input paths,
//! and collecting all files from a set of directories. All functions are
//! designed to be robust and provide clear error handling.

use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};

use lib_sprite_shrink::{FileData};

use crate::error_handling::CliError;

/// Checks if a given path points to a regular file.
///
/// This function uses `fs::symlink_metadata` to get information about
/// the path without following symbolic links. This is useful for ensuring
/// that a path is a file and not a link pointing to one, which is
/// important when recursively scanning directories.
///
/// # Arguments
///
/// * `path`: A reference to a `Path` object to be checked.
///
/// # Returns
///
/// Returns `true` if the path exists and is a regular file. Returns
/// `false` otherwise, or if an error occurs.
fn is_regular_file(path: &Path) -> bool {
    // Get file metadata without following symlinks from system storage.
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_file()) 
        .unwrap_or(false) 
}

/// Sorts input paths into separate vectors of files and directories.
///
/// This function iterates over a slice of `PathBuf`s and categorizes
/// each path based on whether it points to a file or a directory. Paths
/// that do not exist or are not files or directories are ignored.
///
/// # Arguments
///
/// * `input_paths`: A slice of `PathBuf` objects to be organized.
///
/// # Returns
///
/// A `Result` containing a tuple with two `Vec<PathBuf>`:
/// - The first vector contains all valid paths to files.
/// - The second vector contains all valid paths to directories.
pub fn organize_paths(
    input_paths: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), CliError> {
    //Create vector to store file paths.
    let mut file_paths = Vec::new();

    //Create vector to store directory paths.
    let mut dir_paths = Vec::new();

    /*For each path in the provided input_paths variable:
    - If path is a directory, add that path to the directory paths vector.
    - If path is a file, add that path to the file paths vector.*/
    for path in input_paths {
        if path.is_dir() {
            dir_paths.push(path.clone());
        } else if path.is_file() {
            file_paths.push(path.clone());
        } else {
            // println!("WARN: Ignoring unsupported path type: {}", path.display());
        }
    }

    /*Return tuple containing the file and directory path vectors. */
    Ok((file_paths, dir_paths))
}

/// Collects all regular file paths from a slice of directories.
///
/// This function iterates over a list of directory paths and gathers
/// all entries that are regular files. It performs a shallow search,
/// meaning it does not recurse into subdirectories. Symbolic links are
/// ignored to prevent duplicate processing.
///
/// # Arguments
///
/// * `dir_paths`: A slice of `PathBuf`s for directories to scan.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<PathBuf>)` containing a list of all regular files found.
/// - `Err(CliError)` if reading any of the directories fails.
pub fn files_from_dirs(dir_paths: &[PathBuf]) -> Result<Vec<PathBuf>, CliError> {
    /*Create mutable vector file_paths and then for each path in dir_paths:
    - Read the directory for entries. or each entry found:
        - Check if each entry is a file.
        - If yes add that path to the file_path vector.*/
    dir_paths.iter().try_fold(Vec::new(), |mut file_paths, dir_path| {
        for entry_result in fs::read_dir(dir_path)? {
            let entry = entry_result?;
            let path = entry.path();
            if is_regular_file(&path) {
                file_paths.push(path);
            }
        }
        //Return vector of file paths.
        Ok(file_paths)
    })
}

/// Reads an entire file into memory and packages it.
///
/// This function takes a file path, reads its full content into a byte
/// vector, and extracts the file name. It then returns this data
/// encapsulated within a `FileData` struct, ready for processing.
///
/// # Arguments
///
/// * `filepath`: A `PathBuf` pointing to the file to be loaded.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(FileData)` on success, containing the file's name and content.
/// - `Err(CliError::Io)` if the file cannot be read.
pub fn load_file(
    filepath: &PathBuf
) -> Result<FileData, CliError> {
    //Read all file data of the filepath target.
    let file_contents = fs::read(filepath).map_err(CliError::Io)?;
    
    //Store the filename
    let file_name = filepath
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    
    //Create FileData struct containing the name and file data.
    let file_data = FileData {
        file_name,
        file_data: file_contents,
    };

    //Return file data.
    Ok(file_data)
}

/// Reads a specific segment of a file into a byte vector.
///
/// This function opens a file, seeks to a specified starting offset,
/// and reads a given number of bytes into a buffer. It is useful for
/// accessing parts of a large file without loading the entire content
/// into memory, such as reading a data chunk from an archive.
///
/// # Arguments
///
/// * `filepath`: A `PathBuf` pointing to the file to be read.
/// * `data_index`: A `u64` reference for the starting offset.
/// * `data_length`: A `usize` reference for the number of bytes to read.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the bytes read from the file segment.
/// - `Err(CliError::Io)` if the file cannot be opened or read.
pub fn read_file_data(
    filepath: &PathBuf, 
    data_index: &u64, 
    data_length: &usize
) -> Result<Vec<u8>, CliError> {
    //Open file, but don't read whole file data into memory.
    let mut file = File::open(filepath)?;

    //Create mutable vector the size of the data length to be read.
    let mut file_buffer: Vec<u8> =  vec![0; *data_length];

    //Offset the start of where the read will begin.
    file.seek(SeekFrom::Start(*data_index))?;

    //Read the data data from the file.
    file.read_exact(&mut file_buffer)?;
    
    //Return read data from file.
    Ok(file_buffer)
}

/// Writes a collection of byte slices to a single file.
///
/// This function ensures the destination directory exists, creating it
/// recursively if needed. It then creates a new file at the specified
/// path, overwriting any existing file. The provided data slices are
/// written sequentially using a `BufWriter` for efficiency.
///
/// # Arguments
///
/// * `final_output_path`: A `Path` reference to the target file.
/// * `file_data`: A vector of byte slices (`&[u8]`) to be written.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the file is written successfully.
/// - `Err(CliError::Io)` if creating the directory or file fails.
pub fn write_file(
    final_output_path: &Path,
    file_data: Vec<&[u8]>
) -> Result<(), CliError> {
    /*Check if the parent directory of the target output exists, 
    if not create it.*/
    if let Some(dir) = final_output_path.parent() {
        fs::create_dir_all(dir)?;
    }

    /*Create file as specified by the final output path. */
    let file = File::create(final_output_path)?;

    //Initiate the file writer that will write the data to disk.
    let mut writer = BufWriter::new(file);

    //Write data to disk.
    for data in file_data{
        writer.write_all(data)?;
    }

    Ok(())
}

/// Writes a complete byte slice to a specified file path.
///
/// This function handles writing a generated archive to disk. It ensures
/// the parent directory of the output path exists, creating it if
/// necessary. It then performs a single write operation to save the
/// entire data slice to the destination file.
///
/// # Arguments
///
/// * `output_path`: A `Path` reference to the destination file.
/// * `data`: A byte slice (`&[u8]`) containing the full content.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` on a successful write operation.
/// - `Err(CliError::Io)` if creating the directory or writing fails.
pub fn write_final_archive(output_path: &Path, data: &[u8]) -> Result<(), CliError> {
    /*Check if the parent directory of the target output exists, 
    if not create it.*/
    if let Some(dir) = output_path.parent() {
        fs::create_dir_all(dir)?;
    }
    //Write data to disk.
    fs::write(output_path, data).map_err(CliError::Io)
}