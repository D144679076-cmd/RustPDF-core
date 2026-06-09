//! TrueType/OpenType font table parsing.
//!
//! Parses the essential tables from TrueType fonts embedded in PDFs:
//! - `cmap`: character code to glyph ID mapping
//! - `head`: font header (units per em)
//! - `hhea`: horizontal header (number of hmetrics)
//! - `hmtx`: horizontal metrics (glyph advance widths)
//! - `maxp`: maximum profile (number of glyphs)

use std::collections::HashMap;

/// A parsed TrueType font providing glyph metrics and character mapping.
#[derive(Debug, Clone)]
pub struct TrueTypeFont {
    /// Units per em from the `head` table.
    pub units_per_em: u16,
    /// Number of glyphs from `maxp`.
    pub num_glyphs: u16,
    /// Advance widths for each glyph (in font units).
    advance_widths: Vec<u16>,
    /// Character code → glyph ID mapping from `cmap`.
    cmap: HashMap<u32, u16>,
    /// Bit 0 of `head.macStyle` — set when the face is intrinsically bold.
    pub mac_style_bold: bool,
    /// Bit 1 of `head.macStyle` — set when the face is intrinsically italic.
    pub mac_style_italic: bool,
    /// `OS/2.usWeightClass` — `None` when the `OS/2` table is absent (optional).
    pub os2_weight_class: Option<u16>,
}

/// Errors during TrueType parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtfError {
    /// Data is too short to contain the expected structure.
    TooShort,
    /// Required table not found.
    MissingTable(&'static str),
    /// Invalid table data.
    InvalidData(&'static str),
}

impl std::fmt::Display for TtfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TtfError::TooShort => write!(f, "font data too short"),
            TtfError::MissingTable(name) => write!(f, "missing required table: {name}"),
            TtfError::InvalidData(msg) => write!(f, "invalid font data: {msg}"),
        }
    }
}

impl std::error::Error for TtfError {}

/// A TrueType table directory entry.
#[derive(Debug, Clone, Copy)]
struct TableRecord {
    offset: u32,
    #[allow(dead_code)]
    length: u32,
}

impl TrueTypeFont {
    /// Parse a TrueType font from raw font file bytes.
    pub fn parse(data: &[u8]) -> Result<Self, TtfError> {
        if data.len() < 12 {
            return Err(TtfError::TooShort);
        }

        let num_tables = read_u16(data, 4)?;
        let tables = parse_table_directory(data, num_tables)?;

        let head = tables.get(b"head").ok_or(TtfError::MissingTable("head"))?;
        let hhea = tables.get(b"hhea").ok_or(TtfError::MissingTable("hhea"))?;
        let hmtx = tables.get(b"hmtx").ok_or(TtfError::MissingTable("hmtx"))?;
        let maxp = tables.get(b"maxp").ok_or(TtfError::MissingTable("maxp"))?;

        let units_per_em = parse_head(data, head)?;
        let num_glyphs = parse_maxp(data, maxp)?;
        let num_h_metrics = parse_hhea(data, hhea)?;
        let advance_widths = parse_hmtx(data, hmtx, num_h_metrics, num_glyphs)?;

        let cmap = if let Some(cmap_table) = tables.get(b"cmap") {
            parse_cmap(data, cmap_table)?
        } else {
            HashMap::new()
        };

        // `head.macStyle` bits: 0 = Bold, 1 = Italic.  Located at offset 44 within
        // the head table (after the two 4-byte timestamps at 20/28 and four 2-byte
        // fields at 36).  Fail softly: if the table is truncated, default to 0.
        let mac_style = parse_head_mac_style(data, head).unwrap_or(0);
        let mac_style_bold = (mac_style & 0x01) != 0;
        let mac_style_italic = (mac_style & 0x02) != 0;

        // `OS/2` table is optional.  `usWeightClass` is a u16 at offset 4 within
        // the table; values >= 600 indicate a bold weight (600 = SemiBold, 700 = Bold).
        let os2_weight_class = tables
            .get(b"OS/2")
            .and_then(|rec| parse_os2_weight_class(data, rec).ok());

        Ok(TrueTypeFont {
            units_per_em,
            num_glyphs,
            advance_widths,
            cmap,
            mac_style_bold,
            mac_style_italic,
            os2_weight_class,
        })
    }

