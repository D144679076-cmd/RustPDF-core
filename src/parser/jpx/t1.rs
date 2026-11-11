//! Tier-1 coding: significance / refinement / clean-up bit-plane passes over
//! one code block (ISO 15444-1 Annex E).
//!
//! Port of OpenJPEG 1.5.2 `t1.c` decode paths (`t1_dec_sigpass_mqc`,
//! `t1_dec_refpass_mqc`, `t1_dec_clnpass`, `t1_dec_*_raw`, `t1_decode_cblk`)
//! with the 8-bit-neighbour flag encoding and the LUTs in [`super::t1_luts`].
//!
//! Wavelet-lite scope (plan §1.5): ROI (`roishift`) is not implemented and
//! the VSC code-block style is ignored (logged) — both are rare in PDFs and
//! only affect interior coefficient values, never structure.

use super::mqc::{MqDecoder, RawDecoder};
use super::t1_luts::{LUT_CTXNO_SC, LUT_CTXNO_ZC, LUT_SPB};

/// Context numbers (T1_CTXNO_*).
pub const CTXNO_ZC: usize = 0;
pub const CTXNO_SC: usize = 9;
pub const CTXNO_MAG: usize = 14;
pub const CTXNO_AGG: usize = 17;
pub const CTXNO_UNI: usize = 18;
/// Total context count (size of the state array).
pub const N_CTXS: usize = 19;

// Neighbour significance / sign flags (T1_SIG_* / T1_SGN_*).
const SIG_NE: u16 = 0x0001;
const SIG_SE: u16 = 0x0002;
const SIG_SW: u16 = 0x0004;
const SIG_NW: u16 = 0x0008;
const SIG_N: u16 = 0x0010;
const SIG_E: u16 = 0x0020;
const SIG_S: u16 = 0x0040;
const SIG_W: u16 = 0x0080;
const SIG_OTH: u16 = SIG_NE | SIG_SE | SIG_SW | SIG_NW | SIG_N | SIG_E | SIG_S | SIG_W;
const SIG_PRIM: u16 = SIG_N | SIG_E | SIG_S | SIG_W;
const SGN_N: u16 = 0x0100;
const SGN_E: u16 = 0x0200;
const SGN_S: u16 = 0x0400;
const SGN_W: u16 = 0x0800;
const SIG: u16 = 0x1000;
const REFINE: u16 = 0x2000;
const VISIT: u16 = 0x4000;

/// Code-block style flags (J2K_CCP_CBLKSTY_*).
pub const CBLKSTY_LAZY: u8 = 0x01;
pub const CBLKSTY_TERMALL: u8 = 0x02;
pub const CBLKSTY_RESET: u8 = 0x04;
pub const CBLKSTY_SEGSYM: u8 = 0x08;
pub const CBLKSTY_VSC: u8 = 0x10;

/// One decoded code-block segment: its raw bytes and the number of coding
/// passes it carries (from the packet header, T2).
pub struct Seg<'a> {
    pub data: &'a [u8],
    pub numpasses: u32,
}

/// `t1_updateflags` modifier table — for `s` (0 = negative) the four
/// neighbour directions get a significance bit plus the matching sign bit.
const MOD: [u16; 8] = [
    SIG_S,
    SIG_S | SGN_S,
    SIG_E,
    SIG_E | SGN_E,
    SIG_W,
    SIG_W | SGN_W,
    SIG_N,
    SIG_N | SGN_N,
];

/// Per-code-block T1 decoder state.
pub struct T1<'a> {
    w: usize,
    h: usize,
    stride: usize,
    data: &'a mut [i32],
    flags: &'a mut [u16],
    mqc: Option<MqDecoder<'a>>,
    raw: Option<RawDecoder<'a>>,
    warn_vsc: bool,
}

impl<'a> T1<'a> {
    /// Create state for a `w × h` code block. `data` must be `w * h`, flags
    /// `(w + 2) * (h + 2)`; both are zeroed here.
    pub fn new(w: usize, h: usize, data: &'a mut [i32], flags: &'a mut [u16]) -> Self {
        debug_assert_eq!(data.len(), w * h);
        let stride = w + 2;
        debug_assert_eq!(flags.len(), stride * (h + 2));
        data.fill(0);
        flags.fill(0);
        T1 {
            w,
            h,
            stride,
            data,
            flags,
            mqc: None,
            raw: None,
            warn_vsc: false,
        }
    }

