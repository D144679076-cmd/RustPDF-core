//! Core font types for PDF font representation.
//!
//! Defines the structural types used to represent parsed PDF font dictionaries,
//! font descriptors, width tables, and font classification.

/// PDF font subtype classification (ISO 32000-1 §9.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontType {
    /// Type 1 font (PostScript outlines).
    Type1,
    /// MMType1 (Multiple Master Type 1).
    MMType1,
    /// TrueType font.
    TrueType,
    /// Type 3 font (user-defined glyphs via content streams).
    Type3,
    /// Type 0 (composite) font — references a CIDFont.
    Type0,
    /// CIDFont with Type 0 (CFF) outlines.
    CIDFontType0,
    /// CIDFont with TrueType outlines.
    CIDFontType2,
}

impl FontType {
    /// Parse from the /Subtype name in a font dictionary.
    pub fn from_subtype(name: &str) -> Option<Self> {
        match name {
            "Type1" => Some(FontType::Type1),
            "MMType1" => Some(FontType::MMType1),
            "TrueType" => Some(FontType::TrueType),
            "Type3" => Some(FontType::Type3),
            "Type0" => Some(FontType::Type0),
            "CIDFontType0" => Some(FontType::CIDFontType0),
            "CIDFontType2" => Some(FontType::CIDFontType2),
            _ => None,
        }
    }

    /// Whether this is a composite (CID-keyed) font.
    pub fn is_composite(&self) -> bool {
        matches!(
            self,
            FontType::Type0 | FontType::CIDFontType0 | FontType::CIDFontType2
        )
    }

    /// Whether this is a simple font (single-byte character codes).
    pub fn is_simple(&self) -> bool {
        !self.is_composite()
    }
}

/// Font descriptor flags (ISO 32000-1 §9.8.2, Table 123).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FontFlags(pub u32);

impl FontFlags {
    pub const FIXED_PITCH: u32 = 1 << 0;
    pub const SERIF: u32 = 1 << 1;
    pub const SYMBOLIC: u32 = 1 << 2;
    pub const SCRIPT: u32 = 1 << 3;
    pub const NONSYMBOLIC: u32 = 1 << 5;
    pub const ITALIC: u32 = 1 << 6;
    pub const ALL_CAP: u32 = 1 << 16;
    pub const SMALL_CAP: u32 = 1 << 17;
    pub const FORCE_BOLD: u32 = 1 << 18;

    /// Check if a specific flag is set.
    pub fn has(&self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    /// Whether the font uses a symbolic character set.
    pub fn is_symbolic(&self) -> bool {
        self.has(Self::SYMBOLIC)
    }

    /// Whether the font uses a non-symbolic (standard Latin) character set.
    pub fn is_nonsymbolic(&self) -> bool {
        self.has(Self::NONSYMBOLIC)
    }

    /// Whether the font is fixed-pitch (monospaced).
    pub fn is_fixed_pitch(&self) -> bool {
        self.has(Self::FIXED_PITCH)
    }

    /// Whether the font is italic.
    pub fn is_italic(&self) -> bool {
        self.has(Self::ITALIC)
    }
}

/// Font descriptor (ISO 32000-1 §9.8).
#[derive(Debug, Clone)]
pub struct FontDescriptor {
    /// PostScript name of the font.
    pub font_name: String,
    /// Font family name.
    pub font_family: String,
    /// Font stretch (e.g. "Normal", "Condensed").
    pub font_stretch: String,
    /// Font weight (100-900, 400=normal, 700=bold).
    pub font_weight: f64,
    /// Font flags.
    pub flags: FontFlags,
    /// Font bounding box [llx, lly, urx, ury] in glyph space.
    pub bbox: [f64; 4],
    /// Italic angle in degrees (negative = slanted right).
    pub italic_angle: f64,
    /// Maximum ascender height above baseline.
    pub ascent: f64,
    /// Maximum descender depth below baseline (typically negative).
    pub descent: f64,
    /// Vertical distance between baselines.
    pub leading: f64,
    /// Capital letter height.
    pub cap_height: f64,
    /// x-height (height of lowercase 'x').
    pub x_height: f64,
    /// Dominant vertical stem width.
    pub stem_v: f64,
    /// Dominant horizontal stem width.
    pub stem_h: f64,
    /// Average glyph width.
    pub avg_width: f64,
    /// Maximum glyph width.
    pub max_width: f64,
    /// Width of missing glyph.
    pub missing_width: f64,
}

impl Default for FontDescriptor {
    fn default() -> Self {
        FontDescriptor {
            font_name: String::new(),
            font_family: String::new(),
            font_stretch: "Normal".to_string(),
            font_weight: 400.0,
            flags: FontFlags::default(),
            bbox: [0.0; 4],
            italic_angle: 0.0,
            ascent: 0.0,
            descent: 0.0,
            leading: 0.0,
            cap_height: 0.0,
            x_height: 0.0,
            stem_v: 0.0,
            stem_h: 0.0,
            avg_width: 0.0,
            max_width: 0.0,
            missing_width: 0.0,
        }
    }
}

/// Width entry for CIDFont /W array.
#[derive(Debug, Clone, PartialEq)]
pub enum CidWidthEntry {
    /// Range of consecutive CIDs sharing the same width: start_cid, end_cid, width.
    Range(u32, u32, f64),
    /// Individual CID widths: start_cid, [w1, w2, ...].
    Individual(u32, Vec<f64>),
}

/// Font width information extracted from a PDF font dictionary.
#[derive(Debug, Clone)]
pub struct FontWidths {
    /// First character code with a defined width (simple fonts).
    pub first_char: u32,
    /// Last character code with a defined width (simple fonts).
    pub last_char: u32,
    /// Width array for simple fonts (indexed by char_code - first_char).
    pub widths: Vec<f64>,
    /// Default width for CIDFonts (in 1/1000 units of text space).
    pub default_width: f64,
    /// CIDFont /W array entries.
    pub cid_widths: Vec<CidWidthEntry>,
}

impl Default for FontWidths {
    fn default() -> Self {
        FontWidths {
            first_char: 0,
            last_char: 0,
            widths: Vec::new(),
            default_width: 1000.0,
            cid_widths: Vec::new(),
        }
    }
}

impl FontWidths {
    /// Get the width for a character code in a simple font.
    /// Returns width in 1/1000 units of text space.
    pub fn get_width(&self, char_code: u32) -> f64 {
        if char_code >= self.first_char && char_code <= self.last_char {
            let idx = (char_code - self.first_char) as usize;
            if idx < self.widths.len() {
                return self.widths[idx];
            }
        }
        self.default_width
    }

