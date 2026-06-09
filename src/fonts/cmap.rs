//! ToUnicode CMap and CID CMap parsing.
//!
//! Parses CMap streams embedded in PDF fonts to map character codes to Unicode
//! strings. Supports `beginbfchar`/`beginbfrange` sections (ToUnicode CMaps)
//! and predefined CID CMap references.

use std::collections::HashMap;

/// A parsed CMap that maps character codes to Unicode strings.
#[derive(Debug, Clone)]
pub struct CMap {
    /// All char code → Unicode string mappings (ranges expanded for O(1) lookup).
    char_map: HashMap<u32, String>,
    /// Code space ranges defining valid input byte lengths.
    code_spaces: Vec<CodeSpaceRange>,
}

/// A code space range defining valid byte lengths for input codes.
#[derive(Debug, Clone)]
struct CodeSpaceRange {
    low: u32,
    high: u32,
    num_bytes: u8,
}

impl CMap {
    /// Create an empty CMap.
    pub fn new() -> Self {
        CMap {
            char_map: HashMap::new(),
            code_spaces: Vec::new(),
        }
    }

    /// Parse a ToUnicode CMap from its stream bytes.
    pub fn parse(data: &[u8]) -> Result<Self, CMapError> {
        let text = std::str::from_utf8(data).map_err(|_| CMapError::InvalidUtf8)?;
        let mut cmap = CMap::new();
        cmap.parse_text(text)?;
        Ok(cmap)
    }

    /// Look up a character code, returning the Unicode string it maps to.
    pub fn lookup(&self, code: u32) -> Option<&str> {
        self.char_map.get(&code).map(|s| s.as_str())
    }

    /// Build the inverse mapping `Unicode string → code`.
    ///
    /// Inverts the `code → Unicode` table so edited text can be re-encoded back
    /// into the font's own codes (used by the text editor to render typed text
    /// with the embedded composite font). When several codes map to the same
    /// Unicode the **smallest** code is kept, for deterministic output.
    pub fn unicode_to_code(&self) -> HashMap<String, u32> {
        let mut out: HashMap<String, u32> = HashMap::with_capacity(self.char_map.len());
        for (&code, uni) in &self.char_map {
            out.entry(uni.clone())
                .and_modify(|c| *c = (*c).min(code))
                .or_insert(code);
        }
        out
    }

    /// Get the number of bytes for a given code based on code space ranges.
    pub fn code_length(&self, first_byte: u8) -> u8 {
        for cs in &self.code_spaces {
            let low_first = (cs.low >> ((cs.num_bytes - 1) * 8)) as u8;
            let high_first = (cs.high >> ((cs.num_bytes - 1) * 8)) as u8;
            if first_byte >= low_first && first_byte <= high_first {
                return cs.num_bytes;
            }
        }
        1
    }

    /// Parse the text content of a CMap.
    fn parse_text(&mut self, text: &str) -> Result<(), CMapError> {
        let mut pos = 0;
        let bytes = text.as_bytes();

        // Process sections in document order so bfrange before bfchar works correctly.
        loop {
            let cs_pos = find_keyword(bytes, pos, b"begincodespacerange");
            let bc_pos = find_keyword(bytes, pos, b"beginbfchar");
            let br_pos = find_keyword(bytes, pos, b"beginbfrange");

            let next = [
                cs_pos.map(|p| (p, 0u8)),
                bc_pos.map(|p| (p, 1u8)),
                br_pos.map(|p| (p, 2u8)),
            ]
            .into_iter()
            .flatten()
            .min_by_key(|(p, _)| *p);

            match next {
                Some((idx, 0)) => {
                    pos = idx + b"begincodespacerange".len();
                    pos = self.parse_code_space_ranges(bytes, pos)?;
                }
                Some((idx, 1)) => {
                    pos = idx + b"beginbfchar".len();
                    pos = self.parse_bf_chars(bytes, pos)?;
                }
                Some((idx, 2)) => {
                    pos = idx + b"beginbfrange".len();
                    pos = self.parse_bf_ranges(bytes, pos)?;
                }
                _ => break,
            }
        }

        Ok(())
    }

