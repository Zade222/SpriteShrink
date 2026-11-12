//! Runs the `sprite-shrink` binary end to end on inputs the baseline
//! archives don't cover.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sprite_shrink_cd::{SectorType, rebuild_sector_simd};
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
fn a_damaged_chunk_stops_extraction() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();

    // The last chunk in the fixture ends at the end of the file. A flipped
    // bit 140 bytes from the end still decodes, to the wrong bytes.
    let fixture = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/v2/roms.ssmc"),
    )
    .unwrap();
    let offset = fixture.len() - 140;
    let archive = patched_fixture(tmp.path(), offset, fixture[offset] ^ 1);

    let printed = failure_output(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1-3",
    ]);
    assert!(
        printed.contains("Chunk failed its integrity check"),
        "output:\n{printed}"
    );

    // The file being written is removed, not left half written.
    let left: Vec<_> = fs::read_dir(&out_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().ends_with(".tmp"))
        .collect();
    assert_eq!(left, Vec::<std::ffi::OsString>::new());
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

#[test]
fn unknown_format_id_is_reported_for_every_read_mode() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();
    // `format_id` is the little-endian u16 at header bytes 20..22. The hash
    // type byte is left valid, so a message naming it would be wrong.
    let archive = patched_fixture(tmp.path(), 20, 7);
    let archive = archive.to_str().unwrap();

    let extract = ["-i", archive, "-o", out_dir.to_str().unwrap(), "-e", "1"];
    let list = ["-i", archive, "--list"];
    let metadata = ["-i", archive, "--metadata"];
    for args in [&extract[..], &list[..], &metadata[..]] {
        let printed = failure_output(args);
        assert!(
            printed.contains("Unsupported format ID 7 in the archive header"),
            "sprite-shrink {args:?}:\n{printed}"
        );
    }
    assert!(fs::read_dir(&out_dir).unwrap().next().is_none());
}

/// Two small, unrelated ROMs in `dir/input`, returned as that folder.
fn two_roms(dir: &Path) -> PathBuf {
    let input_dir = dir.join("input");
    fs::create_dir_all(&input_dir).unwrap();
    fs::write(input_dir.join("a.bin"), tiled_bytes(0x5EED_3101, 4 * 1024))
        .unwrap();
    fs::write(input_dir.join("b.bin"), tiled_bytes(0x5EED_3102, 4 * 1024))
        .unwrap();
    input_dir
}

#[test]
fn rejected_arguments_exit_with_failure() {
    let tmp = TempDir::new().unwrap();
    let input_dir = two_roms(tmp.path());
    let existing = tmp.path().join("existing.ssmc");
    fs::write(&existing, b"keep me").unwrap();
    let single = input_dir.join("a.bin");
    let input = input_dir.to_str().unwrap();
    let new_archive = tmp.path().join("new.ssmc");

    let cases: [(&[&str], &str); 3] = [
        (
            &["-i", input, "-o", existing.to_str().unwrap()],
            "File already exists",
        ),
        (
            &["-i", single.to_str().unwrap(), "-o", new_archive.to_str().unwrap()],
            "requires at least two input files",
        ),
        (
            &["--mode", "optical", "-i", existing.to_str().unwrap(), "--list"],
            "only applicable for compression",
        ),
    ];
    for (args, message) in cases {
        let printed = failure_output(args);
        assert!(printed.contains(message), "sprite-shrink {args:?}:\n{printed}");
    }
    assert_eq!(fs::read(&existing).unwrap(), b"keep me");
    assert!(!new_archive.exists());
}

#[test]
fn a_failed_run_exits_with_failure() {
    let tmp = TempDir::new().unwrap();
    let input_dir = two_roms(tmp.path());
    let archive = tmp.path().join("roms.ssmc");
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();
    run_cli(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);

    // The archive holds 2 files, so index 3 fails after the arguments are
    // accepted.
    let printed = failure_output(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "3",
    ]);
    assert!(
        printed.contains("requested index 3 is out of the valid range (1-2)"),
        "output:\n{printed}"
    );
}

