//! Reversible 5-3 integer wavelet inverse transform (ISO 15444-1 Annex F).
//!
//! Port of OpenJPEG 1.5.2 `dwt.c` `dwt_decode_tile` + `dwt_decode_1_` +
//! `dwt_interleave_h/v` — integer lifting, exact (no rounding loss).

/// One 1-D inverse 5-3 lifting step over `mem[0..sn+dn]` (OpenJPEG
/// `dwt_decode_1_`): `S(i)` is at even indices, `D(i)` at odd.
///
/// `cas` handles the odd-length-coordinate case where low/high subbands
/// swap positions in the interleaved buffer.
fn dwt_decode_1(mem: &mut [i32], sn: usize, dn: usize, cas: usize) {
    let clamp_low = |i: isize, hi: isize| -> isize {
        if i < 0 {
            0
        } else if i > hi {
            hi
        } else {
            i
        }
    };

    if cas == 0 {
        if dn > 0 || sn > 1 {
            // S(i) -= (D_(i-1) + D_(i) + 2) >> 2 — D unmodified here
            for i in 0..sn {
                let dm1 = mem[2 * clamp_low(i as isize - 1, dn as isize - 1) as usize + 1];
                let d0 = mem[2 * clamp_low(i as isize, dn as isize - 1) as usize + 1];
                mem[i * 2] -= (dm1 + d0 + 2) >> 2;
            }
            // D(i) += (S_(i) + S_(i+1)) >> 1 — S final after the update above
            for i in 0..dn {
                let s0 = mem[2 * clamp_low(i as isize, sn as isize - 1) as usize];
                let s1 = mem[2 * clamp_low(i as isize + 1, sn as isize - 1) as usize];
                mem[i * 2 + 1] += (s0 + s1) >> 1;
            }
        }
    } else if sn == 0 && dn == 1 {
        // single odd-length column/row
        mem[0] /= 2;
    } else {
        // cas 1: S occupies the even positions (dn values), D the odd
        // positions (sn values) — counts swap relative to cas 0.
        // D(i) -= (SS_(i) + SS_(i+1) + 2) >> 2 — S unmodified here
        for i in 0..sn {
            let s0 = mem[2 * clamp_low(i as isize, dn as isize - 1) as usize];
            let s1 = mem[2 * clamp_low(i as isize + 1, dn as isize - 1) as usize];
            mem[i * 2 + 1] -= (s0 + s1 + 2) >> 2;
        }
        // S(i) += (DD_(i) + DD_(i-1)) >> 1 — D final after the update above
        for i in 0..dn {
            let dm1 = mem[2 * clamp_low(i as isize - 1, sn as isize - 1) as usize + 1];
            let d0 = mem[2 * clamp_low(i as isize, sn as isize - 1) as usize + 1];
            mem[i * 2] += (dm1 + d0) >> 1;
        }
    }
}

/// Inverse lazy transform (horizontal) — `dwt_interleave_h`: de-interleave
/// the tile row into `mem[cas + 2*i]` (low, `sn` values) and
/// `mem[1-cas + 2*i]` (high, `dn` values).
fn interleave_h(mem: &mut [i32], row: &[i32], sn: usize, dn: usize, cas: usize) {
    for (i, &v) in row[..sn].iter().enumerate() {
        mem[cas + 2 * i] = v;
    }
    for (i, &v) in row[sn..sn + dn].iter().enumerate() {
        mem[1 - cas + 2 * i] = v;
    }
}

/// Inverse lazy transform (vertical) — `dwt_interleave_v` over a column of
/// the tile (stride `w`).
fn interleave_v(mem: &mut [i32], col: impl Iterator<Item = i32>, sn: usize, dn: usize, cas: usize) {
    let vals: Vec<i32> = col.collect();
    for (i, &v) in vals[..sn].iter().enumerate() {
        mem[cas + 2 * i] = v;
    }
    for (i, &v) in vals[sn..sn + dn].iter().enumerate() {
        mem[1 - cas + 2 * i] = v;
    }
}

/// `dwt_decode`: lift the tile-component buffer from `numres - 1` up to
/// `numres` resolution levels. `res_dims[res]` = `(x0, y0, x1, y1)` of each
/// resolution level in tile-component coordinates.
pub fn idwt(data: &mut [i32], w: usize, res_dims: &[(i32, i32, i32, i32)], numres: usize) {
    if numres < 2 {
        return;
    }
    let max_dim = res_dims[..numres]
        .iter()
        .map(|r| ((r.2 - r.0).max(r.3 - r.1)) as usize)
        .max()
        .unwrap_or(0)
        .max(2);
    let mut mem = vec![0i32; max_dim];

    let mut rw = (res_dims[0].2 - res_dims[0].0) as usize;
    let mut rh = (res_dims[0].3 - res_dims[0].1) as usize;

    for &(x0, y0, x1, y1) in &res_dims[1..numres] {
        let n_rw = (x1 - x0) as usize;
        let n_rh = (y1 - y0) as usize;
        let sn_h = rw;
        let dn_h = n_rw - sn_h;
        let cas_h = (x0 % 2) as usize;

        // horizontal lifting over every row
        for j in 0..n_rh {
            let row_start = j * w;
            let row: Vec<i32> = data[row_start..row_start + n_rw].to_vec();
            interleave_h(&mut mem, &row, sn_h, dn_h, cas_h);
            dwt_decode_1(&mut mem, sn_h, dn_h, cas_h);
            data[row_start..row_start + n_rw].copy_from_slice(&mem[..n_rw]);
        }

        // vertical lifting over every column
        let sn_v = rh;
        let dn_v = n_rh - sn_v;
        let cas_v = (y0 % 2) as usize;
        for j in 0..n_rw {
            interleave_v(
                &mut mem,
                (0..n_rh).map(|k| data[k * w + j]),
                sn_v,
                dn_v,
                cas_v,
            );
            dwt_decode_1(&mut mem, sn_v, dn_v, cas_v);
            for k in 0..n_rh {
                data[k * w + j] = mem[k];
            }
        }

        rw = n_rw;
        rh = n_rh;
    }
}

