//! Page renderer: implements `OutputDevice` and drives `render_tile` / `render_page`.
//!
//! ## Coordinate system
//!
//! PDF user space has its origin at the lower-left corner of the page, with Y
//! increasing upward.  Screen (pixel) space has its origin at the upper-left,
//! with Y increasing downward.
//!
//! The initial CTM applied at the start of every render encodes both the DPI
//! scale factor and the Y-flip:
//!
//! ```text
//! initial_ctm = [scale, 0, 0, -scale, -tile_x * scale, (tile_y + tile_h) * scale]
//! ```
//!
//! This maps PDF point (x, y) → tile-local pixel (x*s - tile_x*s, (tile_y+tile_h-y)*s).
//! All subsequent `cm` operators stack on top via `Matrix::concat`, so after
//! interpretation every position in `TextSpan.x / .y` and in path coordinates
//! is already in tile-local pixel space.

use std::sync::Arc;

use crate::content::graphics_state::{BlendMode, Color, FillRule, GraphicsState, Matrix, Path};
use crate::content::interpreter::{ContentInterpreter, OutputDevice};
use crate::content::operators::ContentStreamIter;
use crate::content::text_state::TextSpan;
use crate::document::page::Page;
use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject, PdfStream};

use super::canvas::{PixmapBuffer, TileOrigin};
use super::color::color_to_rgba;
use super::font_resolver::{normalize_font_name, EmbeddedFontResolver, FontResolver};
use super::glyph_cache::{FontBytesCache, GlyphCache, RenderCache};
use super::image::{apply_smask, decode_image, decode_image_cached, scale_gray_mask};
use super::path_render::{build_skia_path, fill_path_with_rule, matrix_to_transform, stroke_path};
use super::shading::Shading;
use super::tile::TileRect;

// ---------------------------------------------------------------------------
// Soft mask types
// ---------------------------------------------------------------------------

/// Which channel of the rendered mask form determines per-pixel opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SoftMaskType {
    /// Alpha channel of the mask form XObject image.
    Alpha,
    /// Luminosity of the mask form XObject image (0.2126R + 0.7152G + 0.0722B).
    Luminosity,
}

/// A rendered soft mask ready to modulate drawing output.
struct SoftMask {
    /// Premultiplied RGBA pixels of the rendered mask form, canvas-local coordinates.
    data: Vec<u8>,
    width: u32,
    height: u32,
    mask_type: SoftMaskType,
}

impl SoftMask {
    /// Sample the mask value (0–255) at canvas-local pixel (cx, cy).
    fn sample(&self, cx: i32, cy: i32) -> u8 {
        if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
            return 0;
        }
        let idx = (cy as u32 * self.width + cx as u32) as usize * 4;
        if idx + 3 >= self.data.len() {
            return 0;
        }
        match self.mask_type {
            SoftMaskType::Alpha => self.data[idx + 3],
            SoftMaskType::Luminosity => {
                let alpha = self.data[idx + 3];
                if alpha == 0 {
                    return 0;
                }
                // Un-premultiply to get straight RGB for luminosity calculation.
                let a = alpha as f32 / 255.0;
                let r = self.data[idx] as f32 / a;
                let g = self.data[idx + 1] as f32 / a;
                let b = self.data[idx + 2] as f32 / a;
                (0.2126 * r + 0.7152 * g + 0.0722 * b).min(255.0) as u8
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PageRenderer
// ---------------------------------------------------------------------------

/// Implements `OutputDevice` and paints into a `PixmapBuffer`.
///
/// Created fresh per `render_tile` call.
struct PageRenderer<'doc> {
    canvas: PixmapBuffer,
    glyph_cache: GlyphCache,
    scale: f32,
    /// Reference to the document, used for image XObject decoding.
    doc: &'doc PdfDocument,
    /// Raw resources dictionary for font / XObject lookup.
    /// Wrapped in `Arc` so tile renderers and pattern-fill sub-renderers can
    /// share the same allocation without cloning the full HashMap.
    resources_raw: Arc<PdfDict>,
    /// Fallback font resolver used when the PDF has no embedded font data.
    font_resolver: Box<dyn FontResolver>,
    /// Offscreen buffers for transparency group compositing.
    /// Each entry is (saved_canvas, group_fill_alpha, group_blend_mode).
    transparency_stack: Vec<(PixmapBuffer, f64, crate::content::graphics_state::BlendMode)>,
    /// Decoded font stream bytes keyed by PDF resource name.
    /// Populated lazily on first use; shared across tiles via `RenderCache`.
    font_bytes_cache: FontBytesCache,
    /// Scratch buffer reused across glyph blits — eliminates one heap allocation per glyph.
    blit_scratch: Vec<u8>,
    /// Stack of Form XObject resource dicts.  Pushed on `enter_form_resources`,
    /// popped on `exit_form_resources`.  The top entry is the "current" resources
    /// for pattern, font, and XObject lookup; falls back to `resources_raw`
    /// (page-level resources) when the stack is empty.
    resource_stack: Vec<Arc<PdfDict>>,
    /// Active ExtGState soft mask (ISO 32000-1 §11.6.5.2).  Set by `set_soft_mask`,
    /// cleared by `clear_soft_mask` or when `/SMask /None` appears in a `gs` operator.
    current_soft_mask: Option<SoftMask>,
}

impl<'doc> PageRenderer<'doc> {
    fn new(
        canvas: PixmapBuffer,
        scale: f32,
        doc: &'doc PdfDocument,
        resources_raw: Arc<PdfDict>,
    ) -> Self {
        PageRenderer {
            canvas,
            glyph_cache: GlyphCache::new(),
            scale,
            doc,
            resources_raw,
            font_resolver: Box::new(EmbeddedFontResolver),
            transparency_stack: Vec::new(),
            font_bytes_cache: FontBytesCache::new(),
            blit_scratch: Vec::new(),
            resource_stack: Vec::new(),
            current_soft_mask: None,
        }
    }

    fn with_resolver(
        canvas: PixmapBuffer,
        scale: f32,
        doc: &'doc PdfDocument,
        resources_raw: Arc<PdfDict>,
        font_resolver: Box<dyn FontResolver>,
    ) -> Self {
        PageRenderer {
            canvas,
            glyph_cache: GlyphCache::new(),
            scale,
            doc,
            resources_raw,
            font_resolver,
            transparency_stack: Vec::new(),
            font_bytes_cache: FontBytesCache::new(),
            blit_scratch: Vec::new(),
            resource_stack: Vec::new(),
            current_soft_mask: None,
        }
    }

    /// Construct using a caller-supplied [`RenderCache`] so both glyph bitmaps
    /// and decoded font bytes persist across multiple tile renders on the same page.
    fn new_with_render_cache(
        canvas: PixmapBuffer,
        scale: f32,
        doc: &'doc PdfDocument,
        resources_raw: Arc<PdfDict>,
        cache: RenderCache,
    ) -> Self {
        PageRenderer {
            canvas,
            glyph_cache: cache.glyphs,
            scale,
            doc,
            resources_raw,
            font_resolver: Box::new(EmbeddedFontResolver),
            transparency_stack: Vec::new(),
            font_bytes_cache: cache.font_bytes,
            blit_scratch: Vec::new(),
            resource_stack: Vec::new(),
            current_soft_mask: None,
        }
    }

    /// Construct using a caller-supplied `GlyphCache` so rasterised glyphs
    /// persist across multiple `render_tile_with_cache` calls on the same page.
    fn new_with_external_cache(
        canvas: PixmapBuffer,
        scale: f32,
        doc: &'doc PdfDocument,
        resources_raw: Arc<PdfDict>,
        glyph_cache: GlyphCache,
    ) -> Self {
        PageRenderer {
            canvas,
            glyph_cache,
            scale,
            doc,
            resources_raw,
            font_resolver: Box::new(EmbeddedFontResolver),
            transparency_stack: Vec::new(),
            font_bytes_cache: FontBytesCache::new(),
            blit_scratch: Vec::new(),
            resource_stack: Vec::new(),
            current_soft_mask: None,
        }
    }

    /// Return the effective resources for the current rendering scope.
    ///
    /// While inside a Form XObject, the form's own resources take precedence;
    /// falls back to the page-level resources when no form is active.
    fn current_resources(&self) -> &Arc<PdfDict> {
        self.resource_stack.last().unwrap_or(&self.resources_raw)
    }

    /// Apply `self.current_soft_mask` to every pixel of `canvas` in-place.
    ///
    /// Each pixel's alpha is multiplied by the mask value at the same canvas-local
    /// coordinates.  RGB channels are scaled by the same factor (premultiplied storage).
    fn apply_canvas_soft_mask(mask: &SoftMask, canvas: &mut PixmapBuffer) {
        let data = canvas.inner.data_mut();
        let w = canvas.width;
        let h = canvas.height;
        for cy in 0..h {
            for cx in 0..w {
                let idx = (cy * w + cx) as usize * 4;
                if idx + 3 >= data.len() {
                    break;
                }
                let m = mask.sample(cx as i32, cy as i32) as u32;
                data[idx] = ((data[idx] as u32 * m) / 255) as u8;
                data[idx + 1] = ((data[idx + 1] as u32 * m) / 255) as u8;
                data[idx + 2] = ((data[idx + 2] as u32 * m) / 255) as u8;
                data[idx + 3] = ((data[idx + 3] as u32 * m) / 255) as u8;
            }
        }
    }

    /// Apply `mask` to `rgba` (straight/non-premultiplied RGBA, row-major) that
    /// is about to be blitted at canvas-local position `(dst_x, dst_y)`.
    ///
    /// Pixels outside the mask bounds are made fully transparent.
    fn apply_mask_to_image(
        mask: &SoftMask,
        rgba: &mut Vec<u8>,
        dst_x: i32,
        dst_y: i32,
        dst_w: u32,
        dst_h: u32,
    ) {
        for py in 0..dst_h {
            for px in 0..dst_w {
                let cx = dst_x + px as i32;
                let cy = dst_y + py as i32;
                let m = mask.sample(cx, cy) as u32;
                let idx = (py * dst_w + px) as usize * 4;
                if idx + 3 >= rgba.len() {
                    break;
                }
                rgba[idx + 3] = ((rgba[idx + 3] as u32 * m) / 255) as u8;
            }
        }
    }

    /// Render the soft mask form XObject `form_stream` into a canvas-sized pixmap
    /// and store it as the active soft mask.
    fn render_soft_mask(&mut self, mask_type: SoftMaskType, form_stream: &PdfStream, ctm: &Matrix) {
        let w = self.canvas.width;
        let h = self.canvas.height;
        let origin = self.canvas.origin;

        let mask_canvas = match PixmapBuffer::new_transparent(w, h, origin) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[smask] alloc failed: {}", e);
                return;
            }
        };

        // Use the form's own Resources if available, else fall back to page resources.
        let form_resources = match form_stream.dict.get("Resources") {
            Some(PdfObject::Dictionary(d)) => Arc::new(d.clone()),
            Some(obj) => match self.doc.resolve(obj) {
                Ok(PdfObject::Dictionary(d)) => Arc::new(d),
                _ => Arc::clone(&self.resources_raw),
            },
            None => Arc::clone(&self.resources_raw),
        };

        let res_for_interp = Arc::clone(&form_resources);
        let doc = self.doc;

        let mut sub_renderer = PageRenderer {
            canvas: mask_canvas,
            glyph_cache: GlyphCache::new(),
            scale: self.scale,
            doc,
            resources_raw: form_resources,
            font_resolver: Box::new(EmbeddedFontResolver),
            transparency_stack: Vec::new(),
            font_bytes_cache: FontBytesCache::new(),
            blit_scratch: Vec::new(),
            resource_stack: Vec::new(),
            current_soft_mask: None,
        };

        let mut interp = ContentInterpreter::new();
        interp.gfx.current.ctm = *ctm;

        match form_stream.decode_with_doc(doc) {
            Ok(content) => {
                let iter = ContentStreamIter::new(&content);
                if let Err(e) = interp.interpret_iter(
                    iter,
                    &mut sub_renderer,
                    Some(doc),
                    Some(&*res_for_interp),
                ) {
                    log::warn!("[smask] form render error: {}", e);
                    return;
                }
            }
            Err(e) => {
                log::warn!("[smask] form decode error: {}", e);
                return;
            }
        }

        let mask_data = sub_renderer.canvas.data().to_vec();
        self.current_soft_mask = Some(SoftMask {
            data: mask_data,
            width: w,
            height: h,
            mask_type,
        });
        log::debug!("[smask] rendered {:?} mask {}×{}", mask_type, w, h);
    }
}

impl<'doc> OutputDevice for PageRenderer<'doc> {
    fn stroke_path(&mut self, path: &Path, state: &GraphicsState) {
        // Per-path [stroke] trace silenced (high frequency). Re-enable to debug paths:
        //   if log::log_enabled!(log::Level::Debug) {
        //       let dbb = path_device_bbox(path, &state.ctm);
        //       log::debug!("[stroke] color={:?} alpha={:.2} scale={:.2} \
        //           dev=[{:.0},{:.0},{:.0},{:.0}] clips={}", state.stroke_color,
        //           state.stroke_alpha, self.scale, dbb.0, dbb.1, dbb.2, dbb.3,
        //           state.clip_path.len());
        //   }
        if self.current_soft_mask.is_some() {
            let w = self.canvas.width;
            let h = self.canvas.height;
            let origin = self.canvas.origin;
            if let Ok(mut temp) = PixmapBuffer::new_transparent(w, h, origin) {
                stroke_path(path, state, &state.ctm, &mut temp);
                if let Some(ref mask) = self.current_soft_mask {
                    Self::apply_canvas_soft_mask(mask, &mut temp);
                }
                self.canvas.composite_over(&temp, 1.0, BlendMode::Normal);
            } else {
                stroke_path(path, state, &state.ctm, &mut self.canvas);
            }
        } else {
            stroke_path(path, state, &state.ctm, &mut self.canvas);
        }
    }

