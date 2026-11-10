//! CCITTFaxDecode filter — ITU-T T.4 (Group 3) / T.6 (Group 4) fax decoding.
//!
//! Faithful port of pdf.js v3.11.174 `src/core/ccitt.js` (Apache 2.0; itself
//! a port of Glyph & Cog's XPDF implementation). Decodes 1-D (K = 0), mixed
//! 2-D (K > 0) and pure 2-D MMR (K < 0, Group 4) rows; handles EOL prefixes,
//! byte alignment, `EndOfBlock`, `BlackIs1` and the Return-To-Control (RTC)
//! sequence. The reference file used to regenerate the tables and to
//! cross-validate the test vectors is `tests/ccitt-reference.js`.
//!
//! Public entry points: [`decode`] (parms from a `/DecodeParms` dict) and
//! [`decode_with`] (pre-parsed [`FaxParms`]). No panics (R1): damaged data is
//! tolerated exactly like the reference (bad codes clamp runs and set the
//! error flag); only invalid parameters return `PdfError`.

pub use super::ccitt_parms::FaxParms;
use super::ccitt_tables::{
    RunCode, BLACKTABLE1, BLACKTABLE2, BLACKTABLE3, TWODIMTABLE, WHITETABLE1, WHITETABLE2,
};
use crate::error::{PdfError, Result};
use crate::parser::objects::PdfDict;

// Two-dimensional coding modes (T.6 §4.2).
const TWO_DIM_PASS: i32 = 0;
const TWO_DIM_HORIZ: i32 = 1;
const TWO_DIM_VERT0: i32 = 2;
const TWO_DIM_VERT_R1: i32 = 3;
const TWO_DIM_VERT_L1: i32 = 4;
const TWO_DIM_VERT_R2: i32 = 5;
const TWO_DIM_VERT_L2: i32 = 6;
const TWO_DIM_VERT_R3: i32 = 7;
const TWO_DIM_VERT_L3: i32 = 8;

const MAX_OUTPUT_BYTES: usize = 1 << 28;

/// Decode a CCITTFaxDecode stream using parameters from `/DecodeParms`.
///
/// `parms` is the (already resolved) parameter dictionary; `None` means the
/// stream had no `/DecodeParms` and all defaults apply.
///
/// # Errors
/// `PdfError` for invalid parameters. Damaged *data* is tolerated (matching
/// the reference decoder): bad codes clamp runs and set an internal error
/// flag; output is padded to `/Rows` × row-stride when `/Rows` is given.
pub fn decode(data: &[u8], parms: Option<&PdfDict>) -> Result<Vec<u8>> {
    decode_with(data, &FaxParms::from_dict(parms)?)
}

/// Decode with pre-parsed parameters (see [`FaxParms`]).
pub fn decode_with(data: &[u8], p: &FaxParms) -> Result<Vec<u8>> {
    let stride = p.columns.div_ceil(8) as usize;
    let target = if p.rows > 0 {
        let t = p.rows as usize * stride;
        if t > MAX_OUTPUT_BYTES {
            return Err(PdfError::filter_error(
                0,
                format!("CCITT: /Rows x /Columns too large ({} bytes)", t),
            ));
        }
        t
    } else {
        MAX_OUTPUT_BYTES
    };

    let mut dec = Decoder::new(data, p);
    let mut out = Vec::with_capacity(target.min(1 << 20));
    while out.len() < target {
        match dec.read_next_byte() {
            Some(b) => out.push(b),
            None => break,
        }
    }
    // Missing rows are white-filled in the stream's own convention (1-bits
    // are white unless /BlackIs1) — only meaningful when /Rows declares the
    // expected size; without /Rows the stream ends at EOF like the reference.
    if p.rows > 0 && out.len() < target {
        let fill = if p.black_is1 { 0x00 } else { 0xff };
        out.resize(target, fill);
    }
    Ok(out)
}

