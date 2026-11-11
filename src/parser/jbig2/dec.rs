//! Shared decode helpers for JBIG2: bit reader, adaptive-integer decoding
//! (Annex A), IAID decoding, the bitmap type, and the MMR glue used by
//! generic/halftone regions and collective bitmaps.
//!
//! Port of pdf.js v3.11.174 `src/core/jbig2.js` (Apache 2.0) — the `Reader`,
//! `ContextCache`/`DecodingContext`, `decodeInteger`, `decodeIAID` and
//! `decodeMMRBitmap` pieces.

use super::arith::ArithmeticDecoder;
use crate::error::{PdfError, Result};

/// A decoded JBIG2 bitmap: one `Vec<u8>` row per scanline, 0/1 pixels.
pub type Bitmap = Vec<Vec<u8>>;

/// Bit reader over a byte window (reference `Reader`), MSB first.
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub start: usize,
    pub end: usize,
    pub position: usize,
    shift: i32,
    current_byte: u8,
}

impl<'a> Reader<'a> {
    /// New reader over `data[start..end]`.
    pub fn new(data: &'a [u8], start: usize, end: usize) -> Self {
        Reader {
            data,
            start,
            end,
            position: start,
            shift: -1,
            current_byte: 0,
        }
    }

    /// Read one bit; returns 0/1.
    ///
    /// # Errors
    /// Reading past the window end (reference throws "end of data").
    pub fn read_bit(&mut self) -> Result<u32> {
        if self.shift < 0 {
            if self.position >= self.end {
                return Err(PdfError::filter_error(
                    self.position,
                    "JBIG2: end of data while reading bit",
                ));
            }
            self.current_byte = self.data[self.position];
            self.position += 1;
            self.shift = 7;
        }
        let bit = ((self.current_byte >> self.shift) & 1) as u32;
        self.shift -= 1;
        Ok(bit)
    }

    /// Read `num_bits` bits, MSB first.
    pub fn read_bits(&mut self, num_bits: u32) -> Result<u32> {
        let mut result = 0u32;
        for i in (0..num_bits).rev() {
            result |= self.read_bit()? << i;
        }
        Ok(result)
    }

    /// Drop bits up to the next byte boundary.
    pub fn byte_align(&mut self) {
        self.shift = -1;
    }

    /// Next raw byte, or -1 at end of window (reference `next`).
    pub fn next_byte(&mut self) -> i32 {
        if self.position >= self.end {
            return -1;
        }
        let b = self.data[self.position];
        self.position += 1;
        b as i32
    }
}

/// Adaptive arithmetic context store — one `Int8Array(1<<16)` per procedure
/// id (reference `ContextCache`).
pub struct ContextCache {
    tables: [Option<Vec<u8>>; 64],
}

impl Default for ContextCache {
    fn default() -> Self {
        ContextCache {
            tables: std::array::from_fn(|_| None),
        }
    }
}

/// Context procedure ids (reference uses strings — "GB", "GR", "IADH", …).
/// One distinct slot per procedure, matching the reference's per-string
/// context arrays.
pub mod ctx {
    pub const GB: usize = 0;
    pub const GR: usize = 1;
    pub const IADH: usize = 2;
    pub const IADW: usize = 3;
    pub const IAAI: usize = 4;
    pub const IAEX: usize = 5;
    pub const IADT: usize = 6;
    pub const IAFS: usize = 7;
    pub const IAIT: usize = 8;
    pub const IARI: usize = 9;
    pub const IARDW: usize = 10;
    pub const IARDH: usize = 11;
    pub const IARDX: usize = 12;
    pub const IARDY: usize = 13;
    pub const IADS: usize = 14;
    pub const IAID: usize = 15;
}

impl ContextCache {
    /// Get (creating on first use) the 65536-entry context array for `id`.
    pub fn get_contexts(&mut self, id: usize) -> &mut Vec<u8> {
        const LEN: usize = 1 << 16;
        let slot = &mut self.tables[id % 64];
        slot.get_or_insert_with(|| vec![0u8; LEN])
    }
}

