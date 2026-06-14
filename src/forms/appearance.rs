//! Appearance stream generation for PDF annotations and form fields.
//!
//! Each function returns a content stream byte vector suitable for wrapping
//! in a Form XObject stream dict with `/Type /XObject /Subtype /Form /BBox`.

use crate::writer::content_builder::ContentBuilder;

/// Generate an appearance stream for a highlight annotation.
///
/// Draws a semi-transparent yellow rectangle over the annotation rect.
/// The caller is responsible for setting the `ca` (fill alpha) via an
/// `ExtGState` entry; here we just emit the path operators.
pub fn highlight_appearance(rect: [f64; 4]) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let mut b = ContentBuilder::new();
    b.save()
        .set_fill_rgb(1.0, 1.0, 0.0)
        .rect(0.0, 0.0, w, h)
        .fill()
        .restore();
    b.build()
}

/// Generate an appearance stream for a text (sticky-note) annotation icon.
///
/// Draws a simple filled rectangle with a small corner fold — a stylised
/// note icon that fits in the annotation rect.
pub fn text_note_appearance(rect: [f64; 4]) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let fold = (w * 0.25).min(h * 0.25).max(4.0);
    let mut b = ContentBuilder::new();
    b.save()
        // Yellow fill
        .set_fill_rgb(1.0, 0.95, 0.4)
        .set_stroke_gray(0.3)
        .set_line_width(0.5)
        // Main body (without top-right corner)
        .move_to(0.0, 0.0)
        .line_to(0.0, h)
        .line_to(w - fold, h)
        .line_to(w, h - fold)
        .line_to(w, 0.0)
        .close_path()
        .fill_stroke()
        // Corner fold triangle
        .set_fill_gray(0.8)
        .move_to(w - fold, h)
        .line_to(w - fold, h - fold)
        .line_to(w, h - fold)
        .close_path()
        .fill_stroke()
        .restore();
    b.build()
}

/// Generate a checkbox appearance stream.
///
/// Returns an appearance for the `/On` (checked) or `/Off` (unchecked) state.
pub fn checkbox_appearance(rect: [f64; 4], checked: bool) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let mut b = ContentBuilder::new();
    b.save()
        // Box border
        .set_stroke_gray(0.0)
        .set_line_width(1.0)
        .rect(1.0, 1.0, w - 2.0, h - 2.0)
        .stroke();
    if checked {
        // Draw an X (checkmark) inside the box.
        let pad = (w.min(h) * 0.2).max(2.0);
        b.set_stroke_gray(0.0)
            .set_line_width(1.5)
            .move_to(pad, pad)
            .line_to(w - pad, h - pad)
            .stroke()
            .move_to(w - pad, pad)
            .line_to(pad, h - pad)
            .stroke();
    }
    b.restore();
    b.build()
}

/// Generate a radio button appearance stream.
///
/// Draws a circle outline (off) or a filled circle (on) for radio button states.
pub fn radio_appearance(rect: [f64; 4], selected: bool) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let cx = w / 2.0;
    let cy = h / 2.0;
    let r = (w.min(h) / 2.0) - 1.0;

    let mut b = ContentBuilder::new();
    b.save().set_stroke_gray(0.0).set_line_width(1.0);

    // Outer circle using 4 Bézier curves (kappa ≈ 0.5523)
    let k = r * 0.5523;
    b.move_to(cx + r, cy)
        .curve_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r)
        .curve_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy)
        .curve_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r)
        .curve_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy)
        .close_path()
        .stroke();

    if selected {
        // Inner filled circle (60% of outer radius)
        let ri = r * 0.6;
        let ki = ri * 0.5523;
        b.set_fill_gray(0.0)
            .move_to(cx + ri, cy)
            .curve_to(cx + ri, cy + ki, cx + ki, cy + ri, cx, cy + ri)
            .curve_to(cx - ki, cy + ri, cx - ri, cy + ki, cx - ri, cy)
            .curve_to(cx - ri, cy - ki, cx - ki, cy - ri, cx, cy - ri)
            .curve_to(cx + ki, cy - ri, cx + ri, cy - ki, cx + ri, cy)
            .close_path()
            .fill();
    }

    b.restore();
    b.build()
}

// ── Text field ────────────────────────────────────────────────────────────────

/// Generate an appearance stream for a single-line text field.
///
/// Draws the value string using the standard `/Helv` (Helvetica) alias with an
/// auto-sized font.  `max_len` truncates display to that many characters when set.
pub fn text_field_appearance(value: &str, rect: [f64; 4], max_len: Option<u32>) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let font_size = (h * 0.7).clamp(6.0, 12.0);
    let padding = 2.0;
    let display: String = if let Some(max) = max_len {
        value.chars().take(max as usize).collect()
    } else {
        value.to_owned()
    };
    let _ = w; // width reserved for future multi-line wrapping
    format!(
        "q BT /Helv {} Tf {} {} Td ({}) Tj ET Q",
        font_size,
        padding,
        (h - font_size) / 2.0,
        escape_pdf_string(&display)
    )
    .into_bytes()
}