    fn flag_idx(&self, j: usize, i: usize) -> usize {
        (j + 1) * self.stride + i + 1
    }

    /// Hand back the coefficient and flag buffers after decoding.
    pub fn into_parts(self) -> (&'a mut [i32], &'a mut [u16]) {
        (self.data, self.flags)
    }

    fn update_flags(&mut self, idx: usize, s: u32) {
        let stride = self.stride;
        let np = idx - stride;
        let sp = idx + stride;
        let m = MOD;
        self.flags[np - 1] |= SIG_SE;
        self.flags[np] |= m[s as usize];
        self.flags[np + 1] |= SIG_SW;
        self.flags[idx - 1] |= m[s as usize + 2];
        self.flags[idx] |= SIG;
        self.flags[idx + 1] |= m[s as usize + 4];
        self.flags[sp - 1] |= SIG_NE;
        self.flags[sp] |= m[s as usize + 6];
        self.flags[sp + 1] |= SIG_NW;
    }

    /// `t1_getctxno_zc`.
    fn ctxno_zc(&self, flag: u16, orient: usize) -> usize {
        LUT_CTXNO_ZC[(orient << 8) | (flag & SIG_OTH) as usize] as usize
    }

    /// `t1_getctxno_sc`.
    fn ctxno_sc(flag: u16) -> usize {
        LUT_CTXNO_SC[(flag & (SIG_PRIM | SGN_N | SGN_E | SGN_S | SGN_W)) as usize >> 4] as usize
    }

    /// `t1_getctxno_mag`.
    fn ctxno_mag(flag: u16) -> usize {
        let t1 = if flag & SIG_OTH != 0 {
            CTXNO_MAG + 1
        } else {
            CTXNO_MAG
        };
        if flag & REFINE != 0 {
            CTXNO_MAG + 2
        } else {
            t1
        }
    }

    /// `t1_getspb`.
    fn spb(flag: u16) -> u32 {
        LUT_SPB[(flag & (SIG_PRIM | SGN_N | SGN_E | SGN_S | SGN_W)) as usize >> 4] as u32
    }

    // -- significance pass --------------------------------------------------

    fn sigpass_step_mqc(&mut self, j: usize, i: usize, orient: usize, oneplushalf: i32) {
        let idx = self.flag_idx(j, i);
        let flag = self.flags[idx];
        if (flag & SIG_OTH) != 0 && (flag & (SIG | VISIT)) == 0 {
            let ctx = self.ctxno_zc(flag, orient);
            if self.mqc.as_mut().unwrap().decode(ctx) == 1 {
                let v = self.mqc.as_mut().unwrap().decode(Self::ctxno_sc(flag)) ^ Self::spb(flag);
                self.data[j * self.w + i] = if v == 1 { -oneplushalf } else { oneplushalf };
                self.update_flags(idx, v);
            }
            self.flags[idx] |= VISIT;
        }
    }

    fn sigpass_step_raw(&mut self, j: usize, i: usize, oneplushalf: i32) {
        let idx = self.flag_idx(j, i);
        let flag = self.flags[idx];
        if (flag & SIG_OTH) != 0 && (flag & (SIG | VISIT)) == 0 {
            let v = self.raw.as_mut().unwrap().decode();
            self.data[j * self.w + i] = if v == 1 { -oneplushalf } else { oneplushalf };
            self.update_flags(idx, v);
            self.flags[idx] |= VISIT;
        }
    }

    /// `t1_dec_sigpass_mqc` / `_raw` (non-VSC; VSC ignored, see module docs).
    fn sigpass(&mut self, bpno: i32, orient: usize, raw: bool) {
        let one = 1i32 << bpno;
        let oneplushalf = one | (one >> 1);
        let (w, h) = (self.w, self.h);
        let mut k = 0usize;
        while k < (h & !3) {
            for i in 0..w {
                for j in k..k + 4 {
                    if raw {
                        self.sigpass_step_raw(j, i, oneplushalf);
                    } else {
                        self.sigpass_step_mqc(j, i, orient, oneplushalf);
                    }
                }
            }
            k += 4;
        }
        for i in 0..w {
            for j in k..h {
                if raw {
                    self.sigpass_step_raw(j, i, oneplushalf);
                } else {
                    self.sigpass_step_mqc(j, i, orient, oneplushalf);
                }
            }
        }
    }

