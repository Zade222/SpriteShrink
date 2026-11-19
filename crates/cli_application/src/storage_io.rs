//! Provides all file system input/output operations for the application.
//!
//! This module abstracts the complexities of interacting with the file
//! system. It includes functions for reading from and writing to files,
//! checking for file and directory existence, organizing input paths,
//! and collecting all files from a set of directories. All functions are
//! designed to be robust and provide clear error handling.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    fs::{self, File, OpenOptions, metadata, read_dir, remove_file},
    hash::Hash,
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::id,
    sync::{Arc, Mutex},
    time::SystemTime
};

use dashmap::DashMap;
use directories::ProjectDirs;
use tempfile::{Builder, NamedTempFile};
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

/// The default, commented configuration file, from `default_config.toml`
/// next to this file.
///
/// It is written to the config path when the application runs for the
/// first time and no configuration exists, so each option is explained
/// for users to customize. Its values must match
/// `SpriteShrinkConfig::default()`.
const DEFAULT_CONFIG_WITH_COMMENTS: &str = include_str!("default_config.toml");

/// Why `files_from_dirs` skips a directory entry of type `file_type`, for
/// the warning it logs. `None` for a regular file, which isn't skipped.
///
/// `DirEntry::file_type` doesn't follow symbolic links, so a link reports
/// as a link even when it points at a file or a folder.
fn skip_reason(file_type: fs::FileType) -> Option<&'static str> {
    if file_type.is_file() {
        None
    } else if file_type.is_symlink() {
        Some("symbolic links aren't followed")
    } else if file_type.is_dir() {
        Some("folders inside an input folder aren't searched")
    } else {
        Some("it isn't a regular file")
    }
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

/// The name of the entry an input file becomes inside an SSMC archive,
/// which is the input's file name without its folder, so `x/a.bin` and
/// `y/a.bin` both become `a.bin`.
pub fn entry_name(path: &Path) -> Option<String> {
    path.file_name().map(|name| name.to_string_lossy().into_owned())
}

/// Whether `path`'s extension is `extension` in any case, so `B.CUE` is a
/// CUE sheet as much as `a.cue`. `extension` is given without the dot.
pub fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// The path compression writes its archive to: `output` with `.ssmc`
/// appended, or `.ssmd` in optical mode, unless it already ends in that
/// extension in any case. Any other extension is kept, so `roms.v2` gives
/// `roms.v2.ssmc`.
///
/// # Errors
///
/// Returns `CliError::InvalidPath` if `output` has no file name to add
/// the extension to, such as `..` or `/`.
pub fn archive_path(output: &Path, mode: &str) -> Result<PathBuf, CliError> {
    let extension = if mode == "optical" { "ssmd" } else { "ssmc" };
    if has_extension(output, extension) {
        return Ok(output.to_path_buf());
    }
    let Some(name) = output.file_name() else {
        return Err(CliError::InvalidPath(format!(
            "The output path {} has no file name for the archive.",
            output.display()
        )));
    };
    let mut name = name.to_owned();
    name.push(".");
    name.push(extension);
    Ok(output.with_file_name(name))
}

/// The title a disc is stored under in an SSMD archive: its CUE file's stem,
/// cut to the 255 characters `parse_cue` keeps. `None` for a file that
/// isn't a CUE sheet, since only CUE sheets become discs.
pub fn disc_title(path: &Path) -> Option<String> {
    if !has_extension(path, "cue") {
        return None;
    }
    path.file_stem()
        .map(|stem| stem.to_string_lossy().chars().take(255).collect())
}