    fn fill_path(&mut self, path: &Path, state: &GraphicsState, rule: FillRule) {
        // Per-path [fill] trace silenced (high frequency). Re-enable to debug paths:
        //   if log::log_enabled!(log::Level::Debug) {
        //       let dbb = path_device_bbox(path, &state.ctm);
        //       log::debug!("[fill] color={:?} alpha={:.2} scale={:.2} \
        //           dev=[{:.0},{:.0},{:.0},{:.0}] clips={}", state.fill_color,
        //           state.fill_alpha, self.scale, dbb.0, dbb.1, dbb.2, dbb.3,
        //           state.clip_path.len());
        //   }
        if let Color::Pattern(ref name, ref tint) = state.fill_color {
            self.fill_path_with_pattern(path, state, rule, name.clone(), tint.clone());
            return;
        }
        if self.current_soft_mask.is_some() {
            let w = self.canvas.width;
            let h = self.canvas.height;
            let origin = self.canvas.origin;
            if let Ok(mut temp) = PixmapBuffer::new_transparent(w, h, origin) {
                fill_path_with_rule(path, state, rule, &state.ctm, &mut temp);
                if let Some(ref mask) = self.current_soft_mask {
                    Self::apply_canvas_soft_mask(mask, &mut temp);
                }
                self.canvas.composite_over(&temp, 1.0, BlendMode::Normal);
            } else {
                fill_path_with_rule(path, state, rule, &state.ctm, &mut self.canvas);
            }
        } else {
            fill_path_with_rule(path, state, rule, &state.ctm, &mut self.canvas);
        }
    }

    fn draw_text_span(&mut self, span: &TextSpan, state: &GraphicsState) {
        if span.text.is_empty() {
            return;
        }
        // Soft mask on text is not yet composited through a temp canvas (Phase 3).
        // Text appears unmasked, which is incorrect but better than invisible.
        if self.current_soft_mask.is_some() {
            log::debug!("[smask] soft mask ignored for text span (Phase 3 follow-up)");
        }
        // span.x, span.y are already in tile-local pixel space because the
        // initial CTM (with Y-flip and tile offset) was applied to the interpreter
        // before content stream execution began.
        let [r, g, b, a] = color_to_rgba(&state.fill_color, state.fill_alpha);
        if a == 0 {
            return;
        }

        // Font size in device pixels: pre-computed from the full render matrix
        // (font_size × text_matrix_scale × CTM_scale) at span creation time.
        let size_px = span.font_size_px.abs() as f32;
        if size_px < 1.0 {
            return;
        }

        // Populate font bytes cache on first use for this font resource name.
        // Avoids re-applying FlateDecode for every text span that uses the same font.
        // With RenderCache, this also persists across tile renders on the same page.
        if !self.font_bytes_cache.contains(&span.font_name) {
            let bytes = self.get_ttf_bytes(&span.font_name);
            self.font_bytes_cache.insert(span.font_name.clone(), bytes);
        }

        // Two-tier font resolution:
        //   Tier 1 — embedded font from the PDF (FontFile2/FontFile3), now read from cache.
        //   Tier 2 — bundled Liberation/DejaVu resolved from the /BaseFont name.
        let base_name = self
            .get_base_font_name(&span.font_name)
            .unwrap_or_else(|| span.font_name.clone());
        let (_, bold, italic) = normalize_font_name(&base_name);
        let bundled_bytes = self.font_resolver.resolve(&base_name, bold, italic);

        let has_embedded = self.font_bytes_cache.get(&span.font_name).is_some();
        if !has_embedded && bundled_bytes.is_none() {
            log::warn!(
                "[renderer] key={:?} — no font data, using placeholder",
                span.font_name
            );
        }

        // Separate cache key so a CFF parse failure for the embedded font does not
        // shadow the bundled font under the same key.
        let bundled_key = format!("{}\x00bundled", span.font_name);

        // Extract rotation from the render matrix 2×2 part.
        // For upright text: [scale, 0, 0, -scale] → sin_t ≈ 0.
        // For 90° CCW label: [0, -scale, -scale, 0] → sin_t ≈ -1.
        let [rm_a, rm_b, rm_c, _rm_d] = span.render_matrix_2x2.map(|v| v as f32);
        let scale_x = (rm_a * rm_a + rm_b * rm_b).sqrt();
        let (cos_t, sin_t) = if scale_x > 1e-6 {
            (rm_a / scale_x, rm_b / scale_x)
        } else {
            (1.0_f32, 0.0_f32)
        };
        let has_rotation = sin_t.abs() > 0.01;
        // Synthetic-italic (or any sheared `Tm`): the normalized `c` term is the
        // horizontal shift per unit height above the baseline. Honoured only in the
        // non-rotated path (rotation already routes through an affine blit).
        let skew = if scale_x > 1e-6 { rm_c / scale_x } else { 0.0 };
        let has_shear = !has_rotation && skew.abs() > 1e-3;
        // Sheared/rotated glyphs are placed with an affine `draw_pixmap`, which
        // resamples the rasterized coverage mask. Resampling an already-anti-aliased
        // 1× mask smears the edges (the synthetic-italic / rotated "blur"), so for
        // those paths rasterize a supersampled master and let the affine fold in a
        // `1/ss` downscale — the extra source resolution keeps the edges crisp.
        // Upright glyphs (the common case) keep `ss == 1.0` and the direct,
        // integer-snapped `blit_alpha_mask`, so they stay byte-for-byte unchanged.
        let ss: f32 = if has_rotation || has_shear {
            GLYPH_SUPERSAMPLE
        } else {
            1.0
        };
        let raster_px = size_px * ss;
        // Synthetic-bold approximation: thicken the glyph mask when the text render
        // mode strokes. Radius scales with the (supersampled) raster size; 0 disables it.
        let embolden_radius: u32 = if span.stroke_text {
            ((raster_px * 0.03).round() as i32).clamp(1, (3.0 * ss).round() as i32) as u32
        } else {
            0
        };

        let mut pen_x = span.x as f32;
        let mut pen_y = span.y as f32;

        // Per-span [draw-span] trace silenced (high frequency). Re-enable to debug:
        //   log::debug!("[draw-span] font={:?} size_px={:.1} pos=({:.1},{:.1}) \
        //       chars={} has_pdf_advances={} embedded={} bundled={} rotated={}",
        //       span.font_name, size_px, pen_x, pen_y, span.text.chars().count(),
        //       !span.char_advances.is_empty(), has_embedded, bundled_bytes.is_some(),
        //       has_rotation);
        let _ = (has_embedded, has_rotation);

        // Borrow distinct fields simultaneously so the per-character loop needs no clone.
        //
        // `embedded_bytes_ref` holds an immutable borrow of `font_bytes_cache`.
        // `gc`, `cv`, and `sk` hold exclusive borrows of the three other fields.
        // Rust tracks disjoint field borrows, so all four coexist legally.
        let embedded_bytes_ref: Option<&[u8]> = self.font_bytes_cache.get(&span.font_name);
        let gc = &mut self.glyph_cache;
        let cv = &mut self.canvas;
        let sk = &mut self.blit_scratch;

        for (i, ch) in span.text.chars().enumerate() {
            let cid = span.char_cids.get(i).copied();

            // Tier-1: embedded font.
            // `glyph` borrows from `gc`; the borrow ends when `glyph` goes out of
            // scope at the end of this if-block, before the Tier-2 block below.
            let mut glyph_advance: Option<f32> = None;
            if let Some(ttf) = embedded_bytes_ref {
                let glyph_opt = if let Some(gid) = cid.map(|c| c as u16) {
                    // CID fonts: CID == GID for Identity-H; bypass cmap lookup.
                    gc.rasterize_by_gid(&span.font_name, ttf, gid, raster_px)
                } else {
                    gc.rasterize(&span.font_name, ttf, ch, raster_px)
                };
                if let Some(glyph) = glyph_opt {
                    // Synthetic bold: thicken the cached mask (owned copy) before blit.
                    let bold_holder;
                    let glyph: &super::glyph_cache::GlyphBitmap = if embolden_radius > 0 {
                        bold_holder = embolden_glyph(glyph, embolden_radius);
                        &bold_holder
                    } else {
                        glyph
                    };
                    if glyph.width > 0 && glyph.height > 0 {
                        if has_rotation {
                            blit_glyph_rotated(
                                glyph, pen_x, pen_y, cos_t, sin_t, ss, r, g, b, a, cv,
                            );
                        } else if has_shear {
                            blit_glyph_sheared(glyph, pen_x, pen_y, skew, ss, r, g, b, a, cv);
                        } else {
                            let gx = (pen_x + glyph.bearing_x) as i32;
                            // bearing_y is positive distance from baseline to top of glyph.
                            let gy = (pen_y - glyph.bearing_y) as i32;
                            blit_alpha_mask(
                                &glyph.pixels,
                                &GlyphBlitParams {
                                    mask_w: glyph.width,
                                    mask_h: glyph.height,
                                    dst_x: gx,
                                    dst_y: gy,
                                    r,
                                    g,
                                    b,
                                    base_alpha: a,
                                },
                                cv,
                                sk,
                            );
                        }
                    }
                    glyph_advance = Some(glyph.advance_x / ss);
                    // `glyph` borrow ends here; `gc` is free for Tier-2 below.
                }
            }

            // Tier-2: bundled fallback — only when the embedded font failed to parse.
            if glyph_advance.is_none() {
                if let Some(ttf) = bundled_bytes.as_ref() {
                    // Bundled fonts always use Unicode-based rasterization.
                    let glyph_opt = gc.rasterize(&bundled_key, ttf, ch, raster_px);
                    if let Some(glyph) = glyph_opt {
                        let bold_holder;
                        let glyph: &super::glyph_cache::GlyphBitmap = if embolden_radius > 0 {
                            bold_holder = embolden_glyph(glyph, embolden_radius);
                            &bold_holder
                        } else {
                            glyph
                        };
                        if glyph.width > 0 && glyph.height > 0 {
                            if has_rotation {
                                blit_glyph_rotated(
                                    glyph, pen_x, pen_y, cos_t, sin_t, ss, r, g, b, a, cv,
                                );
                            } else if has_shear {
                                blit_glyph_sheared(glyph, pen_x, pen_y, skew, ss, r, g, b, a, cv);
                            } else {
                                let gx = (pen_x + glyph.bearing_x) as i32;
                                let gy = (pen_y - glyph.bearing_y) as i32;
                                blit_alpha_mask(
                                    &glyph.pixels,
                                    &GlyphBlitParams {
                                        mask_w: glyph.width,
                                        mask_h: glyph.height,
                                        dst_x: gx,
                                        dst_y: gy,
                                        r,
                                        g,
                                        b,
                                        base_alpha: a,
                                    },
                                    cv,
                                    sk,
                                );
                            }
                        }
                        glyph_advance = Some(glyph.advance_x / ss);
                    }
                }
            }

            let advance_x = if let Some(adv) = glyph_advance {
                // Prefer the PDF /W array advance (accurate per-char width from the
                // document) over fontdue's glyph metrics, which may differ for CFF fonts.
                if let Some(&pdf_adv) = span.char_advances.get(i) {
                    if pdf_adv > 0.0 {
                        pdf_adv as f32
                    } else {
                        adv
                    }
                } else {
                    adv
                }
            } else {
                // Genuine font-parse failure — draw a visible placeholder so
                // text positions remain visible even when the font is unsupported.
                draw_text_placeholder(pen_x, pen_y, size_px, [r, g, b, a], cv);
                // Prefer the PDF /W advance over the arbitrary size_px * 0.5 guess.
                if i < span.char_advances.len() && span.char_advances[i] > 0.0 {
                    span.char_advances[i] as f32
                } else {
                    size_px * 0.5
                }
            };
            pen_x += advance_x;
            // Advance pen vertically for rotated text (characters moving in y direction).
            pen_y += span.char_advances_y.get(i).copied().unwrap_or(0.0) as f32;
            // Per-char [draw-char-0] trace silenced (high frequency). Re-enable to debug:
            //   if i == 0 {
            //       log::debug!("[draw-char-0] ch={:?} adv_x={:.2} adv_y={:.2} \
            //           pen_after=({:.1},{:.1})", ch, advance_x,
            //           span.char_advances_y.first().copied().unwrap_or(0.0), pen_x, pen_y);
            //   }
        }
    }

    fn draw_image(&mut self, image_data: &[u8], state: &GraphicsState) {
        // Inline images: the 1×1 unit image square is mapped to pixels by the CTM.
        let (dst_x, dst_y, dst_w, dst_h) = ctm_to_dst_rect(&state.ctm);

        if dst_w == 0 || dst_h == 0 || image_data.is_empty() {
            return;
        }

        // Infer channel count by matching data length to destination pixel count.
        let dst_pixels = (dst_w * dst_h) as usize;
        let (channels, cs) = if image_data.len() == dst_pixels {
            (1usize, "DeviceGray")
        } else if image_data.len() == dst_pixels * 4 {
            (4usize, "DeviceCMYK")
        } else {
            (3usize, "DeviceRGB")
        };

        // Derive source dimensions: if data matches dst exactly, use dst as src.
        let (src_w, src_h) = if image_data.len() == dst_pixels * channels {
            (dst_w, dst_h)
        } else {
            // Data length mismatch — compute nearest square-ish source size.
            let n = (image_data.len() / channels).max(1);
            let side = (n as f64).sqrt().ceil() as u32;
            (side.max(1), (n as u32).div_ceil(side).max(1))
        };

        let img = match decode_image(image_data, None, src_w, src_h, cs, 8) {
            Ok(i) => i,
            Err(e) => {
                log::warn!("inline image decode failed: {}", e);
                return;
            }
        };

        if img.width == dst_w && img.height == dst_h {
            self.canvas
                .blit_rgba(dst_x, dst_y, &img.data, img.width, img.height);
        } else {
            let scaled =
                scale_rgba_bilinear(&img.data, img.width, img.height, dst_w.max(1), dst_h.max(1));
            self.canvas
                .blit_rgba(dst_x, dst_y, &scaled, dst_w.max(1), dst_h.max(1));
        }
    }

