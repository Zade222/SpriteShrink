//! Archives written by the CLI before the archive layout moved into the
//! libraries, kept as a byte-for-byte baseline.
//!
//! Each test generates the same input files from fixed seeds, compresses
//! them, and compares the archive with the one committed under
//! `tests/fixtures/baseline/`. It then extracts the archive and checks every
//! file against its input.
//!
//! A change that is meant to alter the archive bytes has to regenerate the
//! fixtures on purpose:
//!
//! ```text
//! SPRITE_SHRINK_BLESS=1 cargo test -p sprite_shrink_application --test integration baseline_archives::
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use sprite_shrink_cd::{SectorType, rebuild_sector_bitwise};
use tempfile::TempDir;

const BLESS_VAR: &str = "SPRITE_SHRINK_BLESS";

/// Fixed-seed xorshift, so every run generates the same input.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Bytes built from a small palette of random 32-byte tiles. Compressible
/// like real ROM data, so the archives stay small, but not trivially so.
fn tiled_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = XorShift(seed);
    let palette: Vec<[u8; 32]> = (0..64)
        .map(|_| {
            let mut tile = [0_u8; 32];
            for chunk in tile.as_chunks_mut::<8>().0 {
                *chunk = rng.next().to_le_bytes();
            }
            tile
        })
        .collect();

    let mut out = Vec::with_capacity(len + 32);
    while out.len() < len {
        let tile = palette[usize::from(rng.next().to_le_bytes()[0] % 64)];
        out.extend_from_slice(&tile);
    }
    out.truncate(len);
    out
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/baseline")
        .join(name)
}

/// Runs the CLI and fails the test if it reports an error. The CLI exits 0
/// even when it fails, so the output is checked as well as the status.
fn run_cli(args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_sprite-shrink"))
        .args(["--ignore-config", "--disable-logging", "--threads", "1"])
        .args(args)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success()
            && !stdout.contains("encountered an error")
            && !stderr.contains("encountered an error"),
        "sprite-shrink {args:?} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Compares `archive` with the committed fixture, or replaces the fixture
/// when `SPRITE_SHRINK_BLESS` is set.
fn check_against_fixture(archive: &Path, fixture_name: &str) {
    let fixture = fixture_path(fixture_name);
    let actual = fs::read(archive).unwrap();

    if std::env::var_os(BLESS_VAR).is_some() {
        fs::create_dir_all(fixture.parent().unwrap()).unwrap();
        fs::write(&fixture, &actual).unwrap();
        return;
    }

    let expected = fs::read(&fixture).unwrap_or_else(|e| {
        panic!("can't read {}: {e}", fixture.display())
    });
    assert!(
        actual == expected,
        "{fixture_name} no longer matches the baseline ({} bytes, baseline \
         {} bytes). If the change is meant to alter the archive format, \
         rerun with {BLESS_VAR}=1 and commit the new fixture.",
        actual.len(),
        expected.len()
    );
}

/// Three ROM-like files: a base image, a patched copy of it, and an
/// unrelated file. The shared regions exercise deduplication.
fn write_rom_set(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let base = tiled_bytes(0x5EED_0001, 96 * 1024);

    let mut patched = base.clone();
    patched[10_000..10_512].copy_from_slice(&tiled_bytes(0x5EED_0002, 512));
    patched[60_000..61_024].copy_from_slice(&tiled_bytes(0x5EED_0003, 1024));
    patched.extend_from_slice(&tiled_bytes(0x5EED_0004, 8 * 1024));

    let other = tiled_bytes(0x5EED_0005, 40 * 1024);

    let files = vec![
        ("game (Rev 0).bin".to_string(), base),
        ("game (Rev 1).bin".to_string(), patched),
        ("other game.bin".to_string(), other),
    ];
    for (name, data) in &files {
        fs::write(dir.join(name), data).unwrap();
    }
    files
}

#[test]
fn default_mode_archive_matches_baseline() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();

    let files = write_rom_set(&input_dir);
    let archive = tmp.path().join("roms.ssmc");

    // Each file is passed on its own so the order doesn't depend on the
    // order the filesystem lists a directory in.
    let inputs: Vec<String> = files
        .iter()
        .map(|(name, _)| input_dir.join(name).to_string_lossy().into_owned())
        .collect();
    let mut args = vec!["-o", archive.to_str().unwrap()];
    for input in &inputs {
        args.extend(["-i", input]);
    }
    run_cli(&args);

    check_against_fixture(&archive, "roms.ssmc");

    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1-3",
    ]);
    for (name, data) in &files {
        let extracted = fs::read(out_dir.join(name)).unwrap();
        assert!(extracted == *data, "{name} differs after extraction");
    }
}