/// Finds input files that would be stored under the same name, as `name_of`
/// names them, and returns each shared name with its paths, sorted by name.
/// The archive keeps one entry per name, so all but one would be lost.
///
/// The same file given twice, such as through its folder and on its own,
/// isn't a conflict: both paths lead to one file.
pub fn duplicate_names(
    paths: &[PathBuf],
    name_of: impl Fn(&Path) -> Option<String>,
) -> Vec<(String, Vec<PathBuf>)> {
    let mut by_name: BTreeMap<String, Vec<(PathBuf, PathBuf)>> =
        BTreeMap::new();

    for path in paths {
        let Some(name) = name_of(path) else { continue };
        /*Compare where paths lead, not how they're spelled. A path that
        can't be resolved is compared as given.*/
        let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        let same_name = by_name.entry(name).or_default();
        if !same_name.iter().any(|(_, other)| *other == resolved) {
            same_name.push((path.clone(), resolved));
        }
    }

    by_name
        .into_iter()
        .filter(|(_, same_name)| same_name.len() > 1)
        .map(|(name, same_name)| {
            (name, same_name.into_iter().map(|(path, _)| path).collect())
        })
        .collect()
}

/// Collects all regular file paths from a slice of directories.
///
/// This function iterates over a list of directory paths and gathers
/// all entries that are regular files. It performs a shallow search,
/// meaning it does not recurse into subdirectories. Symbolic links are
/// ignored to prevent duplicate processing. Each skipped entry is logged
/// with the reason it was skipped.
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
            match entry.file_type() {
                Ok(file_type) => match skip_reason(file_type) {
                    None => file_paths.push(path),
                    Some(reason) => {
                        warn!("Ignoring {}: {reason}", path.display());
                    }
                },
                Err(err) => warn!(
                    "Ignoring {}: its type couldn't be read: {err}",
                    path.display()
                ),
            }
        }
        //Return vector of file paths.
        Ok(file_paths)
    })
}

/// The most bytes of the final file name that go into its temp file's
/// name. File systems commonly allow 255 bytes per name, and an entry name
/// can use all of them, so the temp name can't hold the whole of it.
const TEMP_NAME_MAX: usize = 64;

/// Writes the file at `final_path` through a temp file in the same folder,
/// so `final_path` is either left as it was or replaced by the complete
/// new file.
///
/// `write` gets a `BufWriter` with `buffer_size` bytes of buffer over the
/// temp file. When it returns `Ok`, the buffer is flushed, and only then is
/// the temp file renamed over `final_path`. On any error, from `write`,
/// the flush or the rename, the temp file is deleted.
///
/// The temp file is named `.<final name>.<random>.tmp`, with the final name
/// cut to [`TEMP_NAME_MAX`] bytes, and is opened with create-new. It never
/// replaces or writes through a file that is already there, a symbolic
/// link included. A process killed outright leaves its temp file behind.
///
/// With [`ExistingFile::Refuse`], the rename fails if `final_path` exists
/// by then, even if it appeared while `write` ran, so a file is never
/// replaced without `--force`. See [`persist`].
///
/// The data isn't synced to disk before the rename, so after a power cut
/// the file may be empty or short, depending on the file system.
///
/// # Errors
///
/// Returns `write`'s error, `CliError::FileExistsError` if `final_path`
/// exists and `existing` is `Refuse`, or the I/O error if the temp file
/// can't be created, flushed or renamed.
pub fn write_via_temp<E>(
    final_path: &Path,
    existing: ExistingFile,
    buffer_size: usize,
    write: impl FnOnce(&mut BufWriter<NamedTempFile>) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<io::Error> + From<CliError>,
{
    /*The temp file has to be on the same file system as final_path for the
    rename to replace it in one step.*/
    let dir = parent_dir(final_path);

    let prefix = temp_prefix(final_path);
    let mut builder = Builder::new();
    builder.prefix(&prefix).suffix(".tmp");
    /*tempfile creates files only the owner can read (0o600). Ask for what
    File::create asks for, so the finished file gets the usual permissions
    (0o666 less the umask).*/
    #[cfg(unix)]
    builder.permissions(
        std::os::unix::fs::PermissionsExt::from_mode(0o666)
    );

    let tmp = builder.tempfile_in(dir)?;
    let tmp = write_buffered(tmp, buffer_size, write)?;
    persist(tmp, final_path, existing)
}

/// What a write does when its final path already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExistingFile {
    /// Replace it, as `--force` asks.
    Replace,
    /// Leave it and fail with `CliError::FileExistsError`.
    Refuse,
}

