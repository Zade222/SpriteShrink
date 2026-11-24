use crate::{
    lib_structs::{SectorType}
};

use crc_fast::{checksum, CrcAlgorithm};
use fearless_simd::{Bytes, Level, Simd, SimdBase, SimdMask, dispatch};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EccError {

}


/// Computes the P and Q parity of a 2352-byte sector in place, with the
/// SIMD level `ecc_level` picks.
///
/// # Panics
///
/// If `sector` isn't 2352 bytes long.
pub fn calc_ecc_simd_inplace(sector: &mut [u8]) {
    assert_eq!(sector.len(), SECTOR_LEN, "ECC needs a full 2352-byte sector");
    // The assert above makes this the whole sector, so the else never runs.
    let Some(sector) = sector.first_chunk_mut::<SECTOR_LEN>() else {
        return;
    };
    calc_ecc_at_level(ecc_level(), sector);
}

/// The best SIMD level the CPU has, except AVX2 where AVX-512 is
/// available. The kernel's time is mostly chains of dependent steps, and at
/// 64 lanes P and Q split into only 2 and 1 chains to overlap, against 3
/// and 2 at 32: AVX-512 measured 388 ns a sector, AVX2 339 ns, on Zen 5.
fn ecc_level() -> Level {
    let level = Level::new();
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if let Some(avx2) = level.as_avx2() {
        return avx2.level();
    }
    level
}

/// `calc_ecc_simd_inplace` at a given level, so tests can run each level
/// the machine has, not only the best one.
///
/// The sector is an array, not a slice, so its length reaches the kernel
/// as part of the type: `dispatch!` calls the kernel through a separate
/// function per level, and a slice's checked length wouldn't cross that
/// call without whole-program optimisation.
fn calc_ecc_at_level(level: Level, sector: &mut [u8; SECTOR_LEN]) {
    // Without SIMD the kernel would run one lane at a time; the bitwise
    // code does the same work with less bookkeeping.
    if level.is_fallback() {
        calc_ecc_bitwise(sector);
        return;
    }
    dispatch!(level, simd => ecc_kernel(simd, sector));
}

const SECTOR_LEN: usize = 2352;

/// Bytes 12..2248 of a sector, as the ECC sees them: 26 rows of 86 bytes
/// (43 little-endian words). Rows 0..24 are the header and user data,
/// rows 24 and 25 the P parity, which Q also covers.
const ROW_START: usize = 12;
const ROW_LEN: usize = 86;
const DATA_ROWS: usize = 24;
const ROWS: usize = 26;
const WORDS_PER_ROW: usize = ROW_LEN / 2;

const P_START: usize = ROW_START + DATA_ROWS * ROW_LEN;
const Q_START: usize = P_START + 2 * ROW_LEN;
/// Q parity bytes per half: 26 diagonals of one word each.
const Q_LEN: usize = 2 * ROWS;

/// Widest vector the kernel supports, in u8 lanes (AVX-512). The scratch
/// buffers are sized for it; `ecc_kernel` fails to build for a wider one.
const MAX_LANES: usize = 64;

/// Length of each Q column buffer: the column's 26 words, then repeated
/// so a load starting anywhere in the first 52 bytes never wraps.
const Q_COL_LEN: usize = Q_LEN + MAX_LANES;
// The repeat after the words is one full copy and part of another.
const _: () = assert!(Q_LEN <= MAX_LANES && MAX_LANES <= 2 * Q_LEN);


/// The P and Q parity of a 2352-byte sector, at SIMD level `simd`.
///
/// Low and high bytes of each word take the same byte-wise math, so every
/// byte is its own lane.
///
/// - P: lane j runs down column j of the 24 data rows. A row's 86 bytes
///   are contiguous, so they load straight from the sector. The last load
///   of a row runs into the next row, or for row 23 into the old P bytes;
///   those lanes are past byte 86 and dropped, and each lane's math only
///   uses that lane.
/// - Q: lane k = 2d + b is byte b of diagonal d, which at word column c
///   reads row (d + c) % 26. Each column's words are copied into a buffer
///   and repeated, so a diagonal's bytes are contiguous there.
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "fearless_simd enables the SIMD level's target features in \
              `dispatch!`; code between there and the vector operations must \
              be inlined to compile with them"
)]
fn ecc_kernel<S: Simd>(simd: S, sector: &mut [u8; SECTOR_LEN]) {
    // The vector counts for P (86 lanes) and Q (52) at each width, as
    // const parameters so the arrays of vectors have exact sizes: then the
    // loops over them unroll and the vectors stay in registers. With
    // run-time counts LLVM kept them in memory and AVX2 got 20% slower.
    match S::u8s::LEN {
        64 => ecc_kernel_with::<S, 2, 1>(simd, sector),
        32 => ecc_kernel_with::<S, 3, 2>(simd, sector),
        16 => ecc_kernel_with::<S, 6, 4>(simd, sector),
        // No fearless_simd level has another u8 width.
        _ => calc_ecc_bitwise(sector),
    }
}

