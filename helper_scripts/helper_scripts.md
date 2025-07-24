# Guide: Using the SpriteShrink Archival Scripts

This guide explains how to use the `shrink_library.ps1` (for Windows PowerShell) and `shrink_library.sh` (for Linux/macOS) scripts. These scripts automate the process of compressing entire ROM libraries using the `SpriteShrink` executable.

***

## 1. Overview

The `SpriteShrink` tool is designed to efficiently deduplicate and compress collections of files, such as ROMs for a single game that may have multiple versions (e.g., USA, Europe, Japan, Revision A).

The provided scripts, `shrink_library.ps1` and `shrink_library.sh`, are helpers that automatically scan a library of ROMs, identify individual game folders, and run the `sprite_shrink` command on each one to create a compressed `.ssmc` archive.

***

## 2. Prerequisites

Before using the scripts, you must have the following:

1.  **`sprite_shrink` Executable**: The main compression tool. You must have it compiled or downloaded from the project's releases page.
2.  **Executable in PATH (Recommended)**: For ease of use, place the `sprite_shrink` (or `sprite_shrink.exe` on Windows) executable in a directory that is part of your system's `PATH` environment variable. If you don't do this, you will need to edit the scripts to provide the full path to the executable.
3.  **Correct Directory Structure**: The scripts are designed to work with a specific folder layout for your ROM library.

***

## 3. Required Directory Structure

Your ROM library **must** be organized in the following three-level directory structure:

```
/path/to/your/roms/
├── [System Manufacturer]/
│   └── [System Name]/
│       ├── [Game Name 1]/
│       │   ├── rom_file_1.n64
│       │   ├── rom_file_2.n64
│       │   └── ...
│       └── [Game Name 2]/
│           ├── game_2_rev_A.sfc
│           └── game_2_rev_B.sfc
└── ...
```

### Example:

```
D:\ROMs\
├── Nintendo\
│   └── Nintendo 64\
│       ├── The Legend of Zelda - Ocarina of Time\
│       │   ├── Zelda - Ocarina of Time (USA).z64
│       │   └── Zelda - Ocarina of Time (Europe).z64
│       └── Super Mario 64\
│           └── Super Mario 64 (USA).z64
└── Sega\
    └── Genesis\
        └── Sonic the Hedgehog 2\
            ├── Sonic 2 (USA, Europe).md
            └── Sonic 2 (Japan).md
```

The script will scan this structure, find each "Game Name" folder, and compress its contents into a single `.ssmc` file.


If you want the resulting archive to be named `Zelda - Ocarina of Time.ssmc` and not `Zelda - Ocarina of Time (USA) (En,Fr,De,Ch) (Rev 1).ssmc` do not include any text in parenthesis or brackets as the folder name. Just name the folder so that you will recognize the archive name and what game contained within it.

***

## 4. How to Use the Scripts

Choose the script that matches your operating system.

### A. `shrink_library.ps1` (Windows PowerShell)

This script is for use in a PowerShell terminal on Windows.

#### **Syntax:**

```powershell
.\shrink_library.ps1 -InputRoot <path_to_roms> -OutputRoot <path_to_archives> [-ExtraFlags "<flags>"]
```

#### **Parameters:**

* `-InputRoot <path_to_roms>`: (Required) The full path to the root of your ROM library (e.g., `D:\ROMs`).
* `-OutputRoot <path_to_archives>`: (Required) The path where the compressed `.ssmc` files will be saved. The script will replicate the `Manufacturer/System` folder structure here.
* `-ExtraFlags "<flags>"`: (Optional) A string containing any additional flags you want to pass to the `sprite_shrink.exe` command. For example, `--force` to overwrite existing archives or `--verbose` for detailed logs.

#### **Example Usage:**

1.  **Basic Compression:**
    Open PowerShell, navigate to the directory containing the script, and run:

    ```powershell
    .\shrink_library.ps1 -InputRoot "D:\ROMs" -OutputRoot "D:\Compressed ROMs"
    ```

2.  **Compression with Extra Flags:**
    To overwrite any existing archives and get detailed output:

    ```powershell
    .\shrink_library.ps1 -InputRoot "D:\ROMs" -OutputRoot "D:\Compressed ROMs" -ExtraFlags "--force --verbose"
    ```

### B. `shrink_library.sh` (Linux & macOS) 

This script is for use in a Bash-compatible shell on Linux or macOS.

#### **Syntax:**

```bash
./shrink_library.sh <input_directory> <output_directory> ['[additional_flags]']
```

#### **Parameters:**

* `<input_directory>`: (Required) The full path to the root of your ROM library (e.g., `/home/user/roms`).
* `<output_directory>`: (Required) The path where the compressed `.ssmc` files will be saved. The script will replicate the `Manufacturer/System` folder structure here.
* `'[additional_flags]'`: (Optional) A single string containing any extra flags for the `sprite_shrink` command. Note the single quotes to ensure the flags are passed correctly.

#### **Example Usage:**

1.  **Make the script executable (only needs to be done once):**

    ```bash
    chmod +x ./shrink_library.sh
    ```

2.  **Basic Compression:**
    Run the script from your terminal:

    ```bash
    ./shrink_library.sh /home/user/roms /home/user/compressed_roms
    ```

3.  **Compression with Extra Flags:**
    To force overwriting existing archives:

    ```bash
    ./shrink_library.sh /home/user/roms /home/user/compressed_roms '---auto-tune --force'
    ```

***

## 5. Output

After running, the script will create a new directory structure in your specified output path. For each game folder it processed, you will find a corresponding `.ssmc` archive.

### Example Output Structure:

If your input was `D:\ROMs`, your output in `D:\Compressed ROMs` would look like this:

```
D:\Compressed ROMs\
├── Nintendo\
│   └── Nintendo 64\
│       ├── The Legend of Zelda - Ocarina of Time.ssmc
│       └── Super Mario 64.ssmc
└── Sega\
    └── Genesis\
        └── Sonic the Hedgehog 2.ssmc
