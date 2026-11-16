//! Runs the `sprite-shrink` binary end to end on inputs the baseline
//! archives don't cover.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sprite_shrink::{HASH_TYPE_XXH3_128, parse_file_header};
use sprite_shrink_test_support::{assert_bytes_eq, rom_set, tiled_bytes};
use tempfile::TempDir;

use crate::common::{cli_command, cli_output, reported_error, run_cli};

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

/// Compresses the shared ROM set from a folder in `dir`, and returns the
/// archive and the files in the order the archive lists them, by name.
fn archived_rom_set(dir: &Path) -> (PathBuf, Vec<(String, Vec<u8>)>) {
    let input_dir = dir.join("input");
    let archive = dir.join("roms.ssmc");
    fs::create_dir_all(&input_dir).unwrap();
    let mut files = rom_set();
    for (name, data) in &files {
        fs::write(input_dir.join(name), data).unwrap();
    }
    run_cli(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    (archive, files)
}

#[test]
fn the_list_shows_each_file_with_its_size() {
    let tmp = TempDir::new().unwrap();
    let (archive, files) = archived_rom_set(tmp.path());

    let stdout = stdout_of(&["-i", archive.to_str().unwrap(), "-l"]);
    let rows: Vec<&str> = stdout.lines().skip(2).collect();
    let expected: Vec<String> = files
        .iter()
        .enumerate()
        .map(|(index, (name, data))| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "the CLI formats sizes from an f64 the same way"
            )]
            let size = human_bytes::human_bytes(data.len() as f64);
            format!("{}\t| {size}\t| {name}", index + 1)
        })
        .collect();
    assert_eq!(rows, expected, "stdout:\n{stdout}");
}

#[test]
fn metadata_lists_each_file_as_json() {
    let tmp = TempDir::new().unwrap();
    let (archive, files) = archived_rom_set(tmp.path());

    let stdout = stdout_of(&["-i", archive.to_str().unwrap(), "-m"]);
    let listed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let expected: Vec<serde_json::Value> = files
        .iter()
        .enumerate()
        .map(|(index, (name, data))| {
            serde_json::json!({
                "index": index + 1,
                "filename": name,
                "size(bytes)": data.len(),
            })
        })
        .collect();
    assert_eq!(listed, serde_json::Value::from(expected));
}

#[test]
fn metadata_lists_each_disc_as_json() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("discs.ssmd");
    let one = write_blank_disc(&tmp.path().join("first"), "one", 300);
    let two = write_blank_disc(&tmp.path().join("second"), "two", 320);
    run_cli(&[
        "--mode", "optical",
        "-i", tmp.path().join("first").to_str().unwrap(),
        "-i", tmp.path().join("second").to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);

    let stdout = stdout_of(&["-i", archive.to_str().unwrap(), "-m"]);
    let listed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        listed,
        serde_json::json!([
            {
                "index": 1,
                "filename": "one.bin",
                "group": null,
                "size(bytes)": one.len(),
            },
            {
                "index": 2,
                "filename": "two.bin",
                "group": null,
                "size(bytes)": two.len(),
            },
        ])
    );
}

#[test]
fn discs_in_a_playlist_share_a_group() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("discs.ssmd");
    let dirs = ["first", "second", "third"].map(|name| tmp.path().join(name));
    write_blank_disc(&dirs[0], "one", 300);
    write_blank_disc(&dirs[1], "two", 310);
    write_blank_disc(&dirs[2], "three", 320);

    // Entries are matched to discs by file stem, so the commented-out path
    // would put disc two in the first group if comments were read.
    fs::write(
        dirs[0].join("set.m3u"),
        "#EXTM3U\n\n# second/two.cue\none.cue\n",
    )
    .unwrap();
    fs::write(dirs[2].join("set.m3u8"), "three.cue\n").unwrap();

    let mut args = vec!["--mode", "optical"];
    for dir in &dirs {
        args.extend(["-i", dir.to_str().unwrap()]);
    }
    args.extend(["-o", archive.to_str().unwrap()]);
    run_cli(&args);

    let stdout = stdout_of(&["-i", archive.to_str().unwrap(), "-m"]);
    let listed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let groups: Vec<(&str, &serde_json::Value)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|disc| (disc["filename"].as_str().unwrap(), &disc["group"]))
        .collect();
    // The archive lists discs by name.
    assert_eq!(
        groups,
        [
            ("one.bin", &serde_json::json!(1)),
            ("three.bin", &serde_json::json!(2)),
            ("two.bin", &serde_json::Value::Null),
        ]
    );
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

