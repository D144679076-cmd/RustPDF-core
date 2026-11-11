//! QM-coder arithmetic decoder for JBIG2 (ITU-T T.88 Annex C / JPEG 2000
//! Annex C.3).  Port of pdf.js v3.11.174 `src/core/arithmetic_decoder.js`
//! (Apache 2.0); the QE state table is Table C.2 of the JPEG 2000 FCD.

/// One row of the Table C.2 QE state table.
struct QeEntry {
    qe: u16,
    nmps: u8,
    nlps: u8,
    switch_flag: u8,
}

// Table C-2 (pdf.js keeps this as objects; flattened here for fast lookup).
const QE_TABLE: [QeEntry; 47] = [
    QeEntry {
        qe: 0x5601,
        nmps: 1,
        nlps: 1,
        switch_flag: 1,
    },
    QeEntry {
        qe: 0x3401,
        nmps: 2,
        nlps: 6,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1801,
        nmps: 3,
        nlps: 9,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0ac1,
        nmps: 4,
        nlps: 12,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0521,
        nmps: 5,
        nlps: 29,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0221,
        nmps: 38,
        nlps: 33,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x5601,
        nmps: 7,
        nlps: 6,
        switch_flag: 1,
    },
    QeEntry {
        qe: 0x5401,
        nmps: 8,
        nlps: 14,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x4801,
        nmps: 9,
        nlps: 14,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x3801,
        nmps: 10,
        nlps: 14,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x3001,
        nmps: 11,
        nlps: 17,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x2401,
        nmps: 12,
        nlps: 18,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1c01,
        nmps: 13,
        nlps: 20,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1601,
        nmps: 29,
        nlps: 21,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x5601,
        nmps: 15,
        nlps: 14,
        switch_flag: 1,
    },
    QeEntry {
        qe: 0x5401,
        nmps: 16,
        nlps: 14,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x5101,
        nmps: 17,
        nlps: 15,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x4801,
        nmps: 18,
        nlps: 16,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x3801,
        nmps: 19,
        nlps: 17,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x3401,
        nmps: 20,
        nlps: 18,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x3001,
        nmps: 21,
        nlps: 19,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x2801,
        nmps: 22,
        nlps: 19,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x2401,
        nmps: 23,
        nlps: 20,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x2201,
        nmps: 24,
        nlps: 21,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1c01,
        nmps: 25,
        nlps: 22,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1801,
        nmps: 26,
        nlps: 23,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1601,
        nmps: 27,
        nlps: 24,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1401,
        nmps: 28,
        nlps: 25,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1201,
        nmps: 29,
        nlps: 26,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x1101,
        nmps: 30,
        nlps: 27,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0ac1,
        nmps: 31,
        nlps: 28,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x09c1,
        nmps: 32,
        nlps: 29,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x08a1,
        nmps: 33,
        nlps: 30,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0521,
        nmps: 34,
        nlps: 31,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0441,
        nmps: 35,
        nlps: 32,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x02a1,
        nmps: 36,
        nlps: 33,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0221,
        nmps: 37,
        nlps: 34,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0141,
        nmps: 38,
        nlps: 35,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0111,
        nmps: 39,
        nlps: 36,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0085,
        nmps: 40,
        nlps: 37,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0049,
        nmps: 41,
        nlps: 38,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0025,
        nmps: 42,
        nlps: 39,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0015,
        nmps: 43,
        nlps: 40,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0009,
        nmps: 44,
        nlps: 41,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0005,
        nmps: 45,
        nlps: 42,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x0001,
        nmps: 45,
        nlps: 43,
        switch_flag: 0,
    },
    QeEntry {
        qe: 0x5601,
        nmps: 46,
        nlps: 46,
        switch_flag: 0,
    },
];

/// QM-coder decoding state over a byte slice (C.3.5 INITDEC).
pub struct ArithmeticDecoder<'a> {
    data: &'a [u8],
    bp: usize,
    data_end: usize,
    chigh: u32,
    clow: u32,
    ct: u32,
    a: u32,
}