    /// Get the width for a CID in a CIDFont.
    /// Returns width in 1/1000 units of text space.
    pub fn get_cid_width(&self, cid: u32) -> f64 {
        for entry in &self.cid_widths {
            match entry {
                CidWidthEntry::Range(start, end, width) => {
                    if cid >= *start && cid <= *end {
                        return *width;
                    }
                }
                CidWidthEntry::Individual(start, widths) => {
                    let offset = cid.checked_sub(*start);
                    if let Some(idx) = offset {
                        if (idx as usize) < widths.len() {
                            return widths[idx as usize];
                        }
                    }
                }
            }
        }
        self.default_width
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_type_from_subtype() {
        assert_eq!(FontType::from_subtype("Type1"), Some(FontType::Type1));
        assert_eq!(FontType::from_subtype("TrueType"), Some(FontType::TrueType));
        assert_eq!(FontType::from_subtype("Type0"), Some(FontType::Type0));
        assert_eq!(
            FontType::from_subtype("CIDFontType2"),
            Some(FontType::CIDFontType2)
        );
        assert_eq!(FontType::from_subtype("Unknown"), None);
    }

    #[test]
    fn test_font_type_classification() {
        assert!(FontType::Type1.is_simple());
        assert!(FontType::TrueType.is_simple());
        assert!(FontType::Type3.is_simple());
        assert!(!FontType::Type0.is_simple());
        assert!(FontType::Type0.is_composite());
        assert!(FontType::CIDFontType0.is_composite());
        assert!(FontType::CIDFontType2.is_composite());
    }

    #[test]
    fn test_font_flags() {
        let flags = FontFlags(FontFlags::SYMBOLIC | FontFlags::ITALIC);
        assert!(flags.is_symbolic());
        assert!(flags.is_italic());
        assert!(!flags.is_nonsymbolic());
        assert!(!flags.is_fixed_pitch());
    }

    #[test]
    fn test_simple_font_widths() {
        let widths = FontWidths {
            first_char: 32,
            last_char: 34,
            widths: vec![250.0, 333.0, 408.0],
            default_width: 1000.0,
            cid_widths: Vec::new(),
        };
        assert_eq!(widths.get_width(32), 250.0);
        assert_eq!(widths.get_width(33), 333.0);
        assert_eq!(widths.get_width(34), 408.0);
        assert_eq!(widths.get_width(31), 1000.0); // below range
        assert_eq!(widths.get_width(35), 1000.0); // above range
    }

    #[test]
    fn test_cid_font_widths_range() {
        let widths = FontWidths {
            first_char: 0,
            last_char: 0,
            widths: Vec::new(),
            default_width: 1000.0,
            cid_widths: vec![CidWidthEntry::Range(1, 100, 500.0)],
        };
        assert_eq!(widths.get_cid_width(1), 500.0);
        assert_eq!(widths.get_cid_width(50), 500.0);
        assert_eq!(widths.get_cid_width(100), 500.0);
        assert_eq!(widths.get_cid_width(101), 1000.0);
    }

    #[test]
    fn test_cid_font_widths_individual() {
        let widths = FontWidths {
            first_char: 0,
            last_char: 0,
            widths: Vec::new(),
            default_width: 1000.0,
            cid_widths: vec![CidWidthEntry::Individual(10, vec![200.0, 300.0, 400.0])],
        };
        assert_eq!(widths.get_cid_width(10), 200.0);
        assert_eq!(widths.get_cid_width(11), 300.0);
        assert_eq!(widths.get_cid_width(12), 400.0);
        assert_eq!(widths.get_cid_width(13), 1000.0);
        assert_eq!(widths.get_cid_width(9), 1000.0);
    }

    #[test]
    fn test_font_descriptor_default() {
        let desc = FontDescriptor::default();
        assert_eq!(desc.font_weight, 400.0);
        assert_eq!(desc.font_stretch, "Normal");
        assert_eq!(desc.flags, FontFlags::default());
    }
}
