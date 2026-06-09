//! Builder for PDF content stream operators (ISO 32000-1 §8–9).
//!
//! All methods return `&mut Self` so calls can be chained.

use crate::writer::serializer::format_real;

/// A single item inside a `TJ` array: either encoded text bytes or a
/// kerning displacement in thousandths of text-space units.
#[derive(Debug, Clone)]
pub enum TjItem {
    /// Encoded text bytes (shown with current font).
    Text(Vec<u8>),
    /// Horizontal kerning displacement (negative = tighten, positive = loosen).
    Kern(f64),
}

/// Ergonomic builder for a PDF content stream.
///
/// Collect drawing commands with the fluent API, then call [`build`](Self::build)
/// to get the raw byte vector suitable for passing to [`make_flate_stream`](crate::writer::streams::make_flate_stream).
#[derive(Debug, Default)]
pub struct ContentBuilder {
    buf: Vec<u8>,
}

// ── Internal helper ───────────────────────────────────────────────────────────

impl ContentBuilder {
    /// Append a formatted real number followed by a space.
    fn num(&mut self, f: f64) -> &mut Self {
        self.buf.extend_from_slice(format_real(f).as_bytes());
        self.buf.push(b' ');
        self
    }

    /// Append a string literal operator line (e.g. `"q\n"`).
    fn op(&mut self, s: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(s);
        self.buf.push(b'\n');
        self
    }

    /// Escape bytes for use inside a PDF literal string `(...)`.
    fn escape_string(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() + 2);
        for &b in bytes {
            match b {
                b'(' | b')' | b'\\' => {
                    out.push(b'\\');
                    out.push(b);
                }
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\t' => out.extend_from_slice(b"\\t"),
                _ => out.push(b),
            }
        }
        out
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

impl ContentBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume the builder and return the accumulated content stream bytes.
    pub fn build(self) -> Vec<u8> {
        self.buf
    }

    // ── Graphics state ────────────────────────────────────────────────────────

    /// `q` — save graphics state.
    pub fn save(&mut self) -> &mut Self {
        self.op(b"q")
    }

    /// `Q` — restore graphics state.
    pub fn restore(&mut self) -> &mut Self {
        self.op(b"Q")
    }

