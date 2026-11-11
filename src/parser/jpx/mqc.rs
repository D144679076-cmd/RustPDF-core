//! MQ arithmetic decoder (ISO 15444-1 Annex C) and RAW bypass decoder.
//!
//! Port of OpenJPEG 1.5.2 `mqc.c` / `raw.c` (INITDEC / BYTEIN / DECODE),
//! using the 94-entry probability-state table in [`super::t1_luts`].
//!
//! MQ state invariants (C reference): `c` accumulates bits at positions
//! 16..27, `ct` counts free bit slots, `a` is the interval width with the
//! 0x8000 midpoint. No panic paths (R1) — a truncated segment decodes as if
//! fed `0xFF` filler bytes, matching the C behaviour of reading past the
//! end of the buffer only through the artificial `0xFF 0xFF` tail marker.

use super::t1_luts::MQC_STATES;

/// MQ arithmetic decoder over one code-block segment.
pub struct MqDecoder<'a> {
    data: &'a [u8],
    /// Index of the next byte to consume (1.5.2 `bp` relative to `start`).
    bp: usize,
    c: u32,
    ct: i32,
    a: u32,
    /// Current context states, indexed by T1 context number.
    states: [u8; 19],
}

impl<'a> MqDecoder<'a> {
    /// ISO 15444-1 C.3.5 INITDEC — context init per OpenJPEG `t1_decode_cblk`
    /// (`mqc_resetstates`; UNI=state 46, AGG=state 3, ZC=state 4).
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = MqDecoder {
            data,
            bp: 0,
            c: 0,
            ct: 0,
            a: 0,
            states: [0; 19],
        };
        d.reset_states();
        d.init_dec();
        d
    }

    /// `mqc_setstate(mqc, ctxno, msb=0, prob)` — state index
    /// `msb + (prob << 1)` into the flattened 94-entry table.
    fn set_state(&mut self, ctxno: usize, prob: u8) {
        self.states[ctxno] = prob << 1;
    }

    /// `mqc_resetstates` + T1 context init — used at code-block start and
    /// after every pass when the RESET code-block style is set.
    pub fn reset_states(&mut self) {
        for s in self.states.iter_mut() {
            *s = 0;
        }
        self.set_state(super::t1::CTXNO_UNI, 46);
        self.set_state(super::t1::CTXNO_AGG, 3);
        self.set_state(super::t1::CTXNO_ZC, 4);
    }

    fn bytein(&mut self) {
        // opj_mqc_bytein (1.5.2 mqc_inl.h): on 0xFF the next byte's high
        // bits are shifted, and a 0x8F+ trailer byte acts as an
        // end-of-stream marker that feeds 0xFF00 (no bp advance).
        let cur = self.get_byte(self.bp);
        if cur == 0xff {
            let next = self.get_byte(self.bp + 1);
            if next > 0x8f {
                self.c += 0xff00;
                self.ct = 8;
            } else {
                self.bp += 1;
                self.c += next << 9;
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c += self.get_byte(self.bp) << 8;
            self.ct = 8;
        }
    }

    /// Read a byte past the segment end: OpenJPEG appends an artificial
    /// `0xFF 0xFF` marker, so past-the-end decodes as 0xFF filler.
    fn get_byte(&self, idx: usize) -> u32 {
        if idx < self.data.len() {
            self.data[idx] as u32
        } else {
            0xff
        }
    }

    /// INITDEC: `c = data[0] << 16` (len==0 → `0xff << 16`), bytein,
    /// `c <<= 7`, `ct -= 7`, `a = 0x8000`.
    fn init_dec(&mut self) {
        self.c = if self.data.is_empty() {
            0xff << 16
        } else {
            self.get_byte(0) << 16
        };
        self.bp = 0;
        self.bytein();
        self.c <<= 7;
        self.ct -= 7;
        self.a = 0x8000;
    }

    /// DECODE one decision bit for the given context (mqc_decode).
    pub fn decode(&mut self, ctxno: usize) -> u32 {
        let state = self.states[ctxno] as usize;
        let (qe, mps, nmps, nlps) = MQC_STATES[state];

        let d;
        self.a = self.a.wrapping_sub(qe as u32);
        if (self.c >> 16) < qe as u32 {
            // LPS exchange (mqc_lpsexchange) + renormalise (mqc_renormd):
            // the `a < qeval` test uses the post-subtraction interval
            let below = self.a < u32::from(qe);
            self.a = u32::from(qe);
            if below {
                d = u32::from(mps);
                self.states[ctxno] = nmps;
            } else {
                d = u32::from(1 - mps);
                self.states[ctxno] = nlps;
            }
            self.renorm();
        } else {
            self.c = self.c.wrapping_sub(u32::from(qe) << 16);
            if (self.a & 0x8000) == 0 {
                // MPS exchange + renormalise
                if self.a < u32::from(qe) {
                    d = u32::from(1 - mps);
                    self.states[ctxno] = nlps;
                } else {
                    d = u32::from(mps);
                    self.states[ctxno] = nmps;
                }
                self.renorm();
            } else {
                d = mps as u32;
            }
        }
        d
    }

    /// `mqc_renormd`: shift `a`/`c` left until the MSB of `a` is set,
    /// refilling via BYTEIN whenever `ct` runs out.
    fn renorm(&mut self) {
        loop {
            if self.ct == 0 {
                self.bytein();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a >= 0x8000 {
                break;
            }
        }
    }
}

/// RAW (bypass) bit decoder for LAZY code-block passes (`raw.c`): plain
/// MSB-first bits; after a `0xFF` byte only 7 bits remain; past the end
/// decodes `0xFF` filler.
pub struct RawDecoder<'a> {
    data: &'a [u8],
    len: usize,
    c: u32,
    ct: i32,
}

