//! Font loading, caching, and Unicode resolution.
//!
//! Provides `FontCache` which manages parsed font data and implements the
//! 4-level Unicode resolution pipeline:
//! 1. ToUnicode CMap (highest priority)
//! 2. Encoding + AGL
//! 3. CID CMap (for CJK fonts)
//! 4. Raw character code as Unicode (last resort)

use std::collections::HashMap;

use super::cff::CffFont;
use super::cmap::CMap;
use super::encoding::{BaseEncoding, Encoding};
use super::standard::StandardFont;
use super::truetype::TrueTypeFont;
use super::types::{FontType, FontWidths};

/// A cached, parsed PDF font ready for text extraction and width queries.
#[derive(Debug, Clone)]
pub struct PdfFont {
    /// Font name from the PDF dictionary.
    pub name: String,
    /// Font subtype.
    pub font_type: FontType,
    /// ToUnicode CMap (if present in the font dictionary).
    pub to_unicode: Option<CMap>,
    /// Font encoding (for simple fonts).
    pub encoding: Option<Encoding>,
    /// Width information.
    pub widths: FontWidths,
    /// Standard font identity (if this is one of the 14 standard fonts).
    pub standard_font: Option<StandardFont>,
    /// Parsed TrueType font data (if embedded).
    pub truetype: Option<TrueTypeFont>,
    /// Parsed CFF font data (if embedded).
    pub cff: Option<CffFont>,
    /// Default width for missing glyphs (in 1/1000 units).
    pub default_width: f64,
}

impl PdfFont {
    /// Resolve a single character code to a Unicode string.
    ///
    /// Uses the 4-level resolution pipeline:
    /// 1. ToUnicode CMap
    /// 2. Encoding + AGL
    /// 3. TrueType cmap (char code → glyph → assume identity mapping)
    /// 4. Raw char code as Unicode
    pub fn resolve_char(&self, code: u32) -> String {
        // Level 1: ToUnicode CMap
        if let Some(ref cmap) = self.to_unicode {
            if let Some(s) = cmap.lookup(code) {
                return s.to_string();
            }
        }

        // Level 2: Encoding + AGL (simple fonts only)
        if let Some(ref enc) = self.encoding {
            if code <= 255 {
                if let Some(ch) = enc.decode_char(code as u8) {
                    return ch.to_string();
                }
            }
        }

        // Level 3: TrueType cmap (use char code directly as Unicode if mapped)
        if let Some(ref ttf) = self.truetype {
            if ttf.glyph_id(code).is_some() {
                if let Some(ch) = char::from_u32(code) {
                    return ch.to_string();
                }
            }
        }

        // Level 4: Raw char code as Unicode (last resort)
        match char::from_u32(code) {
            Some(ch) if !ch.is_control() => ch.to_string(),
            _ => String::from('\u{FFFD}'),
        }
    }

    /// Resolve a sequence of character codes to a Unicode string.
    pub fn resolve_text(&self, codes: &[u32]) -> String {
        let mut result = String::with_capacity(codes.len());
        for &code in codes {
            result.push_str(&self.resolve_char(code));
        }
        result
    }

    /// Get the width of a character code in 1/1000 units of text space.
    pub fn char_width(&self, code: u32) -> f64 {
        // Try the /Widths array first (only if it actually covers this code)
        if self.font_type.is_simple() {
            if !self.widths.widths.is_empty()
                && code >= self.widths.first_char
                && code <= self.widths.last_char
            {
                let w = self.widths.get_width(code);
                if w != 0.0 {
                    return w;
                }
            }
        } else {
            let w = self.widths.get_cid_width(code);
            if w != self.widths.default_width || !self.widths.cid_widths.is_empty() {
                return w;
            }
        }

        // Try TrueType metrics
        if let Some(ref ttf) = self.truetype {
            let w = ttf.char_width(code);
            if w != 0.0 {
                return w;
            }
        }

        // Try CFF metrics
        if let Some(ref cff) = self.cff {
            let w = cff.glyph_width(code as u16);
            if w != self.default_width {
                return w;
            }
        }

        // Try standard font metrics
        if let Some(std_font) = &self.standard_font {
            if code <= 255 {
                let w = std_font.char_width(code as u8);
                if w != 0 {
                    return w as f64;
                }
            }
        }

        self.default_width
    }

    /// Get widths for a sequence of character codes.
    pub fn glyph_widths(&self, codes: &[u32]) -> Vec<f64> {
        codes.iter().map(|&c| self.char_width(c)).collect()
    }

    /// Determine the byte length of a character code for this font.
    /// Simple fonts use 1 byte; CID fonts use 1-2 bytes based on CMap.
    pub fn code_length(&self, first_byte: u8) -> u8 {
        if self.font_type.is_simple() {
            return 1;
        }
        if let Some(ref cmap) = self.to_unicode {
            return cmap.code_length(first_byte);
        }
        2
    }
}