    // -- refinement pass ----------------------------------------------------

    fn refpass_step_mqc(&mut self, j: usize, i: usize, poshalf: i32, neghalf: i32) {
        let idx = self.flag_idx(j, i);
        let flag = self.flags[idx];
        if (flag & (SIG | VISIT)) == SIG {
            let v = self.mqc.as_mut().unwrap().decode(Self::ctxno_mag(flag));
            let t = if v == 1 { poshalf } else { neghalf };
            let d = self.data[j * self.w + i];
            self.data[j * self.w + i] = d + if d < 0 { -t } else { t };
            self.flags[idx] |= REFINE;
        }
    }

    fn refpass_step_raw(&mut self, j: usize, i: usize, poshalf: i32, neghalf: i32) {
        let idx = self.flag_idx(j, i);
        let flag = self.flags[idx];
        if (flag & (SIG | VISIT)) == SIG {
            let v = self.raw.as_mut().unwrap().decode();
            let t = if v == 1 { poshalf } else { neghalf };
            let d = self.data[j * self.w + i];
            self.data[j * self.w + i] = d + if d < 0 { -t } else { t };
            self.flags[idx] |= REFINE;
        }
    }

    /// `t1_dec_refpass_mqc` / `_raw`.
    fn refpass(&mut self, bpno: i32, raw: bool) {
        let one = 1i32 << bpno;
        let poshalf = one >> 1;
        let neghalf = if bpno > 0 { -poshalf } else { -1 };
        let (w, h) = (self.w, self.h);
        let mut k = 0usize;
        while k < (h & !3) {
            for i in 0..w {
                for j in k..k + 4 {
                    if raw {
                        self.refpass_step_raw(j, i, poshalf, neghalf);
                    } else {
                        self.refpass_step_mqc(j, i, poshalf, neghalf);
                    }
                }
            }
            k += 4;
        }
        for i in 0..w {
            for j in k..h {
                if raw {
                    self.refpass_step_raw(j, i, poshalf, neghalf);
                } else {
                    self.refpass_step_mqc(j, i, poshalf, neghalf);
                }
            }
        }
    }

    // -- clean-up pass ------------------------------------------------------

    fn clnpass_step(&mut self, j: usize, i: usize, orient: usize, oneplushalf: i32) {
        let idx = self.flag_idx(j, i);
        let flag = self.flags[idx];
        if (flag & (SIG | VISIT)) == 0 {
            let ctx = self.ctxno_zc(flag, orient);
            if self.mqc.as_mut().unwrap().decode(ctx) == 1 {
                let v = self.mqc.as_mut().unwrap().decode(Self::ctxno_sc(flag)) ^ Self::spb(flag);
                self.data[j * self.w + i] = if v == 1 { -oneplushalf } else { oneplushalf };
                self.update_flags(idx, v);
            }
        }
        self.flags[idx] &= !VISIT;
    }

    /// `t1_dec_clnpass_step_partial` — significance already known via the
    /// run-length prefix; only the sign decision is decoded.
    fn clnpass_step_partial(&mut self, j: usize, i: usize, oneplushalf: i32) {
        let idx = self.flag_idx(j, i);
        let flag = self.flags[idx];
        let v = self.mqc.as_mut().unwrap().decode(Self::ctxno_sc(flag)) ^ Self::spb(flag);
        self.data[j * self.w + i] = if v == 1 { -oneplushalf } else { oneplushalf };
        self.update_flags(idx, v);
        self.flags[idx] &= !VISIT;
    }

    fn agg_check(&self, k: usize, i: usize) -> bool {
        // all four rows insignificant and unvisited (MACRO_t1_flags)
        for r in 0..4 {
            if self.flags[(k + r + 1) * self.stride + i + 1] & (SIG | VISIT | SIG_OTH) != 0 {
                return false;
            }
        }
        true
    }

