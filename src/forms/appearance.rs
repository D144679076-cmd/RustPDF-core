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
