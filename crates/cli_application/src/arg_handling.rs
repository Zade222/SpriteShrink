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


use crate::{
    cli_types::SpriteShrinkConfig,
    error_handling::CliError
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
#[command(
    version, 
    about, 
    long_about = None, 
    help_template = CUSTOM_HELP_TEMPLATE,
    //term_width = 80
)]
pub struct Args {
    //Primary group of arguments.
    #[arg(
        short, 
        long, 
        help_heading = "Primary Options", 
        required = true,
        help = "Path to single ROM file or directory of ROMs for \ninput. Can \
        be specified more than once."
    )]
    pub input: Vec<PathBuf>,

    #[arg(
        short, 
        long, 
        help_heading = "Primary Options",
        help = "Path for the output archive or extracted files."
    )]
    pub output: Option<PathBuf>,

    #[arg(
        short, 
        long, 
        help_heading = "Primary Options", 
        conflicts_with = "extract",
        help = "Activates metadata retrieval mode, used in \ncombination with \
        -i."
    )]
    pub metadata: bool,

    #[arg(
        short, 
        long, 
        help_heading = "Primary Options", 
        conflicts_with = "extract",
        help = "Activates json metadata mode. The metadata output \nwill be \
        printed in json format."
    )]
    pub json: bool,

    #[arg(
        short, 
        long, help_heading = "Primary Options",
        help = "Specifies the index number(s) of the ROM(s) to be \nextracted, \
        as listed by the -m flag."
    )]
    pub extract: Option<String>,

    //Tuning paramters

    /*#[arg(
        short, 
        long, 
        help_heading = "Tuning Parameters", 
        default_value_t = String::from("zstd"),
        help = "Sets the compression algorithm.\n
        Choices: zstd, lzma, deflate. Defaults to zstd."
    )]
    pub compression: String,*/

    #[arg(
        short = 'l', 
        long = "compression-level", 
        value_name = "LEVEL",
        help_heading = "Tuning Parameters",
        default_value_t = 19,
        help = "Sets the numerical compression level for the\nchosen \
        algorithm."
    )]
    pub compression_level: u8,

    #[arg(
        short, 
        long, 
        help_heading = "Tuning Parameters",
        help = "Determines the window size for the hashing \nalgorithm. \
        (e.g., '256K', '4KB', '64KiB')\nRecommended value is 2kb."
    )]
    pub window: Option<ByteSize>,

    #[arg(
        short, 
        long, 
        help_heading = "Tuning Parameters",
        help = "Determines the dictionary size for the \ncompression \
        algorithm. \n(e.g., '256K', '4KB', '64KiB')\nRecommended value is 16kb."
    )]
    pub dictionary: Option<ByteSize>,

    #[arg(
        short = 'b', 
        long = "hash-bit-length", 
        value_name = "LENGTH",
        help_heading = "Tuning Parameters",
        help = "Sets the hashing algorithm bit length. \nDefault value is 64.\n\
        If the hash verification stage fails set to\n128."
    )]
    pub hash_bit_length: Option<u32>,
    
    #[arg(
        long, 
        help_heading = "Tuning Parameters", 
        default_value_t = false,
        help = "Autotune both the window size and dictionary\nsize. Will find \
        the reasonably optimal size\nfor each. This uses a somewhat \
        inefficient\nalgorithm and can take time but will result\nin a smaller \
        file size. If values for either\nthe window or dictionary are provided \
        either\nwill be used instead of autotuning."
    )]
    pub auto_tune: bool,

    #[arg(
        long, 
        help_heading = "Tuning Parameters",
        value_name = "TIMEOUT",
        help = "Sets the maximum time in seconds for each \nautotune iteration. \
        If an iteration exceeds\nthis time, the latest result is used."
    )]
    pub autotune_timeout: Option<u64>,

    #[arg(
        long, 
        help_heading = "Tuning Parameters", 
        default_value_t = false,
        help = "When generating dictionary for compression,\noptimize the \
        dictionary for better\ncompression. NOT recommended for large files\n\
        as it can be very slow."
    )]
    pub optimize_dictionary: bool,

    //Behavior and Output Control

    #[arg(
        short, 
        long, 
        help_heading = "Behavior and Output Control",
        help = "Sets the maximum number of worker threads to use. \nDefaults to \
        all available logical cores."
    )]
    pub threads: Option<usize>,

    #[arg(
        long, 
        help_heading = "Behavior and Output Control",
        help = "Forces SpriteShrink to not read it's config and won't\ncreate \
        one if it doesn't already exist. For flags not\nspecified, each will\
        use default values."
    )]
    pub ignore_config: bool,

    #[arg(
        long, 
        help_heading = "Behavior and Output Control", 
        default_value_t = false,
        help = "Forces low-memory mode by using an on disk cache for \n\
        temporarily storing data. Can adversely effect \napplication \
        performance."
    )]
    pub low_memory: bool,

    #[arg(
        short, 
        long, 
        help_heading = "Behavior and Output Control", 
        default_value_t = false,
        help = "Forces the application to overwrite the output file \nif it \
        exists and if the output directory doesn't\nexist, creates it."
    )]
    pub force: bool,

    #[arg(
        short, 
        long, 
        help_heading = "Behavior and Output Control", 
        default_value_t = false,
        help = "Activates verbose output for detailed diagnostic\ninformation."
    )]
    pub verbose: bool,

    #[arg(
        short, 
        long, 
        help_heading = "Behavior and Output Control", 
        default_value_t = false,
        help = "Activates quiet mode, suppressing all non-essential\noutput."
    )]
    pub quiet: bool,

    #[arg(
        long, 
        help_heading = "Behavior and Output Control",
        help = "Disables logging messages to log file."
    )]
    pub disable_logging: bool,
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
    //Check if the output file already exists and if the force flag is set.
    if let Some(output_path) = &args.output
        && !args.force
        && !args.metadata
        && args.extract.is_none()
        && output_path.exists()
    {
        return Err(CliError::FileExistsError(output_path.to_path_buf()));
    }

    //Extraction Mode ROM Index Validation 
    if let Some(rom_range) = &args.extract {
        if args.input.len() > 1 {
            return Err(CliError::TooManyFiles(
                "Only one input file is allowed for extraction.".to_string(),
            ));
        }
        if range_parser::parse::<u32>(rom_range).is_err() {
            return Err(CliError::InvalidFormRomRange(
                "The ROM index range format is invalid. \
                Use a comma-separated list or a range (e.g., 1,3,5-7)."
                .to_string(),
            ));
        }


        if let Some(output_path) = &args.output 
            && !output_path.is_dir()
            && output_path.exists()
        {
            return Err(CliError::InvalidPath(
                "The output path for extraction must be a directory."
                    .to_string(),
            ));
        }


        if args.output.is_none() {
            return Err(CliError::MissingFlag(
                "Output path (-o, --output) is required for extraction."
                    .to_string(),
            ));
        }
    }

    //Compression Mode
    if !args.metadata && args.extract.is_none() {
        if args.output.is_none() {
            return Err(CliError::MissingFlag(
                "Output path (-o) is required when in compression mode."
                    .to_string(),
            ));
        }

        if let Some(output_path) = &args.output
            && output_path.is_dir()
        {
            return Err(CliError::InvalidPath(
                "The output path must be a file, not a directory.".to_string()
            ));
        }

        if args.input.len() == 1 && args.input[0].is_file() {
            return Err(CliError::NotEnoughFiles);
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
    if args.auto_tune &&
       arg_was_provided("window") &&
       arg_was_provided("dictionary")
    {
        return Err(CliError::ConflictingArguments(
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
    config: &SpriteShrinkConfig,
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
    if !arg_was_present("quiet") {
        args.quiet = config.quiet_output;
    }

    //If auto-tune is enabled (from either CLI or config), and the user
    //did NOT provide a window/dictionary flag, set them to None to ensure
    //auto-tuning actually runs for that parameter.
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