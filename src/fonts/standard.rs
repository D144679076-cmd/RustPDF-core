//! The 14 standard PDF fonts and their glyph width metrics.
//!
//! Every PDF viewer must support these fonts without embedding. Width tables
//! are derived from Adobe AFM data (in 1/1000 units of text space).

/// Identifies one of the 14 standard PDF fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardFont {
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    Symbol,
    ZapfDingbats,
}

/// Strip a subset prefix (`ABCDEF+`) from a font name.
///
/// Subset fonts are named `XXXXXX+FontName` where `XXXXXX` is exactly six
/// uppercase letters (ISO 32000-1 §9.6.4). The tag is irrelevant to metric
/// matching, so we remove it before looking the name up — otherwise a
/// subsetted standard font like `ABCDEF+Arial-BoldMT` would fail to match and
/// fall back to estimated widths, mispositioning text.
fn strip_subset_prefix(name: &str) -> &str {
    if let Some((tag, rest)) = name.split_once('+') {
        if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) {
            return rest;
        }
    }
    name
}

impl StandardFont {
    /// Match a PDF font name to a standard font.
    ///
    /// Tolerates subset prefixes (`ABCDEF+Helvetica`) by stripping the tag
    /// before matching, so subsetted standard fonts still resolve to correct
    /// metrics even when their program bytes are not embedded.
    pub fn from_name(name: &str) -> Option<Self> {
        match strip_subset_prefix(name) {
            "Times-Roman" | "TimesNewRoman" | "TimesNewRomanPSMT" => Some(StandardFont::TimesRoman),
            "Times-Bold" | "TimesNewRoman,Bold" | "TimesNewRomanPS-BoldMT" => {
                Some(StandardFont::TimesBold)
            }
            "Times-Italic" | "TimesNewRoman,Italic" | "TimesNewRomanPS-ItalicMT" => {
                Some(StandardFont::TimesItalic)
            }
            "Times-BoldItalic" | "TimesNewRoman,BoldItalic" | "TimesNewRomanPS-BoldItalicMT" => {
                Some(StandardFont::TimesBoldItalic)
            }
            "Helvetica" | "ArialMT" | "Arial" => Some(StandardFont::Helvetica),
            "Helvetica-Bold" | "Arial,Bold" | "Arial-BoldMT" => Some(StandardFont::HelveticaBold),
            "Helvetica-Oblique" | "Helvetica-Italic" | "Arial,Italic" | "Arial-ItalicMT" => {
                Some(StandardFont::HelveticaOblique)
            }
            "Helvetica-BoldOblique"
            | "Helvetica-BoldItalic"
            | "Arial,BoldItalic"
            | "Arial-BoldItalicMT" => Some(StandardFont::HelveticaBoldOblique),
            "Courier" | "CourierNew" | "CourierNewPSMT" => Some(StandardFont::Courier),
            "Courier-Bold" | "CourierNew,Bold" | "CourierNewPS-BoldMT" => {
                Some(StandardFont::CourierBold)
            }
            "Courier-Oblique"
            | "Courier-Italic"
            | "CourierNew,Italic"
            | "CourierNewPS-ItalicMT" => Some(StandardFont::CourierOblique),
            "Courier-BoldOblique"
            | "Courier-BoldItalic"
            | "CourierNew,BoldItalic"
            | "CourierNewPS-BoldItalicMT" => Some(StandardFont::CourierBoldOblique),
            "Symbol" => Some(StandardFont::Symbol),
            "ZapfDingbats" => Some(StandardFont::ZapfDingbats),
            _ => None,
        }
    }

