//! Provides all file system input/output operations for the application.
//!
//! This module abstracts the complexities of interacting with the file
//! system. It includes functions for reading from and writing to files,
//! checking for file and directory existence, organizing input paths,
//! and collecting all files from a set of directories. All functions are
//! designed to be robust and provide clear error handling.

use std::{
    fs::{self, File, OpenOptions, metadata, read_dir, remove_file},
    io::{BufWriter, copy, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::SystemTime
};

use directories::ProjectDirs;
use tracing::warn;

use crate::{
    cli_types::{APPIDENTIFIER, SpriteShrinkConfig},
    error_handling::CliError
};

/// A string literal representing the default, commented configuration file.
///
/// This constant holds the content that is written to `spriteshrink.toml` 
/// when the application is run for the first time and no existing 
/// configuration is found.
///
/// Its primary purpose is to provide a user-friendly template, with each 
/// option explained, so that users can easily understand and customize their
/// settings.
const DEFAULT_CONFIG_WITH_COMMENTS: &str = r#"
# Sets the numerical compression level.
# Default = 19
compression_level = 19

# Sets the hashing algorithm window size (e.g., "2KB", "4KB").
# Default = "2kb"
window_size = "2kb"

# Sets the zstd compression algorithm dictionary size.
# Default = "16kb"
dictionary_size = "16kb"

# Sets the hashing algorithm bit length.
# Default = 64
hash_bit_length = 64

# Enable (true) or disable (false) whether to auto tune the window and/or
# dictionary size.
# Default = false
auto_tune = false

# Sets the maximum time for each auto tune iteration to take. If exceeded take
# the latest result.
# Default = 15 seconds
autotune_timeout = 15

# Enable (true) or disable (false) whether to optimze the ztd compression 
# dictionary when it is generated. Be sure to disable when processing large 
# amounts of data (e.g. If a single ROM exceeds approximately 64 megabytes, 
# set to false) as this step can take a significant amount of time for neglible
# gain for large amounts of data.
# Default = false
optimize_dictionary = false

# Sets the maximum number of worker threads to use.
# 0 means use all available cores.
threads = 0

# Enable (true) or disable (false) low memory mode. Forces the use of a
# temporary cache instead of processing all files in memory even if the
# application determines there is enough memory to do so.
# Default = false
low_memory = false

# Enable (true) or disable (false) whether to print metadata info in
# json format.
# Default = false
json_output = false

# Activates verbose output for detailed diagnostic information.
# Default = false
verbose = false

# Whether to print anything to console. True will disable printing to console
# and false will enable.
# Default = false
quiet_output = false

# Specifies the amount of days the application will retain log files. Any log
# file found to be older than the retention period, upon the application being
# run, will be removed. A value of 0 will keep logs forever.
# Default = 7
log_retention_days = 7

# Sets the log file verbosity level when enabled.
# - error will only log errors.
# - warning includes warning messages and the above messages (errors)
# - info includes info messages and the above messages (errrors and warnings)
# - debug includes debug messages and the above messages 
#   (errrors, warnings and info)
# - off disables the creation of a log file and won't write any messages to one
#   if one exists
# Default = "error"
log_level = "error"
"#;

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
        .map(|m| {
            m.file_type().is_file()
        }) 
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
            } else {
                warn!("Ignoring symbolic link: {}", path.display());
            }
        }
        //Return vector of file paths.
        Ok(file_paths)
    })
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
/// * `filepath`: A `Path` pointing to the file to be read.
/// * `data_index`: A `u64` reference for the starting offset.
/// * `data_length`: A `usize` reference for the number of bytes to read.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the bytes read from the file segment.
/// - `Err(CliError::Io)` if the file cannot be opened or read.
pub fn read_file_data(
    filepath: &Path, 
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

/// Writes the final archive file by combining the header and compressed data.
///
/// This function orchestrates the final step of the archive creation process.
/// It performs a two-stage write to ensure atomicity and handle large data
/// sets efficiently. First, it writes the metadata portion (header, manifest,
/// etc.) to the final destination file. Second, it appends the compressed
/// chunk data, which is read from a temporary file (`.tmp`) created during
/// the compression stage.
///
/// After successfully appending the data, the temporary file is deleted, 
/// leaving a complete and valid `.ssmc` archive.
///
/// # Arguments
///
/// * `output_path`: A `Path` reference to the destination file.
/// * `data`: A byte slice (`&[u8]`) containing the archive's header and
///   metadata.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` on a successful write operation.
/// - `Err(CliError::Io)` if creating directories, writing the header,
///   reading the temporary file, or cleaning up fails.
pub fn write_final_archive(
    output_path: &Path, 
    data: &[u8]
) -> Result<(), CliError> {
    /*Check if the parent directory of the target output exists, 
    if not create it.*/
    if let Some(dir) = output_path.parent() {
        fs::create_dir_all(dir)?;
    }
    //Write header data to disk
    fs::write(output_path, data).map_err(CliError::IoError)?;

    //Derive tmp file from output path
    let tmp_file_path = output_path.with_extension(".tmp");

    //Open the tmp file
    let mut tmp_file = File::open(&tmp_file_path)?;

    //Set the file open options to append data to the written header
    let mut final_file = OpenOptions::new()
        .append(true)
        .open(output_path)?;

    //Copy the tmp file data to the end of the .ssmc file.
    copy(&mut tmp_file, &mut final_file)?;

    //Remove the tmp file as it is no longer needed.
    remove_file(&tmp_file_path)?;

    Ok(())
}

/// Loads the application configuration from a file, creating a default one if
/// needed.
///
/// This function orchestrates the loading of the `SpriteShrinkConfig` struct.
/// It first determines the appropriate, platform-specific path for the 
/// configuration file using the `confy` crate.
///
/// If the configuration file does not exist at that path, it will create the
/// necessary parent directories and write a default, well-commented 
/// configuration file to help the user get started.
///
/// It then proceeds to load and parse the TOML configuration file into a
/// `SpriteShrinkConfig` instance.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(SpriteShrinkConfig)` containing the loaded application settings.
/// - `Err(CliError)` if any part of the process fails.
///
/// # Errors
///
/// This function will return an error in the following situations:
/// - The configuration directory cannot be created due to permissions issues.
/// - The default configuration file cannot be written to disk.
/// - The existing configuration file is malformed (invalid TOML) and cannot 
///   be parsed.
/// - A general I/O error occurs during file reading or writing.
pub fn load_config() -> Result<SpriteShrinkConfig, CliError> {
    //Set app name to be reused.
    let app_name = &APPIDENTIFIER.application;

    //Get the confy config_path for where the config will be stored.
    let config_path = confy::get_configuration_file_path(
        app_name, 
        APPIDENTIFIER.config_name)?;
    
    /*Check if the config exists. 
    If it does not create and fill that config with a default commented
    config file.
    If the config does, continue to loading it with confy.*/
    if !config_path.exists() {
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(config_path, DEFAULT_CONFIG_WITH_COMMENTS)?;
    }

    //Load config from disk.
    match confy::load(app_name, APPIDENTIFIER.config_name){
        Ok(cfg) => Ok(cfg),
        Err(e) => Err(CliError::from(e)),
    }
}

/// Appends a slice of bytes to the end of a file, creating the file if it
/// does not exist.
///
/// This function is designed for efficiently writing data in chunks, such as
/// when buffering compressed data to a temporary file. It opens the file in
/// append mode and uses a `BufWriter` to batch writes, minimizing system calls
/// and improving performance.
///
/// # Arguments
///
/// * `path`: A `Path` reference to the file to which data will be appended.
/// * `data`: A byte slice (`&[u8]`) containing the data to write.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the data was successfully appended to the file.
/// - `Err(CliError::Io)` if the file cannot be opened or written to.
pub fn append_data_to_file(path: &Path, data: &[u8]) -> Result<(), CliError> {
    let file = OpenOptions::new()
        //Create the file if it doesn't exist.
        .create(true)   
        //Append to the end of the file.
        .append(true)   
        //Open the file with the specified options at the specified path
        .open(path)?; 

    //Creates an in-memory buffer that batches writes to the OS.
    let mut writer = BufWriter::new(file);

    //`write_all` ensures that the entire `data` slice is written.
    writer.write_all(data)?;

    Ok(())
}

/// Calculates the total combined size of all files from a list of paths.
///
/// This function iterates over a slice of `PathBuf`s, retrieves the metadata
/// for each file, and sums their individual sizes to determine the total
/// input data size in bytes. This is useful for making decisions about
/// memory usage, such as whether to use an in-memory or on-disk database
/// during the compression process.
///
/// # Arguments
///
/// * `file_paths`: A slice of `PathBuf`s, where each path points to a file
///   whose size will be included in the total.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(u64)` containing the total combined size of all files in bytes.
/// - `Err(CliError::Io)` if the metadata for any file cannot be read.
///
/// # Errors
///
/// This function will return an `Err` if `fs::metadata` fails for any of the
/// provided paths, for example, if a file does not exist or the application
/// lacks the necessary permissions to read it.
pub fn calc_tot_input_size(
    file_paths: &[PathBuf],
) -> Result<u64, CliError> {
    let mut size_sum: u64 = 0;
    
    for path in file_paths{
        let metadata = fs::metadata(path)?;
        size_sum += metadata.len();
    }

    Ok(size_sum)
}

/// Manages log file retention by deleting old log files.
///
/// This function is responsible for cleaning up the application's log
/// directory. It identifies the platform-specific local data directory,
/// finds any log files matching the application's naming convention
/// (e.g., `debug.log.<date>`), and compares their creation timestamps
/// against the specified retention period. Any log file found to be older
/// than the retention period is removed.
///
/// A special value of `0` for `retention_days` will disable the cleanup
/// process, effectively keeping all log files indefinitely.
///
/// # Arguments
///
/// * `retention_days`: The maximum age of log files to keep, in days. Files
///   older than this will be deleted. A value of 0 disables log cleanup.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the cleanup process completes successfully or if no logs
///   need to be deleted.
/// - `Err(CliError)` if an I/O error occurs while reading the log
///   directory or removing a file.
///
/// # Errors
///
/// This function can return an error if it encounters issues with file
/// system operations, such as a lack of permissions to read the log
/// directory or delete files within it.
pub fn cleanup_old_logs(
    retention_days: u16
) -> Result<(), CliError> {
    if retention_days == 0 {
        //A value of 0 means logs are retained indefinitely.
        return Ok(());
    }

    let proj_dirs = ProjectDirs::from(
        APPIDENTIFIER.qualifier, 
        APPIDENTIFIER.organization, 
        APPIDENTIFIER.application)
    .expect("Failed to find a valid project directory.");

    let mut log_dir = PathBuf::from(proj_dirs.data_local_dir());
    log_dir.push("logs");

    if !log_dir.exists() {
        return Ok(());
    }

    let now = SystemTime::now();
    let retention_duration = std::time::Duration::from_secs(
        retention_days as u64 * 24 * 60 * 60 //Convert to secs
    );

    for log in read_dir(log_dir)? {
        let log = log?;
        let path = log.path();

        if path.is_file() && 
            path.to_string_lossy().contains("debug.log") &&
            let Ok(metadata) = metadata(&path) &&
            let Ok(created_time) = metadata.created() &&
            let Ok(age) = now.duration_since(created_time) &&
            age > retention_duration {
                remove_file(&path)?;
            }
    }


    Ok(())
}