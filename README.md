# SpriteShrink: Efficient ROM Deduplication and Archiving

![Rust](https://github.com/rust-lang/rust/actions/workflows/rust.yml/badge.svg)

SpriteShrink is a powerful command-line tool that revolutionizes how enthusiasts manage their collections of retro game ROMs and other versioned files. By employing advanced byte-level data deduplication across various file versions (e.g., regional variants, revisions), it achieves significantly higher compression ratios than conventional methods, drastically reducing storage space for your entire library.

It also includes `lib_sprite_shrink`, a Rust library for seamless integration into applications like emulators, enabling direct ROM extraction from archives.

---

## Features

* **Intelligent Deduplication**: Analyzes multiple files to identify and eliminate shared byte data, storing only unique copies. This is achieved using the **FastCDC** content-defined chunking algorithm.
* **High Compression Ratios**: Achieves superior space savings by leveraging shared dictionary compression with **Zstandard**.
* **Flexible Archiving**: Consolidates deduplicated data into a single, highly compressed `.ssmc` archive file.
* **Parameter Auto-Tuning**: Automatically tunes chunking and compression parameters to find an optimal balance between compression speed and file size.
* **Data Integrity**: Uses **SHA-512** hashing to verify that every extracted file is a byte-for-byte perfect copy of the original.
* **Metadata Inspection**: View a list of all files contained within an `.ssmc` archive, along with their original filenames and indices.
* **Precise Extraction**: Extract one or more specific files from an `.ssmc` archive by index.
* **Developer-Friendly Library**: A dedicated Rust library (`lib_sprite_shrink`) exposes core functionalities for easy integration into third-party applications.
* **Cross-Platform Compatibility**: Fully operational on 64-bit Linux, macOS, and Windows on x86-64 and AArch64 architectures.
* **Resource Control**: Options to limit CPU thread usage and enable a low-memory mode for resource-constrained environments.

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

Pre-compiled binaries for supported platforms will be made available on the [releases page](https://github.com/Zadeis/sprite_shrink/releases). I currently do not have access to a Mac based machine so I am currently only providing 64-bit x86-64 binaries for Linux and Windows. If you want to help with Mac efforts feel free to contribute!

Alternatively, you can build SpriteShrink from source.

### Building from Source (for Developers)

To build SpriteShrink from source, you will need the [Rust toolchain](https://www.rust-lang.org/tools/install) installed.

1.  **Clone the repository:**

    ```bash
    git clone [https://github.com/Zadeis/sprite_shrink.git](https://github.com/Zadeis/sprite_shrink.git)
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

| Short Flag | Long Flag | Description | Default |
| :--- | :--- | :--- | :--- |
| `-i` | `--input` | Path to a file or directory for input. Can be specified multiple times. | Required |
| `-o` | `--output` | Path for the output archive (compression) or directory (extraction). | Mode Dependent |
| `-m` | `--metadata` | Activates metadata retrieval mode. Used with `-i` (input archive). | N/A |
| `-e` | `--extract` | Specifies file indices to extract (e.g., `1,3,5-7`). Requires `-o` and `-i`. | N/A |
| `-c` | `--compression` | Sets the compression algorithm. | `zstd` |
| | `--compression-level` | Sets the numerical compression level for the chosen algorithm. | `19` |
| `-w` | `--window` | Hashing algorithm window size (e.g., "2KB", "4KB"). Recommended: 2KB. | `2KB` |
| `-d` | `--dictionary` | Compression algorithm dictionary size (e.g., "16KB", "32KB"). Recommended: 16KB. | `16KB` |
| `-a` | `--auto-tune` | Autotune window and dictionary sizes for optimal size. Overrides manual settings. | `false` |
| | `--autotune-timeout` | Sets the maximum time in seconds for each autotune iteration. | N/A |
| | `--optimize-dictionary` | When finalizing the archive, optimize the dictionary for better compression. **Very slow**. | `false` |
| `-t` | `--threads` | Sets max worker threads. Defaults to all available logical cores. | All cores |
| | `--low-memory` | Forces low-memory mode (sequential I/O, 4 threads for compression). | `false` |
| `-f` | `--force` | Overwrites the output file if it exists. | `false` |
| `-v` | `--verbose` | Activates verbose output for detailed diagnostic information. | `false` |
| `-q` | `--quiet` | Activates quiet mode, suppressing non-essential output. | `false` |
| `-h` | `--help` | Prints comprehensive help information and exits. | N/A |

---

## Error Handling & Reliability

SpriteShrink is designed to be robust. It gracefully handles malformed inputs, ensures data integrity (byte-for-byte identical extraction), and provides clear, actionable error messages. Operations are atomic, meaning partially written output files are automatically cleaned up in case of cancellation or failure.

---

## License

This project is open-source and available under the [GPLv3 License](https://www.gnu.org/licenses/gpl-3.0.en.html). Please see the `LICENSE` file for more details.