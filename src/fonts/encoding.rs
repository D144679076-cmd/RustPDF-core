//! PDF standard encodings and Adobe Glyph List (AGL) lookup.
//!
//! Provides the four standard PDF encodings (StandardEncoding, MacRomanEncoding,
//! WinAnsiEncoding, MacExpertEncoding) and a glyph-name-to-Unicode mapping
//! derived from the Adobe Glyph List.

use std::collections::HashMap;
use std::sync::OnceLock;

/// A PDF encoding maps character codes (0–255) to glyph names.
#[derive(Debug, Clone)]
pub struct Encoding {
    /// Base encoding name (if any).
    pub base_encoding: Option<BaseEncoding>,
    /// Glyph name for each character code (0–255). `None` means `.notdef`.
    pub names: [Option<&'static str>; 256],
}

/// The predefined base encodings in PDF (ISO 32000-1 §D.1–D.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseEncoding {
    /// Adobe StandardEncoding.
    Standard,
    /// Mac OS Roman encoding.
    MacRoman,
    /// Windows ANSI (CP1252) encoding.
    WinAnsi,
    /// Adobe MacExpertEncoding.
    MacExpert,
}

impl BaseEncoding {
    /// Parse from the /BaseEncoding name in a font dictionary.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "StandardEncoding" => Some(BaseEncoding::Standard),
            "MacRomanEncoding" => Some(BaseEncoding::MacRoman),
            "WinAnsiEncoding" => Some(BaseEncoding::WinAnsi),
            "MacExpertEncoding" => Some(BaseEncoding::MacExpert),
            _ => None,
        }
    }
}

impl Encoding {
    /// Create an encoding from a predefined base.
    pub fn from_base(base: BaseEncoding) -> Self {
        let names = match base {
            BaseEncoding::Standard => STANDARD_ENCODING,
            BaseEncoding::MacRoman => MAC_ROMAN_ENCODING,
            BaseEncoding::WinAnsi => WIN_ANSI_ENCODING,
            BaseEncoding::MacExpert => MAC_EXPERT_ENCODING,
        };
        Encoding {
            base_encoding: Some(base),
            names,
        }
    }

    /// Create an empty encoding (all `.notdef`).
    pub fn empty() -> Self {
        Encoding {
            base_encoding: None,
            names: [None; 256],
        }
    }

    /// Apply a /Differences array to this encoding.
    /// Each entry is `(code, glyph_name)`.
    pub fn apply_differences(&mut self, differences: &[(u8, &str)]) {
        for &(code, name) in differences {
            self.names[code as usize] = Some(leak_str(name));
        }
    }

    /// Resolve a character code to a Unicode char via encoding + AGL.
    pub fn decode_char(&self, code: u8) -> Option<char> {
        let glyph_name = self.names[code as usize]?;
        agl_lookup(glyph_name)
    }

    /// Decode a sequence of single-byte character codes to a Unicode string.
    pub fn decode_bytes(&self, bytes: &[u8]) -> String {
        let mut result = String::with_capacity(bytes.len());
        for &code in bytes {
            match self.decode_char(code) {
                Some(ch) => result.push(ch),
                None => result.push(char::REPLACEMENT_CHARACTER),
            }
        }
        result
    }
}

/// Leak a string to get a `&'static str` for dynamic /Differences entries.
fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// Look up a glyph name in the Adobe Glyph List, returning its Unicode codepoint.
pub fn agl_lookup(glyph_name: &str) -> Option<char> {
    // Handle "uniXXXX" form (exactly 4 hex digits after "uni")
    if let Some(hex) = glyph_name.strip_prefix("uni") {
        if hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp);
            }
        }
    }

    // Handle "uXXXX" to "uXXXXXX" form (4-6 hex digits after "u")
    if let Some(hex) = glyph_name.strip_prefix('u') {
        if hex.len() >= 4 && hex.len() <= 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp);
            }
        }
    }

    agl_map().get(glyph_name).copied()
}

/// Get or initialize the AGL HashMap.
fn agl_map() -> &'static HashMap<&'static str, char> {
    static MAP: OnceLock<HashMap<&'static str, char>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::with_capacity(AGL_ENTRIES.len());
        for &(name, codepoint) in AGL_ENTRIES.iter() {
            if let Some(ch) = char::from_u32(codepoint) {
                map.insert(name, ch);
            }
        }
        map
    })
}

// ---------------------------------------------------------------------------
// StandardEncoding (ISO 32000-1 §D.1, Table D.1)
// ---------------------------------------------------------------------------