    /// Get the glyph ID for a character code.
    pub fn glyph_id(&self, char_code: u32) -> Option<u16> {
        self.cmap.get(&char_code).copied()
    }

    /// Get the advance width for a glyph ID (in font units).
    pub fn glyph_advance(&self, glyph_id: u16) -> u16 {
        let idx = glyph_id as usize;
        if idx < self.advance_widths.len() {
            self.advance_widths[idx]
        } else if !self.advance_widths.is_empty() {
            // Last entry applies to all subsequent glyphs (monospaced tail)
            *self.advance_widths.last().unwrap_or(&0)
        } else {
            0
        }
    }

    /// Get the advance width for a character code in 1/1000 units of text space.
    pub fn char_width(&self, char_code: u32) -> f64 {
        if self.units_per_em == 0 {
            return 0.0;
        }
        let gid = self.glyph_id(char_code).unwrap_or(0);
        let advance = self.glyph_advance(gid) as f64;
        (advance * 1000.0) / self.units_per_em as f64
    }

    /// Iterate over every mapped character and its advance in 1/1000-em units.
    ///
    /// Useful for building a [`PdfFontMetrics`](crate::editor::PdfFontMetrics) from
    /// an embedded bold/italic TrueType face so that caret and width measurements
    /// use the variant's real glyph advances instead of the regular font's widths.
    pub fn iter_char_advances_1000(&self) -> impl Iterator<Item = (char, f64)> + '_ {
        let upm = self.units_per_em as f64;
        self.cmap.iter().filter_map(move |(&code, &gid)| {
            if upm == 0.0 {
                return None;
            }
            char::from_u32(code).map(|ch| {
                let adv = self.glyph_advance(gid) as f64 * 1000.0 / upm;
                (ch, adv)
            })
        })
    }

    /// Whether this face is intrinsically bold, using the best available signal:
    /// `OS/2.usWeightClass >= 600` (Bold/SemiBold) or `head.macStyle` bit 0.
    pub fn is_bold(&self) -> bool {
        self.os2_weight_class.is_some_and(|w| w >= 600) || self.mac_style_bold
    }

    /// Whether this face is intrinsically italic/oblique:
    /// `head.macStyle` bit 1.  (`OS/2.fsSelection` bit 0 would be more precise but
    /// requires parsing an additional field; macStyle bit 1 is sufficient here.)
    pub fn is_italic(&self) -> bool {
        self.mac_style_italic
    }
}

// ---------------------------------------------------------------------------
// Table directory parsing
// ---------------------------------------------------------------------------

/// Parse the table directory, returning a map of 4-byte tag → TableRecord.
fn parse_table_directory(
    data: &[u8],
    num_tables: u16,
) -> Result<HashMap<[u8; 4], TableRecord>, TtfError> {
    let mut tables = HashMap::with_capacity(num_tables as usize);
    for i in 0..num_tables as usize {
        let offset = 12 + i * 16;
        if offset + 16 > data.len() {
            return Err(TtfError::TooShort);
        }
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&data[offset..offset + 4]);
        let table_offset = read_u32(data, offset + 8)?;
        let table_length = read_u32(data, offset + 12)?;
        tables.insert(
            tag,
            TableRecord {
                offset: table_offset,
                length: table_length,
            },
        );
    }
    Ok(tables)
}

/// Parse `head` table — extract units_per_em.
fn parse_head(data: &[u8], record: &TableRecord) -> Result<u16, TtfError> {
    let off = record.offset as usize;
    if off + 54 > data.len() {
        return Err(TtfError::TooShort);
    }
    // unitsPerEm is at offset 18 within head table
    read_u16(data, off + 18)
}

/// Parse `head.macStyle` (u16 at offset 44 within the head table).
fn parse_head_mac_style(data: &[u8], record: &TableRecord) -> Result<u16, TtfError> {
    let off = record.offset as usize;
    // macStyle is at byte offset 44 within the head table.
    // The head table must be at least 46 bytes for this field to exist.
    if off + 46 > data.len() {
        return Err(TtfError::TooShort);
    }
    read_u16(data, off + 44)
}

/// Parse `OS/2.usWeightClass` (u16 at offset 4 within the OS/2 table).
fn parse_os2_weight_class(data: &[u8], record: &TableRecord) -> Result<u16, TtfError> {
    let off = record.offset as usize;
    // usWeightClass is the second u16 field in the OS/2 table (offset 4 = after version).
    if off + 6 > data.len() {
        return Err(TtfError::TooShort);
    }
    read_u16(data, off + 4)
}