    /// Parse codespacerange section.
    fn parse_code_space_ranges(&mut self, data: &[u8], start: usize) -> Result<usize, CMapError> {
        let mut pos = start;
        loop {
            pos = skip_whitespace(data, pos);
            if pos >= data.len() {
                break;
            }
            if starts_with(data, pos, b"endcodespacerange") {
                return Ok(pos + b"endcodespacerange".len());
            }
            let (low, new_pos) = parse_hex_string(data, pos)?;
            pos = new_pos;
            let (high, new_pos) = parse_hex_string(data, pos)?;
            pos = new_pos;

            let num_bytes = (low.len() as u8).max(1);
            let low_val = bytes_to_u32(&low);
            let high_val = bytes_to_u32(&high);

            self.code_spaces.push(CodeSpaceRange {
                low: low_val,
                high: high_val,
                num_bytes,
            });
        }
        Ok(pos)
    }

    /// Parse beginbfchar section.
    fn parse_bf_chars(&mut self, data: &[u8], start: usize) -> Result<usize, CMapError> {
        let mut pos = start;
        loop {
            pos = skip_whitespace(data, pos);
            if pos >= data.len() {
                break;
            }
            if starts_with(data, pos, b"endbfchar") {
                return Ok(pos + b"endbfchar".len());
            }
            let (src_bytes, new_pos) = parse_hex_string(data, pos)?;
            pos = new_pos;
            let (dst_bytes, new_pos) = parse_hex_string(data, pos)?;
            pos = new_pos;

            let code = bytes_to_u32(&src_bytes);
            // <xxxx> <> means "no Unicode mapping" — skip insertion so lookup returns None.
            if let Some(unicode) = hex_bytes_to_unicode(&dst_bytes) {
                self.char_map.insert(code, unicode);
            }
        }
        Ok(pos)
    }

    /// Parse beginbfrange section.
    fn parse_bf_ranges(&mut self, data: &[u8], start: usize) -> Result<usize, CMapError> {
        let mut pos = start;
        loop {
            pos = skip_whitespace(data, pos);
            if pos >= data.len() {
                break;
            }
            if starts_with(data, pos, b"endbfrange") {
                return Ok(pos + b"endbfrange".len());
            }

            let (start_bytes, new_pos) = parse_hex_string(data, pos)?;
            pos = new_pos;
            let (end_bytes, new_pos) = parse_hex_string(data, pos)?;
            pos = new_pos;

            let range_start = bytes_to_u32(&start_bytes);
            let range_end = bytes_to_u32(&end_bytes);

            pos = skip_whitespace(data, pos);

            if pos < data.len() && data[pos] == b'[' {
                // Array of destination strings
                pos += 1;
                let mut arr: Vec<Option<String>> = Vec::new();
                loop {
                    pos = skip_whitespace(data, pos);
                    if pos >= data.len() || data[pos] == b']' {
                        pos += 1;
                        break;
                    }
                    let (dst_bytes, new_pos) = parse_hex_string(data, pos)?;
                    pos = new_pos;
                    arr.push(hex_bytes_to_unicode(&dst_bytes));
                }
                // Expand into char_map for O(1) lookup; skip <> (None) entries.
                for (i, opt_s) in arr.iter().enumerate() {
                    let code = range_start + i as u32;
                    if code > range_end {
                        break;
                    }
                    if let Some(s) = opt_s {
                        self.char_map.insert(code, s.clone());
                    }
                }
            } else {
                // Single base destination
                let (dst_bytes, new_pos) = parse_hex_string(data, pos)?;
                pos = new_pos;
                let base = bytes_to_u32(&dst_bytes);
                // Expand range into char_map for O(1) lookup
                for offset in 0..=(range_end - range_start) {
                    let cp = base + offset;
                    if let Some(ch) = char::from_u32(cp) {
                        self.char_map.insert(range_start + offset, ch.to_string());
                    }
                }
            }
        }
        Ok(pos)
    }
}

impl Default for CMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors that can occur during CMap parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CMapError {
    /// The CMap data is not valid UTF-8.
    InvalidUtf8,
    /// A hex string is malformed.
    InvalidHexString,
    /// Unexpected end of data.
    UnexpectedEnd,
}

impl std::fmt::Display for CMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CMapError::InvalidUtf8 => write!(f, "CMap data is not valid UTF-8"),
            CMapError::InvalidHexString => write!(f, "malformed hex string in CMap"),
            CMapError::UnexpectedEnd => write!(f, "unexpected end of CMap data"),
        }
    }
}

impl std::error::Error for CMapError {}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Find a keyword in the data starting from `pos`.
fn find_keyword(data: &[u8], pos: usize, keyword: &[u8]) -> Option<usize> {
    if pos >= data.len() {
        return None;
    }
    data[pos..]
        .windows(keyword.len())
        .position(|w| w == keyword)
        .map(|i| i + pos)
}