/// `ecc_kernel` with `P_VECS` vectors covering a row's 86 bytes and
/// `Q_VECS` covering Q's 52.
#[inline(always)]
#[expect(clippy::inline_always, reason = "see `ecc_kernel`")]
#[expect(
    clippy::indexing_slicing,
    reason = "the sector is a 2352-byte array; \
              the furthest P load ends at 12 + 23 * 86 + 2 * 64 = 2118, \
              and the scratch buffers hold P_VECS and Q_VECS vectors"
)]
fn ecc_kernel_with<S: Simd, const P_VECS: usize, const Q_VECS: usize>(
    simd: S,
    sector: &mut [u8; SECTOR_LEN],
) {
    let lanes = S::u8s::LEN;
    // Plain asserts, not `const` ones: every arm of `ecc_kernel`'s match is
    // compiled for every level, including arms the level never takes. In
    // the arm it takes, each is constant and true, so it compiles away.
    assert!(P_VECS * lanes >= ROW_LEN && P_VECS * lanes <= 2 * MAX_LANES, "P vectors");
    assert!(Q_VECS * lanes >= Q_LEN && Q_VECS * lanes <= MAX_LANES, "Q vectors");
    let zero = S::u8s::splat(simd, 0);

    // Each LFSR step needs the one before it, so one vector's steps run one
    // after another. All of a row's vectors step together, so their chains
    // overlap.
    let mut p1 = [zero; P_VECS];
    let mut p0 = [zero; P_VECS];
    for row in 0..DATA_ROWS {
        let row_start = ROW_START + row * ROW_LEN;
        for i in 0..P_VECS {
            let at = row_start + i * lanes;
            let data = S::u8s::from_slice(simd, &sector[at..at + lanes]);
            let (x2, x3) = gf_mul_2_3::<S>(p1[i] ^ data);
            p1[i] = p0[i] ^ x3;
            p0[i] = x2;
        }
    }
    let mut p1_bytes = [0u8; 2 * MAX_LANES];
    let mut p0_bytes = [0u8; 2 * MAX_LANES];
    for i in 0..P_VECS {
        p1[i].store_slice(&mut p1_bytes[i * lanes..(i + 1) * lanes]);
        p0[i].store_slice(&mut p0_bytes[i * lanes..(i + 1) * lanes]);
    }
    sector[P_START..P_START + ROW_LEN].copy_from_slice(&p1_bytes[..ROW_LEN]);
    sector[P_START + ROW_LEN..Q_START].copy_from_slice(&p0_bytes[..ROW_LEN]);

    // Q the same way. At column c, vector i reads the column's buffer
    // (see `fill_q_column`) from (2c + i * lanes) % 52; the buffer repeats, so
    // `offset` is 2c % 52 and the load never wraps
    // (offset + Q_VECS * lanes <= 50 + 64).
    let mut q1 = [zero; Q_VECS];
    let mut q0 = [zero; Q_VECS];
    let mut offset = 0;
    // The columns are built a tile of 8 at a time into one reused buffer,
    // not all 43 up front: that array was 5 KB, zeroed with a `memset`
    // call and needing stack probes on every sector, and measured 4-11%
    // slower. A tile's columns are all written before any is loaded, so
    // the vector loads don't wait on the small stores that built them.
    let mut tile = [[0u8; Q_COL_LEN]; Q_TILE];
    for tile_start in (0..WORDS_PER_ROW).step_by(Q_TILE) {
        let width = Q_TILE.min(WORDS_PER_ROW - tile_start);
        for (t, col) in tile.iter_mut().take(width).enumerate() {
            fill_q_column(sector, tile_start + t, col);
        }
        for col in tile.iter().take(width) {
            q_step(simd, &mut q1, &mut q0, col, &mut offset);
        }
    }
    let mut q1_bytes = [0u8; MAX_LANES];
    let mut q0_bytes = [0u8; MAX_LANES];
    for i in 0..Q_VECS {
        q1[i].store_slice(&mut q1_bytes[i * lanes..(i + 1) * lanes]);
        q0[i].store_slice(&mut q0_bytes[i * lanes..(i + 1) * lanes]);
    }
    sector[Q_START..Q_START + Q_LEN].copy_from_slice(&q1_bytes[..Q_LEN]);
    sector[Q_START + Q_LEN..].copy_from_slice(&q0_bytes[..Q_LEN]);
}


