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
/// providing clear, specific error variants for robust handling.
#[derive(Error, Debug)]
pub enum SpriteShrinkError {
    #[error("An error occurred during archive processing")]
    Archive(#[from] ArchiveError),

    #[error("An error occurred during file parsing")]
    Parsing(#[from] ParsingError),

    #[error("An error occurred during data processing")]
    Processing(#[from] ProcessingError),

    #[error("An error occurred during serialization")]
    Serialization(#[from] SerializationError),

    #[error("I/O Error")] 
    Io(#[from] io::Error), 
}