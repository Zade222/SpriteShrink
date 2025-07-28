//! The primary entry point and dispatcher for the command-line interface.
//!
//! This module orchestrates the entire lifecycle of the application. It is
//! responsible for parsing and validating command-line arguments,
//! organizing input paths, and then dispatching execution to the
//! appropriate operational mode—be it compression, extraction, or
//! metadata retrieval. It serves as the central hub that connects all
//! other components of the CLI application.

use clap::Parser;

mod modes;
use modes::{run_compression, run_extraction, run_info};

mod archive_parser;
use archive_parser::{get_max_rom_index};

mod arg_handling;
use arg_handling::{validate_args, Args};

mod error_handling;
use error_handling::CliError;
    
mod storage_io;
use crate::storage_io::{files_from_dirs, organize_paths};

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
            let file_path = file_paths.first().ok_or_else(CliError::NoFilesError)?;
            let indices: Vec<u8> = range_parser::parse::<u8>(extract_str)?;
            
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
            CliError::MissingFlag("Output path (-o, --output) is required for extraction.".to_string())
            })?;

            run_extraction(&file_paths[0], output_path, &indices, args)?;
        }
        
        (None, true) =>{
            match file_paths.len() {
                1 => run_info(&file_paths[0])?,
                0 => return Err(CliError::NoFilesError()),
                _ => {
                    if !dir_paths.is_empty() {
                        return Err(CliError::DirError(
                            "Metadata flag does not support ingesting directories.".to_string(),
                        ));
                    }
                    return Err(CliError::MultiFileError(
                        "Only one file is supported with metadata flag.".to_string(),
                    ));
                }
            }
        }
        (None, false) => {
            run_compression(file_paths, args)?;
        }

        (Some(_), true) => {
            //This case is already handled by validate_args, but we can be explicit.
            return Err(CliError::ConflictingFlagsError(
                "Metadata and extraction modes cannot be used simultaneously.".to_string(),
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
    //Initiate logging
    /*Line for initiating logging needed. Must be started as early as possible
    in application logic.*/
    
    //Receive and validate user specified arguments/flags where applicable.
    let args = Args::parse();
    validate_args(&args)?;
    
    //Begin application logic.
    run(&args)?;

    Ok(())
}