/// Adobe StandardEncoding: glyph names for codes 0–255.
#[rustfmt::skip]
pub const STANDARD_ENCODING: [Option<&'static str>; 256] = {
    let mut e: [Option<&str>; 256] = [None; 256];
    e[0x20] = Some("space");
    e[0x21] = Some("exclam");
    e[0x22] = Some("quotedbl");
    e[0x23] = Some("numbersign");
    e[0x24] = Some("dollar");
    e[0x25] = Some("percent");
    e[0x26] = Some("ampersand");
    e[0x27] = Some("quoteright");
    e[0x28] = Some("parenleft");
    e[0x29] = Some("parenright");
    e[0x2A] = Some("asterisk");
    e[0x2B] = Some("plus");
    e[0x2C] = Some("comma");
    e[0x2D] = Some("hyphen");
    e[0x2E] = Some("period");
    e[0x2F] = Some("slash");
    e[0x30] = Some("zero");
    e[0x31] = Some("one");
    e[0x32] = Some("two");
    e[0x33] = Some("three");
    e[0x34] = Some("four");
    e[0x35] = Some("five");
    e[0x36] = Some("six");
    e[0x37] = Some("seven");
    e[0x38] = Some("eight");
    e[0x39] = Some("nine");
    e[0x3A] = Some("colon");
    e[0x3B] = Some("semicolon");
    e[0x3C] = Some("less");
    e[0x3D] = Some("equal");
    e[0x3E] = Some("greater");
    e[0x3F] = Some("question");
    e[0x40] = Some("at");
    e[0x41] = Some("A");
    e[0x42] = Some("B");
    e[0x43] = Some("C");
    e[0x44] = Some("D");
    e[0x45] = Some("E");
    e[0x46] = Some("F");
    e[0x47] = Some("G");
    e[0x48] = Some("H");
    e[0x49] = Some("I");
    e[0x4A] = Some("J");
    e[0x4B] = Some("K");
    e[0x4C] = Some("L");
    e[0x4D] = Some("M");
    e[0x4E] = Some("N");
    e[0x4F] = Some("O");
    e[0x50] = Some("P");
    e[0x51] = Some("Q");
    e[0x52] = Some("R");
    e[0x53] = Some("S");
    e[0x54] = Some("T");
    e[0x55] = Some("U");
    e[0x56] = Some("V");
    e[0x57] = Some("W");
    e[0x58] = Some("X");
    e[0x59] = Some("Y");
    e[0x5A] = Some("Z");
    e[0x5B] = Some("bracketleft");
    e[0x5C] = Some("backslash");
    e[0x5D] = Some("bracketright");
    e[0x5E] = Some("asciicircum");
    e[0x5F] = Some("underscore");
    e[0x60] = Some("quoteleft");
    e[0x61] = Some("a");
    e[0x62] = Some("b");
    e[0x63] = Some("c");
    e[0x64] = Some("d");
    e[0x65] = Some("e");
    e[0x66] = Some("f");
    e[0x67] = Some("g");
    e[0x68] = Some("h");
    e[0x69] = Some("i");
    e[0x6A] = Some("j");
    e[0x6B] = Some("k");
    e[0x6C] = Some("l");
    e[0x6D] = Some("m");
    e[0x6E] = Some("n");
    e[0x6F] = Some("o");
    e[0x70] = Some("p");
    e[0x71] = Some("q");
    e[0x72] = Some("r");
    e[0x73] = Some("s");
    e[0x74] = Some("t");
    e[0x75] = Some("u");
    e[0x76] = Some("v");
    e[0x77] = Some("w");
    e[0x78] = Some("x");
    e[0x79] = Some("y");
    e[0x7A] = Some("z");
    e[0x7B] = Some("braceleft");
    e[0x7C] = Some("bar");
    e[0x7D] = Some("braceright");
    e[0x7E] = Some("asciitilde");
    e[0xA1] = Some("exclamdown");
    e[0xA2] = Some("cent");
    e[0xA3] = Some("sterling");
    e[0xA4] = Some("fraction");
    e[0xA5] = Some("yen");
    e[0xA6] = Some("florin");
    e[0xA7] = Some("section");
    e[0xA8] = Some("currency");
    e[0xA9] = Some("quotesingle");
    e[0xAA] = Some("quotedblleft");
    e[0xAB] = Some("guillemotleft");
    e[0xAC] = Some("guilsinglleft");
    e[0xAD] = Some("guilsinglright");
    e[0xAE] = Some("fi");
    e[0xAF] = Some("fl");
    e[0xB1] = Some("endash");
    e[0xB2] = Some("dagger");
    e[0xB3] = Some("daggerdbl");
    e[0xB4] = Some("periodcentered");
    e[0xB6] = Some("paragraph");
    e[0xB7] = Some("bullet");
    e[0xB8] = Some("quotesinglbase");
    e[0xB9] = Some("quotedblbase");
    e[0xBA] = Some("quotedblright");
    e[0xBB] = Some("guillemotright");
    e[0xBC] = Some("ellipsis");
    e[0xBD] = Some("perthousand");
    e[0xBF] = Some("questiondown");
    e[0xC1] = Some("grave");
    e[0xC2] = Some("acute");
    e[0xC3] = Some("circumflex");
    e[0xC4] = Some("tilde");
    e[0xC5] = Some("macron");
    e[0xC6] = Some("breve");
    e[0xC7] = Some("dotaccent");
    e[0xC8] = Some("dieresis");
    e[0xCA] = Some("ring");
    e[0xCB] = Some("cedilla");
    e[0xCD] = Some("hungarumlaut");
    e[0xCE] = Some("ogonek");
    e[0xCF] = Some("caron");
    e[0xD0] = Some("emdash");
    e[0xE1] = Some("AE");
    e[0xE3] = Some("ordfeminine");
    e[0xE8] = Some("Lslash");
    e[0xE9] = Some("Oslash");
    e[0xEA] = Some("OE");
    e[0xEB] = Some("ordmasculine");
    e[0xF1] = Some("ae");
    e[0xF5] = Some("dotlessi");
    e[0xF8] = Some("lslash");
    e[0xF9] = Some("oslash");
    e[0xFA] = Some("oe");
    e[0xFB] = Some("germandbls");
    e
};

// ---------------------------------------------------------------------------
// WinAnsiEncoding (ISO 32000-1 §D.2)
// ---------------------------------------------------------------------------

