//! Runs the `sprite-shrink` binary end to end on inputs the baseline
//! archives don't cover.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sprite_shrink_test_support::{assert_bytes_eq, rom_set, tiled_bytes};
use tempfile::TempDir;

use crate::common::{cli_output, reported_error, run_cli};

/// Part of the note the CLI prints when it builds an archive without a
/// compression dictionary.
const NO_DICTIONARY_NOTE: &str =
    "too little distinct data to train a compression dictionary";

/// Runs the CLI, fails the test if it reports an error, and returns what it
/// printed to stdout.
fn stdout_of(args: &[&str]) -> String {
    let output = cli_output(args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !reported_error(&output),
        "sprite-shrink {args:?} failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// Extracts file 1 of `archive` into `out_dir`.
fn extract_first(archive: &Path, out_dir: &Path) {
    fs::create_dir_all(out_dir).unwrap();
    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1",
    ]);
}

#[test]
fn single_small_rom_is_archived_without_a_dictionary() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let out_dir = tmp.path().join("extract");
    let archive = tmp.path().join("small.ssmc");
    fs::create_dir_all(&input_dir).unwrap();

    // A 4 KiB ROM cuts into too few chunks to train a dictionary on.
    let rom = tiled_bytes(0x5EED_3001, 4 * 1024);
    fs::write(input_dir.join("small.bin"), &rom).unwrap();

    let stdout = stdout_of(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    assert!(stdout.contains(NO_DICTIONARY_NOTE), "stdout:\n{stdout}");

    extract_first(&archive, &out_dir);
    let extracted = fs::read(out_dir.join("small.bin")).unwrap();
    assert_bytes_eq(&extracted, &rom, "small.bin");
}

#[test]
fn blank_data_disc_is_archived_without_a_dictionary() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let out_dir = tmp.path().join("extract");
    let archive = tmp.path().join("blank.ssmd");
    fs::create_dir_all(&input_dir).unwrap();

    // Every sector is zero, so the data track dedups to almost nothing.
    let image = vec![0_u8; 300 * 2352];
    fs::write(input_dir.join("disc.bin"), &image).unwrap();
    fs::write(
        input_dir.join("disc.cue"),
        "FILE \"disc.bin\" BINARY\n\
         \x20 TRACK 01 MODE2/2352\n\
         \x20   INDEX 01 00:00:00\n",
    )
    .unwrap();

    let stdout = stdout_of(&[
        "--mode", "optical",
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    assert!(stdout.contains(NO_DICTIONARY_NOTE), "stdout:\n{stdout}");

    extract_first(&archive, &out_dir);
    let extracted = fs::read(out_dir.join("disc.bin")).unwrap();
    assert_bytes_eq(&extracted, &image, "disc.bin");
}

#[test]
fn rom_set_is_archived_with_a_dictionary() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let archive = tmp.path().join("roms.ssmc");
    fs::create_dir_all(&input_dir).unwrap();
    for (name, data) in rom_set() {
        fs::write(input_dir.join(name), data).unwrap();
    }

    let stdout = stdout_of(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    assert!(!stdout.contains(NO_DICTIONARY_NOTE), "stdout:\n{stdout}");
}

/// Copies the committed v2 SSMC fixture into `dir` with the byte at
/// `offset` set to `value`, and returns the copy's path.
fn patched_fixture(dir: &Path, offset: usize, value: u8) -> PathBuf {
    let mut bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/v2/roms.ssmc"),
    )
    .unwrap();
    bytes[offset] = value;

    let path = dir.join("patched.ssmc");
    fs::write(&path, &bytes).unwrap();
    path
}

/// Runs the CLI, expecting it to report an error, and returns everything it
/// printed.
fn failure_output(args: &[&str]) -> String {
    let output = cli_output(args);
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(reported_error(&output), "sprite-shrink {args:?}:\n{printed}");
    printed
}

#[test]
fn library_errors_are_reported_with_their_detail() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();
    let archive = patched_fixture(tmp.path(), 0, b'X');

    let printed = failure_output(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1",
    ]);

    // The library's own message, not only the stage that failed.
    assert!(
        printed.contains("File Magic Number is invalid"),
        "output:\n{printed}"
    );
}

#[test]
fn unknown_hash_type_stops_extraction_and_listing() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();
    // `hash_type` is the header byte at offset 18.
    let archive = patched_fixture(tmp.path(), 18, 3);
    let archive = archive.to_str().unwrap();

    let extract = ["-i", archive, "-o", out_dir.to_str().unwrap(), "-e", "1"];
    let list = ["-i", archive, "--list"];
    for args in [&extract[..], &list[..]] {
        let printed = failure_output(args);
        assert!(
            printed.contains("Unsupported hash type 3 in the archive header"),
            "sprite-shrink {args:?}:\n{printed}"
        );
    }
    assert!(fs::read_dir(&out_dir).unwrap().next().is_none());
}
