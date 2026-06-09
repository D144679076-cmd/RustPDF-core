//! CFF (Compact Font Format) parser for glyph width extraction.
//!
//! Parses CFF binary data (ISO 32000-1 Annex D, Adobe Type 2 Charstring spec)
//! to extract glyph advance widths needed for correct text layout.
//! Only the minimum structure is parsed — no full charstring execution.

use std::collections::HashMap;

/// Parsed CFF font with glyph metrics.
#[derive(Debug, Clone)]
pub struct CffFont {
    /// Default advance width (from Private DICT /defaultWidthX).
    pub default_width: f64,
    /// Nominal width (from Private DICT /nominalWidthX).
    pub nominal_width: f64,
    /// Glyph ID → advance width in 1/1000 em units.
    widths: HashMap<u16, f64>,
    /// Charset mapping.
    pub charset: CffCharset,
}

/// CFF charset variants.
#[derive(Debug, Clone)]
pub enum CffCharset {
    ISOAdobe,
    Expert,
    ExpertSubset,
    Custom(Vec<u16>),
}

/// CFF parsing error.
#[derive(Debug, Clone, PartialEq)]
pub enum CffError {
    TooShort,
    MissingIndex(&'static str),
    InvalidData(&'static str),
}

impl std::fmt::Display for CffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CffError::TooShort => write!(f, "CFF data truncated"),
            CffError::MissingIndex(name) => write!(f, "CFF missing INDEX: {}", name),
            CffError::InvalidData(msg) => write!(f, "CFF invalid data: {}", msg),
        }
    }
}

impl CffFont {
    /// Get the advance width for a glyph ID.
    pub fn glyph_width(&self, gid: u16) -> f64 {
        self.widths.get(&gid).copied().unwrap_or(self.default_width)
    }
}

/// Parse CFF binary data and extract glyph metrics.
///
/// Reads the minimum structure needed: Header, Name INDEX (skip),
/// Top DICT (charset/Charstrings/Private offsets), Private DICT
/// (defaultWidthX/nominalWidthX), Charset, and Charstring widths.
pub fn parse_cff(data: &[u8]) -> Result<CffFont, CffError> {
    if data.len() < 4 {
        return Err(CffError::TooShort);
    }

    // Header: major, minor, hdrSize, offSize
    let hdr_size = data[2] as usize;
    if data.len() < hdr_size {
        return Err(CffError::TooShort);
    }

    let mut pos = hdr_size;

    // Name INDEX — skip
    pos = skip_index(data, pos)?;

    // Top DICT INDEX — parse first entry
    let (top_dict_entries, after_top) = read_index(data, pos)?;
    if top_dict_entries.is_empty() {
        return Err(CffError::MissingIndex("Top DICT"));
    }
    let top_dict = parse_dict(&top_dict_entries[0])?;
    pos = after_top;

    // String INDEX — skip
    pos = skip_index(data, pos)?;

    // Global Subr INDEX — skip
    let _after_gsubr = skip_index(data, pos)?;

    // Extract offsets from Top DICT
    let charset_offset = dict_get_int(&top_dict, 15).unwrap_or(0) as usize;
    let charstrings_offset =
        dict_get_int(&top_dict, 17).ok_or(CffError::MissingIndex("CharStrings"))? as usize;
    let private_size_offset = dict_get_two_ints(&top_dict, 18);

    // Parse CharStrings INDEX to get glyph count
    if charstrings_offset >= data.len() {
        return Err(CffError::TooShort);
    }
    let (charstrings, _) = read_index(data, charstrings_offset)?;
    let num_glyphs = charstrings.len() as u16;

    // Parse Private DICT
    let (default_width, nominal_width) = match private_size_offset {
        Some((size, offset)) => {
            let off = offset as usize;
            let sz = size as usize;
            if off + sz > data.len() {
                (0.0, 0.0)
            } else {
                let private_dict = parse_dict(&data[off..off + sz])?;
                let dw = dict_get_real(&private_dict, 20).unwrap_or(0.0);
                let nw = dict_get_real(&private_dict, 21).unwrap_or(0.0);
                (dw, nw)
            }
        }
        None => (0.0, 0.0),
    };

    // Parse Charset
    let charset = parse_charset(data, charset_offset, num_glyphs)?;

    // Extract widths from charstrings (Type 2 width encoding)
    let mut widths = HashMap::new();
    for (gid, cs_data) in charstrings.iter().enumerate() {
        if let Some(w) = extract_charstring_width(cs_data, nominal_width, default_width) {
            widths.insert(gid as u16, w);
        }
    }

    Ok(CffFont {
        default_width,
        nominal_width,
        widths,
        charset,
    })
}

