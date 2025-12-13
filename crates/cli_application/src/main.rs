//! The primary entry point and dispatcher for the command-line interface.
//!
//! This module orchestrates the entire lifecycle of the application. It is
//! responsible for parsing and validating command-line arguments,
//! organizing input paths, and then dispatching execution to the
//! appropriate operational mode—be it compression, extraction, or
//! metadata retrieval. It serves as the central hub that connects all
//! other components of the CLI application.

use std::{
    fs,
    sync::{
        Arc, atomic::{AtomicBool, Ordering}
    },
};

use clap::{CommandFactory, FromArgMatches};
use ctrlc::set_handler;
use tracing::{
    debug,
    error,
    warn,
};

mod modes;
use modes::{
    run_extraction, run_info,
    compress_modes::{
        run_default_compression
    }
};

mod archive_parser;
use archive_parser::{get_max_rom_index};

mod arg_handling;
use arg_handling::{merge_config_and_args, validate_args, Args};

mod auto_tune;

mod cli_types;

mod error_handling;
use error_handling::{
    CliError, initiate_logging, offset_to_line_col};
mod storage_io;
use crate::{cli_types::SpriteShrinkConfig, modes::compress_modes::run_optical_compression, storage_io::{
    cleanup_old_logs, files_from_dirs, load_config, organize_paths
    }
};

#[cfg(feature = "dhat-heap")]
use dhat::Profiler;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Dispatches the primary application logic based on parsed arguments.
///
/// This function serves as the central controller for the application. It
/// takes the validated command-line arguments and determines which
/// operational mode to execute: compression, extraction, or metadata
/// retrieval. It is also responsible for the initial processing of input
/// paths, expanding directories into a list of files to be processed.
///
/// # Arguments
///
/// * `args`: A reference to the `Args` struct, which contains all the
///   user-provided settings and flags that dictate the application's behavior.
/// * `running`: An `Arc<AtomicBool>` that is used as a cancellation token.
///   Long-running operations, like compression, will monitor this flag to
///   allow for a graceful shutdown if the user issues a termination signal
///   (e.g., Ctrl+C).
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the selected operation completes successfully.
/// - `Err(CliError)` if any part of the process fails, such as invalid path
///   handling, an error within a specific run mode, or invalid argument
///   combinations.
///
/// # Errors
///
/// This function can return an error in several cases, including:
/// - An invalid combination of arguments is provided (e.g., requesting
///   extraction on more than one archive).
/// - An I/O error occurs while collecting files from input directories.
/// - A required argument for a specific mode is missing (e.g., no output
///   path for extraction).a
/// - Any error propagated from the underlying `run_compression`,
///   `run_extraction`, or `run_info` functions.
fn run(
    args: &Args,
    running: Arc<AtomicBool>
) -> Result<(), CliError> {
    let (mut file_paths, dir_paths) = organize_paths(&args.input)?;

    if !dir_paths.is_empty() {
        file_paths.extend(files_from_dirs(&dir_paths)?);
    }

    match (args.extract.as_ref(), (args.list || args.metadata)) {
        (Some(extract_str), false) => {
            if file_paths.len() > 1 {
                return Err(CliError::TooManyFiles(
                    "Only one input file is supported for extraction."
                    .to_string(),
                ));
            }
            let file_path = file_paths.first().ok_or_else(
                CliError::NoFilesFound
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
                args,
                running,
            )?;
        }

        (None, true) =>{
            match file_paths.len() {
                1 => {
                    debug!("Mode: Info");
                    run_info(&file_paths[0], args.list, args.metadata)?
                },
                0 => return Err(CliError::NoFilesFound()),
                _ => {
                    if !dir_paths.is_empty() {
                        return Err(CliError::DirectoryNotSupported(
                            "Metadata flag does not support ingesting \
                            directories.".to_string(),
                        ));
                    }
                    return Err(CliError::TooManyFiles(
                        "Only one file is supported with metadata flag."
                            .to_string(),
                    ));
                }
            }
        }
        (None, false) => {
            debug!("Mode: Compress");
            let hash_bit_length = args.hash_bit_length.unwrap_or(64);

            match args.mode.as_str() {
                "optical" => {
                    run_optical_compression(
                        file_paths,
                        args,
                        &hash_bit_length,
                        running,
                    )?;
                }
                "default" =>{
                    run_default_compression(
                        file_paths,
                        args,
                        &hash_bit_length,
                        running,
                    )?;
                }
                _ =>  return Err(CliError::InvalidMode(args.mode.clone()))
            }
        }

        (Some(_), true) => {
            /*This case is already handled by validate_args.*/
            return Err(CliError::ConflictingArguments(
                "Metadata and extraction modes cannot be used simultaneously."
                    .to_string(),
            ));
        }
    }

    Ok(())
}

