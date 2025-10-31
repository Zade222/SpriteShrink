# SpriteShrink User Manual

Welcome to the official user manual for SpriteShrink! This guide provides everything you need to know to effectively use the application, from basic commands to advanced performance tuning and everything in between.

## Table of Contents

- [SpriteShrink User Manual](#spriteshrink-user-manual)
  - [Table of Contents](#table-of-contents)
  - [A Gentle Introduction to the Command Line](#a-gentle-introduction-to-the-command-line)
    - [What is a Command-Line Interface?](#what-is-a-command-line-interface)
    - [Why Use a CLI?](#why-use-a-cli)
    - [Opening Your Terminal](#opening-your-terminal)
    - [Basic Concepts](#basic-concepts)
  - [Getting Started](#getting-started)
    - [Installation](#installation)
  - [Core Concepts](#core-concepts)
    - [What Problem Does SpriteShrink Solve?](#what-problem-does-spriteshrink-solve)
    - [How It Works](#how-it-works)
  - [Usage Guide](#usage-guide)
    - [1. Compressing Files into an `.ssmc` Archive](#1-compressing-files-into-an-ssmc-archive)
    - [2. Inspecting an `.ssmc` Archive](#2-inspecting-an-ssmc-archive)
    - [3. Extracting Files from an `.ssmc` Archive](#3-extracting-files-from-an-ssmc-archive)
  - [Command Line Reference](#command-line-reference)
  - [Advanced Usage \& Performance Tuning](#advanced-usage--performance-tuning)
    - [The Configuration File](#the-configuration-file)
      - [Configuration File Location](#configuration-file-location)
  - [Helper Scripts](#helper-scripts)
  - [Troubleshooting](#troubleshooting)
  - [License and Support](#license-and-support)

---

## A Gentle Introduction to the Command Line

If you're new to command-line interface (CLI) applications, this section is for you!

### What is a Command-Line Interface?

A CLI is a text-based way to interact with your computer. Instead of clicking on icons and buttons, you type commands to tell the computer what to do. It might seem intimidating at first, but it's an incredibly powerful and efficient way to perform tasks. SpriteShrink is a CLI tool, meaning you'll run it from a terminal.

### Why Use a CLI?

*   **Power & Flexibility**: CLIs give you fine-grained control over the application's behavior through flags and options.
*   **Automation**: You can easily write scripts to perform complex, repetitive tasks—something the included [Helper Scripts](#helper-scripts) do for you!
*   **Efficiency**: Once you're comfortable, using a CLI can be much faster than navigating graphical menus.

### Opening Your Terminal

*   **Windows**: Press `Win + X`, and click windows terminal to open a terminal window.
*   **macOS**: Open the "Terminal" app from your Applications/Utilities folder.
*   **Linux**: Look for "Terminal" in your applications menu, or use a shortcut like `Ctrl + Alt + T`. The terminal will likely be Gnome Terminal, Konsole or whatever your distribution has provided by default. Given the open nature of linux you may have even installed one you prefer over the default one but use whatever you like.

### Basic Concepts

*   **Command**: The name of the program you want to run (e.g., `sprite-shrink`).
*   **Argument/Flag**: Options that modify the command's behavior. They usually start with a hyphen (`-i`) or double hyphen (`--input`).
*   **Path**: A "road map" to a file or directory.
    *   **Absolute Path**: A full address from the root of your filesystem (e.g., `'C:\Users\user\Games'` or `'/home/user/games'`).
    *   **Relative Path**: A path from your current location (e.g., `../games` means "go up one level, then into the games directory").
    *   Most applications and terminal interpret spaces as different aspects/flags of a command, be sure to use apostrophes to enclose paths such as `'/home/user/fun games'`. If you were to use a path without the apostrophes for a path with a space it will cause an error.

Your first step with any CLI tool should be to ask for help. This is usually done with `-h` or `--help`. Navigate to the directory where you extracted SpriteShrink and try it:

```bash
# On Windows
.\sprite-shrink.exe --help

# On Linux/macOS
./sprite-shrink --help
```

This command will print a full list of all available arguments and what they do. While SpriteShink has help text that is called in this manner, you can call the help text of most other cli applications in this way too!

Also note the `.\` or `./` before the sprite-shrink command. This tells your system to run or execute an application in the current working directory where your terminal is looking. Some operating systems and linux distros provide an easy way to open a terminal in a file browser by right clicking an empty space of a folder and clicking something along the lines of `Open Terminal Here` or `Open Power Shell Here`. Otherwise you will need to set the working directory to where the executable is stored using `cd`.

Using `cd` is straight forward. To get a list of files and folders in the current working directory use the `ls` command on linux or macos or `dir` on windows. Then type the name of the folder or subdirectory you want to move to such as `cd Games` or `cd 'RPG Games'`(Note the use of the apostrophes as explained from earlier).

---

## Getting Started

### Installation

You have two primary ways to get SpriteShrink:

1.  **Pre-compiled Binaries (Recommended)**: The easiest way is to download the latest version for your operating system from the [GitHub Releases page](https://github.com/Zade222/SpriteShrink/releases). Simply download, unzip, and you're ready to go.

2.  **Building from Source**: If you are a developer, someone who prefers to compile things themselves or a binary isn't available for your system, you can build it yourself. This requires the [Rust toolchain](https://www.rust-lang.org/tools/install). For detailed instructions, please refer to the [Building from Source section in the README.md file](../README.md#building-from-source-for-developers).

---

## Core Concepts

### What Problem Does SpriteShrink Solve?

Imagine you have a collection of retro game ROMs. For a single game, you might have the USA version, a European version, a Japanese version, and maybe a "Rev A" or "Rev B" bug-fix release. All these files are very similar, containing large amounts of identical data such as game assets, code and music.

Standard compression tools like ZIP or 7-Zip compress each file individually using what is known as solid compression. They can't see that the same data exists in multiple files, so they store it over and over again depending on the parameters given to the compression application.

SpriteShrink is different. It's **content-aware**. It looks inside all the files and finds the identical pieces. By storing each unique piece of data only *once*, it achieves much higher compression ratios for these kinds of collections.

### How It Works

SpriteShrink uses a multi-stage process to intelligently archive your files:

1.  **Chunking**: It breaks your files into small, variable-sized chunks using an algorithm that ensures identical data segments result in identical chunks.
2.  **Deduplication**: It keeps only one copy of each unique chunk, discarding all duplicates.
3.  **Compression**: It then compresses this set of unique chunks using the highly efficient Zstandard algorithm.
4.  **Archiving**: Finally, it packs the compressed data and a manifest (a list of the original files and how to rebuild them) into a single `.ssmc` file.

This combination of deduplication and compression results in superior storage savings for file sets with redundant data.

---

## Usage Guide

SpriteShrink operates in three main modes: **compressing**, **inspecting**, and **extracting**.

### 1. Compressing Files into an `.ssmc` Archive

This is the primary function of SpriteShrink. You point it at a directory of files, and it creates a single, highly optimized `.ssmc` archive.

**Command Structure:**
`./sprite_shrink -i <path_to_your_files> -o <path_for_your_archive.ssmc>`

*   `-i` or `--input`: The path to the directory containing the files you want to compress.
*   `-o` or `--output`: The desired path and filename for the new `.ssmc` archive.

**Example:**
Let's say you have your game variants in a folder named `MyGameCollection`.

```bash
./sprite_shrink -i ./MyGameCollection -o ./MyGame.ssmc -p
```

*   This command reads all files inside `MyGameCollection`.
*   It creates a new archive named `MyGame.ssmc` in the current directory.
*   The `-p` flag shows a progress bar so you can see what's happening.

### 2. Inspecting an `.ssmc` Archive

Before you extract files, you need to know what's inside the archive. The file list mode lets you view the contents.

**Command Structure:**
`./sprite_shrink -i <path_to_your_archive.ssmc> -l`

*   `-i` or `--input`: The path to the `.ssmc` archive you want to inspect.
*   `-l` or `--list`: This flag activates file list mode and lists all the files in an archive.

**Example:**

```bash
./sprite_shrink -i ./MyGame.ssmc -l
```

**Output:**
The tool will print a table listing all the files in the archive, each with an **index number**. This index is crucial for extraction.

```
+-------+--------------------------------+---------------+
| Index | Filename                       | Original Size |
+-------+--------------------------------+---------------+
| 1     | MyGame (USA).rom               | 4.19 MB       |
| 2     | MyGame (Europe).rom            | 4.19 MB       |
| 3     | MyGame (Japan).rom             | 4.21 MB       |
+-------+--------------------------------+---------------+
```

For automation, you can get this same information in a machine-readable format using the `-m` (`--metadata`) flag.

### 3. Extracting Files from an `.ssmc` Archive

Once you know the index of the file(s) you want, you can extract them.

**Command Structure:**
`./sprite_shrink -i <archive.ssmc> -o <output_directory> -e <indices>`

*   `-i` or `--input`: The `.ssmc` archive to extract from.
*   `-o` or `--output`: The directory where the extracted files should be saved.
*   `-e` or `--extract`: The index or indices of the files to extract. You can specify multiple indices separated by commas (e.g., `1,2`) or a range (e.g., `1-3`).

**Example:**
Let's extract the USA and Japan versions (indices 1 and 3) from our `MyGame.ssmc` archive into a folder called `ExtractedGames`.

```bash
./sprite_shrink -i ./MyGame.ssmc -o ./ExtractedGames -e 1,3 -f
```

*   This will re-create `MyGame (USA).rom` and `MyGame (Japan).rom` inside the `ExtractedGames` folder.
*   The `-f` (`--force`) flag is used here to create the output directory if it doesn't already exist.

---

## Command Line Reference

This is a quick reference for the most common arguments. For a full list, run `./sprite_shrink --help`.

| Short Flag | Long Flag             | Description                                                              |
| :---       | :---                  | :---                                                                     |
| `-i`       | `--input`             | **Required.** Path to the input file(s) or directory.                    |
| `-o`       | `--output`            | Path to the compression/extraction output path.                          |
| `-l`       | `--list`              | View archive contents in a plain text table.                             |
| `-e`       | `--extract`           | Extract files by index (e.g., `1,3,5-7`).                                |
| `-p`       | `--progress`          | Show a progress bar.                                                     |
| `-f`       | `--force`             | Overwrite output file or create output directory if it doesn't exist.    |
| `-q`       | `--quiet`             | Suppress all non-error output.                                           |
| `-h`       | `--help`              | Print the full help message and exit.                                    |

---

## Advanced Usage & Performance Tuning

SpriteShrink offers several options to fine-tune the compression process. For most users, the defaults are enough. However, if you want to squeeze out every last byte of space or are working on a resource-constrained system, these flags are for you.

*   `-c <1-22>` or `--compression-level <1-22>`: Sets the Zstandard compression level. Higher levels are slower but produce smaller files. The default (`19`) is a strong setting. A good alternative is `3` for a respectable compressed size, albeit at a much faster compression speed.

*   `--auto-tune`: This is a powerful feature that automatically tests different chunking and dictionary sizes to find the optimal settings for your specific dataset. It can be slow but often yields the best compression ratio. Use this when file size is your absolute priority.

*   `--optimize-dictionary`: This flag makes the dictionary training process much more thorough. It can be **very slow** and **unstable** with large input data sets, but further improves the compression ratio. It is best used in combination with `--auto-tune`.

*   `--low-memory`: If you are running SpriteShrink on a machine with limited RAM (e.g., a Raspberry Pi), this flag will force the application to use a disk-based cache instead of keeping everything in memory. This will be noticeably slower but will prevent the system from running out of memory.

*   `-t` or `--threads`: By default, SpriteShrink will use all available CPU cores to work as fast as possible. You can use this flag to limit the number of threads if you need to use your computer for other tasks while it's running (e.g., `-t 4`).

* `-w <size>` or `--window <size>`: This flag allows you to manually set the window size for the content-defined chunking algorithm (e.g., 2kb, 4kb). The window
size influences the average size of the data chunks. Smaller chunks can lead to better deduplication on files with small, repetitive segments, while larger chunks can be faster to process. This is automatically handled by --auto-tune, but manual control is available for fine-tuning. The recommended default is 2kb.

* `-d <size>` or `--dictionary <size>`: Manually sets the size of the compression dictionary (e.g., 16kb, 32kb). The dictionary is a small file, generated from samples of your input data, this helps the Zstandard compression algorithm used by SpriteShrink to achieve much better compression ratios. A larger dictionary can significantly improve compression for datasets with a lot of repetitive patterns, but it will increase memory usage and the initial training time. The recommended default is 16kb.

* `-b <64|128>` or `--hash-bit-length <64|128>`: By default, SpriteShrink uses a 64-bit hash to identify unique data chunks. While the chance of two different chunks producing the same hash (a "collision") is astronomically low, it is not zero. This flag allows you to increase the hash size to 128 bits. This makes collisions even less likely, at the cost of a minor increase in memory usage, CPU time and final archive size. It is recommended to use this option if the hash verification step fails.

* `--autotune-timeout <seconds>`: When using --auto-tune, SpriteShrink can sometimes spend a long time searching for the perfect parameters. This flag sets a time limit, in seconds, for each iteration of the tuning process. If an iteration hits the timeout, it will stop and use the best result it has found so far, ensuring the process finishes in a reasonable amount of time.

* `--ignore-config`: SpriteShrink can load settings from a configuration file for convenience. This flag forces the application to ignore that file and run with its default settings or the ones you provide on the command line. This is useful for scripting or when you want to ensure a clean, predictable run without any stored preferences interfering.

* `--disable-logging`: By default, SpriteShrink creates a SpriteShrink.log file to help with debugging. If you do not need this log file, you can use this flag to prevent it from being created. This is useful for keeping your working directory clean or when running the tool in environments where file writes are restricted.

### The Configuration File

For users who frequently use the same settings, SpriteShrink supports a configuration file to set your own defaults. This allows you to avoid typing the same flags (like `--compression-level` or `--threads`) every time you run the application.

When you run SpriteShrink, it looks for a configuration file and applies its settings automatically. Any flags you provide on the command line will always override the settings in the configuration file for that specific run.

The application will also automatically create a default configuration file for you the first time it is run.

#### Configuration File Location

The configuration file is named spriteshrink-config.toml and is stored in a standard location depending on your operating system:

* **Windows**: `C:\Users\<YourUser>\AppData\Roaming\spriteshrink\config\spriteshrink-config.toml`
* **macOS**: `/Users/<YourUser>/Library/Application Support/spriteshrink/spriteshrink-config.toml`
* **Linux**: `/home/<YourUser>/.config/spriteshrink/spriteshrink-config.toml`

The configuration file uses the TOML format, which is designed to be easy to read and edit. Below is an example file showing all the available options with comments explaining what they do. You can edit this file to change SpriteShrink's default behavior.


```toml
# SpriteShrink Configuration File
# -------------------------------
# Sets the numerical compression level.
# Default = 19
compression_level = 19

# Sets the hashing algorithm window size (e.g., "2KB", "4KB").
# Default = "2kb"
window_size = "2kb"

# Sets the zstd compression algorithm dictionary size.
# Default = "16kb"
dictionary_size = "16kb"

# Sets the hashing algorithm bit length.
# Default = 64
hash_bit_length = 64

# Enable (true) or disable (false) whether to auto tune the window and/or
# dictionary size.
# Default = false
auto_tune = false

# Sets the maximum time for each auto tune iteration to take. If exceeded take
# the latest result.
# Default = 15 seconds
autotune_timeout = 15

# Enable (true) or disable (false) whether to optimze the ztd compression 
# dictionary when it is generated. Be sure to disable when processing large 
# amounts of data (e.g. If a single ROM exceeds approximately 64 megabytes, 
# set to false) as this step can take a significant amount of time for negligible
# gain for large amounts of data.
# Default = false
optimize_dictionary = false

# Sets the maximum number of worker threads to use.
# 0 means use all available cores.
threads = 0

# Enable (true) or disable (false) low memory mode. Forces the use of a
# temporary cache instead of processing all files in memory even if the
# application determines there is enough memory to do so.
# Default = false
low_memory = false

# Enable (true) or disable (false) whether to print metadata info in
# json format.
# Default = false
json_output = false

# Whether to print anything to console. True will disable printing to console
# and false will enable.
# Default = false
quiet_output = false

# Specifies the amount of days the application will retain log files. Any log
# file found to be older than the retention period, upon the application being
# run, will be removed. A value of 0 will keep logs forever.
# Default = 7
log_retention_days = 7

# Sets the log file verbosity level when enabled.
# - error will only log errors.
# - warning includes warning messages and the above messages (errors)
# - info includes info messages and the above messages (errors and warnings)
# - debug includes debug messages and the above messages 
#   (errrors, warnings and info)
# - off disables the creation of a log file and won't write any messages to one
#   if one exists
# Default = "error"
log_level = "error"
```

---

## Helper Scripts

For processing an entire library of games, using the command line for each one can be tedious. To solve this, we provide helper scripts that can automate the process of finding and compressing game sets.

These scripts are located in the [`helper_scripts`](../helper_scripts/) directory. Please read the `README.md` file in that directory for detailed instructions on how to use them.

---

## Troubleshooting

*   **"File not found" or "The system cannot find the path specified"**: Double-check your `-i` and `-o` paths. Make sure there are no typos. Remember to use `./` for files in the current directory.

*   **"Permission Denied"**: Ensure you have the rights to read the input directory and write to the output directory. On Linux/macOS, you may need to use `chmod` to adjust permissions.

*   **Hash Verification Fails**: During compression, SpriteShrink verifies that its internal representation of the data is correct. If this fails, it may be due to a rare hash collision. You can try the compression again with `--hash-bit-length 128` to use a more robust (but slightly slower) hashing algorithm.

*   **Log Files**: SpriteShrink creates a detailed log file that can be very helpful for debugging persistent issues. If you encounter a crash or unexpected behavior, the contents of this log file provide valuable information. The log level can be configured in the [configuration file](#the-configuration-file) to provide more or less detail.

The log files are stored in a standard location for your operating system, inside a logs sub-directory:

* **Windows**: `C:\Users\<YourUser>\AppData\Local\spriteshrink\logs\`
* **macOS**: `/Users/<YourUser>/Library/Application Support/spriteshrink/logs/`
* **Linux**: `/home/<YourUser>/.local/share/spriteshrink/logs/`

The log files are rotated daily, with the base name being `debug.log.YYYY-MM-DD`. If you do not need this feature, you can turn it off completely by using the `--disable-logging` flag.

You can also control the verbosity of the log files by changing the log_level setting in the configuration file. Choosing the right level can help you get the exact information you need without overwhelming you with unnecessary details. The levels are hierarchical; each level includes all the messages from the levels above it.

Here is a guide to the available log levels and when to use them:

* `error` (Default): This is the default and most quiet setting. It will only record critical errors that prevent the application from performing a task, such as a corrupted archive or an invalid file path. You should use this for normal day-to-day operation.

* `warn`: This level includes all error messages, plus warnings. Warnings highlight potential issues that don't stop the application but might lead to  unexpected results, such as using a fallback setting because a value in the config was invalid.

* `info`: This level includes all error and warn messages, plus high-level informational messages about the application's progress. It will log major     events, like which mode (Compress, Extract, Info) is running and when it completes successfully. This is a good level to use if you want a general     overview of what the application is doing.

* `debug`: This is a highly verbose level that includes all of the above, plus detailed diagnostic information. It records granular operations, such as which specific file is being processed, the arguments being used, and other internal state data. This level is most useful for developers or when you are trying to pinpoint the exact cause of a complex bug.

* `off`: This setting completely disables file logging. The application will not create or write to any log files. This is equivalent to using the `--disable-logging` flag.

---

## License and Support

*   **License**: The SpriteShrink application is licensed under GPLv3, while the core library is under MPL-2.0. For specifics, please see the [License section in the README](../README.md#license).
*   **Support**: For bug reports and feature requests, please open an issue on our [GitHub Issues page](https://github.com/Zade222/SpriteShrink/issues). For general questions and discussion, please use the [GitHub Discussions page](https://github.com/Zade222/SpriteShrink/discussions).