/// Parse `maxp` table — extract numGlyphs.
fn parse_maxp(data: &[u8], record: &TableRecord) -> Result<u16, TtfError> {
    let off = record.offset as usize;
    if off + 6 > data.len() {
        return Err(TtfError::TooShort);
    }
    // numGlyphs is at offset 4 within maxp table
    read_u16(data, off + 4)
}

/// Parse `hhea` table — extract numberOfHMetrics.
fn parse_hhea(data: &[u8], record: &TableRecord) -> Result<u16, TtfError> {
    let off = record.offset as usize;
    if off + 36 > data.len() {
        return Err(TtfError::TooShort);
    }
    // numberOfHMetrics is at offset 34 within hhea table
    read_u16(data, off + 34)
}

/// Parse `hmtx` table — extract advance widths for all glyphs.
fn parse_hmtx(
    data: &[u8],
    record: &TableRecord,
    num_h_metrics: u16,
    num_glyphs: u16,
) -> Result<Vec<u16>, TtfError> {
    let off = record.offset as usize;
    let mut widths = Vec::with_capacity(num_glyphs as usize);

    // First num_h_metrics entries are (advanceWidth: u16, lsb: i16)
    for i in 0..num_h_metrics as usize {
        let entry_off = off + i * 4;
        if entry_off + 2 > data.len() {
            return Err(TtfError::TooShort);
        }
        widths.push(read_u16(data, entry_off)?);
    }

    // Remaining glyphs share the last advance width
    if num_h_metrics > 0 && num_glyphs > num_h_metrics {
        let last_width = widths[num_h_metrics as usize - 1];
        for _ in num_h_metrics..num_glyphs {
            widths.push(last_width);
        }
    }

    Ok(widths)
}

/// Parse `cmap` table — extract character code to glyph ID mapping.
/// Prefers format 4 (BMP) or format 12 (full Unicode).
fn parse_cmap(data: &[u8], record: &TableRecord) -> Result<HashMap<u32, u16>, TtfError> {
    let off = record.offset as usize;
    if off + 4 > data.len() {
        return Err(TtfError::TooShort);
    }

    let num_subtables = read_u16(data, off + 2)? as usize;
    let mut format12_off: Option<usize> = None;
    let mut format4_off: Option<usize> = None;

    for i in 0..num_subtables {
        let entry = off + 4 + i * 8;
        if entry + 8 > data.len() {
            break;
        }
        let platform_id = read_u16(data, entry)?;
        let encoding_id = read_u16(data, entry + 2)?;
        let subtable_offset = read_u32(data, entry + 4)? as usize + off;

        if subtable_offset + 2 > data.len() {
            continue;
        }
        let format = read_u16(data, subtable_offset)?;

        // Prefer Unicode platform (0) or Windows Unicode BMP (3,1) / full (3,10)
        match format {
            12 if platform_id == 0
                || (platform_id == 3 && encoding_id == 10)
                || format12_off.is_none() =>
            {
                format12_off = Some(subtable_offset);
            }
            4 if platform_id == 0
                || (platform_id == 3 && encoding_id == 1)
                || format4_off.is_none() =>
            {
                format4_off = Some(subtable_offset);
            }
            _ => {}
        }
    }

    // Prefer format 12 (full Unicode), fall back to format 4 (BMP)
    if let Some(off) = format12_off {
        parse_cmap_format12(data, off)
    } else if let Some(off) = format4_off {
        parse_cmap_format4(data, off)
    } else {
        Ok(HashMap::new())
    }
}