// ---------------------------------------------------------------------------
// INDEX parsing
// ---------------------------------------------------------------------------

/// Skip an INDEX structure and return the position after it.
fn skip_index(data: &[u8], pos: usize) -> Result<usize, CffError> {
    if pos + 2 > data.len() {
        return Err(CffError::TooShort);
    }
    let count = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    if count == 0 {
        return Ok(pos + 2);
    }
    if pos + 3 > data.len() {
        return Err(CffError::TooShort);
    }
    let off_size = data[pos + 2] as usize;
    if off_size == 0 || off_size > 4 {
        return Err(CffError::InvalidData("INDEX offSize out of range"));
    }

    let offset_array_start = pos + 3;
    let offset_array_len = (count + 1) * off_size;
    if offset_array_start + offset_array_len > data.len() {
        return Err(CffError::TooShort);
    }

    // Last offset gives the end of data
    let last_offset = read_offset(data, offset_array_start + count * off_size, off_size)?;
    let data_start = offset_array_start + offset_array_len;

    Ok(data_start + last_offset - 1)
}

/// Read an INDEX and return all entries as byte slices (copied to Vec).
fn read_index(data: &[u8], pos: usize) -> Result<(Vec<Vec<u8>>, usize), CffError> {
    if pos + 2 > data.len() {
        return Err(CffError::TooShort);
    }
    let count = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    if count == 0 {
        return Ok((Vec::new(), pos + 2));
    }
    if pos + 3 > data.len() {
        return Err(CffError::TooShort);
    }
    let off_size = data[pos + 2] as usize;
    if off_size == 0 || off_size > 4 {
        return Err(CffError::InvalidData("INDEX offSize out of range"));
    }

    let offset_array_start = pos + 3;
    let offset_array_len = (count + 1) * off_size;
    if offset_array_start + offset_array_len > data.len() {
        return Err(CffError::TooShort);
    }

    let data_start = offset_array_start + offset_array_len;
    let mut entries = Vec::with_capacity(count);

    for i in 0..count {
        let start = read_offset(data, offset_array_start + i * off_size, off_size)?;
        let end = read_offset(data, offset_array_start + (i + 1) * off_size, off_size)?;
        let abs_start = data_start + start - 1;
        let abs_end = data_start + end - 1;
        if abs_end > data.len() {
            return Err(CffError::TooShort);
        }
        entries.push(data[abs_start..abs_end].to_vec());
    }

    let last_offset = read_offset(data, offset_array_start + count * off_size, off_size)?;
    let end_pos = data_start + last_offset - 1;

    Ok((entries, end_pos))
}

/// Read an offset of `size` bytes (big-endian, 1-based).
fn read_offset(data: &[u8], pos: usize, size: usize) -> Result<usize, CffError> {
    if pos + size > data.len() {
        return Err(CffError::TooShort);
    }
    let mut val = 0usize;
    for i in 0..size {
        val = (val << 8) | data[pos + i] as usize;
    }
    Ok(val)
}

// ---------------------------------------------------------------------------
// DICT parsing
// ---------------------------------------------------------------------------

/// A DICT entry: operator → list of operands.
type DictEntries = HashMap<u16, Vec<f64>>;