/// Check if data at `pos` starts with the given prefix.
fn starts_with(data: &[u8], pos: usize, prefix: &[u8]) -> bool {
    data[pos..].starts_with(prefix)
}

/// Skip whitespace characters.
fn skip_whitespace(data: &[u8], mut pos: usize) -> usize {
    while pos < data.len()
        && (data[pos] == b' ' || data[pos] == b'\n' || data[pos] == b'\r' || data[pos] == b'\t')
    {
        pos += 1;
    }
    pos
}

/// Parse a hex string like `<0041>` and return raw bytes.
fn parse_hex_string(data: &[u8], pos: usize) -> Result<(Vec<u8>, usize), CMapError> {
    let pos = skip_whitespace(data, pos);
    if pos >= data.len() || data[pos] != b'<' {
        return Err(CMapError::InvalidHexString);
    }
    let start = pos + 1;
    let mut end = start;
    while end < data.len() && data[end] != b'>' {
        end += 1;
    }
    if end >= data.len() {
        return Err(CMapError::UnexpectedEnd);
    }

    let hex_str = &data[start..end];
    let mut bytes = Vec::with_capacity(hex_str.len() / 2);
    // Skip whitespace within hex string
    let filtered: Vec<u8> = hex_str
        .iter()
        .copied()
        .filter(|&b| !matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        .collect();

    let mut i = 0;
    while i < filtered.len() {
        let hi = hex_digit(filtered[i]).ok_or(CMapError::InvalidHexString)?;
        let lo = if i + 1 < filtered.len() {
            hex_digit(filtered[i + 1]).ok_or(CMapError::InvalidHexString)?
        } else {
            0
        };
        bytes.push((hi << 4) | lo);
        i += 2;
    }

    Ok((bytes, end + 1))
}

/// Convert a hex ASCII digit to its value.
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Convert raw bytes to a u32 (big-endian).
fn bytes_to_u32(bytes: &[u8]) -> u32 {
    let mut val: u32 = 0;
    for &b in bytes {
        val = (val << 8) | (b as u32);
    }
    val
}

/// Convert hex bytes (big-endian UTF-16BE) to a Unicode string.
///
/// Returns `None` for an empty byte slice (`<>` in CMap syntax), which signals
/// "no Unicode mapping" for the corresponding CID.
fn hex_bytes_to_unicode(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    if !bytes.len().is_multiple_of(2) {
        // Odd number of bytes — treat as single byte codes
        return Some(
            bytes
                .iter()
                .filter_map(|&b| char::from_u32(b as u32))
                .collect(),
        );
    }

    // Interpret as UTF-16BE
    let mut result = String::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let unit = ((bytes[i] as u16) << 8) | (bytes[i + 1] as u16);
        if (0xD800..=0xDBFF).contains(&unit) {
            // High surrogate — look for low surrogate
            if i + 3 < bytes.len() {
                let low = ((bytes[i + 2] as u16) << 8) | (bytes[i + 3] as u16);
                if (0xDC00..=0xDFFF).contains(&low) {
                    let cp = 0x10000 + ((unit as u32 - 0xD800) << 10) + (low as u32 - 0xDC00);
                    if let Some(ch) = char::from_u32(cp) {
                        result.push(ch);
                    }
                    i += 4;
                    continue;
                }
            }
            result.push(char::REPLACEMENT_CHARACTER);
            i += 2;
        } else {
            if let Some(ch) = char::from_u32(unit as u32) {
                result.push(ch);
            } else {
                result.push(char::REPLACEMENT_CHARACTER);
            }
            i += 2;
        }
    }
    Some(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bfchar() {
        let cmap_data = b"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
2 beginbfchar
<01> <0041>
<02> <0042>
endbfchar
endcmap
";
        let cmap = CMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.lookup(0x01), Some("A"));
        assert_eq!(cmap.lookup(0x02), Some("B"));
        assert_eq!(cmap.lookup(0x03), None);
    }

    #[test]
    fn test_unicode_to_code_inverts_mapping() {
        let cmap_data = b"1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0003> <0041>
<0004> <0042>
endbfchar
";
        let cmap = CMap::parse(cmap_data).unwrap();
        let rev = cmap.unicode_to_code();
        assert_eq!(rev.get("A").copied(), Some(0x0003));
        assert_eq!(rev.get("B").copied(), Some(0x0004));
        assert_eq!(rev.get("Z").copied(), None);
    }

    #[test]
    fn test_unicode_to_code_keeps_smallest_on_collision() {
        // Two codes map to "A" — the inverse must keep the smaller code.
        let cmap_data = b"1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0007> <0041>
<0002> <0041>
endbfchar
";
        let cmap = CMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.unicode_to_code().get("A").copied(), Some(0x0002));
    }

    #[test]
    fn test_parse_bfrange_base() {
        let cmap_data = b"1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfrange
<20> <25> <0041>
endbfrange
";
        let cmap = CMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.lookup(0x20), Some("A"));
        assert_eq!(cmap.lookup(0x21), Some("B"));
        assert_eq!(cmap.lookup(0x22), Some("C"));
        assert_eq!(cmap.lookup(0x25), Some("F"));
        assert_eq!(cmap.lookup(0x26), None);
    }

    #[test]
    fn test_parse_bfrange_array() {
        let cmap_data = b"1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfrange
<10> <12> [<0058> <0059> <005A>]
endbfrange
";
        let cmap = CMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.lookup(0x10), Some("X"));
        assert_eq!(cmap.lookup(0x11), Some("Y"));
        assert_eq!(cmap.lookup(0x12), Some("Z"));
    }

    #[test]
    fn test_parse_multibyte_codespace() {
        let cmap_data = b"1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfchar
<0048> <0048>
endbfchar
";
        let cmap = CMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.lookup(0x0048), Some("H"));
        assert_eq!(cmap.code_length(0x00), 2);
    }

    #[test]
    fn test_parse_multi_char_unicode() {
        // Maps a single code to a multi-character Unicode string (ligature expansion)
        let cmap_data = b"1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<FB> <00660069>
endbfchar
";
        let cmap = CMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.lookup(0xFB), Some("fi"));
    }

    #[test]
    fn test_empty_cmap() {
        let cmap = CMap::new();
        assert_eq!(cmap.lookup(0x41), None);
    }

    #[test]
    fn test_invalid_hex_string() {
        let cmap_data = b"1 beginbfchar
<GG> <0041>
endbfchar
";
        let result = CMap::parse(cmap_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_hex_bytes_to_unicode_bmp() {
        assert_eq!(hex_bytes_to_unicode(&[0x00, 0x41]), Some("A".to_string()));
        assert_eq!(
            hex_bytes_to_unicode(&[0x00, 0x48, 0x00, 0x69]),
            Some("Hi".to_string())
        );
    }

    #[test]
    fn test_hex_bytes_to_unicode_empty_returns_none() {
        assert_eq!(hex_bytes_to_unicode(&[]), None);
    }

    #[test]
    fn test_hex_bytes_to_unicode_surrogate() {
        // U+1F600 (😀) = D83D DE00 in UTF-16
        let bytes = vec![0xD8, 0x3D, 0xDE, 0x00];
        let result = hex_bytes_to_unicode(&bytes);
        assert_eq!(result, Some("\u{1F600}".to_string()));
    }

    #[test]
    fn test_empty_unicode_mapping_not_inserted() {
        // <xxxx> <> means no Unicode mapping — lookup should return None, not Some("")
        let cmap_data = b"1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0041> <>
<0042> <0042>
endbfchar
";
        let cmap = CMap::parse(cmap_data).unwrap();
        assert_eq!(
            cmap.lookup(0x0041),
            None,
            "empty mapping should not be inserted"
        );
        assert_eq!(cmap.lookup(0x0042), Some("B"));
    }

    #[test]
    fn test_bfrange_before_bfchar_parsed_correctly() {
        // bfrange appears before bfchar in document order
        let cmap_data = b"1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfrange
<0050> <0052> <0058>
endbfrange
1 beginbfchar
<0041> <0041>
endbfchar
";
        let cmap = CMap::parse(cmap_data).unwrap();
        // bfrange: 0x50→'X', 0x51→'Y', 0x52→'Z'
        assert_eq!(
            cmap.lookup(0x0050),
            Some("X"),
            "bfrange entry should be parsed"
        );
        assert_eq!(cmap.lookup(0x0051), Some("Y"));
        assert_eq!(cmap.lookup(0x0052), Some("Z"));
        // bfchar: 0x41→'A'
        assert_eq!(
            cmap.lookup(0x0041),
            Some("A"),
            "bfchar entry should be parsed"
        );
    }

    #[test]
    fn test_bytes_to_u32() {
        assert_eq!(bytes_to_u32(&[0x00, 0x41]), 0x0041);
        assert_eq!(bytes_to_u32(&[0xFF]), 0xFF);
        assert_eq!(bytes_to_u32(&[0x01, 0x00]), 0x0100);
    }
}