    /// `a b c d e f cm` — concatenate transformation matrix.
    pub fn concat_matrix(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> &mut Self {
        self.num(a).num(b).num(c).num(d).num(e).num(f);
        self.op(b"cm")
    }

    // ── Line style ────────────────────────────────────────────────────────────

    /// `w w` — set line width.
    pub fn set_line_width(&mut self, w: f64) -> &mut Self {
        self.num(w);
        self.op(b"w")
    }

    /// `n J` — set line cap style (0=butt, 1=round, 2=projecting square).
    pub fn set_line_cap(&mut self, cap: i32) -> &mut Self {
        self.buf.extend_from_slice(cap.to_string().as_bytes());
        self.buf.push(b' ');
        self.op(b"J")
    }

    /// `n j` — set line join style (0=miter, 1=round, 2=bevel).
    pub fn set_line_join(&mut self, join: i32) -> &mut Self {
        self.buf.extend_from_slice(join.to_string().as_bytes());
        self.buf.push(b' ');
        self.op(b"j")
    }

    /// `M M` — set miter limit.
    pub fn set_miter_limit(&mut self, m: f64) -> &mut Self {
        self.num(m);
        self.op(b"M")
    }

    /// `[p…] phase d` — set dash pattern.
    pub fn set_dash(&mut self, pattern: &[f64], phase: f64) -> &mut Self {
        self.buf.push(b'[');
        for (i, &p) in pattern.iter().enumerate() {
            if i > 0 {
                self.buf.push(b' ');
            }
            self.buf.extend_from_slice(format_real(p).as_bytes());
        }
        self.buf.push(b']');
        self.buf.push(b' ');
        self.num(phase);
        self.op(b"d")
    }

    // ── Color ─────────────────────────────────────────────────────────────────

    /// `g G` — set stroke color (gray).
    pub fn set_stroke_gray(&mut self, g: f64) -> &mut Self {
        self.num(g);
        self.op(b"G")
    }

    /// `g g` — set fill color (gray).
    pub fn set_fill_gray(&mut self, g: f64) -> &mut Self {
        self.num(g);
        self.op(b"g")
    }

    /// `r g b RG` — set stroke color (RGB).
    pub fn set_stroke_rgb(&mut self, r: f64, g: f64, b: f64) -> &mut Self {
        self.num(r).num(g).num(b);
        self.op(b"RG")
    }

    /// `r g b rg` — set fill color (RGB).
    pub fn set_fill_rgb(&mut self, r: f64, g: f64, b: f64) -> &mut Self {
        self.num(r).num(g).num(b);
        self.op(b"rg")
    }

    /// `c m y k K` — set stroke color (CMYK).
    pub fn set_stroke_cmyk(&mut self, c: f64, m: f64, y: f64, k: f64) -> &mut Self {
        self.num(c).num(m).num(y).num(k);
        self.op(b"K")
    }

    /// `c m y k k` — set fill color (CMYK).
    pub fn set_fill_cmyk(&mut self, c: f64, m: f64, y: f64, k: f64) -> &mut Self {
        self.num(c).num(m).num(y).num(k);
        self.op(b"k")
    }

    // ── Path construction ─────────────────────────────────────────────────────

    /// `x y m` — move to.
    pub fn move_to(&mut self, x: f64, y: f64) -> &mut Self {
        self.num(x).num(y);
        self.op(b"m")
    }

    /// `x y l` — line to.
    pub fn line_to(&mut self, x: f64, y: f64) -> &mut Self {
        self.num(x).num(y);
        self.op(b"l")
    }

    /// `x1 y1 x2 y2 x3 y3 c` — cubic Bézier curve.
    pub fn curve_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) -> &mut Self {
        self.num(x1).num(y1).num(x2).num(y2).num(x3).num(y3);
        self.op(b"c")
    }

