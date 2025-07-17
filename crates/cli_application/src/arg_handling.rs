//! Manages command-line argument parsing and validation.
//!
//! This module defines the command-line interface for the application
//! using the `clap` crate. It includes the `Args` struct, which specifies
//! all available options and flags, and the validation logic to ensure
//! that user input is sensible and complete before execution.

use std::path::{PathBuf};

use bytesize::ByteSize;
use clap::Parser;

use crate::error_handling::CliError;
use crate::storage_io::dir_contains_files;

const CUSTOM_HELP_TEMPLATE: &str = "\
{before-help}{name} {version}
{author-with-newline}{about-with-newline}
{all-args}{after-help}
";

/// Defines the command-line arguments for the application.
///
/// This struct uses `clap` to parse and manage all options, flags, and
/// inputs provided by the user. It is organized into logical groups for
/// primary operations, tuning, and behavior control to provide a clear
/// and user-friendly command-line interface.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None, 
    help_template = CUSTOM_HELP_TEMPLATE)]
pub struct Args {
    //Primary group of arguments.
    
    ///Path to single ROM file or directory of ROMs for input. 
    ///Can be specified more than once.
    #[arg(short, long, help_heading = "Primary Options")]
    pub input: Vec<PathBuf>,

    ///Path for the output archive or extracted files.
    #[arg(short, long, help_heading = "Primary Options")]
    pub output: Option<PathBuf>,

    ///Activates metadata retrieval mode, used in combination with -i.
    #[arg(short, long, help_heading = "Primary Options")]
    pub metadata: bool,

    ///Specifies the index number(s) of the ROM(s) to be extracted,
    ///as listed by the -m flag.
    #[arg(short, long, help_heading = "Primary Options")]
    pub extract: Option<String>,

    //Tuning paramters

    ///Sets the compression algorithm.
    ///Choices: zstd, lzma, deflate. Defaults to zstd.
    #[arg(short, long, help_heading = "Tuning Parameters", default_value_t = 
    String::from("zstd"))]
    pub compression: String,

    ///Sets the numerical compression level for the chosen algorithm.
    #[arg(long, help_heading = "Tuning Parameters", default_value_t = 19)]
    pub compression_level: u8,

    ///Determines the window size for the hashing algorithm.
    ///(e.g., "256K", "4KB", "64KiB")
    #[arg(short, long, help_heading = "Tuning Parameters", default_value_t = ByteSize::b(2*1024))]
    pub window: ByteSize,

    ///Determines the dictionary size for the compression algorithm.
    ///(e.g., "256K", "4KB", "64KiB")
    #[arg(short, long, help_heading = "Tuning Parameters", default_value_t = ByteSize::b(16*1024))]
    pub dictionary: ByteSize,

    ///Autotune both the window size and dictionary size. Will find the 
    /// reasonably optimal size for each. This uses a somewhat inefficient 
    /// algorithm and can take time but will lead to a smaller file size
    /// more easily. If vlaues for either the window or dictionary are 
    /// provided they will be used instead of autotuning.
    #[arg(short, long, help_heading = "Tuning Parameters", 
    default_value_t = false)]
    pub auto_tune: bool,

    ///Sets the maximum number of worker threads to use.
    ///Defaults to all available logical cores.
    #[arg(short, long, help_heading = "Tuning Parameters")]
    pub threads: Option<usize>,

    ///Forces low-memory mode by processing files sequentially and
    ///limiting worker threads to 4 for compression.
    #[arg(long, help_heading = "Tuning Parameters", default_value_t = false)]
    pub low_memory: bool,

    //Behavior and Output Control

    ///Forces the system to overwrite the output file if it exists.
    #[arg(short, long, help_heading = "Behavior and Output Control", 
    default_value_t = false)]
    pub force: bool,

    ///Activates verbose output for detailed diagnostic information.
    #[arg(short, long, help_heading = "Behavior and Output Control", 
    default_value_t = false)]
    pub verbose: bool,

    ///Activates quiet mode, suppressing all non-essential output.
    #[arg(short, long, help_heading = "Behavior and Output Control", 
    default_value_t = false)]
    pub quiet: bool,
}