/// Streaming decoder — one output byte per [`Decoder::read_next_byte`] call,
/// exactly mirroring the reference `readNextChar()`.
struct Decoder<'a> {
    src: &'a [u8],
    pos: usize,
    eof: bool,
    err: bool,
    encoding: i32,
    eoline: bool,
    byte_align: bool,
    columns: u32,
    rows: i32,
    eoblock: bool,
    black: bool,
    coding_line: Vec<u32>,
    ref_line: Vec<u32>,
    coding_pos: usize,
    row: i32,
    next_line_2d: bool,
    input_bits: u32,
    input_buf: u32,
    output_bits: u32,
    rows_done: bool,
}

impl<'a> Decoder<'a> {
    fn new(src: &'a [u8], p: &FaxParms) -> Self {
        let mut d = Decoder {
            src,
            pos: 0,
            eof: false,
            err: false,
            encoding: p.k,
            eoline: p.eoline,
            byte_align: p.byte_align,
            columns: p.columns,
            rows: p.rows as i32,
            eoblock: p.eoblock,
            black: p.black_is1,
            coding_line: vec![0u32; p.columns as usize + 1],
            ref_line: vec![0u32; p.columns as usize + 2],
            coding_pos: 0,
            row: 0,
            next_line_2d: p.k < 0,
            input_bits: 0,
            input_buf: 0,
            output_bits: 0,
            rows_done: false,
        };
        d.coding_line[0] = d.columns;

        // Skip any leading zero bits; if an EOL is at the head, eat it.
        let mut code1;
        loop {
            code1 = d.look_bits(12);
            if code1 != 0 {
                break;
            }
            d.eat_bits(1);
        }
        if code1 == 1 {
            d.eat_bits(12);
        }
        if d.encoding > 0 {
            // Extension bit after EOL (reference: bit 0 means 2-D follows).
            d.next_line_2d = d.look_bits(1) == 0;
            d.eat_bits(1);
        }
        d
    }

