//! Embed a bundled font as a `Type0` / `CIDFontType2` (Identity-H) object set.
//!
//! Write-back Tier 3: when an edited glyph exists in *neither* the block's
//! ToUnicode map nor its embedded program, we fall back to a full font from the
//! bundled set (`core-fonts/` via the resolver) and embed it as a composite font
//! so the typed glyph is saveable and renders.
//!
//! The font program is embedded **whole** (not glyf/loca subset) with
//! `CIDToGIDMap /Identity` — so the CID *is* the source font's glyph id. This is
//! larger than a true subset but always correct, and is the explicit fallback in
//! the plan. `/W` and `/ToUnicode` are generated for the characters actually used
//! so widths and text-extraction round-trip.

use std::collections::BTreeMap;

use crate::editor::document_editor::PdfEditor;
use crate::error::{PdfError, Result};
use crate::fonts::truetype::TrueTypeFont;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::streams::{make_flate_stream, make_raw_stream};

/// A composite font embedded into the document, ready to encode edited text.
pub struct EmbeddedCidFont {
    /// Object id of the `Type0` font dict — reference this from `/Resources/Font`.
    pub font_id: u32,
    /// Parsed source program, for unicode→GID at encode time.
    ttf: TrueTypeFont,
}

impl EmbeddedCidFont {
    /// Whether `ch` has a real (non-`.notdef`) glyph in the embedded program.
    pub fn can_encode(&self, ch: char) -> bool {
        self.ttf.glyph_id(ch as u32).is_some_and(|g| g != 0)
    }

    /// Iterate over every mapped character and its advance in 1/1000-em units.
    ///
    /// Delegates to [`TrueTypeFont::iter_char_advances_1000`] so callers can build
    /// a [`PdfFontMetrics`](crate::editor::PdfFontMetrics) for this embedded face
    /// without exposing the `ttf` field directly.
    pub fn iter_char_advances_1000(&self) -> impl Iterator<Item = (char, f64)> + '_ {
        self.ttf.iter_char_advances_1000()
    }

    /// Encode `text` into 2-byte Identity-H codes (GID, big-endian).
    ///
    /// Characters with no glyph map to GID 0 (`.notdef`); callers that care should
    /// check [`can_encode`](Self::can_encode) first, but this never panics.
    pub fn encode(&self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.chars().count() * 2);
        for ch in text.chars() {
            let gid = self.ttf.glyph_id(ch as u32).unwrap_or(0);
            out.push((gid >> 8) as u8);
            out.push((gid & 0xFF) as u8);
        }
        out
    }
}

