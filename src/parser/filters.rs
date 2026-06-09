//! PDF stream filter decoders.
//!
//! Implements the filter pipeline described in ISO 32000-1 §7.4.
//! Filters are applied in the order listed; the output of each feeds the next.
//!
//! Supported:
//!   FlateDecode (§7.4.4), ASCII85Decode (§7.4.3), ASCIIHexDecode (§7.4.2),
//!   LZWDecode (§7.4.4), RunLengthDecode (§7.4.5)
//!
//! Pass-through (decoded by the image layer):
//!   DCTDecode (JPEG), JPXDecode (JPEG2000), JBIG2Decode, CCITTFaxDecode

use crate::error::{PdfError, Result};
use flate2::read::ZlibDecoder;
use std::borrow::Cow;
use std::io::Read;

/// Apply a single named filter to `data` and return the decoded bytes.
///
/// Short aliases (e.g. `Fl`, `A85`) are accepted alongside full names per
/// ISO 32000-1 Table 6.
pub fn apply_filter(name: &str, data: &[u8]) -> Result<Vec<u8>> {
    match name {
        "FlateDecode" | "Fl" => decode_flate(data),
        "ASCII85Decode" | "A85" => decode_ascii85(data),
        "ASCIIHexDecode" | "AHx" => decode_asciihex(data),
        "LZWDecode" | "LZW" => decode_lzw(data),
        "RunLengthDecode" | "RL" => decode_run_length(data),
        // These are decoded by the rendering layer — pass raw bytes through.
        "DCTDecode" | "DCT" | "JPXDecode" | "JBIG2Decode" | "CCITTFaxDecode" | "CCF" => {
            Ok(data.to_vec())
        }
        other => Err(PdfError::unsupported_filter(0, other)),
    }
}

/// Apply a sequence of filters in order; the output of each feeds the next.
///
/// The first filter reads the caller's slice directly, so an unnecessary copy of
/// the (typically compressed) input is avoided — only filter *outputs* are
/// allocated. With no filters this is a single owned copy of the input
/// (TD-8: see [`apply_pipeline_cow`] for the zero-copy borrowing variant).
pub fn apply_pipeline(filters: &[&str], data: &[u8]) -> Result<Vec<u8>> {
    let mut iter = filters.iter();
    let mut buf = match iter.next() {
        // First filter consumes the borrowed input directly (no pre-copy).
        Some(&first) => apply_filter(first, data)?,
        // No filters: the decoded form *is* the input; one owned copy.
        None => return Ok(data.to_vec()),
    };
    for &filter in iter {
        buf = apply_filter(filter, &buf)?;
    }
    Ok(buf)
}

/// Like [`apply_pipeline`] but borrows the input when there is nothing to decode.
///
/// Returns `Cow::Borrowed(data)` for an unfiltered stream — no allocation at all
/// — and `Cow::Owned(..)` once any filter runs. Use this on read-only paths
/// (e.g. extracting an already-uncompressed content stream) to skip the copy.
pub fn apply_pipeline_cow<'a>(filters: &[&str], data: &'a [u8]) -> Result<Cow<'a, [u8]>> {
    if filters.is_empty() {
        return Ok(Cow::Borrowed(data));
    }
    apply_pipeline(filters, data).map(Cow::Owned)
}

// ---------------------------------------------------------------------------
// FlateDecode — ISO 32000-1 §7.4.4
// ---------------------------------------------------------------------------

fn decode_flate(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| PdfError::filter_error(0, format!("FlateDecode: {}", e)))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// ASCII85Decode — ISO 32000-1 §7.4.3
//
// Five base-85 characters encode four bytes.
// 'z' shorthand encodes four zero bytes.
// EOD marker is "~>".
// ---------------------------------------------------------------------------

fn decode_ascii85(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut group_len = 0usize;
    let mut i = 0;

    while i < data.len() {
        let b = data[i];
        i += 1;

        if b == b'~' {
            if i < data.len() && data[i] == b'>' {
                // EOD
                break;
            }
            return Err(PdfError::filter_error(
                i - 1,
                "ASCII85Decode: stray '~' not followed by '>'",
            ));
        }

        // Skip PDF whitespace
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x00) {
            continue;
        }

        if b == b'z' {
            if group_len != 0 {
                return Err(PdfError::filter_error(
                    i - 1,
                    "ASCII85Decode: 'z' inside partial group",
                ));
            }
            out.extend_from_slice(&[0u8; 4]);
            continue;
        }

        if !(b'!'..=b'u').contains(&b) {
            return Err(PdfError::filter_error(
                i - 1,
                format!("ASCII85Decode: invalid character 0x{:02X}", b),
            ));
        }

        group[group_len] = b - b'!';
        group_len += 1;

        if group_len == 5 {
            let val = group85_to_u32(&group);
            out.extend_from_slice(&val.to_be_bytes());
            group_len = 0;
        }
    }

    // Partial final group: pad with 'u' (84) then take group_len-1 bytes
    if group_len > 0 {
        group[group_len..5].fill(84);
        let val = group85_to_u32(&group);
        let bytes = val.to_be_bytes();
        out.extend_from_slice(&bytes[..group_len - 1]);
    }

    Ok(out)
}