#[test]
fn rom_index_zero_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let input_dir = two_roms(tmp.path());
    let archive = tmp.path().join("roms.ssmc");
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();
    run_cli(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);

    // Indices are 1-based, so 0 names no file, alone or in a range.
    for range in ["0", "0-1"] {
        let printed = failure_output(&[
            "-i", archive.to_str().unwrap(),
            "-o", out_dir.to_str().unwrap(),
            "-e", range,
        ]);
        assert!(
            printed.contains("ROM indexes start at 1"),
            "-e {range}:\n{printed}"
        );
    }
    assert!(fs::read_dir(&out_dir).unwrap().next().is_none());
}

#[test]
fn files_past_index_255_can_be_extracted() {
    const FILE_COUNT: u32 = 256;
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let archive = tmp.path().join("many.ssmc");
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();

    // Zero-padded names keep the table of contents in this order, so file
    // `n` is at index `n`.
    for n in 1..=FILE_COUNT {
        let data = tiled_bytes(0x5EED_4000 + u64::from(n), 256);
        fs::write(input_dir.join(format!("{n:03}.bin")), data).unwrap();
    }
    run_cli(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);

    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1,255-256",
    ]);
    for n in [1, 255, 256] {
        let name = format!("{n:03}.bin");
        let extracted = fs::read(out_dir.join(&name)).unwrap();
        let original = fs::read(input_dir.join(&name)).unwrap();
        assert_bytes_eq(&extracted, &original, &name);
    }
    assert_eq!(fs::read_dir(&out_dir).unwrap().count(), 3);
}

#[test]
fn a_skipped_subfolder_is_not_called_a_symbolic_link() {
    let tmp = TempDir::new().unwrap();
    let input_dir = two_roms(tmp.path());
    let nested = input_dir.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("c.bin"), tiled_bytes(0x5EED_3103, 4 * 1024))
        .unwrap();
    let archive = tmp.path().join("roms.ssmc");

    let stdout = stdout_of(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    assert!(
        stdout.contains("folders inside an input folder aren't searched"),
        "stdout:\n{stdout}"
    );
    assert!(!stdout.contains("symbolic link"), "stdout:\n{stdout}");

    // Only the two top-level files were archived.
    let list = stdout_of(&["-i", archive.to_str().unwrap(), "--list"]);
    assert!(list.contains("a.bin") && list.contains("b.bin"), "{list}");
    assert!(!list.contains("c.bin"), "{list}");
}

/// Writes a blank Mode 2 data disc of `sectors` sectors to `dir` as
/// `<title>.cue` pointing at `disc.bin`, and returns the image.
fn write_blank_disc(dir: &Path, title: &str, sectors: usize) -> Vec<u8> {
    fs::create_dir_all(dir).unwrap();
    let image = vec![0_u8; sectors * 2352];
    fs::write(dir.join("disc.bin"), &image).unwrap();
    fs::write(
        dir.join(format!("{title}.cue")),
        "FILE \"disc.bin\" BINARY\n\
         \x20 TRACK 01 MODE2/2352\n\
         \x20   INDEX 01 00:00:00\n",
    )
    .unwrap();
    image
}

