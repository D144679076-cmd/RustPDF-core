//! Text measurement & layout primitives for Word-style PDF editing.
//!
//! Pure helpers (caret offsets, hit-testing, greedy word-wrap) operate on any
//! [`Measurer`], so the text-edit engine can be unit-tested with a stub metric.
//! [`PdfFontMetrics`] is the real implementation, backed by [`PdfFont`] glyph
//! widths; [`font_metrics_for`] builds one straight from a page's font resource.

use std::collections::HashMap;

use crate::content::interpreter::{resolve_font_info, resolve_font_style, FontStyleInfo};
use crate::document::catalog::{resolve_inherited_attribute, Catalog};
use crate::error::Result;
use crate::fonts::cmap::CMap;
use crate::fonts::types::FontWidths;
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

/// Supplies the advance width of a single Unicode scalar for a fixed font+size.
///
/// Widths are in PDF user-space points (the font size is already applied), so
/// callers can sum them directly to get caret positions in page coordinates.
pub trait Measurer {
    /// Advance width of `ch` in user-space points.
    fn advance(&self, ch: char) -> f64;
}

/// Caret x-offset before each character, in user-space points.
///
/// Returns a vector of length `chars + 1`: element `i` is the distance from the
/// text origin to the left edge of character `i`; the final element is the full
/// advance width of `text`.
pub fn caret_offsets(m: &dyn Measurer, text: &str) -> Vec<f64> {
    let mut offsets = Vec::with_capacity(text.chars().count() + 1);
    let mut acc = 0.0_f64;
    offsets.push(0.0);
    for ch in text.chars() {
        acc += m.advance(ch);
        offsets.push(acc);
    }
    offsets
}

/// Total advance width of `text` in user-space points.
pub fn text_width(m: &dyn Measurer, text: &str) -> f64 {
    text.chars().map(|c| m.advance(c)).sum()
}

/// Caret index (0..=char_count) whose x-offset is closest to local `x`.
///
/// `x` is measured from the text origin in user-space points. Used to place the
/// caret where the user clicked.
pub fn hit_test(m: &dyn Measurer, text: &str, x: f64) -> usize {
    let offsets = caret_offsets(m, text);
    let mut best = 0usize;
    let mut best_d = f64::MAX;
    for (i, &off) in offsets.iter().enumerate() {
        let d = (off - x).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Greedy word-wrap of `text` into character ranges that each fit `max_width`.
///
/// Breaks on ASCII spaces; a single word longer than `max_width` is emitted on
/// its own (over-long) line. Returns inclusive-start / exclusive-end **character**
/// index pairs. With `max_width <= 0` the whole text is one line.
pub fn wrap_lines(m: &dyn Measurer, text: &str, max_width: f64) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    if max_width <= 0.0 || chars.is_empty() {
        return vec![(0, chars.len())];
    }

    let mut lines = Vec::new();
    let mut line_start = 0usize; // char index of current line start
    let mut last_break: Option<usize> = None; // char index just after the last space seen
    let mut width = 0.0_f64;

    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        let w = m.advance(ch);
        if width + w > max_width && i > line_start {
            // Break: prefer the last word boundary; else hard-break here.
            let brk = match last_break {
                Some(b) if b > line_start => b,
                _ => i,
            };
            lines.push((line_start, brk));
            line_start = brk;
            last_break = None;
            // Recompute width of the carried-over remainder.
            width = chars[line_start..i].iter().map(|&c| m.advance(c)).sum();
        }
        width += w;
        if ch == ' ' {
            last_break = Some(i + 1);
        }
        i += 1;
    }
    lines.push((line_start, chars.len()));
    lines
}

/// Glyph metrics for a simple font at a fixed size.
///
/// Precomputes a `char -> advance` table for single-byte codes (0..=255), which
/// covers the standard-14 and simple Latin/WinAnsi fonts targeted in Phase 1.
/// Built from the `(ToUnicode CMap, FontWidths)` pair that
/// [`resolve_font_info`] returns; unknown characters fall back to the default
/// width. Advances are in user-space points (font size already applied).
#[derive(Clone)]
pub struct PdfFontMetrics {
    map: HashMap<char, f64>,
    /// Reverse Encoding: displayed char → 1-byte font code, for re-encoding edited
    /// text into the embedded simple font (smallest code wins on collisions).
    char_code: HashMap<char, u8>,
    default: f64,
}

