# SpriteShrink: Efficient ROM Deduplication and Archiving

![Rust](https://github.com/rust-lang/rust/actions/workflows/rust.yml/badge.svg)
SpriteShrink is a powerful command-line tool that revolutionizes how retro game ROM enthusiasts manage their collections. By employing advanced byte-level data deduplication across various ROM versions (e.g., regional variants, revisions), it achieves significantly higher compression ratios than conventional methods, drastically reducing storage space for your entire game library.

It also includes `lib_sprite_shrink`, a Rust library for seamless integration into applications like emulators, enabling direct ROM extraction from archives.

## ✨ Features

* **Intelligent Deduplication**: Analyzes multiple ROMs of a single game to identify and eliminate shared byte data, storing only unique copies.
* **High Compression Ratios**: Achieves superior space savings compared to traditional compression methods by leveraging content-defined chunking and shared dictionary compression (Zstandard).
* **Flexible Archiving**: Consolidates deduplicated ROM data into a single, highly compressed `.ssmc` archive file.
* **Metadata Inspection**: View a list of all ROMs contained within an `.ssmc` archive, along with their original filenames and indices.
* **Precise Extraction**: Extract one or more specific ROM versions from an `.ssmc` archive, recreating the original file byte-for-byte.
* **Developer-Friendly Library**: A dedicated Rust library (`lib_sprite_shrink`) exposes core functionalities for easy integration into third-party applications.
* **Cross-Platform Compatibility**: Fully operational on 64-bit Linux (Ubuntu LTS, RHEL), macOS (latest stable versions), and Windows (10/11) on x86-64 and AArch64 architectures.
* **Resource Control**: Options to limit CPU thread usage and enable a low-memory mode for resource-constrained environments.

## 🚀 Getting Started

### Installation

Pre-compiled binaries for supported platforms will be made available on the [releases page](https://github.com/Zadeis/sprite_shrink/releases) (link will be updated upon first release).

Alternatively, you can build SpriteShrink from source:

### Building from Source (for Developers)

To build SpriteShrink from source, you will need the [Rust toolchain](https://www.rust-lang.org/tools/install) installed.

1.  **Clone the repository:**

    ```bash
    git clone [https://github.com/your-username/sprite-shrink.git](https://github.com/your-username/sprite-shrink.git)
    cd sprite-shrink
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

### Usage

SpriteShrink is a command-line interface (CLI) tool. All interactions are done via flags and arguments.

#### Basic Commands

* **Compress ROMs**:
    Compress a directory of ROMs into a new `.ssmc` archive:

    ```bash
    ./sprite_shrink -i ./path/to/roms/directory -o ./output/my_game_collection.ssmc
    ```

    You can specify multiple input files or directories:

    ```bash
    ./sprite_shrink -i ./roms/game_a/ -i ./roms/game_b/rom_file.nes -o ./output/combined_collection.ssmc
    ```

* **Extract ROMs**:
    First, list the contents of an `.ssmc` archive to find the ROM indices:

    ```bash
    ./sprite_shrink -m ./output/my_game_collection.ssmc
    ```

    Example Output:

    ```
    Index   | Filename
    ------------------
    1       | game_us.nes
    2       | game_jp.nes
    3       | game_rev1.nes
    ```

    Then, extract specific ROMs by their index (e.g., extract index 1 and 3):

    ```bash
    ./sprite_shrink -i ./output/my_game_collection.ssmc -o ./extracted_roms/ -e 1,3
    ```

    You can also specify a range (e.g., extract ROMs from index 1 to 5):

    ```bash
    ./sprite_shrink -i ./output/my_game_collection.ssmc -o ./extracted_roms/ -e 1-5
    ```

* **View Archive Metadata**:
    Display the filenames and indices of all ROMs within an `.ssmc` archive:

    ```bash
    ./sprite_shrink -m ./output/my_game_collection.ssmc
    ```

#### Command-Line Arguments

Here's a quick reference for the main arguments:

| Short Flag | Long Flag         | Description                                                                                             | Default     |
|------------|-------------------|---------------------------------------------------------------------------------------------------------|-------------|
| `-i`       | `--input`         | Path to a single ROM file or directory of ROMs for input. Can be specified multiple times.              | Required    |
| `-o`       | `--output`        | Path for the output archive (compression) or directory (extraction).                                    | Required    |
| `-m`       | `--metadata`      | Activates metadata retrieval mode. Used with `-i` (input archive).                                      | N/A         |
| `-e`       | `--extract`       | Specifies ROM index(s) to extract (e.g., `1,3,5-7`). Requires `-o` and `-i`.                            | N/A         |
| `-c`       | `--compression`   | Sets the compression algorithm (choices: `zstd`, `lzma`, `deflate`).                                    | `zstd`      |
|            | `--compression-level` | Sets the numerical compression level.                                                                   | `19`        |
| `-w`       | `--window`        | Hashing algorithm window size (e.g., `256K`, `4KB`).                                                    | `2KB`       |
| `-d`       | `--dictionary`    | Compression algorithm dictionary size (e.g., `256K`, `4KB`).                                            | `16KB`      |
| `-a`       | `--auto-tune`     | Autotune window and dictionary sizes for reasonable optimality. Overrides manual settings.                | `false`     |
| `-t`       | `--threads`       | Sets max worker threads. Defaults to all available logical cores.                                       | All cores   |
|            | `--low-memory`    | Forces low-memory mode (sequential processing, 4 threads for compression).                              | `false`     |
| `-f`       | `--force`         | Overwrites output file if it exists.                                                                    | `false`     |
| `-v`       | `--verbose`       | Activates verbose output for detailed diagnostic information.                                           | `false`     |
| `-q`       | `--quiet`         | Activates quiet mode, suppressing non-essential output.                                                 | `false`     |
| `-h`       | `--help`          | Prints comprehensive help information and exits.                                                        | N/A         |

## 🤝 Contributing

We welcome contributions to SpriteShrink! If you're interested in contributing, please refer to our [CONTRIBUTING.md](CONTRIBUTING.md) (link will be added once the file is created) for guidelines on setting up your development environment, coding standards, and submitting pull requests.

## 🐛 Error Handling & Reliability

SpriteShrink is designed to be robust. It gracefully handles malformed inputs, ensures data integrity (byte-for-byte identical extraction), and provides clear, actionable error messages. Operations are atomic, meaning partially written output files are automatically cleaned up in case of cancellation or failure.

## 📄 License

This project is open-source and available under the [GPLv3 License](https://www.gnu.org/licenses/gpl-3.0.en.html). Please see the `LICENSE` file for more details.

## 📞 Support and Contact

If you have any questions, encounter issues, or would like to provide feedback, please open an issue on the [GitHub Issues page](https://github.com/your-username/sprite-shrink/issues).
