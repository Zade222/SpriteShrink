//! Declares the error handling types for the sprite-shrink library.
//!
//! This module defines the custom `LibError` enum, which encapsulates
//! all possible failure conditions that can occur during the core
//! archiving and compression processes. By centralizing error
//! definitions here, the library provides a clear and consistent
//! error-handling mechanism.
use std::io;
use std::string::FromUtf8Error;

use thiserror::Error;

/// Represents all possible errors that can occur within the library.
///
/// This enum centralizes failure conditions that arise during the core
/// processes of compression, archiving, and data verification. It is
/// designed to be used by applications that consume this library,
/// providing clear, specific error variants for robust handling.
#[derive(Error, Debug)]
pub enum LibError {
    #[error("I/O Error")]
    Io(#[from] io::Error),
    
    #[error("File header is malformed. {0}")]
    InvalidHeaderError(String),

    #[error("Failed to convert data to UTF-8")]
    FromUtf8(#[from] FromUtf8Error),

    #[error("Read file is of a newer version than what this library supports.")]
    FileVersionError(),

    #[error("Failed to decode file manifest: {0}")]
    ManifestDecodeError(String),

    #[error("Failed to decode chunk index: {0}")]
    IndexDecodeError(String),

    #[error("Failed to encode file manifest: {0}")]
    ManifestEncodeError(String),

    #[error("Failed to encode file manifest: {0}")]
    IndexEncodeError(String),

    #[error("An internal logic error occurred: {0}")]
    InternalLibError(String),

    #[error("Verification failed for file:  {0}")]
    HashMismatchError(String),

    #[error("Compression failed: {0}")]
    CompressionError(String),

    #[error("Dictionary creation failed: {0}")]
    DictionaryError(String),

    #[error("Thread pool creation failed: {0}")]
    ThreadPoolError(String),

    #[error("File Verification failed: {0}")]
    VerificationError(String),

    #[error("Chunk missing: {0}")]
    SerializationMissingChunkError(String),

    #[error("Seek request is outside the file bounds: {0}")]
    SeekOutOfBounds(String),
}