/// Escape special characters in a PDF literal string.
fn escape_pdf_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

// ── Stamp ─────────────────────────────────────────────────────────────────────

/// Generate an appearance stream for a stamp annotation.
///
/// Draws a coloured border rectangle with the stamp label centred inside.
/// `rect` is the annotation bounding box; `color` is the RGB border/text colour.
pub fn stamp_appearance(name: &str, rect: [f64; 4], color: [f64; 3]) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let font_size = (h * 0.6).clamp(8.0, 24.0);
    let approx_text_width = font_size * 0.55 * name.len() as f64;
    let cx = (w / 2.0 - approx_text_width / 2.0).max(2.0);
    let cy = ((h - font_size) / 2.0).max(2.0);
    let mut b = ContentBuilder::new();
    b.save()
        .set_stroke_rgb(color[0], color[1], color[2])
        .set_line_width(1.5)
        .rect(2.0, 2.0, w - 4.0, h - 4.0)
        .stroke()
        .set_fill_rgb(color[0], color[1], color[2])
        .begin_text()
        .set_font("Helv", font_size)
        .move_text_pos(cx, cy)
        .show_text(name.as_bytes())
        .end_text()
        .restore();
    b.build()
}

// ── FreeText ──────────────────────────────────────────────────────────────────

/// Generate an appearance stream for a FreeText annotation.
///
/// Renders `text` at `font_size` in the given `color` inside the annotation bbox.
pub fn freetext_appearance(text: &str, rect: [f64; 4], font_size: f64, color: [f64; 3]) -> Vec<u8> {
    let [_x1, y1, _x2, y2] = rect;
    let h = y2 - y1;
    let y_pos = (h - font_size - 2.0).max(2.0);
    let mut b = ContentBuilder::new();
    b.save()
        .set_fill_rgb(color[0], color[1], color[2])
        .begin_text()
        .set_font("Helv", font_size)
        .move_text_pos(2.0, y_pos)
        .show_text(escape_pdf_string(text).as_bytes())
        .end_text()
        .restore();
    b.build()
}

// ── Ink ───────────────────────────────────────────────────────────────────────

/// Generate an appearance stream for an Ink (freehand) annotation.
///
/// `ink_list` is a list of strokes; each stroke is a list of `[x, y]` points
/// in page space. `bbox` is the annotation bounding box (used as origin offset).
pub fn ink_appearance(ink_list: &[Vec<[f64; 2]>], bbox: [f64; 4], color: [f64; 3]) -> Vec<u8> {
    let [ox, oy, _, _] = bbox;
    let mut b = ContentBuilder::new();
    b.save()
        .set_stroke_rgb(color[0], color[1], color[2])
        .set_line_width(1.5);
    for stroke in ink_list {
        if stroke.len() < 2 {
            continue;
        }
        b.move_to(stroke[0][0] - ox, stroke[0][1] - oy);
        for pt in &stroke[1..] {
            b.line_to(pt[0] - ox, pt[1] - oy);
        }
        b.stroke();
    }
    b.restore();
    b.build()
}

// ── Highlight (quad-based) ────────────────────────────────────────────────────

/// Generate an appearance stream for a Highlight annotation using QuadPoints.
///
/// Each entry in `quad_points` is 8 coordinates `[x1,y1, x2,y2, x3,y3, x4,y4]`
/// (lower-left, lower-right, upper-left, upper-right in page space).
/// `bbox` is the annotation bounding box used as the local coordinate origin.
pub fn highlight_appearance_quad(
    quad_points: &[[f64; 8]],
    bbox: [f64; 4],
    color: [f64; 3],
) -> Vec<u8> {
    let [ox, oy, _, _] = bbox;
    let mut b = ContentBuilder::new();
    b.save().set_fill_rgb(color[0], color[1], color[2]);
    for quad in quad_points {
        let x = quad[0].min(quad[2]).min(quad[4]).min(quad[6]) - ox;
        let y = quad[1].min(quad[3]).min(quad[5]).min(quad[7]) - oy;
        let w = quad[0].max(quad[2]).max(quad[4]).max(quad[6]) - ox - x;
        let h = quad[1].max(quad[3]).max(quad[5]).max(quad[7]) - oy - y;
        b.rect(x, y, w, h).fill();
    }
    b.restore();
    b.build()
}

// ── Polygon / Polyline ────────────────────────────────────────────────────────

