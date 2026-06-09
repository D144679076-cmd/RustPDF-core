//! Type1 font metric extraction.
//!
//! In PDF, Type1 fonts typically provide metrics via the /Widths array in the
//! font dictionary rather than requiring parsing of the actual Type1 font program.
//! This module handles width extraction and encoding resolution for Type1 fonts.

use super::encoding::{BaseEncoding, Encoding};
use super::standard::StandardFont;
use super::types::FontWidths;

/// A parsed Type1 font providing character widths and encoding.
#[derive(Debug, Clone)]
pub struct Type1Font {
    /// The font's encoding (for char code → glyph name mapping).
    pub encoding: Encoding,
    /// Width information from the PDF font dictionary.
    pub widths: FontWidths,
    /// If this is a standard 14 font, its identity.
    pub standard_font: Option<StandardFont>,
}

impl Type1Font {
    /// Create a Type1 font from a /Widths array and encoding.
    ///
    /// `first_char` and `last_char` come from the font dictionary's /FirstChar and /LastChar.
    /// `widths` is the /Widths array (one entry per code from first_char to last_char).
    /// `base_encoding` is the /BaseEncoding name (if specified).
    /// `differences` are from the /Encoding /Differences array.
    pub fn new(
        first_char: u32,
        last_char: u32,
        widths: Vec<f64>,
        base_encoding: Option<BaseEncoding>,
        differences: &[(u8, &str)],
        font_name: Option<&str>,
    ) -> Self {
        let standard_font = font_name.and_then(StandardFont::from_name);

        let mut encoding = match base_encoding {
            Some(base) => Encoding::from_base(base),
            None => Encoding::from_base(BaseEncoding::Standard),
        };

        if !differences.is_empty() {
            encoding.apply_differences(differences);
        }

        let font_widths = FontWidths {
            first_char,
            last_char,
            widths,
            default_width: 0.0,
            cid_widths: Vec::new(),
        };

        Type1Font {
            encoding,
            widths: font_widths,
            standard_font,
        }
    }

    /// Create a Type1 font backed by a standard 14 font (no /Widths in dict).
    pub fn from_standard(font: StandardFont, base_encoding: Option<BaseEncoding>) -> Self {
        let encoding = match base_encoding {
            Some(base) => Encoding::from_base(base),
            None => Encoding::from_base(BaseEncoding::Standard),
        };

        // Build widths from the standard font table
        let table = font.width_table();
        let widths: Vec<f64> = table.iter().map(|&w| w as f64).collect();

        let font_widths = FontWidths {
            first_char: 0,
            last_char: 255,
            widths,
            default_width: 0.0,
            cid_widths: Vec::new(),
        };

        Type1Font {
            encoding,
            widths: font_widths,
            standard_font: Some(font),
        }
    }

    /// Get the width of a character code in 1/1000 units of text space.
    pub fn char_width(&self, code: u8) -> f64 {
        let w = self.widths.get_width(code as u32);
        if w != 0.0 {
            return w;
        }
        // Fall back to standard font metrics if available
        if let Some(std_font) = &self.standard_font {
            return std_font.char_width(code) as f64;
        }
        0.0
    }

    /// Decode a character code to Unicode using the font's encoding.
    pub fn decode_char(&self, code: u8) -> Option<char> {
        self.encoding.decode_char(code)
    }

    /// Decode a byte sequence to a Unicode string.
    pub fn decode_bytes(&self, bytes: &[u8]) -> String {
        self.encoding.decode_bytes(bytes)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type1_from_widths() {
        let font = Type1Font::new(
            32, // firstChar
            34, // lastChar
            vec![250.0, 333.0, 408.0],
            Some(BaseEncoding::WinAnsi),
            &[],
            None,
        );
        assert_eq!(font.char_width(32), 250.0);
        assert_eq!(font.char_width(33), 333.0);
        assert_eq!(font.char_width(34), 408.0);
        assert_eq!(font.char_width(35), 0.0); // outside range
    }

    #[test]
    fn test_type1_from_standard() {
        let font = Type1Font::from_standard(StandardFont::Helvetica, None);
        assert_eq!(font.char_width(b' '), 278.0);
        assert_eq!(font.char_width(b'A'), 667.0);
        assert_eq!(font.char_width(b'i'), 222.0);
    }

    #[test]
    fn test_type1_decode_winansi() {
        let font = Type1Font::new(
            32,
            127,
            vec![0.0; 96],
            Some(BaseEncoding::WinAnsi),
            &[],
            None,
        );
        assert_eq!(font.decode_char(0x41), Some('A'));
        assert_eq!(font.decode_char(0x61), Some('a'));
    }

    #[test]
    fn test_type1_with_differences() {
        let font = Type1Font::new(
            32,
            127,
            vec![0.0; 96],
            Some(BaseEncoding::WinAnsi),
            &[(0x41, "germandbls")],
            None,
        );
        // Code 0x41 now maps to ß instead of A
        assert_eq!(font.decode_char(0x41), Some('\u{00DF}'));
    }

    #[test]
    fn test_type1_standard_font_fallback_widths() {
        // Create with empty widths but a standard font name
        let font = Type1Font::new(
            0,
            0,
            vec![],
            Some(BaseEncoding::WinAnsi),
            &[],
            Some("Helvetica"),
        );
        // Should fall back to standard font widths
        assert_eq!(font.char_width(b'A'), 667.0);
    }

    #[test]
    fn test_type1_decode_bytes() {
        let font = Type1Font::from_standard(StandardFont::Helvetica, Some(BaseEncoding::WinAnsi));
        let result = font.decode_bytes(&[0x48, 0x65, 0x6C, 0x6C, 0x6F]);
        assert_eq!(result, "Hello");
    }
}
