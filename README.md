# SpriteShrink: Efficient ROM Deduplication and Archiving

![Rust](https://github.com/rust-lang/rust/actions/workflows/rust.yml/badge.svg)

SpriteShrink is a powerful command-line tool that helps enthusiasts manage their collections of retro game ROMs. By employing data deduplication across various file versions (e.g., regional variants, revisions), it achieves higher compression ratios than conventional methods, reducing storage for each multi variant game.

It also includes `lib_sprite_shrink`, a Rust library for integration into applications like emulators or frontends, enabling direct ROM extraction from archives.

---

## Features

* **Intelligent Deduplication**: Analyzes multiple files to identify and eliminate shared byte data, storing only unique copies. This is achieved using the **FastCDC** content-defined chunking algorithm.
* **High Compression Ratios**: Achieves superior space savings by leveraging shared dictionary compression with **Zstandard** in addition tot he deduplication.
* **Flexible Archiving**: Consolidates deduplicated data into a single, highly compressed `.ssmc` archive file.
* **Parameter Auto-Tuning**: Automatically tunes chunking and compression parameters to find an optimal balance between compression speed and file size.
* **Data Integrity**: Internally uses **SHA-512** hashing to verify that every extracted file is a byte-for-byte perfect copy of the original. Upon decompression it will also verify each chunk of data to ensure no corruption occurred since the creation of the archive.
* **Metadata Inspection**: View a list of all files contained within an `.ssmc` archive, along with their original filenames, sizes and indices.
* **Precise Extraction**: Extract one or more specific files from an `.ssmc` archive by index.
* **Developer-Friendly Library**: A dedicated Rust library (`lib_sprite_shrink`) exposes core functionalities for easy integration into third-party applications.
* **Cross-Platform Compatibility**: Fully operational on 64-bit Linux, macOS, and Windows on x86-64 and AArch64 architectures.
* **Resource Control**: Options to limit CPU thread usage and enable a low-memory mode for resource-constrained environments.

---

## Intended Use Cases and Limitations

SpriteShrink is specifically designed for collections of files that are relatively small and share portions of data. The ideal use case is managing variant versions of a single game in your library or multi-disc games that contain duplicate data.

While it is technically capable of processing any file type, it is **not recommended** for:
* **Files with Little Redundancy**: Collections of files that do not share common data (e.g., a directory of unrelated documents or videos).
* **Files with Little Redundancy**: Games that internally use their own compression or contain a large amount of compressed video data.

The tool's content-defined chunking and deduplication algorithms provide the most benefit when byte-level similarities exist across multiple files, which is common in ROM sets with different regional versions and revisions.

**However**, given the aforementioned limitations there are circumstances where it can work well. Systems that have games that span multiple discs being one of them. I have tested it specifically on my copy of Final Fantasy 7 which is three discs. Despite being one contiguous game across the three discs, there is a fair amount of assets used across all the discs that are duplicated should the player return to earlier parts of the game. Despite each disk technically being overall different this still leads to an approximate compressed:original size ratio of 1:2 when I compressed my disc images using SpriteShrink. So in the end the SpriteShrink size is smaller than what CHD with cdzs or a tar file using zstd at compression level 19 could manage.

---

## The .ssmc File Format

SpriteShrink uses a custom archive format called **SpriteShrink Multi-Cart (`.ssmc`)**. This format is specifically designed for the efficient storage of file collections with a high degree of redundant data, such as ROM sets with multiple regional versions or revisions.

By using content-defined chunking and deduplication, the `.ssmc` format stores each unique piece of data only once, leading to significant storage savings compared to traditional compression tools.

A complete technical document detailing the binary layout, data structures, and design principles of the format is available for developers and enthusiasts.

[**Read the full .ssmc File Specification**](./SSMC_File_Specification/SPECIFICATION.md)

---

## How It Works

The compression process follows a multi-stage pipeline to ensure maximum efficiency:

1.  **Chunking**: Input files are broken down into variable-sized chunks using the **FastCDC** content-defined chunking algorithm. This method ensures that identical segments of data produce identical chunks, regardless of their position in a file.
2.  **Deduplication**: Each chunk is hashed using **xxh3**, and only one copy of each unique chunk is stored in a `data_store`. This significantly reduces redundancy across multiple files.
3.  **Verification**: A **SHA-512** hash is generated for each input file to ensure data integrity during reconstruction.
4.  **Dictionary Training**: A compression dictionary is generated from samples of the unique data chunks. This dictionary helps the Zstandard compression algorithm achieve better ratios on the specific data being archived.
5.  **Compression**: The unique data chunks are compressed in parallel using the generated dictionary.
6.  **Archiving**: The compressed chunks, file manifest, and header are serialized and written to a final `.ssmc` archive file.

---

## Getting Started

### Installation

Pre-compiled binaries for supported platforms will be made available on the [releases page](https://github.com/Zade222/SpriteShrink/releases). I currently do not have access to a Mac based machine so I am currently only providing 64-bit x86-64 binaries for Linux and Windows. If you want to help with Mac efforts feel free to contribute!

Alternatively, you can build SpriteShrink from source.

### Building from Source (for Developers)

To build SpriteShrink from source, you will need the [Rust toolchain](https://www.rust-lang.org/tools/install) installed.

1.  **Clone the repository:**

    ```bash
    git clone https://github.com/Zade222/SpriteShrink.git
    cd sprite_shrink
    ```

2.  **Install OS-specific build tools:**

    * **Linux/Unix-like systems (including WSL)**: `build-essential` (Debian/Ubuntu), "Development Tools" group (RHEL-based), or `base-devel` (Arch Linux).
    * **macOS**: Xcode Command Line Tools (`xcode-select --install`).
    * **Windows**: Microsoft Visual C++ Build Tools (available as part of Visual Studio or standalone).

3.  **Build the project:**

    ```bash
    cargo build --release
    ```

    The executable will be located in `target/release/sprite_shrink` (or `sprite_shrink.exe` on Windows).

---

### Cross Compiling for Specific Hardware

1.  **Clone the repository:**

    ```bash
    git clone https://github.com/Zade222/SpriteShrink.git
    cd sprite_shrink
    ```

2.  **Install OS-specific build tools:**

    * **Linux/Unix-like systems (including WSL)**: `build-essential` (Debian/Ubuntu), "Development Tools" group (RHEL-based), or `base-devel` (Arch Linux).
    * **macOS**: Xcode Command Line Tools (`xcode-select --install`).
    * **Windows**: Microsoft Visual C++ Build Tools (available as part of Visual Studio or standalone).

3. **Download and prepare platform specific toolchain**
    This changes from platform to platform. Please consult the toolchain documentation you intend to use to have everything prepared.

4. **Add Rustup Target**
    Depending on the hardware of your target this will differ.
    For armv7 linux it's as follows:

    ```bash
    rustup target add armv7-unknown-linux-gnueabihf
    ```

    For a 64-bit ARM target for Linux:

    ```bash
    rustup target add aarch64-unknown-linux-gnu
    ```

5.  **Build the project:**
    The linker rust flag will depend on where you have the linker stored. Specify the path to the gcc binary. Normally this will not need to be explicitly set expect for unique targets. For example for the RG35XX Plus/H/2024 Toolchain that will be as follows:

    # Generic Template
    ```RUSTFLAGS="-C linker=/path/to/your/toolchain/bin/target-gcc" cargo build --target <your-target-triple> --release```

    # Example for RG35XX Plus/H
    ```RUSTFLAGS="-C linker=aarch64-buildroot-linux-gnu_sdk-buildroot/bin/aarch64-buildroot-linux-gnu-gcc" cargo build --target aarch64-unknown-linux-gnu --release```

---

## Usage

SpriteShrink is a command-line interface (CLI) tool. All interactions are done via flags and arguments.

### Basic Commands

* **Compress Files**:
    Compress a directory of files into a new `.ssmc` archive:

    ```bash
    ./sprite_shrink -i ./path/to/files/directory -o ./output/my_collection.ssmc
    ```

* **Extract Files**:
    First, list the contents of an `.ssmc` archive to find the file indices:

    ```bash
    ./sprite_shrink -i ./output/my_collection.ssmc -m
    ```

    Then, extract specific files by their index (e.g., extract index 1 and 3):

    ```bash
    ./sprite_shrink -i ./output/my_collection.ssmc -o ./extracted_files/ -e 1,3
    ```

---

## Command-Line Arguments

Here's a quick reference for the main arguments:

| Short Flag | Long Flag             | Description                                                                              | Default       |
| :---       | :---                  | :---                                                                                     | :---          |
| `-i`       | `--input`             | Path to a file or directory for input. Can be specified multiple times.                  | Required      |
| `-o`       | `--output`            | Path for the output archive (compression) or directory (extraction).                     | Mode Dependent|
| `-m`       | `--metadata`          | Activates metadata retrieval mode. Used with `-i` (input archive).                       | N/A           |
| `-j`       | `--json`              | Activates json metadata mode. The metadata output will be printed in json format.        | `false`       |
| `-e`       | `--extract`           | Specifies file indices to extract (e.g., `1,3,5-7`). Requires `-o` and `-i`.             | N/A           |
|            | `--compression-level` | Sets the numerical compression level for the zstd algorithm.                             | `19`          |
| `-w`       | `--window`            | Hashing algorithm window size (e.g., "2KB", "4KB"). Recommended: 2KB.                    | `2KB`         |
| `-d`       | `--dictionary`        | Compression algorithm dictionary size (e.g., "16KB", "32KB"). Recommended: 16KB.         | `16KB`        |
| `-b`       | `--hash-bit-length`   | Sets the hashing algorithm bit length. Default value is 64. If the hash verification stage fails try setting to 128. | `false`       |
|            | `--auto-tune`         | Autotune window and dictionary sizes for optimal size. Overrides manual settings.        | `false`       |
|            | `--autotune-timeout`  | Sets the maximum time in seconds for each autotune iteration.                            | `None`        |
|            | `--optimize-dictionary`| When finalizing the archive, optimize the dictionary for better compression. Can be **Very slow** depending on dictionary size. | `false`       |
| `-t`       | `--threads`           | Sets max worker threads. Defaults to all available logical cores.                        | All cores     |
|            | `ignore-config`       | Forces application to not read it's config and won't create one if it doesn't already exist. | `false`       |
|            | `--low-memory`        | Forces low-memory mode by using an on disk cache for temporarily storing data. Can adversely effect application performance. | `false`       |
| `-f`       | `--force`             | Overwrites the output file if it exists creates the output directory if it doesn't exist. | `false`       |
| `-p`       | `--progress`          | Activates printing progress to console.                                                  | `false`       |
| `-q`       | `--quiet`             | Activates quiet mode, suppressing non-essential output.                                  | `false`       |
| `-h`       | `--help`              | Prints comprehensive help information and exits.                                         | N/A           |
|            | `--disable-logging`   | Disables logging messages to log file.                                                   | `false`       |

---

## Helper Scripts

Bash and PowerShell scripts have been made for processing a whole library are also included in this repository. Their use is encouraged.

The guide and the scripts can be found [here](./helper_scripts/):

---

## Error Handling & Reliability

SpriteShrink is designed to be robust. It gracefully handles malformed inputs, ensures data integrity (byte-for-byte identical extraction), and provides clear, actionable error messages. Operations are atomic, meaning partially written output files are automatically cleaned up in case of cancellation or failure.

---

## License

This project utilizes multiple licenses for its different components. Please ensure you understand and comply with the license for any part of the project you use.

-   **Cli Application (`sprite_shrink`)** is licensed under the **[GNU General Public License v3.0 (GPLv3)](https://www.gnu.org/licenses/gpl-3.0.en.html)**.
    This is a copyleft license, which means that any derivative works of the application must also be licensed under the GPLv3.

-   **Sprite Shrink Library (`lib_sprite_shrink`)** is licensed under the **[Mozilla Public License 2.0 (MPL-2.0)](https://www.mozilla.org/en-US/MPL/2.0/)**. This is a "weak copyleft" license. It requires that modifications to the library's source files be shared under the MPL-2.0, but it allows you to link the library into a larger work under a different license.

-   **SSMC Specification (`SPECIFICATION.md`)** is licensed under the **[Creative Commons Attribution 4.0 International (CC BY 4.0)](https://creativecommons.org/licenses/by/4.0/)**. This license allows you to share and adapt the specification for any purpose, even commercially, as long as you give appropriate credit.

## Support and Contact

If you encounter issues or would like to provide feedback, please open an issue on the [GitHub Issues page](https://github.com/Zade222/SpriteShrink/issues). Please understand that the issue tracker isn't a forum. It is for tracking feature requests, problems, issues, etc.

If you want to discuss the software and it's capabilities with others please use the [GitHub Discussions page](https://github.com/Zade222/SpriteShrink/discussions).