impl<'a> ArithmeticDecoder<'a> {
    /// C.3.5 Initialisation of the decoder (INITDEC).
    ///
    /// `start`/`end` select the byte range of the coded data inside `data`;
    /// the range must be non-empty (`start < end`).
    pub fn new(data: &'a [u8], start: usize, end: usize) -> Self {
        // Reference indexes data[start] unconditionally; callers always pass
        // a region that contains at least the two init bytes.
        let first = data.get(start).copied().unwrap_or(0) as u32;
        let mut d = ArithmeticDecoder {
            data,
            bp: start,
            data_end: end,
            chigh: first,
            clow: 0,
            ct: 0,
            a: 0,
        };
        d.byte_in();
        d.chigh = ((d.chigh << 7) & 0xffff) | ((d.clow >> 9) & 0x7f);
        d.clow = (d.clow << 7) & 0xffff;
        d.ct -= 7;
        d.a = 0x8000;
        d
    }

    /// C.3.4 Compressed data input (BYTEIN).
    fn byte_in(&mut self) {
        let data = self.data;
        let mut bp = self.bp;

        if data.get(bp).copied().unwrap_or(0) == 0xff {
            if data.get(bp + 1).copied().unwrap_or(0) > 0x8f {
                self.clow += 0xff00;
                self.ct = 8;
            } else {
                bp += 1;
                self.clow += (data.get(bp).copied().unwrap_or(0) as u32) << 9;
                self.ct = 7;
                self.bp = bp;
            }
        } else {
            bp += 1;
            self.clow += if bp < self.data_end {
                (data.get(bp).copied().unwrap_or(0) as u32) << 8
            } else {
                0xff00
            };
            self.ct = 8;
            self.bp = bp;
        }
        if self.clow > 0xffff {
            self.chigh += self.clow >> 16;
            self.clow &= 0xffff;
        }
    }

    /// C.3.2 Decoding a decision (DECODE): returns the decoded bit.
    ///
    /// `contexts` holds the adaptive per-position state packed exactly like
    /// the reference: the high 7 bits are the cx index, the low bit the MPS.
    pub fn read_bit(&mut self, contexts: &mut [u8], pos: usize) -> u32 {
        let mut cx_index = (contexts[pos] >> 1) as usize;
        let mut cx_mps = contexts[pos] & 1;
        let entry = &QE_TABLE[cx_index];
        let qe_icx = entry.qe;
        let d: u32;
        let a_orig = self.a - qe_icx as u32;
        let mut a = a_orig;

        if self.chigh < qe_icx as u32 {
            // exchangeLps: A is replaced by Qe in both cases.
            a = qe_icx as u32;
            if a_orig < qe_icx as u32 {
                d = cx_mps as u32;
                cx_index = entry.nmps as usize;
            } else {
                d = 1 ^ cx_mps as u32;
                if entry.switch_flag == 1 {
                    cx_mps = d as u8;
                }
                cx_index = entry.nlps as usize;
            }
        } else {
            self.chigh -= qe_icx as u32;
            if (a & 0x8000) != 0 {
                self.a = a;
                return cx_mps as u32;
            }
            // exchangeMps
            if a < qe_icx as u32 {
                d = 1 ^ cx_mps as u32;
                if entry.switch_flag == 1 {
                    cx_mps = d as u8;
                }
                cx_index = entry.nlps as usize;
            } else {
                d = cx_mps as u32;
                cx_index = entry.nmps as usize;
            }
        }
        // C.3.3 renormD
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            a <<= 1;
            self.chigh = ((self.chigh << 1) & 0xffff) | ((self.clow >> 15) & 1);
            self.clow = (self.clow << 1) & 0xffff;
            self.ct -= 1;
            if (a & 0x8000) != 0 {
                break;
            }
        }
        self.a = a;

        contexts[pos] = ((cx_index as u8) << 1) | cx_mps;
        d
    }
}
