//! Declares the error handling types for the sprite-shrink library.
//!
//! This module defines the custom `LibError` enum, which encapsulates
//! all possible failure conditions that can occur during the core
//! archiving and compression processes. By centralizing error
//! definitions here, the library provides a clear and consistent
//! error-handling mechanism.
use std::io;

use thiserror::Error;

use crate::{
    archive::ArchiveError,
    parsing::ParsingError,
    processing::ProcessingError,
    serialization::SerializationError
};

/// Represents all possible errors that can occur within the library.
///
/// This enum centralizes failure conditions that arise during the core
/// processes of compression, archiving, and data verification. It is
/// designed to be used by applications that consume this library,
/// providing clear, specific error variants and surface interior errors
/// for robust handling.
#[derive(Error, Debug)]
pub enum SpriteShrinkError {
    #[error("An error occurred during archive processing")]
    Archive(ArchiveError),

    #[error("An error occurred during file parsing")]
    Parsing(#[from] ParsingError),

    #[error("An error occurred during data processing")]
    Processing(ProcessingError),

    #[error("An error occurred during serialization")]
    Serialization(SerializationError),

    #[error("I/O Error")]
    Io(#[from] io::Error),

    #[error("An external error occurred: {0}")]
    External(Box<dyn std::error::Error + Send + Sync>),

    #[error("Operation cancelled by user")]
    Cancelled,
}

/// Converts an `ArchiveError` into a `SpriteShrinkError`.
///
/// This implementation allows for the ergonomic propagation of errors from
/// the archive module into the library's top-level error type. It wraps most
/// archive-specific errors within the `SpriteShrinkError::Archive` variant.
///
/// A special case is handled for `ArchiveError::Cancelled` to ensure that a
/// user-initiated cancellation is preserved as the top-level
/// `SpriteShrinkError::Cancelled` variant, allowing for specific cancellation
/// logic in the calling application.
impl From<ArchiveError> for SpriteShrinkError {
    fn from(err: ArchiveError) -> Self {
        match err {
            ArchiveError::Cancelled => SpriteShrinkError::Cancelled,
            _ => SpriteShrinkError::Archive(err),
        }
    }
}

/// Converts a `ProcessingError` into a `SpriteShrinkError`.
///
/// This implementation allows for the ergonomic propagation of errors from
/// the data processing module into the library's top-level error type. It
/// wraps most processing-specific errors within the
/// `SpriteShrinkError::Processing` variant.
///
/// A special case is handled for `ProcessingError::Cancelled` to ensure that a
/// user-initiated cancellation is preserved as the top-level
/// `SpriteShrinkError::Cancelled` variant, allowing for specific cancellation
/// logic in the calling application.
impl From<ProcessingError> for SpriteShrinkError {
    fn from(err: ProcessingError) -> Self {
        match err {
            ProcessingError::Cancelled => SpriteShrinkError::Cancelled,
            _ => SpriteShrinkError::Processing(err),
        }
    }
}

/// Converts a `SerializationError` into a `SpriteShrinkError`.
///
/// This implementation allows for the ergonomic propagation of errors from
/// the data serialization module into the library's top-level error type. It
/// wraps most serialization-specific errors within the
/// `SpriteShrinkError::Serialization` variant.
///
/// A special case is handled for `ProcessingError::Cancelled` to ensure that a
/// user-initiated cancellation is preserved as the top-level
/// `SpriteShrinkError::Cancelled` variant, allowing for specific cancellation
/// logic in the calling application.
impl From<SerializationError> for SpriteShrinkError {
    fn from(err: SerializationError) -> Self {
        match err {
            SerializationError::Cancelled => SpriteShrinkError::Cancelled,
            _ => SpriteShrinkError::Serialization(err),
        }
    }
}

/// Provides a common interface for error types to indicate cancellation.
///
/// This trait is implemented by error enums throughout the crate to provide a
/// consistent way to check if an error represents a user-initiated
/// cancellation (e.g., via a Ctrl+C signal). It allows higher-level logic
/// to handle cancellation gracefully without needing to match against specific
/// error variants from different modules.
pub trait IsCancelled {
    /// Returns `true` if the error represents a user-initiated cancellation.
    fn is_cancelled(&self) -> bool;
}

/// Checks if the error represents a user-initiated cancellation.
///
/// This implementation of the `IsCancelled` trait allows the application to
/// distinguish between regular errors and a cancellation event (e.g., the user
/// pressing Ctrl+C). It is used to ensure that the application can shut down
/// gracefully without treating a cancellation as a critical failure.
///
/// # Returns
///
/// * `true` if the error is `SpriteShrinkError::Cancelled`.
/// * `false` for all other `SpriteShrinkError` variants.
impl IsCancelled for SpriteShrinkError {
    fn is_cancelled(&self) -> bool {
        matches!(self, SpriteShrinkError::Cancelled)
    }
}
