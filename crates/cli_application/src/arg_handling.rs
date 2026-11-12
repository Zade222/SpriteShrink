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
use clap::{ArgMatches, Parser, parser::ValueSource};
use sprite_shrink::{MAX_WINDOW_SIZE, MIN_WINDOW_SIZE};


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
        help = "Activates archive file list mode, used in \ncombination with \
        -i. Lists all files, and their\n indices, in an archive."
    )]
    pub list: bool,

    #[arg(
        short,
        long,
        help_heading = "Primary Options",
        conflicts_with = "extract",
        help = "Activates json metadata mode. The metadata output \nwill be \
        printed in json format."
    )]
    pub metadata: bool,

    #[arg(
        short,
        long, help_heading = "Primary Options",
        help = "Specifies the index number(s) of the ROM(s) to be \nextracted, \
        as listed by the -m flag."
    )]
    pub extract: Option<String>,

    //Tuning paramters

    #[arg(
        short = 'c',
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
        value_name = "MODE",
        help_heading = "Tuning Parameters",
        default_value = "default",
        help = "Sets the compression mode for\nformat-specific archives. \n\
                Use 'optical' for bin/cue images.\n"
    )]
    pub mode: String,

    #[arg(
        long,
        help_heading = "Tuning Parameters",
        default_value_t = false,
        help = "When generating dictionary for compression,\noptimize the \
        dictionary for better\ncompression.\n\
        Considered experimental and unstable due to\n\
        unpredictable amount of memory it can\n\
        consume.\n\
        NOT recommended for large files as it can\n\
        be very slow and can cause the application\n\
        to consume a significant amount of memory\n\
        even with the low-memory flag.\n\
        To avoid system instability, do not use\n\
        with a large input data set."
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
        exists and/or if the output directory doesn't\nexist, creates it."
    )]
    pub force: bool,

    #[arg(
        short,
        long,
        help_heading = "Behavior and Output Control",
        default_value_t = false,
        help = "Activates printing progress to console."
    )]
    pub progress: bool,

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

    /// Whether `--list` prints JSON, from the config's `json_output`. It has
    /// no flag; `-m` prints JSON for a single run.
    #[arg(skip)]
    pub json_output: bool,
}