impl PdfFontMetrics {
    /// Build metrics from a resolved simple-font `(cmap, widths)` pair.
    ///
    /// For each code 0..=255 the displayed character is taken from the ToUnicode
    /// CMap when present, else the Latin-1 identity (`code as char`); its advance
    /// is `widths.get_width(code)` in 1/1000 em scaled by `font_size`.
    pub fn from_font_info(
        cmap: &Option<CMap>,
        widths: &Option<FontWidths>,
        font_size: f64,
    ) -> Self {
        let scale = font_size / 1000.0;
        // Fallback for any code not covered by an explicit `/Widths` entry.
        //
        // `FontWidths::default_width` is hardcoded to 1000 (a full em) for simple
        // fonts and is meant as a CID sentinel — using it here makes every
        // un-listed glyph one em wide, which roughly doubled measured line widths
        // (base-14 fonts usually omit `/Widths` entirely). 0.5 em matches the
        // average advance of Helvetica/Times and keeps boxes sane.
        let fallback = 0.5 * font_size;
        let mut map = HashMap::new();
        let mut char_code: HashMap<char, u8> = HashMap::new();
        for code in 0u32..=255 {
            let ch = match cmap.as_ref().and_then(|c| c.lookup(code)) {
                Some(s) => {
                    let mut it = s.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => c,
                        _ => continue, // multi-char mapping: skip (rare for simple fonts)
                    }
                }
                None => match char::from_u32(code) {
                    Some(c) => c,
                    None => continue,
                },
            };
            // Use a real `/Widths` entry only when this code is inside the
            // declared range and has a non-zero advance; otherwise fall back.
            let w = match widths {
                Some(fw)
                    if !fw.widths.is_empty() && code >= fw.first_char && code <= fw.last_char =>
                {
                    let idx = (code - fw.first_char) as usize;
                    match fw.widths.get(idx).copied() {
                        Some(raw) if raw > 0.0 => raw * scale,
                        _ => fallback,
                    }
                }
                _ => fallback,
            };
            map.entry(ch).or_insert(w);
            // Reverse map: smallest code wins so re-encoding is deterministic.
            char_code.entry(ch).or_insert(code as u8);
        }
        Self {
            map,
            char_code,
            default: fallback,
        }
    }

    /// 1-byte font code that displays `ch` in this simple font, if any.
    ///
    /// Inverse of the font's Encoding; used to re-encode edited text back into the
    /// embedded font so the renderer draws the correct glyphs.
    pub fn code_for_char(&self, ch: char) -> Option<u8> {
        self.char_code.get(&ch).copied()
    }

    /// Build metrics for a composite (Type0/CID) font from its ToUnicode CMap and
    /// CID `/W` widths.
    ///
    /// Inverts the CMap to map each displayed Unicode char → its CID code, then
    /// reads the real glyph advance via `widths.get_cid_width(code)` (1/1000 em ×
    /// size). This gives accurate caret/box widths for CID text instead of the
    /// crude 0.5-em estimate. `default` is the font's `DW` (default width) scaled,
    /// for any char not covered.
    pub fn from_composite(cmap: &CMap, widths: &Option<FontWidths>, font_size: f64) -> Self {
        let scale = font_size / 1000.0;
        let default = widths.as_ref().map(|w| w.default_width).unwrap_or(1000.0) * scale;
        let rev = cmap.unicode_to_code();
        let mut map = HashMap::new();
        for (uni, &code) in &rev {
            let mut it = uni.chars();
            if let (Some(c), None) = (it.next(), it.next()) {
                let w = match widths {
                    Some(fw) => fw.get_cid_width(code) * scale,
                    None => default,
                };
                map.entry(c).or_insert(w);
            }
        }
        // Sample a few well-known chars to confirm real /W widths are flowing.
        let sample_v = map.get(&'V').copied();
        let sample_a = map.get(&'a').copied();
        log::debug!(
            "[from-composite] rev_entries={} map_entries={} default={:.2} widths_some={} cid_w_count={} V={:?} a={:?}",
            rev.len(),
            map.len(),
            default,
            widths.is_some(),
            widths.as_ref().map(|w| w.cid_widths.len()).unwrap_or(0),
            sample_v,
            sample_a,
        );
        Self {
            map,
            char_code: HashMap::new(), // CID re-encode goes through the CMap, not this table
            default,
        }
    }

    /// Build metrics from a TrueType glyph-advance iterator (1/1000-em units).
    ///
    /// Used to measure bold/italic embedded fonts with their real advance widths
    /// instead of the regular face's metrics. Typically called with
    /// [`EmbeddedCidFont::iter_char_advances_1000`] after a preview font is embedded.
    pub fn from_ttf_iter(char_advances: impl Iterator<Item = (char, f64)>, font_size: f64) -> Self {
        let scale = font_size / 1000.0;
        let default = 0.5 * font_size;
        let mut map = HashMap::new();
        for (ch, adv_1000) in char_advances {
            map.entry(ch).or_insert(adv_1000 * scale);
        }
        Self {
            map,
            char_code: HashMap::new(),
            default,
        }
    }

    /// Metrics for when no font can be resolved: a proportional estimate of
    /// `0.5·font_size` per character, so the engine still functions.
    pub fn fallback(font_size: f64) -> Self {
        Self {
            map: HashMap::new(),
            char_code: HashMap::new(),
            default: 0.5 * font_size,
        }
    }
}

impl Measurer for PdfFontMetrics {
    fn advance(&self, ch: char) -> f64 {
        self.map.get(&ch).copied().unwrap_or(self.default)
    }
}

/// Resolve a page's effective `/Resources` dictionary (walking `/Parent`).
fn page_resources(doc: &PdfDocument, page_index: usize) -> Option<PdfDict> {
    let catalog = Catalog::from_document(doc).ok()?;
    let page_dict = catalog.get_page_dict(doc, page_index).ok()?;
    match resolve_inherited_attribute(doc, &page_dict, "Resources").ok()? {
        Some(PdfObject::Dictionary(d)) => Some(d),
        _ => None,
    }
}