/// One Q step: every vector takes column `col`'s byte for its diagonals.
/// `offset` is 2c % 52 for column c, and moves on to the next column's.
#[inline(always)]
#[expect(clippy::inline_always, reason = "see `ecc_kernel`")]
#[expect(
    clippy::indexing_slicing,
    reason = "offset <= 50 and Q_VECS * lanes <= 64, so loads end by \
              byte 114 of the 116"
)]
fn q_step<S: Simd, const Q_VECS: usize>(
    simd: S,
    q1: &mut [S::u8s; Q_VECS],
    q0: &mut [S::u8s; Q_VECS],
    col: &[u8; Q_COL_LEN],
    offset: &mut usize,
) {
    let lanes = S::u8s::LEN;
    for i in 0..Q_VECS {
        let at = *offset + i * lanes;
        let data = S::u8s::from_slice(simd, &col[at..at + lanes]);
        let (x2, x3) = gf_mul_2_3::<S>(q1[i] ^ data);
        q1[i] = q0[i] ^ x3;
        q0[i] = x2;
    }
    *offset += 2;
    if *offset == Q_LEN {
        *offset = 0;
    }
}

/// Q columns built at a time: 8 * 116 = 928 bytes of stack.
const Q_TILE: usize = 8;

/// Fills `col` with word column `c` of the sector's 26 rows (P included,
/// so P must be stored first): row r's word at bytes 2r and 2r + 1, then
/// repeated so a diagonal starting at any row reads on without wrapping.
#[inline(always)]
#[expect(clippy::inline_always, reason = "see `ecc_kernel`")]
#[expect(
    clippy::indexing_slicing,
    reason = "c < 43 and r < 26, so the furthest read ends at \
              12 + 25 * 86 + 2 * 42 + 2 = 2248"
)]
fn fill_q_column(sector: &[u8; SECTOR_LEN], c: usize, col: &mut [u8; Q_COL_LEN]) {
    let (words, repeats) = col.split_at_mut(Q_LEN);
    for (r, word) in words.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let at = ROW_START + r * ROW_LEN + 2 * c;
        word.copy_from_slice(&sector[at..at + 2]);
    }
    // Two block copies: the words again, then as many as fit.
    let (again, partly) = repeats.split_at_mut(Q_LEN);
    again.copy_from_slice(words);
    partly.copy_from_slice(&words[..partly.len()]);
}

pub fn calculate_edc(sector_data: &[u8], sector_type: SectorType) -> [u8; 4] {
    let range = match sector_type {
        SectorType::Mode1 => 0..2064,
        SectorType::Mode2Form1 => 16..2072,
        SectorType::Mode2Form2 => 16..2348,
        _ => {
            0..0 //Possibly throw an error instead.
        },
    };

    let edc_val = checksum(CrcAlgorithm::Crc32CdRomEdc, &sector_data[range]);
    (edc_val as u32).to_le_bytes()
}


pub fn gen_all_p_par_bitwise(sector_data: &[u8]) -> [u8; 172] {
    let mut p1_low = [0u8; 43];
    let mut p0_low = [0u8; 43];
    let mut p1_high = [0u8; 43];
    let mut p0_high = [0u8; 43];

    let data_slice = &sector_data[12..2076];

    for row_chunk in data_slice.chunks_exact(86) {
        for col in 0..43 {
            let val_low = row_chunk[col * 2];
            let val_high = row_chunk[col * 2 + 1];

            let (x2_l, x3_l) = gf_mul_scalar(p1_low[col] ^ val_low);
            p1_low[col] = p0_low[col] ^ x3_l;
            p0_low[col] = x2_l;

            let (x2_h, x3_h) = gf_mul_scalar(p1_high[col] ^ val_high);
            p1_high[col] = p0_high[col] ^ x3_h;
            p0_high[col] = x2_h;
        }
    }

    let mut p_parity = [0u8; 172];
    for col in 0..43 {
        p_parity[col * 2]     = p1_low[col];
        p_parity[col * 2 + 1] = p1_high[col];

        p_parity[86 + col * 2]     = p0_low[col];
        p_parity[86 + col * 2 + 1] = p0_high[col];
    }

    p_parity
}


