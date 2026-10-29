//! Archives written by earlier versions, which the current CLI must still
//! extract.
//!
//! Each format version has a folder under `tests/fixtures/compat/` holding
//! one SSMC and one SSMD archive written by that version. The files and
//! the hashes below are never regenerated. When `file_version` changes, add
//! a new folder and its tests and keep the old ones, so every version the
//! reader claims to support stays tested.
//!
//! These differ from the baseline archives, which are compared with what
//! the current code writes and get regenerated when the output changes on
//! purpose. A dependency update that changes the encoding (a bitcode minor
//! version, for one) would pass a regenerated baseline but fail here.

use std::{
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha512};
use sprite_shrink::{SUPPORTED_VERSION, parse_file_header};
use tempfile::TempDir;

use crate::common::{cli_output, reported_error, run_cli};

/// Written by the CLI at format version 2.0 (`file_version` `0x0002_0000`).
const V2_DIR: &str = "v2";
const V2_VERSION: u32 = 0x0002_0000;

/// The SHA-512 of each file in `v2/roms.ssmc`, in archive order.
const V2_ROMS: [(&str, &str); 3] = [
    (
        "game (Rev 0).bin",
        "bbac2fc096e74b7b603f607960296edcf498603c97bf06cc00035bce3e4eaac3\
         262d1e25386ad897c77163ce25db17f71f5025ef9d887aa94b33314e570fa26a",
    ),
    (
        "game (Rev 1).bin",
        "7845abc05d64971aaa8b5816173a30ef9c85adb36c38d0263e98765e0407ee4e\
         aec813a466936785ce44cc64fcb15926070e523ae8676a74404e7afd342068c7",
    ),
    (
        "other game.bin",
        "cd54b483b7578cbee8d3a0d3168aa2f7da634c07306a954f3c131a9d1a40fd0a\
         fe8f9c80b4a6d7e7b8aa4b08a7758571c8d6e1c87be790f86a97263647dc0951",
    ),
];

/// The SHA-512 of the disc image in `v2/disc.ssmd`.
const V2_DISC: (&str, &str) = (
    "disc.bin",
    "0f5dca56109cbf8e74d418ea6235c48ecc62a7c07faa9327c1ba02b56cf4f9a8\
     bd77f02a73eff5e8f934975f0fa61289b2f1f130b3eb271b954657c045a0301b",
);

fn compat_path(version_dir: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/compat")
        .join(version_dir)
        .join(name)
}

fn sha512_hex(path: &Path) -> String {
    let data = fs::read(path)
        .unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()));

    Sha512::digest(&data).iter().fold(String::new(), |mut hex, byte| {
        write!(hex, "{byte:02x}").unwrap();
        hex
    })
}

/// Fails if the fixture isn't the format version its folder says it is,
/// so a newer archive copied into an old folder doesn't go unnoticed.
fn assert_file_version(archive: &Path, expected: u32) {
    let bytes = fs::read(archive).unwrap();
    let header = parse_file_header(&bytes[..36]).unwrap();

    assert_eq!(
        header.file_version, expected,
        "{} has the wrong file_version",
        archive.display()
    );
    assert!(expected <= SUPPORTED_VERSION);
}

fn extract(archive: &Path, out_dir: &Path, indices: &str) {
    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", indices,
    ]);
}

fn assert_extracted(out_dir: &Path, name: &str, expected_sha512: &str) {
    assert_eq!(
        sha512_hex(&out_dir.join(name)),
        expected_sha512,
        "{name} differs after extraction"
    );
}

#[test]
fn v2_ssmc_archive_extracts() {
    let archive = compat_path(V2_DIR, "roms.ssmc");
    assert_file_version(&archive, V2_VERSION);
    let out = TempDir::new().unwrap();

    extract(&archive, out.path(), "1-3");

    for (name, sha512) in V2_ROMS {
        assert_extracted(out.path(), name, sha512);
    }
}

#[test]
fn v2_ssmd_archive_extracts() {
    let archive = compat_path(V2_DIR, "disc.ssmd");
    assert_file_version(&archive, V2_VERSION);
    let out = TempDir::new().unwrap();

    extract(&archive, out.path(), "1");

    let (name, sha512) = V2_DISC;
    assert_extracted(out.path(), name, sha512);

    // The CUE sheet is rebuilt from the disc manifest, so its tracks show
    // whether the manifest decoded correctly. Only the lines that come from
    // the manifest are checked, not the formatting around them.
    let cue = fs::read_to_string(out.path().join("disc.cue")).unwrap();
    for line in [
        "FILE \"disc.bin\" BINARY",
        "TRACK 01 MODE2/2352",
        "TRACK 02 AUDIO",
        "INDEX 01 00:04:00",
    ] {
        assert!(cue.contains(line), "disc.cue is missing {line:?}:\n{cue}");
    }
}

/// Extracting a truncated copy of `archive` has to fail with the CLI's
/// error message, not a panic.
///
/// Each file is written to a `.tmp` file and renamed only once every chunk
/// has passed its hash check, so files finished before the error stay. Any
/// file left behind has to be one of `expected` and match its hash, and no
/// `.tmp` file may be left.
fn assert_truncated_copies_fail_cleanly(
    archive: &Path,
    indices: &str,
    expected: &[(&str, &str)],
) {
    let bytes = fs::read(archive).unwrap();
    let tmp = TempDir::new().unwrap();

    // Inside the header, inside the metadata, and inside the compressed
    // data after intact metadata.
    for len in [20, 300, bytes.len() - 100] {
        let truncated = tmp.path().join(format!("truncated-{len}"));
        fs::write(&truncated, &bytes[..len]).unwrap();
        let out_dir = tmp.path().join(format!("out-{len}"));
        fs::create_dir(&out_dir).unwrap();

        let output = cli_output(&[
            "-i", truncated.to_str().unwrap(),
            "-o", out_dir.to_str().unwrap(),
            "-e", indices,
        ]);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            reported_error(&output) && !stderr.contains("panicked"),
            "{len} of {} bytes: expected an error, not a panic or \
             success\nstderr:\n{stderr}",
            bytes.len()
        );

        for entry in fs::read_dir(&out_dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let Some((_, sha512)) = expected.iter().find(|(n, _)| *n == name)
            else {
                panic!("{len} of {} bytes left {name}", bytes.len());
            };
            assert_extracted(&out_dir, &name, sha512);
        }
    }
}

#[test]
fn v2_ssmc_truncated_archive_reports_error() {
    assert_truncated_copies_fail_cleanly(
        &compat_path(V2_DIR, "roms.ssmc"), "1-3", &V2_ROMS,
    );
}

#[test]
fn v2_ssmd_truncated_archive_reports_error() {
    assert_truncated_copies_fail_cleanly(
        &compat_path(V2_DIR, "disc.ssmd"), "1", &[V2_DISC],
    );
}