/// Windows ANSI (CP1252) encoding: glyph names for codes 0–255.
#[rustfmt::skip]
pub const WIN_ANSI_ENCODING: [Option<&'static str>; 256] = {
    let mut e: [Option<&str>; 256] = [None; 256];
    e[0x20] = Some("space");
    e[0x21] = Some("exclam");
    e[0x22] = Some("quotedbl");
    e[0x23] = Some("numbersign");
    e[0x24] = Some("dollar");
    e[0x25] = Some("percent");
    e[0x26] = Some("ampersand");
    e[0x27] = Some("quotesingle");
    e[0x28] = Some("parenleft");
    e[0x29] = Some("parenright");
    e[0x2A] = Some("asterisk");
    e[0x2B] = Some("plus");
    e[0x2C] = Some("comma");
    e[0x2D] = Some("hyphen");
    e[0x2E] = Some("period");
    e[0x2F] = Some("slash");
    e[0x30] = Some("zero");
    e[0x31] = Some("one");
    e[0x32] = Some("two");
    e[0x33] = Some("three");
    e[0x34] = Some("four");
    e[0x35] = Some("five");
    e[0x36] = Some("six");
    e[0x37] = Some("seven");
    e[0x38] = Some("eight");
    e[0x39] = Some("nine");
    e[0x3A] = Some("colon");
    e[0x3B] = Some("semicolon");
    e[0x3C] = Some("less");
    e[0x3D] = Some("equal");
    e[0x3E] = Some("greater");
    e[0x3F] = Some("question");
    e[0x40] = Some("at");
    e[0x41] = Some("A");
    e[0x42] = Some("B");
    e[0x43] = Some("C");
    e[0x44] = Some("D");
    e[0x45] = Some("E");
    e[0x46] = Some("F");
    e[0x47] = Some("G");
    e[0x48] = Some("H");
    e[0x49] = Some("I");
    e[0x4A] = Some("J");
    e[0x4B] = Some("K");
    e[0x4C] = Some("L");
    e[0x4D] = Some("M");
    e[0x4E] = Some("N");
    e[0x4F] = Some("O");
    e[0x50] = Some("P");
    e[0x51] = Some("Q");
    e[0x52] = Some("R");
    e[0x53] = Some("S");
    e[0x54] = Some("T");
    e[0x55] = Some("U");
    e[0x56] = Some("V");
    e[0x57] = Some("W");
    e[0x58] = Some("X");
    e[0x59] = Some("Y");
    e[0x5A] = Some("Z");
    e[0x5B] = Some("bracketleft");
    e[0x5C] = Some("backslash");
    e[0x5D] = Some("bracketright");
    e[0x5E] = Some("asciicircum");
    e[0x5F] = Some("underscore");
    e[0x60] = Some("grave");
    e[0x61] = Some("a");
    e[0x62] = Some("b");
    e[0x63] = Some("c");
    e[0x64] = Some("d");
    e[0x65] = Some("e");
    e[0x66] = Some("f");
    e[0x67] = Some("g");
    e[0x68] = Some("h");
    e[0x69] = Some("i");
    e[0x6A] = Some("j");
    e[0x6B] = Some("k");
    e[0x6C] = Some("l");
    e[0x6D] = Some("m");
    e[0x6E] = Some("n");
    e[0x6F] = Some("o");
    e[0x70] = Some("p");
    e[0x71] = Some("q");
    e[0x72] = Some("r");
    e[0x73] = Some("s");
    e[0x74] = Some("t");
    e[0x75] = Some("u");
    e[0x76] = Some("v");
    e[0x77] = Some("w");
    e[0x78] = Some("x");
    e[0x79] = Some("y");
    e[0x7A] = Some("z");
    e[0x7B] = Some("braceleft");
    e[0x7C] = Some("bar");
    e[0x7D] = Some("braceright");
    e[0x7E] = Some("asciitilde");
    e[0x80] = Some("Euro");
    e[0x82] = Some("quotesinglbase");
    e[0x83] = Some("florin");
    e[0x84] = Some("quotedblbase");
    e[0x85] = Some("ellipsis");
    e[0x86] = Some("dagger");
    e[0x87] = Some("daggerdbl");
    e[0x88] = Some("circumflex");
    e[0x89] = Some("perthousand");
    e[0x8A] = Some("Scaron");
    e[0x8B] = Some("guilsinglleft");
    e[0x8C] = Some("OE");
    e[0x8E] = Some("Zcaron");
    e[0x91] = Some("quoteleft");
    e[0x92] = Some("quoteright");
    e[0x93] = Some("quotedblleft");
    e[0x94] = Some("quotedblright");
    e[0x95] = Some("bullet");
    e[0x96] = Some("endash");
    e[0x97] = Some("emdash");
    e[0x98] = Some("tilde");
    e[0x99] = Some("trademark");
    e[0x9A] = Some("scaron");
    e[0x9B] = Some("guilsinglright");
    e[0x9C] = Some("oe");
    e[0x9E] = Some("zcaron");
    e[0x9F] = Some("Ydieresis");
    e[0xA0] = Some("space");
    e[0xA1] = Some("exclamdown");
    e[0xA2] = Some("cent");
    e[0xA3] = Some("sterling");
    e[0xA4] = Some("currency");
    e[0xA5] = Some("yen");
    e[0xA6] = Some("brokenbar");
    e[0xA7] = Some("section");
    e[0xA8] = Some("dieresis");
    e[0xA9] = Some("copyright");
    e[0xAA] = Some("ordfeminine");
    e[0xAB] = Some("guillemotleft");
    e[0xAC] = Some("logicalnot");
    e[0xAD] = Some("hyphen");
    e[0xAE] = Some("registered");
    e[0xAF] = Some("macron");
    e[0xB0] = Some("degree");
    e[0xB1] = Some("plusminus");
    e[0xB2] = Some("twosuperior");
    e[0xB3] = Some("threesuperior");
    e[0xB4] = Some("acute");
    e[0xB5] = Some("mu");
    e[0xB6] = Some("paragraph");
    e[0xB7] = Some("periodcentered");
    e[0xB8] = Some("cedilla");
    e[0xB9] = Some("onesuperior");
    e[0xBA] = Some("ordmasculine");
    e[0xBB] = Some("guillemotright");
    e[0xBC] = Some("onequarter");
    e[0xBD] = Some("onehalf");
    e[0xBE] = Some("threequarters");
    e[0xBF] = Some("questiondown");
    e[0xC0] = Some("Agrave");
    e[0xC1] = Some("Aacute");
    e[0xC2] = Some("Acircumflex");
    e[0xC3] = Some("Atilde");
    e[0xC4] = Some("Adieresis");
    e[0xC5] = Some("Aring");
    e[0xC6] = Some("AE");
    e[0xC7] = Some("Ccedilla");
    e[0xC8] = Some("Egrave");
    e[0xC9] = Some("Eacute");
    e[0xCA] = Some("Ecircumflex");
    e[0xCB] = Some("Edieresis");
    e[0xCC] = Some("Igrave");
    e[0xCD] = Some("Iacute");
    e[0xCE] = Some("Icircumflex");
    e[0xCF] = Some("Idieresis");
    e[0xD0] = Some("Eth");
    e[0xD1] = Some("Ntilde");
    e[0xD2] = Some("Ograve");
    e[0xD3] = Some("Oacute");
    e[0xD4] = Some("Ocircumflex");
    e[0xD5] = Some("Otilde");
    e[0xD6] = Some("Odieresis");
    e[0xD7] = Some("multiply");
    e[0xD8] = Some("Oslash");
    e[0xD9] = Some("Ugrave");
    e[0xDA] = Some("Uacute");
    e[0xDB] = Some("Ucircumflex");
    e[0xDC] = Some("Udieresis");
    e[0xDD] = Some("Yacute");
    e[0xDE] = Some("Thorn");
    e[0xDF] = Some("germandbls");
    e[0xE0] = Some("agrave");
    e[0xE1] = Some("aacute");
    e[0xE2] = Some("acircumflex");
    e[0xE3] = Some("atilde");
    e[0xE4] = Some("adieresis");
    e[0xE5] = Some("aring");
    e[0xE6] = Some("ae");
    e[0xE7] = Some("ccedilla");
    e[0xE8] = Some("egrave");
    e[0xE9] = Some("eacute");
    e[0xEA] = Some("ecircumflex");
    e[0xEB] = Some("edieresis");
    e[0xEC] = Some("igrave");
    e[0xED] = Some("iacute");
    e[0xEE] = Some("icircumflex");
    e[0xEF] = Some("idieresis");
    e[0xF0] = Some("eth");
    e[0xF1] = Some("ntilde");
    e[0xF2] = Some("ograve");
    e[0xF3] = Some("oacute");
    e[0xF4] = Some("ocircumflex");
    e[0xF5] = Some("otilde");
    e[0xF6] = Some("odieresis");
    e[0xF7] = Some("divide");
    e[0xF8] = Some("oslash");
    e[0xF9] = Some("ugrave");
    e[0xFA] = Some("uacute");
    e[0xFB] = Some("ucircumflex");
    e[0xFC] = Some("udieresis");
    e[0xFD] = Some("yacute");
    e[0xFE] = Some("thorn");
    e[0xFF] = Some("ydieresis");
    e
};