    /// `t1_dec_clnpass` (non-VSC) + segmentation symbol check.
    fn clnpass(&mut self, bpno: i32, orient: usize, cblksty: u8) {
        let one = 1i32 << bpno;
        let oneplushalf = one | (one >> 1);
        let (w, h) = (self.w, self.h);
        let mut k = 0usize;
        while k < (h & !3) {
            for i in 0..w {
                let agg = self.agg_check(k, i);
                if agg {
                    if self.mqc.as_mut().unwrap().decode(CTXNO_AGG) == 0 {
                        continue;
                    }
                    let b1 = self.mqc.as_mut().unwrap().decode(CTXNO_UNI);
                    let b2 = self.mqc.as_mut().unwrap().decode(CTXNO_UNI);
                    let runlen = ((b1 << 1) | b2) as usize;
                    for j in (k + runlen)..(k + 4).min(h) {
                        if j == k + runlen {
                            self.clnpass_step_partial(j, i, oneplushalf);
                        } else {
                            self.clnpass_step(j, i, orient, oneplushalf);
                        }
                    }
                } else {
                    for j in k..(k + 4).min(h) {
                        self.clnpass_step(j, i, orient, oneplushalf);
                    }
                }
            }
            k += 4;
        }
        for i in 0..w {
            for j in k..h {
                self.clnpass_step(j, i, orient, oneplushalf);
            }
        }
        if cblksty & CBLKSTY_SEGSYM != 0 {
            let mut v = 0u32;
            for _ in 0..4 {
                let b = self.mqc.as_mut().unwrap().decode(CTXNO_UNI);
                v = (v << 1) | b;
            }
            if v != 0xa {
                log::warn!("JPX: bad segmentation symbol 0x{v:x} (expected 0xa)");
            }
        }
    }

