//! Declares the error handling types for the command-line application.
//!
//! This module defines the custom `CliError` enum, which encapsulates
//! all possible failure conditions that can occur within the CLI
//! application. By centralizing error definitions here, the tool
//! provides a clear and consistent error-handling mechanism for all
//! of its command-line operations.

use std::{
    io,
    path::PathBuf
};

use sprite_shrink::SpriteShrinkError;
use directories::ProjectDirs;
use range_parser::RangeError;
use thiserror::Error;
use tracing::{
    level_filters::LevelFilter,
    debug,
    error,
    warn
};
use tracing_appender::{
    rolling, non_blocking::WorkerGuard,
};
use tracing_subscriber::{
    prelude::*,
    registry,
    fmt,
};

use crate::{
    cli_types::APPIDENTIFIER
};

/// Represents all possible errors that can occur in the application.
///
/// This enum uses `thiserror` to derive the `Error` trait, providing a
/// centralized and descriptive way to handle various failure modes,
/// from I/O issues to invalid arguments and internal logic failures.
/// Each variant corresponds to a specific error condition encountered
/// during the application's runtime.
#[derive(Error, Debug)]
pub enum CliError {
    #[error("I/O Error")] 
    Io(#[from] io::Error), 

    #[error("Provided path does not exist. {0}")]
    InvalidPath(String),

    #[error("Provided output path does not exist. \
        Use force flag to create parent directory."
    )]
    InvalidOutputPath(),

    #[error("Invalid ROM index range format. {0}")]
    InvalidFormRomRange(String),

    #[error("Invalid ROM index range. {0}")]
    InvalidRomIndex(String),

    #[error("Required flag missing. {0}")]
    MissingFlag(String),

    #[error("Provided path contains no files.")]
    NoFilesError(),

    #[error("Only one file provided. More than one is required.")]
    SingleFileError(),

    #[error("Too many files provided. Only one is required for flag. {0}")]
    MultiFileError(String),

    #[error("File exists. {0}")]
    FileExistsError(String),

    #[error("Directory not supported by provided flags. {0}")]
    DirError(String),

    #[error("Conflicting flags. {0}")]
    ConflictingFlagsError(String),

    #[error("An internal logic error occurred: {0}")]
    InternalError(String),

    #[error("Library error: {0}")]
    SpriteShrinkError(#[from] SpriteShrinkError),

    #[error("Range parse error {0}")]
    RangeParseError(#[from] RangeError),

    #[error("OS priority error {0}")]
    OSPriorityError(#[from] thread_priority::Error),

    #[error("Hash bit length error {0}")]
    InvalidHashBitLength(String),

    #[error("Confy config error {0}")]
    ConfigError(#[from] confy::ConfyError),

    #[error("Confy config error {0}")]
    ClapError(#[from] clap::Error),

    #[error("RedB Table error {0}")]
    RedbTableError(#[from] redb::TableError),

    #[error("RedB Storage error {0}")]
    RedbStorageError(#[from] redb::StorageError),

    #[error("Key not found {0}")]
    KeyNotFound(String),
    
    #[error("Chunking Error {0}")]
    FileChunkingError(#[from] fastcdc::v2020::Error),

    #[error("Data integrity check failed: {0}. The archive file may be corrupt.")]
    DataIntegrityError(String),

    #[error("Logging Error {0}")]
    LoggingError(#[from] tracing::dispatcher::SetGlobalDefaultError),

    #[error("Logging Subscriber Error {0}")]
    LoggingSubscriberError(#[from] tracing_subscriber::util::TryInitError),
}

/// This function sets up a dual-layered logging system using the `tracing`
/// crate. It is responsible for:
///
/// 1.  **Console Logging**: A layer that writes formatted log messages to
///     standard output. The verbosity of this layer is controlled by the
///     `verbose` and `quiet` flags.
/// 2.  **File Logging**: A layer that writes logs in JSON format to a
///     rolling daily log file (`debug.log`). This file is stored in a
///     platform-appropriate local data directory. This layer is configured
///     to only capture messages at the `ERROR` level and above.
///
/// The file logger uses a non-blocking writer to minimize performance
/// impact. The returned `WorkerGuard` must be kept in scope for the
/// duration of the application's life to ensure that all log messages
/// are flushed to disk before the program exits.
///
/// # Arguments
///
/// * `verbose`: If `true`, the console log level is set to `DEBUG` for
///   detailed output.
/// * `quiet`: If `true`, all console output is suppressed by setting the
///   log level to `OFF`. If both `verbose` and `quiet` are false, the
///   level defaults to `INFO`.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(WorkerGuard)` containing the guard for the non-blocking file
///   writer. The caller is responsible for holding onto this guard.
/// - `Err(CliError)` if the logging system cannot be initialized.
///
/// # Errors
///
/// This function will return an error if another global logging subscriber
/// has already been set, or if there is an issue initializing the tracing
/// dispatcher.
pub fn initiate_logging(
    verbose: bool,
    quiet: bool,
    file_log_level: &str
) -> Result<Option<WorkerGuard>, CliError> {
    let proj_dirs = ProjectDirs::from(
        APPIDENTIFIER.qualifier, 
        APPIDENTIFIER.organization, 
        APPIDENTIFIER.application)
    .expect("Failed to find a valid project directory.");

    let mut log_dir = PathBuf::from(proj_dirs.data_local_dir());
    log_dir.push("logs");

    let console_level = if quiet {
        LevelFilter::OFF
    } else if verbose {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };

    if file_log_level != "off"{
        let console_layer = fmt::layer()
        .with_writer(std::io::stdout)
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_filter(console_level);

            let file_appender = rolling::daily(
                log_dir, 
                "debug.log");
            let (non_blocking_writer, guard) = tracing_appender::non_blocking(
                file_appender
            );

        let mut warning_flag: bool = false;

        let log_file_level = match file_log_level {
            "error" => LevelFilter::ERROR,
            "warning" => LevelFilter::WARN,
            "info" => LevelFilter::INFO,
            "debug" => LevelFilter::DEBUG,
            "off" => LevelFilter::OFF,
            _ => {
                warning_flag = true;
                LevelFilter::ERROR
            }
        };

        let file_layer = fmt::layer()
            .json()
            .with_writer(non_blocking_writer)
            .with_filter(log_file_level);

        registry()
            .with(file_layer)
            .with(console_layer)
            .try_init()?;

        if warning_flag {
            warn!("Invalid log level provided in config. \
            Defaulting level to ERROR.");
        } else{
            debug!("Log file enabled.");
        }

        Ok(Some(guard))
    } else {
        let console_layer = fmt::layer()
            .with_writer(std::io::stdout)
            .without_time()
            .with_level(false)
            .with_target(false)
            .with_filter(console_level);

        registry()
            .with(console_layer)
            .try_init()?;

        debug!("Log file disabled.");
        Ok(None)
    }

    
}