impl ExistingFile {
    /// `Replace` with `--force`, `Refuse` without.
    pub fn from_force(force: bool) -> Self {
        if force { Self::Replace } else { Self::Refuse }
    }
}

/// Renames the finished temp file to `final_path`. On an error the temp
/// file is dropped, which deletes it.
///
/// `Refuse` uses tempfile's no-replace rename, which is one atomic step on
/// Linux, macOS and Windows: an existing file, or one created after any
/// earlier check, is never replaced. Elsewhere tempfile hard-links the
/// file to its name instead. On a file system that supports neither, such
/// as some network shares, the no-replace rename fails for that reason
/// and the file is renamed after checking that nothing is there. That
/// leaves a short window in which a file created by another program could
/// be replaced.
fn persist<E>(
    tmp: NamedTempFile,
    final_path: &Path,
    existing: ExistingFile,
) -> Result<(), E>
where
    E: From<io::Error> + From<CliError>,
{
    let exists = || CliError::FileExistsError(final_path.to_path_buf());
    let result = match existing {
        ExistingFile::Replace => tmp.persist(final_path),
        ExistingFile::Refuse => match tmp.persist_noclobber(final_path) {
            /*Whatever the error, a name already at final_path is the
            reason to report. symlink_metadata checks the link itself, so
            a dangling symbolic link counts as there.*/
            Err(_) if fs::symlink_metadata(final_path).is_ok() => {
                return Err(exists().into());
            }
            Err(err) => err.file.persist(final_path),
            Ok(file) => Ok(file),
        },
    };
    result.map(drop).map_err(|err| E::from(err.error))
}

/// Runs `write` on a `BufWriter` over `inner`, then flushes it and returns
/// `inner`.
///
/// A `BufWriter` flushes itself when dropped but discards any error, so
/// the end of the data could go missing without notice.
/// `into_inner` flushes and returns that error.
fn write_buffered<W, E>(
    inner: W,
    buffer_size: usize,
    write: impl FnOnce(&mut BufWriter<W>) -> Result<(), E>,
) -> Result<W, E>
where
    W: Write,
    E: From<io::Error>,
{
    let mut writer = BufWriter::with_capacity(buffer_size, inner);
    write(&mut writer)?;
    writer.into_inner().map_err(|err| E::from(err.into_error()))
}

/// Returns `.<final name>.`, the start of a temp file's name, with the
/// final name cut to at most [`TEMP_NAME_MAX`] bytes on a character
/// boundary.
fn temp_prefix(final_path: &Path) -> String {
    let name = final_path.file_name().unwrap_or_default().to_string_lossy();
    let mut prefix = String::from(".");
    for c in name.chars() {
        if prefix.len() + c.len_utf8() > TEMP_NAME_MAX + 1 {
            break;
        }
        prefix.push(c);
    }
    prefix.push('.');
    prefix
}

/// Reads `data_length` bytes of a file, starting at `data_index`. See
/// [`ArchiveFile::read`].
///
/// # Errors
///
/// Returns `CliError::DataIntegrity` if the bytes run past the end of the
/// file, or `CliError::Io` if the file can't be opened or read.
pub fn read_file_data(
    filepath: &Path,
    data_index: u64,
    data_length: usize
) -> Result<Vec<u8>, CliError> {
    ArchiveFile::open(filepath)?.read(data_index, data_length)
}

/// An archive opened for reading, with its length read once.
///
/// Reads don't move a shared file position, so several threads can read
/// through one `&ArchiveFile` at once.
#[derive(Debug)]
pub struct ArchiveFile {
    file: File,
    len: u64,
}