/// `dwt_getgain` — subband gain for the reversible path: 0 (LL), 1 (HL/LH),
/// 2 (HH) — used for the dequantisation bit-plane count of irreversible
/// bands and mirrored in `tcd.c`.
pub fn gain(bandno: u8) -> u32 {
    match bandno {
        0 => 0,
        1 | 2 => 1,
        _ => 2,
    }
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `int_ceildivpow2(a, b)` — ceiling(a / 2^b) with C division semantics.
    fn ceildivpow2(a: i32, b: u32) -> i32 {
        ((a + (1i32 << b) - 1) >> b) as i32
    }

    /// Perfect-reconstruction check: encode with the inverse of
    /// `dwt_decode_1_` (derived lifting order) and decode with ours; the
    /// original must come back.
    fn forward_1d(a: &mut [i32], sn: usize, dn: usize, cas: usize) {
        let cl = |i: isize, hi: isize| -> usize { i.clamp(0, hi) as usize };
        if cas == 0 {
            if dn > 0 || sn > 1 {
                // undo decode step 2: D(i) -= (S_(i) + S_(i+1)) >> 1
                for i in 0..dn {
                    let s0 = a[2 * cl(i as isize, sn as isize - 1)];
                    let s1 = a[2 * cl(i as isize + 1, sn as isize - 1)];
                    a[2 * i + 1] -= (s0 + s1) >> 1;
                }
                // undo decode step 1: S(i) += (D_(i-1) + D_(i) + 2) >> 2
                for i in 0..sn {
                    let dm1 = a[2 * cl(i as isize - 1, dn as isize - 1) + 1];
                    let d0 = a[2 * cl(i as isize, dn as isize - 1) + 1];
                    a[2 * i] += (dm1 + d0 + 2) >> 2;
                }
            }
        } else if sn == 0 && dn == 1 {
            a[0] *= 2;
        } else {
            // cas 1: undo decode step 2 first (S -= g(D)), then step 1
            for i in 0..dn {
                let dm1 = a[2 * cl(i as isize - 1, sn as isize - 1) + 1];
                let d0 = a[2 * cl(i as isize, sn as isize - 1) + 1];
                a[2 * i] -= (dm1 + d0) >> 1;
            }
            for i in 0..sn {
                let s0 = a[2 * cl(i as isize, dn as isize - 1)];
                let s1 = a[2 * cl(i as isize + 1, dn as isize - 1)];
                a[2 * i + 1] += (s0 + s1 + 2) >> 2;
            }
        }
    }

    fn roundtrip_1d(cas: usize) {
        // sn = 3, dn = 2 (total 5), cas 0; sn = 2, dn = 3 for cas 1
        let (sn, dn) = if cas == 0 { (3, 2) } else { (2, 3) };
        let n = sn + dn;
        let orig: Vec<i32> = (0..n as i32 * 37 + 11).take(n).collect();
        // place: low band at cas+2i, high at 1-cas+2i
        let mut buf = vec![0i32; n];
        for i in 0..sn {
            buf[cas + 2 * i] = orig[i];
        }
        for i in 0..dn {
            buf[1 - cas + 2 * i] = orig[sn + i];
        }
        forward_1d(&mut buf, sn, dn, cas);
        dwt_decode_1(&mut buf, sn, dn, cas);
        for i in 0..sn {
            assert_eq!(buf[cas + 2 * i], orig[i], "cas={cas} low {i}");
        }
        for i in 0..dn {
            assert_eq!(buf[1 - cas + 2 * i], orig[sn + i], "cas={cas} high {i}");
        }
    }

    #[test]
    fn roundtrip_cas0() {
        roundtrip_1d(0);
    }

    #[test]
    fn roundtrip_cas1() {
        roundtrip_1d(1);
    }

    /// Flat lowpass with zero detail must decode unchanged (lossless).
    #[test]
    fn idwt_constant_image_lossless() {
        let w = 16usize;
        let mut data = vec![0i32; 256];
        // res0 = 8x8 lowpass = 6; detail bands zero
        for j in 0..8 {
            for i in 0..8 {
                data[j * w + i] = 6;
            }
        }
        let res_dims = [(0, 0, 8, 8), (0, 0, 16, 16)];
        idwt(&mut data, w, &res_dims, 2);
        assert!(data.iter().all(|&v| v == 6), "expected 6 everywhere");
    }

    #[test]
    fn single_resolution_is_noop() {
        let mut data = vec![7i32; 25];
        let res_dims = [(0, 0, 5, 5)];
        idwt(&mut data, 5, &res_dims, 1);
        assert!(data.iter().all(|&v| v == 7));
    }

    #[test]
    fn gain_table() {
        assert_eq!(gain(0), 0);
        assert_eq!(gain(1), 1);
        assert_eq!(gain(2), 1);
        assert_eq!(gain(3), 2);
    }

    #[test]
    fn ceildivpow2_matches_c() {
        assert_eq!(ceildivpow2(7, 1), 4);
        assert_eq!(ceildivpow2(8, 1), 4);
        assert_eq!(ceildivpow2(-7, 1), -3);
        assert_eq!(ceildivpow2(0, 3), 0);
    }
}