/// A small mixed-mode disc: a Mode 2 data track (Form 1 sectors, some with
/// empty user data, and a few Form 2 sectors) followed by one second of audio.
/// Sectors are built with the library so their EDC and ECC are valid.
fn disc_image() -> Vec<u8> {
    const DATA_SECTORS: u32 = 300;
    const AUDIO_SECTORS: usize = 75;

    let form1_subheader = [0, 0, 0x08, 0, 0, 0, 0x08, 0];
    let form2_subheader = [0, 0, 0x20, 0, 0, 0, 0x20, 0];
    let mut image = Vec::new();

    // One stream for the whole track, so every sector draws on the same tile
    // palette and the data compresses the way real disc data does.
    let user_data = tiled_bytes(0xD15C_0001, 2324 * DATA_SECTORS as usize);
    let mut sector_data = user_data.chunks(2324);

    for i in 0..DATA_SECTORS {
        // Real discs start at MSF 00:02:00, frame 150.
        let frame = 150 + i;
        let data = sector_data.next().unwrap();
        let sector = if (200..210).contains(&i) {
            rebuild_sector_bitwise(
                data, &form2_subheader, SectorType::Mode2Form2, frame, None,
            )
        } else {
            let data = if i < 16 { &[0_u8; 2048][..] } else { &data[..2048] };
            rebuild_sector_bitwise(
                data, &form1_subheader, SectorType::Mode2Form1, frame, None,
            )
        };
        image.extend_from_slice(&sector.unwrap());
    }

    // One second of 16-bit stereo PCM: a triangle wave on the left channel
    // and a slower one on the right.
    let triangle = |n: usize, period: usize| -> i16 {
        let t = n % period;
        let rise = t.min(period - t);
        i16::try_from(rise * 6000 / period).unwrap_or(0) - 1500
    };
    for n in 0..AUDIO_SECTORS * 588 {
        image.extend_from_slice(&triangle(n, 200).to_le_bytes());
        image.extend_from_slice(&triangle(n, 500).to_le_bytes());
    }

    image
}

#[test]
fn optical_mode_archive_matches_baseline() {
    let tmp = TempDir::new().unwrap();
    let input_dir = tmp.path().join("input");
    let out_dir = tmp.path().join("extract");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();

    let image = disc_image();
    fs::write(input_dir.join("disc.bin"), &image).unwrap();
    fs::write(
        input_dir.join("disc.cue"),
        "FILE \"disc.bin\" BINARY\n\
         \x20 TRACK 01 MODE2/2352\n\
         \x20   INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n\
         \x20   INDEX 01 00:04:00\n",
    )
    .unwrap();

    let archive = tmp.path().join("disc.ssmd");
    run_cli(&[
        "--mode", "optical",
        "-i", input_dir.to_str().unwrap(),
        "-o", archive.to_str().unwrap(),
    ]);

    check_against_fixture(&archive, "disc.ssmd");

    run_cli(&[
        "-i", archive.to_str().unwrap(),
        "-o", out_dir.to_str().unwrap(),
        "-e", "1",
    ]);
    let extracted = fs::read(out_dir.join("disc.bin")).unwrap();
    assert!(extracted == image, "disc.bin differs after extraction");
}
