//! Test input and assertions shared by the workspace's test suites.
//!
//! The input is generated from fixed seeds, so every run, on every machine,
//! produces the same bytes. The CLI's baseline archives are built from this
//! input, so changing what any of it generates changes those archives too.

/// Fixed-seed xorshift, so every run generates the same input.
#[derive(Debug)]
pub struct XorShift(u64);

impl XorShift {
    /// Starts the generator at `seed`, which must not be zero: xorshift
    /// maps zero to zero, so every output would be zero.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Advances the generator and returns its next output.
    pub const fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Bytes built from a small palette of random 32-byte tiles. Compressible
/// like real ROM data, so the archives stay small, but not trivially so.
#[must_use]
pub fn tiled_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = XorShift::new(seed);
    let palette: Vec<[u8; 32]> = (0..64)
        .map(|_| {
            let mut tile = [0_u8; 32];
            for chunk in tile.as_chunks_mut::<8>().0 {
                *chunk = rng.next_u64().to_le_bytes();
            }
            tile
        })
        .collect();

    let mut out = Vec::with_capacity(len + 32);
    while out.len() < len {
        let index = usize::from(rng.next_u64().to_le_bytes()[0] % 64);
        // `index` is below 64, the palette's length, so `get` always finds
        // a tile.
        if let Some(tile) = palette.get(index) {
            out.extend_from_slice(tile);
        }
    }
    out.truncate(len);
    out
}

/// Three ROM-like files, as `(name, contents)`: a base image, a patched
/// copy of it, and an unrelated file. The shared regions exercise
/// deduplication.
#[must_use]
pub fn rom_set() -> Vec<(String, Vec<u8>)> {
    let base = tiled_bytes(0x5EED_0001, 96 * 1024);

    let mut patched = base.clone();
    patched.splice(10_000..10_512, tiled_bytes(0x5EED_0002, 512));
    patched.splice(60_000..61_024, tiled_bytes(0x5EED_0003, 1024));
    patched.extend_from_slice(&tiled_bytes(0x5EED_0004, 8 * 1024));

    let other = tiled_bytes(0x5EED_0005, 40 * 1024);

    vec![
        ("game (Rev 0).bin".to_string(), base),
        ("game (Rev 1).bin".to_string(), patched),
        ("other game.bin".to_string(), other),
    ]
}

/// Describes where `actual` first differs from `expected`, or returns
/// `None` if they're equal.
///
/// For buffers too large to print, such as whole files or archives: the
/// first differing offset and byte usually say whether a header, a section
/// or the length is wrong.
#[must_use]
pub fn first_difference(actual: &[u8], expected: &[u8]) -> Option<String> {
    let mismatch = actual
        .iter()
        .zip(expected)
        .enumerate()
        .find(|(_, (got, want))| got != want);
    if let Some((offset, (got, want))) = mismatch {
        return Some(format!(
            "first difference at byte {offset:#x}: got {got:#04x}, expected \
             {want:#04x} ({} bytes, expected {})",
            actual.len(),
            expected.len()
        ));
    }

    (actual.len() != expected.len()).then(|| {
        format!(
            "{} bytes, expected {}; the first {} match",
            actual.len(),
            expected.len(),
            actual.len().min(expected.len())
        )
    })
}

/// Asserts that `actual` equals `expected`, naming the first difference
/// instead of printing both buffers the way `assert_eq!` would.
///
/// # Panics
///
/// Panics if the buffers differ, with `what` and the difference in the
/// message.
#[track_caller]
pub fn assert_bytes_eq(actual: &[u8], expected: &[u8], what: &str) {
    let difference = first_difference(actual, expected);
    assert!(
        difference.is_none(),
        "{what}: {}",
        difference.unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_buffers_have_no_difference() {
        assert_eq!(first_difference(b"same", b"same"), None);
        assert_eq!(first_difference(b"", b""), None);
    }

    #[test]
    fn first_differing_byte_is_named() {
        assert_eq!(
            first_difference(b"abXd", b"abcd").as_deref(),
            Some(
                "first difference at byte 0x2: got 0x58, expected 0x63 \
                 (4 bytes, expected 4)"
            )
        );
    }

    #[test]
    fn length_difference_is_named_when_the_prefix_matches() {
        assert_eq!(
            first_difference(b"abc", b"abcde").as_deref(),
            Some("3 bytes, expected 5; the first 3 match")
        );
        assert_eq!(
            first_difference(b"", b"a").as_deref(),
            Some("0 bytes, expected 1; the first 0 match")
        );
    }

    #[test]
    #[should_panic(expected = "rom.bin: first difference at byte 0x1")]
    fn assert_bytes_eq_reports_what_and_where() {
        assert_bytes_eq(b"aXc", b"abc", "rom.bin");
    }
}
