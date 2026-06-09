//! PDF Graphics State (ISO 32000-1 §8.4).
//!
//! Manages the current transformation matrix, colors, line style, and other
//! device-independent graphics parameters. Supports save/restore via a stack.

use crate::error::{PdfError, Result};

/// A 2D affine transformation matrix [a, b, c, d, e, f].
///
/// Represents the matrix:
/// ```text
/// | a  b  0 |
/// | c  d  0 |
/// | e  f  1 |
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    /// Identity matrix (no transformation).
    pub fn identity() -> Self {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Multiply this matrix by another: self * other.
    pub fn concat(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Transform a point (x, y) by this matrix.
    pub fn transform_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::identity()
    }
}

/// A color value in a device color space.
#[derive(Debug, Clone, PartialEq)]
pub enum Color {
    /// DeviceGray: single component [0.0, 1.0].
    Gray(f64),
    /// DeviceRGB: three components [0.0, 1.0] each.
    Rgb(f64, f64, f64),
    /// DeviceCMYK: four components [0.0, 1.0] each.
    Cmyk(f64, f64, f64, f64),
    /// Pattern color space (name reference, optional tint for uncoloured tiling patterns).
    /// The tint is the numeric prefix from `scn` / `SCN` (e.g. `r g b /P1 scn`).
    Pattern(String, Option<Vec<f64>>),
}

impl Default for Color {
    fn default() -> Self {
        Color::Gray(0.0)
    }
}

impl Color {
    /// Convert to RGBA (0-255 per channel). Alpha is always 255.
    pub fn to_rgba(&self) -> [u8; 4] {
        match self {
            Color::Gray(g) => {
                let v = (g.clamp(0.0, 1.0) * 255.0) as u8;
                [v, v, v, 255]
            }
            Color::Rgb(r, g, b) => [
                (r.clamp(0.0, 1.0) * 255.0) as u8,
                (g.clamp(0.0, 1.0) * 255.0) as u8,
                (b.clamp(0.0, 1.0) * 255.0) as u8,
                255,
            ],
            Color::Cmyk(c, m, y, k) => {
                let r = (1.0 - c) * (1.0 - k);
                let g = (1.0 - m) * (1.0 - k);
                let b = (1.0 - y) * (1.0 - k);
                [
                    (r.clamp(0.0, 1.0) * 255.0) as u8,
                    (g.clamp(0.0, 1.0) * 255.0) as u8,
                    (b.clamp(0.0, 1.0) * 255.0) as u8,
                    255,
                ]
            }
            Color::Pattern(..) => [0, 0, 0, 255],
        }
    }
}

/// Line cap style (ISO 32000-1 §8.4.3.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineCap {
    Butt = 0,
    Round = 1,
    Square = 2,
}

#[allow(clippy::derivable_impls)]
impl Default for LineCap {
    fn default() -> Self {
        LineCap::Butt
    }
}

impl LineCap {
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => LineCap::Round,
            2 => LineCap::Square,
            _ => LineCap::Butt,
        }
    }
}

/// Line join style (ISO 32000-1 §8.4.3.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineJoin {
    Miter = 0,
    Round = 1,
    Bevel = 2,
}

#[allow(clippy::derivable_impls)]
impl Default for LineJoin {
    fn default() -> Self {
        LineJoin::Miter
    }
}

impl LineJoin {
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => LineJoin::Round,
            2 => LineJoin::Bevel,
            _ => LineJoin::Miter,
        }
    }
}

/// Dash pattern for stroked paths.
#[derive(Debug, Clone, PartialEq)]
pub struct DashPattern {
    /// Array of dash/gap lengths.
    pub array: Vec<f64>,
    /// Phase offset into the pattern.
    pub phase: f64,
}

impl Default for DashPattern {
    fn default() -> Self {
        DashPattern {
            array: Vec::new(),
            phase: 0.0,
        }
    }
}

/// PDF blend mode (ISO 32000-1 §11.3.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

