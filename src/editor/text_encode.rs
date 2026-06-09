//! Re-encode edited Unicode text into a PDF font's own show-string bytes.
//!
//! The edit engine works in Unicode, but a content-stream show operator carries
//! *font codes* (1-byte for simple fonts, 2-byte CIDs for composite/Type0). To
//! write an edit back — or to render it with the embedded font in the preview —
//! the typed text must be mapped back to those codes:
//!
//! - **Simple font:** invert the font's Encoding/Differences/AGL via
//!   [`PdfFontMetrics::code_for_char`] → one byte per char.
//! - **Composite (Type0/CID):** invert the ToUnicode CMap
//!   ([`CMap::unicode_to_code`]) → two big-endian bytes per char (the inverse of
//!   how the renderer decodes composite show-strings).
//!
//! Characters with no code in the font are reported in [`EncodeResult::missing`]
//! rather than silently mis-encoded; the caller decides whether to fall back
//! (sibling font / subset embed — write-back Tiers 2/3) or keep the original.

use crate::content::interpreter::resolve_font_info;
use crate::document::page::Page;
use crate::editor::text_shape::PdfFontMetrics;
use crate::fonts::truetype::TrueTypeFont;
use crate::parser::objects::{PdfDocument, PdfObject};

/// Outcome of encoding text into a font's codes.
#[derive(Debug, Clone, Default)]
pub struct EncodeResult {
    /// Show-string bytes for the characters that *could* be encoded, in order.
    pub bytes: Vec<u8>,
    /// Characters with no code in this font, in first-seen order (deduplicated).
    pub missing: Vec<char>,
}

