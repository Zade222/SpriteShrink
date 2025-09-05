//! The primary entry point and dispatcher for the command-line interface.
//!
//! This module orchestrates the entire lifecycle of the application. It is
//! responsible for parsing and validating command-line arguments,
//! organizing input paths, and then dispatching execution to the
//! appropriate operational mode—be it compression, extraction, or
//! metadata retrieval. It serves as the central hub that connects all
//! other components of the CLI application.

use clap::{CommandFactory, FromArgMatches};
use tracing::{
    debug,
    error,
    warn,
};

mod modes;
use modes::{run_compression, run_extraction, run_info};

mod archive_parser;
use archive_parser::{get_max_rom_index};

mod arg_handling;
use arg_handling::{merge_config_and_args, validate_args, Args};

mod cli_types;

mod db_transactions;

mod error_handling;
use error_handling::{CliError,
    initiate_logging};
mod storage_io;
use crate::{cli_types::SpriteShrinkConfig, storage_io::{
    cleanup_old_logs, files_from_dirs, load_config, organize_paths
}};

#[cfg(feature = "dhat-heap")]
use dhat::Profiler;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Executes the main application logic based on parsed arguments.
///
/// This function acts as the central dispatcher for the application.
/// It takes the validated command-line arguments and determines which
/// primary operation to perform: compression, extraction, or metadata
/// retrieval. It also handles the initial organization of input paths,
/// distinguishing between files and directories.
///
/// # Arguments
///
/// * `args`: A reference to the `Args` struct, which contains all the
///   user-provided settings and flags.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the selected operation completes successfully.
/// - `Err(CliError)` if any part of the process fails, such as
///   invalid path handling or an error within a specific run mode.
fn run(args: &Args) -> Result<(), CliError> {
    let (mut file_paths, dir_paths) = organize_paths(&args.input)?;
    
    if !dir_paths.is_empty() {
        file_paths.extend(files_from_dirs(&dir_paths)?);
    }

    match (args.extract.as_ref(), args.metadata){
        (Some(extract_str), false) => {
            if file_paths.len() > 1 {
                return Err(CliError::MultiFileError(
                    "Only one file is supported for extraction.".to_string(),
                ));
            }
            let file_path = file_paths.first().ok_or_else(
                CliError::NoFilesError
            )?;
            let indices: Vec<u8> = range_parser::parse::<u8>(
                extract_str
            )?;
            
            if let Some(max_arg_index) = indices.iter().max().cloned() {
                let file_max_index: u8 = get_max_rom_index(file_path)?;

                if file_max_index < max_arg_index {
                    return Err(CliError::InvalidRomIndex(format!(
                    "One or more ROM indexes are invalid. The requested index \
                        {max_arg_index} is out of the valid range \
                        (1-{file_max_index})." 
                    )));
                }
            }

            let output_path = args.output.as_ref().ok_or_else(|| {
                CliError::MissingFlag("Output path (-o, --output) is required \
                    for extraction.".to_string())
            })?;

            debug!("Mode: Extract");
            run_extraction(
                &file_paths[0], 
                output_path, 
                &indices, 
                args
            )?;
        }
        
        (None, true) =>{
            match file_paths.len() {
                1 => {
                    debug!("Mode: Info");
                    run_info(&file_paths[0], args.json)?
                },
                0 => return Err(CliError::NoFilesError()),
                _ => {
                    if !dir_paths.is_empty() {
                        return Err(CliError::DirError(
                            "Metadata flag does not support ingesting \
                            directories.".to_string(),
                        ));
                    }
                    return Err(CliError::MultiFileError(
                        "Only one file is supported with metadata flag."
                            .to_string(),
                    ));
                }
            }
        }
        (None, false) => {
            debug!("Mode: Compress");
            let hash_bit_length = args.hash_bit_length.unwrap_or(64);

            match hash_bit_length{
                64 => run_compression::<u64>(file_paths, args, &1)?,
                128 => run_compression::<u128>(file_paths, args, &2)?,
                _ => return Err(CliError::InvalidHashBitLength(
                    "Must be 64 or 128.".to_string(),
                )),
            }
        }

        (Some(_), true) => {
            /*This case is already handled by validate_args, but we can be 
            explicit.*/
            return Err(CliError::ConflictingFlagsError(
                "Metadata and extraction modes cannot be used simultaneously."
                    .to_string(),
            ));
        }
    }

    Ok(())
}

/// The main entry point for the command-line application.
///
/// This function initializes the application by parsing and validating
/// the command-line arguments provided by the user. Once the
/// arguments are confirmed to be valid, it calls the main `run`
/// function to execute the requested operation, such as compression,
/// extraction, or metadata retrieval.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the entire application runs to completion without
///   any unrecoverable errors.
/// - `Err(CliError)` if a critical error occurs during argument
///   parsing, validation, or the main execution flow.
fn main() -> Result<(), CliError>{
    #[cfg(feature = "dhat-heap")]
    let _dhat = Profiler::new_heap();

    //Receive and validate user specified arguments/flags where applicable.
    let matches = Args::command().get_matches();
    let args = Args::from_arg_matches(&matches)?;

    //Load config from disk if the ignore_config flag isn't provided.
    let file_cfg = if args.ignore_config {
        debug!("Ignoring config.");
        SpriteShrinkConfig::default()
    } else {
        match load_config() {
            Ok(cfg) => {
                debug!("Config loaded.");
                cfg
            },
            Err(e) => {
                error!(error = %e, "Failed to load application configuration.\
                    Exiting."
                );

                std::process::exit(1);
            }
        }
    };

    /*Determine which options supersede others between the user provided 
    flags and the on disk config.*/
    let final_args = merge_config_and_args(&file_cfg, args, &matches);

    //Cleanup old log files according to retention policy.
    if let Err(e) = cleanup_old_logs(file_cfg.log_retention_days){
        warn!("Failed to clean up old log files. Error: {}", e);
    }

    //Initiate logging
    let _guard = initiate_logging(
        final_args.verbose, 
        final_args.quiet
    )?;

    //Further validate and check for conflicting options.
    match validate_args(&final_args, &matches) {
        Ok(_) => {
            debug!("Command-line arguments are valid.");
        },
        Err(e) => {
            error!(error = %e, "Invalid command-line arguments. \
                Please verify input."
            );
            eprintln!("\nError: {}", e);

            std::process::exit(1);
        }
    }
    
    //Begin application logic.
    match run(&final_args){
        Ok(_) => {
            debug!("Application completed task successfully.");
        },
        Err(e) => {
            error!("Application encountered an error and has exited.\
                Error: {}", e
            );
        }
    };

    Ok(())
}