    /// Next source byte, or None at end of input.
    fn next_src(&mut self) -> Option<u8> {
        match self.src.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                Some(b)
            }
            None => None,
        }
    }

    /// Peek `n` (1..=13) bits; returns -1 when the buffer is empty at EOF and
    /// a zero-padded value when EOF hits mid-look (reference semantics).
    fn look_bits(&mut self, n: u32) -> i32 {
        let mask: u32 = 0xffff >> (16 - n);
        while self.input_bits < n {
            match self.next_src() {
                None => {
                    if self.input_bits == 0 {
                        return -1;
                    }
                    return ((self.input_buf << (n - self.input_bits)) & mask) as i32;
                }
                Some(b) => {
                    self.input_buf = (self.input_buf << 8) | b as u32;
                    self.input_bits += 8;
                }
            }
        }
        ((self.input_buf >> (self.input_bits - n)) & mask) as i32
    }

    fn eat_bits(&mut self, n: u32) {
        self.input_bits = self.input_bits.saturating_sub(n);
    }

    /// Linear code search for the non-`EndOfBlock` path.
    /// Returns `(found, value, valid_match)` like the reference.
    fn find_table_code(
        &mut self,
        start: u32,
        end: u32,
        table: &[RunCode],
        limit: u32,
    ) -> (bool, i32, bool) {
        for i in start..=end {
            let code = self.look_bits(i);
            if code < 0 {
                return (true, 1, false);
            }
            let code = if i < end { code << (end - i) } else { code } as u32;
            if limit == 0 || code >= limit {
                if let Some(&(bits, val)) = table.get((code - limit) as usize) {
                    if bits as u32 == i {
                        self.eat_bits(i);
                        return (true, val as i32, true);
                    }
                }
            }
        }
        (false, 0, false)
    }

    fn get_two_dim_code(&mut self) -> i32 {
        if self.eoblock {
            let code = self.look_bits(7);
            if code >= 0 {
                if let Some(&(bits, val)) = TWODIMTABLE.get(code as usize) {
                    if bits > 0 {
                        self.eat_bits(bits as u32);
                        return val as i32;
                    }
                }
            }
        } else {
            let (found, val, valid) = self.find_table_code(1, 7, &TWODIMTABLE, 0);
            if found && valid {
                return val;
            }
        }
        log::debug!("ccitt: bad two-dimensional code");
        -1
    }

    fn get_white_code(&mut self) -> i32 {
        if self.eoblock {
            let code = self.look_bits(12);
            if code < 0 {
                return 1;
            }
            let entry = if code >> 5 == 0 {
                WHITETABLE1.get(code as usize).copied()
            } else {
                WHITETABLE2.get((code >> 3) as usize).copied()
            };
            if let Some((bits, val)) = entry {
                if bits > 0 {
                    self.eat_bits(bits as u32);
                    return val as i32;
                }
            }
        } else {
            let (found, val, _) = self.find_table_code(1, 9, &WHITETABLE2, 0);
            if found {
                return val;
            }
            let (found, val, _) = self.find_table_code(11, 12, &WHITETABLE1, 0);
            if found {
                return val;
            }
        }
        log::debug!("ccitt: bad white code");
        self.eat_bits(1);
        1
    }

    fn get_black_code(&mut self) -> i32 {
        if self.eoblock {
            let code = self.look_bits(13);
            if code < 0 {
                return 1;
            }
            let entry = if code >> 7 == 0 {
                BLACKTABLE1.get(code as usize).copied()
            } else if code >> 9 == 0 && code >> 7 != 0 {
                BLACKTABLE2.get(((code >> 1) - 64) as usize).copied()
            } else {
                BLACKTABLE3.get((code >> 7) as usize).copied()
            };
            if let Some((bits, val)) = entry {
                if bits > 0 {
                    self.eat_bits(bits as u32);
                    return val as i32;
                }
            }
        } else {
            let (found, val, _) = self.find_table_code(2, 6, &BLACKTABLE3, 0);
            if found {
                return val;
            }
            let (found, val, _) = self.find_table_code(7, 12, &BLACKTABLE2, 64);
            if found {
                return val;
            }
            let (found, val, _) = self.find_table_code(10, 13, &BLACKTABLE1, 0);
            if found {
                return val;
            }
        }
        log::debug!("ccitt: bad black code");
        self.eat_bits(1);
        1
    }

    /// Add a run boundary at `a1` (forward direction). `black` flips run
    /// colour parity, mirroring the reference's `blackPixels` (0 or 1).
    fn add_pixels(&mut self, a1: i64, black: u32) {
        let columns = self.columns as i64;
        let mut coding_pos = self.coding_pos;
        if a1 > self.coding_line[coding_pos] as i64 {
            let mut a1 = a1;
            if a1 > columns {
                log::debug!("ccitt: row is wrong length");
                self.err = true;
                a1 = columns;
            }
            if ((coding_pos as u32) & 1) ^ black != 0 {
                coding_pos += 1;
            }
            self.coding_line[coding_pos] = a1 as u32;
        }
        self.coding_pos = coding_pos;
    }

    /// `addPixelsNeg` — boundary may move left (vertical L1..L3 modes).
    fn add_pixels_neg(&mut self, a1: i64, black: u32) {
        let columns = self.columns as i64;
        let mut coding_pos = self.coding_pos;
        if a1 > self.coding_line[coding_pos] as i64 {
            let mut a1 = a1;
            if a1 > columns {
                log::debug!("ccitt: row is wrong length");
                self.err = true;
                a1 = columns;
            }
            if ((coding_pos as u32) & 1) ^ black != 0 {
                coding_pos += 1;
            }
            self.coding_line[coding_pos] = a1 as u32;
        } else if a1 < self.coding_line[coding_pos] as i64 {
            let mut a1 = a1;
            if a1 < 0 {
                log::debug!("ccitt: invalid code");
                self.err = true;
                a1 = 0;
            }
            let a1 = a1 as u32;
            while coding_pos > 0 && a1 < self.coding_line[coding_pos - 1] {
                coding_pos -= 1;
            }
            self.coding_line[coding_pos] = a1;
        }
        self.coding_pos = coding_pos;
    }

    /// Advance `ref_pos` past reference boundaries consumed by the coding
    /// position (reference: the inline `while (refLine[refPos] <= …)` loops).
    fn advance_ref_pos(&self, mut ref_pos: usize) -> usize {
        while self.ref_line[ref_pos] <= self.coding_line[self.coding_pos]
            && self.ref_line[ref_pos] < self.columns
        {
            ref_pos += 2;
        }
        ref_pos
    }

    /// Decode one row into `coding_line` and set up `output_bits`
    /// (reference `readNextChar` row section). Only called when the previous
    /// row's output is fully consumed and `rows_done`/`eof` are false.
    fn decode_row(&mut self) {
        let columns = self.columns;

        self.err = false;

        if self.next_line_2d {
            // Copy the just-finished coding line as this row's reference.
            let mut i = 0;
            while self.coding_line[i] < columns {
                self.ref_line[i] = self.coding_line[i];
                i += 1;
            }
            self.ref_line[i] = columns;
            self.ref_line[i + 1] = columns;
            self.coding_line[0] = 0;
            self.coding_pos = 0;
            let mut ref_pos: usize = 0;
            let mut black_pixels: u32 = 0;

            while self.coding_line[self.coding_pos] < columns {
                match self.get_two_dim_code() {
                    TWO_DIM_PASS => {
                        let a = self.ref_line[ref_pos + 1] as i64;
                        self.add_pixels(a, black_pixels);
                        if self.ref_line[ref_pos + 1] < columns {
                            ref_pos += 2;
                        }
                    }
                    TWO_DIM_HORIZ => {
                        let mut code1 = 0u32;
                        let mut code2 = 0u32;
                        if black_pixels != 0 {
                            loop {
                                let c = self.get_black_code();
                                code1 += c as u32;
                                if c < 64 {
                                    break;
                                }
                            }
                            loop {
                                let c = self.get_white_code();
                                code2 += c as u32;
                                if c < 64 {
                                    break;
                                }
                            }
                        } else {
                            loop {
                                let c = self.get_white_code();
                                code1 += c as u32;
                                if c < 64 {
                                    break;
                                }
                            }
                            loop {
                                let c = self.get_black_code();
                                code2 += c as u32;
                                if c < 64 {
                                    break;
                                }
                            }
                        }
                        let base = self.coding_line[self.coding_pos] as i64;
                        self.add_pixels(base + code1 as i64, black_pixels);
                        if self.coding_line[self.coding_pos] < columns {
                            let base = self.coding_line[self.coding_pos] as i64;
                            self.add_pixels(base + code2 as i64, black_pixels ^ 1);
                        }
                        ref_pos = self.advance_ref_pos(ref_pos);
                    }
                    TWO_DIM_VERT_R3 => {
                        let a = self.ref_line[ref_pos] as i64 + 3;
                        self.add_pixels(a, black_pixels);
                        black_pixels ^= 1;
                        if self.coding_line[self.coding_pos] < columns {
                            ref_pos = self.advance_ref_pos(ref_pos + 1);
                        }
                    }
                    TWO_DIM_VERT_R2 => {
                        let a = self.ref_line[ref_pos] as i64 + 2;
                        self.add_pixels(a, black_pixels);
                        black_pixels ^= 1;
                        if self.coding_line[self.coding_pos] < columns {
                            ref_pos = self.advance_ref_pos(ref_pos + 1);
                        }
                    }
                    TWO_DIM_VERT_R1 => {
                        let a = self.ref_line[ref_pos] as i64 + 1;
                        self.add_pixels(a, black_pixels);
                        black_pixels ^= 1;
                        if self.coding_line[self.coding_pos] < columns {
                            ref_pos = self.advance_ref_pos(ref_pos + 1);
                        }
                    }
                    TWO_DIM_VERT_L3 => {
                        let a = self.ref_line[ref_pos] as i64 - 3;
                        self.add_pixels_neg(a, black_pixels);
                        black_pixels ^= 1;
                        if self.coding_line[self.coding_pos] < columns {
                            let next = if ref_pos > 0 {
                                ref_pos - 1
                            } else {
                                ref_pos + 1
                            };
                            ref_pos = self.advance_ref_pos(next);
                        }
                    }
                    TWO_DIM_VERT_L2 => {
                        let a = self.ref_line[ref_pos] as i64 - 2;
                        self.add_pixels_neg(a, black_pixels);
                        black_pixels ^= 1;
                        if self.coding_line[self.coding_pos] < columns {
                            let next = if ref_pos > 0 {
                                ref_pos - 1
                            } else {
                                ref_pos + 1
                            };
                            ref_pos = self.advance_ref_pos(next);
                        }
                    }
                    TWO_DIM_VERT_L1 => {
                        let a = self.ref_line[ref_pos] as i64 - 1;
                        self.add_pixels_neg(a, black_pixels);
                        black_pixels ^= 1;
                        if self.coding_line[self.coding_pos] < columns {
                            let next = if ref_pos > 0 {
                                ref_pos - 1
                            } else {
                                ref_pos + 1
                            };
                            ref_pos = self.advance_ref_pos(next);
                        }
                    }
                    TWO_DIM_VERT0 => {
                        let a = self.ref_line[ref_pos] as i64;
                        self.add_pixels(a, black_pixels);
                        black_pixels ^= 1;
                        if self.coding_line[self.coding_pos] < columns {
                            ref_pos = self.advance_ref_pos(ref_pos + 1);
                        }
                    }
                    -1 => {
                        self.add_pixels(columns as i64, 0);
                        self.eof = true;
                    }
                    _ => {
                        log::debug!("ccitt: bad 2-D code");
                        self.add_pixels(columns as i64, 0);
                        self.err = true;
                    }
                }
            }
        } else {
            self.coding_line[0] = 0;
            self.coding_pos = 0;
            let mut black_pixels: u32 = 0;
            while self.coding_line[self.coding_pos] < columns {
                let mut code1 = 0u32;
                if black_pixels != 0 {
                    loop {
                        let c = self.get_black_code();
                        code1 += c as u32;
                        if c < 64 {
                            break;
                        }
                    }
                } else {
                    loop {
                        let c = self.get_white_code();
                        code1 += c as u32;
                        if c < 64 {
                            break;
                        }
                    }
                }
                let base = self.coding_line[self.coding_pos] as i64;
                self.add_pixels(base + code1 as i64, black_pixels);
                black_pixels ^= 1;
            }
        }

        let mut got_eol = false;

        if self.byte_align {
            self.input_bits &= !7;
        }

        if !self.eoblock && self.rows > 0 && self.row == self.rows - 1 {
            self.rows_done = true;
        } else {
            let mut code1 = self.look_bits(12);
            if self.eoline {
                while code1 != -1 && code1 != 1 {
                    self.eat_bits(1);
                    code1 = self.look_bits(12);
                }
            } else {
                while code1 == 0 {
                    self.eat_bits(1);
                    code1 = self.look_bits(12);
                }
            }
            if code1 == 1 {
                self.eat_bits(12);
                got_eol = true;
            } else if code1 == -1 {
                self.eof = true;
            }
        }

        if !self.eof && self.encoding > 0 && !self.rows_done {
            self.next_line_2d = self.look_bits(1) == 0;
            self.eat_bits(1);
        }

        if self.eoblock && got_eol && self.byte_align {
            // Return-To-Control: EOL 1 EOL EOL EOL EOL EOL EOL (+ext bits).
            let code1 = self.look_bits(12);
            if code1 == 1 {
                self.eat_bits(12);
                if self.encoding > 0 {
                    let _ = self.look_bits(1);
                    self.eat_bits(1);
                }
                if self.encoding >= 0 {
                    for _ in 0..4 {
                        let c = self.look_bits(12);
                        if c != 1 {
                            log::debug!("ccitt: bad RTC code");
                        }
                        self.eat_bits(12);
                        if self.encoding > 0 {
                            let _ = self.look_bits(1);
                            self.eat_bits(1);
                        }
                    }
                }
                self.eof = true;
            }
        } else if self.err && self.eoline {
            // Resync: search for an EOL, then honour its 1-D/2-D tag bit.
            let mut code1;
            loop {
                code1 = self.look_bits(13);
                if code1 == -1 {
                    self.eof = true;
                    return;
                }
                if code1 >> 1 == 1 {
                    break;
                }
                self.eat_bits(1);
            }
            self.eat_bits(12);
            if self.encoding > 0 {
                self.eat_bits(1);
                self.next_line_2d = code1 & 1 == 0;
            }
        }

        // First output run: if the row starts with white at position 0, the
        // first boundary is coding_line[0]; otherwise coding_line[1].
        if self.coding_line[0] > 0 {
            self.coding_pos = 0;
            self.output_bits = self.coding_line[0];
        } else {
            self.coding_pos = 1;
            self.output_bits = self.coding_line[1];
        }
        self.row += 1;
    }

    /// Emit the next output byte, or None at end of stream
    /// (reference `readNextChar`).
    fn read_next_byte(&mut self) -> Option<u8> {
        if self.eof {
            return None;
        }
        if self.output_bits == 0 {
            // Reference: `if (rowsDone) eof = true; if (eof) return -1;` —
            // a finished !eoblock stream stops without a phantom row.
            if self.rows_done {
                self.eof = true;
                return None;
            }
            // A row decoded in this call may set eof; the reference still
            // emits this call's byte from whatever output runs exist.
            self.decode_row();
        }

        let columns = self.columns;
        let mut c: u32;
        if self.output_bits >= 8 {
            c = if self.coding_pos & 1 == 1 { 0 } else { 0xff };
            self.output_bits -= 8;
            if self.output_bits == 0 && self.coding_line[self.coding_pos] < columns {
                self.coding_pos += 1;
                self.output_bits =
                    self.coding_line[self.coding_pos] - self.coding_line[self.coding_pos - 1];
            }
        } else {
            let mut bits = 8u32;
            c = 0;
            loop {
                if self.output_bits > bits {
                    c <<= bits;
                    if self.coding_pos & 1 == 0 {
                        c |= 0xff >> (8 - bits);
                    }
                    self.output_bits -= bits;
                    bits = 0;
                } else {
                    c <<= self.output_bits;
                    if self.coding_pos & 1 == 0 {
                        c |= 0xff >> (8 - self.output_bits);
                    }
                    bits -= self.output_bits;
                    self.output_bits = 0;
                    if self.coding_line[self.coding_pos] < columns {
                        self.coding_pos += 1;
                        self.output_bits = self.coding_line[self.coding_pos]
                            - self.coding_line[self.coding_pos - 1];
                    } else if bits > 0 {
                        c <<= bits;
                        bits = 0;
                    } else {
                        break;
                    }
                }
                if bits == 0 {
                    break;
                }
            }
        }
        let byte = (c & 0xff) as u8;
        Some(if self.black { byte ^ 0xff } else { byte })
    }
}

// ---------------------------------------------------------------------------
// Tests (R5)
//
// Encoded streams were generated by `tests/gen_ccitt_fixtures.py` from the
// T.4/T.6 code tables in `tests/ccitt-reference.js` (pdf.js v3.11.174) and
// VALIDATED by running that exact reference decoder under node; every
// *_EXPECT constant below is the byte sequence pdf.js itself produced.
// ---------------------------------------------------------------------------

// Tests live in a sibling file to respect the 800-line module cap (R4).
#[cfg(test)]
#[path = "ccitt_tests.rs"]
mod tests;