#[test]
fn a_disc_whose_header_numbers_jump_is_archived_and_extracted() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let out_dir = tmp.path().join("extract");
    let archive = tmp.path().join("jump.ssmd");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();

    // Mode 2 Form 1 sectors numbered from 150, jumping 10,000 forward at
    // sector 400, as at a GD-ROM's gap between its two areas.
    let subheader = [1, 0, 0x08, 0, 1, 0, 0x08, 0];
    let image: Vec<u8> = (0..600_u32)
        .flat_map(|i| {
            let number = i + 150 + if i < 400 { 0 } else { 10_000 };
            let user_data = tiled_bytes(u64::from(number), 2048);
            rebuild_sector_simd(
                &user_data,
                &subheader,
                SectorType::Mode2Form1,
                number,
                None,
            )
            .unwrap()
        })
        .collect();
    fs::write(input_dir.join("disc.bin"), &image).unwrap();
    fs::write(
        input_dir.join("disc.cue"),
        "FILE \"disc.bin\" BINARY\n\
         \x20 TRACK 01 MODE2/2352\n\
         \x20   INDEX 01 00:00:00\n",
    )
    .unwrap();

    run_cli(&[
        "--mode", "optical",
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    extract_first(&archive, &out_dir);
    let extracted = fs::read(out_dir.join("disc.bin")).unwrap();
    assert_bytes_eq(&extracted, &image, "disc.bin");
}

#[test]
fn discs_from_different_folders_read_their_own_bin_files() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("extract");
    let archive = tmp.path().join("discs.ssmd");
    fs::create_dir_all(&out_dir).unwrap();

    // Both CUE sheets name `disc.bin`; only the folder tells them apart.
    let first_dir = tmp.path().join("first");
    let second_dir = tmp.path().join("second");
    let one = write_blank_disc(&first_dir, "one", 300);
    let two = write_blank_disc(&second_dir, "two", 320);

    run_cli(&[
        "--mode", "optical",
        "-i", first_dir.to_str().unwrap(),
        "-i", second_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1-2",
    ]);

    assert_bytes_eq(&fs::read(out_dir.join("one.bin")).unwrap(), &one, "one");
    assert_bytes_eq(&fs::read(out_dir.join("two.bin")).unwrap(), &two, "two");
}

#[test]
fn optical_mode_without_cue_files_says_what_is_missing() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let archive = tmp.path().join("discs.ssmd");
    fs::create_dir_all(&input_dir).unwrap();
    // Two BIN images and no CUE sheet to describe them.
    for name in ["one.bin", "two.bin"] {
        fs::write(input_dir.join(name), vec![0_u8; 300 * 2352]).unwrap();
    }

    let printed = failure_output(&[
        "--mode", "optical",
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    assert!(
        printed.contains("Optical mode needs a .cue file for each disc"),
        "output:\n{printed}"
    );
    assert!(!printed.contains("internal logic error"), "output:\n{printed}");
    assert!(!archive.exists());
}

#[test]
fn a_window_size_outside_the_chunker_range_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let input_dir = two_roms(tmp.path());
    let archive = tmp.path().join("roms.ssmc");

    // FastCDC accepts average sizes from 256 bytes to 4 MiB.
    for window in ["100", "8MiB"] {
        let printed = failure_output(&[
            "-i", input_dir.to_str().unwrap(),
            "-o", archive.to_str().unwrap(),
            "-w", window,
        ]);
        assert!(
            printed.contains("outside the supported range of 256 to 4194304 \
                bytes"),
            "-w {window}:\n{printed}"
        );
        assert!(!printed.contains("panicked"), "-w {window}:\n{printed}");
        assert!(!archive.exists());
    }

    // The limits themselves are accepted.
    for window in ["256", "4MiB"] {
        run_cli(&[
            "-i", input_dir.to_str().unwrap(),
            "-o", archive.to_str().unwrap(),
            "-w", window,
            "--force",
        ]);
    }

    // The window only matters when compressing, so a bad value doesn't stop
    // extraction.
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();
    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1",
        "-w", "100",
    ]);
    assert!(out_dir.join("a.bin").exists());
}

#[test]
fn an_auto_tuned_archive_extracts_the_original_files() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let archive = tmp.path().join("roms.ssmc");
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();
    let files = rom_set();
    for (name, data) in &files {
        fs::write(input_dir.join(name), data).unwrap();
    }

    run_cli(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
        "--auto-tune",
    ]);

    let range = format!("1-{}", files.len());
    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", &range,
    ]);
    for (name, data) in &files {
        let extracted = fs::read(out_dir.join(name)).unwrap();
        assert_bytes_eq(&extracted, data, name);
    }
}