/// Sets up a handler to gracefully shut down the application on Ctrl+C.
///
/// This function registers a handler that listens for the Ctrl+C signal. When
/// the signal is received, it flips a shared atomic boolean flag from `true`
/// to `false`. This flag can be passed to long-running tasks, allowing them to
/// check for a cancellation request and terminate gracefully instead of
/// abruptly exiting.
///
/// # Returns
///
/// An `Arc<AtomicBool>` that serves as a shared "running" flag. It is
/// initially `true` and will be set to `false` when the user presses Ctrl+C.
fn setup_ctrlc_handler() -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    running
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


    let running = setup_ctrlc_handler();

    //Receive and validate user specified arguments/flags where applicable.
    let matches = Args::command().get_matches();
    let args = Args::from_arg_matches(&matches)?;

    //Load config from disk if the ignore_config flag isn't provided.
    let file_cfg = if args.ignore_config {
        debug!("Ignoring config.");
        SpriteShrinkConfig::default()
    } else {
        match load_config() {
            Ok(cfg) => cfg,
            Err(CliError::ConfigError(confy::ConfyError::BadTomlData(e))) =>{
                /*Due to the logger not being initialized yet, just return the
                error.*/
                eprintln!("There's an issue with your configuration file.");

                if let Ok(path) = confy::get_configuration_file_path(
                    "spriteshrink",
                    "spriteshrink-config"
                ) {
                    eprintln!("File: {}", path.display());

                    if let Some(span) = e.span() &&
                        let Ok(contents) = fs::read_to_string(&path) {
                            let (line, col) = offset_to_line_col(
                                &contents,
                                span.start
                            );

                            eprintln!("Error is on line {}, column {}:",
                                line, col
                            );

                            if let Some(line_content) = contents
                            .lines()
                            .nth(line - 1) {
                                eprint!("\n  \x1b[93m{}\x1b[0m\n", line_content);
                                eprintln!("  \x1b[91;1m{}^\x1b[0m", " "
                                    .repeat(col.saturating_sub(1))
                                );
                            }
                    }
                }
                std::process::exit(1);
            }
            Err(e) => {
                /*Due to the logger not being initialized yet, just return the
                error.*/
                return Err(e);
            }
        }
    };

    /*Determine which options supersede others between the user provided
    flags and the on disk config.*/
    let final_args = merge_config_and_args(&file_cfg, args, &matches);

    let log_level = if final_args.disable_logging ||
        file_cfg.log_level == "off" {
            "off".to_string()
    } else {
        file_cfg.log_level.to_string()
    };

    //Initiate logging
    let _guard = initiate_logging(
        final_args.quiet,
        &log_level
    )?;

    //Cleanup old log files according to retention policy.
    if let Err(e) = cleanup_old_logs(file_cfg.log_retention_days){
        warn!("Failed to clean up old log files. Error: {}", e);
    }

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
    match run(&final_args, running){
        Ok(_) => {
            debug!("Application completed task successfully.");
        },
        Err(CliError::Cancelled) => {
            warn!(
                "Operation cancelled by user."
            );
        }
        Err(e) => {
            error!("Application encountered an error and has exited.\
                Error: {}", e
            );
        }
    };

    Ok(())
}
