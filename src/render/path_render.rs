//! Vector path rasterization using `tiny-skia`.
//!
//! Translates our `Path` + `GraphicsState` into tiny-skia draw calls on a
//! `PixmapBuffer`.  The `transform` argument is the fully-composed device
//! transform (initial CTM × any `cm` operators), already incorporating the
//! Y-flip and scale factor.

use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Shader, Stroke, StrokeDash, Transform,
};

use crate::content::graphics_state::{
    ClipEntry, FillRule as PdfFillRule, GraphicsState, LineCap as PdfLineCap,
    LineJoin as PdfLineJoin, Path, PathSegment,
};

use super::canvas::PixmapBuffer;
use super::color::color_to_rgba;

fn make_paint(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(tiny_skia::Color::from_rgba8(r, g, b, a)),
        anti_alias: true,
        ..Paint::default()
    }
}

/// Fill a path using the current graphics state fill color (NonZero rule).
pub fn fill_path(
    path: &Path,
    gfx: &GraphicsState,
    transform: &crate::content::graphics_state::Matrix,
    canvas: &mut PixmapBuffer,
) {
    fill_path_with_rule(path, gfx, PdfFillRule::NonZero, transform, canvas);
}

/// Fill a path with an explicit fill rule (NonZero or EvenOdd).
pub fn fill_path_with_rule(
    path: &Path,
    gfx: &GraphicsState,
    rule: PdfFillRule,
    transform: &crate::content::graphics_state::Matrix,
    canvas: &mut PixmapBuffer,
) {
    let Some(sk_path) = build_skia_path(path) else {
        return;
    };
    let [r, g, b, a] = color_to_rgba(&gfx.fill_color, gfx.fill_alpha);
    if a == 0 {
        return;
    }
    let paint = make_paint(r, g, b, a);
    let sk_rule = match rule {
        PdfFillRule::NonZero => FillRule::Winding,
        PdfFillRule::EvenOdd => FillRule::EvenOdd,
    };
    let sk_transform = matrix_to_transform(transform);
    let clip_mask = build_clip_mask(&gfx.clip_path, canvas.width, canvas.height);
    canvas
        .pixmap_mut()
        .fill_path(&sk_path, &paint, sk_rule, sk_transform, clip_mask.as_ref());
}

/// Stroke a path using the current graphics state stroke color and line style.
pub fn stroke_path(
    path: &Path,
    gfx: &GraphicsState,
    transform: &crate::content::graphics_state::Matrix,
    canvas: &mut PixmapBuffer,
) {
    let Some(sk_path) = build_skia_path(path) else {
        return;
    };
    let [r, g, b, a] = color_to_rgba(&gfx.stroke_color, gfx.stroke_alpha);
    if a == 0 {
        return;
    }
    let paint = make_paint(r, g, b, a);

    let dash = if gfx.dash_pattern.array.is_empty() {
        None
    } else {
        let intervals: Vec<f32> = gfx.dash_pattern.array.iter().map(|&v| v as f32).collect();
        StrokeDash::new(intervals, gfx.dash_pattern.phase as f32)
    };

    let stroke = Stroke {
        width: (gfx.line_width as f32).max(0.001),
        miter_limit: gfx.miter_limit as f32,
        line_cap: pdf_line_cap(gfx.line_cap),
        line_join: pdf_line_join(gfx.line_join),
        dash,
    };

    let sk_transform = matrix_to_transform(transform);
    let clip_mask = build_clip_mask(&gfx.clip_path, canvas.width, canvas.height);
    canvas
        .pixmap_mut()
        .stroke_path(&sk_path, &paint, &stroke, sk_transform, clip_mask.as_ref());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a tiny_skia clip mask by intersecting all active clip layers.
///
/// Each `ClipEntry` carries the path in user space and the CTM that was active
/// when the clip was established (frozen at `W`/`W*` time).  Using the frozen
/// CTM ensures clips are placed correctly even when `cm` operators change the
/// current CTM between the `W` call and the painting operator.
///
/// Returns `None` when there are no clips (the common case), so callers can
/// pass the result directly as the `clip_mask` argument to tiny_skia draw calls.
fn build_clip_mask(clips: &[ClipEntry], width: u32, height: u32) -> Option<tiny_skia::Mask> {
    if clips.is_empty() {
        return None;
    }
    let mut mask = tiny_skia::Mask::new(width, height)?;
    // tiny_skia::Mask::new() zero-initialises the buffer (all transparent = no
    // area visible).  We want to start fully-open and intersect, so fill to 255.
    for byte in mask.data_mut() {
        *byte = 255;
    }
    for entry in clips {
        let sk_clip = match build_skia_path(&entry.path) {
            Some(p) => p,
            None => continue,
        };
        let sk_rule = match entry.rule {
            PdfFillRule::NonZero => FillRule::Winding,
            PdfFillRule::EvenOdd => FillRule::EvenOdd,
        };
        // Use the CTM frozen at W-time so the clip doesn't drift when the CTM
        // changes later via cm operators.
        let transform = matrix_to_transform(&entry.ctm);
        let mut layer = tiny_skia::Mask::new(width, height)?;
        layer.fill_path(&sk_clip, sk_rule, true, transform);
        // Intersection: keep the minimum coverage of the two layers.
        for (m, l) in mask.data_mut().iter_mut().zip(layer.data().iter()) {
            *m = (*m).min(*l);
        }
    }
    Some(mask)
}

pub(crate) fn build_skia_path(path: &Path) -> Option<tiny_skia::Path> {
    if path.segments.is_empty() {
        return None;
    }
    let mut pb = PathBuilder::new();
    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(p) => pb.move_to(p.x as f32, p.y as f32),
            PathSegment::LineTo(p) => pb.line_to(p.x as f32, p.y as f32),
            PathSegment::CurveTo(p1, p2, p3) => pb.cubic_to(
                p1.x as f32,
                p1.y as f32,
                p2.x as f32,
                p2.y as f32,
                p3.x as f32,
                p3.y as f32,
            ),
            PathSegment::ClosePath => pb.close(),
        }
    }
    pb.finish()
}