// ---------------------------------------------------------------------------
// MacRomanEncoding (ISO 32000-1 §D.3)
// ---------------------------------------------------------------------------

/// Mac OS Roman encoding: glyph names for codes 0–255.
#[rustfmt::skip]
pub const MAC_ROMAN_ENCODING: [Option<&'static str>; 256] = {
    let mut e: [Option<&str>; 256] = [None; 256];
    e[0x20] = Some("space");
    e[0x21] = Some("exclam");
    e[0x22] = Some("quotedbl");
    e[0x23] = Some("numbersign");
    e[0x24] = Some("dollar");
    e[0x25] = Some("percent");
    e[0x26] = Some("ampersand");
    e[0x27] = Some("quotesingle");
    e[0x28] = Some("parenleft");
    e[0x29] = Some("parenright");
    e[0x2A] = Some("asterisk");
    e[0x2B] = Some("plus");
    e[0x2C] = Some("comma");
    e[0x2D] = Some("hyphen");
    e[0x2E] = Some("period");
    e[0x2F] = Some("slash");
    e[0x30] = Some("zero");
    e[0x31] = Some("one");
    e[0x32] = Some("two");
    e[0x33] = Some("three");
    e[0x34] = Some("four");
    e[0x35] = Some("five");
    e[0x36] = Some("six");
    e[0x37] = Some("seven");
    e[0x38] = Some("eight");
    e[0x39] = Some("nine");
    e[0x3A] = Some("colon");
    e[0x3B] = Some("semicolon");
    e[0x3C] = Some("less");
    e[0x3D] = Some("equal");
    e[0x3E] = Some("greater");
    e[0x3F] = Some("question");
    e[0x40] = Some("at");
    e[0x41] = Some("A");
    e[0x42] = Some("B");
    e[0x43] = Some("C");
    e[0x44] = Some("D");
    e[0x45] = Some("E");
    e[0x46] = Some("F");
    e[0x47] = Some("G");
    e[0x48] = Some("H");
    e[0x49] = Some("I");
    e[0x4A] = Some("J");
    e[0x4B] = Some("K");
    e[0x4C] = Some("L");
    e[0x4D] = Some("M");
    e[0x4E] = Some("N");
    e[0x4F] = Some("O");
    e[0x50] = Some("P");
    e[0x51] = Some("Q");
    e[0x52] = Some("R");
    e[0x53] = Some("S");
    e[0x54] = Some("T");
    e[0x55] = Some("U");
    e[0x56] = Some("V");
    e[0x57] = Some("W");
    e[0x58] = Some("X");
    e[0x59] = Some("Y");
    e[0x5A] = Some("Z");
    e[0x5B] = Some("bracketleft");
    e[0x5C] = Some("backslash");
    e[0x5D] = Some("bracketright");
    e[0x5E] = Some("asciicircum");
    e[0x5F] = Some("underscore");
    e[0x60] = Some("grave");
    e[0x61] = Some("a");
    e[0x62] = Some("b");
    e[0x63] = Some("c");
    e[0x64] = Some("d");
    e[0x65] = Some("e");
    e[0x66] = Some("f");
    e[0x67] = Some("g");
    e[0x68] = Some("h");
    e[0x69] = Some("i");
    e[0x6A] = Some("j");
    e[0x6B] = Some("k");
    e[0x6C] = Some("l");
    e[0x6D] = Some("m");
    e[0x6E] = Some("n");
    e[0x6F] = Some("o");
    e[0x70] = Some("p");
    e[0x71] = Some("q");
    e[0x72] = Some("r");
    e[0x73] = Some("s");
    e[0x74] = Some("t");
    e[0x75] = Some("u");
    e[0x76] = Some("v");
    e[0x77] = Some("w");
    e[0x78] = Some("x");
    e[0x79] = Some("y");
    e[0x7A] = Some("z");
    e[0x7B] = Some("braceleft");
    e[0x7C] = Some("bar");
    e[0x7D] = Some("braceright");
    e[0x7E] = Some("asciitilde");
    e[0x80] = Some("Adieresis");
    e[0x81] = Some("Aring");
    e[0x82] = Some("Ccedilla");
    e[0x83] = Some("Eacute");
    e[0x84] = Some("Ntilde");
    e[0x85] = Some("Odieresis");
    e[0x86] = Some("Udieresis");
    e[0x87] = Some("aacute");
    e[0x88] = Some("agrave");
    e[0x89] = Some("acircumflex");
    e[0x8A] = Some("adieresis");
    e[0x8B] = Some("atilde");
    e[0x8C] = Some("aring");
    e[0x8D] = Some("ccedilla");
    e[0x8E] = Some("eacute");
    e[0x8F] = Some("egrave");
    e[0x90] = Some("ecircumflex");
    e[0x91] = Some("edieresis");
    e[0x92] = Some("iacute");
    e[0x93] = Some("igrave");
    e[0x94] = Some("icircumflex");
    e[0x95] = Some("idieresis");
    e[0x96] = Some("ntilde");
    e[0x97] = Some("oacute");
    e[0x98] = Some("ograve");
    e[0x99] = Some("ocircumflex");
    e[0x9A] = Some("odieresis");
    e[0x9B] = Some("otilde");
    e[0x9C] = Some("uacute");
    e[0x9D] = Some("ugrave");
    e[0x9E] = Some("ucircumflex");
    e[0x9F] = Some("udieresis");
    e[0xA0] = Some("dagger");
    e[0xA1] = Some("degree");
    e[0xA2] = Some("cent");
    e[0xA3] = Some("sterling");
    e[0xA4] = Some("section");
    e[0xA5] = Some("bullet");
    e[0xA6] = Some("paragraph");
    e[0xA7] = Some("germandbls");
    e[0xA8] = Some("registered");
    e[0xA9] = Some("copyright");
    e[0xAA] = Some("trademark");
    e[0xAB] = Some("acute");
    e[0xAC] = Some("dieresis");
    e[0xAE] = Some("AE");
    e[0xAF] = Some("Oslash");
    e[0xB1] = Some("plusminus");
    e[0xB4] = Some("yen");
    e[0xB5] = Some("mu");
    e[0xBB] = Some("ordfeminine");
    e[0xBC] = Some("ordmasculine");
    e[0xC0] = Some("questiondown");
    e[0xC1] = Some("exclamdown");
    e[0xC2] = Some("logicalnot");
    e[0xC4] = Some("florin");
    e[0xC7] = Some("guillemotleft");
    e[0xC8] = Some("guillemotright");
    e[0xC9] = Some("ellipsis");
    e[0xCA] = Some("space");
    e[0xCB] = Some("Agrave");
    e[0xCC] = Some("Atilde");
    e[0xCD] = Some("Otilde");
    e[0xCE] = Some("OE");
    e[0xCF] = Some("oe");
    e[0xD0] = Some("endash");
    e[0xD1] = Some("emdash");
    e[0xD2] = Some("quotedblleft");
    e[0xD3] = Some("quotedblright");
    e[0xD4] = Some("quoteleft");
    e[0xD5] = Some("quoteright");
    e[0xD6] = Some("divide");
    e[0xD8] = Some("ydieresis");
    e[0xD9] = Some("Ydieresis");
    e[0xDA] = Some("fraction");
    e[0xDB] = Some("currency");
    e[0xDC] = Some("guilsinglleft");
    e[0xDD] = Some("guilsinglright");
    e[0xDE] = Some("fi");
    e[0xDF] = Some("fl");
    e[0xE0] = Some("daggerdbl");
    e[0xE1] = Some("periodcentered");
    e[0xE2] = Some("quotesinglbase");
    e[0xE3] = Some("quotedblbase");
    e[0xE4] = Some("perthousand");
    e[0xE5] = Some("Acircumflex");
    e[0xE6] = Some("Ecircumflex");
    e[0xE7] = Some("Aacute");
    e[0xE8] = Some("Edieresis");
    e[0xE9] = Some("Egrave");
    e[0xEA] = Some("Iacute");
    e[0xEB] = Some("Icircumflex");
    e[0xEC] = Some("Idieresis");
    e[0xED] = Some("Igrave");
    e[0xEE] = Some("Oacute");
    e[0xEF] = Some("Ocircumflex");
    e[0xF1] = Some("Ograve");
    e[0xF2] = Some("Uacute");
    e[0xF3] = Some("Ucircumflex");
    e[0xF4] = Some("Ugrave");
    e[0xF5] = Some("dotlessi");
    e[0xF6] = Some("circumflex");
    e[0xF7] = Some("tilde");
    e[0xF8] = Some("macron");
    e[0xF9] = Some("breve");
    e[0xFA] = Some("dotaccent");
    e[0xFB] = Some("ring");
    e[0xFC] = Some("cedilla");
    e[0xFD] = Some("hungarumlaut");
    e[0xFE] = Some("ogonek");
    e[0xFF] = Some("caron");
    e
};

