//! Provides all file system input/output operations for the application.
//!
//! This module abstracts the complexities of interacting with the file
//! system. It includes functions for reading from and writing to files,
//! checking for file and directory existence, organizing input paths,
//! and collecting all files from a set of directories. All functions are
//! designed to be robust and provide clear error handling.

use std::{
    collections::HashMap,
    fmt::Display,
    fs::{self, File, OpenOptions, metadata, read_dir, remove_file},
    hash::Hash,
    io::{self, BufWriter, Read, Seek, SeekFrom, Write, copy},
    path::{Path, PathBuf},
    process::id,
    sync::{Arc, Mutex},
    time::SystemTime
};

use dashmap::DashMap;
use directories::ProjectDirs;
use tracing::{
    debug,
    error,
    warn
};

use crate::{
    cli_types::{
        APPIDENTIFIER, CacheBackend, CacheInfo, LocationData,
        SpriteShrinkConfig
    },
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
const DEFAULT_CONFIG_WITH_COMMENTS:
&str = r#"# Sets the numerical compression level.
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
pub fn files_from_dirs(
    dir_paths: &[PathBuf]
) -> Result<Vec<PathBuf>, CliError> {
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

/// Assembles the final archive file by combining metadata and compressed data.
///
/// This function completes the archive creation process by writing all
/// necessary components to the final output file. It operates in two stages to
/// ensure data integrity and efficient handling of large files:
///
/// 1. It first writes the initial data block, which contains the archive
///    header and all serialized metadata (file manifest, chunk index, etc.),
///    to the final destination file.
/// 2. It then appends the compressed data, which is streamed from a temporary
///    file where it was stored during the compression phase.
///
/// After the contents of the temporary file have been successfully appended,
/// the temporary file is deleted, leaving a complete and valid `.ssmc`
/// archive.
///
/// # Arguments
///
/// * `output_path`: A `Path` reference to the final destination file for the
///   archive.
/// * `tmp_file_path`: A `Path` reference to the temporary file containing the
///   compressed data blob.
/// * `data`: A byte slice (`&[u8]`) containing the archive's header and all
///   other serialized metadata.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the final archive is successfully written and the temporary
///   file is cleaned up.
/// - `Err(CliError::Io)` if any file system operation fails, such as creating
///   the output directory, writing the header, reading the temporary file, or
///   deleting it.
pub fn write_final_archive(
    output_path: &Path,
    tmp_file_path: &Path,
    data: &[u8]
) -> Result<(), CliError> {
    /*Check if the parent directory of the target output exists,
    if not create it.*/
    if let Some(dir) = output_path.parent() {
        fs::create_dir_all(dir)?;
    }

    //Write header data to disk
    fs::write(output_path, data).map_err(CliError::IoError)?;

    //Open the tmp file
    let mut tmp_file = File::open(tmp_file_path)?;

    //Set the file open options to append data to the written header
    let mut final_file = OpenOptions::new()
        .append(true)
        .open(output_path)?;

    //Copy the tmp file data to the end of the .ssmc file.
    copy(&mut tmp_file, &mut final_file)?;

    //Remove the tmp file as it is no longer needed.
    remove_file(tmp_file_path)?;

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
            let Ok(created_time) = metadata.modified() &&
            let Ok(age) = now.duration_since(created_time) &&
            age > retention_duration {
                remove_file(&path)?;
            }
    }


    Ok(())
}

/// Generates unique paths for temporary cache.
///
/// This function creates platform-specific paths for two temporary cache
/// files: one for the main compression process and another for auto-tuning.
/// The paths are constructed using the application's cache directory and the
/// current process ID (PID) to ensure uniqueness and avoid conflicts between
/// multiple concurrent instances of the application.
///
/// # Returns
///
/// A tuple `(PathBuf, PathBuf, u32)` containing:
/// - The path for the main temporary database (e.g., `.../cache/12345.tmp`).
/// - The path for the auto-tuning temporary database (e.g.,
///   `.../cache/auto_tune_12345.tmp`).
/// - The process ID (PID) used to generate the unique filenames.
pub fn get_cache_paths() -> CacheInfo {
    let pid = id();

    let cache_dir = ProjectDirs::from(
        APPIDENTIFIER.qualifier,
        APPIDENTIFIER.organization,
        APPIDENTIFIER.application)
    .unwrap()
    .cache_dir()
    .to_path_buf();

    let cache_path = cache_dir.clone()
        .join(format!("{pid}"))
        .with_extension("tmp");

    let at_cache_path = cache_dir.clone()
        .join(format!("auto_tune_{pid}"))
        .with_extension("tmp");

    CacheInfo {
        cache_path,
        at_cache_path,
        id: pid
    }
}


pub struct TempCache<H: Hash + Eq> {
    backend: Arc<Mutex<CacheBackend<H>>>,
}

impl<H: Hash + Eq + Copy + Display + Send + Sync + 'static> TempCache<H> {
    pub fn new(file_path: PathBuf, use_in_memory: bool) -> io::Result<Self> {
        let backend = if use_in_memory {

            CacheBackend::InMemory{
                map: Arc::new(DashMap::new()),
                data_size: 0
            }
        } else {
            File::create(&file_path)?;
            CacheBackend::OnDisk {
                file_path,
                location_map: HashMap::new(),
                current_offset: 0,
            }
        };

        Ok(Self {
            backend: Arc::new(Mutex::new(backend)),
        })
    }

    pub fn insert_batch(&self, items: &[(H, Vec<u8>)]) -> Result<(), CliError> {
        let mut backend_guard = self.backend.lock().unwrap();

        match &mut *backend_guard {
            CacheBackend::InMemory{map, data_size} => {
                for (key, data) in items {
                    if let dashmap::mapref::entry::Entry::Vacant(entry) = map.entry(*key) {
                        entry.insert(data.to_vec());
                        *data_size += data.len() as u64;
                    }
                }
            }
            CacheBackend::OnDisk {
                file_path,
                location_map,
                current_offset
            } => {
                let mut tmp_file = OpenOptions::new()
                    .append(true)
                    .open(file_path)?;

                for (key, data) in items {
                    if !location_map.contains_key(key) {
                        let data_len = data.len() as u32;

                        tmp_file.write_all(data)?;

                        location_map.insert(*key, LocationData {
                            offset: *current_offset,
                            length: data_len,
                        });

                        *current_offset += data_len as u64;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn get_chunks(
        &self,
        hashes: &[H],
    ) -> Result<Vec<Vec<u8>>, CliError>{
        let backend_guard = self.backend.lock().unwrap();

        match &*backend_guard {
            CacheBackend::InMemory{map, ..} => {
                hashes
                    .iter()
                    .map(|hash| {
                        map.get(hash)
                            .map(|r| r.value().clone())
                            .ok_or_else(|| CliError::KeyNotFound(format!(
                                "Hash {} not found in data_store",
                                hash
                            )))
                    })
                    .collect()
            }
            CacheBackend::OnDisk{
                file_path,
                location_map,
                ..
            } => {
                let mut tmp_file = File::open(file_path)?;
                let mut ret_chunks: Vec<Vec<u8>> = Vec::with_capacity(
                    hashes.len()
                );

                for hash in hashes{
                    match location_map.get(hash){
                        Some(location) => {
                            let mut data_buffer: Vec<u8> =  vec![
                                0;
                                location.length as usize
                            ];
                            tmp_file.seek(SeekFrom::Start(location.offset))?;
                            tmp_file.read_exact(&mut data_buffer)?;
                            ret_chunks.push(data_buffer)
                        }
                        None => {
                            return Err(CliError::KeyNotFound(
                               format!("Hash {} not found in data_store", hash)
                            ));
                        }
                    }
                }
                Ok(ret_chunks)
            }
        }
    }

    pub fn get_keys(&self) -> Result<Vec<H>, CliError> {
        let backend_guard = self.backend.lock().unwrap();

        let keys = match &*backend_guard {
            CacheBackend::InMemory{map, ..} => {
                map.iter()
                .map(|entry| *entry.key())
                .collect()
            }
            CacheBackend::OnDisk {location_map, ..} => {
                location_map
                    .keys()
                    .cloned()
                    .collect()
            }
        };

        Ok(keys)
    }

    pub fn get_tot_data_size(&self) -> u64 {
        let backend_guard = self.backend.lock().unwrap();

        match &*backend_guard {
            CacheBackend::InMemory{data_size, ..} => {
                *data_size
            }
            CacheBackend::OnDisk {current_offset, ..} => {
                *current_offset
            }
        }
    }
}


impl <H: Hash + Eq> Drop for TempCache<H> {
    fn drop(&mut self) {
        let backend_guard = self.backend.lock().unwrap();

        if let CacheBackend::OnDisk {file_path, .. } = &*backend_guard &&
            file_path.exists()
        {
            if let Err(e) = remove_file(file_path) {
                error!(
                    "Failed to remove temporary file at {:?}: {}",
                    file_path, e
                );
            } else {
                debug!("Successfully removed temporary file: {:?}",
                    file_path
                );
            }
        }
    }
}


/// Reads the entire metadata block (manifest, dictionary, and chunk index)
/// from an archive file into a single byte vector.
///
/// This function leverages the information provided in the `FileHeader` to
/// determine the starting offset and the total contiguous length of the
/// metadata block within the archive. It then uses `read_file_data` to
/// efficiently retrieve this block without loading the entire file into
/// memory.
///
/// The metadata block is contiguous in the file, according to the file
/// specification, immediately following the header and consisting of the
/// manifest, dictionary, and chunk index in that order.
///
/// # Arguments
///
/// * `file_path`: A `Path` pointing to the archive file from which the
///   metadata block is to be read.
/// * `header`: A reference to the `sprite_shrink::FileHeader` which contains
///   the necessary offset and length information for the metadata sections.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<u8>)` containing the complete metadata block as a byte vector.
/// - `Err(CliError)` if the file cannot be opened, seeking fails, or reading
///   the specified range of bytes encounters an I/O error.
///
/// # Errors
///
/// This function can return an error primarily if `read_file_data` fails,
/// which can happen due to:
/// - `CliError::Io` if the file specified by `file_path` cannot be accessed,
///   if seeking to `header.man_offset` fails, or if the number of bytes
///   indicated by `total_length` cannot be read from the file.
pub fn read_metadata_block(
    file_path: &Path,
    header: &sprite_shrink::FileHeader,
) -> Result<Vec<u8>, CliError> {
    let start_offset = header.man_offset;

    /*The block is contiguous, so the total length is the sum of the three
    sections.*/
    let total_length = (
        header.man_length +
        header.dict_length +
        header.chunk_index_length
    ) as usize;

    read_file_data(file_path, &start_offset, &total_length)
}