    /// Get the canonical PostScript name for this font.
    pub fn ps_name(&self) -> &'static str {
        match self {
            StandardFont::TimesRoman => "Times-Roman",
            StandardFont::TimesBold => "Times-Bold",
            StandardFont::TimesItalic => "Times-Italic",
            StandardFont::TimesBoldItalic => "Times-BoldItalic",
            StandardFont::Helvetica => "Helvetica",
            StandardFont::HelveticaBold => "Helvetica-Bold",
            StandardFont::HelveticaOblique => "Helvetica-Oblique",
            StandardFont::HelveticaBoldOblique => "Helvetica-BoldOblique",
            StandardFont::Courier => "Courier",
            StandardFont::CourierBold => "Courier-Bold",
            StandardFont::CourierOblique => "Courier-Oblique",
            StandardFont::CourierBoldOblique => "Courier-BoldOblique",
            StandardFont::Symbol => "Symbol",
            StandardFont::ZapfDingbats => "ZapfDingbats",
        }
    }

    /// Whether this is a monospaced font (Courier variants).
    pub fn is_monospaced(&self) -> bool {
        matches!(
            self,
            StandardFont::Courier
                | StandardFont::CourierBold
                | StandardFont::CourierOblique
                | StandardFont::CourierBoldOblique
        )
    }

    /// Get the width of a character code (0–255) in 1/1000 units.
    /// Returns 0 for undefined characters.
    pub fn char_width(&self, code: u8) -> u16 {
        let table = self.width_table();
        table[code as usize]
    }

    /// Get the full 256-entry width table for this font.
    pub fn width_table(&self) -> &'static [u16; 256] {
        match self {
            StandardFont::TimesRoman => &TIMES_ROMAN_WIDTHS,
            StandardFont::TimesBold => &TIMES_BOLD_WIDTHS,
            StandardFont::TimesItalic => &TIMES_ITALIC_WIDTHS,
            StandardFont::TimesBoldItalic => &TIMES_BOLD_ITALIC_WIDTHS,
            StandardFont::Helvetica => &HELVETICA_WIDTHS,
            StandardFont::HelveticaBold => &HELVETICA_BOLD_WIDTHS,
            StandardFont::HelveticaOblique => &HELVETICA_OBLIQUE_WIDTHS,
            StandardFont::HelveticaBoldOblique => &HELVETICA_BOLD_OBLIQUE_WIDTHS,
            StandardFont::Courier => &COURIER_WIDTHS,
            StandardFont::CourierBold => &COURIER_WIDTHS,
            StandardFont::CourierOblique => &COURIER_WIDTHS,
            StandardFont::CourierBoldOblique => &COURIER_WIDTHS,
            StandardFont::Symbol => &SYMBOL_WIDTHS,
            StandardFont::ZapfDingbats => &ZAPF_DINGBATS_WIDTHS,
        }
    }
}

// ---------------------------------------------------------------------------
// Width tables — 256 entries each, indexed by character code (WinAnsiEncoding).
// Values in 1/1000 units of text space. 0 = undefined/no glyph.
// Derived from Adobe AFM files.
// ---------------------------------------------------------------------------

/// Courier: all glyphs are 600 units wide (monospaced).
#[rustfmt::skip]
static COURIER_WIDTHS: [u16; 256] = {
    let mut w = [0u16; 256];
    let mut i = 0x20;
    while i <= 0xFF {
        w[i] = 600;
        i += 1;
    }
    w
};