/// Runs the CLI with its config, cache and data folders inside `home`, so it
/// reads and writes its config file there. Only Linux finds them through
/// these variables.
#[cfg(target_os = "linux")]
fn output_with_config_in(home: &Path, args: &[&str]) -> std::process::Output {
    cli_command()
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"))
        .args(["--disable-logging", "--threads", "1"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
#[cfg(target_os = "linux")]
fn json_output_in_the_config_lists_as_json() {
    let tmp = TempDir::new().unwrap();
    let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/compat/v2/roms.ssmc");
    let archive = archive.to_str().unwrap();
    let list = ["-i", archive, "-l"];

    // The first run writes the default config.
    let table = output_with_config_in(tmp.path(), &list);
    assert!(table.status.success());
    let config = tmp
        .path()
        .join("config/spriteshrink/SpriteShrink-config.toml");
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("json_output = false"), "{text}");
    let text = text.replace("json_output = false", "json_output = true");
    fs::write(&config, text).unwrap();

    let json = output_with_config_in(tmp.path(), &list);
    assert!(json.status.success());
    assert_eq!(
        String::from_utf8_lossy(&json.stdout),
        stdout_of(&["-i", archive, "-m"]),
    );
    assert_ne!(json.stdout, table.stdout);

    // Compression still works: the setting doesn't switch to `-m` mode.
    let input_dir = tmp.path().join("input");
    fs::create_dir_all(&input_dir).unwrap();
    for (name, data) in rom_set() {
        fs::write(input_dir.join(name), data).unwrap();
    }
    let new_archive = tmp.path().join("new.ssmc");
    let compressed = output_with_config_in(tmp.path(), &[
        "-i", input_dir.to_str().unwrap(),
        "-o", new_archive.to_str().unwrap(),
    ]);
    assert!(
        compressed.status.success(),
        "{}",
        String::from_utf8_lossy(&compressed.stderr)
    );
    assert!(new_archive.exists());
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

    // The unit tests check each rejection `validate_args` makes. This checks
    // that `main` stops on one with a failure, before writing anything.
    let printed = failure_output(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", existing.to_str().unwrap(),
    ]);
    assert!(printed.contains("File already exists"), "output:\n{printed}");
    assert_eq!(fs::read(&existing).unwrap(), b"keep me");
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
fn an_audio_only_disc_is_archived_and_extracted() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let out_dir = tmp.path().join("extract");
    let archive = tmp.path().join("audio.ssmd");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();

    // Two seconds of PCM, with no data sector to read a header from.
    let image = tiled_bytes(0xA0D1_0001, 150 * 2352);
    let cue = "FILE \"disc.bin\" BINARY\n\
               \x20 TRACK 01 AUDIO\n\
               \x20   INDEX 01 00:00:00\n";
    fs::write(input_dir.join("disc.bin"), &image).unwrap();
    fs::write(input_dir.join("disc.cue"), cue).unwrap();

    run_cli(&[
        "--mode", "optical",
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    extract_first(&archive, &out_dir);

    let extracted = fs::read(out_dir.join("disc.bin")).unwrap();
    assert_bytes_eq(&extracted, &image, "disc.bin");
    assert_eq!(fs::read_to_string(out_dir.join("disc.cue")).unwrap(), cue);
}

#[test]
fn a_disc_in_two_files_is_extracted_as_one() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let out_dir = tmp.path().join("extract");
    let archive = tmp.path().join("split.ssmd");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();

    let data = vec![0_u8; 150 * 2352];
    let audio = tiled_bytes(0xA0D1_0002, 150 * 2352);
    fs::write(input_dir.join("track1.bin"), &data).unwrap();
    fs::write(input_dir.join("track2.bin"), &audio).unwrap();
    fs::write(
        input_dir.join("disc.cue"),
        "FILE \"track1.bin\" BINARY\n\
         \x20 TRACK 01 MODE2/2352\n\
         \x20   INDEX 01 00:00:00\n\
         FILE \"track2.bin\" BINARY\n\
         \x20 TRACK 02 AUDIO\n\
         \x20   INDEX 01 00:00:00\n",
    )
    .unwrap();

    run_cli(&[
        "--mode", "optical",
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);
    extract_first(&archive, &out_dir);

    let joined = [data, audio].concat();
    let extracted = fs::read(out_dir.join("disc.bin")).unwrap();
    assert_bytes_eq(&extracted, &joined, "disc.bin");
    assert_eq!(
        fs::read_to_string(out_dir.join("disc.cue")).unwrap(),
        "FILE \"disc.bin\" BINARY\n\
         \x20 TRACK 01 MODE2/2352\n\
         \x20   INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n\
         \x20   INDEX 01 00:02:00\n"
    );
}

#[test]
fn both_modes_write_128_bit_archives_on_request() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("extract");
    let roms = tmp.path().join("roms.ssmc");
    let disc = tmp.path().join("disc.ssmd");
    let input_dir = two_roms(tmp.path());
    let disc_dir = tmp.path().join("disc");
    let image = write_blank_disc(&disc_dir, "disc", 300);

    run_cli(&[
        "-i", input_dir.to_str().unwrap(),
        "-o", roms.to_str().unwrap(),
        "-b", "128",
    ]);
    run_cli(&[
        "--mode", "optical",
        "-i", disc_dir.to_str().unwrap(),
        "-o", disc.to_str().unwrap(),
        "-b", "128",
    ]);

    for archive in [&roms, &disc] {
        let bytes = fs::read(archive).unwrap();
        let header = parse_file_header(&bytes[..36]).unwrap();
        assert_eq!(
            header.hash_type,
            HASH_TYPE_XXH3_128,
            "{}",
            archive.display()
        );
    }

    fs::create_dir_all(&out_dir).unwrap();
    run_cli(&[
        "-i", roms.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1-2",
    ]);
    extract_first(&disc, &out_dir);
    for name in ["a.bin", "b.bin"] {
        assert_bytes_eq(
            &fs::read(out_dir.join(name)).unwrap(),
            &fs::read(input_dir.join(name)).unwrap(),
            name,
        );
    }
    assert_bytes_eq(
        &fs::read(out_dir.join("disc.bin")).unwrap(),
        &image,
        "disc.bin",
    );
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
