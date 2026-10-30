//! Test input shared by the workspace's test suites.
//!
//! Everything here is generated from fixed seeds, so every run, on every
//! machine, produces the same bytes. The CLI's baseline archives are built
//! from this input, so changing what any of it generates changes those
//! archives too.

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
