//! PDF Text State (ISO 32000-1 §9.3).
//!
//! Tracks font, size, spacing, and text positioning matrices used during
//! text object rendering (between BT and ET operators).

use super::graphics_state::Matrix;

/// Text rendering mode (ISO 32000-1 §9.3.6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextRenderMode {
    Fill = 0,
    Stroke = 1,
    FillStroke = 2,
    Invisible = 3,
    FillClip = 4,
    StrokeClip = 5,
    FillStrokeClip = 6,
    Clip = 7,
}

#[allow(clippy::derivable_impls)]
impl Default for TextRenderMode {
    fn default() -> Self {
        TextRenderMode::Fill
    }
}

impl TextRenderMode {
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => TextRenderMode::Fill,
            1 => TextRenderMode::Stroke,
            2 => TextRenderMode::FillStroke,
            3 => TextRenderMode::Invisible,
            4 => TextRenderMode::FillClip,
            5 => TextRenderMode::StrokeClip,
            6 => TextRenderMode::FillStrokeClip,
            7 => TextRenderMode::Clip,
            _ => TextRenderMode::Fill,
        }
    }

    /// Whether this mode involves filling.
    pub fn fills(&self) -> bool {
        matches!(
            self,
            TextRenderMode::Fill
                | TextRenderMode::FillStroke
                | TextRenderMode::FillClip
                | TextRenderMode::FillStrokeClip
        )
    }

    /// Whether this mode involves stroking.
    pub fn strokes(&self) -> bool {
        matches!(
            self,
            TextRenderMode::Stroke
                | TextRenderMode::FillStroke
                | TextRenderMode::StrokeClip
                | TextRenderMode::FillStrokeClip
        )
    }

    /// Whether this mode adds to the clipping path.
    pub fn clips(&self) -> bool {
        matches!(
            self,
            TextRenderMode::FillClip
                | TextRenderMode::StrokeClip
                | TextRenderMode::FillStrokeClip
                | TextRenderMode::Clip
        )
    }
}

/// Text state parameters (ISO 32000-1 §9.3, Table 104).
#[derive(Debug, Clone)]
pub struct TextState {
    /// Character spacing (Tc). Extra space added after each character.
    pub char_spacing: f64,
    /// Word spacing (Tw). Extra space added after ASCII space (0x20).
    pub word_spacing: f64,
    /// Horizontal scaling (Tz). Percentage, default 100.
    pub horiz_scaling: f64,
    /// Text leading (TL). Vertical distance between baselines.
    pub leading: f64,
    /// Current font name (resource key, e.g. "F1").
    pub font_name: String,
    /// Current font size in text space units.
    pub font_size: f64,
    /// Text rendering mode (Tr).
    pub render_mode: TextRenderMode,
    /// Text rise (Ts). Vertical offset from baseline.
    pub rise: f64,
    /// Text matrix (Tm). Set by Tm operator, updated by text-showing ops.
    pub tm: Matrix,
    /// Text line matrix (Tlm). Set at the start of each line (Td, TD, T*, ', ").
    pub tlm: Matrix,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            char_spacing: 0.0,
            word_spacing: 0.0,
            horiz_scaling: 100.0,
            leading: 0.0,
            font_name: String::new(),
            font_size: 0.0,
            render_mode: TextRenderMode::default(),
            rise: 0.0,
            tm: Matrix::identity(),
            tlm: Matrix::identity(),
        }
    }
}

impl TextState {
    /// Reset text matrices to identity (called at BT).
    pub fn begin_text(&mut self) {
        self.tm = Matrix::identity();
        self.tlm = Matrix::identity();
    }

    /// Set the text matrix and line matrix (Tm operator).
    pub fn set_text_matrix(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        let m = Matrix { a, b, c, d, e, f };
        self.tm = m;
        self.tlm = m;
    }

    /// Move to the start of the next line (Td operator).
    /// Offsets from the start of the current line (Tlm).
    pub fn move_text_position(&mut self, tx: f64, ty: f64) {
        let offset = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        };
        self.tlm = offset.concat(&self.tlm);
        self.tm = self.tlm;
    }

    /// Move to the next line (T* operator). Uses current leading.
    pub fn next_line(&mut self) {
        self.move_text_position(0.0, -self.leading);
    }

    /// Advance the text position after rendering a glyph.
    ///
    /// `width` is the glyph width in text space (already scaled by font size).
    /// `is_space` indicates if the character is ASCII space (for word spacing).
    pub fn advance_glyph(&mut self, width: f64, is_space: bool) {
        let tx = (width + self.char_spacing + if is_space { self.word_spacing } else { 0.0 })
            * (self.horiz_scaling / 100.0);

        let advance = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: 0.0,
        };
        self.tm = advance.concat(&self.tm);
    }

    /// Advance by a TJ displacement value (in thousandths of a unit of text space).
    /// Negative values move right (standard for kerning adjustments).
    pub fn advance_tj_displacement(&mut self, displacement: f64) {
        let tx = -(displacement / 1000.0) * self.font_size * (self.horiz_scaling / 100.0);
        let advance = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: 0.0,
        };
        self.tm = advance.concat(&self.tm);
    }

    /// Get the current text rendering position in user space.
    /// Accounts for text rise.
    pub fn get_render_matrix(&self, ctm: &Matrix) -> Matrix {
        let rise_matrix = Matrix {
            a: self.font_size * (self.horiz_scaling / 100.0),
            b: 0.0,
            c: 0.0,
            d: self.font_size,
            e: 0.0,
            f: self.rise,
        };
        rise_matrix.concat(&self.tm).concat(ctm)
    }
}