/// Coded-data window plus lazily created decoder/contexts
/// (reference `DecodingContext`).
pub struct DecodingContext<'a> {
    pub data: &'a [u8],
    pub start: usize,
    pub end: usize,
}

impl<'a> DecodingContext<'a> {
    /// New decoding context over `data[start..end]`.
    pub fn new(data: &'a [u8], start: usize, end: usize) -> Self {
        DecodingContext { data, start, end }
    }

    /// Fresh arithmetic decoder over the window (reference creates one per
    /// accessor call; the port creates one per region decode).
    pub fn decoder(&self) -> ArithmeticDecoder<'a> {
        ArithmeticDecoder::new(self.data, self.start, self.end)
    }
}

/// Per-region decode state: MQ decoder, shared adaptive contexts, and — for
/// Huffman-coded regions — the bit reader positioned after the region
/// header. Mirrors the reference's `decodingContext` + `huffmanInput` pair.
/// Field borrows (`cx.decoder`, `cx.cache`, `cx.reader`) stay disjoint, so
/// callers can hold decoder/cache across reader uses.
pub struct DecodeCtx<'a> {
    /// Raw coded-data window (MMR sub-windows slice into this).
    pub data: &'a [u8],
    /// MQ decoder (always constructed; unused for Huffman regions).
    pub decoder: ArithmeticDecoder<'a>,
    /// Shared adaptive integer/bitmap contexts.
    pub cache: ContextCache,
    /// Huffman bit reader, `Some` only for Huffman-coded regions.
    pub reader: Option<Reader<'a>>,
}

impl<'a> DecodeCtx<'a> {
    /// Build from a [`DecodingContext`], optionally with a pre-positioned
    /// Huffman reader over the same data.
    pub fn new(dc: &DecodingContext<'a>, reader: Option<Reader<'a>>) -> Self {
        DecodeCtx {
            data: dc.data,
            decoder: dc.decoder(),
            cache: ContextCache::default(),
            reader,
        }
    }

    /// The Huffman reader, or an error when the region is not Huffman-coded.
    pub fn reader_mut(&mut self) -> Result<&mut Reader<'a>> {
        self.reader
            .as_mut()
            .ok_or_else(|| PdfError::filter_error(0, "JBIG2: huffman input missing"))
    }
}

const MAX_INT_32: i64 = i32::MAX as i64;
const MIN_INT_32: i64 = i32::MIN as i64;

/// Annex A.2 arithmetic integer decoding procedure.
///
/// `procedure` selects the adaptive context table (`"IADH"` → 0, etc.); the
/// mapping used here matches the reference's string ids.
///
/// Returns `None` on OOB-style overflow (reference returns `null`).
#[allow(clippy::too_many_lines)]
pub fn decode_integer(
    cache: &mut ContextCache,
    procedure: usize,
    decoder: &mut ArithmeticDecoder,
) -> Option<i32> {
    let contexts = cache.get_contexts(procedure);
    let mut prev: usize = 1;

    // readBits helper over the procedure's own contexts.
    fn read_bits(
        contexts: &mut [u8],
        decoder: &mut ArithmeticDecoder,
        prev: &mut usize,
        length: u32,
    ) -> u32 {
        let mut v = 0u32;
        for _ in 0..length {
            let bit = decoder.read_bit(contexts, *prev);
            *prev = if *prev < 256 {
                (*prev << 1) | bit as usize
            } else {
                (((*prev << 1) | bit as usize) & 511) | 256
            };
            v = (v << 1) | bit;
        }
        v
    }

    let sign = read_bits(contexts, decoder, &mut prev, 1);
    let value: i64 = if read_bits(contexts, decoder, &mut prev, 1) == 0 {
        read_bits(contexts, decoder, &mut prev, 2) as i64
    } else if read_bits(contexts, decoder, &mut prev, 1) == 0 {
        read_bits(contexts, decoder, &mut prev, 4) as i64 + 4
    } else if read_bits(contexts, decoder, &mut prev, 1) == 0 {
        read_bits(contexts, decoder, &mut prev, 6) as i64 + 20
    } else if read_bits(contexts, decoder, &mut prev, 1) == 0 {
        read_bits(contexts, decoder, &mut prev, 8) as i64 + 84
    } else if read_bits(contexts, decoder, &mut prev, 1) == 0 {
        read_bits(contexts, decoder, &mut prev, 12) as i64 + 340
    } else {
        read_bits(contexts, decoder, &mut prev, 32) as i64 + 4436
    };

    // A.2: sign=1 with value=0 leaves JS `signedValue` undefined, which
    // fails the range check — the reference returns null (the OOB marker).
    // Map that case to None instead of collapsing it to Some(0).
    if sign != 0 && value == 0 {
        return None;
    }
    let signed_value: i64 = if sign == 0 { value } else { -value };
    if (MIN_INT_32..=MAX_INT_32).contains(&signed_value) {
        Some(signed_value as i32)
    } else {
        None
    }
}