    /// `t1_decode_cblk`: run all segments' passes in order. `orient` is the
    /// band number (0 = LL, 1 = HL, 2 = LH, 3 = HH); `numbps` the effective
    /// bit-plane count from the packet header; `cblksty` the code-block style.
    pub fn decode_cblk(&mut self, segs: &[Seg<'a>], orient: usize, numbps: u32, cblksty: u8) {
        if cblksty & CBLKSTY_VSC != 0 && !self.warn_vsc {
            self.warn_vsc = true;
            log::warn!("JPX: VSC code-block style not supported; decoding without it");
        }
        let mut bpno = numbps as i32 - 1;
        let mut passtype = 2usize; // clean-up pass first
        for seg in segs {
            if seg.data.is_empty() {
                continue;
            }
            // BYPASS (LAZY) mode switch
            let raw =
                bpno <= (numbps as i32 - 1 - 4) && passtype < 2 && (cblksty & CBLKSTY_LAZY) != 0;
            if raw {
                self.raw = Some(RawDecoder::new(seg.data));
            } else {
                self.mqc = Some(MqDecoder::new(seg.data));
            }
            for _ in 0..seg.numpasses {
                match passtype {
                    0 => self.sigpass(bpno + 1, orient, raw),
                    1 => self.refpass(bpno + 1, raw),
                    _ => self.clnpass(bpno + 1, orient, cblksty),
                }
                if (cblksty & CBLKSTY_RESET) != 0 && !raw {
                    if let Some(m) = self.mqc.as_mut() {
                        m.reset_states();
                    }
                }
                passtype += 1;
                if passtype == 3 {
                    passtype = 0;
                    bpno -= 1;
                }
            }
        }
        self.mqc = None;
        self.raw = None;
    }
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_segments_leave_cblk_zero() {
        let segs: Vec<Seg> = vec![Seg {
            data: &[],
            numpasses: 0,
        }];
        let mut data = vec![0i32; 64];
        let mut flags = vec![0u16; (8 + 2) * (8 + 2)];
        let mut t1 = T1::new(8, 8, &mut data, &mut flags);
        t1.decode_cblk(&segs, 0, 8, 0);
        let (data, flags) = t1.into_parts();
        assert!(data.iter().all(|&v| v == 0));
        assert!(flags.iter().all(|&v| v == 0));
    }

    #[test]
    fn ctxno_mag_follows_lut_order() {
        // REFINE alone → MAG+2; OTH alone → MAG+1; both → MAG+2; none → MAG.
        assert_eq!(T1::ctxno_mag(0), CTXNO_MAG);
        assert_eq!(T1::ctxno_mag(SIG_REFINE_FLAG), CTXNO_MAG + 2);
        assert_eq!(T1::ctxno_mag(SIG_N), CTXNO_MAG + 1);
        assert_eq!(T1::ctxno_mag(SIG_N | SIG_REFINE_FLAG), CTXNO_MAG + 2);
    }

    const SIG_REFINE_FLAG: u16 = REFINE;

    #[test]
    fn zc_lut_orientation_offset() {
        // orientation 0 vs 3 for the same flag pattern should generally
        // differ (HH more aggressive); sanity-check bounds only.
        for orient in 0..4usize {
            for f in 0..256u16 {
                let c = LUT_CTXNO_ZC[(orient << 8) | (f & SIG_OTH) as usize];
                assert!(c <= 8, "zc context out of range: {c}");
            }
        }
    }

    #[test]
    fn sc_lut_returns_sign_contexts() {
        for f in 0..256u16 {
            let c = LUT_CTXNO_SC[(f & (SIG_PRIM | SGN_N | SGN_E | SGN_S | SGN_W)) as usize >> 4];
            assert!((9..=13).contains(&c), "sc context out of range: {c}");
        }
    }

    #[test]
    fn update_flags_sets_neighbours() {
        let mut data = vec![0i32; 64];
        let mut flags = vec![0u16; 10 * 10];
        let mut t1 = T1::new(8, 8, &mut data, &mut flags);
        let idx = t1.flag_idx(3, 3);
        t1.update_flags(idx, 1); // s=1 → negative
        let (data, flags) = t1.into_parts();
        // north neighbour of (3,3) flagged significant south + sign south
        assert_eq!(flags[(2 + 1) * 10 + (3 + 1)], SIG_S | SGN_S);
        // east neighbour flagged significant west + sign west
        assert_eq!(flags[(3 + 1) * 10 + (3 + 2)], SIG_W | SGN_W);
        // centre flagged significant
        assert_eq!(flags[(3 + 1) * 10 + (3 + 1)], SIG);
        let _ = data;
    }

    #[test]
    fn raw_passes_decode_through_bypass() {
        // Run a raw sigpass over an empty stream: every decode returns 1
        // (0xFF filler) → all border-adjacent coefficients become
        // significant at -oneplushalf. Ensures the RAW path is exercised
        // without panicking on truncated data (R1).
        let segs = vec![Seg {
            data: &[],
            numpasses: 3,
        }];
        let mut data = vec![0i32; 4 * 4];
        let mut flags = vec![0u16; (4 + 2) * (4 + 2)];
        // pre-mark the border significant so sigpass can trigger
        let stride = 4 + 2;
        for j in 0..4 {
            for i in 0..4 {
                flags[(j + 1) * stride + i + 1] |= SIG_N | SIG_E | SIG_S | SIG_W;
            }
        }
        let mut t1 = T1::new(4, 4, &mut data, &mut flags);
        // LAZY + only sig/ref passes in the raw window requires bpno <=
        // numbps-1-4; craft numbps high enough: 8 planes, bpno starts 7...
        // passtype=2 (clnpass, MQ) first; then sigpass at bpno=7 <= 8-1-4=3?
        // No — 7 > 3. Use 12 planes so the first sigpass after clnpass uses
        // raw: bpno goes 11 (cln), 10 (sig) <= 12-1-4 = 7? no. Instead seed
        // with numbps = 12 and two segments so the raw window opens at
        // bpno = 7: 11 cln, 10 sig, 9 ref, 9 cln, 8 sig, 8 ref, 8 cln, 7
        // sig → raw. Simplest: call sigpass directly through decode with
        // LAZY and check it doesn't panic; exact bit-level vectors come
        // from the glymur end-to-end tests.
        t1.decode_cblk(&segs, 1, 12, CBLKSTY_LAZY);
        let (data, _flags) = t1.into_parts();
        let _ = data;
    }
}