#[allow(clippy::derivable_impls)]
impl Default for BlendMode {
    fn default() -> Self {
        BlendMode::Normal
    }
}

impl BlendMode {
    pub fn from_name(name: &str) -> Self {
        match name {
            "Multiply" => BlendMode::Multiply,
            "Screen" => BlendMode::Screen,
            "Overlay" => BlendMode::Overlay,
            "Darken" => BlendMode::Darken,
            "Lighten" => BlendMode::Lighten,
            "ColorDodge" => BlendMode::ColorDodge,
            "ColorBurn" => BlendMode::ColorBurn,
            "HardLight" => BlendMode::HardLight,
            "SoftLight" => BlendMode::SoftLight,
            "Difference" => BlendMode::Difference,
            "Exclusion" => BlendMode::Exclusion,
            _ => BlendMode::Normal,
        }
    }
}

/// A single point in a path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A path segment.
#[derive(Debug, Clone, PartialEq)]
pub enum PathSegment {
    MoveTo(Point),
    LineTo(Point),
    CurveTo(Point, Point, Point),
    ClosePath,
}

/// Fill rule for path painting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// The current path being constructed.
#[derive(Debug, Clone, Default)]
pub struct Path {
    pub segments: Vec<PathSegment>,
}

impl Path {
    pub fn new() -> Self {
        Path {
            segments: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn clear(&mut self) {
        self.segments.clear();
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.segments.push(PathSegment::MoveTo(Point { x, y }));
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        self.segments.push(PathSegment::LineTo(Point { x, y }));
    }

    pub fn curve_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) {
        self.segments.push(PathSegment::CurveTo(
            Point { x: x1, y: y1 },
            Point { x: x2, y: y2 },
            Point { x: x3, y: y3 },
        ));
    }

    pub fn close(&mut self) {
        self.segments.push(PathSegment::ClosePath);
    }

    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.close();
    }
}

/// A clip region frozen with the CTM that was active when `W`/`W*` was applied.
///
/// Storing the CTM here lets `build_clip_mask` use the correct transform for
/// each clip layer even if `cm` operators change the CTM after the clip was set.
#[derive(Debug, Clone)]
pub struct ClipEntry {
    pub path: Path,
    pub rule: FillRule,
    /// CTM at the time the clip was established.
    pub ctm: Matrix,
}

/// The full graphics state (ISO 32000-1 §8.4.1, Table 52).
#[derive(Debug, Clone)]
pub struct GraphicsState {
    /// Current transformation matrix.
    pub ctm: Matrix,
    /// Stroke color.
    pub stroke_color: Color,
    /// Fill color.
    pub fill_color: Color,
    /// Stroke color space name.
    pub stroke_color_space: String,
    /// Fill color space name.
    pub fill_color_space: String,
    /// Line width.
    pub line_width: f64,
    /// Line cap style.
    pub line_cap: LineCap,
    /// Line join style.
    pub line_join: LineJoin,
    /// Miter limit.
    pub miter_limit: f64,
    /// Dash pattern.
    pub dash_pattern: DashPattern,
    /// Stroke alpha (0.0 = transparent, 1.0 = opaque).
    pub stroke_alpha: f64,
    /// Fill alpha.
    pub fill_alpha: f64,
    /// Blend mode.
    pub blend_mode: BlendMode,
    /// Active clip layers (W/W* and Form BBox clips), intersected during rendering.
    pub clip_path: Vec<ClipEntry>,
    /// Flatness tolerance.
    pub flatness: f64,
    /// Rendering intent name.
    pub rendering_intent: String,
}

impl Default for GraphicsState {
    fn default() -> Self {
        GraphicsState {
            ctm: Matrix::identity(),
            stroke_color: Color::Gray(0.0),
            fill_color: Color::Gray(0.0),
            stroke_color_space: "DeviceGray".to_string(),
            fill_color_space: "DeviceGray".to_string(),
            line_width: 1.0,
            line_cap: LineCap::default(),
            line_join: LineJoin::default(),
            miter_limit: 10.0,
            dash_pattern: DashPattern::default(),
            stroke_alpha: 1.0,
            fill_alpha: 1.0,
            blend_mode: BlendMode::Normal,
            clip_path: Vec::new(),
            flatness: 0.0,
            rendering_intent: "RelativeColorimetric".to_string(),
        }
    }
}