/// Helvetica (from Helvetica.afm).
#[rustfmt::skip]
static HELVETICA_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0, // 0x00-0x0F
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0, // 0x10-0x1F
    278,278,355,556,556,889,667,191,333,333,389,584,278,333,278,278, // 0x20-0x2F
    556,556,556,556,556,556,556,556,556,556,278,278,584,584,584,556, // 0x30-0x3F
    1015,667,667,722,722,667,611,778,722,278,500,667,556,833,722,778, // 0x40-0x4F
    667,778,722,667,611,722,667,944,667,667,611,278,278,278,469,556, // 0x50-0x5F
    333,556,556,500,556,556,278,556,556,222,222,500,222,833,556,556, // 0x60-0x6F
    556,556,333,500,278,556,500,722,500,500,500,334,260,334,584,0, // 0x70-0x7F
    556,0,222,556,333,1000,556,556,333,1000,667,333,1000,0,611,0, // 0x80-0x8F
    0,222,222,333,333,350,556,1000,333,1000,500,333,944,0,500,667, // 0x90-0x9F
    278,333,556,556,556,556,260,556,333,737,370,556,584,333,737,333, // 0xA0-0xAF
    400,584,333,333,333,556,537,278,333,333,365,556,834,834,834,611, // 0xB0-0xBF
    667,667,667,667,667,667,1000,722,667,667,667,667,278,278,278,278, // 0xC0-0xCF
    722,722,778,778,778,778,778,584,778,722,722,722,722,667,667,611, // 0xD0-0xDF
    556,556,556,556,556,556,889,500,556,556,556,556,278,278,278,278, // 0xE0-0xEF
    556,556,556,556,556,556,556,584,611,556,556,556,556,500,556,500, // 0xF0-0xFF
];

/// Helvetica-Bold (from Helvetica-Bold.afm).
#[rustfmt::skip]
static HELVETICA_BOLD_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    278,333,474,556,556,889,722,238,333,333,389,584,278,333,278,278, // 0x20
    556,556,556,556,556,556,556,556,556,556,333,333,584,584,584,611, // 0x30
    975,722,722,722,722,667,611,778,722,278,556,722,611,833,722,778, // 0x40
    667,778,722,667,611,722,667,944,667,667,611,333,278,333,584,556, // 0x50
    333,556,611,556,611,556,333,611,611,278,278,556,278,889,611,611, // 0x60
    611,611,389,556,333,611,556,778,556,556,500,389,280,389,584,0, // 0x70
    556,0,278,556,500,1000,556,556,333,1000,667,333,1000,0,611,0, // 0x80
    0,278,278,500,500,350,556,1000,333,1000,556,333,944,0,500,667, // 0x90
    278,333,556,556,556,556,280,556,333,737,370,556,584,333,737,333, // 0xA0
    400,584,333,333,333,611,556,278,333,333,365,556,834,834,834,611, // 0xB0
    722,722,722,722,722,722,1000,722,667,667,667,667,278,278,278,278, // 0xC0
    722,722,778,778,778,778,778,584,778,722,722,722,722,667,667,611, // 0xD0
    556,556,556,556,556,556,889,556,556,556,556,556,278,278,278,278, // 0xE0
    611,611,611,611,611,611,611,584,611,611,611,611,611,556,611,556, // 0xF0
];

/// Helvetica-Oblique (same widths as Helvetica).
static HELVETICA_OBLIQUE_WIDTHS: [u16; 256] = HELVETICA_WIDTHS;

/// Helvetica-BoldOblique (same widths as Helvetica-Bold).
static HELVETICA_BOLD_OBLIQUE_WIDTHS: [u16; 256] = HELVETICA_BOLD_WIDTHS;

/// Times-Roman (from Times-Roman.afm).
#[rustfmt::skip]
static TIMES_ROMAN_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    250,333,408,500,500,833,778,180,333,333,500,564,250,333,250,278, // 0x20
    500,500,500,500,500,500,500,500,500,500,278,278,564,564,564,444, // 0x30
    921,722,667,667,722,611,556,722,722,333,389,722,611,889,722,722, // 0x40
    556,722,667,556,611,722,722,944,722,722,611,333,278,333,469,500, // 0x50
    333,444,500,444,500,444,333,500,500,278,278,500,278,778,500,500, // 0x60
    500,500,333,389,278,500,500,722,500,500,444,480,200,480,541,0, // 0x70
    500,0,333,500,444,1000,500,500,333,1000,556,333,889,0,611,0, // 0x80
    0,333,333,444,444,350,500,1000,333,980,389,333,722,0,444,722, // 0x90
    250,333,500,500,500,500,200,500,333,760,276,500,564,333,760,333, // 0xA0
    400,564,300,300,333,500,453,250,333,300,310,500,750,750,750,444, // 0xB0
    722,722,722,722,722,722,889,667,611,611,611,611,333,333,333,333, // 0xC0
    722,722,722,722,722,722,722,564,722,722,722,722,722,722,556,500, // 0xD0
    444,444,444,444,444,444,667,444,444,444,444,444,278,278,278,278, // 0xE0
    500,500,500,500,500,500,500,564,500,500,500,500,500,500,500,500, // 0xF0
];