/// Parse cmap format 4 (segment mapping to delta values, BMP only).
fn parse_cmap_format4(data: &[u8], off: usize) -> Result<HashMap<u32, u16>, TtfError> {
    if off + 14 > data.len() {
        return Err(TtfError::TooShort);
    }

    let seg_count = read_u16(data, off + 6)? as usize / 2;
    let end_codes_off = off + 14;
    let start_codes_off = end_codes_off + seg_count * 2 + 2; // +2 for reservedPad
    let deltas_off = start_codes_off + seg_count * 2;
    let range_offsets_off = deltas_off + seg_count * 2;

    let mut map = HashMap::new();

    for seg in 0..seg_count {
        let end_code = read_u16(data, end_codes_off + seg * 2)? as u32;
        let start_code = read_u16(data, start_codes_off + seg * 2)? as u32;
        let delta = read_u16(data, deltas_off + seg * 2)? as i16;
        let range_offset_pos = range_offsets_off + seg * 2;

        if range_offset_pos + 2 > data.len() {
            break;
        }
        let range_offset = read_u16(data, range_offset_pos)?;

        if end_code == 0xFFFF && start_code == 0xFFFF {
            break;
        }

        for code in start_code..=end_code {
            let glyph_id = if range_offset == 0 {
                (code as i32 + delta as i32) as u16
            } else {
                let glyph_off =
                    range_offset_pos + range_offset as usize + (code - start_code) as usize * 2;
                if glyph_off + 2 > data.len() {
                    0
                } else {
                    let gid = read_u16(data, glyph_off).unwrap_or(0);
                    if gid != 0 {
                        (gid as i32 + delta as i32) as u16
                    } else {
                        0
                    }
                }
            };

            if glyph_id != 0 {
                map.insert(code, glyph_id);
            }
        }
    }

    Ok(map)
}

