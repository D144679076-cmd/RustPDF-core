//! RGBA pixel buffer for page and tile rendering.
//!
//! `PixmapBuffer` wraps a `tiny_skia::Pixmap` and records the tile origin
//! (in page-pixel space) so that draw calls can translate coordinates into
//! tile-local space.  For full-page renders the origin is (0, 0).

use crate::content::graphics_state::BlendMode;
use crate::error::{PdfError, Result};

/// A tile origin and pixel dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileOrigin {
    /// Top-left x of this tile in page-pixel space.
    pub x: u32,
    /// Top-left y of this tile in page-pixel space.
    pub y: u32,
}

/// Raw RGBA pixel buffer, backed by a `tiny_skia::Pixmap`.
///
/// For tile-based rendering, `origin` records where the top-left pixel of
/// this buffer sits within the full page.  All draw helpers in `path_render`
/// and `page_renderer` incorporate the origin when building transforms so
/// callers do not need to translate coordinates themselves.
pub struct PixmapBuffer {
    /// Pixel width of this buffer (tile width, not full page width).
    pub width: u32,
    /// Pixel height of this buffer (tile height, not full page height).
    pub height: u32,
    /// Tile top-left in page-pixel space.  (0, 0) for full-page renders.
    pub origin: TileOrigin,
    pub(crate) inner: tiny_skia::Pixmap,
}