/// Convert our `Matrix` to a tiny-skia `Transform`.
///
/// Both use the same [a, b, c, d, e, f] affine layout.  tiny-skia uses f32.
pub(crate) fn matrix_to_transform(m: &crate::content::graphics_state::Matrix) -> Transform {
    Transform::from_row(
        m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32,
    )
}

fn pdf_line_cap(cap: PdfLineCap) -> LineCap {
    match cap {
        PdfLineCap::Butt => LineCap::Butt,
        PdfLineCap::Round => LineCap::Round,
        PdfLineCap::Square => LineCap::Square,
    }
}

fn pdf_line_join(join: PdfLineJoin) -> LineJoin {
    match join {
        PdfLineJoin::Miter => LineJoin::Miter,
        PdfLineJoin::Round => LineJoin::Round,
        PdfLineJoin::Bevel => LineJoin::Bevel,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::graphics_state::{Color, GraphicsState, Matrix, Path};
    use crate::render::canvas::PixmapBuffer;

    fn identity() -> Matrix {
        Matrix::identity()
    }

    fn make_rect_path() -> Path {
        let mut p = Path::new();
        p.rect(10.0, 10.0, 30.0, 30.0);
        p
    }

    #[test]
    fn test_fill_rect_path() {
        let mut canvas = PixmapBuffer::new(60, 60).unwrap();
        let mut gfx = GraphicsState::default();
        gfx.fill_color = Color::Rgb(1.0, 0.0, 0.0);
        let path = make_rect_path();
        fill_path_with_rule(&path, &gfx, PdfFillRule::NonZero, &identity(), &mut canvas);
        // Center pixel at (25, 25) should be red
        let data = canvas.data();
        let idx = (25 * 60 + 25) * 4;
        assert_eq!(data[idx], 255, "R should be 255");
        assert_eq!(data[idx + 1], 0, "G should be 0");
        assert_eq!(data[idx + 2], 0, "B should be 0");
    }

    #[test]
    fn test_stroke_rect_path() {
        let mut canvas = PixmapBuffer::new(60, 60).unwrap();
        let mut gfx = GraphicsState::default();
        gfx.stroke_color = Color::Rgb(0.0, 0.0, 1.0);
        gfx.line_width = 2.0;
        let path = make_rect_path();
        stroke_path(&path, &gfx, &identity(), &mut canvas);
        let data = canvas.data();
        let has_blue = data.chunks(4).any(|px| px[2] > 100 && px[0] < 100);
        assert!(has_blue, "expected blue stroke pixels");
    }
}
