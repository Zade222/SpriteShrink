use crate::{
    lib_structs::{SectorType}
};

use crc_fast::{checksum, CrcAlgorithm};
use pulp::{Arch, Simd, WithSimd};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EccError {

}


pub fn calc_ecc_simd_inplace(sector: &mut [u8]) {
    Arch::new().dispatch(EccKernel(sector));
}


/// Widest vector the ECC kernel supports, in u8 lanes (AVX2). Each scratch
/// buffer has this much room after its last load offset, so a full-width
/// vector load never reads past the end.
///
/// pulp only selects AVX-512 (64 lanes) with its `x86-v4` feature, which this
/// crate doesn't enable. Padding for 64 lanes measured 0.5-2% slower per
/// sector, all of it spent zeroing padding that AVX2 never reads. Raise this
/// to 64 if `x86-v4` is enabled; the assert in `EccKernel` fails the build
/// until then.
const MAX_LANES: usize = 32;

/// Length of each Q column buffer. A column holds 26 bytes (24 data rows and
/// two P parity bytes) followed by a copy of them so a diagonal can wrap.
/// Loads start at offsets 0..26, so this leaves room for a full-width load.
const Q_COL_LEN: usize = 26 + MAX_LANES;

/// Computes the P and Q parity of a 2352-byte sector in place.
///
/// The kernel is generic over the SIMD level so tests can run each level
/// directly, not only the one `Arch::new()` picks on the test machine.
struct EccKernel<'a>(&'a mut [u8]);

impl WithSimd for EccKernel<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) -> Self::Output {
        // The parity is written through raw pointers up to byte 2352.
        assert_eq!(self.0.len(), 2352, "ECC needs a full 2352-byte sector");

        // pulp's `Scalar` level uses `bool` masks, so `transmute_u8s_m8s`
        // turns true into 0x01 instead of 0xFF and `gf_mul_simd` reduces by
        // the wrong value. With one lane the bitwise code does the same work.
        if S::IS_SCALAR {
            return calc_ecc_bitwise(self.0);
        }
        const { assert!(size_of::<S::u8s>() <= MAX_LANES) };

        let sector = self.0;
        let lanes = S::U8_LANES;

        let mut low_data = [0u8; 1032 + MAX_LANES];
        let mut high_data = [0u8; 1032 + MAX_LANES];

        for (i, chunk) in sector[12..2076].chunks_exact(2).enumerate() {
            low_data[i] = chunk[0];
            high_data[i] = chunk[1];
        }

        let mut low_p = [0u8; 86];
        let mut high_p = [0u8; 86];
        let mut temp_out_p1 = [0u8; MAX_LANES];
        let mut temp_out_p0 = [0u8; MAX_LANES];

        let p1_dst = unsafe { sector.as_mut_ptr().add(2076) as *mut u16 };
        let p0_dst = unsafe { sector.as_mut_ptr().add(2162) as *mut u16 };