/// A span of extracted text with position information.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpan {
    /// The Unicode text content.
    pub text: String,
    /// X position in user space (lower-left of first glyph).
    pub x: f64,
    /// Y position in user space (baseline).
    pub y: f64,
    /// Total width of the span in user space.
    pub width: f64,
    /// Font size in user space.
    pub font_size: f64,
    /// Font size in device pixels: magnitude of the Y-basis vector of the render matrix
    /// (font_size × text_matrix_scale × CTM_scale). Use this for rasterization.
    pub font_size_px: f64,
    /// Font resource name (e.g. "F1").
    pub font_name: String,
    /// Per-character x-advance in pixel space derived from the PDF /W array.
    /// One entry per Unicode character in `text`.  Empty when no width data
    /// was available; renderer falls back to fontdue metrics in that case.
    pub char_advances: Vec<f64>,
    /// Per-character y-advance in pixel space.  Non-zero for rotated text
    /// (e.g. a 90° y-axis label) where glyph advance has a vertical component.
    /// Parallel to `char_advances`; empty when all advances are horizontal.
    pub char_advances_y: Vec<f64>,
    /// Original CID per Unicode character in `text`.  Non-empty only for
    /// composite (Type0/CIDFont) fonts.  For Identity-H encoding, CID == GID
    /// in the embedded font file, so the renderer can call `rasterize_indexed`.
    pub char_cids: Vec<u32>,
    /// The 2×2 rotation+scale part `[a, b, c, d]` of the final render matrix
    /// (rise_matrix × text_matrix × CTM).  Used by the renderer to rotate glyph
    /// bitmaps.  For upright text this is approximately `[scale, 0, 0, -scale]`.
    pub render_matrix_2x2: [f64; 4],
    /// Whether the text render mode strokes the glyph outline (`Tr` 1/2/5/6).
    /// The renderer thickens the glyph to approximate the stroke — used to fake
    /// synthetic bold on a font that has no real bold face.
    pub stroke_text: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_state_default() {
        let ts = TextState::default();
        assert_eq!(ts.char_spacing, 0.0);
        assert_eq!(ts.word_spacing, 0.0);
        assert_eq!(ts.horiz_scaling, 100.0);
        assert_eq!(ts.font_size, 0.0);
        assert_eq!(ts.render_mode, TextRenderMode::Fill);
    }

    #[test]
    fn test_begin_text_resets_matrices() {
        let mut ts = TextState::default();
        ts.tm = Matrix {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            e: 50.0,
            f: 50.0,
        };
        ts.begin_text();
        assert_eq!(ts.tm, Matrix::identity());
        assert_eq!(ts.tlm, Matrix::identity());
    }

    #[test]
    fn test_move_text_position() {
        let mut ts = TextState::default();
        ts.begin_text();
        ts.move_text_position(100.0, 200.0);
        assert_eq!(ts.tm.e, 100.0);
        assert_eq!(ts.tm.f, 200.0);
        assert_eq!(ts.tlm.e, 100.0);
        assert_eq!(ts.tlm.f, 200.0);
    }

    #[test]
    fn test_next_line() {
        let mut ts = TextState::default();
        ts.leading = 12.0;
        ts.begin_text();
        ts.move_text_position(72.0, 700.0);
        ts.next_line();
        assert!((ts.tm.e - 72.0).abs() < 1e-10);
        assert!((ts.tm.f - 688.0).abs() < 1e-10);
    }

    #[test]
    fn test_advance_glyph() {
        let mut ts = TextState::default();
        ts.font_size = 12.0;
        ts.begin_text();
        ts.advance_glyph(6.0, false);
        assert!((ts.tm.e - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_advance_glyph_with_word_spacing() {
        let mut ts = TextState::default();
        ts.font_size = 12.0;
        ts.word_spacing = 2.0;
        ts.begin_text();
        ts.advance_glyph(6.0, true);
        assert!((ts.tm.e - 8.0).abs() < 1e-10);
    }

    #[test]
    fn test_tj_displacement() {
        let mut ts = TextState::default();
        ts.font_size = 12.0;
        ts.begin_text();
        // -120 thousandths → move right by 120/1000 * 12 = 1.44
        ts.advance_tj_displacement(-120.0);
        assert!((ts.tm.e - 1.44).abs() < 1e-10);
    }

    #[test]
    fn test_render_mode_properties() {
        assert!(TextRenderMode::Fill.fills());
        assert!(!TextRenderMode::Fill.strokes());
        assert!(!TextRenderMode::Fill.clips());
        assert!(TextRenderMode::FillStrokeClip.fills());
        assert!(TextRenderMode::FillStrokeClip.strokes());
        assert!(TextRenderMode::FillStrokeClip.clips());
        assert!(!TextRenderMode::Invisible.fills());
    }
}