/// Cache of parsed fonts, keyed by font resource name within a page.
#[derive(Debug, Clone, Default)]
pub struct FontCache {
    /// Fonts keyed by their resource name (e.g., "F1", "F2").
    fonts: HashMap<String, PdfFont>,
}

impl FontCache {
    /// Create an empty font cache.
    pub fn new() -> Self {
        FontCache {
            fonts: HashMap::new(),
        }
    }

    /// Insert a parsed font into the cache.
    pub fn insert(&mut self, name: String, font: PdfFont) {
        self.fonts.insert(name, font);
    }

    /// Look up a font by its resource name.
    pub fn get(&self, name: &str) -> Option<&PdfFont> {
        self.fonts.get(name)
    }

    /// Check if a font is cached.
    pub fn contains(&self, name: &str) -> bool {
        self.fonts.contains_key(name)
    }

    /// Number of cached fonts.
    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    /// Clear all cached fonts.
    pub fn clear(&mut self) {
        self.fonts.clear();
    }
}

/// Builder for constructing a `PdfFont` from PDF dictionary data.
#[derive(Debug, Default)]
pub struct PdfFontBuilder {
    name: String,
    font_type: Option<FontType>,
    to_unicode_data: Option<Vec<u8>>,
    base_encoding: Option<BaseEncoding>,
    differences: Vec<(u8, String)>,
    first_char: u32,
    last_char: u32,
    widths: Vec<f64>,
    default_width: f64,
    truetype_data: Option<Vec<u8>>,
    cff_data: Option<Vec<u8>>,
    cid_widths: Vec<super::types::CidWidthEntry>,
}

impl PdfFontBuilder {
    /// Create a new builder with the font's name.
    pub fn new(name: impl Into<String>) -> Self {
        PdfFontBuilder {
            name: name.into(),
            default_width: 1000.0,
            ..Default::default()
        }
    }

    /// Set the font subtype.
    pub fn font_type(mut self, ft: FontType) -> Self {
        self.font_type = Some(ft);
        self
    }

    /// Set the ToUnicode CMap stream data.
    pub fn to_unicode(mut self, data: Vec<u8>) -> Self {
        self.to_unicode_data = Some(data);
        self
    }

    /// Set the base encoding.
    pub fn base_encoding(mut self, enc: BaseEncoding) -> Self {
        self.base_encoding = Some(enc);
        self
    }

    /// Set the /Differences array entries.
    pub fn differences(mut self, diffs: Vec<(u8, String)>) -> Self {
        self.differences = diffs;
        self
    }

    /// Set the /Widths array with /FirstChar and /LastChar.
    pub fn widths(mut self, first_char: u32, last_char: u32, widths: Vec<f64>) -> Self {
        self.first_char = first_char;
        self.last_char = last_char;
        self.widths = widths;
        self
    }

    /// Set the default width for CID fonts.
    pub fn default_width(mut self, w: f64) -> Self {
        self.default_width = w;
        self
    }

    /// Set CID width entries from /W array.
    pub fn cid_widths(mut self, entries: Vec<super::types::CidWidthEntry>) -> Self {
        self.cid_widths = entries;
        self
    }

    /// Set embedded TrueType font data.
    pub fn truetype_data(mut self, data: Vec<u8>) -> Self {
        self.truetype_data = Some(data);
        self
    }

    /// Set embedded CFF font data.
    pub fn cff_data(mut self, data: Vec<u8>) -> Self {
        self.cff_data = Some(data);
        self
    }