        for chunk_start in (0..43).step_by(lanes) {
            let chunk_len = std::cmp::min(chunk_start + lanes, 43) - chunk_start;

            let (mut p1v_l, mut p0v_l) = (simd.splat_u8s(0), simd.splat_u8s(0));
            let (mut p1v_h, mut p0v_h) = (simd.splat_u8s(0), simd.splat_u8s(0));

            for row in 0..24 {
                let offset = row * 43 + chunk_start;
                debug_assert!(offset + size_of::<S::u8s>() <= low_data.len());

                // Low Plane
                let ptr_l = low_data.as_ptr().wrapping_add(offset) as *const S::u8s;
                let d_l = unsafe { ptr_l.read_unaligned() };
                let feedback_l = simd.xor_u8s(p1v_l, d_l);
                let (x2_l, x3_l) = gf_mul_simd(simd, feedback_l);
                p1v_l = simd.xor_u8s(p0v_l, x3_l);
                p0v_l = x2_l;

                // High Plane
                let ptr_h = high_data.as_ptr().wrapping_add(offset) as *const S::u8s;
                let d_h = unsafe { ptr_h.read_unaligned() };
                let feedback_h = simd.xor_u8s(p1v_h, d_h);
                let (x2_h, x3_h) = gf_mul_simd(simd, feedback_h);
                p1v_h = simd.xor_u8s(p0v_h, x3_h);
                p0v_h = x2_h;
            }

            unsafe {
                (temp_out_p1.as_mut_ptr() as *mut S::u8s).write_unaligned(p1v_l);
                (temp_out_p0.as_mut_ptr() as *mut S::u8s).write_unaligned(p0v_l);
            }
            for k in 0..chunk_len {
                low_p[chunk_start + k] = temp_out_p1[k];
                low_p[chunk_start + k + 43] = temp_out_p0[k];
            }

            unsafe {
                (temp_out_p1.as_mut_ptr() as *mut S::u8s).write_unaligned(p1v_h);
                (temp_out_p0.as_mut_ptr() as *mut S::u8s).write_unaligned(p0v_h);
            }

            for k in 0..chunk_len {
                let c = chunk_start + k;
                high_p[c] = temp_out_p1[k];
                high_p[c + 43] = temp_out_p0[k];

                let p1_val = (low_p[c] as u16) | ((high_p[c] as u16) << 8);
                let p0_val = (low_p[c + 43] as u16) | ((high_p[c + 43] as u16) << 8);

                unsafe {
                    p1_dst.add(c).write_unaligned(p1_val.to_le());
                    p0_dst.add(c).write_unaligned(p0_val.to_le());
                }
            }
        }

        let mut low_cols = [[0u8; Q_COL_LEN]; 43];
        let mut high_cols = [[0u8; Q_COL_LEN]; 43];

        for c in 0..43 {
            for r in 0..24 {
                low_cols[c][r] = low_data[r * 43 + c];
                high_cols[c][r] = high_data[r * 43 + c];
            }
            low_cols[c][24] = low_p[c];
            low_cols[c][25] = low_p[c + 43];
            high_cols[c][24] = high_p[c];
            high_cols[c][25] = high_p[c + 43];

            let (src_l, dst_l) = low_cols[c].split_at_mut(26);
            dst_l[..26].copy_from_slice(&src_l[..26]);

            let (src_h, dst_h) = high_cols[c].split_at_mut(26);
            dst_h[..26].copy_from_slice(&src_h[..26]);
        }

        let q1_dst = unsafe { sector.as_mut_ptr().add(2248) as *mut u16 };
        let q0_dst = unsafe { sector.as_mut_ptr().add(2300) as *mut u16 };

        let mut temp_out_q1_h = [0u8; MAX_LANES];
        let mut temp_out_q0_h = [0u8; MAX_LANES];

        for diag_start in (0..26).step_by(lanes) {
            let chunk_len = std::cmp::min(diag_start + lanes, 26) - diag_start;
            let (mut q1v_l, mut q0v_l) = (simd.splat_u8s(0), simd.splat_u8s(0));
            let (mut q1v_h, mut q0v_h) = (simd.splat_u8s(0), simd.splat_u8s(0));

            for c in 0..43 {
                let row_offset = (diag_start + c) % 26;
                debug_assert!(row_offset + size_of::<S::u8s>() <= Q_COL_LEN);

                let ptr_l = low_cols[c].as_ptr().wrapping_add(row_offset) as *const S::u8s;
                let d_l = unsafe { ptr_l.read_unaligned() };
                let feedback_l = simd.xor_u8s(q1v_l, d_l);
                let (x2_l, x3_l) = gf_mul_simd(simd, feedback_l);
                q1v_l = simd.xor_u8s(q0v_l, x3_l);
                q0v_l = x2_l;

                let ptr_h = high_cols[c].as_ptr().wrapping_add(row_offset) as *const S::u8s;
                let d_h = unsafe { ptr_h.read_unaligned() };
                let feedback_h = simd.xor_u8s(q1v_h, d_h);
                let (x2_h, x3_h) = gf_mul_simd(simd, feedback_h);
                q1v_h = simd.xor_u8s(q0v_h, x3_h);
                q0v_h = x2_h;
            }

            unsafe {
                (temp_out_q1_h.as_mut_ptr() as *mut S::u8s).write_unaligned(q1v_h);
                (temp_out_q0_h.as_mut_ptr() as *mut S::u8s).write_unaligned(q0v_h);
            }

            unsafe {
                (temp_out_p1.as_mut_ptr() as *mut S::u8s).write_unaligned(q1v_l);
                (temp_out_p0.as_mut_ptr() as *mut S::u8s).write_unaligned(q0v_l);
            }

            for k in 0..chunk_len {
                let d = diag_start + k;

                let l_q1 = temp_out_p1[k];
                let l_q0 = temp_out_p0[k];
                let h_q1 = temp_out_q1_h[k];
                let h_q0 = temp_out_q0_h[k];

                let q1_val = (l_q1 as u16) | ((h_q1 as u16) << 8);
                let q0_val = (l_q0 as u16) | ((h_q0 as u16) << 8);

                unsafe {
                    q1_dst.add(d).write_unaligned(q1_val.to_le());
                    q0_dst.add(d).write_unaligned(q0_val.to_le());
                }
            }
        }
    }
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