    fn draw_image_xobject(
        &mut self,
        _name: &str,
        obj_id: Option<u32>,
        stream: &PdfStream,
        state: &GraphicsState,
    ) {
        log::debug!(
            "[image] xobject name={:?} scale={:.2} ctm=[{:.1},{:.1},{:.1},{:.1},{:.1},{:.1}]",
            _name,
            self.scale,
            state.ctm.a,
            state.ctm.b,
            state.ctm.c,
            state.ctm.d,
            state.ctm.e,
            state.ctm.f
        );
        let dict = &stream.dict;

        let width = dict.get("Width").and_then(pdf_int).unwrap_or(0) as u32;
        let height = dict.get("Height").and_then(pdf_int).unwrap_or(0) as u32;
        if width == 0 || height == 0 {
            return;
        }

        // Stencil mask: 1-bit image painted with the current fill color.
        // Decode array [0 1] (default): bit 0 → paint fill, bit 1 → transparent.
        // Decode array [1 0]: bit 1 → paint fill, bit 0 → transparent.
        let is_image_mask = matches!(dict.get("ImageMask"), Some(PdfObject::Boolean(true)));
        if is_image_mask {
            let raw = match obj_id.and_then(|id| self.doc.get_stream_data(id).ok()) {
                Some(d) => d,
                None => match stream.decode_with_doc(self.doc) {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!("[imagemask] decode failed: {}", e);
                        return;
                    }
                },
            };
            // Default Decode for ImageMask is [0 1]: bit=0 maps to 0.0 (paint).
            let paint_on_zero = match dict.get("Decode") {
                Some(PdfObject::Array(arr)) if arr.len() >= 2 => match &arr[0] {
                    PdfObject::Integer(n) => *n == 0,
                    PdfObject::Real(r) => *r == 0.0,
                    _ => true,
                },
                _ => true,
            };
            let fill = color_to_rgba(&state.fill_color, state.fill_alpha);
            let rgba = decode_image_mask(&raw, width, height, paint_on_zero, fill);
            let (dst_x, dst_y, dst_w, dst_h) = ctm_to_dst_rect(&state.ctm);
            if dst_w > 0 && dst_h > 0 && dst_w == width && dst_h == height {
                self.canvas.blit_rgba(dst_x, dst_y, &rgba, width, height);
            } else if width > 0 && height > 0 {
                let scaled = scale_rgba_bilinear(&rgba, width, height, dst_w.max(1), dst_h.max(1));
                self.canvas
                    .blit_rgba(dst_x, dst_y, &scaled, dst_w.max(1), dst_h.max(1));
            }
            return;
        }

        let bpc = dict.get("BitsPerComponent").and_then(pdf_int).unwrap_or(8) as u8;
        let filter = dict.get("Filter").and_then(|o| o.as_name());

        // Resolve color space — handles simple names and array forms ([/ICCBased], [/Indexed], etc.)
        let cs_obj = dict.get("ColorSpace").cloned();
        let (cs_name, indexed_lookup) = resolve_image_color_space(cs_obj.as_ref(), self.doc);

        let raw = match obj_id.and_then(|id| self.doc.get_stream_data(id).ok()) {
            Some(d) => d,
            None => match stream.decode_with_doc(self.doc) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("image XObject decode failed: {}", e);
                    return;
                }
            },
        };

        // Expand indexed images before decoding.
        // After expansion the data is 8-bit/channel regardless of the original bpc.
        let (raw, cs_name, effective_bpc) =
            if let Some((base_cs, lookup, base_channels)) = &indexed_lookup {
                let expanded = apply_indexed_lookup(&raw, lookup, *base_channels, bpc);
                (expanded, base_cs.as_str(), 8u8)
            } else {
                (raw, cs_name.as_str(), bpc)
            };

        // Decode via the cross-render cache (shared `Rc`, no re-decode on a hit).
        let img = match decode_image_cached(&raw, filter, width, height, cs_name, effective_bpc) {
            Ok(i) => i,
            Err(e) => {
                log::warn!("image decode error: {}", e);
                return;
            }
        };
        let iw = img.width;
        let ih = img.height;

        // Apply soft mask (SMask): a separate grayscale stream whose pixel values
        // encode the alpha for the corresponding color image pixel. Since `img` is
        // a shared cache entry, copy its pixels before mutating them.
        let mut masked: Option<Vec<u8>> = None;
        if let Some(smask_ref) = dict.get("SMask") {
            // For encrypted PDFs the SMask stream must be decrypted before defiltering.
            let sm_decrypted = match smask_ref {
                crate::parser::objects::PdfObject::Reference(sid, _) => {
                    self.doc.get_stream_data(*sid).ok()
                }
                _ => None,
            };
            match self.doc.resolve(smask_ref) {
                Ok(crate::parser::objects::PdfObject::Stream(smask_stream)) => {
                    let sd = &smask_stream.dict;
                    let sm_w = sd.get("Width").and_then(pdf_int).unwrap_or(0) as u32;
                    let sm_h = sd.get("Height").and_then(pdf_int).unwrap_or(0) as u32;
                    let sm_bpc = sd.get("BitsPerComponent").and_then(pdf_int).unwrap_or(8) as u8;
                    let sm_filter = sd.get("Filter").and_then(|o| o.as_name());
                    let sm_raw_result = sm_decrypted
                        .map(Ok)
                        .unwrap_or_else(|| smask_stream.decode_with_doc(self.doc));
                    match sm_raw_result {
                        Ok(sm_raw) => {
                            match decode_image_cached(
                                &sm_raw,
                                sm_filter,
                                sm_w,
                                sm_h,
                                "DeviceGray",
                                sm_bpc,
                            ) {
                                Ok(sm_img) => {
                                    // decode_image on DeviceGray produces RGBA with G→(G,G,G,255).
                                    // Extract the red channel (= gray value) as the alpha bytes.
                                    let gray_raw: Vec<u8> =
                                        sm_img.data.chunks(4).map(|p| p[0]).collect();
                                    // Nearest-neighbour scale the mask if dimensions differ.
                                    let gray = if sm_img.width == iw && sm_img.height == ih {
                                        gray_raw
                                    } else {
                                        log::debug!(
                                            "SMask {}×{} rescaled to match image {}×{}",
                                            sm_img.width,
                                            sm_img.height,
                                            iw,
                                            ih
                                        );
                                        scale_gray_mask(
                                            &gray_raw,
                                            sm_img.width,
                                            sm_img.height,
                                            iw,
                                            ih,
                                        )
                                    };
                                    let mut data = img.data.clone();
                                    apply_smask(&mut data, &gray);
                                    masked = Some(data);
                                }
                                Err(e) => log::warn!("SMask decode error: {}", e),
                            }
                        }
                        Err(e) => log::warn!("SMask stream decode failed: {}", e),
                    }
                }
                Ok(_) => log::warn!("SMask entry is not a stream, skipping"),
                Err(e) => log::warn!("SMask resolve failed: {}", e),
            }
        }

        // Pixels to blit: the masked copy when an SMask applied, else the shared
        // cache pixels directly (no clone).
        let data: &[u8] = masked.as_deref().unwrap_or(&img.data);

        // Compute destination rect from all four corners of the unit square
        // mapped through the CTM — handles rotation and shear correctly.
        let (dst_x, dst_y, dst_w, dst_h) = ctm_to_dst_rect(&state.ctm);

        // Scale to destination size first, then apply any active ExtGState soft mask.
        let final_data: Vec<u8>;
        let (blit_w, blit_h, blit_data): (u32, u32, &[u8]) =
            if dst_w > 0 && dst_h > 0 && dst_w == iw && dst_h == ih {
                (iw, ih, data)
            } else if iw > 0 && ih > 0 {
                final_data = scale_rgba_bilinear(data, iw, ih, dst_w.max(1), dst_h.max(1));
                (dst_w.max(1), dst_h.max(1), &final_data)
            } else {
                return;
            };

        if let Some(ref mask) = self.current_soft_mask {
            let mut buf = blit_data.to_vec();
            Self::apply_mask_to_image(mask, &mut buf, dst_x, dst_y, blit_w, blit_h);
            self.canvas.blit_rgba(dst_x, dst_y, &buf, blit_w, blit_h);
        } else {
            self.canvas
                .blit_rgba(dst_x, dst_y, blit_data, blit_w, blit_h);
        }
    }

    fn paint_shading(
        &mut self,
        shading_dict: &crate::parser::objects::PdfDict,
        doc: &crate::parser::objects::PdfDocument,
        state: &GraphicsState,
    ) {
        match Shading::parse(shading_dict, doc) {
            Ok(shading) => shading.rasterize(&state.ctm, &mut self.canvas),
            Err(e) => log::warn!("shading parse error: {}", e),
        }
    }

    fn begin_transparency_group(&mut self) {
        let w = self.canvas.width;
        let h = self.canvas.height;
        let origin = self.canvas.origin;
        match PixmapBuffer::new_transparent(w, h, origin) {
            Ok(group_canvas) => {
                let saved = std::mem::replace(&mut self.canvas, group_canvas);
                self.transparency_stack.push((
                    saved,
                    1.0,
                    crate::content::graphics_state::BlendMode::Normal,
                ));
            }
            Err(e) => log::warn!("transparency group alloc failed: {}", e),
        }
    }

    fn end_transparency_group(
        &mut self,
        fill_alpha: f64,
        blend_mode: crate::content::graphics_state::BlendMode,
    ) {
        if let Some((saved_canvas, _, _)) = self.transparency_stack.pop() {
            let group_result = std::mem::replace(&mut self.canvas, saved_canvas);
            self.canvas
                .composite_over(&group_result, fill_alpha, blend_mode);
        }
    }

    fn enter_form_resources(&mut self, resources: &crate::parser::objects::PdfDict) {
        self.resource_stack.push(Arc::new(resources.clone()));
    }

    fn exit_form_resources(&mut self) {
        self.resource_stack.pop();
    }

    fn set_soft_mask(
        &mut self,
        mask_type: &str,
        form_stream: &crate::parser::objects::PdfStream,
        ctm: &Matrix,
    ) {
        let mt = if mask_type == "Alpha" {
            SoftMaskType::Alpha
        } else {
            SoftMaskType::Luminosity
        };
        self.render_soft_mask(mt, form_stream, ctm);
    }

    fn clear_soft_mask(&mut self) {
        self.current_soft_mask = None;
    }
}

impl<'doc> PageRenderer<'doc> {
    /// Retrieve raw TrueType bytes for a font resource name, if the font has
    /// an embedded `/FontFile2` or `/FontFile3` stream.
    fn get_ttf_bytes(&self, font_name: &str) -> Option<Vec<u8>> {
        let font_dict = self.current_resources().get("Font")?;
        let font_dict = match font_dict {
            PdfObject::Dictionary(d) => d,
            _ => return None,
        };

        let font_ref = font_dict.get(font_name)?;
        let font_obj = self.doc.resolve(font_ref).ok()?;
        let font_d = match font_obj {
            PdfObject::Dictionary(d) => d,
            _ => return None,
        };

        // For Type 0 (composite) fonts, /FontDescriptor lives inside
        // DescendantFonts[0] (the CIDFont dict), not in the Type0 dict itself.
        let is_type0 = font_d
            .get("Subtype")
            .and_then(|o| o.as_name())
            .map(|s| s == "Type0")
            .unwrap_or(false);

        let desc_holder: PdfDict = if is_type0 {
            font_d
                .get("DescendantFonts")
                .and_then(|o| match o {
                    PdfObject::Array(a) => a.first().cloned(),
                    _ => None,
                })
                .and_then(|r| self.doc.resolve(&r).ok())
                .and_then(|obj| match obj {
                    PdfObject::Dictionary(d) => Some(d),
                    _ => None,
                })?
        } else {
            font_d
        };

        // Try FontDescriptor → FontFile2 (TrueType) or FontFile3 (CFF/OTF).
        let desc_ref = desc_holder.get("FontDescriptor")?;
        let desc_obj = self.doc.resolve(desc_ref).ok()?;
        let desc = match desc_obj {
            PdfObject::Dictionary(d) => d,
            _ => return None,
        };

        let ff_ref = desc.get("FontFile2").or_else(|| desc.get("FontFile3"))?;

        // Encrypted PDFs: the font-program stream is ciphertext on disk and must
        // be decrypted before parsing. `get_stream_data` does decrypt → defilter
        // in the correct order; for unencrypted PDFs it returns the same bytes as
        // `decode_with_doc`, so there is no behaviour change there.
        if let PdfObject::Reference(id, _) = ff_ref {
            if let Ok(data) = self.doc.get_stream_data(*id) {
                return Some(data);
            }
        }

        let ff_obj = self.doc.resolve(ff_ref).ok()?;
        let stream = match ff_obj {
            PdfObject::Stream(s) => s,
            _ => return None,
        };

        stream.decode_with_doc(self.doc).ok()
    }

