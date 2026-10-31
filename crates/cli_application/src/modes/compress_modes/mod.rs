use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use sprite_shrink::{CompressionResult, HashType};
use tracing::info;

use crate::arg_handling::Args;
use crate::error_handling::CliError;

use default::default_compression;
use optical::compress_optical_mode;

mod default;
mod optical;

/// Tells the user when the archive was built without a compression
/// dictionary. The library skips the dictionary when it can't train one,
/// which happens when the input has too little distinct data, such as a
/// single small ROM.
fn note_if_no_dictionary(comp_data: &CompressionResult) {
    if comp_data.dictionary.is_empty() {
        info!(
            "Note: the input has too little distinct data to train a \
            compression dictionary, so the archive was built without one."
        );
    }
}


pub fn run_default_compression(
    file_paths: Vec<PathBuf>,
    args: &Args,
    hash_bit_length: &u32,
    running: Arc<AtomicBool>,
) -> Result<(), CliError> {
    match hash_bit_length {
        64 => default_compression::<u64>(
            file_paths,
            args,
            HashType::Xxh3_64,
            running
        ),
        128 => default_compression::<u128>(
            file_paths,
            args,
            HashType::Xxh3_128,
            running
        ),
        _ => Err(CliError::InvalidHashBitLength(
            "Must be 64 or 128.".to_string(),
        )),
    }
}


pub fn run_optical_compression(
    file_paths: Vec<PathBuf>,
    args: &Args,
    hash_bit_length: &u32,
    running: Arc<AtomicBool>,
) -> Result<(), CliError> {
    match hash_bit_length {
        64 => compress_optical_mode::<u64>(
            file_paths,
            args,
            HashType::Xxh3_64,
            running
        ),
        128 => compress_optical_mode::<u128>(
            file_paths,
            args,
            HashType::Xxh3_128,
            running
        ),
        _ => Err(CliError::InvalidHashBitLength(
            "Must be 64 or 128.".to_string(),
        )),
    }
}
