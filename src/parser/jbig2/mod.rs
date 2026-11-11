//! JBIG2 decoder (ITU-T T.88) — port of pdf.js v3.11.174 `src/core/jbig2.js`
//! (Apache 2.0), covering the segment types used by PDF's `/JBIG2Decode`
//! filter: symbol dictionaries (0), text regions (4/6/7), pattern
//! dictionaries (16), halftone regions (20/22/23), generic regions
//! (36/38/39, MMR + arithmetic), custom Huffman tables (53) and page
//! information (48).
//!
//! The public entry point is [`decode`], which mirrors pdf.js
//! `parseJbig2Chunks`: it processes the optional `/JBIG2Globals` segment
//! stream followed by the image stream and returns the packed 1-bpp page
//! buffer (1 = black, row stride `(width + 7) / 8`).

pub mod arith;
pub mod dec;
pub mod generic;
pub mod halftone;
pub mod huffman;
pub mod segments;
pub mod symbol;
pub mod tables;

// Tests live in a sibling file to respect the 800-line module cap (R4);
// fixture constants generated + node-validated by `tests/gen_jbig2_fixtures.py`.
#[cfg(test)]
mod tests;

use crate::error::Result;

/// Decode a JBIG2 image stream (and its optional `/JBIG2Globals` companion)
/// into `(width, height, packed_rows)`.
///
/// `packed_rows` holds one packed row per scanline with stride
/// `(width + 7) / 8`, MSB-first bits, `1` = black — the raw JBIG2
/// convention. Callers feeding PDF image samples must invert the bytes
/// (pdf.js `Jbig2Stream.readBlock` does `data[i] ^= 0xff`).
///
/// # Errors
/// Malformed or truncated segment data, unsupported segment types, or any
/// region-decode failure.
pub fn decode(embedded: &[u8], globals: Option<&[u8]>) -> Result<(u32, u32, Vec<u8>)> {
    match globals {
        Some(g) => segments::decode_chunks(&[g, embedded]),
        None => segments::decode_chunks(&[embedded]),
    }
}

pub use segments::decode_chunks;
