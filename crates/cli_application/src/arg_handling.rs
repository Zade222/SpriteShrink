//! Manages command-line argument parsing and validation.
//!
//! This module defines the command-line interface for the application
//! using the `clap` crate. It includes the `Args` struct, which specifies
//! all available options and flags, and the validation logic to ensure
//! that user input is sensible and complete before execution.

use std::{
    path::PathBuf,
    option::Option
};

use bytesize::ByteSize;
use clap::{ArgMatches, Parser};

use crate::error_handling::CliError;

use crate::storage_io::{
    SpriteShrinkConfig
};

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
#[derive(Clone, Parser, Debug)]
#[command(version, about, long_about = None, 
    help_template = CUSTOM_HELP_TEMPLATE)]
pub struct Args {
    //Primary group of arguments.
    
    ///Path to single ROM file or directory of ROMs for input. 
    ///Can be specified more than once.
    #[arg(short, long, help_heading = "Primary Options", 
    required = true)]
    pub input: Vec<PathBuf>,

    ///Path for the output archive or extracted files.
    #[arg(short, long, help_heading = "Primary Options")]
    pub output: Option<PathBuf>,

    ///Activates metadata retrieval mode, used in combination with -i.
    #[arg(short, long, help_heading = "Primary Options", 
    conflicts_with = "extract")]
    pub metadata: bool,

    ///Activates json metadata mode. The metadata output will be printed in 
    /// json format.
    #[arg(short, long, help_heading = "Primary Options", 
    conflicts_with = "extract")]
    pub json: bool,

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
    ///Recommended value is 2kb.
    #[arg(short, long, help_heading = "Tuning Parameters")]
    pub window: Option<ByteSize>,

    ///Sets the hashing algorithm bit length.
    /// Default value is 64.
    /// If the hash verification stage fails set to 128.
    #[arg(long, help_heading = "Tuning Parameters")]
    pub hash_bit_length: Option<u32>,

    ///Determines the dictionary size for the compression algorithm.
    ///(e.g., "256K", "4KB", "64KiB")
    ///Recommended value is 16kb.
    #[arg(short, long, help_heading = "Tuning Parameters")]
    pub dictionary: Option<ByteSize>,

    ///Autotune both the window size and dictionary size. Will find the 
    /// reasonably optimal size for each. This uses a somewhat inefficient 
    /// algorithm and can take time but will lead to a smaller file size
    /// more easily. If values for either the window or dictionary are 
    /// provided they will be used instead of autotuning.
    #[arg(long, help_heading = "Tuning Parameters", 
    default_value_t = false)]
    pub auto_tune: bool,

    ///Sets the maximum time in seconds for each autotune iteration.
    ///If an iteration exceeds this time, the latest result is used.
    #[arg(long, help_heading = "Tuning Parameters")]
    pub autotune_timeout: Option<u64>,

    ///When finalizing the archive, optimize the dictionary for better compression.
    /// NOT recommended for large files as it can be very slow. 
    #[arg(long, help_heading = "Tuning Parameters", 
    default_value_t = false)]
    pub optimize_dictionary: bool,

    //Behavior and Output Control

    ///Sets the maximum number of worker threads to use.
    ///Defaults to all available logical cores.
    #[arg(short, long, help_heading = "Behavior and Output Control")]
    pub threads: Option<usize>,

    ///Forces low-memory mode by processing files sequentially and
    ///limiting worker threads to 4 for compression.
    #[arg(long, help_heading = "Behavior and Output Control", 
    default_value_t = false)]
    pub low_memory: bool,

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
/// improper usage of flags based on the selected operation mode and resolves
/// config conflicts. Its purpose is to ensure the application has a valid 
/// set of arguments before proceeding with any work.
///
/// # Arguments
///
/// * `args`: A reference to the `Args` struct, which contains the
///   parsed command-line arguments.
/// * `matches`: A reference to `clap::ArgMatches`, used to determine which
///   arguments were actually provided by the user on the command line.
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
pub fn validate_args(
    args: &Args,
    matches: &ArgMatches
) -> Result<(), CliError> {
    let arg_was_provided = |name: &str| matches.contains_id(name);
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

    if !args.force && !args.metadata && args.extract.is_none() {
        if let Some(output_path) = &args.output {
            if output_path.exists() {
                return Err(CliError::FileExistsError(format!(
                    "Output file already exists: {}. Use --force to overwrite.",
                    output_path.display()
                )));
            }
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
        if let Some(output_path) = &args.output {
            if !output_path.is_dir() && output_path.exists() {
                return Err(CliError::InvalidPath(
                    "The output path for extraction must be a directory."
                        .to_string(),
                ));
            }
        }
        if args.output.is_none() {
            return Err(CliError::MissingFlag(
                "Output path (-o, --output) is required for extraction.".to_string(),
            ));
        }
        if args.force {
            return Err(CliError::ConflictingFlagsError(
                "--force cannot be used with --extract".to_string(),
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

    //Check for existence of input paths
    for path in &args.input {
        if !path.exists() {
            return Err(CliError::InvalidPath(format!(
                "Input path does not exist: {}",
                path.display()
            )));
        }
    }

    /*Check if both window and dictionary parameters are specified when 
    auto-tune flag is provided.*/
    if arg_was_provided("auto_tune") &&
       arg_was_provided("window") &&
       arg_was_provided("dictionary")
    {
        return Err(CliError::ConflictingFlagsError(
                "When using auto-tune only one, window or dictionary, \
                    parameters can be used.".to_string(),
            ));
    }

    Ok(())
}

/// Merges settings from a configuration file with command-line arguments.
///
/// This function establishes a clear order of precedence for application
/// settings, where command-line arguments override the corresponding values
/// found in the configuration file. It iterates through each configurable
/// option, checks if it was provided by the user as a command-line argument,
/// and if not, falls back to the value from the loaded configuration.
///
/// # Arguments
///
/// * `config`: A `SpriteShrinkConfig` struct containing the settings loaded
///   from the configuration file.
/// * `args`: An `Args` struct that has been parsed from the command-line
///   arguments. This struct will be mutated to incorporate the settings
///   from the config file.
/// * `matches`: A reference to `clap::ArgMatches`, used to determine which
///   arguments were actually provided by the user on the command line.
///
/// # Returns
///
/// An `Args` struct where the final, merged settings are stored. This returned
/// struct is what the application will use to control its execution.
pub fn merge_config_and_args (
    config: SpriteShrinkConfig,
    mut args: Args, 
    matches: &ArgMatches,
) -> Args {
    //This helper checks if an argument was present on the command line.
    let arg_was_present = |name: &str| matches.contains_id(name);

    //If the user did NOT provide a flag, use the value from the config file.
    if !arg_was_present("compression_level") {
        args.compression_level = config.compression_level;
    }
    if !arg_was_present("window") {
        args.window = config.window_size.parse().ok();
    }
    if !arg_was_present("dictionary") {
        args.dictionary = config.dictionary_size.parse().ok();
    }
    if !arg_was_present("hash_bit_length") {
        args.hash_bit_length = Some(config.hash_bit_length);
    }
    if !arg_was_present("auto_tune") {
        args.auto_tune = config.auto_tune;
    }
    if !arg_was_present("autotune_timeout") {
        args.autotune_timeout = Some(config.autotune_timeout);
    }
    if !arg_was_present("optimize_dictionary") {
        args.optimize_dictionary = config.optimize_dictionary;
    }
    if !arg_was_present("threads") {
        args.threads = Some(config.threads);
    }
    if !arg_was_present("low_memory") {
        args.low_memory = config.low_memory;
    }
    if !arg_was_present("verbose") {
        args.verbose = config.verbose;
    }
    if !arg_was_present("json") {
        args.json = config.json_output;
    }

    if args.auto_tune {
        if !arg_was_present("window") {
            args.window = None;
        }
        if !arg_was_present("dictionary") {
            args.dictionary = None;
        }
    }

    args
}