/// Times-Bold (from Times-Bold.afm).
#[rustfmt::skip]
static TIMES_BOLD_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    250,333,555,500,500,1000,833,278,333,333,500,570,250,333,250,278, // 0x20
    500,500,500,500,500,500,500,500,500,500,333,333,570,570,570,500, // 0x30
    930,722,667,722,722,667,611,778,778,389,500,778,667,944,722,778, // 0x40
    611,778,722,556,667,722,722,1000,722,722,667,333,278,333,581,500, // 0x50
    333,500,556,444,556,444,333,500,556,278,333,556,278,833,556,500, // 0x60
    556,556,444,389,333,556,500,722,500,500,444,394,220,394,520,0, // 0x70
    500,0,333,500,500,1000,500,500,333,1000,556,333,1000,0,667,0, // 0x80
    0,333,333,500,500,350,500,1000,333,1000,389,333,722,0,444,722, // 0x90
    250,333,500,500,500,500,220,500,333,747,300,500,570,333,747,333, // 0xA0
    400,570,300,300,333,556,540,250,333,300,330,500,750,750,750,500, // 0xB0
    722,722,722,722,722,722,1000,722,667,667,667,667,389,389,389,389, // 0xC0
    722,722,778,778,778,778,778,570,778,722,722,722,722,722,611,556, // 0xD0
    500,500,500,500,500,500,722,444,444,444,444,444,278,278,278,278, // 0xE0
    500,556,500,500,500,500,500,570,500,556,556,556,556,500,556,500, // 0xF0
];

/// Times-Italic (from Times-Italic.afm).
#[rustfmt::skip]
static TIMES_ITALIC_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    250,333,420,500,500,833,778,214,333,333,500,675,250,333,250,278, // 0x20
    500,500,500,500,500,500,500,500,500,500,333,333,675,675,675,500, // 0x30
    920,611,611,667,722,611,611,722,722,333,444,667,556,833,667,722, // 0x40
    611,722,611,500,556,722,611,833,611,556,556,389,278,389,422,500, // 0x50
    333,500,500,444,500,444,278,500,500,278,278,444,278,722,500,500, // 0x60
    500,500,389,389,278,500,444,667,444,444,389,400,275,400,541,0, // 0x70
    500,0,333,500,556,889,500,500,333,1000,500,333,944,0,556,0, // 0x80
    0,333,333,556,556,350,500,889,333,980,389,333,667,0,389,556, // 0x90
    250,389,500,500,500,500,275,500,333,760,276,500,675,333,760,333, // 0xA0
    400,675,300,300,333,500,523,250,333,300,310,500,750,750,750,500, // 0xB0
    611,611,611,611,611,611,889,667,611,611,611,611,333,333,333,333, // 0xC0
    722,667,722,722,722,722,722,675,722,722,722,722,722,556,611,500, // 0xD0
    500,500,500,500,500,500,667,444,444,444,444,444,278,278,278,278, // 0xE0
    500,500,500,500,500,500,500,675,500,500,500,500,500,444,500,444, // 0xF0
];