/// Generate an appearance stream for a Polygon annotation.
///
/// `vertices` are page-space `[x, y]` points. `rect` is the annotation bounding
/// box (subtracted to produce form-XObject-local coordinates). The path is
/// always closed; fill is applied when `fill_color` is `Some`.
pub fn polygon_appearance(
    vertices: &[[f64; 2]],
    rect: [f64; 4],
    stroke_color: [f64; 3],
    fill_color: Option<[f64; 3]>,
    line_width: f64,
) -> Vec<u8> {
    if vertices.len() < 2 {
        return vec![];
    }
    let [ox, oy, _, _] = rect;
    let mut b = ContentBuilder::new();
    b.save()
        .set_stroke_rgb(stroke_color[0], stroke_color[1], stroke_color[2])
        .set_line_width(line_width);
    if let Some(fc) = fill_color {
        b.set_fill_rgb(fc[0], fc[1], fc[2]);
    }
    b.move_to(vertices[0][0] - ox, vertices[0][1] - oy);
    for v in &vertices[1..] {
        b.line_to(v[0] - ox, v[1] - oy);
    }
    b.close_path();
    if fill_color.is_some() {
        b.fill_stroke();
    } else {
        b.stroke();
    }
    b.restore();
    b.build()
}

/// Generate an appearance stream for a Polyline annotation.
///
/// Like `polygon_appearance` but the path is left open (no close-path).
pub fn polyline_appearance(
    vertices: &[[f64; 2]],
    rect: [f64; 4],
    stroke_color: [f64; 3],
    line_width: f64,
) -> Vec<u8> {
    if vertices.len() < 2 {
        return vec![];
    }
    let [ox, oy, _, _] = rect;
    let mut b = ContentBuilder::new();
    b.save()
        .set_stroke_rgb(stroke_color[0], stroke_color[1], stroke_color[2])
        .set_line_width(line_width)
        .move_to(vertices[0][0] - ox, vertices[0][1] - oy);
    for v in &vertices[1..] {
        b.line_to(v[0] - ox, v[1] - oy);
    }
    b.stroke().restore();
    b.build()
}

// ── Caret ─────────────────────────────────────────────────────────────────────

/// Generate an appearance stream for a Caret annotation.
///
/// Draws a simple ^ (up-caret) shape within the annotation bounding box.
pub fn caret_appearance(rect: [f64; 4]) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let mut b = ContentBuilder::new();
    b.save()
        .set_stroke_rgb(0.0, 0.0, 0.5)
        .set_line_width(1.0)
        .move_to(0.0, 0.0)
        .line_to(w / 2.0, h)
        .line_to(w, 0.0)
        .stroke()
        .restore();
    b.build()
}

// ── FileAttachment ────────────────────────────────────────────────────────────

/// Generate an appearance stream for a FileAttachment annotation.
///
/// Draws a simple pin-like icon centred in the annotation bounding box.
/// The `_icon_name` parameter is accepted for future per-icon rendering but
/// currently all icons render as the same push-pin shape.
pub fn file_attachment_appearance(rect: [f64; 4], _icon_name: &str) -> Vec<u8> {
    let [x1, y1, x2, y2] = rect;
    let w = x2 - x1;
    let h = y2 - y1;
    let cx = w / 2.0;
    let mut b = ContentBuilder::new();
    b.save()
        // Pin body
        .set_fill_rgb(0.5, 0.5, 0.5)
        .rect(cx - 2.0, 0.0, 4.0, h * 0.6)
        .fill()
        // Pin head
        .set_fill_rgb(0.3, 0.3, 0.3)
        .rect(cx - w * 0.25, h * 0.55, w * 0.5, h * 0.4)
        .fill()
        .restore();
    b.build()
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_appearance_non_empty() {
        let bytes = highlight_appearance([0.0, 0.0, 100.0, 20.0]);
        assert!(!bytes.is_empty());
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("rg")); // fill color operator
    }

    #[test]
    fn text_note_appearance_non_empty() {
        let bytes = text_note_appearance([0.0, 0.0, 20.0, 20.0]);
        assert!(!bytes.is_empty());
    }

    #[test]
    fn checkbox_unchecked_no_checkmark() {
        let bytes = checkbox_appearance([0.0, 0.0, 12.0, 12.0], false);
        let s = String::from_utf8_lossy(&bytes);
        // Only the border stroke — no extra move_to for checkmark
        assert!(s.contains("re"));
        assert!(s.contains("S\n"));
    }

    #[test]
    fn checkbox_checked_has_x() {
        let bytes = checkbox_appearance([0.0, 0.0, 12.0, 12.0], true);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("m\n")); // move_to for X strokes
    }

    #[test]
    fn text_field_appearance_contains_value() {
        let bytes = text_field_appearance("Hello", [0.0, 0.0, 100.0, 20.0], None);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("Hello"));
        assert!(s.contains("BT"));
        assert!(s.contains("ET"));
    }

    #[test]
    fn text_field_appearance_truncates_to_max_len() {
        let bytes = text_field_appearance("ABCDE", [0.0, 0.0, 100.0, 20.0], Some(3));
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("ABC"));
        assert!(!s.contains("ABCDE"));
    }

    #[test]
    fn escape_pdf_string_roundtrip() {
        let s = escape_pdf_string("(hello\\world)");
        assert_eq!(s, "\\(hello\\\\world\\)");
    }
}