#[test]
fn auto_tune_handles_too_little_data_for_a_dictionary() {
    let tmp = TempDir::new().unwrap();
    let input_dir = two_roms(tmp.path());
    let archive = tmp.path().join("roms.ssmc");
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&out_dir).unwrap();

    // Two 4 KiB ROMs cut into too few chunks to train a dictionary on, so
    // every auto-tune estimate has to work without one.
    let stdout = stdout_of(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
        "--auto-tune",
    ]);
    assert!(stdout.contains(NO_DICTIONARY_NOTE), "stdout:\n{stdout}");

    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1-2",
    ]);
    for name in ["a.bin", "b.bin"] {
        let extracted = fs::read(out_dir.join(name)).unwrap();
        let original = fs::read(input_dir.join(name)).unwrap();
        assert_bytes_eq(&extracted, &original, name);
    }
}

#[test]
fn files_sharing_a_name_are_rejected() {
    let tmp = TempDir::new().unwrap();
    let first = two_roms(&tmp.path().join("first"));
    let second = two_roms(&tmp.path().join("second"));
    let archive = tmp.path().join("roms.ssmc");

    let printed = failure_output(&[
        "-i", first.to_str().unwrap(),
        "-i", second.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    for name in ["a.bin", "b.bin"] {
        assert!(printed.contains(name), "output:\n{printed}");
    }
    assert!(printed.contains("duplicate file names"), "output:\n{printed}");
    assert!(!archive.exists());
}

#[test]
fn discs_sharing_a_cue_name_are_rejected() {
    let tmp = TempDir::new().unwrap();
    let first_dir = tmp.path().join("first");
    let second_dir = tmp.path().join("second");
    write_blank_disc(&first_dir, "game", 300);
    write_blank_disc(&second_dir, "game", 320);
    let archive = tmp.path().join("discs.ssmd");

    let printed = failure_output(&[
        "--mode", "optical",
        "-i", first_dir.to_str().unwrap(),
        "-i", second_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    assert!(printed.contains("duplicate file names"), "output:\n{printed}");
    assert!(printed.contains("game"), "output:\n{printed}");
    assert!(!archive.exists());
}

#[test]
fn the_same_file_given_twice_is_not_a_conflict() {
    let tmp = TempDir::new().unwrap();
    let input_dir = two_roms(tmp.path());
    let archive = tmp.path().join("roms.ssmc");

    run_cli(&[
        "-i", input_dir.to_str().unwrap(),
        "-i", input_dir.join("a.bin").to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
}

#[test]
fn input_order_does_not_change_the_archive() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    fs::create_dir_all(&input_dir).unwrap();
    let names = ["c.bin", "a.bin", "d.bin", "b.bin"];
    for (seed, name) in (0x5EED_2801..).zip(names) {
        fs::write(input_dir.join(name), tiled_bytes(seed, 8 * 1024)).unwrap();
    }

    // The folder in whatever order the filesystem lists it, then the files
    // one by one in two different orders.
    let path = |name: &str| input_dir.join(name).to_str().unwrap().to_string();
    let orders: [Vec<String>; 3] = [
        vec![input_dir.to_str().unwrap().to_string()],
        names.iter().map(|name| path(name)).collect(),
        names.iter().rev().map(|name| path(name)).collect(),
    ];
    let archives: Vec<Vec<u8>> = orders
        .iter()
        .enumerate()
        .map(|(i, inputs)| {
            let archive = tmp.path().join(format!("{i}.ssmc"));
            let mut args: Vec<&str> = Vec::new();
            for input in inputs {
                args.extend(["-i", input]);
            }
            args.extend(["-o", archive.to_str().unwrap()]);
            run_cli(&args);
            fs::read(&archive).unwrap()
        })
        .collect();

    assert_bytes_eq(&archives[1], &archives[0], "files in listed order");
    assert_bytes_eq(&archives[2], &archives[0], "files in reverse order");
}