/// A.3 The IAID decoding procedure.
pub fn decode_iaid(
    cache: &mut ContextCache,
    decoder: &mut ArithmeticDecoder,
    code_length: u32,
) -> u32 {
    let contexts = cache.get_contexts(ctx::IAID);
    let mut prev: u32 = 1;
    for _ in 0..code_length {
        let bit = decoder.read_bit(contexts, prev as usize);
        prev = (prev << 1) | bit;
    }
    if code_length < 31 {
        prev & ((1u32 << code_length) - 1)
    } else {
        prev & 0x7fff_ffff
    }
}

/// floor(log2(x)); 0 for x <= 1 (reference `log2` helper).
pub fn log2(x: usize) -> u32 {
    if x <= 1 {
        0
    } else {
        31 - (x as u32).leading_zeros()
    }
}

/// MMR-coded bitmap decode — the same compression as CCITTFaxDecode with
/// `/K -1` (reference `decodeMMRBitmap`). Returns the bitmap and the number
/// of bytes consumed from `window` (needed when several MMR bitmaps share
/// one stream, e.g. halftone gray-scale planes).
///
/// `end_of_block` mirrors pdf.js: `true` means the stream carries EOFB codes
/// that must be consumed after the rows.
pub fn decode_mmr_bitmap(
    window: &[u8],
    width: u32,
    height: u32,
    end_of_block: bool,
) -> Result<(Bitmap, usize)> {
    let params = super::super::ccitt_parms::FaxParms {
        k: -1,
        columns: width,
        rows: height,
        black_is1: true,
        byte_align: false,
        eoline: false,
        eoblock: end_of_block,
    };
    // The CCITT decoder consumes bytes lazily; its consumed-byte count is
    // required to keep a shared stream (halftone planes) in sync.
    let (bytes, consumed) = super::super::ccitt::decode_with_consumed(window, &params)?;
    let w = width as usize;
    let mut bitmap: Bitmap = Vec::with_capacity(height as usize);
    for row_bytes in bytes.chunks(w.div_ceil(8)) {
        let mut row = Vec::with_capacity(w);
        for x in 0..w {
            let bit = match row_bytes.get(x / 8) {
                Some(&b) => (b >> (7 - (x % 8))) & 1,
                None => 0,
            };
            row.push(bit);
        }
        bitmap.push(row);
    }
    // Missing rows (EOF before height) stay all-zero like the reference.
    while bitmap.len() < height as usize {
        bitmap.push(vec![0u8; w]);
    }
    Ok((bitmap, consumed))
}

/// Read a `width x height` bitmap bit-by-bit, byte-aligned per row
/// (reference `readUncompressedBitmap`).
pub fn read_uncompressed_bitmap(reader: &mut Reader, width: u32, height: u32) -> Result<Bitmap> {
    let mut bitmap = Vec::with_capacity(height as usize);
    for _ in 0..height {
        let mut row = Vec::with_capacity(width as usize);
        for _ in 0..width {
            row.push(reader.read_bit()? as u8);
        }
        reader.byte_align();
        bitmap.push(row);
    }
    Ok(bitmap)
}