    /// Read the `/BaseFont` name for a resource key (e.g. `"F1"` → `"Helvetica-Bold"`).
    fn get_base_font_name(&self, resource_key: &str) -> Option<String> {
        let font_dict = self.current_resources().get("Font")?;
        let font_dict = match font_dict {
            PdfObject::Dictionary(d) => d,
            _ => return None,
        };
        let font_ref = font_dict.get(resource_key)?;
        let font_obj = self.doc.resolve(font_ref).ok()?;
        let font_d = match font_obj {
            PdfObject::Dictionary(d) => d,
            _ => return None,
        };
        match font_d.get("BaseFont") {
            Some(PdfObject::Name(n)) => Some(n.clone()),
            _ => None,
        }
    }
    /// Render a PatternType 1 (tiling) pattern: run the pattern content stream once into
    /// an off-screen cell buffer, tile it across the fill path's bounding box, then apply
    /// the path as a clip mask and composite onto the main canvas.
    ///
    /// Mirrors ONLYOFFICE's `RendererOutputDev::tilingPatternFill` fast-path: render the
    /// cell once, blit at each (xi, yi) tile offset, then clip.
    #[allow(clippy::too_many_arguments)]
    fn render_tiling_pattern(
        &mut self,
        path: &Path,
        state: &GraphicsState,
        rule: FillRule,
        pattern_name: &str,
        pat_dict: &PdfDict,
        pat_stream: &PdfStream,
        tint: Option<Vec<f64>>,
    ) {
        // Extract tile geometry from the pattern dictionary.
        let bbox = extract_bbox(pat_dict);
        let x_step = pat_dict
            .get("XStep")
            .and_then(pdf_f64)
            .unwrap_or(bbox[2] - bbox[0]);
        let y_step = pat_dict
            .get("YStep")
            .and_then(pdf_f64)
            .unwrap_or(bbox[3] - bbox[1]);
        let pat_matrix = extract_matrix(pat_dict).unwrap_or_else(Matrix::identity);

        // tile_ctm maps pattern space → tile-local pixel space.
        let tile_ctm = pat_matrix.concat(&state.ctm);

        // BBox corners in tile-local pixels.
        let corners = [
            tile_ctm.transform_point(bbox[0], bbox[1]),
            tile_ctm.transform_point(bbox[2], bbox[1]),
            tile_ctm.transform_point(bbox[2], bbox[3]),
            tile_ctm.transform_point(bbox[0], bbox[3]),
        ];
        let cell_min_x = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::INFINITY, f64::min)
            .floor();
        let cell_min_y = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::INFINITY, f64::min)
            .floor();
        let cell_max_x = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil();
        let cell_max_y = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil();

        let cell_w = (cell_max_x - cell_min_x).max(1.0) as u32;
        let cell_h = (cell_max_y - cell_min_y).max(1.0) as u32;

        if cell_w > 8192 || cell_h > 8192 {
            log::warn!(
                "[pattern-type1] '{}' cell {}×{} exceeds limit — skipping",
                pattern_name,
                cell_w,
                cell_h
            );
            return;
        }

        // Off-screen cell buffer (tile-local origin = 0).
        let cell_canvas =
            match PixmapBuffer::new_transparent(cell_w, cell_h, TileOrigin { x: 0, y: 0 }) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!(
                        "[pattern-type1] '{}' cell alloc failed: {}",
                        pattern_name,
                        e
                    );
                    return;
                }
            };

        // CTM for the cell: pattern space → cell-local (0-indexed) pixel coordinates.
        let cell_ctm = Matrix {
            a: tile_ctm.a,
            b: tile_ctm.b,
            c: tile_ctm.c,
            d: tile_ctm.d,
            e: tile_ctm.e - cell_min_x,
            f: tile_ctm.f - cell_min_y,
        };

        let paint_type = pat_dict
            .get("PaintType")
            .and_then(|o| {
                if let PdfObject::Integer(n) = o {
                    Some(*n)
                } else {
                    None
                }
            })
            .unwrap_or(1);

        let mut cell_renderer = PageRenderer::new(
            cell_canvas,
            self.scale,
            self.doc,
            Arc::clone(self.current_resources()),
        );
        let mut cell_interp = ContentInterpreter::new();
        cell_interp.gfx.current.ctm = cell_ctm;

        // For PaintType 2 (uncoloured), pre-set the tint colour.
        if paint_type == 2 {
            if let Some(ref comps) = tint {
                let tc = match comps.len() {
                    1 => Color::Gray(comps[0]),
                    3 => Color::Rgb(comps[0], comps[1], comps[2]),
                    4 => Color::Cmyk(comps[0], comps[1], comps[2], comps[3]),
                    _ => Color::Gray(0.5),
                };
                cell_interp.gfx.current.fill_color = tc.clone();
                cell_interp.gfx.current.stroke_color = tc;
            }
        }

        // Use pattern's own Resources if present, otherwise fall back to page resources.
        // Arc::clone is O(1); a new Arc is only allocated when the pattern has its own dict.
        let pat_resources: Arc<PdfDict> = match pat_dict.get("Resources") {
            Some(PdfObject::Dictionary(d)) => Arc::new(d.clone()),
            Some(r) => self
                .doc
                .resolve(r)
                .ok()
                .and_then(|o| {
                    if let PdfObject::Dictionary(d) = o {
                        Some(Arc::new(d))
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| Arc::clone(&self.resources_raw)),
            None => Arc::clone(&self.resources_raw),
        };

        let stream_bytes = match pat_stream.decode() {
            Ok(b) => b,
            Err(e) => {
                log::warn!("[pattern-type1] '{}' stream decode: {}", pattern_name, e);
                return;
            }
        };

        if let Err(e) = cell_interp.interpret_with_doc(
            &stream_bytes,
            &mut cell_renderer,
            self.doc,
            &pat_resources,
        ) {
            log::warn!("[pattern-type1] '{}' interpret: {}", pattern_name, e);
        }

        let cell_data = cell_renderer.canvas.data().to_vec();

        // Step vectors: device pixel offset when xi or yi increases by 1.
        let step_x_dx = tile_ctm.a * x_step;
        let step_x_dy = tile_ctm.b * x_step;
        let step_y_dx = tile_ctm.c * y_step;
        let step_y_dy = tile_ctm.d * y_step;

        let (clip_x_min, clip_y_min, clip_x_max, clip_y_max) = path_device_bbox(path, &state.ctm);

        let (xi_min, xi_max) =
            tiling_index_range(clip_x_min, clip_x_max, cell_min_x, cell_max_x, step_x_dx);
        let (yi_min, yi_max) =
            tiling_index_range(clip_y_min, clip_y_max, cell_min_y, cell_max_y, step_y_dy);

        log::debug!(
            "[pattern-type1] '{}' cell={}×{}@({:.0},{:.0}) step=({:.1},{:.1}) xi={}..{} yi={}..{}",
            pattern_name,
            cell_w,
            cell_h,
            cell_min_x,
            cell_min_y,
            step_x_dx,
            step_y_dy,
            xi_min,
            xi_max,
            yi_min,
            yi_max
        );

        // Full-canvas buffer to accumulate tiled pattern pixels.
        let canvas_w = self.canvas.width;
        let canvas_h = self.canvas.height;
        let canvas_origin = self.canvas.origin;
        let Ok(mut tiled_buf) = PixmapBuffer::new_transparent(canvas_w, canvas_h, canvas_origin)
        else {
            return;
        };

        // Blit cell at each (xi, yi) position using premultiplied source-over.
        for yi in yi_min..=yi_max {
            for xi in xi_min..=xi_max {
                let tile_x = (cell_min_x + xi as f64 * step_x_dx + yi as f64 * step_y_dx) as i32;
                let tile_y = (cell_min_y + xi as f64 * step_x_dy + yi as f64 * step_y_dy) as i32;
                blit_premultiplied(&cell_data, cell_w, cell_h, tile_x, tile_y, &mut tiled_buf);
            }
        }

        // Build a clip mask from the fill path and zero out pixels outside it.
        let Some(sk_path) = build_skia_path(path) else {
            return;
        };
        let sk_rule = match rule {
            FillRule::NonZero => tiny_skia::FillRule::Winding,
            FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
        };
        let transform = matrix_to_transform(&state.ctm);

        if let Some(mut mask_pix) = tiny_skia::Pixmap::new(canvas_w, canvas_h) {
            mask_pix.fill(tiny_skia::Color::TRANSPARENT);
            let mut white_paint = tiny_skia::Paint::default();
            white_paint.set_color(tiny_skia::Color::WHITE);
            mask_pix.fill_path(&sk_path, &white_paint, sk_rule, transform, None);
            let mask_data = mask_pix.data().to_vec();
            let tile_data = tiled_buf.inner.data_mut();
            for i in (0..tile_data.len()).step_by(4) {
                if mask_data.get(i + 3).copied().unwrap_or(0) == 0 {
                    tile_data[i] = 0;
                    tile_data[i + 1] = 0;
                    tile_data[i + 2] = 0;
                    tile_data[i + 3] = 0;
                }
            }
        }

        self.canvas
            .composite_over(&tiled_buf, state.fill_alpha, state.blend_mode);
    }

    /// Fill a path using a pattern resolved from Resources.
    ///
    /// `tint` carries the numeric prefix operands from `scn` for uncoloured tiling
    /// patterns (PatternType 1, PaintType 2); ignored for shading patterns.
    fn fill_path_with_pattern(
        &mut self,
        path: &Path,
        state: &GraphicsState,
        rule: FillRule,
        pattern_name: String,
        tint: Option<Vec<f64>>,
    ) {
        let pattern_result = (|| -> Option<(PdfDict, Option<PdfStream>)> {
            let patterns = self.current_resources().get("Pattern")?;
            let patterns = match patterns {
                PdfObject::Dictionary(d) => d,
                _ => return None,
            };
            let pat_ref = patterns.get(&pattern_name)?;
            let pat_obj = self.doc.resolve(pat_ref).ok()?;
            match pat_obj {
                PdfObject::Dictionary(d) => Some((d, None)),
                PdfObject::Stream(s) => {
                    let dict = s.dict.clone();
                    Some((dict, Some(*s)))
                }
                _ => None,
            }
        })();

        let Some((pat_dict, pat_stream)) = pattern_result else {
            log::warn!("pattern '{}' not found in Resources", pattern_name);
            return;
        };

        let pattern_type = pat_dict
            .get("PatternType")
            .and_then(|o| match o {
                PdfObject::Integer(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);

        if pattern_type == 1 {
            let paint_type = pat_dict
                .get("PaintType")
                .and_then(|o| match o {
                    PdfObject::Integer(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or(1);

            if paint_type == 2 {
                if let Some(ref comps) = tint {
                    let tint_color = match comps.len() {
                        1 => Color::Gray(comps[0]),
                        3 => Color::Rgb(comps[0], comps[1], comps[2]),
                        4 => Color::Cmyk(comps[0], comps[1], comps[2], comps[3]),
                        _ => Color::Gray(0.5),
                    };
                    let mut tint_state = state.clone();
                    tint_state.fill_color = tint_color;
                    log::debug!(
                        "[pattern] tiling '{}' PaintType=2 — filling with tint {:?}",
                        pattern_name,
                        comps
                    );
                    fill_path_with_rule(path, &tint_state, rule, &state.ctm, &mut self.canvas);
                } else {
                    log::warn!(
                        "tiling pattern '{}' PaintType=2 but no tint — skipping",
                        pattern_name
                    );
                }
            } else if let Some(stream) = pat_stream {
                log::debug!(
                    "[pattern-type1] '{}' PaintType=1 — rendering content stream",
                    pattern_name
                );
                self.render_tiling_pattern(
                    path,
                    state,
                    rule,
                    &pattern_name,
                    &pat_dict,
                    &stream,
                    tint,
                );
            } else {
                log::warn!(
                    "tiling pattern '{}' PaintType=1 but no stream — skipping",
                    pattern_name
                );
            }
            return;
        }

        if pattern_type != 2 {
            log::warn!(
                "unsupported PatternType {} for '{}'",
                pattern_type,
                pattern_name
            );
            return;
        }

        // Extract the Shading sub-dict
        let shading_dict = match pat_dict.get("Shading") {
            Some(PdfObject::Dictionary(d)) => d.clone(),
            Some(other) => {
                if let Ok(PdfObject::Dictionary(d)) = self.doc.resolve(other) {
                    d
                } else {
                    log::warn!("cannot resolve Shading in pattern '{}'", pattern_name);
                    return;
                }
            }
            None => {
                log::warn!("no Shading entry in pattern '{}'", pattern_name);
                return;
            }
        };

        let shading = match Shading::parse(&shading_dict, self.doc) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("shading parse error in pattern '{}': {}", pattern_name, e);
                return;
            }
        };

        // Build the clip path in tiny_skia
        let Some(sk_path) = build_skia_path(path) else {
            return;
        };
        let sk_rule = match rule {
            FillRule::NonZero => tiny_skia::FillRule::Winding,
            FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
        };
        let transform = matrix_to_transform(&state.ctm);

        // Rasterize shading into a temporary buffer
        let w = self.canvas.width;
        let h = self.canvas.height;
        let Ok(mut shading_buf) = PixmapBuffer::new_transparent(w, h, self.canvas.origin) else {
            return;
        };
        shading.rasterize(&state.ctm, &mut shading_buf);

        // Apply the path as a clip mask: for each pixel, if it's inside the path
        // keep the shading pixel, otherwise discard it.
        let mut mask_pixmap =
            tiny_skia::Pixmap::new(w, h).unwrap_or_else(|| tiny_skia::Pixmap::new(1, 1).unwrap());
        mask_pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let white_paint = {
            let mut p = tiny_skia::Paint::default();
            p.set_color(tiny_skia::Color::WHITE);
            p
        };
        mask_pixmap.fill_path(&sk_path, &white_paint, sk_rule, transform, None);

        // Mask the shading buffer: zero out pixels where mask alpha is 0
        let mask_data = mask_pixmap.data();
        let shading_data = shading_buf.inner.data_mut();
        for i in (0..shading_data.len()).step_by(4) {
            let mask_a = mask_data.get(i + 3).copied().unwrap_or(0);
            if mask_a == 0 {
                shading_data[i] = 0;
                shading_data[i + 1] = 0;
                shading_data[i + 2] = 0;
                shading_data[i + 3] = 0;
            }
        }

        // Composite the masked shading onto the main canvas
        self.canvas
            .composite_over(&shading_buf, state.fill_alpha, state.blend_mode);
    }
}

// ---------------------------------------------------------------------------
// Public rendering API
// ---------------------------------------------------------------------------

/// Render a rectangular tile of a PDF page to an RGBA pixel buffer.
///
/// `tile` specifies the region in PDF user-space (points).  Only content
/// within this region is rasterized into the returned buffer, whose pixel
/// dimensions are `ceil(tile.width * scale) × ceil(tile.height * scale)`.
///
/// For full-page rendering use [`render_page`].
pub fn render_tile(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    tile: TileRect,
) -> Result<PixmapBuffer> {
    let mb = page.media_box;
    let page_h_pts = mb.height();

    // Per-tile [render_tile] trace silenced (fires for every edit-preview render).
    // Re-enable to debug tiling/scale:
    //   log::debug!("[render_tile] mediabox=[{:.1},{:.1},{:.1},{:.1}] \
    //       tile=[{:.1},{:.1},{:.1}×{:.1}] scale={:.4}", mb.x1, mb.y1, mb.x2, mb.y2,
    //       tile.x, tile.y, tile.width, tile.height, scale);

    let (origin, tile_w_px, tile_h_px) = tile.to_pixel_space(page_h_pts, scale);

    let canvas = PixmapBuffer::new_tile(tile_w_px, tile_h_px, origin)?;

    // Build initial CTM: PDF user-space → tile-local pixel space.
    // Base (no rotation): [scale, 0, 0, -scale, -tile.x * scale, (tile.y + tile.h) * scale]
    // Page /Rotate is pre-multiplied so the correct content orientation is always rendered.
    let base_ctm = Matrix {
        a: scale as f64,
        b: 0.0,
        c: 0.0,
        d: -(scale as f64),
        e: -(tile.x * scale as f64),
        f: (tile.y + tile.height) * scale as f64,
    };

    // Page rotation matrix (PDF spec §8.4.4, Table 10): maps rotated user space to
    // unrotated user space before the base CTM is applied.
    let pw = page.width(); // effective width after rotation
    let ph = page.height(); // effective height after rotation
    let rot_matrix = match page.rotate.rem_euclid(360) {
        90 => Matrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: 0.0,
            f: pw,
        },
        180 => Matrix {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: pw,
            f: ph,
        },
        270 => Matrix {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            e: ph,
            f: 0.0,
        },
        _ => Matrix::identity(),
    };
    let initial_ctm = rot_matrix.concat(&base_ctm);

    let resources_raw = Arc::new(page.resources.raw.clone());
    let mut renderer = PageRenderer::new(canvas, scale, doc, Arc::clone(&resources_raw));

    // Set the initial CTM on the interpreter before execution.
    let mut interp = ContentInterpreter::new();
    interp.gfx.current.ctm = initial_ctm;

    let content = page.decode_contents(doc)?;
    let iter = ContentStreamIter::new(&content);
    interp.interpret_iter(iter, &mut renderer, Some(doc), Some(&*resources_raw))?;

    Ok(renderer.canvas)
}

/// Render a full PDF page and return raw RGBA bytes with pixel dimensions.
///
/// Convenience wrapper over [`render_page`] that resolves the page by index and
/// converts the `PixmapBuffer` into an owned `Vec<u8>`.  Suitable for use at
/// the WASM boundary where callers cannot hold Rust references.
///
/// `page_index` is 0-based.  `scale` controls resolution: `1.0` = 72 DPI,
/// `2.0` = 144 DPI.  Returns `(width_px, height_px, rgba_bytes)`.
pub fn render_page_rgba(
    doc: &PdfDocument,
    page_index: usize,
    scale: f64,
) -> Result<(u32, u32, Vec<u8>)> {
    use crate::document::catalog::Catalog;
    use crate::document::page::Page;

    let catalog = Catalog::from_document(doc)?;
    let page_dict = catalog.get_page_dict(doc, page_index)?;
    let page = Page::from_dict(doc, &page_dict)?;
    let buf = render_page(doc, &page, scale as f32)?;
    let (w, h) = (buf.width, buf.height);
    // tiny-skia stores pixels as premultiplied RGBA; JavaScript ImageData expects
    // straight (unpremultiplied) RGBA.  Convert before crossing the WASM boundary.
    let mut data = buf.data().to_vec();
    for pixel in data.chunks_exact_mut(4) {
        let a = pixel[3];
        if a > 0 && a < 255 {
            let inv = 255.0 / a as f32;
            pixel[0] = (pixel[0] as f32 * inv).min(255.0) as u8;
            pixel[1] = (pixel[1] as f32 * inv).min(255.0) as u8;
            pixel[2] = (pixel[2] as f32 * inv).min(255.0) as u8;
        }
    }
    Ok((w, h, data))
}

/// Render a rectangular tile using caller-supplied content bytes instead of the
/// page's own content stream.
///
/// The full `content` is interpreted (so all graphics state — colour, CTM,
/// clips — is preserved exactly as in a normal page render), but only pixels
/// inside `tile` are kept. The text editor uses this to preview an edited block:
/// it passes the page's content with the block's show-text operand replaced, and
/// `tile` set to the block's bounding box, yielding a crop that is pixel-identical
/// to how the saved PDF will rasterise.
///
/// Returns `(origin_x_px, origin_y_px, buffer)` where the origin is the tile's
/// top-left in device pixels at `scale`.
pub fn render_tile_content(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    tile: TileRect,
    content: &[u8],
) -> Result<((u32, u32), PixmapBuffer)> {
    let mb = page.media_box;
    let page_h_pts = mb.height();
    let (origin, tile_w_px, tile_h_px) = tile.to_pixel_space(page_h_pts, scale);
    let canvas = PixmapBuffer::new_tile(tile_w_px, tile_h_px, origin)?;
    let initial_ctm = crate::content::graphics_state::Matrix {
        a: scale as f64,
        b: 0.0,
        c: 0.0,
        d: -(scale as f64),
        e: -(tile.x * scale as f64),
        f: (tile.y + tile.height) * scale as f64,
    };
    let resources_raw = Arc::new(page.resources.raw.clone());
    let mut renderer = PageRenderer::new(canvas, scale, doc, Arc::clone(&resources_raw));
    let mut interp = ContentInterpreter::new();
    interp.gfx.current.ctm = initial_ctm;
    let iter = ContentStreamIter::new(content);
    interp.interpret_iter(iter, &mut renderer, Some(doc), Some(&*resources_raw))?;
    Ok(((origin.x, origin.y), renderer.canvas))
}

/// Render only `block_tile` from caller-supplied `content`, into a buffer sized
/// to the block — not the whole page. Returns `(origin_px, buffer)` where the
/// origin is the block's top-left in **full-page** device pixels (so the caller
/// can place the crop), and the buffer is `block_w × block_h` pixels.
///
/// Unlike [`render_tile_content`] (which allocates a full-page buffer and is
/// only correct for a full-page tile), this allocates a **block-sized** canvas
/// with origin `(0,0)` and a tile-relative CTM that maps the block's own
/// rectangle into `[0,w]×[0,h]`. With origin 0, every draw path is consistent:
/// glyphs (which subtract `canvas.origin`) and vector fills (which don't) both
/// land tile-local — so an arbitrary sub-tile renders correctly. This is the
/// same convention [`render_tile`] uses for a full page (tile == whole page →
/// origin 0); here the tile is just the block. Per-keystroke edit preview uses
/// it so cost is O(block area), independent of page size.
pub fn render_block_tile(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    block_tile: TileRect,
    content: &[u8],
) -> Result<((u32, u32), PixmapBuffer)> {
    let page_h_pts = page.media_box.height();
    let (origin, w_px, h_px) = block_tile.to_pixel_space(page_h_pts, scale);
    // Origin (0,0): the tile-relative CTM below already produces block-local
    // pixel coordinates, so no draw path needs an origin offset (see doc above).
    let canvas = PixmapBuffer::new(w_px, h_px)?;
    let initial_ctm = crate::content::graphics_state::Matrix {
        a: scale as f64,
        b: 0.0,
        c: 0.0,
        d: -(scale as f64),
        e: -(block_tile.x * scale as f64),
        f: (block_tile.y + block_tile.height) * scale as f64,
    };
    let resources_raw = Arc::new(page.resources.raw.clone());
    let mut renderer = PageRenderer::new(canvas, scale, doc, Arc::clone(&resources_raw));
    let mut interp = ContentInterpreter::new();
    interp.gfx.current.ctm = initial_ctm;
    let iter = ContentStreamIter::new(content);
    interp.interpret_iter(iter, &mut renderer, Some(doc), Some(&*resources_raw))?;
    Ok(((origin.x, origin.y), renderer.canvas))
}

/// Render a full PDF page at the given scale.
///
/// Equivalent to calling `render_tile` with the page's full `media_box`.
///
/// `scale` controls DPI: `1.0` = 72 DPI (one PDF point = one pixel),
/// `2.0` = 144 DPI, etc.
pub fn render_page(doc: &PdfDocument, page: &Page, scale: f32) -> Result<PixmapBuffer> {
    let mb = page.media_box;
    render_tile(
        doc,
        page,
        scale,
        TileRect {
            x: mb.x1,
            y: mb.y1,
            width: mb.width(),
            height: mb.height(),
        },
    )
}

/// Render a tile using a custom [`FontResolver`] for font fallback.
///
/// Use this variant when you want to supply additional fonts beyond the
/// embedded Liberation set — for example, a [`super::font_resolver::DirectoryFontResolver`]
/// pointing to a `core-fonts` directory on native builds.
pub fn render_tile_with_resolver(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    tile: TileRect,
    font_resolver: Box<dyn FontResolver>,
) -> Result<PixmapBuffer> {
    let mb = page.media_box;
    let page_h_pts = mb.height();
    let (origin, tile_w_px, tile_h_px) = tile.to_pixel_space(page_h_pts, scale);
    let canvas = PixmapBuffer::new_tile(tile_w_px, tile_h_px, origin)?;
    let initial_ctm = crate::content::graphics_state::Matrix {
        a: scale as f64,
        b: 0.0,
        c: 0.0,
        d: -(scale as f64),
        e: -(tile.x * scale as f64),
        f: (tile.y + tile.height) * scale as f64,
    };
    let resources_raw = Arc::new(page.resources.raw.clone());
    let mut renderer = PageRenderer::with_resolver(
        canvas,
        scale,
        doc,
        Arc::clone(&resources_raw),
        font_resolver,
    );
    let mut interp = ContentInterpreter::new();
    interp.gfx.current.ctm = initial_ctm;
    let content = page.decode_contents(doc)?;
    let iter = ContentStreamIter::new(&content);
    interp.interpret_iter(iter, &mut renderer, Some(doc), Some(&*resources_raw))?;
    Ok(renderer.canvas)
}

/// Render a full page using a custom [`FontResolver`] for font fallback.
pub fn render_page_with_resolver(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    font_resolver: Box<dyn FontResolver>,
) -> Result<PixmapBuffer> {
    let mb = page.media_box;
    render_tile_with_resolver(
        doc,
        page,
        scale,
        TileRect {
            x: mb.x1,
            y: mb.y1,
            width: mb.width(),
            height: mb.height(),
        },
        font_resolver,
    )
}

/// Render a rectangular tile, reusing a caller-supplied [`GlyphCache`].
///
/// Identical to [`render_tile`] except that the caller provides and receives
/// back a `GlyphCache`.  Passing the same cache across consecutive tile calls
/// for the same page and scale avoids re-rasterising glyphs that were already
/// computed for a previous tile.
///
/// # Example
/// ```rust,ignore
/// let mut cache = GlyphCache::new();
/// for tile in tiles {
///     let (buf, returned_cache) = render_tile_with_cache(doc, page, scale, tile, cache)?;
///     cache = returned_cache;
///     display(buf);
/// }
/// ```
pub fn render_tile_with_cache(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    tile: TileRect,
    cache: GlyphCache,
) -> Result<(PixmapBuffer, GlyphCache)> {
    let mb = page.media_box;
    let page_h_pts = mb.height();
    let (origin, tile_w_px, tile_h_px) = tile.to_pixel_space(page_h_pts, scale);
    let canvas = PixmapBuffer::new_tile(tile_w_px, tile_h_px, origin)?;
    let initial_ctm = Matrix {
        a: scale as f64,
        b: 0.0,
        c: 0.0,
        d: -(scale as f64),
        e: -(tile.x * scale as f64),
        f: (tile.y + tile.height) * scale as f64,
    };
    let resources_raw = Arc::new(page.resources.raw.clone());
    let mut renderer = PageRenderer::new_with_external_cache(
        canvas,
        scale,
        doc,
        Arc::clone(&resources_raw),
        cache,
    );
    let mut interp = ContentInterpreter::new();
    interp.gfx.current.ctm = initial_ctm;
    let content = page.decode_contents(doc)?;
    let iter = ContentStreamIter::new(&content);
    interp.interpret_iter(iter, &mut renderer, Some(doc), Some(&*resources_raw))?;
    Ok((renderer.canvas, renderer.glyph_cache))
}

/// Render a full page, reusing a caller-supplied [`GlyphCache`].
///
/// Convenience wrapper over [`render_tile_with_cache`].
pub fn render_page_with_cache(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    cache: GlyphCache,
) -> Result<(PixmapBuffer, GlyphCache)> {
    let mb = page.media_box;
    render_tile_with_cache(
        doc,
        page,
        scale,
        TileRect {
            x: mb.x1,
            y: mb.y1,
            width: mb.width(),
            height: mb.height(),
        },
        cache,
    )
}

/// Render a rectangular tile, reusing a caller-supplied [`RenderCache`].
///
/// Like [`render_tile_with_cache`] but also persists decoded font stream bytes
/// across tiles, eliminating one FlateDecode call per font per tile beyond the
/// first.
///
/// Pass the same `cache` value across all tiles for a page to get the full
/// benefit.  The cache is returned on success so the caller can reuse it.
pub fn render_tile_with_render_cache(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    tile: TileRect,
    cache: RenderCache,
) -> Result<(PixmapBuffer, RenderCache)> {
    let mb = page.media_box;
    let page_h_pts = mb.height();
    let (origin, tile_w_px, tile_h_px) = tile.to_pixel_space(page_h_pts, scale);
    let canvas = PixmapBuffer::new_tile(tile_w_px, tile_h_px, origin)?;
    let initial_ctm = Matrix {
        a: scale as f64,
        b: 0.0,
        c: 0.0,
        d: -(scale as f64),
        e: -(tile.x * scale as f64),
        f: (tile.y + tile.height) * scale as f64,
    };
    let resources_raw = Arc::new(page.resources.raw.clone());
    let mut renderer =
        PageRenderer::new_with_render_cache(canvas, scale, doc, Arc::clone(&resources_raw), cache);
    let mut interp = ContentInterpreter::new();
    interp.gfx.current.ctm = initial_ctm;
    let content = page.decode_contents(doc)?;
    let iter = ContentStreamIter::new(&content);
    interp.interpret_iter(iter, &mut renderer, Some(doc), Some(&*resources_raw))?;
    let returned_cache = RenderCache {
        glyphs: renderer.glyph_cache,
        font_bytes: renderer.font_bytes_cache,
    };
    Ok((renderer.canvas, returned_cache))
}

/// Render a full page, reusing a caller-supplied [`RenderCache`].
///
/// Convenience wrapper over [`render_tile_with_render_cache`] for full-page renders.
pub fn render_page_with_render_cache(
    doc: &PdfDocument,
    page: &Page,
    scale: f32,
    cache: RenderCache,
) -> Result<(PixmapBuffer, RenderCache)> {
    let mb = page.media_box;
    render_tile_with_render_cache(
        doc,
        page,
        scale,
        TileRect {
            x: mb.x1,
            y: mb.y1,
            width: mb.width(),
            height: mb.height(),
        },
        cache,
    )
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Compute the user-space bounding box (min_x, min_y, max_x, max_y) of a path.
///
/// Returns `(0, 0, 0, 0)` for empty paths.  Used for diagnostic logging.
/// Compute the bounding box of a path transformed into device space.
///
/// Transforms each control point through `ctm` and returns
/// `(min_x, min_y, max_x, max_y)` in device pixel coordinates.
fn path_device_bbox(
    path: &Path,
    ctm: &crate::content::graphics_state::Matrix,
) -> (f64, f64, f64, f64) {
    use crate::content::graphics_state::PathSegment;
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for seg in &path.segments {
        let pts: &[crate::content::graphics_state::Point] = match seg {
            PathSegment::MoveTo(p) | PathSegment::LineTo(p) => std::slice::from_ref(p),
            PathSegment::CurveTo(p1, p2, p3) => &[*p1, *p2, *p3],
            PathSegment::ClosePath => &[],
        };
        for p in pts {
            let (dx, dy) = ctm.transform_point(p.x, p.y);
            if dx < min_x {
                min_x = dx;
            }
            if dy < min_y {
                min_y = dy;
            }
            if dx > max_x {
                max_x = dx;
            }
            if dy > max_y {
                max_y = dy;
            }
        }
    }
    if min_x.is_infinite() {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (min_x, min_y, max_x, max_y)
    }
}

struct GlyphBlitParams {
    mask_w: u32,
    mask_h: u32,
    dst_x: i32,
    dst_y: i32,
    r: u8,
    g: u8,
    b: u8,
    base_alpha: u8,
}

/// Blit a glyph rotated by the text matrix's rotation component.
///
/// `cos_t` / `sin_t` are the normalised rotation extracted from the 2×2 part of
/// the render matrix.  `pen_x`, `pen_y` are the current pen position in
/// tile-local pixel space.  The function builds a tiny-skia affine transform that
/// places the glyph bitmap rotated around the pen position, correctly accounting
/// for the bearing offsets.
/// Supersampling factor for glyphs drawn through an affine `draw_pixmap`
/// (synthetic-italic shear or rotation): the glyph is rasterized this many times
/// larger and the affine folds in a `1/factor` downscale, so resampling reads from
/// a high-resolution master and edges stay crisp instead of smearing an
/// already-anti-aliased 1× mask.
#[cfg(feature = "render")]
const GLYPH_SUPERSAMPLE: f32 = 3.0;

#[cfg(feature = "render")]
#[allow(clippy::too_many_arguments)]
fn blit_glyph_rotated(
    glyph: &super::glyph_cache::GlyphBitmap,
    pen_x: f32,
    pen_y: f32,
    cos_t: f32,
    sin_t: f32,
    ss: f32,
    r: u8,
    g: u8,
    b: u8,
    base_alpha: u8,
    canvas: &mut PixmapBuffer,
) {
    let w = glyph.width;
    let h = glyph.height;
    let mut pm = match tiny_skia::Pixmap::new(w, h) {
        Some(p) => p,
        None => return,
    };
    // Fill the temporary pixmap with the glyph colour, premultiplied by coverage alpha.
    {
        let data = pm.data_mut();
        for (i, &cov) in glyph.pixels.iter().enumerate() {
            let a = ((cov as u32 * base_alpha as u32) / 255) as u8;
            let pr = ((r as u32 * a as u32) / 255) as u8;
            let pg = ((g as u32 * a as u32) / 255) as u8;
            let pb = ((b as u32 * a as u32) / 255) as u8;
            let idx = i * 4;
            if idx + 3 < data.len() {
                data[idx] = pr;
                data[idx + 1] = pg;
                data[idx + 2] = pb;
                data[idx + 3] = a;
            }
        }
    }

    // The bearing offsets in glyph-local space (y flipped: bearing_y is positive-upward).
    // In screen space the glyph's "anchor" (pen position) is at offset
    // (bearing_x, -bearing_y) from the top-left of the bitmap.
    // After rotation by θ the top-left of the rotated bitmap sits at:
    //   tx = pen_x + cos*bearing_x + sin*bearing_y − canvas_origin_x
    //   ty = pen_y + sin*bearing_x − cos*bearing_y − canvas_origin_y
    let bearing_x = glyph.bearing_x;
    let bearing_y = glyph.bearing_y;
    let ox = canvas.origin.x as f32;
    let oy = canvas.origin.y as f32;
    // `ss` master → 1×: fold a `1/ss` downscale into the rotation 2×2 and divide the
    // (supersampled) bearings back to 1× device space so the glyph lands true-size.
    let inv = 1.0 / ss;
    let bx = bearing_x * inv;
    let by = bearing_y * inv;
    let tx = pen_x - ox + cos_t * bx + sin_t * by;
    let ty = pen_y - oy + sin_t * bx - cos_t * by;

    let transform =
        tiny_skia::Transform::from_row(cos_t * inv, sin_t * inv, -sin_t * inv, cos_t * inv, tx, ty);
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    canvas
        .inner
        .draw_pixmap(0, 0, pm.as_ref(), &paint, transform, None);
}

/// Thicken a glyph's coverage mask by a box max-filter of the given pixel
/// `radius`, returning a new bitmap of the same dimensions.
///
/// Approximates synthetic bold (text render mode 2 + line width) in the live
/// preview without re-rasterizing: each output pixel takes the maximum coverage
/// in its `(2·radius+1)²` neighbourhood, fattening strokes. The saved PDF carries
/// the real `2 Tr`/`w` stroke, which compliant viewers render exactly; this is a
/// visual approximation for the editing tile only.
fn embolden_glyph(
    glyph: &super::glyph_cache::GlyphBitmap,
    radius: u32,
) -> super::glyph_cache::GlyphBitmap {
    let w = glyph.width as i32;
    let h = glyph.height as i32;
    let r = radius as i32;
    let mut out = vec![0u8; glyph.pixels.len()];
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for dy in -r..=r {
                let yy = y + dy;
                if yy < 0 || yy >= h {
                    continue;
                }
                for dx in -r..=r {
                    let xx = x + dx;
                    if xx < 0 || xx >= w {
                        continue;
                    }
                    let cov = glyph.pixels[(yy * w + xx) as usize];
                    if cov > m {
                        m = cov;
                    }
                }
            }
            out[(y * w + x) as usize] = m;
        }
    }
    super::glyph_cache::GlyphBitmap {
        pixels: out,
        ..glyph.clone()
    }
}

/// Blit a glyph bitmap with a horizontal shear (synthetic italic / any text
/// matrix carrying a non-zero `c` term), for the non-rotated case.
///
/// `skew` is the normalized render-matrix `c` term (`rm_c / scale_x`): the
/// horizontal shift per unit of height above the baseline. A glyph-local pixel
/// `(lx, ly)` (ly from the bitmap top) lands at screen
/// `x = pen_x + bearing_x + lx + skew·(bearing_y − ly)`, so pixels above the
/// baseline slide right and the glyph leans — matching the sheared `Tm` written
/// into the saved content stream.
#[allow(clippy::too_many_arguments)]
fn blit_glyph_sheared(
    glyph: &super::glyph_cache::GlyphBitmap,
    pen_x: f32,
    pen_y: f32,
    skew: f32,
    ss: f32,
    r: u8,
    g: u8,
    b: u8,
    base_alpha: u8,
    canvas: &mut PixmapBuffer,
) {
    let w = glyph.width;
    let h = glyph.height;
    let mut pm = match tiny_skia::Pixmap::new(w, h) {
        Some(p) => p,
        None => return,
    };
    {
        let data = pm.data_mut();
        for (i, &cov) in glyph.pixels.iter().enumerate() {
            let a = ((cov as u32 * base_alpha as u32) / 255) as u8;
            let pr = ((r as u32 * a as u32) / 255) as u8;
            let pg = ((g as u32 * a as u32) / 255) as u8;
            let pb = ((b as u32 * a as u32) / 255) as u8;
            let idx = i * 4;
            if idx + 3 < data.len() {
                data[idx] = pr;
                data[idx + 1] = pg;
                data[idx + 2] = pb;
                data[idx + 3] = a;
            }
        }
    }

    let ox = canvas.origin.x as f32;
    let oy = canvas.origin.y as f32;
    // `ss` master → 1×: fold a `1/ss` downscale into the shear and divide the
    // (supersampled) bearings back to 1× device space.
    //   x' = (lx − skew·ly)/ss + (pen_x − ox + bearing_x + skew·bearing_y)
    //   y' = ly/ss + (pen_y − oy − bearing_y)
    let inv = 1.0 / ss;
    let bx = glyph.bearing_x * inv;
    let by = glyph.bearing_y * inv;
    let tx = pen_x - ox + bx + skew * by;
    let ty = pen_y - oy - by;
    let transform = tiny_skia::Transform::from_row(inv, 0.0, -skew * inv, inv, tx, ty);
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    canvas
        .inner
        .draw_pixmap(0, 0, pm.as_ref(), &paint, transform, None);
}

/// Blit a single-channel alpha mask tinted with RGBA into the canvas.
///
/// `scratch` is a caller-owned buffer reused across calls to avoid per-glyph
/// heap allocation.  It is cleared and resized as needed; the caller keeps it
/// alive for the lifetime of the render loop.
fn blit_alpha_mask(
    mask: &[u8],
    p: &GlyphBlitParams,
    canvas: &mut PixmapBuffer,
    scratch: &mut Vec<u8>,
) {
    let n = p.mask_w as usize * p.mask_h as usize;
    scratch.clear();
    scratch.reserve(n * 4);
    for &coverage in mask {
        let a = ((coverage as u32 * p.base_alpha as u32) / 255) as u8;
        scratch.push(p.r);
        scratch.push(p.g);
        scratch.push(p.b);
        scratch.push(a);
    }
    canvas.blit_rgba(p.dst_x, p.dst_y, scratch, p.mask_w, p.mask_h);
}

/// Draw a filled rectangle as a placeholder for a character with no glyph.
fn draw_text_placeholder(
    pen_x: f32,
    baseline_y: f32,
    size_px: f32,
    color: [u8; 4],
    canvas: &mut PixmapBuffer,
) {
    let w = (size_px * 0.5).max(1.0) as u32;
    let h = (size_px * 0.8).max(1.0) as u32;
    let gx = pen_x as i32;
    let gy = (baseline_y - size_px * 0.8) as i32;
    let flat: Vec<u8> = std::iter::repeat_n(color, (w * h) as usize)
        .flatten()
        .collect();
    canvas.blit_rgba(gx, gy, &flat, w, h);
}

/// Bilinear RGBA image scaling.
///
/// Maps each destination pixel centre to fractional source coordinates and
/// blends the four surrounding source pixels by their bilinear weights.
/// Produces smooth results for both upscaling and downscaling.
fn scale_rgba_bilinear(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dst_w * dst_h * 4) as usize);
    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let sx = (dx as f32 + 0.5) * src_w as f32 / dst_w as f32 - 0.5;
            let sy = (dy as f32 + 0.5) * src_h as f32 / dst_h as f32 - 0.5;

            let x0 = (sx.floor() as i32).clamp(0, src_w as i32 - 1) as u32;
            let y0 = (sy.floor() as i32).clamp(0, src_h as i32 - 1) as u32;
            let x1 = (x0 + 1).min(src_w - 1);
            let y1 = (y0 + 1).min(src_h - 1);

            let fx = sx - sx.floor();
            let fy = sy - sy.floor();

            let p00 = rgba_pixel(src, src_w, x0, y0);
            let p10 = rgba_pixel(src, src_w, x1, y0);
            let p01 = rgba_pixel(src, src_w, x0, y1);
            let p11 = rgba_pixel(src, src_w, x1, y1);

            for c in 0..4 {
                let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
                let bot = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
                out.push((top * (1.0 - fy) + bot * fy).round() as u8);
            }
        }
    }
    out
}

