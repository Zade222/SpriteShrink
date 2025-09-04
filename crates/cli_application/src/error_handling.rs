//! Declares the error handling types for the command-line application.
//!
//! This module defines the custom `CliError` enum, which encapsulates
//! all possible failure conditions that can occur within the CLI
//! application. By centralizing error definitions here, the tool
//! provides a clear and consistent error-handling mechanism for all
//! of its command-line operations.

use std::io;

use sprite_shrink::SpriteShrinkError;
use range_parser::RangeError;
use thiserror::Error;

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
    HashBitLengthError(String),

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
}