impl ArchiveFile {
    /// # Errors
    ///
    /// Returns `CliError::Io` if the file can't be opened or its length
    /// can't be read.
    pub fn open(path: &Path) -> Result<Self, CliError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }

    /// Reads `length` bytes starting at `offset`.
    ///
    /// The offset and length usually come from the archive itself, so
    /// they're checked against the file's length before a buffer that large
    /// is allocated.
    ///
    /// # Errors
    ///
    /// Returns `CliError::DataIntegrity` if the bytes run past the end of
    /// the file, or `CliError::Io` if they can't be read.
    pub fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>, CliError> {
        let end = u64::try_from(length).ok()
            .and_then(|length| offset.checked_add(length));
        if end.is_none_or(|end| end > self.len) {
            return Err(CliError::DataIntegrity(format!(
                "{length} bytes at offset {offset} run past the end of the \
                {}-byte file",
                self.len
            )));
        }

        let mut buf = vec![0; length];
        read_exact_at(&self.file, &mut buf, offset)?;
        Ok(buf)
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

/// Fills `buf` from `offset`. `seek_read`, like `read`, can return fewer
/// bytes than asked for, and Windows has no `read_exact_at`, so this calls
/// it until `buf` is full.
#[cfg(windows)]
fn read_exact_at(
    file: &File,
    mut buf: &mut [u8],
    mut offset: u64,
) -> io::Result<()> {
    use std::os::windows::fs::FileExt;

    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => {
                let rest = buf;
                buf = &mut rest[read..];
                offset += read as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Returns the folder `path` is in, or `.` when `path` is a bare file name
/// (or has no parent at all, as `/` and the empty path don't).
pub fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// Writes an archive to `final_path`: `prefix` (the header and all
/// metadata), then the whole of `data`, the compressed data the compress
/// modes collected in a temp file.
///
/// The archive is written through [`write_via_temp`], so an existing
/// archive at `final_path` is replaced only once the new one is complete,
/// and only with [`ExistingFile::Replace`]. A failure leaves it as it was.
///
/// # Errors
///
/// Returns `CliError::FileExistsError` if `final_path` exists and
/// `existing` is `Refuse`, or the I/O error if `data` can't be read or the
/// archive can't be written.
pub fn write_archive(
    final_path: &Path,
    existing: ExistingFile,
    prefix: &[u8],
    data: &mut File,
) -> Result<(), CliError> {
    data.rewind()?;
    write_via_temp(final_path, existing, 64 * 1024, |writer| {
        writer.write_all(prefix)?;
        /*Copy file to file, past the buffer, so std can use the system's
        file copy (copy_file_range on Linux). The buffer has to be empty
        first, or the prefix would land after the data.*/
        writer.flush()?;
        io::copy(data, writer.get_mut().as_file_mut())?;
        Ok(())
    })
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
/// This function finds the application's log directory in the
/// platform-specific local data directory and removes the logs in it that
/// are older than the retention period, as [`remove_old_logs`] describes.
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
    let proj_dirs = ProjectDirs::from(
        APPIDENTIFIER.qualifier,
        APPIDENTIFIER.organization,
        APPIDENTIFIER.application)
    .expect("Failed to find a valid project directory.");

    let mut log_dir = PathBuf::from(proj_dirs.data_local_dir());
    log_dir.push("logs");

    remove_old_logs(&log_dir, retention_days, SystemTime::now())
}

/// Removes the log files in `log_dir` (named like `debug.log.<date>`) whose
/// last modification is more than `retention_days` before `now`. A log
/// exactly that old is kept. `0` keeps every log, and a missing `log_dir`
/// has nothing to remove.
///
/// `now` is a parameter so tests can place files at exact ages.
///
/// # Errors
///
/// Returns `CliError::IoError` if `log_dir` can't be read or a log can't be
/// removed.
fn remove_old_logs(
    log_dir: &Path,
    retention_days: u16,
    now: SystemTime,
) -> Result<(), CliError> {
    if retention_days == 0 {
        //A value of 0 means logs are retained indefinitely.
        return Ok(());
    }

    if !log_dir.exists() {
        return Ok(());
    }

    let retention_duration = std::time::Duration::from_secs(
        u64::from(retention_days) * 24 * 60 * 60 //Convert to secs
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
/// A `CacheInfo` holding:
/// - The path for the main temporary database (e.g., `.../cache/12345.tmp`).
/// - The path for the auto-tuning temporary database (e.g.,
///   `.../cache/auto_tune_12345.tmp`).
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
    }
}


pub struct TempCache<H: Hash + Eq> {
    backend: Arc<Mutex<CacheBackend<H>>>,
}

impl<H: Copy + Display + Eq + Hash + Ord + Send + Sync + 'static> TempCache<H> {
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

        let mut keys: Vec<H> = match &*backend_guard {
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

        keys.sort();

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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_file_read_must_end_inside_the_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data");
        fs::write(&path, [1, 2, 3, 4]).unwrap();

        assert_eq!(read_file_data(&path, 2, 2).unwrap(), [3, 4]);

        for (offset, length) in [(2, 3), (u64::MAX, 1)] {
            let Err(CliError::DataIntegrity(message)) =
                read_file_data(&path, offset, length)
            else {
                panic!("{length} bytes at {offset} were read");
            };
            assert!(message.contains("4-byte file"), "{message}");
        }
    }

    #[test]
    fn the_default_config_file_holds_the_default_config() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, DEFAULT_CONFIG_WITH_COMMENTS).unwrap();
        let loaded: SpriteShrinkConfig = confy::load_path(&path).unwrap();

        // The config has no `PartialEq`, so compare it as JSON.
        let defaults =
            serde_json::to_value(SpriteShrinkConfig::default()).unwrap();
        assert_eq!(serde_json::to_value(&loaded).unwrap(), defaults);

        /*Missing keys load as their defaults, so the comparison above can't
        catch one the file leaves out.*/
        for key in defaults.as_object().unwrap().keys() {
            let line = format!("\n{key} = ");
            assert!(
                DEFAULT_CONFIG_WITH_COMMENTS.contains(&line),
                "the default config file has no {key}"
            );
        }
    }

    /// A folder holding `a.bin`, `b.bin` and a subfolder with `c.bin`.
    fn input_folder() -> TempDir {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.bin"), b"a").unwrap();
        fs::write(tmp.path().join("b.bin"), b"b").unwrap();
        fs::create_dir(tmp.path().join("nested")).unwrap();
        fs::write(tmp.path().join("nested/c.bin"), b"c").unwrap();
        tmp
    }

    fn file_type_of(path: &Path) -> fs::FileType {
        fs::symlink_metadata(path).unwrap().file_type()
    }

    #[test]
    fn input_paths_are_split_into_files_and_folders_in_order() {
        let tmp = input_folder();
        let at = |name: &str| tmp.path().join(name);

        let (files, folders) = organize_paths(&[
            at("b.bin"),
            at("nested"),
            at("missing.bin"),
            at("a.bin"),
            tmp.path().to_path_buf(),
        ])
        .unwrap();

        // `validate_args` has already rejected a missing path, so one here
        // is dropped.
        assert_eq!(files, [at("b.bin"), at("a.bin")]);
        assert_eq!(folders, [at("nested"), tmp.path().to_path_buf()]);
    }

    #[test]
    fn only_top_level_regular_files_are_collected() {
        let tmp = input_folder();

        let mut files = files_from_dirs(&[tmp.path().to_path_buf()]).unwrap();
        files.sort();
        assert_eq!(
            files,
            [tmp.path().join("a.bin"), tmp.path().join("b.bin")]
        );
    }

    #[test]
    fn a_subfolder_is_skipped_as_a_folder() {
        let tmp = input_folder();

        assert_eq!(skip_reason(file_type_of(&tmp.path().join("a.bin"))), None);
        assert_eq!(
            skip_reason(file_type_of(&tmp.path().join("nested"))),
            Some("folders inside an input folder aren't searched")
        );
    }

    #[test]
    fn duplicate_names_lists_each_shared_name_with_its_paths() {
        let tmp = TempDir::new().unwrap();
        let mut paths = Vec::new();
        for dir in ["x", "y"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
            for name in ["a.bin", "b.bin"] {
                let path = tmp.path().join(dir).join(name);
                fs::write(&path, name).unwrap();
                paths.push(path);
            }
        }
        fs::write(tmp.path().join("x/c.bin"), b"c").unwrap();
        paths.push(tmp.path().join("x/c.bin"));

        let duplicates = duplicate_names(&paths, entry_name);
        let at = |dir: &str, name: &str| tmp.path().join(dir).join(name);
        assert_eq!(
            duplicates,
            [
                ("a.bin".to_string(), vec![at("x", "a.bin"), at("y", "a.bin")]),
                ("b.bin".to_string(), vec![at("x", "b.bin"), at("y", "b.bin")]),
            ]
        );
    }

    #[test]
    fn one_file_reached_two_ways_is_not_a_duplicate() {
        let tmp = input_folder();
        let direct = tmp.path().join("a.bin");
        let roundabout = tmp.path().join("nested/../a.bin");

        assert_eq!(
            duplicate_names(&[direct, roundabout], entry_name),
            Vec::new()
        );
    }

    #[test]
    fn the_archive_path_keeps_the_output_and_adds_the_extension() {
        let cases = [
            ("roms", "default", "roms.ssmc"),
            ("roms", "optical", "roms.ssmd"),
            ("out/roms.ssmc", "default", "out/roms.ssmc"),
            ("out/roms.SSMC", "default", "out/roms.SSMC"),
            ("roms.v2", "default", "roms.v2.ssmc"),
            ("roms.ssmd", "default", "roms.ssmd.ssmc"),
            ("out/", "default", "out.ssmc"),
        ];
        for (output, mode, expected) in cases {
            assert_eq!(
                archive_path(Path::new(output), mode).unwrap(),
                Path::new(expected),
                "{output} in {mode} mode"
            );
        }

        for output in ["..", "/"] {
            assert!(
                matches!(
                    archive_path(Path::new(output), "default"),
                    Err(CliError::InvalidPath(_))
                ),
                "{output}"
            );
        }
    }

    #[test]
    fn a_disc_title_is_the_cue_stem_cut_to_255_characters() {
        assert_eq!(disc_title(Path::new("x/game.cue")).as_deref(), Some("game"));
        assert_eq!(disc_title(Path::new("x/game.bin")), None);
        assert_eq!(disc_title(Path::new("x/GAME.CUE")).as_deref(), Some("GAME"));

        // Before extensions were compared in any case, `game.CUE` wasn't a
        // disc, so the clash went unreported and that disc was left out.
        let lower = PathBuf::from("x/game.cue");
        let upper = PathBuf::from("y/game.CUE");
        assert_eq!(
            duplicate_names(&[lower.clone(), upper.clone()], disc_title),
            [("game".to_string(), vec![lower, upper])]
        );

        // Two long names that only differ past 255 characters get the same
        // title, as `parse_cue` keeps only the first 255.
        let long = "é".repeat(255);
        let first = PathBuf::from(format!("x/{long}1.cue"));
        let second = PathBuf::from(format!("y/{long}2.cue"));
        assert_eq!(disc_title(&first), Some(long.clone()));
        assert_eq!(
            duplicate_names(&[first.clone(), second.clone()], disc_title),
            [(long, vec![first, second])]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_skipped_as_a_link() {
        let tmp = input_folder();
        let to_file = tmp.path().join("link_to_file");
        let to_folder = tmp.path().join("link_to_folder");
        std::os::unix::fs::symlink(tmp.path().join("a.bin"), &to_file)
            .unwrap();
        std::os::unix::fs::symlink(tmp.path().join("nested"), &to_folder)
            .unwrap();

        for link in [&to_file, &to_folder] {
            assert_eq!(
                skip_reason(file_type_of(link)),
                Some("symbolic links aren't followed"),
                "{}",
                link.display()
            );
        }
        let mut files = files_from_dirs(&[tmp.path().to_path_buf()]).unwrap();
        files.sort();
        assert_eq!(
            files,
            [tmp.path().join("a.bin"), tmp.path().join("b.bin")]
        );
    }

    #[test]
    fn only_debug_logs_past_the_retention_are_removed() {
        const DAY: Duration = Duration::from_hours(24);
        let now = SystemTime::now();
        let tmp = TempDir::new().unwrap();
        let file_aged = |name: &str, age: Duration| {
            let path = tmp.path().join(name);
            File::create(&path).unwrap().set_modified(now - age).unwrap();
            path
        };
        let expired = file_aged(
            "debug.log.2026-01-01",
            7 * DAY + Duration::from_secs(1),
        );
        let at_limit = file_aged("debug.log.2026-01-02", 7 * DAY);
        let recent = file_aged("debug.log.2026-01-07", DAY);
        let not_a_log = file_aged("notes.txt", 30 * DAY);

        // 0 keeps every log, however old.
        remove_old_logs(tmp.path(), 0, now).unwrap();
        assert!(expired.exists());

        remove_old_logs(tmp.path(), 7, now).unwrap();
        assert!(!expired.exists());
        for kept in [&at_limit, &recent, &not_a_log] {
            assert!(kept.exists(), "{}", kept.display());
        }

        remove_old_logs(&tmp.path().join("missing"), 7, now).unwrap();
    }

    /*Every test input is small enough for the in-memory cache, so only
    this test runs the on-disk one large inputs use. Both backends get the
    same steps and must give the same answers.*/
    #[test]
    fn both_cache_backends_keep_the_first_copy_of_each_chunk() {
        let tmp = TempDir::new().unwrap();

        for use_in_memory in [false, true] {
            let path = tmp.path().join("cache.tmp");
            let cache = TempCache::<u64>::new(path.clone(), use_in_memory)
                .unwrap();

            cache.insert_batch(&[(1, b"aaa".to_vec()), (2, b"bb".to_vec())])
                .unwrap();
            // Key 2 is already stored, so its new data is dropped.
            cache.insert_batch(&[(2, b"XX".to_vec()), (3, b"c".to_vec())])
                .unwrap();

            assert_eq!(
                cache.get_chunks(&[3, 1, 2]).unwrap(),
                [b"c".to_vec(), b"aaa".to_vec(), b"bb".to_vec()],
                "in memory: {use_in_memory}"
            );
            assert_eq!(cache.get_keys().unwrap(), [1, 2, 3]);
            assert_eq!(cache.get_tot_data_size(), 6);
            assert!(matches!(
                cache.get_chunks(&[4]),
                Err(CliError::KeyNotFound(_))
            ));

            drop(cache);
            assert!(!path.exists(), "in memory: {use_in_memory}");
        }
    }

    /// A writer that fails every write, as a full disk does.
    struct FullDisk;

    impl Write for FullDisk {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /*The data fits in the buffer, so the closure's writes succeed and only
    the final flush reaches the disk.*/
    #[test]
    fn a_failed_final_flush_is_reported() {
        let result = write_buffered(FullDisk, 64, |writer| {
            writer.write_all(b"the last bytes")
        });

        assert_eq!(result.err().unwrap().to_string(), "disk full");
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_and_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("game.bin");
        fs::write(&path, b"old").unwrap();

        let result = write_via_temp(&path, ExistingFile::Replace, 64, |writer| {
            writer.write_all(b"new")?;
            Err(CliError::DataIntegrity("hash mismatch".to_string()))
        });

        assert!(
            matches!(&result, Err(CliError::DataIntegrity(m)) if m == "hash mismatch"),
            "{result:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    /*The file appears while the data is written, after any check made
    before the write started. Without --force it must survive.*/
    #[test]
    fn a_file_that_appears_during_the_write_is_kept_without_force() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("game.bin");

        let result = write_via_temp(&path, ExistingFile::Refuse, 64, |writer| {
            fs::write(&path, b"theirs")?;
            Ok::<_, CliError>(writer.write_all(b"ours")?)
        });

        assert!(
            matches!(&result, Err(CliError::FileExistsError(p)) if *p == path),
            "{result:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"theirs");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);

        write_via_temp::<CliError>(&path, ExistingFile::Replace, 64, |writer| {
            Ok(writer.write_all(b"ours")?)
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"ours");
    }

    /*A link to nothing is still a name in the folder, and the link must
    stay as it was.*/
    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_not_replaced_without_force() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("game.bin");
        std::os::unix::fs::symlink(tmp.path().join("missing"), &path).unwrap();

        let result = write_via_temp::<CliError>(&path, ExistingFile::Refuse, 64, |writer| {
            Ok(writer.write_all(b"ours")?)
        });

        assert!(matches!(result, Err(CliError::FileExistsError(_))), "{result:?}");
        assert!(path.is_symlink());
        assert!(!tmp.path().join("missing").exists());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    /*The data file is opened write-only, so reading it fails after the
    prefix is written. The archive already there must survive.*/
    #[test]
    fn a_failed_archive_write_leaves_the_old_archive() {
        let tmp = TempDir::new().unwrap();
        let out_dir = tmp.path().join("out");
        fs::create_dir(&out_dir).unwrap();
        let archive = out_dir.join("roms.ssmc");
        fs::write(&archive, b"old archive").unwrap();
        let mut data = File::create(tmp.path().join("data")).unwrap();

        assert!(
            write_archive(&archive, ExistingFile::Replace, b"prefix", &mut data)
                .is_err()
        );

        assert_eq!(fs::read(&archive).unwrap(), b"old archive");
        assert_eq!(fs::read_dir(&out_dir).unwrap().count(), 1);
    }

    /*An entry name can use all 255 bytes a file system allows, so the temp
    name can't be the whole name plus more. The name is cut inside a
    two-byte character to check the cut lands on a character boundary.*/
    #[test]
    fn a_255_byte_name_can_be_written() {
        let tmp = TempDir::new().unwrap();
        let name = format!("x{}", "é".repeat(127));
        assert_eq!(name.len(), 255);
        let path = tmp.path().join(&name);

        write_via_temp::<CliError>(&path, ExistingFile::Refuse, 64, |writer| {
            Ok(writer.write_all(b"data")?)
        })
        .unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"data");
        // 1 + 31 × 2 = 63 bytes; the 32nd "é" would pass TEMP_NAME_MAX.
        assert_eq!(temp_prefix(&path), format!(".x{}.", "é".repeat(31)));
    }

    /*tempfile makes owner-only files by default; the extracted file should
    get the permissions any other new file gets.*/
    #[cfg(unix)]
    #[test]
    fn a_written_file_gets_the_usual_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let reference = tmp.path().join("reference");
        File::create(&reference).unwrap();
        let path = tmp.path().join("game.bin");

        write_via_temp::<CliError>(&path, ExistingFile::Refuse, 64, |writer| {
            Ok(writer.write_all(b"data")?)
        })
        .unwrap();

        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode();
        assert_eq!(mode(&path), mode(&reference));
    }
}