#[inline]
fn rgba_pixel(src: &[u8], src_w: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = (y * src_w + x) as usize * 4;
    [src[idx], src[idx + 1], src[idx + 2], src[idx + 3]]
}

/// Expand a 1-bit packed ImageMask byte stream into an RGBA pixel buffer.
///
/// Each bit controls one pixel: when the bit value equals `paint_bit`, the pixel
/// is painted with `fill` (and is fully opaque); otherwise it is transparent.
/// Rows are padded to byte boundaries (MSB first per PDF spec §8.9.6.2).
fn decode_image_mask(
    raw: &[u8],
    width: u32,
    height: u32,
    paint_on_zero: bool,
    fill: [u8; 4],
) -> Vec<u8> {
    let row_bytes = (width as usize).div_ceil(8);
    let mut rgba = vec![0u8; (width * height) as usize * 4];
    for row in 0..height as usize {
        for col in 0..width as usize {
            let byte_idx = row * row_bytes + col / 8;
            let bit = if byte_idx < raw.len() {
                (raw[byte_idx] >> (7 - (col % 8))) & 1
            } else {
                0
            };
            if (bit == 0) == paint_on_zero {
                let base = (row * width as usize + col) * 4;
                rgba[base] = fill[0];
                rgba[base + 1] = fill[1];
                rgba[base + 2] = fill[2];
                rgba[base + 3] = fill[3];
            }
        }
    }
    rgba
}