/// Times-BoldItalic (from Times-BoldItalic.afm).
#[rustfmt::skip]
static TIMES_BOLD_ITALIC_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    250,389,555,500,500,833,778,278,333,333,500,570,250,333,250,278, // 0x20
    500,500,500,500,500,500,500,500,500,500,333,333,570,570,570,500, // 0x30
    832,667,667,667,722,667,667,722,778,389,500,667,611,889,722,722, // 0x40
    611,722,667,556,611,722,667,889,667,611,611,333,278,333,570,500, // 0x50
    333,500,500,444,500,444,333,500,556,278,278,500,278,778,556,500, // 0x60
    500,500,389,389,278,556,444,667,500,444,389,348,220,348,570,0, // 0x70
    500,0,333,500,500,1000,500,500,333,1000,556,333,944,0,611,0, // 0x80
    0,333,333,500,500,350,500,1000,333,1000,389,333,722,0,389,611, // 0x90
    250,389,500,500,500,500,220,500,333,747,266,500,606,333,747,333, // 0xA0
    400,570,300,300,333,576,500,250,333,300,300,500,750,750,750,500, // 0xB0
    667,667,667,667,667,667,944,667,667,667,667,667,389,389,389,389, // 0xC0
    722,722,722,722,722,722,722,570,722,722,722,722,722,611,611,500, // 0xD0
    500,500,500,500,500,500,722,444,444,444,444,444,278,278,278,278, // 0xE0
    500,556,500,500,500,500,500,570,500,556,556,556,556,444,500,444, // 0xF0
];

/// Symbol (from Symbol.afm).
#[rustfmt::skip]
static SYMBOL_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    250,333,713,500,549,833,778,439,333,333,500,549,250,549,250,278, // 0x20
    500,500,500,500,500,500,500,500,500,500,278,278,549,549,549,444, // 0x30
    549,722,667,722,612,611,763,603,722,333,631,722,686,889,722,722, // 0x40
    768,741,556,592,611,690,439,768,645,795,611,333,863,333,658,500, // 0x50
    500,631,549,549,494,439,521,411,603,329,603,549,549,576,521,549, // 0x60
    549,521,549,603,439,576,713,686,493,686,494,480,200,480,549,0, // 0x70
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0, // 0x80
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0, // 0x90
    750,620,247,549,167,713,500,753,753,753,753,1042,987,603,987,603, // 0xA0
    400,549,411,549,549,713,494,460,549,549,549,549,1000,603,1000,658, // 0xB0
    823,686,795,987,768,768,823,768,768,713,713,713,713,713,713,713, // 0xC0
    768,713,790,790,890,823,549,250,713,603,603,1042,987,603,987,603, // 0xD0
    494,329,790,790,786,713,384,384,384,384,384,384,494,494,494,494, // 0xE0
    0,329,274,686,686,686,384,384,384,384,384,384,494,494,494,0, // 0xF0
];

