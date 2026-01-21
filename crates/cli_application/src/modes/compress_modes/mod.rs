use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use crate::arg_handling::Args;
use crate::error_handling::CliError;

use default::default_compression;
use optical::compress_optical_mode;

mod default;
mod optical;


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
            &1,
            running
        ),
        128 => default_compression::<u128>(
            file_paths,
            args,
            &2,
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
            &1,
            running
        ),
        128 => compress_optical_mode::<u128>(
            file_paths,
            args,
            &2,
            running
        ),
        _ => Err(CliError::InvalidHashBitLength(
            "Must be 64 or 128.".to_string(),
        )),
    }
}