pub fn sector_ecc_check_bitwise(
    sector_data: &[u8],
    p_parity: &[u8],
    q_parity: &[u8],
    sector_type: SectorType
) -> bool {
    let mut test_sector_data = [0u8; 2352];
    test_sector_data.copy_from_slice(sector_data);
    if sector_type == SectorType::Mode2Form1 {
        test_sector_data[12..16].copy_from_slice(&[0u8; 4]);
    }

    let ecc_p = gen_all_p_par_bitwise(&test_sector_data);

    let ecc_q = gen_all_q_par_bitwise(&test_sector_data);

    ecc_p == *p_parity && ecc_q == *q_parity
}


pub fn sector_edc_check(
    sector_data: &[u8],
    crc_bytes: &[u8],
    sector_type: SectorType
) -> bool {
    let checksum = calculate_edc(sector_data, sector_type);

    *crc_bytes == checksum
}


#[inline(always)]
fn gf_mul_scalar(feedback: u8) -> (u8, u8) {
    let mask = if (feedback & 0x80) != 0 { 0x1D } else { 0 };
    let x2 = (feedback << 1) ^ mask;
    (x2, x2 ^ feedback)
}

/// Assumes a true `m8s` lane is all ones. pulp's `Scalar` level breaks that,
/// so `EccKernel` never calls this at that level.
#[inline(always)]
fn gf_mul_simd<S: Simd>(simd: S, feedback: S::u8s) -> (S::u8s, S::u8s) {
    let x80 = simd.splat_u8s(0x80);
    let x1d = simd.splat_u8s(0x1D);

    let msb = simd.and_u8s(feedback, x80);
    let mask_m = simd.equal_u8s(msb, x80);
    let mask = simd.transmute_u8s_m8s(mask_m);

    // (feedback << 1)
    let f_shl1 = simd.add_u8s(feedback, feedback);

    // (mask & 0x1D)
    let term2 = simd.and_u8s(mask, x1d);

    let x2 = simd.xor_u8s(f_shl1, term2);
    (x2, simd.xor_u8s(x2, feedback))
}