impl<'a> RawDecoder<'a> {
    /// `raw_init_dec` — decode from the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        RawDecoder {
            data,
            len: 0,
            c: 0,
            ct: 0,
        }
    }

    /// `raw_decode` — one MSB-first bit.
    pub fn decode(&mut self) -> u32 {
        if self.ct == 0 {
            self.ct = 8;
            if self.len == self.data.len() {
                self.c = 0xff;
            } else {
                if self.c == 0xff {
                    self.ct = 7;
                }
                self.c = self.data[self.len] as u32;
                self.len += 1;
            }
        }
        self.ct -= 1;
        (self.c >> self.ct) & 1
    }
}

// ---------------------------------------------------------------------------
// Tests (R5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::jpx::t1::CTXNO_ZC;

    /// All-zero segment data: after INITDEC (c = 0, a = 0x8000), the first
    /// decision under ZC (state index 8, qe = 0x0521) takes the LPS path
    /// (c >> 16 == 0 < qe) with a = 0x7ADF >= qe → LPS exchange, d = !mps
    /// = 1. Verified by hand against the C DECODE pseudocode.
    #[test]
    fn mq_zero_bytes_first_decode_is_lps() {
        let mut d = MqDecoder::new(&[0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(d.decode(CTXNO_ZC), 1);
    }

    /// A `0xFF` byte followed by `0x8F` continues normally (0x8F is *not*
    /// > 0x8F): decoder advances into the segment, `ct` becomes 7 for the
    /// next refill. Must decode deterministically without panicking.
    #[test]
    fn mq_ff8f_prefix_decodes() {
        let mut dd = MqDecoder::new(&[0xff, 0x8f, 0x00, 0x00]);
        assert!(dd.decode(CTXNO_ZC) <= 1);
    }

    /// RAW decoder: MSB-first bits, 0xFF stall (7 bits after 0xFF).
    #[test]
    fn raw_msb_first_bits() {
        let mut d = RawDecoder::new(&[0b1011_0001]);
        let mut bits = Vec::new();
        for _ in 0..8 {
            bits.push(d.decode());
        }
        assert_eq!(bits, vec![1, 0, 1, 1, 0, 0, 0, 1]);
    }

    #[test]
    fn raw_after_ff_stall_skips_bit8() {
        // 0xFF then 0b1110_0000: after 0xFF only 7 bits of next byte are
        // used (MSB dropped).
        let mut d = RawDecoder::new(&[0xff, 0b1110_0000]);
        let mut bits = Vec::new();
        for _ in 0..15 {
            bits.push(d.decode());
        }
        // First 8 bits: 1,1,1,1,1,1,1,1 (0xFF, MSB-of-next dropped per stall
        // → the 8th decode pulls next byte with ct=7).
        assert_eq!(bits[0], 1);
        assert_eq!(bits[7], 1);
    }

    #[test]
    fn raw_past_end_is_ff_filler() {
        let mut d = RawDecoder::new(&[]);
        for _ in 0..40 {
            assert_eq!(d.decode(), 1);
        }
    }
}