/// Whether the font under `resource_key` on `page_index` is a composite
/// (Type0 / CID) font.
///
/// Used by the editor to decide the preview path: CID blocks can't yet be
/// re-encoded for the pixel-exact renderer (Phase B), so the host falls back to
/// a Canvas2D preview of the decoded Unicode text. Returns `false` when the font
/// or resources can't be resolved.
pub fn is_composite_font(doc: &PdfDocument, page_index: usize, resource_key: &str) -> bool {
    let Some(resources) = page_resources(doc, page_index) else {
        return false;
    };
    resolve_font_info(resource_key, Some(doc), Some(&resources)).1
}

/// Intrinsic style ([`FontStyleInfo`]) of the font under `resource_key` on
/// `page_index`: its `/BaseFont` plus bold/italic read from the FontDescriptor.
///
/// Returns `None` when the page resources or the font can't be resolved. Used by
/// the text model to seed a block's CharStyle with the font's real bold/italic.
pub(crate) fn font_style_for(
    doc: &PdfDocument,
    page_index: usize,
    resource_key: &str,
) -> Option<FontStyleInfo> {
    let resources = page_resources(doc, page_index)?;
    resolve_font_style(resource_key, Some(doc), Some(&resources))
}

/// Build [`PdfFontMetrics`] for the font under `resource_key` on `page_index`.
///
/// Returns `None` if the font cannot be resolved (e.g. scanned page or a
/// resource key that is not present).
pub fn font_metrics_for(
    doc: &PdfDocument,
    page_index: usize,
    resource_key: &str,
    font_size: f64,
) -> Result<Option<PdfFontMetrics>> {
    let Some(resources) = page_resources(doc, page_index) else {
        return Ok(None);
    };
    let (cmap, is_composite, widths) = resolve_font_info(resource_key, Some(doc), Some(&resources));
    log::debug!(
        "[font-metrics] key={} composite={} cmap_some={} widths_some={}",
        resource_key,
        is_composite,
        cmap.is_some(),
        widths.is_some(),
    );
    // Composite/CID: measure via the real `/W` widths through the inverted CMap.
    // Needs the ToUnicode CMap to map displayed chars back to CID codes; without
    // it we can't measure, so report None (caller uses the proportional estimate).
    if is_composite {
        return Ok(cmap
            .as_ref()
            .map(|cm| PdfFontMetrics::from_composite(cm, &widths, font_size)));
    }
    if cmap.is_none() && widths.is_none() {
        return Ok(None);
    }
    Ok(Some(PdfFontMetrics::from_font_info(
        &cmap, &widths, font_size,
    )))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Monospace stub: every glyph is `unit` wide; spaces too.
    struct Mono(f64);
    impl Measurer for Mono {
        fn advance(&self, _ch: char) -> f64 {
            self.0
        }
    }

    #[test]
    fn caret_offsets_are_cumulative() {
        let m = Mono(10.0);
        let off = caret_offsets(&m, "abc");
        assert_eq!(off, vec![0.0, 10.0, 20.0, 30.0]);
    }

    #[test]
    fn caret_offsets_empty_text() {
        let m = Mono(10.0);
        assert_eq!(caret_offsets(&m, ""), vec![0.0]);
    }

    #[test]
    fn text_width_sums_advances() {
        let m = Mono(7.0);
        assert!((text_width(&m, "hello") - 35.0).abs() < 1e-9);
    }

    #[test]
    fn hit_test_snaps_to_nearest_caret() {
        let m = Mono(10.0);
        assert_eq!(hit_test(&m, "abcd", 0.0), 0);
        assert_eq!(hit_test(&m, "abcd", 14.0), 1); // closer to 10 than 20
        assert_eq!(hit_test(&m, "abcd", 16.0), 2); // closer to 20 than 10
        assert_eq!(hit_test(&m, "abcd", 999.0), 4); // clamps to end
    }

    #[test]
    fn wrap_lines_breaks_on_spaces() {
        let m = Mono(10.0);
        // "aa bb cc" — 10pt each glyph, width 30 fits 2 chars + space boundary.
        let lines = wrap_lines(&m, "aa bb cc", 30.0);
        // Greedy: "aa " (30) then "bb " then "cc".
        let text: Vec<char> = "aa bb cc".chars().collect();
        let rendered: Vec<String> = lines
            .iter()
            .map(|&(s, e)| text[s..e].iter().collect())
            .collect();
        assert_eq!(rendered, vec!["aa ", "bb ", "cc"]);
    }

    #[test]
    fn wrap_lines_hard_breaks_long_word() {
        let m = Mono(10.0);
        let lines = wrap_lines(&m, "aaaaa", 25.0);
        assert!(lines.len() >= 2, "long word must hard-break: {:?}", lines);
    }

    #[test]
    fn wrap_lines_zero_width_is_single_line() {
        let m = Mono(10.0);
        assert_eq!(wrap_lines(&m, "abc", 0.0), vec![(0, 3)]);
    }
}
