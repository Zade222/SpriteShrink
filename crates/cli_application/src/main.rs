//! The primary entry point and dispatcher for the command-line interface.
//!
//! This module orchestrates the entire lifecycle of the application. It is
//! responsible for parsing and validating command-line arguments,
//! organizing input paths, and then dispatching execution to the
//! appropriate operational mode—be it compression, extraction, or
//! metadata retrieval. It serves as the central hub that connects all
//! other components of the CLI application.

use std::{
    fs::{self, create_dir_all},
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc, atomic::{AtomicBool, Ordering}
    },
};

use clap::{CommandFactory, FromArgMatches};
use ctrlc::set_handler;
use sprite_shrink::IsCancelled;
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
use archive_parser::{get_file_header, parse_rom_indices};
mod arg_handling;
use arg_handling::{merge_config_and_args, validate_args, Args};
mod auto_tune;
mod cli_types;
mod error_handling;
use error_handling::{
    CliError, initiate_logging, offset_to_line_col};
mod storage_io;
use crate::{
    cli_types::SpriteShrinkConfig,
    modes::compress_modes::run_optical_compression,
    storage_io::{
    entry_name, cleanup_old_logs, disc_title, duplicate_names,
    files_from_dirs, load_config, organize_paths
    }
};
mod utils;

#[cfg(feature = "dhat-heap")]
use dhat::Profiler;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Exit status for a run the user cancelled: 128 plus SIGINT's number, which
/// is what shells report for a command stopped with Ctrl+C.
const EXIT_CANCELLED: u8 = 130;

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

            let file_count = get_file_header(file_path)?.file_count;
            let indices = parse_rom_indices(extract_str, file_count)?;

            let output_path = args.output.as_ref().ok_or_else(|| {
                CliError::MissingFlag("Output path (-o, --output) is required \
                    for extraction.".to_string())
            })?;

            debug!("Mode: Extract");
            run_extraction(
                file_path,
                output_path,
                &indices,
                args,
                running,
            )?;
        }

        (None, true) =>{
            match file_paths.as_slice() {
                [file_path] => {
                    debug!("Mode: Info");
                    run_info(file_path, args.metadata || args.json_output)?;
                },
                [] => return Err(CliError::NoFilesFound()),
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

            /*Verify if the list of files paths is empty, throw error if true. */
            if file_paths.is_empty() {
                return Err(CliError::NoFilesFound());
            }

            check_unique_names(&file_paths, &args.mode)?;

            let out_dir = args.output
                .as_ref()
                .unwrap()
                .parent()
                .unwrap();

            if !out_dir.exists() && !args.force{
                return Err(CliError::InvalidOutputPath())
            } else {
                create_dir_all(out_dir)?;
            }

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

/// Checks that no two inputs would be stored under the same name: the file
/// name in default mode, the CUE file's stem in optical mode. Each archive
/// entry is keyed by name, so all but one would be lost.
///
/// # Errors
///
/// Returns `CliError::DuplicateNames`, listing each shared name and the
/// paths that share it, one name per line.
fn check_unique_names(
    file_paths: &[PathBuf],
    mode: &str,
) -> Result<(), CliError> {
    let name_of = if mode == "optical" { disc_title } else { entry_name };
    let duplicates = duplicate_names(file_paths, name_of);
    if duplicates.is_empty() {
        return Ok(());
    }

    let lines: Vec<String> = duplicates
        .iter()
        .map(|(name, paths)| {
            let paths: Vec<String> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            format!("  {name}: {}", paths.join(", "))
        })
        .collect();
    Err(CliError::DuplicateNames(lines.join("\n")))
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

/// Reports how `run` ended and picks the process's exit status: success,
/// [`EXIT_CANCELLED`] for a cancellation (including one wrapped in a library
/// error), or failure for any other error.
fn finish(result: Result<(), CliError>) -> ExitCode {
    match result {
        Ok(()) => {
            debug!("Application completed task successfully.");
            ExitCode::SUCCESS
        }
        Err(e) if e.is_cancelled() => {
            warn!("Operation cancelled by user.");
            ExitCode::from(EXIT_CANCELLED)
        }
        Err(e) => {
            error!("Application encountered an error and has exited. \
                Error: {}", e
            );
            eprintln!("\nError: {e}");
            ExitCode::FAILURE
        }
    }
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
/// - `Ok(ExitCode)` once the selected operation has run, with the status
///   [`finish`] picked: non-zero if the operation failed or was cancelled.
/// - `Err(CliError)` if argument parsing, loading the config or starting
///   logging fails. Rust prints the error and exits with status 1.
fn main() -> Result<ExitCode, CliError>{
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
    Ok(finish(run(&final_args, running)))
}

#[cfg(test)]
mod tests {
    use sprite_shrink::SpriteShrinkError;
    use sprite_shrink_cd::SpriteShrinkCDError;

    use super::*;

    #[test]
    fn a_finished_run_exits_successfully() {
        assert_eq!(finish(Ok(())), ExitCode::SUCCESS);
    }

    #[test]
    fn a_cancelled_run_exits_with_the_cancel_status() {
        let cancelled = [
            CliError::Cancelled,
            CliError::from(SpriteShrinkError::Cancelled),
            CliError::from(SpriteShrinkCDError::from(
                SpriteShrinkError::Cancelled,
            )),
        ];
        for err in cancelled {
            let shown = err.to_string();
            assert_eq!(
                finish(Err(err)),
                ExitCode::from(EXIT_CANCELLED),
                "{shown}"
            );
        }
    }

    #[test]
    fn a_failed_run_exits_with_failure() {
        let failed = [
            CliError::NoFilesFound(),
            CliError::from(SpriteShrinkError::External("read".into())),
            CliError::from(SpriteShrinkCDError::from(
                SpriteShrinkError::External("read".into()),
            )),
        ];
        for err in failed {
            let shown = err.to_string();
            assert_eq!(finish(Err(err)), ExitCode::FAILURE, "{shown}");
        }
    }
}