/// Parse cmap format 12 (segmented coverage, full 32-bit).
fn parse_cmap_format12(data: &[u8], off: usize) -> Result<HashMap<u32, u16>, TtfError> {
    if off + 16 > data.len() {
        return Err(TtfError::TooShort);
    }

    let num_groups = read_u32(data, off + 12)? as usize;
    let groups_off = off + 16;
    let mut map = HashMap::new();

    for i in 0..num_groups {
        let group_off = groups_off + i * 12;
        if group_off + 12 > data.len() {
            break;
        }
        let start_code = read_u32(data, group_off)?;
        let end_code = read_u32(data, group_off + 4)?;
        let start_glyph = read_u32(data, group_off + 8)?;

        for code in start_code..=end_code {
            let gid = start_glyph + (code - start_code);
            if gid <= u16::MAX as u32 {
                map.insert(code, gid as u16);
            }
        }
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// Binary reading helpers
// ---------------------------------------------------------------------------

fn read_u16(data: &[u8], offset: usize) -> Result<u16, TtfError> {
    if offset + 2 > data.len() {
        return Err(TtfError::TooShort);
    }
    Ok(u16::from_be_bytes([data[offset], data[offset + 1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, TtfError> {
    if offset + 4 > data.len() {
        return Err(TtfError::TooShort);
    }
    Ok(u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid TrueType font with the required tables.
    fn build_minimal_ttf() -> Vec<u8> {
        // We'll build a font with 4 tables: head, hhea, hmtx, maxp
        // (no cmap for simplicity in this test)
        let num_tables: u16 = 4;
        let mut data = Vec::new();

        // Offset table (12 bytes)
        data.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // sfVersion 1.0
        data.extend_from_slice(&num_tables.to_be_bytes());
        data.extend_from_slice(&[0x00, 0x20]); // searchRange
        data.extend_from_slice(&[0x00, 0x02]); // entrySelector
        data.extend_from_slice(&[0x00, 0x00]); // rangeShift

        // Table directory: 4 entries × 16 bytes = 64 bytes (starts at offset 12)
        // We'll place tables after the directory at offset 76
        let dir_end = 12 + 4 * 16; // 76

        // head table: 54 bytes minimum
        let head_off = dir_end as u32;
        let head_len = 54u32;
        // hhea table
        let hhea_off = head_off + head_len;
        let hhea_len = 36u32;
        // maxp table
        let maxp_off = hhea_off + hhea_len;
        let maxp_len = 6u32;
        // hmtx table: 2 glyphs × 4 bytes = 8 bytes
        let hmtx_off = maxp_off + maxp_len;
        let hmtx_len = 8u32;

        // Table directory entries
        // head
        data.extend_from_slice(b"head");
        data.extend_from_slice(&0u32.to_be_bytes()); // checksum
        data.extend_from_slice(&head_off.to_be_bytes());
        data.extend_from_slice(&head_len.to_be_bytes());
        // hhea
        data.extend_from_slice(b"hhea");
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&hhea_off.to_be_bytes());
        data.extend_from_slice(&hhea_len.to_be_bytes());
        // hmtx
        data.extend_from_slice(b"hmtx");
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&hmtx_off.to_be_bytes());
        data.extend_from_slice(&hmtx_len.to_be_bytes());
        // maxp
        data.extend_from_slice(b"maxp");
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&maxp_off.to_be_bytes());
        data.extend_from_slice(&maxp_len.to_be_bytes());

        assert_eq!(data.len(), dir_end);

        // head table (54 bytes) — unitsPerEm at offset 18
        let mut head = vec![0u8; 54];
        head[18] = 0x03; // unitsPerEm = 1000 (0x03E8)
        head[19] = 0xE8;
        data.extend_from_slice(&head);

        // hhea table (36 bytes) — numberOfHMetrics at offset 34
        let mut hhea = vec![0u8; 36];
        hhea[34] = 0x00; // numberOfHMetrics = 2
        hhea[35] = 0x02;
        data.extend_from_slice(&hhea);

        // maxp table (6 bytes) — numGlyphs at offset 4
        let mut maxp = vec![0u8; 6];
        maxp[4] = 0x00; // numGlyphs = 2
        maxp[5] = 0x02;
        data.extend_from_slice(&maxp);

        // hmtx table: 2 entries of (advanceWidth: u16, lsb: i16)
        data.extend_from_slice(&500u16.to_be_bytes()); // glyph 0: width 500
        data.extend_from_slice(&0i16.to_be_bytes()); // lsb
        data.extend_from_slice(&700u16.to_be_bytes()); // glyph 1: width 700
        data.extend_from_slice(&0i16.to_be_bytes()); // lsb

        data
    }

    #[test]
    fn test_parse_minimal_ttf() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::parse(&data).unwrap();
        assert_eq!(font.units_per_em, 1000);
        assert_eq!(font.num_glyphs, 2);
        assert_eq!(font.glyph_advance(0), 500);
        assert_eq!(font.glyph_advance(1), 700);
    }

    #[test]
    fn test_glyph_advance_out_of_range() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::parse(&data).unwrap();
        // Beyond num_glyphs — returns last width
        assert_eq!(font.glyph_advance(99), 700);
    }

    #[test]
    fn test_char_width_no_cmap() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::parse(&data).unwrap();
        // No cmap, so glyph_id returns None → glyph 0 → width 500
        // 500 * 1000 / 1000 = 500.0
        assert_eq!(font.char_width(0x41), 500.0);
    }

    #[test]
    fn test_too_short_data() {
        let result = TrueTypeFont::parse(&[0; 4]);
        assert!(matches!(result, Err(TtfError::TooShort)));
    }

    #[test]
    fn test_missing_table() {
        // Valid offset table but no table entries
        let mut data = vec![0x00, 0x01, 0x00, 0x00]; // sfVersion
        data.extend_from_slice(&0u16.to_be_bytes()); // 0 tables
        data.extend_from_slice(&[0; 6]); // padding
        let result = TrueTypeFont::parse(&data);
        assert!(matches!(result, Err(TtfError::MissingTable("head"))));
    }

    #[test]
    fn test_read_u16() {
        let data = [0x03, 0xE8, 0xFF];
        assert_eq!(read_u16(&data, 0).unwrap(), 1000);
        assert_eq!(read_u16(&data, 1).unwrap(), 0xE8FF);
        assert!(read_u16(&data, 2).is_err());
    }

    #[test]
    fn test_read_u32() {
        let data = [0x00, 0x01, 0x00, 0x00, 0xFF];
        assert_eq!(read_u32(&data, 0).unwrap(), 0x00010000);
        assert!(read_u32(&data, 3).is_err());
    }

    /// Build a minimal TTF where `head.macStyle` and an optional OS/2 weight can
    /// be controlled.  Used to test `is_bold()` / `is_italic()` detection.
    fn build_ttf_with_style(mac_style: u16, os2_weight: Option<u16>) -> Vec<u8> {
        let has_os2 = os2_weight.is_some();
        let num_tables: u16 = if has_os2 { 5 } else { 4 };
        let mut data = Vec::new();

        // Offset table (12 bytes)
        data.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        data.extend_from_slice(&num_tables.to_be_bytes());
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // searchRange…

        let dir_end = 12 + num_tables as usize * 16;

        let head_off = dir_end as u32;
        let head_len = 54u32;
        let hhea_off = head_off + head_len;
        let hhea_len = 36u32;
        let maxp_off = hhea_off + hhea_len;
        let maxp_len = 6u32;
        let hmtx_off = maxp_off + maxp_len;
        let hmtx_len = 4u32; // 1 glyph
        let os2_off = hmtx_off + hmtx_len;
        let os2_len = 6u32; // version(2) + usWeightClass(2) + usWidthClass(2)

        // Table directory — head, hhea, maxp, hmtx
        for &(tag, off, len) in &[
            (b"head", head_off, head_len),
            (b"hhea", hhea_off, hhea_len),
            (b"hmtx", hmtx_off, hmtx_len),
            (b"maxp", maxp_off, maxp_len),
        ] {
            data.extend_from_slice(tag);
            data.extend_from_slice(&0u32.to_be_bytes());
            data.extend_from_slice(&off.to_be_bytes());
            data.extend_from_slice(&len.to_be_bytes());
        }
        if has_os2 {
            data.extend_from_slice(b"OS/2");
            data.extend_from_slice(&0u32.to_be_bytes());
            data.extend_from_slice(&os2_off.to_be_bytes());
            data.extend_from_slice(&os2_len.to_be_bytes());
        }

        assert_eq!(data.len(), dir_end);

        // head (54 bytes): unitsPerEm at offset 18; macStyle at offset 44
        let mut head = vec![0u8; 54];
        head[18] = 0x03;
        head[19] = 0xE8; // unitsPerEm = 1000
        head[44] = (mac_style >> 8) as u8;
        head[45] = (mac_style & 0xFF) as u8;
        data.extend_from_slice(&head);

        // hhea (36 bytes): numberOfHMetrics = 1 at offset 34
        let mut hhea = vec![0u8; 36];
        hhea[35] = 0x01;
        data.extend_from_slice(&hhea);

        // maxp (6 bytes): numGlyphs = 1 at offset 4
        let mut maxp = vec![0u8; 6];
        maxp[5] = 0x01;
        data.extend_from_slice(&maxp);

        // hmtx (4 bytes): 1 glyph with advance 500
        data.extend_from_slice(&500u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());

        // OS/2 (6 bytes): version=0, usWeightClass at offset 4, usWidthClass=5
        if let Some(wc) = os2_weight {
            data.extend_from_slice(&0u16.to_be_bytes()); // version
            data.extend_from_slice(&0u16.to_be_bytes()); // achVendID padding (unused here)
            data.extend_from_slice(&wc.to_be_bytes()); // usWeightClass at offset 4
        }

        data
    }

    #[test]
    fn parse_reads_macstyle_bold() {
        // macStyle bit 0 = 1 → is_bold() true; no OS/2 table.
        let data = build_ttf_with_style(0x0001, None);
        let font = TrueTypeFont::parse(&data).unwrap();
        assert!(font.mac_style_bold, "macStyle bit 0 should mark bold");
        assert!(!font.mac_style_italic);
        assert!(font.is_bold());
        assert!(!font.is_italic());
        assert_eq!(font.os2_weight_class, None, "no OS/2 table → None");
    }

    #[test]
    fn parse_reads_macstyle_italic() {
        // macStyle bit 1 = 1 → is_italic() true.
        let data = build_ttf_with_style(0x0002, None);
        let font = TrueTypeFont::parse(&data).unwrap();
        assert!(!font.mac_style_bold);
        assert!(font.mac_style_italic, "macStyle bit 1 should mark italic");
        assert!(!font.is_bold());
        assert!(font.is_italic());
    }

    #[test]
    fn parse_os2_weight_class_bold() {
        // OS/2 usWeightClass = 700 (Bold) → is_bold() true even without macStyle.
        let data = build_ttf_with_style(0x0000, Some(700));
        let font = TrueTypeFont::parse(&data).unwrap();
        assert!(!font.mac_style_bold);
        assert_eq!(font.os2_weight_class, Some(700));
        assert!(font.is_bold(), "usWeightClass 700 should yield is_bold()");
    }

    #[test]
    fn parse_missing_os2_is_none() {
        // No OS/2 table → field is None; is_bold() reads macStyle only.
        let data = build_ttf_with_style(0x0000, None);
        let font = TrueTypeFont::parse(&data).unwrap();
        assert_eq!(font.os2_weight_class, None);
        assert!(!font.is_bold());
    }
}