    /// `x y w h re` — append rectangle.
    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) -> &mut Self {
        self.num(x).num(y).num(w).num(h);
        self.op(b"re")
    }

    /// `h` — close current sub-path.
    pub fn close_path(&mut self) -> &mut Self {
        self.op(b"h")
    }

    // ── Path painting ─────────────────────────────────────────────────────────

    /// `S` — stroke path.
    pub fn stroke(&mut self) -> &mut Self {
        self.op(b"S")
    }

    /// `s` — close and stroke.
    pub fn close_stroke(&mut self) -> &mut Self {
        self.op(b"s")
    }

    /// `f` — fill path (non-zero winding).
    pub fn fill(&mut self) -> &mut Self {
        self.op(b"f")
    }

    /// `f*` — fill path (even-odd rule).
    pub fn fill_even_odd(&mut self) -> &mut Self {
        self.op(b"f*")
    }

    /// `B` — fill and stroke (non-zero winding).
    pub fn fill_stroke(&mut self) -> &mut Self {
        self.op(b"B")
    }

    /// `B*` — fill and stroke (even-odd rule).
    pub fn fill_stroke_even_odd(&mut self) -> &mut Self {
        self.op(b"B*")
    }

    /// `n` — end path without painting (used for clipping paths).
    pub fn no_op(&mut self) -> &mut Self {
        self.op(b"n")
    }

    // ── Clipping ──────────────────────────────────────────────────────────────

    /// `W n` — set clipping path (non-zero winding rule).
    pub fn clip(&mut self) -> &mut Self {
        self.buf.extend_from_slice(b"W\n");
        self.op(b"n")
    }

    /// `W* n` — set clipping path (even-odd rule).
    pub fn clip_even_odd(&mut self) -> &mut Self {
        self.buf.extend_from_slice(b"W*\n");
        self.op(b"n")
    }

    // ── Text ──────────────────────────────────────────────────────────────────

    /// `BT` — begin text object.
    pub fn begin_text(&mut self) -> &mut Self {
        self.op(b"BT")
    }

    /// `ET` — end text object.
    pub fn end_text(&mut self) -> &mut Self {
        self.op(b"ET")
    }

    /// `/Name size Tf` — set font and size.
    pub fn set_font(&mut self, name: &str, size: f64) -> &mut Self {
        self.buf.push(b'/');
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.push(b' ');
        self.num(size);
        self.op(b"Tf")
    }

    /// `s Tc` — set character spacing.
    pub fn set_char_spacing(&mut self, s: f64) -> &mut Self {
        self.num(s);
        self.op(b"Tc")
    }

    /// `s Tw` — set word spacing.
    pub fn set_word_spacing(&mut self, s: f64) -> &mut Self {
        self.num(s);
        self.op(b"Tw")
    }

    /// `s Tz` — set horizontal scaling (%).
    pub fn set_horizontal_scaling(&mut self, s: f64) -> &mut Self {
        self.num(s);
        self.op(b"Tz")
    }

    /// `l TL` — set text leading.
    pub fn set_text_leading(&mut self, l: f64) -> &mut Self {
        self.num(l);
        self.op(b"TL")
    }

    /// `r Ts` — set text rise.
    pub fn set_text_rise(&mut self, r: f64) -> &mut Self {
        self.num(r);
        self.op(b"Ts")
    }

    /// `m Tr` — set text rendering mode.
    pub fn set_text_render_mode(&mut self, m: i32) -> &mut Self {
        self.buf.extend_from_slice(m.to_string().as_bytes());
        self.buf.push(b' ');
        self.op(b"Tr")
    }

    /// `tx ty Td` — move text position.
    pub fn move_text_pos(&mut self, tx: f64, ty: f64) -> &mut Self {
        self.num(tx).num(ty);
        self.op(b"Td")
    }

    /// `a b c d e f Tm` — set text matrix and line matrix.
    pub fn set_text_matrix(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> &mut Self {
        self.num(a).num(b).num(c).num(d).num(e).num(f);
        self.op(b"Tm")
    }

    /// `T*` — move to start of next line.
    pub fn next_line(&mut self) -> &mut Self {
        self.op(b"T*")
    }

    /// `(bytes) Tj` — show string from raw bytes.
    pub fn show_text(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf.push(b'(');
        self.buf.extend_from_slice(&Self::escape_string(bytes));
        self.buf.push(b')');
        self.buf.push(b' ');
        self.op(b"Tj")
    }

    /// `(str) Tj` — show UTF-8 string (encoded as PDFDocEncoding / Latin-1).
    pub fn show_text_str(&mut self, s: &str) -> &mut Self {
        self.show_text(s.as_bytes())
    }

    /// `[...] TJ` — show string array with individual displacements.
    pub fn show_text_array(&mut self, items: &[TjItem]) -> &mut Self {
        self.buf.push(b'[');
        for item in items {
            match item {
                TjItem::Text(bytes) => {
                    self.buf.push(b'(');
                    self.buf.extend_from_slice(&Self::escape_string(bytes));
                    self.buf.push(b')');
                }
                TjItem::Kern(k) => {
                    self.buf.extend_from_slice(format_real(*k).as_bytes());
                }
            }
            self.buf.push(b' ');
        }
        if !items.is_empty() && self.buf.last() == Some(&b' ') {
            self.buf.pop();
        }
        self.buf.push(b']');
        self.buf.push(b' ');
        self.op(b"TJ")
    }

    // ── XObject ───────────────────────────────────────────────────────────────

    /// `/Name Do` — invoke named XObject (image or form).
    pub fn do_xobject(&mut self, name: &str) -> &mut Self {
        self.buf.push(b'/');
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.push(b' ');
        self.op(b"Do")
    }

    // ── Graphics state dict ───────────────────────────────────────────────────

    /// `/Name gs` — apply named ExtGState dictionary.
    pub fn apply_gs(&mut self, name: &str) -> &mut Self {
        self.buf.push(b'/');
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.push(b' ');
        self.op(b"gs")
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::operators::parse_content_stream;

    fn build(f: impl Fn(&mut ContentBuilder)) -> Vec<u8> {
        let mut b = ContentBuilder::new();
        f(&mut b);
        b.build()
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).to_string()
    }

    #[test]
    fn save_restore() {
        let out = build(|b| {
            b.save().restore();
        });
        assert!(text(&out).contains("q\n"));
        assert!(text(&out).contains("Q\n"));
    }

    #[test]
    fn concat_matrix() {
        let out = build(|b| {
            b.concat_matrix(1.0, 0.0, 0.0, 1.0, 10.0, 20.0);
        });
        assert!(text(&out).contains("1 0 0 1 10 20 cm"));
    }

    #[test]
    fn fill_rgb() {
        let out = build(|b| {
            b.set_fill_rgb(1.0, 0.0, 0.5);
        });
        assert!(text(&out).contains("1 0 0.5 rg"));
    }

    #[test]
    fn stroke_cmyk() {
        let out = build(|b| {
            b.set_stroke_cmyk(0.1, 0.2, 0.3, 0.4);
        });
        assert!(text(&out).contains("0.1 0.2 0.3 0.4 K"));
    }

    #[test]
    fn path_operators() {
        let out = build(|b| {
            b.move_to(0.0, 0.0)
                .line_to(100.0, 0.0)
                .line_to(100.0, 100.0)
                .close_path()
                .stroke();
        });
        let s = text(&out);
        assert!(s.contains("0 0 m"));
        assert!(s.contains("100 0 l"));
        assert!(s.contains("h\n"));
        assert!(s.contains("S\n"));
    }

    #[test]
    fn rect_fill() {
        let out = build(|b| {
            b.rect(10.0, 20.0, 200.0, 100.0).fill();
        });
        let s = text(&out);
        assert!(s.contains("10 20 200 100 re"));
        assert!(s.contains("f\n"));
    }

    #[test]
    fn text_basic() {
        let out = build(|b| {
            b.begin_text()
                .set_font("F1", 12.0)
                .move_text_pos(10.0, 700.0)
                .show_text_str("Hello")
                .end_text();
        });
        let s = text(&out);
        assert!(s.contains("BT\n"));
        assert!(s.contains("/F1 12 Tf"));
        assert!(s.contains("10 700 Td"));
        assert!(s.contains("(Hello) Tj"));
        assert!(s.contains("ET\n"));
    }

    #[test]
    fn show_text_escapes_parens() {
        let out = build(|b| {
            b.show_text(b"a(b)c");
        });
        let s = text(&out);
        assert!(s.contains(r"(a\(b\)c)"));
    }

    #[test]
    fn show_text_array() {
        let items = vec![
            TjItem::Text(b"Hi".to_vec()),
            TjItem::Kern(-50.0),
            TjItem::Text(b"!".to_vec()),
        ];
        let out = build(|b| {
            b.show_text_array(&items);
        });
        let s = text(&out);
        assert!(s.contains("[(Hi) -50 (!)] TJ"));
    }

    #[test]
    fn do_xobject() {
        let out = build(|b| {
            b.do_xobject("Im1");
        });
        assert!(text(&out).contains("/Im1 Do"));
    }

    #[test]
    fn dash_pattern() {
        let out = build(|b| {
            b.set_dash(&[3.0, 5.0], 0.0);
        });
        assert!(text(&out).contains("[3 5] 0 d"));
    }

    #[test]
    fn parseable_by_content_interpreter() {
        let out = build(|b| {
            b.save()
                .set_fill_rgb(1.0, 0.0, 0.0)
                .rect(0.0, 0.0, 100.0, 100.0)
                .fill()
                .restore();
        });
        let ops = parse_content_stream(&out).unwrap();
        // q rg re f Q → 5 operations
        assert_eq!(ops.len(), 5);
    }
}