    /// Build the `PdfFont`.
    pub fn build(self) -> PdfFont {
        let font_type = self.font_type.unwrap_or(FontType::Type1);
        let standard_font = StandardFont::from_name(&self.name);

        // Parse ToUnicode CMap
        let to_unicode = self
            .to_unicode_data
            .and_then(|data| CMap::parse(&data).ok());

        // Build encoding for simple fonts
        let encoding = if font_type.is_simple() {
            let mut enc = match self.base_encoding {
                Some(base) => Encoding::from_base(base),
                None => {
                    if standard_font.is_some() {
                        Encoding::from_base(BaseEncoding::Standard)
                    } else {
                        Encoding::from_base(BaseEncoding::WinAnsi)
                    }
                }
            };
            if !self.differences.is_empty() {
                let diffs: Vec<(u8, &str)> = self
                    .differences
                    .iter()
                    .map(|(code, name)| (*code, name.as_str()))
                    .collect();
                enc.apply_differences(&diffs);
            }
            Some(enc)
        } else {
            None
        };

        // Build widths
        let widths = FontWidths {
            first_char: self.first_char,
            last_char: self.last_char,
            widths: self.widths,
            default_width: self.default_width,
            cid_widths: self.cid_widths,
        };

        // Parse TrueType data
        let truetype = self
            .truetype_data
            .and_then(|data| TrueTypeFont::parse(&data).ok());

        // Parse CFF data
        let cff = self
            .cff_data
            .and_then(|data| super::cff::parse_cff(&data).ok());

        PdfFont {
            name: self.name,
            font_type,
            to_unicode,
            encoding,
            widths,
            standard_font,
            truetype,
            cff,
            default_width: self.default_width,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_cache_basic() {
        let mut cache = FontCache::new();
        assert!(cache.is_empty());

        let font = PdfFontBuilder::new("Helvetica")
            .font_type(FontType::Type1)
            .base_encoding(BaseEncoding::WinAnsi)
            .build();

        cache.insert("F1".to_string(), font);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains("F1"));
        assert!(!cache.contains("F2"));
    }

    #[test]
    fn test_resolve_char_with_to_unicode() {
        let cmap_data = b"1 begincodespacerange
<00> <FF>
endcodespacerange
2 beginbfchar
<01> <0048>
<02> <0069>
endbfchar
";
        let font = PdfFontBuilder::new("CustomFont")
            .font_type(FontType::Type1)
            .to_unicode(cmap_data.to_vec())
            .build();

        assert_eq!(font.resolve_char(0x01), "H");
        assert_eq!(font.resolve_char(0x02), "i");
    }

    #[test]
    fn test_resolve_char_encoding_fallback() {
        let font = PdfFontBuilder::new("Helvetica")
            .font_type(FontType::Type1)
            .base_encoding(BaseEncoding::WinAnsi)
            .build();

        assert_eq!(font.resolve_char(0x41), "A");
        assert_eq!(font.resolve_char(0x20), " ");
    }

    #[test]
    fn test_resolve_text() {
        let font = PdfFontBuilder::new("Helvetica")
            .font_type(FontType::Type1)
            .base_encoding(BaseEncoding::WinAnsi)
            .build();

        let text = font.resolve_text(&[0x48, 0x65, 0x6C, 0x6C, 0x6F]);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn test_char_width_from_widths_array() {
        let font = PdfFontBuilder::new("TestFont")
            .font_type(FontType::Type1)
            .widths(32, 34, vec![250.0, 333.0, 408.0])
            .build();

        assert_eq!(font.char_width(32), 250.0);
        assert_eq!(font.char_width(33), 333.0);
        assert_eq!(font.char_width(34), 408.0);
    }

    #[test]
    fn test_char_width_standard_fallback() {
        let font = PdfFontBuilder::new("Helvetica")
            .font_type(FontType::Type1)
            .build();

        // Falls back to standard Helvetica widths
        assert_eq!(font.char_width(0x41), 667.0); // 'A'
        assert_eq!(font.char_width(0x20), 278.0); // space
    }

    #[test]
    fn test_glyph_widths() {
        let font = PdfFontBuilder::new("Helvetica")
            .font_type(FontType::Type1)
            .build();

        let widths = font.glyph_widths(&[0x41, 0x42, 0x43]);
        assert_eq!(widths, vec![667.0, 667.0, 722.0]);
    }

    #[test]
    fn test_to_unicode_takes_priority() {
        // ToUnicode maps code 0x41 to 'Z' (overriding encoding)
        let cmap_data = b"1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <005A>
endbfchar
";
        let font = PdfFontBuilder::new("TestFont")
            .font_type(FontType::Type1)
            .base_encoding(BaseEncoding::WinAnsi)
            .to_unicode(cmap_data.to_vec())
            .build();

        // ToUnicode says 0x41 → 'Z', even though WinAnsi says 'A'
        assert_eq!(font.resolve_char(0x41), "Z");
    }

    #[test]
    fn test_code_length_simple_font() {
        let font = PdfFontBuilder::new("Helvetica")
            .font_type(FontType::Type1)
            .build();
        assert_eq!(font.code_length(0x41), 1);
    }

    #[test]
    fn test_code_length_cid_font() {
        let font = PdfFontBuilder::new("CIDFont")
            .font_type(FontType::Type0)
            .build();
        assert_eq!(font.code_length(0x41), 2);
    }

    #[test]
    fn test_builder_with_differences() {
        let font = PdfFontBuilder::new("TestFont")
            .font_type(FontType::Type1)
            .base_encoding(BaseEncoding::WinAnsi)
            .differences(vec![(0x41, "germandbls".to_string())])
            .build();

        assert_eq!(font.resolve_char(0x41), "\u{00DF}");
    }
}
