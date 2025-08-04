#!/bin/bash

# --- ROM Archiver Script ---
#
# This script recursively finds game directories within a specified input path
# and creates compressed archives of each game's files in an output directory.
#
# Directory Structure Expectation:
# Input:  .../System Manufacturer/System Name/Game Name/...rom files...
# Output: .../System Manufacturer/System Name/Game Name.ssmc
#
# Usage:
# ./archive_roms.sh /path/to/your/roms /path/to/your/archives
#

# --- Configuration ---

# Path to the sprite-shrink executable.
# Change this if the executable is not in your system's PATH.
SPRITE_SHRINK_EXEC="sprite-shrink"

# --- Script Start ---

# Check for the correct number of arguments
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "Usage: $0 <input_directory> <output_directory> '[additional_flags]'"
    exit 1
fi

INPUT_ROOT="$1"
OUTPUT_ROOT="$2"
EXTRA_FLAGS="$3" # Optional third argument for extra flags

# Check if the input directory exists
if [ ! -d "$INPUT_ROOT" ]; then
    echo "Error: Input directory '$INPUT_ROOT' not found."
    exit 1
fi

# Create the output directory if it doesn't exist
mkdir -p "$OUTPUT_ROOT"

echo "Starting ROM archival process..."
echo "Input directory: $INPUT_ROOT"
echo "Output directory: $OUTPUT_ROOT"
if [ -n "$EXTRA_FLAGS" ]; then
    echo "Additional flags: $EXTRA_FLAGS"
fi
echo "---"

# Find all directories that are 3 levels deep from the input root.
# This corresponds to the "Game" directories.
# -mindepth 3 and -maxdepth 3 ensure we only get the game folders.
find "$INPUT_ROOT" -mindepth 3 -maxdepth 3 -type d | while IFS= read -r game_dir; do

    # Get the relative path from the input root
    relative_path="${game_dir#$INPUT_ROOT/}"

    # Extract the System Manufacturer, System Name, and Game Name
    manufacturer=$(echo "$relative_path" | cut -d'/' -f1)
    system_name=$(echo "$relative_path" | cut -d'/' -f2)
    game_name=$(echo "$relative_path" | cut -d'/' -f3)

    echo "Processing Game: $game_name"
    echo "  System: $system_name"
    echo "  Manufacturer: $manufacturer"

    # Define the output directory for the current system
    output_system_dir="$OUTPUT_ROOT/$manufacturer/$system_name"

    # Create the output directory structure
    mkdir -p "$output_system_dir"

    # Define the final output archive file path
    output_archive_file="$output_system_dir/$game_name.ssmc"

    echo "  Input files from: $game_dir"
    echo "  Output archive to: $output_archive_file"

    # Check if the archive already exists. Use --force if you want to overwrite.
    if [ -f "$output_archive_file" ]; then
        echo "  Archive already exists. Skipping."
    else
        # Run the compression command
        # The -i flag takes the game directory as input.
        # The -o flag specifies the output archive file.
        # Using --quiet to reduce console output, remove for more verbosity.
        "$SPRITE_SHRINK_EXEC" -i "$game_dir" -o "$output_archive_file" --quiet

        if [ $? -eq 0 ]; then
            echo "  Successfully created archive for $game_name."
        else
            echo "  Error creating archive for $game_name."
        fi
    fi
    echo "---"

done

echo "Archival process completed."