pub fn gen_all_q_par_bitwise(sector_data: &[u8]) -> [u8; 104] {
    let mut q_parity = [0u8; 104];

    for d in 0..26 {
        let mut q1_low = 0u8;
        let mut q0_low = 0u8;
        let mut q1_high = 0u8;
        let mut q0_high = 0u8;

        let mut row = d;

        for col in 0..43 {
            let val_low;
            let val_high;

            if row < 24 {
                let idx = 12 + row * 86 + col * 2;
                val_low = sector_data[idx];
                val_high = sector_data[idx + 1];
            } else if row == 24 {
                let idx = 2076 + col * 2;
                val_low = sector_data[idx];
                val_high = sector_data[idx + 1];
            } else {
                let idx = 2162 + col * 2;
                val_low = sector_data[idx];
                val_high = sector_data[idx + 1];
            }

            let (x2_l, x3_l) = gf_mul_scalar(q1_low ^ val_low);
            q1_low = q0_low ^ x3_l;
            q0_low = x2_l;

            let (x2_h, x3_h) = gf_mul_scalar(q1_high ^ val_high);
            q1_high = q0_high ^ x3_h;
            q0_high = x2_h;

            row += 1;
            if row >= 26 { row = 0; }
        }

        q_parity[d * 2]     = q1_low;
        q_parity[d * 2 + 1] = q1_high;
        q_parity[52 + d * 2]     = q0_low;
        q_parity[52 + d * 2 + 1] = q0_high;
    }

    q_parity
}


pub fn calc_ecc_bitwise(sector_data: &mut [u8]) {
    let ecc_p = gen_all_p_par_bitwise(sector_data);
    sector_data[2076..2248].copy_from_slice(&ecc_p);

    let ecc_q = gen_all_q_par_bitwise(sector_data);
    sector_data[2248..].copy_from_slice(&ecc_q);
}


#[inline(always)]
fn gf_mul_scalar(feedback: u8) -> (u8, u8) {
    let mask = if (feedback & 0x80) != 0 { 0x1D } else { 0 };
    let x2 = (feedback << 1) ^ mask;
    (x2, x2 ^ feedback)
}

/// Multiplies each lane by 2 and by 3 in GF(2^8) (polynomial 0x11D), as
/// `gf_mul_scalar` does for one byte.
#[inline(always)]
#[expect(clippy::inline_always, reason = "see `ecc_kernel`")]
fn gf_mul_2_3<S: Simd>(feedback: S::u8s) -> (S::u8s, S::u8s) {
    // As i8, a byte with its top bit set is negative: one signed compare
    // finds the lanes whose doubling overflows and must be reduced.
    let overflows = feedback.bitcast::<S::i8s>().simd_lt(0);
    // A true lane is all ones, so AND keeps 0x1D there. Cheaper than
    // `select`, which SSE4.2 does with `blendv`.
    let reduce = overflows.to_vector().bitcast::<S::u8s>() & 0x1D;
    let times_2 = (feedback + feedback) ^ reduce;
    (times_2, times_2 ^ feedback)
}