impl PixmapBuffer {
    /// Allocate a white-filled buffer of the given size with origin (0, 0).
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let mut pixmap = tiny_skia::Pixmap::new(width.max(1), height.max(1))
            .ok_or_else(|| PdfError::filter_error(0, "failed to allocate pixmap".to_string()))?;
        pixmap.fill(tiny_skia::Color::WHITE);
        Ok(PixmapBuffer {
            width: width.max(1),
            height: height.max(1),
            origin: TileOrigin { x: 0, y: 0 },
            inner: pixmap,
        })
    }

    /// Allocate a white-filled tile buffer with the given pixel origin.
    pub fn new_tile(width: u32, height: u32, origin: TileOrigin) -> Result<Self> {
        let mut buf = Self::new(width, height)?;
        buf.origin = origin;
        Ok(buf)
    }

    /// Raw RGBA bytes (pre-multiplied alpha, row-major).
    pub fn data(&self) -> &[u8] {
        self.inner.data()
    }

    /// Mutable reference to the underlying `tiny_skia::Pixmap`.
    pub(crate) fn pixmap_mut(&mut self) -> &mut tiny_skia::Pixmap {
        &mut self.inner
    }

    /// Allocate a fully-transparent (all-zero RGBA) buffer with the given origin.
    ///
    /// Used to create an offscreen canvas for transparency group compositing.
    pub fn new_transparent(width: u32, height: u32, origin: TileOrigin) -> Result<Self> {
        // tiny_skia::Pixmap::new() zero-initialises (transparent), so no extra fill needed.
        let pixmap = tiny_skia::Pixmap::new(width.max(1), height.max(1))
            .ok_or_else(|| PdfError::filter_error(0, "failed to allocate pixmap".to_string()))?;
        Ok(PixmapBuffer {
            width: width.max(1),
            height: height.max(1),
            origin,
            inner: pixmap,
        })
    }

    /// Porter-Duff source-over composite of `src` onto `self`.
    ///
    /// `alpha` scales the entire source layer's opacity (the group fill alpha).
    /// Blend modes other than Normal follow ISO 32000-1 §11.3.5.
    /// Both buffers store premultiplied RGBA (tiny_skia convention).
    pub fn composite_over(&mut self, src: &PixmapBuffer, alpha: f64, blend_mode: BlendMode) {
        let group_alpha = alpha.clamp(0.0, 1.0) as f32;
        let dst_data = self.inner.data_mut();
        let src_data = src.inner.data();
        let n = dst_data.len().min(src_data.len());
        let mut i = 0;
        while i + 3 < n {
            let src_a_raw = src_data[i + 3] as f32 / 255.0;
            let sa = src_a_raw * group_alpha;

            if sa < 1.0 / 255.0 {
                i += 4;
                continue;
            }

            // Unpremultiply source to get straight RGB
            let (sr, sg, sb) = if src_a_raw > 0.0 {
                let inv = 1.0 / src_a_raw;
                (
                    (src_data[i] as f32 / 255.0) * inv,
                    (src_data[i + 1] as f32 / 255.0) * inv,
                    (src_data[i + 2] as f32 / 255.0) * inv,
                )
            } else {
                (0.0, 0.0, 0.0)
            };

            let da = dst_data[i + 3] as f32 / 255.0;

            // Unpremultiply destination to get straight RGB
            let (dr, dg, db) = if da > 0.0 {
                let inv = 1.0 / da;
                (
                    (dst_data[i] as f32 / 255.0) * inv,
                    (dst_data[i + 1] as f32 / 255.0) * inv,
                    (dst_data[i + 2] as f32 / 255.0) * inv,
                )
            } else {
                (0.0, 0.0, 0.0)
            };

            // Apply blend mode on straight colors
            let (br, bg, bb) = match blend_mode {
                BlendMode::Multiply => (sr * dr, sg * dg, sb * db),
                BlendMode::Screen => (sr + dr - sr * dr, sg + dg - sg * dg, sb + db - sb * db),
                BlendMode::Darken => (sr.min(dr), sg.min(dg), sb.min(db)),
                BlendMode::Lighten => (sr.max(dr), sg.max(dg), sb.max(db)),
                BlendMode::Overlay => {
                    // Overlay(Cb,Cs) == HardLight(Cs,Cb) — roles swapped.
                    (
                        blend_hard_light(sr, dr),
                        blend_hard_light(sg, dg),
                        blend_hard_light(sb, db),
                    )
                }
                BlendMode::ColorDodge => (
                    blend_color_dodge(dr, sr),
                    blend_color_dodge(dg, sg),
                    blend_color_dodge(db, sb),
                ),
                BlendMode::ColorBurn => (
                    blend_color_burn(dr, sr),
                    blend_color_burn(dg, sg),
                    blend_color_burn(db, sb),
                ),
                BlendMode::HardLight => (
                    blend_hard_light(dr, sr),
                    blend_hard_light(dg, sg),
                    blend_hard_light(db, sb),
                ),
                BlendMode::SoftLight => (
                    blend_soft_light(dr, sr),
                    blend_soft_light(dg, sg),
                    blend_soft_light(db, sb),
                ),
                BlendMode::Difference => ((dr - sr).abs(), (dg - sg).abs(), (db - sb).abs()),
                BlendMode::Exclusion => (
                    dr + sr - 2.0 * dr * sr,
                    dg + sg - 2.0 * dg * sg,
                    db + sb - 2.0 * db * sb,
                ),
                BlendMode::Hue => {
                    let c = set_sat((sr, sg, sb), sat((dr, dg, db)));
                    set_lum(c, lum((dr, dg, db)))
                }
                BlendMode::Saturation => {
                    let c = set_sat((dr, dg, db), sat((sr, sg, sb)));
                    set_lum(c, lum((dr, dg, db)))
                }
                BlendMode::Color => set_lum((sr, sg, sb), lum((dr, dg, db))),
                BlendMode::Luminosity => set_lum((dr, dg, db), lum((sr, sg, sb))),
                BlendMode::Normal => (sr, sg, sb),
            };

            // Porter-Duff source-over in straight space
            let inv_sa = 1.0 - sa;
            let out_a = sa + da * inv_sa;
            let (out_r, out_g, out_b) = if out_a > 0.0 {
                (
                    (br * sa + dr * da * inv_sa) / out_a,
                    (bg * sa + dg * da * inv_sa) / out_a,
                    (bb * sa + db * da * inv_sa) / out_a,
                )
            } else {
                (0.0, 0.0, 0.0)
            };

            // Write back as premultiplied RGBA
            dst_data[i] = (out_r * out_a * 255.0).clamp(0.0, 255.0) as u8;
            dst_data[i + 1] = (out_g * out_a * 255.0).clamp(0.0, 255.0) as u8;
            dst_data[i + 2] = (out_b * out_a * 255.0).clamp(0.0, 255.0) as u8;
            dst_data[i + 3] = (out_a * 255.0).clamp(0.0, 255.0) as u8;
            i += 4;
        }
    }

    /// Blit an RGBA byte slice (width × height × 4) at page-pixel position (px, py).
    ///
    /// Out-of-bounds pixels are silently clipped to the buffer bounds.
    pub fn blit_rgba(&mut self, px: i32, py: i32, src: &[u8], src_w: u32, src_h: u32) {
        for row in 0..src_h as i32 {
            let dst_y = py + row - self.origin.y as i32;
            if dst_y < 0 || dst_y >= self.height as i32 {
                continue;
            }
            for col in 0..src_w as i32 {
                let dst_x = px + col - self.origin.x as i32;
                if dst_x < 0 || dst_x >= self.width as i32 {
                    continue;
                }
                let src_idx = (row as usize * src_w as usize + col as usize) * 4;
                if src_idx + 3 >= src.len() {
                    continue;
                }
                let r = src[src_idx];
                let g = src[src_idx + 1];
                let b = src[src_idx + 2];
                let a = src[src_idx + 3];
                let dst_idx = (dst_y as usize * self.width as usize + dst_x as usize) * 4;
                let dst = self.inner.data_mut();
                if dst_idx + 3 < dst.len() {
                    // Porter-Duff source-over on pre-multiplied values
                    let a_f = a as f32 / 255.0;
                    let inv_a = 1.0 - a_f;
                    dst[dst_idx] = (r as f32 * a_f + dst[dst_idx] as f32 * inv_a) as u8;
                    dst[dst_idx + 1] = (g as f32 * a_f + dst[dst_idx + 1] as f32 * inv_a) as u8;
                    dst[dst_idx + 2] = (b as f32 * a_f + dst[dst_idx + 2] as f32 * inv_a) as u8;
                    dst[dst_idx + 3] = (a_f * 255.0 + dst[dst_idx + 3] as f32 * inv_a) as u8;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Blend-mode functions — ISO 32000-1 §11.3.5
// ---------------------------------------------------------------------------

/// Separable modes operate component-wise; `cb` is the backdrop component
/// (destination), `cs` the source component.
fn blend_hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        // Multiply-like on the dark half
        cb * 2.0 * cs
    } else {
        // Screen-like on the light half
        cb + 2.0 * cs - 1.0 - cb * (2.0 * cs - 1.0)
    }
}

fn blend_color_dodge(cb: f32, cs: f32) -> f32 {
    if cb <= 0.0 {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

fn blend_color_burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 {
        1.0
    } else if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

fn blend_soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

/// Luminance of a straight RGB triple (spec weights 0.3 / 0.59 / 0.11).
fn lum(c: (f32, f32, f32)) -> f32 {
    0.3 * c.0 + 0.59 * c.1 + 0.11 * c.2
}

/// Saturation = max component − min component.
fn sat(c: (f32, f32, f32)) -> f32 {
    c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2)
}

/// Clip components back into the gamut after a luminance shift, per spec:
/// scale the chroma toward the luminance axis so min ≥ 0 and max ≤ 1.
fn clip_color(c: (f32, f32, f32)) -> (f32, f32, f32) {
    let l = lum(c);
    let n = c.0.min(c.1).min(c.2);
    let mut c = c;
    if n < 0.0 && l - n != 0.0 {
        c = (
            l + (c.0 - l) * l / (l - n),
            l + (c.1 - l) * l / (l - n),
            l + (c.2 - l) * l / (l - n),
        );
    }
    let l = lum(c);
    let x = c.0.max(c.1).max(c.2);
    if x > 1.0 && x - l != 0.0 {
        c = (
            l + (c.0 - l) * (1.0 - l) / (x - l),
            l + (c.1 - l) * (1.0 - l) / (x - l),
            l + (c.2 - l) * (1.0 - l) / (x - l),
        );
    }
    c
}

/// Shift a colour's luminance to `l`, preserving hue and saturation.
fn set_lum(c: (f32, f32, f32), l: f32) -> (f32, f32, f32) {
    let d = l - lum(c);
    clip_color((c.0 + d, c.1 + d, c.2 + d))
}

/// Rescale a colour's saturation to `s`, preserving hue and luminance.
/// The smallest component becomes 0 and the largest becomes `s`.
fn set_sat(c: (f32, f32, f32), s: f32) -> (f32, f32, f32) {
    let arr = [c.0, c.1, c.2];
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| {
        arr[a]
            .partial_cmp(&arr[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let (mn, md, mx) = (arr[idx[0]], arr[idx[1]], arr[idx[2]]);
    let mut out = [0.0f32; 3];
    if mx > mn {
        out[idx[1]] = s * (md - mn) / (mx - mn);
        out[idx[2]] = s;
    }
    (out[0], out[1], out[2])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_canvas_is_white() {
        let buf = PixmapBuffer::new(10, 10).unwrap();
        let data = buf.data();
        assert_eq!(data.len(), 10 * 10 * 4);
        // All pixels should be white (255, 255, 255, 255) or pre-multiplied equivalent
        // tiny-skia stores WHITE as [255, 255, 255, 255] in RGBA
        assert_eq!(&data[0..4], &[255u8, 255, 255, 255]);
    }

    #[test]
    fn test_tile_origin() {
        let buf = PixmapBuffer::new_tile(50, 50, TileOrigin { x: 100, y: 200 }).unwrap();
        assert_eq!(buf.origin.x, 100);
        assert_eq!(buf.origin.y, 200);
        assert_eq!(buf.width, 50);
        assert_eq!(buf.height, 50);
    }

    #[test]
    fn test_blit_rgba_red_pixel() {
        let mut buf = PixmapBuffer::new(4, 4).unwrap();
        // Blit a single red fully-opaque pixel at (1, 1) page-pixel == tile-local (1,1)
        let red = [255u8, 0, 0, 255];
        buf.blit_rgba(1, 1, &red, 1, 1);
        let data = buf.data();
        // Pixel at (1,1) in row-major: index = (1*4 + 1) * 4 = 20
        assert_eq!(data[20], 255); // R
        assert_eq!(data[21], 0); // G
        assert_eq!(data[22], 0); // B
    }

    #[test]
    fn test_blit_rgba_out_of_bounds_is_safe() {
        let mut buf = PixmapBuffer::new(4, 4).unwrap();
        let red = [255u8, 0, 0, 255];
        // These should not panic
        buf.blit_rgba(-100, -100, &red, 1, 1);
        buf.blit_rgba(1000, 1000, &red, 1, 1);
    }

    #[test]
    fn test_new_transparent_is_all_zero() {
        let buf = PixmapBuffer::new_transparent(4, 4, TileOrigin { x: 0, y: 0 }).unwrap();
        let data = buf.data();
        assert_eq!(data.len(), 4 * 4 * 4);
        assert!(
            data.iter().all(|&b| b == 0),
            "transparent buffer should be all zeros"
        );
    }

    #[test]
    fn test_composite_over_normal_50pct_alpha() {
        // dst = opaque red (255,0,0,255), src = opaque blue (0,0,255,255) at 50% group alpha
        let mut dst = PixmapBuffer::new(1, 1).unwrap();
        // new() fills white; set dst pixel to opaque red manually
        {
            let d = dst.inner.data_mut();
            d[0] = 255;
            d[1] = 0;
            d[2] = 0;
            d[3] = 255;
        }
        let mut src = PixmapBuffer::new_transparent(1, 1, TileOrigin { x: 0, y: 0 }).unwrap();
        {
            let d = src.inner.data_mut();
            d[0] = 0;
            d[1] = 0;
            d[2] = 255;
            d[3] = 255;
        }
        dst.composite_over(&src, 0.5, BlendMode::Normal);
        let result = dst.data();
        // src effective alpha = 0.5, so result = 0.5*blue + 0.5*red
        // out_r = (0*0.5 + 255*1.0*0.5)/1.0 ≈ 127, out_b = (255*0.5 + 0)/1.0 ≈ 127
        assert!(
            result[0] > 100 && result[0] < 155,
            "red channel ~127, got {}",
            result[0]
        );
        assert!(
            result[2] > 100 && result[2] < 155,
            "blue channel ~127, got {}",
            result[2]
        );
        assert_eq!(result[3], 255, "alpha should be fully opaque");
    }

    #[test]
    fn test_composite_over_multiply() {
        // dst = grey (128,128,128,255), src = grey (128,128,128,255) at 100% alpha, Multiply
        let mut dst = PixmapBuffer::new(1, 1).unwrap();
        {
            let d = dst.inner.data_mut();
            d[0] = 128;
            d[1] = 128;
            d[2] = 128;
            d[3] = 255;
        }
        let mut src = PixmapBuffer::new_transparent(1, 1, TileOrigin { x: 0, y: 0 }).unwrap();
        {
            let d = src.inner.data_mut();
            d[0] = 128;
            d[1] = 128;
            d[2] = 128;
            d[3] = 255;
        }
        dst.composite_over(&src, 1.0, BlendMode::Multiply);
        let result = dst.data();
        // 0.502 * 0.502 ≈ 0.252, so ~64
        assert!(
            result[0] < 80,
            "multiply blend should darken: got {}",
            result[0]
        );
    }

    /// Composite two opaque 1×1 pixels with `mode` and return the result.
    fn blend_1x1(dst_px: [u8; 4], src_px: [u8; 4], mode: BlendMode) -> [u8; 4] {
        let mut dst = PixmapBuffer::new(1, 1).unwrap();
        {
            let d = dst.inner.data_mut();
            d.copy_from_slice(&dst_px);
        }
        let mut src = PixmapBuffer::new_transparent(1, 1, TileOrigin { x: 0, y: 0 }).unwrap();
        {
            let d = src.inner.data_mut();
            d.copy_from_slice(&src_px);
        }
        dst.composite_over(&src, 1.0, mode);
        let r = dst.data();
        [r[0], r[1], r[2], r[3]]
    }

    #[test]
    fn blend_difference_red_blue_is_magenta() {
        let out = blend_1x1([255, 0, 0, 255], [0, 0, 255, 255], BlendMode::Difference);
        assert_eq!(out, [255, 0, 255, 255]);
    }

    #[test]
    fn blend_exclusion_mid_gray_stays_mid_gray() {
        let out = blend_1x1(
            [128, 128, 128, 255],
            [128, 128, 128, 255],
            BlendMode::Exclusion,
        );
        // 0.502 + 0.502 − 2·0.502² ≈ 0.5 → ~128
        assert!(out[0] > 115 && out[0] < 140, "got {}", out[0]);
    }

    #[test]
    fn blend_color_dodge_scales_by_one_minus_source() {
        // cb = 64/255 ≈ 0.251, cs = 0.5 → 0.251/0.5 ≈ 0.502 → ~128
        let out = blend_1x1(
            [64, 64, 64, 255],
            [128, 128, 128, 255],
            BlendMode::ColorDodge,
        );
        assert!(out[0] > 115 && out[0] < 140, "got {}", out[0]);
    }

    #[test]
    fn blend_color_dodge_zero_backdrop_stays_zero() {
        let out = blend_1x1([0, 0, 0, 255], [128, 128, 128, 255], BlendMode::ColorDodge);
        assert_eq!(out, [0, 0, 0, 255]);
    }

    #[test]
    fn blend_color_burn_full_source_is_backdrop() {
        // cs = 1 → 1 − (1−cb)/1 = cb
        let out = blend_1x1(
            [128, 128, 128, 255],
            [255, 255, 255, 255],
            BlendMode::ColorBurn,
        );
        assert!(out[0] > 115 && out[0] < 140, "got {}", out[0]);
    }

    #[test]
    fn blend_color_burn_full_backdrop_stays_white() {
        let out = blend_1x1(
            [255, 255, 255, 255],
            [128, 128, 128, 255],
            BlendMode::ColorBurn,
        );
        assert_eq!(out, [255, 255, 255, 255]);
    }

    #[test]
    fn blend_hard_light_screen_half() {
        // cb ≈ 0.251, cs ≈ 0.749 → screen: cb + (2cs−1)(1−cb) ≈ 0.624 → ~159
        let out = blend_1x1(
            [64, 64, 64, 255],
            [191, 191, 191, 255],
            BlendMode::HardLight,
        );
        assert!(out[0] > 145 && out[0] < 175, "got {}", out[0]);
    }

    #[test]
    fn blend_soft_light_dark_source_darkens() {
        // cb ≈ 0.502, cs ≈ 0.251 → cb − (1−2cs)·cb·(1−cb) ≈ 0.378 → ~96
        let out = blend_1x1(
            [128, 128, 128, 255],
            [64, 64, 64, 255],
            BlendMode::SoftLight,
        );
        assert!(out[0] > 85 && out[0] < 108, "got {}", out[0]);
    }

    #[test]
    fn blend_luminosity_gray_over_red() {
        // lum(gray)=0.5 shifted into red's chroma: clip to (1, ~0.286, ~0.286)
        let out = blend_1x1(
            [255, 0, 0, 255],
            [128, 128, 128, 255],
            BlendMode::Luminosity,
        );
        assert!(out[0] > 240, "r got {}", out[0]);
        assert!(out[1] > 60 && out[1] < 90, "g got {}", out[1]);
        assert!(out[2] > 60 && out[2] < 90, "b got {}", out[2]);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn blend_color_gray_over_red_uses_backdrop_luminance() {
        // Color(Cb,Cs) = SetLum(Cs, Lum(Cb)): gray source at red's luminance 0.3 → ~77
        let out = blend_1x1([255, 0, 0, 255], [128, 128, 128, 255], BlendMode::Color);
        assert!(out[0] > 65 && out[0] < 90, "got {}", out[0]);
        assert_eq!(out[1], out[0]);
        assert_eq!(out[2], out[0]);
    }

    #[test]
    fn blend_hue_blue_over_red() {
        // Hue: blue's hue with red's saturation and luminosity → (≈0.21, ≈0.21, 1)
        let out = blend_1x1([255, 0, 0, 255], [0, 0, 255, 255], BlendMode::Hue);
        assert_eq!(out[2], 255, "b got {}", out[2]);
        assert!(out[0] > 40 && out[0] < 70, "r got {}", out[0]);
        assert_eq!(out[1], out[0]);
    }

    #[test]
    fn blend_saturation_zero_source_desaturates_backdrop() {
        // Saturation: gray backdrop has 0 saturation → black, then gray's luminance → ~128
        let out = blend_1x1(
            [128, 128, 128, 255],
            [255, 0, 0, 255],
            BlendMode::Saturation,
        );
        assert!(out[0] > 115 && out[0] < 140, "got {}", out[0]);
        assert_eq!(out[1], out[0]);
        assert_eq!(out[2], out[0]);
    }

    #[test]
    fn blend_mode_from_name_parses_all_non_separable() {
        use crate::content::graphics_state::BlendMode;
        assert_eq!(BlendMode::from_name("Hue"), BlendMode::Hue);
        assert_eq!(BlendMode::from_name("Saturation"), BlendMode::Saturation);
        assert_eq!(BlendMode::from_name("Color"), BlendMode::Color);
        assert_eq!(BlendMode::from_name("Luminosity"), BlendMode::Luminosity);
        assert_eq!(BlendMode::from_name("ColorDodge"), BlendMode::ColorDodge);
        assert_eq!(BlendMode::from_name("ColorBurn"), BlendMode::ColorBurn);
        assert_eq!(BlendMode::from_name("HardLight"), BlendMode::HardLight);
        assert_eq!(BlendMode::from_name("SoftLight"), BlendMode::SoftLight);
        assert_eq!(BlendMode::from_name("Difference"), BlendMode::Difference);
        assert_eq!(BlendMode::from_name("Exclusion"), BlendMode::Exclusion);
        assert_eq!(BlendMode::from_name("Bogus"), BlendMode::Normal);
    }
}