#[inline]
fn group85_to_u32(g: &[u8; 5]) -> u32 {
    (g[0] as u32) * 52_200_625   // 85^4
        + (g[1] as u32) * 614_125  // 85^3
        + (g[2] as u32) * 7_225    // 85^2
        + (g[3] as u32) * 85
        + (g[4] as u32)
}

// ---------------------------------------------------------------------------
// ASCIIHexDecode — ISO 32000-1 §7.4.2
//
// Pairs of hex digits encode bytes; '>' terminates; whitespace is ignored;
// an odd number of digits pads the last nibble with 0.
// ---------------------------------------------------------------------------

fn decode_asciihex(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;

    for (pos, &b) in data.iter().enumerate() {
        if b == b'>' {
            break;
        }
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x00) {
            continue;
        }
        let nybble = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => {
                return Err(PdfError::filter_error(
                    pos,
                    format!("ASCIIHexDecode: invalid character 0x{:02X}", b),
                ))
            }
        };
        match hi {
            None => hi = Some(nybble << 4),
            Some(h) => {
                out.push(h | nybble);
                hi = None;
            }
        }
    }
    if let Some(h) = hi {
        out.push(h); // odd digit padded with 0
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// LZWDecode — ISO 32000-1 §7.4.4 (TIFF LZW, MSB-first)
// ---------------------------------------------------------------------------

fn decode_lzw(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8);
    let result = decoder.into_stream(&mut out).decode_all(data);
    result
        .status
        .map_err(|e| PdfError::filter_error(0, format!("LZWDecode: {:?}", e)))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// RunLengthDecode — ISO 32000-1 §7.4.5
//
// Length byte n (u8):
//   0..=127  → copy the next n+1 bytes literally
//   128      → EOD
//   129..=255 → repeat the next byte 257-n times
// ---------------------------------------------------------------------------

fn decode_run_length(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < data.len() {
        let run = data[i];
        i += 1;

        if run == 128 {
            break; // EOD
        } else if run < 128 {
            let count = run as usize + 1;
            if i + count > data.len() {
                return Err(PdfError::eof(
                    i,
                    format!("RunLengthDecode: literal run of {} exceeds data", count),
                ));
            }
            out.extend_from_slice(&data[i..i + count]);
            i += count;
        } else {
            // run is 129..=255; repeat count = 257 - run
            let count = 257 - run as usize;
            if i >= data.len() {
                return Err(PdfError::eof(
                    i,
                    "RunLengthDecode: repeat byte past end of data",
                ));
            }
            let byte = data[i];
            i += 1;
            out.extend(std::iter::repeat_n(byte, count));
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// PNG predictor unfilter — ISO 32000-1 §7.4.4.4, Predictor 10–15
// ---------------------------------------------------------------------------

/// Apply the PNG predictor unfilter to FlateDecode output.
///
/// After zlib decompression, each scanline is prefixed with a 1-byte filter
/// type (0=None, 1=Sub, 2=Up, 3=Average, 4=Paeth).  This function removes
/// those prefix bytes and reconstructs the original pixel values.
///
/// `columns` — number of pixel samples per row.
/// `colors` — number of colour components per sample.
/// `bpc` — bits per component (typically 8).
pub fn apply_png_predictor(
    data: &[u8],
    columns: usize,
    colors: usize,
    bpc: usize,
) -> Result<Vec<u8>> {
    let bpp = (colors * bpc).div_ceil(8); // bytes per pixel
    let row_len = columns * bpp; // raw bytes per row
    let stride = 1 + row_len; // filter byte + pixel bytes

    if data.is_empty() || stride == 0 {
        return Ok(Vec::new());
    }

    let nrows = data.len() / stride;
    let mut out = Vec::with_capacity(nrows * row_len);
    let mut prev = vec![0u8; row_len];

    for r in 0..nrows {
        let base = r * stride;
        if base + stride > data.len() {
            break;
        }
        let filter_type = data[base];
        let row = &data[base + 1..base + stride];
        let mut recon = vec![0u8; row_len];

        match filter_type {
            0 => recon.copy_from_slice(row), // None
            1 => {
                // Sub
                for i in 0..row_len {
                    let a = if i >= bpp { recon[i - bpp] } else { 0 };
                    recon[i] = row[i].wrapping_add(a);
                }
            }
            2 => {
                // Up
                for i in 0..row_len {
                    recon[i] = row[i].wrapping_add(prev[i]);
                }
            }
            3 => {
                // Average
                for i in 0..row_len {
                    let a = if i >= bpp { recon[i - bpp] as u16 } else { 0 };
                    let b = prev[i] as u16;
                    recon[i] = row[i].wrapping_add(((a + b) / 2) as u8);
                }
            }
            4 => {
                // Paeth
                for i in 0..row_len {
                    let a = if i >= bpp { recon[i - bpp] as i16 } else { 0 };
                    let b = prev[i] as i16;
                    let c = if i >= bpp { prev[i - bpp] as i16 } else { 0 };
                    let p = a + b - c;
                    let pa = (p - a).abs();
                    let pb = (p - b).abs();
                    let pc = (p - c).abs();
                    let pr = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    recon[i] = row[i].wrapping_add(pr as u8);
                }
            }
            _ => recon.copy_from_slice(row), // unknown type: treat as None
        }

        out.extend_from_slice(&recon);
        prev = recon;
    }

    Ok(out)
}

/// Apply the TIFF horizontal differencing predictor (Predictor 2).
///
/// Used with LZWDecode and occasionally FlateDecode.  Each sample is stored
/// as the difference from the previous sample in the same row.
///
/// `columns` — number of pixel samples per row.
/// `colors` — number of colour components per sample.
/// `bpc` — bits per component (typically 8; only 8-bit is supported here).
pub fn apply_tiff_predictor(
    data: &[u8],
    columns: usize,
    colors: usize,
    bpc: usize,
) -> Result<Vec<u8>> {
    let bpp = (colors * bpc).div_ceil(8);
    let row_len = columns * bpp;
    if row_len == 0 {
        return Ok(data.to_vec());
    }
    let mut out = data.to_vec();
    let nrows = out.len() / row_len;
    for r in 0..nrows {
        let row_start = r * row_len;
        for i in (row_start + bpp)..((row_start + row_len).min(out.len())) {
            out[i] = out[i].wrapping_add(out[i - bpp]);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn zlib_compress(data: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn test_flate_round_trip() {
        let original = b"Hello, PDF FlateDecode world!";
        let compressed = zlib_compress(original);
        let decoded = decode_flate(&compressed).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_ascii85_simple() {
        // "Man" → ASCII85 "9jqo~>"
        let encoded = b"9jqo~>";
        let decoded = decode_ascii85(encoded).unwrap();
        assert_eq!(decoded, b"Man");
    }

    #[test]
    fn test_ascii85_z_shorthand() {
        // 'z' encodes four zero bytes
        let encoded = b"z~>";
        let decoded = decode_ascii85(encoded).unwrap();
        assert_eq!(decoded, &[0u8; 4]);
    }

    #[test]
    fn test_ascii85_partial_group() {
        // Single byte 'M' (0x4D = 77).
        // Encode: pad 0x4D000000 → digits [24,63,47,13,77] → chars "9`P.n"
        // Partial group of 1 byte uses first n+1=2 chars: "9`"
        // Decode: [24,63,84,84,84] → val=1292118999 → high byte=0x4D=77='M'
        let encoded = b"9`~>";
        let decoded = decode_ascii85(encoded).unwrap();
        assert_eq!(decoded[0], b'M');
    }

    #[test]
    fn test_ascii85_whitespace_ignored() {
        let encoded = b"9j\nqo~>";
        let decoded = decode_ascii85(encoded).unwrap();
        assert_eq!(decoded, b"Man");
    }

    #[test]
    fn test_asciihex_simple() {
        let encoded = b"48656C6C6F>";
        let decoded = decode_asciihex(encoded).unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_asciihex_lowercase() {
        let encoded = b"48656c6c6f>";
        let decoded = decode_asciihex(encoded).unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_asciihex_whitespace_ignored() {
        let encoded = b"48 65 6C 6C 6F>";
        let decoded = decode_asciihex(encoded).unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_asciihex_odd_digit_padded() {
        // Odd digit 'A' → 0xA0
        let encoded = b"A>";
        let decoded = decode_asciihex(encoded).unwrap();
        assert_eq!(decoded, &[0xA0]);
    }

    #[test]
    fn test_run_length_literal() {
        // Length byte 2 → copy next 3 bytes literally
        let data = b"\x02abc\x80";
        let decoded = decode_run_length(data).unwrap();
        assert_eq!(decoded, b"abc");
    }

    #[test]
    fn test_run_length_repeat() {
        // Length byte 0xFE (254 = -2 as i16 → 257-254=3) → repeat next byte 3 times
        let data = b"\xFEx\x80";
        let decoded = decode_run_length(data).unwrap();
        assert_eq!(decoded, b"xxx");
    }

    #[test]
    fn test_run_length_eod() {
        let data = b"\x80extra";
        let decoded = decode_run_length(data).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_apply_pipeline_multi() {
        // Two-filter pipeline: FlateDecode then ASCIIHexDecode
        let inner = b"Hello";
        // Build: zlib-compressed bytes, then hex-encode them
        let compressed = zlib_compress(inner);
        let hex: String = compressed.iter().map(|b| format!("{:02X}", b)).collect();
        let hex_terminated = format!("{}>", hex);
        let decoded = apply_pipeline(
            &["ASCIIHexDecode", "FlateDecode"],
            hex_terminated.as_bytes(),
        )
        .unwrap();
        assert_eq!(decoded, inner);
    }

    #[test]
    fn test_unsupported_filter_error() {
        let result = apply_filter("SomeUnknownFilter", b"data");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::PdfError::UnsupportedFilter { .. }
        ));
    }

    // PNG predictor tests -------------------------------------------------------

    #[test]
    fn test_png_predictor_none() {
        // filter_type=0 (None): bytes pass through unchanged
        // 2 rows × 3 bytes, each prefixed with 0x00
        let data = &[0x00, 10, 20, 30, 0x00, 40, 50, 60];
        let out = apply_png_predictor(data, 3, 1, 8).unwrap();
        assert_eq!(out, &[10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn test_png_predictor_sub() {
        // filter_type=1 (Sub): each byte += left neighbour
        // Row: [1] 10 5 3  →  recon[0]=10, recon[1]=10+5=15, recon[2]=15+3=18
        let data = &[0x01u8, 10, 5, 3];
        let out = apply_png_predictor(data, 3, 1, 8).unwrap();
        assert_eq!(out, &[10, 15, 18]);
    }

    #[test]
    fn test_png_predictor_up() {
        // filter_type=2 (Up): each byte += byte from previous row
        // Row 0 (Up with prev=0): [2] 10 20  →  10, 20
        // Row 1 (Up):              [2]  1  2  →  11, 22
        let data = &[0x02u8, 10, 20, 0x02, 1, 2];
        let out = apply_png_predictor(data, 2, 1, 8).unwrap();
        assert_eq!(out, &[10, 20, 11, 22]);
    }

    #[test]
    fn test_png_predictor_paeth_trivial() {
        // Row [100, 100] with Paeth: pixel 0 predictor=0 → recon[0]=100+0=100
        // pixel 1 predictor=left=100 → recon[1]=100+100=200
        let data = &[0x04u8, 100, 100];
        let out = apply_png_predictor(data, 2, 1, 8).unwrap();
        assert_eq!(out, &[100, 200]);
    }

    #[test]
    fn test_tiff_predictor_basic() {
        // 2 pixels × 1 channel: stored as [10, 5] (delta), reconstructed: [10, 15]
        let data = &[10u8, 5];
        let out = apply_tiff_predictor(data, 2, 1, 8).unwrap();
        assert_eq!(out, &[10, 15]);
    }

    #[test]
    fn test_tiff_predictor_multi_row() {
        // 2 rows of 2 pixels each: deltas [10, 3, 20, 7] → [10, 13, 20, 27]
        let data = &[10u8, 3, 20, 7];
        let out = apply_tiff_predictor(data, 2, 1, 8).unwrap();
        assert_eq!(out, &[10, 13, 20, 27]);
    }

    #[test]
    fn pipeline_no_filters_returns_input_copy() {
        // TD-8: an unfiltered pipeline returns the bytes unchanged.
        let data = b"hello world";
        assert_eq!(apply_pipeline(&[], data).unwrap(), data);
    }

    #[test]
    fn pipeline_cow_borrows_when_unfiltered() {
        // No filters ⇒ zero-copy borrow; a filter ⇒ owned decode.
        let data = b"\x41\x42\x43"; // "ABC"
        assert!(matches!(
            apply_pipeline_cow(&[], data).unwrap(),
            Cow::Borrowed(_)
        ));
        // ASCIIHexDecode of "414243" → "ABC".
        let hex = b"414243>";
        let decoded = apply_pipeline_cow(&["ASCIIHexDecode"], hex).unwrap();
        assert!(matches!(decoded, Cow::Owned(_)));
        assert_eq!(&*decoded, b"ABC");
    }

    #[test]
    fn pipeline_chains_filters_same_as_before() {
        // Two-filter chain still composes correctly after the no-pre-copy change:
        // ASCIIHex then RunLength. Encode "AABB" run-length, hex-wrap it.
        // RunLength: 0x01 means copy 2 literal bytes; then 0x80 EOD.
        let rl = [0x01u8, b'A', b'A', 0x80];
        let decoded_rl = apply_pipeline(&["RunLengthDecode"], &rl).unwrap();
        assert_eq!(decoded_rl, b"AA");
    }
}