#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const SECTOR_SIZE: usize = 2352;

    /// Sectors from a fixed-seed xorshift generator, plus solid fills. The
    /// 0x80 and 0xFF fills set the top bit of every byte, which is the case
    /// where the GF(2^8) multiply has to reduce by 0x1D.
    ///
    /// Miri gets one generated sector instead of 64. It checks memory
    /// accesses, which are the same for any sector, and with all 67 sectors
    /// the kernel tests took it about 6 minutes instead of 20 seconds.
    fn test_sectors() -> Vec<[u8; SECTOR_SIZE]> {
        let generated = if cfg!(miri) { 1 } else { 64 };
        let mut sectors = vec![
            [0x00; SECTOR_SIZE],
            [0x80; SECTOR_SIZE],
            [0xFF; SECTOR_SIZE],
        ];

        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..generated {
            let mut sector = [0_u8; SECTOR_SIZE];
            for byte in &mut sector {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state.to_be_bytes()[0];
            }
            sectors.push(sector);
        }

        sectors
    }

    /// Every level this machine can run: the best one, each lower x86
    /// level it implies, and the fallback.
    fn levels() -> Vec<(&'static str, Level)> {
        let best = Level::new();
        let mut levels = vec![("best", best), ("fallback", Level::fallback())];
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            levels.extend(best.as_sse2().map(|simd| ("sse2", simd.level())));
            levels.extend(best.as_sse4_2().map(|simd| ("sse4.2", simd.level())));
            levels.extend(best.as_avx2().map(|simd| ("avx2", simd.level())));
        }
        levels
    }

    #[test]
    fn ecc_kernel_matches_bitwise_at_every_level() {
        for (name, level) in levels() {
            for (i, sector) in test_sectors().iter().enumerate() {
                let mut expected = *sector;
                calc_ecc_bitwise(&mut expected);

                let mut actual = *sector;
                calc_ecc_at_level(level, &mut actual);

                assert!(
                    actual[2076..2248] == expected[2076..2248],
                    "{name}, sector {i}: P parity differs from calc_ecc_bitwise"
                );
                assert!(
                    actual[2248..] == expected[2248..],
                    "{name}, sector {i}: Q parity differs from calc_ecc_bitwise"
                );
            }
        }
    }

    #[test]
    #[should_panic(expected = "ECC needs a full 2352-byte sector")]
    fn calc_ecc_simd_inplace_short_sector_panics() {
        let mut sector = [0_u8; 2300];
        calc_ecc_simd_inplace(&mut sector);
    }

    /// The CD-ROM EDC computed one bit at a time: CRC-32 with the reflected
    /// polynomial 0xD8018001 (x^32 + x^31 + x^16 + x^15 + x^4 + x^3 + x + 1),
    /// initial value 0 and no final XOR (ECMA-130 section 14.3). Written
    /// independently of `crc-fast` so `calculate_edc` isn't checked against
    /// itself.
    fn edc_reference(data: &[u8]) -> [u8; 4] {
        let mut crc = 0_u32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xD801_8001
                } else {
                    crc >> 1
                };
            }
        }
        crc.to_le_bytes()
    }

    /// Multiplication in GF(2^8) with the CD-ROM field polynomial
    /// x^8 + x^4 + x^3 + x^2 + 1 (0x11D), by shift and add.
    fn gf_mul_reference(mut a: u8, mut b: u8) -> u8 {
        let mut product = 0_u8;
        while b != 0 {
            if b & 1 == 1 {
                product ^= a;
            }
            let carry = a & 0x80 != 0;
            a <<= 1;
            if carry {
                a ^= 0x1D;
            }
            b >>= 1;
        }
        product
    }

    #[test]
    fn gf_mul_scalar_returns_times_2_and_times_3() {
        for feedback in 0..=u8::MAX {
            assert_eq!(
                gf_mul_scalar(feedback),
                (
                    gf_mul_reference(feedback, 2),
                    gf_mul_reference(feedback, 3)
                ),
                "feedback {feedback:#04x}"
            );
        }
    }

    #[test]
    fn edc_of_an_unsupported_type_is_the_crc_of_nothing() {
        let sector = [0xA5_u8; SECTOR_SIZE];
        for sector_type in [
            SectorType::Audio,
            SectorType::Mode1Exception,
            SectorType::PregapMode1,
            SectorType::ZeroedMode1Data,
            SectorType::None,
        ] {
            assert_eq!(calculate_edc(&sector, sector_type), [0; 4]);
        }
    }

    proptest! {
        #[test]
        fn calculate_edc_matches_the_bitwise_crc(
            body in proptest::collection::vec(any::<u8>(), SECTOR_SIZE),
        ) {
            for (sector_type, range) in [
                (SectorType::Mode1, 0..2064),
                (SectorType::Mode2Form1, 16..2072),
                (SectorType::Mode2Form2, 16..2348),
            ] {
                prop_assert_eq!(
                    calculate_edc(&body, sector_type),
                    edc_reference(&body[range]),
                    "{:?}", sector_type
                );
            }
        }
    }
}