/// Build and register a `Type0`/`CIDFontType2` font embedding `ttf_bytes` whole,
/// with `/W` + `/ToUnicode` covering `chars`.
///
/// Returns an [`EmbeddedCidFont`] whose `font_id` should be added to the page's
/// `/Resources/Font` under a fresh key. `base_name` is the PostScript `/BaseFont`
/// (e.g. a resolved family like `"LiberationSerif"`).
pub fn embed_cidfont_for_chars(
    editor: &mut PdfEditor,
    ttf_bytes: &[u8],
    base_name: &str,
    chars: &[char],
) -> Result<EmbeddedCidFont> {
    let ttf = TrueTypeFont::parse(ttf_bytes)
        .map_err(|e| PdfError::invalid_structure(format!("font_subset: parse failed: {e}")))?;
    let upm = ttf.units_per_em.max(1) as f64;

    // 1. Embed the program bytes as /FontFile2 (flate, with /Length1 = raw size).
    let mut ff_extras: PdfDict = PdfDict::new();
    ff_extras.insert(
        "Length1".to_owned(),
        PdfObject::Integer(ttf_bytes.len() as i64),
    );
    let font_file = make_flate_stream(ttf_bytes, ff_extras)?;
    let font_file_id = editor.add_object(PdfObject::Stream(Box::new(font_file)));

    // 2. FontDescriptor. Metrics are generous defaults in 1000-em space; renderers
    //    use the embedded program's own tables, so exact values here aren't
    //    critical for display, only for layout hints.
    let mut desc: PdfDict = PdfDict::new();
    desc.insert(
        "Type".to_owned(),
        PdfObject::Name("FontDescriptor".to_owned()),
    );
    desc.insert("FontName".to_owned(), PdfObject::Name(base_name.to_owned()));
    desc.insert("Flags".to_owned(), PdfObject::Integer(4)); // Symbolic
    desc.insert(
        "FontBBox".to_owned(),
        PdfObject::Array(vec![
            PdfObject::Integer(-200),
            PdfObject::Integer(-300),
            PdfObject::Integer(1200),
            PdfObject::Integer(1000),
        ]),
    );
    desc.insert("ItalicAngle".to_owned(), PdfObject::Integer(0));
    desc.insert("Ascent".to_owned(), PdfObject::Integer(900));
    desc.insert("Descent".to_owned(), PdfObject::Integer(-200));
    desc.insert("CapHeight".to_owned(), PdfObject::Integer(700));
    desc.insert("StemV".to_owned(), PdfObject::Integer(80));
    desc.insert(
        "FontFile2".to_owned(),
        PdfObject::Reference(font_file_id, 0),
    );
    let desc_id = editor.add_object(PdfObject::Dictionary(desc));

    // 3. Per-GID width array `/W [ gid [w] gid [w] ... ]` for the used glyphs,
    //    in 1000-em units. Dedup + sort by GID for deterministic output.
    let mut gid_width: BTreeMap<u16, i64> = BTreeMap::new();
    for &ch in chars {
        if let Some(gid) = ttf.glyph_id(ch as u32) {
            let w = (ttf.glyph_advance(gid) as f64 * 1000.0 / upm).round() as i64;
            gid_width.insert(gid, w);
        }
    }
    let mut w_array: Vec<PdfObject> = Vec::with_capacity(gid_width.len() * 2);
    for (gid, w) in &gid_width {
        w_array.push(PdfObject::Integer(*gid as i64));
        w_array.push(PdfObject::Array(vec![PdfObject::Integer(*w)]));
    }

    // 4. CIDFontType2 descendant: Identity CID↔GID, the descriptor, widths.
    let mut cidsysinfo: PdfDict = PdfDict::new();
    cidsysinfo.insert("Registry".to_owned(), PdfObject::String(b"Adobe".to_vec()));
    cidsysinfo.insert(
        "Ordering".to_owned(),
        PdfObject::String(b"Identity".to_vec()),
    );
    cidsysinfo.insert("Supplement".to_owned(), PdfObject::Integer(0));

    let mut cid_font: PdfDict = PdfDict::new();
    cid_font.insert("Type".to_owned(), PdfObject::Name("Font".to_owned()));
    cid_font.insert(
        "Subtype".to_owned(),
        PdfObject::Name("CIDFontType2".to_owned()),
    );
    cid_font.insert("BaseFont".to_owned(), PdfObject::Name(base_name.to_owned()));
    cid_font.insert(
        "CIDSystemInfo".to_owned(),
        PdfObject::Dictionary(cidsysinfo),
    );
    cid_font.insert(
        "FontDescriptor".to_owned(),
        PdfObject::Reference(desc_id, 0),
    );
    cid_font.insert(
        "CIDToGIDMap".to_owned(),
        PdfObject::Name("Identity".to_owned()),
    );
    cid_font.insert("DW".to_owned(), PdfObject::Integer(1000));
    if !w_array.is_empty() {
        cid_font.insert("W".to_owned(), PdfObject::Array(w_array));
    }
    let cid_font_id = editor.add_object(PdfObject::Dictionary(cid_font));

    // 5. ToUnicode CMap (GID → unicode) for the used glyphs, so copy/extract works.
    let tounicode = build_tounicode(&ttf, chars);
    let tu_stream = make_raw_stream(tounicode.into_bytes(), PdfDict::new());
    let tu_id = editor.add_object(PdfObject::Stream(Box::new(tu_stream)));

    // 6. Type0 font — the object referenced from page resources.
    let mut type0: PdfDict = PdfDict::new();
    type0.insert("Type".to_owned(), PdfObject::Name("Font".to_owned()));
    type0.insert("Subtype".to_owned(), PdfObject::Name("Type0".to_owned()));
    type0.insert("BaseFont".to_owned(), PdfObject::Name(base_name.to_owned()));
    type0.insert(
        "Encoding".to_owned(),
        PdfObject::Name("Identity-H".to_owned()),
    );
    type0.insert(
        "DescendantFonts".to_owned(),
        PdfObject::Array(vec![PdfObject::Reference(cid_font_id, 0)]),
    );
    type0.insert("ToUnicode".to_owned(), PdfObject::Reference(tu_id, 0));
    let font_id = editor.add_object(PdfObject::Dictionary(type0));

    Ok(EmbeddedCidFont { font_id, ttf })
}