/// Parse a CFF DICT (sequence of operands followed by operators).
fn parse_dict(data: &[u8]) -> Result<DictEntries, CffError> {
    let mut entries = HashMap::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0;

    while i < data.len() {
        let b0 = data[i];
        match b0 {
            // Operators
            0..=21 => {
                let op = if b0 == 12 {
                    i += 1;
                    if i >= data.len() {
                        return Err(CffError::TooShort);
                    }
                    (12u16 << 8) | data[i] as u16
                } else {
                    b0 as u16
                };
                entries.insert(op, operands.clone());
                operands.clear();
                i += 1;
            }
            // Integer: 1 byte
            32..=246 => {
                operands.push((b0 as i32 - 139) as f64);
                i += 1;
            }
            // Integer: 2 bytes
            247..=250 => {
                if i + 1 >= data.len() {
                    return Err(CffError::TooShort);
                }
                let b1 = data[i + 1];
                let val = ((b0 as i32 - 247) * 256 + b1 as i32 + 108) as f64;
                operands.push(val);
                i += 2;
            }
            251..=254 => {
                if i + 1 >= data.len() {
                    return Err(CffError::TooShort);
                }
                let b1 = data[i + 1];
                let val = (-(b0 as i32 - 251) * 256 - b1 as i32 - 108) as f64;
                operands.push(val);
                i += 2;
            }
            // 4-byte integer
            28 => {
                if i + 2 >= data.len() {
                    return Err(CffError::TooShort);
                }
                let val = i16::from_be_bytes([data[i + 1], data[i + 2]]) as f64;
                operands.push(val);
                i += 3;
            }
            29 => {
                if i + 4 >= data.len() {
                    return Err(CffError::TooShort);
                }
                let val =
                    i32::from_be_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]) as f64;
                operands.push(val);
                i += 5;
            }
            // Real number (BCD encoded)
            30 => {
                let (val, consumed) = parse_real_number(data, i + 1)?;
                operands.push(val);
                i = consumed;
            }
            _ => {
                i += 1;
            }
        }
    }

    Ok(entries)
}

/// Parse a BCD-encoded real number in a CFF DICT.
fn parse_real_number(data: &[u8], start: usize) -> Result<(f64, usize), CffError> {
    let mut s = String::new();
    let mut pos = start;

    loop {
        if pos >= data.len() {
            return Err(CffError::TooShort);
        }
        let byte = data[pos];
        pos += 1;

        for nibble in [byte >> 4, byte & 0x0F] {
            match nibble {
                0..=9 => s.push((b'0' + nibble) as char),
                0xA => s.push('.'),
                0xB => s.push('E'),
                0xC => s.push_str("E-"),
                0xD => {} // reserved
                0xE => s.push('-'),
                0xF => {
                    let val = s.parse::<f64>().unwrap_or(0.0);
                    return Ok((val, pos));
                }
                _ => {}
            }
        }
    }
}

/// Get an integer value from a DICT entry.
fn dict_get_int(dict: &DictEntries, op: u16) -> Option<i64> {
    dict.get(&op).and_then(|ops| ops.first()).map(|v| *v as i64)
}

/// Get a real value from a DICT entry.
fn dict_get_real(dict: &DictEntries, op: u16) -> Option<f64> {
    dict.get(&op).and_then(|ops| ops.first()).copied()
}