#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use pulp::{Scalar, Scalar128b, Scalar256b};

    use super::*;

    const SECTOR_SIZE: usize = 2352;

    /// Sectors from a fixed-seed xorshift generator, plus solid fills. The
    /// 0x80 and 0xFF fills set the top bit of every byte, which is the case
    /// where the GF(2^8) multiply has to reduce by 0x1D.
    fn test_sectors() -> Vec<[u8; SECTOR_SIZE]> {
        let mut sectors = vec![
            [0x00; SECTOR_SIZE],
            [0x80; SECTOR_SIZE],
            [0xFF; SECTOR_SIZE],
        ];

        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..64 {
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

    /// Runs the kernel at one SIMD level and checks its parity against
    /// `calc_ecc_bitwise` for every test sector.
    fn assert_level_matches_bitwise<S: Simd>(simd: S) {
        for (i, sector) in test_sectors().iter().enumerate() {
            let mut expected = *sector;
            calc_ecc_bitwise(&mut expected);

            let mut actual = *sector;
            simd.vectorize(EccKernel(&mut actual));

            assert!(
                actual[2076..2248] == expected[2076..2248],
                "sector {i}: P parity differs from calc_ecc_bitwise"
            );
            assert!(
                actual[2248..] == expected[2248..],
                "sector {i}: Q parity differs from calc_ecc_bitwise"
            );
        }
    }

    #[test]
    fn ecc_kernel_scalar_level_matches_bitwise() {
        assert_level_matches_bitwise(Scalar::new());
    }

    #[test]
    fn ecc_kernel_16_lanes_matches_bitwise() {
        assert_level_matches_bitwise(Scalar128b);
    }

    #[test]
    fn ecc_kernel_32_lanes_matches_bitwise() {
        assert_level_matches_bitwise(Scalar256b);
    }

    // V2 and V3 use real SSE4.2 and AVX2 instructions, so they only run on
    // CPUs that have them. The ScalarNb levels above cover the same widths
    // on every machine.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn ecc_kernel_v2_matches_bitwise() {
        if let Some(simd) = pulp::x86::V2::try_new() {
            assert_level_matches_bitwise(simd);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn ecc_kernel_v3_matches_bitwise() {
        if let Some(simd) = pulp::x86::V3::try_new() {
            assert_level_matches_bitwise(simd);
        }
    }

    #[test]
    fn calc_ecc_simd_inplace_matches_bitwise() {
        for (i, sector) in test_sectors().iter().enumerate() {
            let mut expected = *sector;
            calc_ecc_bitwise(&mut expected);

            let mut actual = *sector;
            calc_ecc_simd_inplace(&mut actual);

            assert!(actual == expected, "sector {i}");
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

    /// A sector with a random body and valid P and Q parity.
    fn sector_with_ecc(body: &[u8]) -> [u8; SECTOR_SIZE] {
        let mut sector = [0_u8; SECTOR_SIZE];
        sector.copy_from_slice(body);
        calc_ecc_bitwise(&mut sector);
        sector
    }

    fn ecc_check(sector: &[u8; SECTOR_SIZE], sector_type: SectorType) -> bool {
        sector_ecc_check_bitwise(
            sector,
            &sector[2076..2248],
            &sector[2248..],
            sector_type,
        )
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

    #[test]
    fn a_header_change_breaks_mode1_ecc_but_not_mode2_form1() {
        // Mode 2 Form 1 parity is computed with the header zeroed, so a
        // valid sector stays valid whatever its header says. Mode 1 parity
        // covers the header.
        let mut sector = sector_with_ecc(&test_sectors()[3]);
        assert!(ecc_check(&sector, SectorType::Mode1));

        let mut zero_header = sector;
        zero_header[12..16].fill(0);
        calc_ecc_bitwise(&mut zero_header);
        sector[2076..].copy_from_slice(&zero_header[2076..]);
        assert!(ecc_check(&sector, SectorType::Mode2Form1));

        sector[12..16].copy_from_slice(&[0x00, 0x02, 0x16, 0x02]);
        assert!(!ecc_check(&sector, SectorType::Mode1));
        assert!(ecc_check(&sector, SectorType::Mode2Form1));
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

        #[test]
        fn mode1_ecc_check_rejects_any_changed_byte_after_the_sync(
            body in proptest::collection::vec(any::<u8>(), SECTOR_SIZE),
            index in 12..SECTOR_SIZE,
            flip in 1..=u8::MAX,
        ) {
            // The P and Q codewords cover every byte from the header to
            // the end. A distance-3 Reed-Solomon code detects any single
            // wrong symbol, so one changed byte must always fail.
            let mut sector = sector_with_ecc(&body);
            prop_assert!(ecc_check(&sector, SectorType::Mode1));

            sector[index] ^= flip;
            prop_assert!(!ecc_check(&sector, SectorType::Mode1));
        }

        #[test]
        fn mode2_form1_ecc_check_rejects_any_changed_byte_after_the_header(
            body in proptest::collection::vec(any::<u8>(), SECTOR_SIZE),
            index in 16..SECTOR_SIZE,
            flip in 1..=u8::MAX,
        ) {
            let mut body = body;
            body[12..16].fill(0);
            let mut sector = sector_with_ecc(&body);
            prop_assert!(ecc_check(&sector, SectorType::Mode2Form1));

            sector[index] ^= flip;
            prop_assert!(!ecc_check(&sector, SectorType::Mode2Form1));
        }
    }
}