/// ZapfDingbats (from ZapfDingbats.afm).
#[rustfmt::skip]
static ZAPF_DINGBATS_WIDTHS: [u16; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    278,974,961,974,980,719,789,790,791,690,960,939,549,855,911,933, // 0x20
    911,945,974,755,846,762,761,571,677,763,760,759,754,494,552,537, // 0x30
    577,692,786,788,788,790,793,794,816,823,789,841,823,833,816,831, // 0x40
    923,744,723,749,790,792,695,776,768,792,759,707,708,682,701,826, // 0x50
    815,789,789,707,687,696,689,786,787,713,791,785,791,873,761,762, // 0x60
    762,759,759,892,892,788,784,438,138,277,415,392,392,668,668,0, // 0x70
    390,390,317,317,276,276,509,509,410,410,234,234,334,334,0,0, // 0x80
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0, // 0x90
    0,732,544,544,910,667,760,760,776,595,694,626,788,788,788,788, // 0xA0
    788,788,788,788,788,788,788,788,788,788,788,788,788,788,788,788, // 0xB0
    788,788,788,788,788,788,788,788,788,788,788,788,788,788,788,788, // 0xC0
    788,788,788,788,894,838,1016,458,748,924,748,918,927,928,928,834, // 0xD0
    873,828,924,924,917,930,931,463,883,836,836,867,867,696,696,874, // 0xE0
    0,874,760,946,771,865,771,888,967,888,831,873,927,970,918,0, // 0xF0
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_name_canonical() {
        assert_eq!(
            StandardFont::from_name("Helvetica"),
            Some(StandardFont::Helvetica)
        );
        assert_eq!(
            StandardFont::from_name("Times-Roman"),
            Some(StandardFont::TimesRoman)
        );
        assert_eq!(
            StandardFont::from_name("Courier"),
            Some(StandardFont::Courier)
        );
        assert_eq!(
            StandardFont::from_name("Symbol"),
            Some(StandardFont::Symbol)
        );
        assert_eq!(
            StandardFont::from_name("ZapfDingbats"),
            Some(StandardFont::ZapfDingbats)
        );
    }

    #[test]
    fn test_from_name_aliases() {
        assert_eq!(
            StandardFont::from_name("ArialMT"),
            Some(StandardFont::Helvetica)
        );
        assert_eq!(
            StandardFont::from_name("TimesNewRomanPSMT"),
            Some(StandardFont::TimesRoman)
        );
        assert_eq!(
            StandardFont::from_name("CourierNewPSMT"),
            Some(StandardFont::Courier)
        );
    }

    #[test]
    fn test_from_name_strips_subset_prefix() {
        // TD-9: subsetted standard fonts (XXXXXX+Name) must resolve to the same
        // metrics as the un-prefixed name, so non-embedded subset fonts get
        // correct widths instead of a fallback estimate.
        assert_eq!(
            StandardFont::from_name("ABCDEF+Helvetica"),
            Some(StandardFont::Helvetica)
        );
        assert_eq!(
            StandardFont::from_name("WXYZAB+Arial-BoldMT"),
            Some(StandardFont::HelveticaBold)
        );
    }

    #[test]
    fn test_subset_prefix_only_six_uppercase() {
        // A non-conforming prefix must NOT be stripped (avoids false matches).
        assert_eq!(strip_subset_prefix("ABC+Helvetica"), "ABC+Helvetica");
        assert_eq!(strip_subset_prefix("abcdef+Helvetica"), "abcdef+Helvetica");
        assert_eq!(strip_subset_prefix("ABCDEF+Helvetica"), "Helvetica");
        assert_eq!(strip_subset_prefix("Helvetica"), "Helvetica");
    }

    #[test]
    fn test_from_name_unknown() {
        assert_eq!(StandardFont::from_name("ComicSans"), None);
        assert_eq!(StandardFont::from_name(""), None);
    }

    #[test]
    fn test_courier_monospaced() {
        let font = StandardFont::Courier;
        assert!(font.is_monospaced());
        assert_eq!(font.char_width(b'A'), 600);
        assert_eq!(font.char_width(b'i'), 600);
        assert_eq!(font.char_width(b' '), 600);
    }

    #[test]
    fn test_helvetica_widths() {
        let font = StandardFont::Helvetica;
        assert!(!font.is_monospaced());
        assert_eq!(font.char_width(b' '), 278);
        assert_eq!(font.char_width(b'A'), 667);
        assert_eq!(font.char_width(b'i'), 222);
        assert_eq!(font.char_width(b'M'), 833);
    }

    #[test]
    fn test_times_roman_widths() {
        let font = StandardFont::TimesRoman;
        assert_eq!(font.char_width(b' '), 250);
        assert_eq!(font.char_width(b'A'), 722);
        assert_eq!(font.char_width(b'i'), 278);
    }

    #[test]
    fn test_undefined_char_returns_zero() {
        let font = StandardFont::Helvetica;
        assert_eq!(font.char_width(0x00), 0);
        assert_eq!(font.char_width(0x01), 0);
    }

    #[test]
    fn test_ps_name() {
        assert_eq!(StandardFont::Helvetica.ps_name(), "Helvetica");
        assert_eq!(StandardFont::TimesBoldItalic.ps_name(), "Times-BoldItalic");
        assert_eq!(StandardFont::CourierOblique.ps_name(), "Courier-Oblique");
    }
}