// ---------------------------------------------------------------------------
// MacExpertEncoding (ISO 32000-1 §D.4)
// ---------------------------------------------------------------------------

/// Adobe MacExpertEncoding: glyph names for codes 0–255.
#[rustfmt::skip]
pub const MAC_EXPERT_ENCODING: [Option<&'static str>; 256] = {
    let mut e: [Option<&str>; 256] = [None; 256];
    e[0x20] = Some("space");
    e[0x21] = Some("exclamsmall");
    e[0x22] = Some("Hungarumlautsmall");
    e[0x24] = Some("dollaroldstyle");
    e[0x26] = Some("ampersandsmall");
    e[0x27] = Some("Acutesmall");
    e[0x28] = Some("parenleftsuperior");
    e[0x29] = Some("parenrightsuperior");
    e[0x2A] = Some("twodotenleader");
    e[0x2B] = Some("onedotenleader");
    e[0x2C] = Some("comma");
    e[0x2D] = Some("hyphen");
    e[0x2E] = Some("period");
    e[0x2F] = Some("fraction");
    e[0x30] = Some("zerooldstyle");
    e[0x31] = Some("oneoldstyle");
    e[0x32] = Some("twooldstyle");
    e[0x33] = Some("threeoldstyle");
    e[0x34] = Some("fouroldstyle");
    e[0x35] = Some("fiveoldstyle");
    e[0x36] = Some("sixoldstyle");
    e[0x37] = Some("sevenoldstyle");
    e[0x38] = Some("eightoldstyle");
    e[0x39] = Some("nineoldstyle");
    e[0x3A] = Some("colon");
    e[0x3B] = Some("semicolon");
    e[0x3C] = Some("commasuperior");
    e[0x3D] = Some("threequartersemdash");
    e[0x3E] = Some("periodsuperior");
    e[0x3F] = Some("questionsmall");
    e[0x44] = Some("Ethsmall");
    e[0x5B] = Some("bracketleftsuperior");
    e[0x5D] = Some("bracketrightsuperior");
    e[0x5E] = Some("asuperior");
    e[0x5F] = Some("bsuperior");
    e[0x60] = Some("centsuperior");
    e[0x61] = Some("dsuperior");
    e[0x62] = Some("esuperior");
    e[0x64] = Some("isuperior");
    e[0x66] = Some("lsuperior");
    e[0x67] = Some("msuperior");
    e[0x68] = Some("nsuperior");
    e[0x69] = Some("osuperior");
    e[0x6B] = Some("rsuperior");
    e[0x6C] = Some("ssuperior");
    e[0x6D] = Some("tsuperior");
    e[0x6F] = Some("ff");
    e[0x70] = Some("fi");
    e[0x71] = Some("fl");
    e[0x72] = Some("ffi");
    e[0x73] = Some("ffl");
    e[0x74] = Some("parenleftinferior");
    e[0x76] = Some("parenrightinferior");
    e[0x78] = Some("Circumflexsmall");
    e[0x79] = Some("hyphensuperior");
    e[0x7A] = Some("Gravesmall");
    e[0x7B] = Some("Asmall");
    e[0x7C] = Some("Bsmall");
    e[0x7D] = Some("Csmall");
    e[0x7E] = Some("Dsmall");
    e[0x7F] = Some("Esmall");
    e[0x80] = Some("Fsmall");
    e[0x81] = Some("Gsmall");
    e[0x82] = Some("Hsmall");
    e[0x83] = Some("Ismall");
    e[0x84] = Some("Jsmall");
    e[0x85] = Some("Ksmall");
    e[0x86] = Some("Lsmall");
    e[0x87] = Some("Msmall");
    e[0x88] = Some("Nsmall");
    e[0x89] = Some("Osmall");
    e[0x8A] = Some("Psmall");
    e[0x8B] = Some("Qsmall");
    e[0x8C] = Some("Rsmall");
    e[0x8D] = Some("Ssmall");
    e[0x8E] = Some("Tsmall");
    e[0x8F] = Some("Usmall");
    e[0x90] = Some("Vsmall");
    e[0x91] = Some("Wsmall");
    e[0x92] = Some("Xsmall");
    e[0x93] = Some("Ysmall");
    e[0x94] = Some("Zsmall");
    e[0x95] = Some("colonmonetary");
    e[0x96] = Some("onefitted");
    e[0x97] = Some("rupiah");
    e[0x98] = Some("Tildesmall");
    e[0x9A] = Some("asupperior");
    e[0x9B] = Some("centsuperior");
    e[0xA1] = Some("Agravesmall");
    e[0xA2] = Some("Adieresissmall");
    e[0xA3] = Some("Acircumflexsmall");
    e[0xA4] = Some("Aacutesmall");
    e[0xA5] = Some("Atildesmall");
    e[0xA6] = Some("Aringsmall");
    e[0xA7] = Some("Ccedillasmall");
    e[0xA8] = Some("Aborevesmall");
    e[0xA9] = Some("Eacutesmall");
    e[0xAA] = Some("Egravesmall");
    e[0xAB] = Some("Ecircumflexsmall");
    e[0xAC] = Some("Edieresissmall");
    e[0xAD] = Some("Iacutesmall");
    e[0xAE] = Some("Igravesmall");
    e[0xAF] = Some("Icircumflexsmall");
    e[0xB0] = Some("Idieresissmall");
    e[0xB1] = Some("Ntildesmall");
    e[0xB2] = Some("Oacutesmall");
    e[0xB3] = Some("Ogravesmall");
    e[0xB4] = Some("Ocircumflexsmall");
    e[0xB5] = Some("Odieresissmall");
    e[0xB6] = Some("Otildesmall");
    e[0xB7] = Some("Uacutesmall");
    e[0xB8] = Some("Ugravesmall");
    e[0xB9] = Some("Ucircumflexsmall");
    e[0xBA] = Some("Udieresissmall");
    e[0xBD] = Some("eightsuperior");
    e[0xBE] = Some("fourinferior");
    e[0xBF] = Some("threeinferior");
    e[0xC0] = Some("sixinferior");
    e[0xC1] = Some("eightinferior");
    e[0xC2] = Some("seveninferior");
    e[0xC3] = Some("Scaronsmall");
    e[0xC6] = Some("centoldstyle");
    e[0xCF] = Some("Diaborevesmall");
    e[0xD0] = Some("figuredash");
    e[0xD1] = Some("hypheninferior");
    e[0xD4] = Some("Ogoneksmall");
    e[0xD5] = Some("Rcaronsmall");
    e[0xD6] = Some("Scaronsmall");
    e[0xD7] = Some("Tcaronsmall");
    e[0xD8] = Some("Yacutesmall");
    e[0xD9] = Some("Zcaronsmall");
    e[0xDA] = Some("aborevesmall");
    e[0xDB] = Some("ocaronsmall");
    e[0xDC] = Some("Scaronsmall");
    e[0xDD] = Some("Tcaronsmall");
    e[0xDE] = Some("Zcaronsmall");
    e[0xE5] = Some("laborevesmall");
    e[0xF1] = Some("Aringsmall");
    e[0xF5] = Some("dotlessi");
    e[0xF8] = Some("lslash");
    e[0xF9] = Some("oslash");
    e[0xFA] = Some("oe");
    e[0xFB] = Some("germandbls");
    e
};

