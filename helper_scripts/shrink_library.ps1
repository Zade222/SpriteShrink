<#
.SYNOPSIS
    A PowerShell script to recursively find game directories and create compressed
    archives using the sprite_shrink executable.

.DESCRIPTION
    This script searches an input directory for game folders located three levels
    deep. For each game folder found, it creates a compressed .ssmc archive in a
    corresponding output directory structure.

    Expected Input Directory Structure:
    ...\System Manufacturer\System Name\Game Name\...rom files...

    Resulting Output Directory Structure:
    ...\System Manufacturer\System Name\Game Name.ssmc

.PARAMETER InputRoot
    The root directory containing your ROMs, structured by manufacturer and system.

.PARAMETER OutputRoot
    The root directory where the compressed .ssmc archives will be saved.

.PARAMETER ExtraFlags
    An optional string of additional flags to pass to the sprite_shrink executable.

.EXAMPLE
    .\shrink_library.ps1 -InputRoot "D:\ROMs" -OutputRoot "D:\Archives"

.EXAMPLE
    .\shrink_library.ps1 -InputRoot "D:\ROMs" -OutputRoot "D:\Archives" -ExtraFlags "--force --verbose"
#>
param(
    [Parameter(Mandatory=$true)]
    [string]$InputRoot,

    [Parameter(Mandatory=$true)]
    [string]$OutputRoot,

    [string]$ExtraFlags
)

# --- Configuration ---

# Path to the sprite_shrink executable.
# Assumes 'sprite_shrink.exe' is in the system's PATH.
# If not, provide the full path: e.g., "C:\Tools\sprite_shrink.exe"
$spriteShrinkExec = "sprite_shrink.exe"

# --- Script Start ---

# Resolve to absolute paths for consistency
$InputRoot = Resolve-Path -Path $InputRoot
$OutputRoot = Resolve-Path -Path $OutputRoot -ErrorAction SilentlyContinue

# Check if the input directory exists
if (-not (Test-Path -Path $InputRoot -PathType Container)) {
    Write-Error "Error: Input directory '$InputRoot' not found."
    exit 1
}

# Create the output directory if it doesn't exist
if (-not $OutputRoot) {
    $null = New-Item -Path $OutputRoot -ItemType Directory
}

Write-Host "Starting ROM archival process..."
Write-Host "Input directory: $InputRoot"
Write-Host "Output directory: $OutputRoot"
if (-not [string]::IsNullOrEmpty($ExtraFlags)) {
    Write-Host "Additional flags: $ExtraFlags"
}
Write-Host "---"

# Get all directories that are exactly 3 levels deep from the input root.
# The `Get-ChildItem` depth parameter is 0-indexed, so a depth of 2 gives us 3 levels.
$gameDirectories = Get-ChildItem -Path $InputRoot -Recurse -Depth 2 -Directory | Where-Object {
    # Ensure we are exactly 3 levels deep
    $_.FullName.Split([System.IO.Path]::DirectorySeparatorChar).Count -eq $InputRoot.Split([System.IO.Path]::DirectorySeparatorChar).Count + 3
}


foreach ($gameDir in $gameDirectories) {
    try {
        # Get the relative path from the input root
        $relativePath = $gameDir.FullName.Substring($InputRoot.Length + 1)

        # Split the relative path to get the manufacturer, system, and game name
        $pathParts = $relativePath.Split([System.IO.Path]::DirectorySeparatorChar)
        $manufacturer = $pathParts[0]
        $systemName = $pathParts[1]
        $gameName = $pathParts[2]

        Write-Host "Processing Game: $gameName"
        Write-Host "  System: $systemName"
        Write-Host "  Manufacturer: $manufacturer"

        # Define the output directory for the current system
        $outputSystemDir = Join-Path -Path $OutputRoot -ChildPath (Join-Path -Path $manufacturer -ChildPath $systemName)

        # Create the output directory structure if it doesn't exist
        if (-not (Test-Path -Path $outputSystemDir -PathType Container)) {
            $null = New-Item -Path $outputSystemDir -ItemType Directory
        }

        # Define the final output archive file path
        $outputArchiveFile = Join-Path -Path $outputSystemDir -ChildPath "$gameName.ssmc"

        Write-Host "  Input files from: $($gameDir.FullName)"
        Write-Host "  Output archive to: $outputArchiveFile"

        # Check if the archive already exists.
        if (Test-Path -Path $outputArchiveFile -PathType Leaf) {
            # The --force flag would handle overwriting, so we only skip if it's not present.
            if ($ExtraFlags -notlike '*--force*') {
                 Write-Host "  Archive already exists. Skipping."
                 continue
            }
        }

        # Construct the arguments for the executable
        $arguments = @(
            "-i", "`"$($gameDir.FullName)`"",
            "-o", "`"$outputArchiveFile`"",
            "--quiet"
        )

        # Add extra flags if they were provided
        if (-not [string]::IsNullOrEmpty($ExtraFlags)) {
            $arguments += $ExtraFlags.Split(' ')
        }

        # Run the compression command
        & $spriteShrinkExec $arguments

        if ($LASTEXITCODE -eq 0) {
            Write-Host "  Successfully created archive for $gameName."
        } else {
            Write-Host "  Error creating archive for $gameName. (Exit Code: $LASTEXITCODE)"
        }
    }
    catch {
        Write-Error "An unexpected error occurred while processing '$($gameDir.FullName)': $_"
    }
    finally {
        Write-Host "---"
    }
}

Write-Host "Archival process completed."