/// Get two integer operands from a DICT entry (e.g., Private [size offset]).
fn dict_get_two_ints(dict: &DictEntries, op: u16) -> Option<(i64, i64)> {
    dict.get(&op).and_then(|ops| {
        if ops.len() >= 2 {
            Some((ops[0] as i64, ops[1] as i64))
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Charset parsing
// ---------------------------------------------------------------------------

/// Parse the charset structure.
fn parse_charset(data: &[u8], offset: usize, num_glyphs: u16) -> Result<CffCharset, CffError> {
    match offset {
        0 => return Ok(CffCharset::ISOAdobe),
        1 => return Ok(CffCharset::Expert),
        2 => return Ok(CffCharset::ExpertSubset),
        _ => {}
    }

    if offset >= data.len() {
        return Err(CffError::TooShort);
    }

    let format = data[offset];
    let mut sids = Vec::with_capacity(num_glyphs as usize);
    sids.push(0); // .notdef is always GID 0

    let mut pos = offset + 1;

    match format {
        0 => {
            // Format 0: array of SIDs
            for _ in 1..num_glyphs {
                if pos + 2 > data.len() {
                    return Err(CffError::TooShort);
                }
                let sid = u16::from_be_bytes([data[pos], data[pos + 1]]);
                sids.push(sid);
                pos += 2;
            }
        }
        1 => {
            // Format 1: ranges with 1-byte count
            while sids.len() < num_glyphs as usize {
                if pos + 3 > data.len() {
                    return Err(CffError::TooShort);
                }
                let first = u16::from_be_bytes([data[pos], data[pos + 1]]);
                let n_left = data[pos + 2] as u16;
                pos += 3;
                for j in 0..=n_left {
                    sids.push(first + j);
                    if sids.len() >= num_glyphs as usize {
                        break;
                    }
                }
            }
        }
        2 => {
            // Format 2: ranges with 2-byte count
            while sids.len() < num_glyphs as usize {
                if pos + 4 > data.len() {
                    return Err(CffError::TooShort);
                }
                let first = u16::from_be_bytes([data[pos], data[pos + 1]]);
                let n_left = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                pos += 4;
                for j in 0..=n_left {
                    sids.push(first + j);
                    if sids.len() >= num_glyphs as usize {
                        break;
                    }
                }
            }
        }
        _ => return Err(CffError::InvalidData("unknown charset format")),
    }

    Ok(CffCharset::Custom(sids))
}

// ---------------------------------------------------------------------------
// Charstring width extraction
// ---------------------------------------------------------------------------

/// Extract the advance width from a Type 2 charstring.
///
/// Per the Type 2 spec, if the first value before the first operator is a
/// number, it encodes `width = value + nominalWidthX`. If no leading number
/// precedes the first operator, the width is `defaultWidthX`.
fn extract_charstring_width(cs: &[u8], nominal_width: f64, _default_width: f64) -> Option<f64> {
    let mut stack: Vec<f64> = Vec::new();
    let mut i = 0;

    while i < cs.len() {
        let b0 = cs[i];
        match b0 {
            // Operators (0-31 except 28)
            0..=27 | 29..=31 => {
                // First operator encountered — check if stack has an odd
                // number of args (width is the first one)
                let has_width = match b0 {
                    1 | 3 | 18 | 23 => !stack.len().is_multiple_of(2), // hstem, vstem, hstemhm, vstemhm
                    4 | 22 => stack.len() > 1,                         // vmoveto, hmoveto
                    21 => stack.len() > 2,                             // rmoveto
                    14 => !stack.is_empty(),                           // endchar
                    _ => false,
                };

                if has_width && !stack.is_empty() {
                    return Some(stack[0] + nominal_width);
                }
                return None;
            }
            // 2-byte integer
            28 => {
                if i + 2 >= cs.len() {
                    return None;
                }
                let val = i16::from_be_bytes([cs[i + 1], cs[i + 2]]) as f64;
                stack.push(val);
                i += 3;
            }
            // 4-byte fixed-point (16.16)
            255 => {
                if i + 4 >= cs.len() {
                    return None;
                }
                let val = i32::from_be_bytes([cs[i + 1], cs[i + 2], cs[i + 3], cs[i + 4]]);
                stack.push(val as f64 / 65536.0);
                i += 5;
            }
            // 1-byte integers
            32..=246 => {
                stack.push((b0 as i32 - 139) as f64);
                i += 1;
            }
            247..=250 => {
                if i + 1 >= cs.len() {
                    return None;
                }
                let b1 = cs[i + 1];
                stack.push(((b0 as i32 - 247) * 256 + b1 as i32 + 108) as f64);
                i += 2;
            }
            251..=254 => {
                if i + 1 >= cs.len() {
                    return None;
                }
                let b1 = cs[i + 1];
                stack.push((-(b0 as i32 - 251) * 256 - b1 as i32 - 108) as f64);
                i += 2;
            }
        }
    }

    // If we reach end without an operator, use default
    if !stack.is_empty() {
        Some(stack[0] + nominal_width)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dict_integers() {
        // Encode: operand 100 (32+100+139-139 = 100 → byte 239), operator 20
        let data = vec![239u8, 20];
        let dict = parse_dict(&data).unwrap();
        assert_eq!(dict_get_int(&dict, 20), Some(100));
    }

    #[test]
    fn test_parse_dict_negative() {
        // Encode: operand -100 → 251 + (100-108)/256... use 28 (short int) instead
        // 28, 0xFF, 0x9C = -100 as i16
        let data = vec![28, 0xFF, 0x9C, 20];
        let dict = parse_dict(&data).unwrap();
        assert_eq!(dict_get_int(&dict, 20), Some(-100));
    }

    #[test]
    fn test_skip_empty_index() {
        // count=0
        let data = vec![0, 0];
        let pos = skip_index(&data, 0).unwrap();
        assert_eq!(pos, 2);
    }

    #[test]
    fn test_read_index_single_entry() {
        // count=1, offSize=1, offsets=[1, 4], data=[0xAA, 0xBB, 0xCC]
        let data = vec![0, 1, 1, 1, 4, 0xAA, 0xBB, 0xCC];
        let (entries, end) = read_index(&data, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(end, 8);
    }

    #[test]
    fn test_too_short_returns_error() {
        let data = vec![0x01];
        assert!(matches!(parse_cff(&data), Err(CffError::TooShort)));
    }

    #[test]
    fn test_extract_width_with_rmoveto() {
        // Stack: [200, 10, 20] then rmoveto (21) → width = 200 + nominal
        // Encode 200: 32+200-139 = overflow, use 247 range: (200-108)/256=0 rem 92 → 247, 92
        // Actually: 200 → 247 + (200-108)/256 ... let's use 28-based encoding
        // 28, 0x00, 0xC8 = 200; 28, 0x00, 0x0A = 10; 28, 0x00, 0x14 = 20; 21 = rmoveto
        let cs = vec![28, 0x00, 0xC8, 28, 0x00, 0x0A, 28, 0x00, 0x14, 21];
        let w = extract_charstring_width(&cs, 0.0, 1000.0);
        assert_eq!(w, Some(200.0));
    }

    #[test]
    fn test_extract_width_no_width_arg() {
        // Stack: [10, 20] then rmoveto (21) → exactly 2 args, no width
        let cs = vec![28, 0x00, 0x0A, 28, 0x00, 0x14, 21];
        let w = extract_charstring_width(&cs, 0.0, 1000.0);
        assert_eq!(w, None);
    }

    #[test]
    fn test_charset_predefined() {
        let data = vec![0u8; 10];
        let charset = parse_charset(&data, 0, 5).unwrap();
        assert!(matches!(charset, CffCharset::ISOAdobe));
    }

    #[test]
    fn test_charset_format0() {
        // format=0, then SIDs: 1, 2, 3 (for 4 glyphs including .notdef)
        let mut data = vec![0u8; 100];
        data[50] = 0; // format 0
        data[51] = 0;
        data[52] = 1; // SID 1
        data[53] = 0;
        data[54] = 2; // SID 2
        data[55] = 0;
        data[56] = 3; // SID 3
        let charset = parse_charset(&data, 50, 4).unwrap();
        match charset {
            CffCharset::Custom(sids) => {
                assert_eq!(sids, vec![0, 1, 2, 3]);
            }
            _ => panic!("expected Custom charset"),
        }
    }
}
