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
    struct Calculator<'a>(&'a mut [u8]);
    impl<'a> WithSimd for Calculator<'a> {
        type Output = ();

        #[inline(always)]
        fn with_simd<S: Simd>(self, simd: S) -> Self::Output {
            let sector = self.0;
            let lanes = S::U8_LANES;

            let mut low_data = [0u8; 1032];
            let mut high_data = [0u8; 1032];

            for (i, chunk) in sector[12..2076].chunks_exact(2).enumerate() {
                low_data[i] = chunk[0];
                high_data[i] = chunk[1];
            }

            let mut low_p = [0u8; 86];
            let mut high_p = [0u8; 86];
            let mut temp_out_p1 = [0u8; 64];
            let mut temp_out_p0 = [0u8; 64];

            let p1_dst = unsafe { sector.as_mut_ptr().add(2076) as *mut u16 };
            let p0_dst = unsafe { sector.as_mut_ptr().add(2162) as *mut u16 };

            for chunk_start in (0..43).step_by(lanes) {
                let chunk_len = std::cmp::min(chunk_start + lanes, 43) - chunk_start;

                let (mut p1v_l, mut p0v_l) = (simd.splat_u8s(0), simd.splat_u8s(0));
                let (mut p1v_h, mut p0v_h) = (simd.splat_u8s(0), simd.splat_u8s(0));

                for row in 0..24 {
                    let offset = row * 43 + chunk_start;

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

            let mut low_cols = [[0u8; 64]; 43];
            let mut high_cols = [[0u8; 64]; 43];

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

            let mut temp_out_q1_h = [0u8; 64];
            let mut temp_out_q0_h = [0u8; 64];

            for diag_start in (0..26).step_by(lanes) {
                let chunk_len = std::cmp::min(diag_start + lanes, 26) - diag_start;
                let (mut q1v_l, mut q0v_l) = (simd.splat_u8s(0), simd.splat_u8s(0));
                let (mut q1v_h, mut q0v_h) = (simd.splat_u8s(0), simd.splat_u8s(0));

                for c in 0..43 {
                    let row_offset = (diag_start + c) % 26;

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
    Arch::new().dispatch(Calculator(sector));
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