/// A stack of graphics states supporting save (q) and restore (Q).
#[derive(Debug)]
pub struct GraphicsStateStack {
    stack: Vec<GraphicsState>,
    pub current: GraphicsState,
}

impl GraphicsStateStack {
    pub fn new() -> Self {
        GraphicsStateStack {
            stack: Vec::new(),
            current: GraphicsState::default(),
        }
    }

    /// Save the current state (PDF `q` operator).
    pub fn save(&mut self) {
        self.stack.push(self.current.clone());
    }

    /// Restore the previously saved state (PDF `Q` operator).
    ///
    /// Returns an error if the stack is empty (unbalanced q/Q).
    pub fn restore(&mut self) -> Result<()> {
        match self.stack.pop() {
            Some(state) => {
                self.current = state;
                Ok(())
            }
            None => Err(PdfError::invalid_token(
                0,
                "graphics state stack underflow (unbalanced q/Q)",
            )),
        }
    }

    /// Current stack depth.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

impl Default for GraphicsStateStack {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matrix_identity() {
        let m = Matrix::identity();
        let (x, y) = m.transform_point(3.0, 4.0);
        assert_eq!(x, 3.0);
        assert_eq!(y, 4.0);
    }

    #[test]
    fn test_matrix_translation() {
        let m = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 10.0,
            f: 20.0,
        };
        let (x, y) = m.transform_point(5.0, 5.0);
        assert_eq!(x, 15.0);
        assert_eq!(y, 25.0);
    }

    #[test]
    fn test_matrix_concat() {
        let scale = Matrix {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            e: 0.0,
            f: 0.0,
        };
        let translate = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 100.0,
            f: 200.0,
        };
        // scale first, then translate: (1,1) → (2,2) → (102, 202)
        let combined = scale.concat(&translate);
        let (x, y) = combined.transform_point(1.0, 1.0);
        assert!((x - 102.0).abs() < 1e-10);
        assert!((y - 202.0).abs() < 1e-10);
    }

    #[test]
    fn test_color_gray_to_rgba() {
        assert_eq!(Color::Gray(1.0).to_rgba(), [255, 255, 255, 255]);
        assert_eq!(Color::Gray(0.0).to_rgba(), [0, 0, 0, 255]);
    }

    #[test]
    fn test_color_rgb_to_rgba() {
        assert_eq!(Color::Rgb(1.0, 0.0, 0.0).to_rgba(), [255, 0, 0, 255]);
    }

    #[test]
    fn test_color_cmyk_to_rgba() {
        // Pure black in CMYK
        let rgba = Color::Cmyk(0.0, 0.0, 0.0, 1.0).to_rgba();
        assert_eq!(rgba, [0, 0, 0, 255]);
        // Pure white in CMYK
        let rgba = Color::Cmyk(0.0, 0.0, 0.0, 0.0).to_rgba();
        assert_eq!(rgba, [255, 255, 255, 255]);
    }

    #[test]
    fn test_graphics_state_stack() {
        let mut stack = GraphicsStateStack::new();
        stack.current.line_width = 5.0;
        stack.save();
        stack.current.line_width = 10.0;
        assert_eq!(stack.current.line_width, 10.0);
        stack.restore().unwrap();
        assert_eq!(stack.current.line_width, 5.0);
    }

    #[test]
    fn test_graphics_state_stack_underflow() {
        let mut stack = GraphicsStateStack::new();
        assert!(stack.restore().is_err());
    }

    #[test]
    fn test_path_rect() {
        let mut path = Path::new();
        path.rect(0.0, 0.0, 100.0, 50.0);
        assert_eq!(path.segments.len(), 5); // move + 3 lines + close
    }
}