// ---------------------------------------------------------------------------
// Adobe Glyph List (AGL) — glyph name to Unicode codepoint mapping
// Subset covering all glyphs used in the standard encodings plus common extras.
// ---------------------------------------------------------------------------

/// AGL entries: (glyph_name, unicode_codepoint).
#[rustfmt::skip]
static AGL_ENTRIES: &[(&str, u32)] = &[
    ("A", 0x0041),
    ("AE", 0x00C6),
    ("Aacute", 0x00C1),
    ("Acircumflex", 0x00C2),
    ("Adieresis", 0x00C4),
    ("Agrave", 0x00C0),
    ("Aring", 0x00C5),
    ("Atilde", 0x00C3),
    ("B", 0x0042),
    ("C", 0x0043),
    ("Ccedilla", 0x00C7),
    ("D", 0x0044),
    ("E", 0x0045),
    ("Eacute", 0x00C9),
    ("Ecircumflex", 0x00CA),
    ("Edieresis", 0x00CB),
    ("Egrave", 0x00C8),
    ("Eth", 0x00D0),
    ("Euro", 0x20AC),
    ("F", 0x0046),
    ("G", 0x0047),
    ("H", 0x0048),
    ("I", 0x0049),
    ("Iacute", 0x00CD),
    ("Icircumflex", 0x00CE),
    ("Idieresis", 0x00CF),
    ("Igrave", 0x00CC),
    ("J", 0x004A),
    ("K", 0x004B),
    ("L", 0x004C),
    ("Lslash", 0x0141),
    ("M", 0x004D),
    ("N", 0x004E),
    ("Ntilde", 0x00D1),
    ("O", 0x004F),
    ("OE", 0x0152),
    ("Oacute", 0x00D3),
    ("Ocircumflex", 0x00D4),
    ("Odieresis", 0x00D6),
    ("Ograve", 0x00D2),
    ("Oslash", 0x00D8),
    ("Otilde", 0x00D5),
    ("P", 0x0050),
    ("Q", 0x0051),
    ("R", 0x0052),
    ("S", 0x0053),
    ("Scaron", 0x0160),
    ("T", 0x0054),
    ("Thorn", 0x00DE),
    ("U", 0x0055),
    ("Uacute", 0x00DA),
    ("Ucircumflex", 0x00DB),
    ("Udieresis", 0x00DC),
    ("Ugrave", 0x00D9),
    ("V", 0x0056),
    ("W", 0x0057),
    ("X", 0x0058),
    ("Y", 0x0059),
    ("Yacute", 0x00DD),
    ("Ydieresis", 0x0178),
    ("Z", 0x005A),
    ("Zcaron", 0x017D),
    ("a", 0x0061),
    ("aacute", 0x00E1),
    ("acircumflex", 0x00E2),
    ("acute", 0x00B4),
    ("adieresis", 0x00E4),
    ("ae", 0x00E6),
    ("agrave", 0x00E0),
    ("ampersand", 0x0026),
    ("aring", 0x00E5),
    ("asciicircum", 0x005E),
    ("asciitilde", 0x007E),
    ("asterisk", 0x002A),
    ("at", 0x0040),
    ("atilde", 0x00E3),
    ("b", 0x0062),
    ("backslash", 0x005C),
    ("bar", 0x007C),
    ("braceleft", 0x007B),
    ("braceright", 0x007D),
    ("bracketleft", 0x005B),
    ("bracketright", 0x005D),
    ("breve", 0x02D8),
    ("brokenbar", 0x00A6),
    ("bullet", 0x2022),
    ("c", 0x0063),
    ("caron", 0x02C7),
    ("ccedilla", 0x00E7),
    ("cedilla", 0x00B8),
    ("cent", 0x00A2),
    ("circumflex", 0x02C6),
    ("colon", 0x003A),
    ("comma", 0x002C),
    ("copyright", 0x00A9),
    ("currency", 0x00A4),
    ("d", 0x0064),
    ("dagger", 0x2020),
    ("daggerdbl", 0x2021),
    ("degree", 0x00B0),
    ("dieresis", 0x00A8),
    ("divide", 0x00F7),
    ("dollar", 0x0024),
    ("dotaccent", 0x02D9),
    ("dotlessi", 0x0131),
    ("e", 0x0065),
    ("eacute", 0x00E9),
    ("ecircumflex", 0x00EA),
    ("edieresis", 0x00EB),
    ("egrave", 0x00E8),
    ("eight", 0x0038),
    ("ellipsis", 0x2026),
    ("emdash", 0x2014),
    ("endash", 0x2013),
    ("equal", 0x003D),
    ("eth", 0x00F0),
    ("exclam", 0x0021),
    ("exclamdown", 0x00A1),
    ("f", 0x0066),
    ("fi", 0xFB01),
    ("five", 0x0035),
    ("fl", 0xFB02),
    ("florin", 0x0192),
    ("four", 0x0034),
    ("fraction", 0x2044),
    ("g", 0x0067),
    ("germandbls", 0x00DF),
    ("grave", 0x0060),
    ("greater", 0x003E),
    ("guillemotleft", 0x00AB),
    ("guillemotright", 0x00BB),
    ("guilsinglleft", 0x2039),
    ("guilsinglright", 0x203A),
    ("h", 0x0068),
    ("hungarumlaut", 0x02DD),
    ("hyphen", 0x002D),
    ("i", 0x0069),
    ("iacute", 0x00ED),
    ("icircumflex", 0x00EE),
    ("idieresis", 0x00EF),
    ("igrave", 0x00EC),
    ("j", 0x006A),
    ("k", 0x006B),
    ("l", 0x006C),
    ("less", 0x003C),
    ("logicalnot", 0x00AC),
    ("lslash", 0x0142),
    ("m", 0x006D),
    ("macron", 0x00AF),
    ("minus", 0x2212),
    ("mu", 0x00B5),
    ("multiply", 0x00D7),
    ("n", 0x006E),
    ("nine", 0x0039),
    ("ntilde", 0x00F1),
    ("numbersign", 0x0023),
    ("o", 0x006F),
    ("oacute", 0x00F3),
    ("ocircumflex", 0x00F4),
    ("odieresis", 0x00F6),
    ("oe", 0x0153),
    ("ogonek", 0x02DB),
    ("ograve", 0x00F2),
    ("one", 0x0031),
    ("onehalf", 0x00BD),
    ("onequarter", 0x00BC),
    ("onesuperior", 0x00B9),
    ("ordfeminine", 0x00AA),
    ("ordmasculine", 0x00BA),
    ("oslash", 0x00F8),
    ("otilde", 0x00F5),
    ("p", 0x0070),
    ("paragraph", 0x00B6),
    ("parenleft", 0x0028),
    ("parenright", 0x0029),
    ("percent", 0x0025),
    ("period", 0x002E),
    ("periodcentered", 0x00B7),
    ("perthousand", 0x2030),
    ("plus", 0x002B),
    ("plusminus", 0x00B1),
    ("q", 0x0071),
    ("question", 0x003F),
    ("questiondown", 0x00BF),
    ("quotedbl", 0x0022),
    ("quotedblbase", 0x201E),
    ("quotedblleft", 0x201C),
    ("quotedblright", 0x201D),
    ("quoteleft", 0x2018),
    ("quoteright", 0x2019),
    ("quotesinglbase", 0x201A),
    ("quotesingle", 0x0027),
    ("r", 0x0072),
    ("registered", 0x00AE),
    ("ring", 0x02DA),
    ("s", 0x0073),
    ("scaron", 0x0161),
    ("section", 0x00A7),
    ("semicolon", 0x003B),
    ("seven", 0x0037),
    ("six", 0x0036),
    ("slash", 0x002F),
    ("space", 0x0020),
    ("sterling", 0x00A3),
    ("t", 0x0074),
    ("thorn", 0x00FE),
    ("three", 0x0033),
    ("threequarters", 0x00BE),
    ("threesuperior", 0x00B3),
    ("tilde", 0x02DC),
    ("trademark", 0x2122),
    ("two", 0x0032),
    ("twosuperior", 0x00B2),
    ("u", 0x0075),
    ("uacute", 0x00FA),
    ("ucircumflex", 0x00FB),
    ("udieresis", 0x00FC),
    ("ugrave", 0x00F9),
    ("underscore", 0x005F),
    ("v", 0x0076),
    ("w", 0x0077),
    ("x", 0x0078),
    ("y", 0x0079),
    ("yacute", 0x00FD),
    ("ydieresis", 0x00FF),
    ("yen", 0x00A5),
    ("z", 0x007A),
    ("zcaron", 0x017E),
    ("zero", 0x0030),
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agl_lookup_basic() {
        assert_eq!(agl_lookup("A"), Some('A'));
        assert_eq!(agl_lookup("space"), Some(' '));
        assert_eq!(agl_lookup("emdash"), Some('\u{2014}'));
        assert_eq!(agl_lookup("fi"), Some('\u{FB01}'));
        assert_eq!(agl_lookup("Euro"), Some('\u{20AC}'));
        assert_eq!(agl_lookup("nonexistent"), None);
    }

    #[test]
    fn test_agl_lookup_uni_form() {
        assert_eq!(agl_lookup("uni0041"), Some('A'));
        assert_eq!(agl_lookup("uni20AC"), Some('\u{20AC}'));
        assert_eq!(agl_lookup("uni0000"), Some('\0'));
        // Invalid: not exactly 4 hex digits
        assert_eq!(agl_lookup("uni041"), None);
        assert_eq!(agl_lookup("uni00411"), None);
    }

    #[test]
    fn test_agl_lookup_u_form() {
        assert_eq!(agl_lookup("u0041"), Some('A'));
        assert_eq!(agl_lookup("u20AC"), Some('\u{20AC}'));
        assert_eq!(agl_lookup("u1F600"), Some('\u{1F600}'));
        // Invalid: too short
        assert_eq!(agl_lookup("u041"), None);
    }

    #[test]
    fn test_standard_encoding_ascii() {
        let enc = Encoding::from_base(BaseEncoding::Standard);
        assert_eq!(enc.decode_char(0x41), Some('A'));
        assert_eq!(enc.decode_char(0x61), Some('a'));
        assert_eq!(enc.decode_char(0x20), Some(' '));
        assert_eq!(enc.decode_char(0x30), Some('0'));
    }

    #[test]
    fn test_standard_encoding_special() {
        let enc = Encoding::from_base(BaseEncoding::Standard);
        assert_eq!(enc.decode_char(0xAE), Some('\u{FB01}')); // fi ligature
        assert_eq!(enc.decode_char(0xD0), Some('\u{2014}')); // emdash
        assert_eq!(enc.decode_char(0xB7), Some('\u{2022}')); // bullet
    }

    #[test]
    fn test_winansi_encoding() {
        let enc = Encoding::from_base(BaseEncoding::WinAnsi);
        assert_eq!(enc.decode_char(0x41), Some('A'));
        assert_eq!(enc.decode_char(0x80), Some('\u{20AC}')); // Euro
        assert_eq!(enc.decode_char(0x93), Some('\u{201C}')); // left double quote
        assert_eq!(enc.decode_char(0xE9), Some('\u{00E9}')); // eacute
    }

    #[test]
    fn test_macroman_encoding() {
        let enc = Encoding::from_base(BaseEncoding::MacRoman);
        assert_eq!(enc.decode_char(0x41), Some('A'));
        assert_eq!(enc.decode_char(0x80), Some('\u{00C4}')); // Adieresis
        assert_eq!(enc.decode_char(0x83), Some('\u{00C9}')); // Eacute
    }

    #[test]
    fn test_decode_bytes() {
        let enc = Encoding::from_base(BaseEncoding::WinAnsi);
        let result = enc.decode_bytes(&[0x48, 0x65, 0x6C, 0x6C, 0x6F]);
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_decode_bytes_with_notdef() {
        let enc = Encoding::from_base(BaseEncoding::Standard);
        // Code 0x00 is .notdef in StandardEncoding
        let result = enc.decode_bytes(&[0x00, 0x41]);
        assert_eq!(result, "\u{FFFD}A");
    }

    #[test]
    fn test_apply_differences() {
        let mut enc = Encoding::from_base(BaseEncoding::WinAnsi);
        enc.apply_differences(&[(0x41, "germandbls")]);
        // Now code 0x41 maps to germandbls (ß) instead of A
        assert_eq!(enc.decode_char(0x41), Some('\u{00DF}'));
    }

    #[test]
    fn test_base_encoding_from_name() {
        assert_eq!(
            BaseEncoding::from_name("WinAnsiEncoding"),
            Some(BaseEncoding::WinAnsi)
        );
        assert_eq!(
            BaseEncoding::from_name("MacRomanEncoding"),
            Some(BaseEncoding::MacRoman)
        );
        assert_eq!(
            BaseEncoding::from_name("StandardEncoding"),
            Some(BaseEncoding::Standard)
        );
        assert_eq!(BaseEncoding::from_name("Unknown"), None);
    }

    #[test]
    fn test_empty_encoding() {
        let enc = Encoding::empty();
        assert_eq!(enc.base_encoding, None);
        for code in 0..=255u8 {
            assert_eq!(enc.decode_char(code), None);
        }
    }
}