/// Validates the command-line arguments provided by the user.
///
/// This function checks for logical conflicts, missing required options,
/// and improper usage of flags based on the selected operation mode. Its
/// purpose is to ensure the application has a valid set of arguments
/// before proceeding with any work.
///
/// # Arguments
///
/// * `args`: A reference to the `Args` struct, which contains the
///   parsed command-line arguments.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if all argument combinations are valid.
/// - `Err(CliError)` if any validation check fails.
///
/// # Errors
///
/// Returns an error in the following cases:
/// - Using `--metadata` and `--extract` together.
/// - No input path (`-i`) is provided.
/// - No files are found for an operation that requires them.
/// - Attempting extraction on a single file without a ROM index (`-r`).
/// - Attempting extraction with more than one input file.
/// - Providing an invalid format for the ROM index.
/// - Missing an output path (`-o`) for extraction or compression.
/// - The output path for compression is a directory.
/// - Initiating compression with only one input file.
pub fn validate_args(args: &Args) -> Result<(), CliError> {
    /*Will need to come back and work on validation logic. Due to modular
    intent of adding compression algorithms I want to assess how to best 
    accomplish this with the validation.*/


    /*A basic validation case is as follows:
     match args.algorithm.as_str() {
        "zip" => println!("Executing ZIP compression logic..."),
        "gzip" => println!("Executing GZIP compression logic..."),
        "brotli" => println!("Executing Brotli compression logic..."),
        _ => unreachable!(),
    }*/

    /*The following check(s) are for Primary Operations */
    
    if args.metadata && args.extract.is_some() {
        return Err(CliError::ConflictingFlagsError(
            "Metadata and extraction modes cannot be used simultaneously."
            .to_string()));
    }

    if args.input.is_empty() {
        return Err(CliError::MissingFlag(
            "At least one input path (-i) is required.".to_string()
        ));
    }

    let has_files_to_process = args.input.iter().any(|path| {
        if !path.exists() {
            return true;
        }
        path.is_file() || (path.is_dir() && dir_contains_files(path).unwrap_or(false))
    });

    if !has_files_to_process && !args.metadata && args.extract.is_none() {
         return Err(CliError::NoFilesError());
    }

    //Extraction Mode
    if args.input.len() == 1{
        if args.extract.is_none() && !args.metadata && !args.input[0].is_dir(){
            return Err(CliError::MissingFlag(
                "A ROM index selection (-r) is required for extraction.".to_string(),
            ));
        }
    }   

    //Extraction Mode ROM Index Validation 
    if let Some(rom_range) = &args.extract {
        if args.input.len() > 1 {
            return Err(CliError::MultiFileError(
                "Only one input file is allowed for extraction.".to_string(),
            ));
        }
        if range_parser::parse::<u32>(rom_range).is_err() {
            return Err(CliError::InvalidFormRomRange(
                "The ROM index range format is invalid. Use a comma-separated list or a range (e.g., 1,3,5-7).".to_string(),
            ));
        }
        if args.output.is_none() {
            return Err(CliError::MissingFlag(
                "Output path (-o, --output) is required for extraction.".to_string(),
            ));
        }
    }

    //Compression Mode
    if !args.metadata && args.extract.is_none() {
        if args.output.is_none() {
            return Err(CliError::MissingFlag(
                "Output path (-o) is required when in compression mode.".to_string(),
            ));
        }

        if let Some(output_path) = &args.output {
             if output_path.is_dir() {
                return Err(CliError::InvalidPath(
                    "The output path must be a file, not a directory.".to_string()
                ));
            }
        }

        if args.input.len() == 1 && args.input[0].is_file() {
            return Err(CliError::SingleFileError());
        }
    }

    Ok(())
}