/// Whether the user typed the argument `name` on the command line.
///
/// `ArgMatches::contains_id` can't tell: it is also true for an argument
/// that only has its default value, and every `bool` flag has the default
/// `false`.
fn given_on_command_line(matches: &ArgMatches, name: &str) -> bool {
    matches.value_source(name) == Some(ValueSource::CommandLine)
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
/// - The compression output (`-o`) already exists and `--force` isn't set.
/// - `--mode optical` with `--list`, `--extract` or `--metadata`.
/// - Extraction with more than one input path.
/// - An extraction range (`-e`) that isn't a list or range of numbers.
/// - An extraction output path that exists and isn't a directory.
/// - Extraction or compression without an output path (`-o`).
/// - A compression output path that is a directory.
/// - Compression of a single input file.
/// - A window size, from `-w` or the config file, outside
///   `MIN_WINDOW_SIZE..=MAX_WINDOW_SIZE` when compressing.
/// - An input path that doesn't exist.
/// - `--auto-tune` with both `-w` and `-d` given on the command line.
///
/// clap rejects a missing `-i`, and `--extract` with `--list` or
/// `--metadata`, before this runs.
pub fn validate_args(
    args: &Args,
    matches: &ArgMatches
) -> Result<(), CliError> {
    let arg_was_provided = |name: &str| given_on_command_line(matches, name);
    //Check if the output file already exists and if the force flag is set.
    if let Some(output_path) = &args.output
        && !args.force
        && !args.list
        && args.extract.is_none()
        && output_path.exists()
    {
        return Err(CliError::FileExistsError(output_path.to_path_buf()));
    }

    if args.mode == "optical" && (args.list || args.extract.is_some() || args.metadata) {
        return Err(CliError::ConflictingArguments(
            "The --mode 'optical' flag is only applicable for compression and \
            cannot be used with --list, --extract, or --metadata.".to_string(),
        ));
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
    if !args.list && !args.metadata && args.extract.is_none() {
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

        /*The chunker panics on a window size outside FastCDC's range. The
        check is only made when compressing, so a bad config value doesn't
        stop extraction or listing.*/
        if let Some(window) = args.window
            && !(MIN_WINDOW_SIZE..=MAX_WINDOW_SIZE).contains(&window.as_u64())
        {
            return Err(CliError::InvalidWindowSize(window.as_u64()));
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
    let arg_was_present = |name: &str| given_on_command_line(matches, name);

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
    // `json_output` formats `--list` as JSON. It isn't `-m`, which would
    // switch every run to printing metadata.
    args.json_output = config.json_output;
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

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, FromArgMatches};

    use super::*;

    /// Parses `flags` after the program name, the way `main` does.
    fn parse(flags: &[&str]) -> (Args, ArgMatches) {
        let matches = Args::command()
            .try_get_matches_from(
                std::iter::once("sprite-shrink").chain(flags.iter().copied()),
            )
            .unwrap();
        let args = Args::from_arg_matches(&matches).unwrap();
        (args, matches)
    }

    /// A config whose every setting differs from the flags' defaults.
    fn config() -> SpriteShrinkConfig {
        SpriteShrinkConfig {
            compression_level: 7,
            window_size: "4kb".to_string(),
            dictionary_size: "32kb".to_string(),
            hash_bit_length: 128,
            auto_tune: false,
            autotune_timeout: 30,
            optimize_dictionary: true,
            threads: 3,
            low_memory: true,
            json_output: true,
            quiet_output: true,
            ..SpriteShrinkConfig::default()
        }
    }

    #[test]
    fn config_values_apply_to_flags_not_given() {
        let (args, matches) = parse(&["-i", "in"]);
        let merged = merge_config_and_args(&config(), args, &matches);

        assert_eq!(merged.compression_level, 7);
        assert_eq!(merged.window, Some(ByteSize::kb(4)));
        assert_eq!(merged.dictionary, Some(ByteSize::kb(32)));
        assert_eq!(merged.hash_bit_length, Some(128));
        assert_eq!(merged.autotune_timeout, Some(30));
        assert!(merged.optimize_dictionary);
        assert_eq!(merged.threads, Some(3));
        assert!(merged.low_memory);
        assert!(merged.quiet);
    }

    #[test]
    fn flags_given_override_the_config() {
        // `-c 19` is the flag's default value, so this also checks that a
        // flag is told apart from its default by where it came from.
        let (args, matches) = parse(&[
            "-i", "in", "-c", "19", "-w", "8kb", "-d", "64kb", "-b", "64",
            "--autotune-timeout", "5", "-t", "2",
        ]);
        let merged = merge_config_and_args(&config(), args, &matches);

        assert_eq!(merged.compression_level, 19);
        assert_eq!(merged.window, Some(ByteSize::kb(8)));
        assert_eq!(merged.dictionary, Some(ByteSize::kb(64)));
        assert_eq!(merged.hash_bit_length, Some(64));
        assert_eq!(merged.autotune_timeout, Some(5));
        assert_eq!(merged.threads, Some(2));
    }

    #[test]
    fn bool_flags_given_turn_on_what_the_config_turns_off() {
        let (args, matches) = parse(&[
            "-i", "in", "--auto-tune", "--optimize-dictionary",
            "--low-memory", "-q",
        ]);
        let merged = merge_config_and_args(
            &SpriteShrinkConfig::default(),
            args,
            &matches,
        );

        assert!(merged.auto_tune);
        assert!(merged.optimize_dictionary);
        assert!(merged.low_memory);
        assert!(merged.quiet);
    }

    #[test]
    fn json_output_formats_the_list_without_switching_to_metadata() {
        let (args, matches) = parse(&["-i", "in"]);
        let merged = merge_config_and_args(&config(), args, &matches);
        assert!(merged.json_output);
        assert!(!merged.metadata);
    }

    #[test]
    fn auto_tune_from_the_config_tunes_sizes_not_given() {
        let config = SpriteShrinkConfig {
            auto_tune: true,
            ..config()
        };

        let (args, matches) = parse(&["-i", "in"]);
        let merged = merge_config_and_args(&config, args, &matches);
        assert!(merged.auto_tune);
        assert_eq!((merged.window, merged.dictionary), (None, None));

        let (args, matches) = parse(&["-i", "in", "-w", "8kb"]);
        let merged = merge_config_and_args(&config, args, &matches);
        assert_eq!(merged.window, Some(ByteSize::kb(8)));
        assert_eq!(merged.dictionary, None);
    }

    /// A folder with two ROMs, a subfolder and an existing archive.
    struct Inputs {
        dir: tempfile::TempDir,
    }

    impl Inputs {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            std::fs::write(dir.path().join("a.bin"), b"a").unwrap();
            std::fs::write(dir.path().join("b.bin"), b"b").unwrap();
            std::fs::write(dir.path().join("old.ssmc"), b"old").unwrap();
            std::fs::create_dir(dir.path().join("sub")).unwrap();
            Self { dir }
        }

        /// The path of `name` inside the folder, as a string.
        fn path(&self, name: &str) -> String {
            self.dir.path().join(name).to_str().unwrap().to_string()
        }

        /// `validate_args` on `flags`, where `{name}` stands for the path of
        /// `name` inside the folder.
        fn validate(&self, flags: &[&str]) -> Result<(), CliError> {
            let flags: Vec<String> = flags
                .iter()
                .map(|flag| match flag.strip_prefix('{') {
                    Some(name) => self.path(name.trim_end_matches('}')),
                    None => (*flag).to_string(),
                })
                .collect();
            let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
            let (args, matches) = parse(&flags);
            validate_args(&args, &matches)
        }
    }

    #[test]
    fn valid_arguments_are_accepted() {
        let inputs = Inputs::new();
        let cases: [&[&str]; 6] = [
            &["-i", "{a.bin}", "-i", "{b.bin}", "-o", "{new.ssmc}"],
            &["-i", "{sub}", "-o", "{new.ssmc}"],
            &["-i", "{a.bin}", "-i", "{b.bin}", "-o", "{old.ssmc}", "-f"],
            &["-i", "{old.ssmc}", "-o", "{sub}", "-e", "1,3-5"],
            &["-i", "{old.ssmc}", "-l"],
            &["-i", "{sub}", "-o", "{new.ssmc}", "--auto-tune", "-w", "4kb"],
        ];
        for flags in cases {
            let result = inputs.validate(flags);
            assert!(result.is_ok(), "{flags:?}: {result:?}");
        }
    }

    /// What a case is called, its flags, and whether an error is the one
    /// it should give.
    type Rejection =
        (&'static str, &'static [&'static str], fn(&CliError) -> bool);

    #[test]
    fn each_documented_rejection_is_made() {
        let inputs = Inputs::new();
        let cases: [Rejection; 12] = [
            (
                "compression output exists",
                &["-i", "{sub}", "-o", "{old.ssmc}"],
                |e| matches!(e, CliError::FileExistsError(_)),
            ),
            (
                "optical with list",
                &["-i", "{old.ssmc}", "-l", "--mode", "optical"],
                |e| matches!(e, CliError::ConflictingArguments(_)),
            ),
            (
                "extraction from two inputs",
                &["-i", "{old.ssmc}", "-i", "{a.bin}", "-o", "{sub}", "-e", "1"],
                |e| matches!(e, CliError::TooManyFiles(_)),
            ),
            (
                "extraction range not a number",
                &["-i", "{old.ssmc}", "-o", "{sub}", "-e", "one"],
                |e| matches!(e, CliError::InvalidFormRomRange(_)),
            ),
            (
                "extraction output is a file",
                &["-i", "{old.ssmc}", "-o", "{a.bin}", "-e", "1"],
                |e| matches!(e, CliError::InvalidPath(_)),
            ),
            (
                "extraction without output",
                &["-i", "{old.ssmc}", "-e", "1"],
                |e| matches!(e, CliError::MissingFlag(_)),
            ),
            (
                "compression without output",
                &["-i", "{sub}"],
                |e| matches!(e, CliError::MissingFlag(_)),
            ),
            (
                // Without `-f`, an existing folder is rejected as existing.
                "compression output is a folder",
                &["-i", "{a.bin}", "-i", "{b.bin}", "-o", "{sub}", "-f"],
                |e| matches!(e, CliError::InvalidPath(_)),
            ),
            (
                "compression of one file",
                &["-i", "{a.bin}", "-o", "{new.ssmc}"],
                |e| matches!(e, CliError::NotEnoughFiles),
            ),
            (
                "window too large",
                &["-i", "{sub}", "-o", "{new.ssmc}", "-w", "1GiB"],
                |e| matches!(e, CliError::InvalidWindowSize(_)),
            ),
            (
                "input missing",
                &["-i", "{missing}", "-l"],
                |e| matches!(e, CliError::InvalidPath(_)),
            ),
            (
                "auto-tune with window and dictionary",
                &[
                    "-i", "{sub}", "-o", "{new.ssmc}", "--auto-tune",
                    "-w", "4kb", "-d", "16kb",
                ],
                |e| matches!(e, CliError::ConflictingArguments(_)),
            ),
        ];
        for (what, flags, expected) in cases {
            match inputs.validate(flags) {
                Err(e) => assert!(expected(&e), "{what}: got {e:?}"),
                Ok(()) => panic!("{what}: accepted {flags:?}"),
            }
        }
    }

    #[test]
    fn clap_rejects_what_validate_args_relies_on() {
        let cases: [&[&str]; 3] = [
            &["-o", "out"],
            &["-i", "in", "-l", "-e", "1"],
            &["-i", "in", "-m", "-e", "1"],
        ];
        for flags in cases {
            let result = Args::command().try_get_matches_from(
                std::iter::once("sprite-shrink").chain(flags.iter().copied()),
            );
            assert!(result.is_err(), "{flags:?} parsed");
        }
    }
}