fn pdf_int(obj: &PdfObject) -> Option<i64> {
    match obj {
        PdfObject::Integer(n) => Some(*n),
        PdfObject::Real(r) => Some(*r as i64),
        _ => None,
    }
}

/// Compute the axis-aligned destination rectangle for an image whose unit
/// square (0,0)-(1,1) is mapped through `ctm`.
///
/// Transforms all four corners and takes the bounding box, so rotation and
/// shear are handled correctly instead of only reading `ctm.a`/`ctm.d`.
fn ctm_to_dst_rect(ctm: &crate::content::graphics_state::Matrix) -> (i32, i32, u32, u32) {
    let corners = [
        ctm.transform_point(0.0, 0.0),
        ctm.transform_point(1.0, 0.0),
        ctm.transform_point(1.0, 1.0),
        ctm.transform_point(0.0, 1.0),
    ];
    let min_x = corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let max_x = corners
        .iter()
        .map(|p| p.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let max_y = corners
        .iter()
        .map(|p| p.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let dst_x = min_x.floor() as i32;
    let dst_y = min_y.floor() as i32;
    let dst_w = (max_x - min_x).ceil().max(0.0) as u32;
    let dst_h = (max_y - min_y).ceil().max(0.0) as u32;
    (dst_x, dst_y, dst_w, dst_h)
}

/// Resolve a PDF ColorSpace object to an effective device color space name.
///
/// Returns `(cs_name, indexed_lookup)` where `indexed_lookup` is
/// `Some((base_cs_name, lookup_bytes, base_channels))` when the space is Indexed.
/// `cs_name` is one of `"DeviceGray"`, `"DeviceRGB"`, `"DeviceCMYK"`.
fn resolve_image_color_space(
    cs: Option<&PdfObject>,
    doc: &PdfDocument,
) -> (String, Option<(String, Vec<u8>, usize)>) {
    let cs = match cs {
        Some(o) => o,
        None => return ("DeviceRGB".to_string(), None),
    };

    match cs {
        PdfObject::Name(n) => (map_cs_name(n), None),

        PdfObject::Array(arr) => {
            let kind = arr.first().and_then(|o| o.as_name()).unwrap_or("");
            match kind {
                "ICCBased" => {
                    // Extract N (number of components) from the ICC profile stream dict.
                    let n_ch = arr
                        .get(1)
                        .and_then(|r| doc.resolve(r).ok())
                        .and_then(|obj| match obj {
                            PdfObject::Stream(s) => s.dict.get("N").and_then(pdf_int),
                            _ => None,
                        })
                        .unwrap_or(3);
                    let name = match n_ch {
                        1 => "DeviceGray",
                        4 => "DeviceCMYK",
                        _ => "DeviceRGB",
                    };
                    (name.to_string(), None)
                }
                "Indexed" => {
                    // [/Indexed base hival lookup]
                    let base_cs_obj = arr.get(1);
                    let base_cs = base_cs_obj
                        .and_then(|o| o.as_name())
                        .map(map_cs_name)
                        .unwrap_or_else(|| "DeviceRGB".to_string());
                    let base_channels: usize = match base_cs.as_str() {
                        "DeviceGray" => 1,
                        "DeviceCMYK" => 4,
                        _ => 3,
                    };
                    let hival = arr.get(2).and_then(pdf_int).unwrap_or(255) as usize;
                    let lookup: Vec<u8> = match arr.get(3) {
                        Some(PdfObject::String(s)) => s.clone(),
                        Some(r) => doc
                            .resolve(r)
                            .ok()
                            .and_then(|o| match o {
                                PdfObject::Stream(s) => s.decode().ok(),
                                PdfObject::String(b) => Some(b),
                                _ => None,
                            })
                            .unwrap_or_default(),
                        None => Vec::new(),
                    };
                    // Ensure lookup covers [0..=hival] * base_channels bytes.
                    let expected = (hival + 1) * base_channels;
                    let lookup = if lookup.len() < expected {
                        let mut v = lookup;
                        v.resize(expected, 0);
                        v
                    } else {
                        lookup
                    };
                    (base_cs.clone(), Some((base_cs, lookup, base_channels)))
                }
                "CalRGB" => ("DeviceRGB".to_string(), None),
                "CalGray" => ("DeviceGray".to_string(), None),
                "Lab" => ("DeviceRGB".to_string(), None),
                "Separation" | "DeviceN" => {
                    // Approximate: treat as gray (1 component)
                    ("DeviceGray".to_string(), None)
                }
                _ => ("DeviceRGB".to_string(), None),
            }
        }

        // Resolve indirect reference (rare but valid for color space objects)
        obj => {
            if let Ok(resolved) = doc.resolve(obj) {
                resolve_image_color_space(Some(&resolved), doc)
            } else {
                ("DeviceRGB".to_string(), None)
            }
        }
    }
}

fn map_cs_name(name: &str) -> String {
    match name {
        "DeviceGray" | "CalGray" | "G" => "DeviceGray".to_string(),
        "DeviceCMYK" | "CMYK" => "DeviceCMYK".to_string(),
        _ => "DeviceRGB".to_string(),
    }
}

/// Expand an indexed image: map palette indices to `base_channels` bytes via the lookup table.
///
/// `bpc` is the number of bits per index (1, 2, 4, or 8).  For `bpc < 8`, multiple
/// indices are packed into each raw byte (PDF §8.9.3) and are unpacked here first.
fn apply_indexed_lookup(raw: &[u8], lookup: &[u8], base_channels: usize, bpc: u8) -> Vec<u8> {
    let indices: Vec<u8> = if bpc >= 8 {
        raw.to_vec()
    } else {
        let mask = (1u8 << bpc) - 1;
        let per_byte = 8 / bpc as usize;
        let mut unpacked = Vec::with_capacity(raw.len() * per_byte);
        for &byte in raw {
            for shift in (0..per_byte).rev() {
                unpacked.push((byte >> (shift * bpc as usize)) & mask);
            }
        }
        unpacked
    };

    let mut out = Vec::with_capacity(indices.len() * base_channels);
    for idx in indices {
        let offset = idx as usize * base_channels;
        if offset + base_channels <= lookup.len() {
            out.extend_from_slice(&lookup[offset..offset + base_channels]);
        } else {
            out.extend(std::iter::repeat_n(0u8, base_channels));
        }
    }
    out
}

/// Extract a `f64` value from an Integer or Real PDF object.
fn pdf_f64(obj: &PdfObject) -> Option<f64> {
    match obj {
        PdfObject::Integer(n) => Some(*n as f64),
        PdfObject::Real(r) => Some(*r),
        _ => None,
    }
}

/// Extract the `/BBox` array from a pattern dict as `[x0, y0, x1, y1]`.
fn extract_bbox(dict: &PdfDict) -> [f64; 4] {
    dict.get("BBox")
        .and_then(|o| {
            if let PdfObject::Array(a) = o {
                Some(a)
            } else {
                None
            }
        })
        .and_then(|a| {
            let v: Vec<f64> = a.iter().filter_map(pdf_f64).collect();
            if v.len() >= 4 {
                Some([v[0], v[1], v[2], v[3]])
            } else {
                None
            }
        })
        .unwrap_or([0.0, 0.0, 1.0, 1.0])
}

/// Extract the `/Matrix` array from a pattern dict as a `Matrix` (6 floats).
fn extract_matrix(dict: &PdfDict) -> Option<Matrix> {
    let arr = match dict.get("Matrix") {
        Some(PdfObject::Array(a)) => a,
        _ => return None,
    };
    let nums: Vec<f64> = arr.iter().filter_map(pdf_f64).collect();
    if nums.len() >= 6 {
        Some(Matrix {
            a: nums[0],
            b: nums[1],
            c: nums[2],
            d: nums[3],
            e: nums[4],
            f: nums[5],
        })
    } else {
        None
    }
}

/// Compute the integer index range [lo, hi] needed to cover `[clip_min, clip_max]`
/// with a cell of size `[cell_min, cell_max]` stepped by `step` (may be negative).
fn tiling_index_range(
    clip_min: f64,
    clip_max: f64,
    cell_min: f64,
    cell_max: f64,
    step: f64,
) -> (i32, i32) {
    if step.abs() < 0.001 {
        return (-1, 1);
    }
    let (lo, hi) = if step > 0.0 {
        (
            ((clip_min - cell_max) / step).floor() as i32 - 1,
            ((clip_max - cell_min) / step).ceil() as i32 + 1,
        )
    } else {
        (
            ((clip_max - cell_min) / step).floor() as i32 - 1,
            ((clip_min - cell_max) / step).ceil() as i32 + 1,
        )
    };
    (lo.min(hi), lo.max(hi))
}

/// Blit premultiplied RGBA `src` at tile-local position `(tile_x, tile_y)` into `dst`.
///
/// Uses premultiplied source-over compositing:
/// `dst_c = src_c + dst_c * (1 − src_a / 255)`.
fn blit_premultiplied(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    tile_x: i32,
    tile_y: i32,
    dst: &mut PixmapBuffer,
) {
    let dst_w = dst.width as i32;
    let dst_h = dst.height as i32;
    let data = dst.inner.data_mut();
    let data_len = data.len();
    for row in 0..src_h as i32 {
        let dy = tile_y + row;
        if dy < 0 || dy >= dst_h {
            continue;
        }
        for col in 0..src_w as i32 {
            let dx = tile_x + col;
            if dx < 0 || dx >= dst_w {
                continue;
            }
            let si = (row as usize * src_w as usize + col as usize) * 4;
            if si + 3 >= src.len() {
                continue;
            }
            let di = (dy as usize * dst_w as usize + dx as usize) * 4;
            if di + 3 >= data_len {
                continue;
            }
            let src_a = src[si + 3] as u32;
            let inv_a = 255 - src_a;
            // Premultiplied source-over: dst_new = src + dst * (1 - src_a/255)
            data[di] = (src[si] as u32 + (data[di] as u32 * inv_a + 127) / 255).min(255) as u8;
            data[di + 1] =
                (src[si + 1] as u32 + (data[di + 1] as u32 * inv_a + 127) / 255).min(255) as u8;
            data[di + 2] =
                (src[si + 2] as u32 + (data[di + 2] as u32 * inv_a + 127) / 255).min(255) as u8;
            data[di + 3] =
                (src[si + 3] as u32 + (data[di + 3] as u32 * inv_a + 127) / 255).min(255) as u8;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::page::Page;

    fn load_fixture(name: &str) -> PdfDocument {
        let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
        let data = std::fs::read(&path).unwrap_or_else(|_| panic!("fixture not found: {}", path));
        PdfDocument::parse(data).expect("parse failed")
    }

    #[test]
    fn test_render_page_buffer_size() {
        let doc = load_fixture("with_stream.pdf");
        let catalog = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let page_dict = catalog.get_page_dict(&doc, 0).unwrap();
        let page = Page::from_dict(&doc, &page_dict).unwrap();

        let buf = render_page(&doc, &page, 1.0).unwrap();
        let expected = buf.width as usize * buf.height as usize * 4;
        assert_eq!(
            buf.data().len(),
            expected,
            "buffer length should match width * height * 4"
        );
    }

    #[test]
    fn test_render_tile_smaller_than_page() {
        let doc = load_fixture("with_stream.pdf");
        let catalog = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let page_dict = catalog.get_page_dict(&doc, 0).unwrap();
        let page = Page::from_dict(&doc, &page_dict).unwrap();

        let mb = page.media_box;
        let tile = TileRect {
            x: mb.x1,
            y: mb.y1,
            width: mb.width() / 2.0,
            height: mb.height() / 2.0,
        };
        let buf = render_tile(&doc, &page, 1.0, tile).unwrap();
        // Tile should be roughly half the page dimensions.
        assert!(buf.width > 0 && buf.height > 0);
        assert!(buf.width <= (mb.width() / 2.0).ceil() as u32 + 1);
    }

    /// `render_block_tile` (block-sized buffer) must produce exactly the same
    /// pixels as cropping a full-page render at the block's rectangle. Proves the
    /// per-keystroke edit preview's tile render is correct (glyphs AND vector
    /// fills) — the historical sub-tile mis-map. Bit-exact at scale 1.0 with
    /// integer block coordinates and a zero-origin media box.
    #[test]
    fn block_tile_matches_full_crop() {
        let doc = load_fixture("with_stream.pdf");
        let catalog = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let page_dict = catalog.get_page_dict(&doc, 0).unwrap();
        let page = Page::from_dict(&doc, &page_dict).unwrap();
        let content = page.decode_contents(&doc).unwrap();

        let mb = page.media_box;
        // Bit-exact comparison requires a zero-origin media box (else the full
        // buffer's own tile origin offsets the crop indexing).
        assert_eq!(
            (mb.x1, mb.y1),
            (0.0, 0.0),
            "test assumes origin-0 media box"
        );
        let scale = 1.0f32;

        let full_tile = TileRect {
            x: mb.x1,
            y: mb.y1,
            width: mb.width(),
            height: mb.height(),
        };
        let (full_origin, full) =
            render_tile_content(&doc, &page, scale, full_tile, &content).unwrap();
        assert_eq!(full_origin, (0, 0));

        // A near-full block (integer coords) so it overlaps the page content.
        let block_tile = TileRect {
            x: 1.0,
            y: 1.0,
            width: (mb.width() - 2.0).floor(),
            height: (mb.height() - 2.0).floor(),
        };
        let (origin, blk) = render_block_tile(&doc, &page, scale, block_tile, &content).unwrap();

        // The block buffer must equal the same window of the full-page buffer.
        let fw = full.width as usize;
        let src = full.data();
        let bsrc = blk.data();
        let (ox, oy) = (origin.0 as usize, origin.1 as usize);
        assert!(ox + blk.width as usize <= full.width as usize);
        assert!(oy + blk.height as usize <= full.height as usize);
        let mut diffs = 0usize;
        for j in 0..blk.height as usize {
            for i in 0..blk.width as usize {
                let fi = ((oy + j) * fw + (ox + i)) * 4;
                let bi = (j * blk.width as usize + i) * 4;
                if src[fi..fi + 4] != bsrc[bi..bi + 4] {
                    diffs += 1;
                }
            }
        }
        assert_eq!(
            diffs, 0,
            "block tile must match the full-page crop pixel-for-pixel"
        );
        // Guard against a vacuous all-white comparison: the fixture must render
        // some non-white content into the block region.
        let has_content = bsrc
            .chunks_exact(4)
            .any(|p| p[0] != 255 || p[1] != 255 || p[2] != 255);
        assert!(
            has_content,
            "block tile rendered blank — test would be vacuous"
        );
    }

    #[test]
    fn test_apply_indexed_lookup_rgb() {
        // 2-entry palette: index 0 → red, index 1 → blue
        let lookup = vec![255u8, 0, 0, 0, 0, 255];
        let indices = vec![0u8, 1, 0];
        let expanded = apply_indexed_lookup(&indices, &lookup, 3, 8);
        assert_eq!(&expanded[0..3], &[255, 0, 0]); // red
        assert_eq!(&expanded[3..6], &[0, 0, 255]); // blue
        assert_eq!(&expanded[6..9], &[255, 0, 0]); // red again
    }

    #[test]
    fn test_apply_indexed_lookup_gray() {
        let lookup = vec![0u8, 128, 255];
        let indices = vec![2u8, 0, 1];
        let expanded = apply_indexed_lookup(&indices, &lookup, 1, 8);
        assert_eq!(expanded, vec![255, 0, 128]);
    }

    #[test]
    fn test_apply_indexed_lookup_4bit() {
        // 4-bit indexed: each byte contains 2 indices (hi nibble then lo nibble).
        // Palette: index 0 → black [0], index 1 → white [255], index 2 → gray [128]
        let lookup = vec![0u8, 255, 128];
        // Raw byte 0x01 = hi nibble 0, lo nibble 1 → indices [0, 1]
        // Raw byte 0x20 = hi nibble 2, lo nibble 0 → indices [2, 0]
        let raw = vec![0x01u8, 0x20];
        let expanded = apply_indexed_lookup(&raw, &lookup, 1, 4);
        assert_eq!(expanded, vec![0, 255, 128, 0]); // [black, white, gray, black]
    }

    #[test]
    fn test_apply_indexed_lookup_1bit() {
        // 1-bit indexed: each byte contains 8 indices (1 bit each, MSB first).
        // Palette: index 0 → 0x00, index 1 → 0xFF
        let lookup = vec![0x00u8, 0xFF];
        // Raw byte 0b10110100 → indices [1,0,1,1,0,1,0,0]
        let raw = vec![0b10110100u8];
        let expanded = apply_indexed_lookup(&raw, &lookup, 1, 1);
        assert_eq!(
            expanded,
            vec![0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0x00]
        );
    }

    #[test]
    fn test_resolve_color_space_simple_name() {
        // Simple DeviceRGB name
        let cs = PdfObject::Name("DeviceRGB".to_string());
        // Build a minimal document so doc.resolve() exists
        let doc_bytes = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\nxref\n0 3\n0000000000 65535 f \r\n0000000009 00000 n \r\n0000000058 00000 n \r\ntrailer << /Size 3 /Root 1 0 R >>\nstartxref\n110\n%%EOF\n";
        let doc = PdfDocument::parse(doc_bytes.to_vec()).unwrap();
        let (name, lookup) = resolve_image_color_space(Some(&cs), &doc);
        assert_eq!(name, "DeviceRGB");
        assert!(lookup.is_none());
    }

    #[test]
    fn test_resolve_color_space_cal_rgb() {
        let cs = PdfObject::Array(vec![
            PdfObject::Name("CalRGB".to_string()),
            PdfObject::Dictionary(crate::parser::objects::PdfDict::new()),
        ]);
        let doc_bytes = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\nxref\n0 3\n0000000000 65535 f \r\n0000000009 00000 n \r\n0000000058 00000 n \r\ntrailer << /Size 3 /Root 1 0 R >>\nstartxref\n110\n%%EOF\n";
        let doc = PdfDocument::parse(doc_bytes.to_vec()).unwrap();
        let (name, lookup) = resolve_image_color_space(Some(&cs), &doc);
        assert_eq!(name, "DeviceRGB");
        assert!(lookup.is_none());
    }

    #[test]
    fn test_tiling_index_range_positive_step() {
        // Clip [10, 50], cell [0, 20], step 20 → covers xi=0..3 plus margins.
        let (lo, hi) = tiling_index_range(10.0, 50.0, 0.0, 20.0, 20.0);
        assert!(
            lo <= 0 && hi >= 2,
            "xi range {lo}..{hi} should cover xi=0..2"
        );
    }

    #[test]
    fn test_tiling_index_range_negative_step() {
        // Negative step (Y-flip case).
        let (lo, hi) = tiling_index_range(-50.0, -10.0, -20.0, 0.0, -20.0);
        assert!(lo < hi, "should produce a valid range");
    }

    #[test]
    fn test_blit_premultiplied_opaque_pixel() {
        use super::super::canvas::TileOrigin;
        let mut buf = PixmapBuffer::new_transparent(4, 4, TileOrigin { x: 0, y: 0 }).unwrap();
        // Fully-opaque red pixel in premultiplied RGBA.
        let src = [255u8, 0, 0, 255];
        blit_premultiplied(&src, 1, 1, 2, 2, &mut buf);
        let data = buf.inner.data();
        let idx = (2 * 4 + 2) * 4;
        assert_eq!(data[idx], 255); // R
        assert_eq!(data[idx + 1], 0); // G
        assert_eq!(data[idx + 2], 0); // B
        assert_eq!(data[idx + 3], 255); // A
    }

    #[test]
    fn test_blit_premultiplied_out_of_bounds_safe() {
        use super::super::canvas::TileOrigin;
        let mut buf = PixmapBuffer::new_transparent(4, 4, TileOrigin { x: 0, y: 0 }).unwrap();
        let src = [255u8, 0, 0, 255];
        // Should not panic.
        blit_premultiplied(&src, 1, 1, -100, -100, &mut buf);
        blit_premultiplied(&src, 1, 1, 1000, 1000, &mut buf);
    }

    // scale_rgba_bilinear tests ---------------------------------------------------

    #[test]
    fn test_bilinear_scale_identity() {
        // src_w == dst_w, src_h == dst_h → output must be pixel-identical to input.
        #[rustfmt::skip]
        let src = vec![
            255, 0,   0,   255,   0, 255,   0, 255,
              0, 0, 255,   255, 128, 128, 128, 255,
        ];
        let out = scale_rgba_bilinear(&src, 2, 2, 2, 2);
        assert_eq!(out, src);
    }

    #[test]
    fn test_bilinear_scale_2x2_to_1x1() {
        // Four identical red pixels → 1×1 output must also be red.
        let src = vec![
            255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        let out = scale_rgba_bilinear(&src, 2, 2, 1, 1);
        assert_eq!(out.len(), 4);
        assert_eq!(&out[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn test_bilinear_scale_upscale_1x1() {
        // Single white pixel upscaled to 2×2: all four output pixels must be white.
        let src = vec![200u8, 100, 50, 255];
        let out = scale_rgba_bilinear(&src, 1, 1, 2, 2);
        assert_eq!(out.len(), 16);
        for chunk in out.chunks(4) {
            assert_eq!(chunk, &[200, 100, 50, 255]);
        }
    }

    #[test]
    fn test_bilinear_scale_horizontal_blend() {
        // 2×1 image: left pixel black (0,0,0,255), right pixel white (255,255,255,255).
        // Downscale to 1×1 → centre sample at 0.5 → equal blend of both → ~128.
        let src = vec![0u8, 0, 0, 255, 255, 255, 255, 255];
        let out = scale_rgba_bilinear(&src, 2, 1, 1, 1);
        assert_eq!(out.len(), 4);
        // Bilinear at pixel centre (0.5 in src space) gives 50/50 blend = 128 (rounded).
        assert!(
            (out[0] as i32 - 128).abs() <= 1,
            "R channel blend off: {}",
            out[0]
        );
        assert!(
            (out[1] as i32 - 128).abs() <= 1,
            "G channel blend off: {}",
            out[1]
        );
        assert!(
            (out[2] as i32 - 128).abs() <= 1,
            "B channel blend off: {}",
            out[2]
        );
    }

    #[test]
    fn imagemask_bit0_paints_fill_bit1_transparent() {
        // 2×1 image: first byte = 0b10000000.
        // With paint_on_zero=true (Decode [0 1]):
        //   pixel 0: bit=1 → transparent; pixel 1: bit=0 → fill color.
        let raw = [0b10000000u8];
        let fill = [255u8, 0, 0, 200]; // red, semi-opaque
        let rgba = decode_image_mask(&raw, 2, 1, true, fill);
        assert_eq!(rgba.len(), 8);
        // Pixel 0 (bit=1): transparent
        assert_eq!(&rgba[0..4], &[0, 0, 0, 0]);
        // Pixel 1 (bit=0): fill color
        assert_eq!(&rgba[4..8], &[255, 0, 0, 200]);
    }

    #[test]
    fn imagemask_inverted_decode_paints_on_bit1() {
        // With paint_on_zero=false (Decode [1 0]):
        //   pixel 0: bit=1 → fill; pixel 1: bit=0 → transparent.
        let raw = [0b10000000u8];
        let fill = [0u8, 255, 0, 255]; // green opaque
        let rgba = decode_image_mask(&raw, 2, 1, false, fill);
        // Pixel 0 (bit=1): fill color
        assert_eq!(&rgba[0..4], &[0, 255, 0, 255]);
        // Pixel 1 (bit=0): transparent
        assert_eq!(&rgba[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn imagemask_2x2_all_paint() {
        // 2×2: first byte 0b00000000, both rows from same byte → all bits 0 → all paint.
        let raw = [0b00000000u8, 0b00000000u8];
        let fill = [10u8, 20, 30, 255];
        let rgba = decode_image_mask(&raw, 2, 2, true, fill);
        assert_eq!(rgba.len(), 16);
        for pixel in rgba.chunks(4) {
            assert_eq!(pixel, &[10, 20, 30, 255]);
        }
    }

    // ── synthetic bold / italic glyph blits ──────────────────────────────────

    fn solid_glyph(w: u32, h: u32, bearing_y: f32) -> super::super::glyph_cache::GlyphBitmap {
        super::super::glyph_cache::GlyphBitmap {
            pixels: vec![255u8; (w * h) as usize],
            width: w,
            height: h,
            bearing_x: 0.0,
            bearing_y,
            advance_x: w as f32,
        }
    }

    #[test]
    fn embolden_glyph_thickens_isolated_pixel() {
        // A single covered pixel in a 3×3 mask, radius 1 → whole 3×3 becomes covered.
        let mut g = solid_glyph(3, 3, 3.0);
        g.pixels = vec![0; 9];
        g.pixels[4] = 255; // centre
        let bold = embolden_glyph(&g, 1);
        assert!(
            bold.pixels.iter().all(|&p| p == 255),
            "centre dilates to fill 3×3"
        );
        assert_eq!(bold.width, 3);
        assert_eq!(bold.height, 3);
    }

    #[test]
    fn blit_glyph_sheared_leans_top_right() {
        use super::super::canvas::TileOrigin;
        let mut buf = PixmapBuffer::new_transparent(40, 40, TileOrigin { x: 0, y: 0 }).unwrap();
        // A 1-px-wide, 8-px-tall vertical bar with its top 8px above the baseline.
        let glyph = solid_glyph(1, 8, 8.0);
        // Positive skew → pixels above the baseline slide right.
        blit_glyph_sheared(&glyph, 10.0, 30.0, 0.6, 1.0, 0, 0, 0, 255, &mut buf);

        // Collect, per row, the mean x of covered pixels.
        let data = buf.inner.data();
        let row_mean_x = |y: usize| -> Option<f32> {
            let mut sum = 0.0;
            let mut n = 0.0;
            for x in 0..40usize {
                let a = data[(y * 40 + x) * 4 + 3];
                if a > 0 {
                    sum += x as f32;
                    n += 1.0;
                }
            }
            (n > 0.0).then_some(sum / n)
        };
        // The covered rows span y ≈ 22..30 (top above baseline at y=30).
        let top = (22..26).find_map(row_mean_x).expect("top rows painted");
        let bottom = (28..32).find_map(row_mean_x).expect("bottom rows painted");
        assert!(
            top > bottom + 1.0,
            "sheared bar should lean right at the top: top_x={top} bottom_x={bottom}"
        );
    }

    #[test]
    fn blit_glyph_sheared_supersampled_matches_geometry() {
        use super::super::canvas::TileOrigin;
        // The supersampled blit (ss>1) must land in the SAME on-screen bounding box
        // as the 1× blit of the same logical glyph — proving the `1/ss` downscale
        // fold introduces no size or position drift, only crisper edges.
        let covered_bbox = |buf: &PixmapBuffer| -> (usize, usize, usize, usize) {
            let data = buf.inner.data();
            let (w, h) = (buf.width as usize, buf.height as usize);
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
            for y in 0..h {
                for x in 0..w {
                    if data[(y * w + x) * 4 + 3] > 0 {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x);
                        y1 = y1.max(y);
                    }
                }
            }
            (x0, y0, x1, y1)
        };

        let mut buf1 = PixmapBuffer::new_transparent(40, 40, TileOrigin { x: 0, y: 0 }).unwrap();
        let g1 = solid_glyph(2, 8, 8.0);
        blit_glyph_sheared(&g1, 10.0, 30.0, 0.6, 1.0, 0, 0, 0, 255, &mut buf1);

        let mut buf3 = PixmapBuffer::new_transparent(40, 40, TileOrigin { x: 0, y: 0 }).unwrap();
        // Same glyph rasterized 3× larger; ss=3 must downscale it to the same extent.
        let g3 = solid_glyph(6, 24, 24.0);
        blit_glyph_sheared(&g3, 10.0, 30.0, 0.6, 3.0, 0, 0, 0, 255, &mut buf3);

        let (ax0, ay0, ax1, ay1) = covered_bbox(&buf1);
        let (bx0, by0, bx1, by1) = covered_bbox(&buf3);
        let close = |a: usize, b: usize| (a as i32 - b as i32).abs() <= 2;
        assert!(
            close(ax0, bx0) && close(ay0, by0) && close(ax1, bx1) && close(ay1, by1),
            "supersampled sheared glyph must occupy the same box: \
             1x=({ax0},{ay0},{ax1},{ay1}) 3x=({bx0},{by0},{bx1},{by1})"
        );
    }
}
