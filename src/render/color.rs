//! Color space conversions for rendering.
//!
//! Extends `Color::to_rgba()` (which handles Gray/RGB/CMYK without alpha) with
//! alpha-aware variants and an explicit CMYK→RGBA path.

use crate::content::graphics_state::Color;

/// Convert a CMYK color to RGBA bytes, applying the given alpha [0.0, 1.0].
pub fn cmyk_to_rgba(c: f64, m: f64, y: f64, k: f64, alpha: f64) -> [u8; 4] {
    let r = (1.0 - c.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
    let g = (1.0 - m.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
    let b = (1.0 - y.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
    [
        (r * 255.0) as u8,
        (g * 255.0) as u8,
        (b * 255.0) as u8,
        (alpha.clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

/// Convert a grayscale value to RGBA bytes with the given alpha.
pub fn gray_to_rgba(g: f64, alpha: f64) -> [u8; 4] {
    let v = (g.clamp(0.0, 1.0) * 255.0) as u8;
    [v, v, v, (alpha.clamp(0.0, 1.0) * 255.0) as u8]
}

/// Convert a `Color` to RGBA bytes, applying a separate alpha channel.
///
/// The alpha from the `Color` enum itself (always 1.0 for device spaces) is
/// multiplied by the provided `alpha`, which comes from `GraphicsState::fill_alpha`
/// or `stroke_alpha`.
pub fn color_to_rgba(color: &Color, alpha: f64) -> [u8; 4] {
    match color {
        Color::Gray(g) => gray_to_rgba(*g, alpha),
        Color::Rgb(r, g, b) => [
            (r.clamp(0.0, 1.0) * 255.0) as u8,
            (g.clamp(0.0, 1.0) * 255.0) as u8,
            (b.clamp(0.0, 1.0) * 255.0) as u8,
            (alpha.clamp(0.0, 1.0) * 255.0) as u8,
        ],
        Color::Cmyk(c, m, y, k) => cmyk_to_rgba(*c, *m, *y, *k, alpha),
        // Patterns are handled by fill_path_with_pattern; if color_to_rgba is called for
        // a pattern colour it means the path renderer has no pattern handler (e.g. stroke),
        // so return fully transparent rather than black to avoid spurious dark artefacts.
        Color::Pattern(..) => [0, 0, 0, 0],
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmyk_white() {
        assert_eq!(cmyk_to_rgba(0.0, 0.0, 0.0, 0.0, 1.0), [255, 255, 255, 255]);
    }

    #[test]
    fn test_cmyk_black() {
        assert_eq!(cmyk_to_rgba(0.0, 0.0, 0.0, 1.0, 1.0), [0, 0, 0, 255]);
    }

    #[test]
    fn test_gray_midtone() {
        let [r, g, b, a] = gray_to_rgba(0.5, 1.0);
        assert_eq!(r, g);
        assert_eq!(g, b);
        assert_eq!(a, 255);
        // 0.5 * 255 = 127
        assert_eq!(r, 127);
    }

    #[test]
    fn test_color_to_rgba_rgb() {
        let c = Color::Rgb(1.0, 0.0, 0.0);
        assert_eq!(color_to_rgba(&c, 1.0), [255, 0, 0, 255]);
    }

    #[test]
    fn test_color_to_rgba_alpha() {
        let c = Color::Gray(1.0);
        let [_, _, _, a] = color_to_rgba(&c, 0.5);
        assert_eq!(a, 127);
    }

    #[test]
    fn test_color_to_rgba_pattern_is_transparent() {
        // Pattern colours must return fully transparent rather than black
        // to avoid spurious dark artefacts when stroke_path calls color_to_rgba.
        let c = Color::Pattern("P1".to_string(), None);
        assert_eq!(color_to_rgba(&c, 1.0), [0, 0, 0, 0]);
        let c2 = Color::Pattern("P2".to_string(), Some(vec![0.5, 0.3, 0.1]));
        assert_eq!(color_to_rgba(&c2, 1.0), [0, 0, 0, 0]);
    }
}