impl EncodeResult {
    /// Whether every character was encodable (nothing missing).
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Encode `text` into the show-string bytes for the font under `font_key` on
/// `page`, so the renderer/writer uses the embedded font.
///
/// Always returns the bytes for the encodable characters plus the list of
/// characters that have no code in the font (see [`EncodeResult`]). A missing
/// character contributes **no** bytes — callers that need byte↔char alignment
/// should check `missing` is empty first.
pub fn encode_in_font(
    doc: &PdfDocument,
    page: &Page,
    font_key: &str,
    font_size: f64,
    text: &str,
) -> EncodeResult {
    let (cmap, is_composite, _widths) =
        resolve_font_info(font_key, Some(doc), Some(&page.resources.raw));

    let mut out = EncodeResult::default();
    let note_missing = |ch: char, missing: &mut Vec<char>| {
        if !missing.contains(&ch) {
            missing.push(ch);
        }
    };

    if is_composite {
        // unicode → 2-byte CID. Primary: invert the ToUnicode CMap (covers chars
        // the document already uses). Tier-2 fallback: for an Identity-H font
        // (CID == GID), recover a glyph straight from the **embedded font
        // program**'s cmap — it usually contains far more glyphs than the subset's
        // ToUnicode exposes, so many "missing" chars encode with zero new bytes
        // and the original embedded face.
        let rev = cmap.as_ref().map(|cm| cm.unicode_to_code());
        let identity = is_identity_h(doc, &page.resources.raw, font_key);
        let program = if identity {
            embedded_truetype_program(doc, &page.resources.raw, font_key)
        } else {
            None
        };

        out.bytes.reserve(text.chars().count() * 2);
        for ch in text.chars() {
            // 1) ToUnicode reverse map.
            let mut code = rev
                .as_ref()
                .and_then(|r| r.get(&ch.to_string()).copied())
                .filter(|&c| c <= 0xFFFF);
            // 2) Identity-H: embedded program cmap (unicode → GID == CID).
            if code.is_none() {
                if let Some(ttf) = program.as_ref() {
                    if let Some(gid) = ttf.glyph_id(ch as u32) {
                        if gid != 0 {
                            code = Some(gid as u32);
                        }
                    }
                }
            }
            match code {
                Some(c) => {
                    out.bytes.push((c >> 8) as u8);
                    out.bytes.push((c & 0xFF) as u8);
                }
                None => note_missing(ch, &mut out.missing),
            }
        }
    } else {
        // Simple font: unicode → 1-byte code via the metrics reverse table.
        let metrics = font_metrics(doc, page, font_key, font_size);
        out.bytes.reserve(text.chars().count());
        for ch in text.chars() {
            match metrics.as_ref().and_then(|m| m.code_for_char(ch)) {
                Some(code) => out.bytes.push(code),
                None => note_missing(ch, &mut out.missing),
            }
        }
    }

    out
}

/// Resolve the `/Font/<key>` dictionary from a resources dict.
fn font_dict(
    doc: &PdfDocument,
    resources: &crate::parser::objects::PdfDict,
    key: &str,
) -> Option<crate::parser::objects::PdfDict> {
    let fonts = match resources.get("Font") {
        Some(PdfObject::Dictionary(d)) => d,
        _ => return None,
    };
    let font_ref = fonts.get(key)?;
    match doc.resolve(font_ref).ok()? {
        PdfObject::Dictionary(d) => Some(d),
        _ => None,
    }
}

/// Whether the Type0 font under `key` uses Identity-H/V encoding (CID == GID),
/// the prerequisite for recovering glyphs directly from the embedded program.
fn is_identity_h(
    doc: &PdfDocument,
    resources: &crate::parser::objects::PdfDict,
    key: &str,
) -> bool {
    let Some(fd) = font_dict(doc, resources, key) else {
        return false;
    };
    match fd.get("Encoding") {
        Some(PdfObject::Name(n)) => n == "Identity-H" || n == "Identity-V",
        _ => false,
    }
}

/// Decode and parse the embedded TrueType program (`FontFile2`) of the composite
/// font under `key`, for unicode→GID lookup. Returns `None` for CFF-only fonts or
/// when no program is embedded (those fall through to Tier 3 subset embedding).
fn embedded_truetype_program(
    doc: &PdfDocument,
    resources: &crate::parser::objects::PdfDict,
    key: &str,
) -> Option<TrueTypeFont> {
    let fd = font_dict(doc, resources, key)?;
    // Type0 → DescendantFonts[0] → FontDescriptor → FontFile2.
    let desc_font = fd
        .get("DescendantFonts")
        .and_then(|o| match o {
            PdfObject::Array(a) => a.first().cloned(),
            PdfObject::Reference(..) => doc.resolve(o).ok().and_then(|r| match r {
                PdfObject::Array(a) => a.into_iter().next(),
                _ => None,
            }),
            _ => None,
        })
        .and_then(|r| doc.resolve(&r).ok())
        .and_then(|o| match o {
            PdfObject::Dictionary(d) => Some(d),
            _ => None,
        })?;
    let desc = desc_font
        .get("FontDescriptor")
        .and_then(|r| doc.resolve(r).ok())
        .and_then(|o| match o {
            PdfObject::Dictionary(d) => Some(d),
            _ => None,
        })?;
    let ff = desc.get("FontFile2")?;
    // Encrypted PDFs: decrypt the font-program stream before parsing (decrypt →
    // defilter). `get_stream_data` is a no-op vs. `decode_with_doc` when the file
    // is not encrypted, so this is safe for plain PDFs too.
    if let PdfObject::Reference(id, _) = ff {
        if let Ok(data) = doc.get_stream_data(*id) {
            return TrueTypeFont::parse(&data).ok();
        }
    }
    let bytes = match doc.resolve(ff).ok()? {
        PdfObject::Stream(s) => s.decode_with_doc(doc).ok()?,
        _ => return None,
    };
    TrueTypeFont::parse(&bytes).ok()
}

/// Build simple-font metrics for the reverse char→code table (None if the font
/// can't be resolved). Composite fonts don't use this path.
fn font_metrics(
    doc: &PdfDocument,
    page: &Page,
    font_key: &str,
    font_size: f64,
) -> Option<PdfFontMetrics> {
    let (cmap, _is_composite, widths) =
        resolve_font_info(font_key, Some(doc), Some(&page.resources.raw));
    if cmap.is_none() && widths.is_none() {
        return None;
    }
    Some(PdfFontMetrics::from_font_info(&cmap, &widths, font_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_result_complete_when_no_missing() {
        let r = EncodeResult {
            bytes: vec![1, 2, 3],
            missing: vec![],
        };
        assert!(r.is_complete());
    }

    #[test]
    fn encode_result_incomplete_with_missing() {
        let r = EncodeResult {
            bytes: vec![],
            missing: vec!['好'],
        };
        assert!(!r.is_complete());
    }
}