/// Build a ToUnicode CMap mapping each used GID → its Unicode (UTF-16BE hex).
fn build_tounicode(ttf: &TrueTypeFont, chars: &[char]) -> String {
    // GID → unicode (smallest char wins on collision, deterministic).
    let mut gid_uni: BTreeMap<u16, char> = BTreeMap::new();
    for &ch in chars {
        if let Some(gid) = ttf.glyph_id(ch as u32) {
            gid_uni.entry(gid).or_insert(ch);
        }
    }

    let mut s = String::new();
    s.push_str(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );

    // bfchar sections are capped at 100 entries each per the spec.
    let entries: Vec<(u16, char)> = gid_uni.into_iter().collect();
    for chunk in entries.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (gid, ch) in chunk {
            s.push_str(&format!("<{:04X}> <{}>\n", gid, utf16be_hex(*ch)));
        }
        s.push_str("endbfchar\n");
    }

    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

/// Hex of `ch` in UTF-16BE (4 hex for BMP, 8 for a surrogate pair).
fn utf16be_hex(ch: char) -> String {
    let mut buf = [0u16; 2];
    let units = ch.encode_utf16(&mut buf);
    units.iter().map(|u| format!("{:04X}", u)).collect()
}

// The embed test needs a real font; the bundled resolver lives behind `render`.
#[cfg(all(test, feature = "render"))]
mod tests {
    use super::*;
    use crate::render::font_resolver::{EmbeddedFontResolver, FontResolver};

    fn liberation_serif() -> Vec<u8> {
        EmbeddedFontResolver
            .resolve("Times-Roman", false, false)
            .expect("bundled serif font")
    }

    #[test]
    fn utf16be_hex_bmp_and_astral() {
        assert_eq!(utf16be_hex('A'), "0041");
        assert_eq!(utf16be_hex('好'), "597D");
        // U+1F600 → surrogate pair D83D DE00
        assert_eq!(utf16be_hex('\u{1F600}'), "D83DDE00");
    }

    #[test]
    fn embed_builds_type0_font_and_encodes() {
        let bytes = liberation_serif();
        // Minimal editor over a blank doc.
        let blank = include_bytes!("../../tests/fixtures/minimal.pdf");
        let mut editor = PdfEditor::open(blank.to_vec()).expect("open");

        let chars: Vec<char> = "Hello".chars().collect();
        let font =
            embed_cidfont_for_chars(&mut editor, &bytes, "LiberationSerif", &chars).expect("embed");

        // The registered object is a Type0 font with Identity-H + descendant.
        let obj = editor.get_object(font.font_id).expect("font obj");
        let d = obj.as_dict().expect("font dict");
        assert_eq!(d.get("Subtype"), Some(&PdfObject::Name("Type0".to_owned())));
        assert_eq!(
            d.get("Encoding"),
            Some(&PdfObject::Name("Identity-H".to_owned()))
        );
        assert!(d.contains_key("DescendantFonts"));
        assert!(d.contains_key("ToUnicode"));

        // Encoding "Hello" yields 2 bytes per char, all non-zero GIDs.
        assert!(font.can_encode('H'));
        let enc = font.encode("Hello");
        assert_eq!(enc.len(), 10);
        assert_ne!(&enc[0..2], &[0, 0], "H should map to a real glyph");
    }
}
