//! User-facing watermark API — text and image watermarks.
//!
//! Gated on the `writer` Cargo feature and a Pro-tier license.

use crate::editor::content_draw::register_resource_entry;
use crate::editor::document_editor::PdfEditor;
use crate::editor::page_editor::begin_edit_page;
use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::image::{write_image_xobject, ImageColorSpace, ImageData};

// ── Types ─────────────────────────────────────────────────────────────────────

/// Parameters for a text watermark.
#[derive(Debug, Clone)]
pub struct TextWatermark {
    /// Watermark text (UTF-8, PDFDocEncoding on output).
    pub text: String,
    /// Font size in points.
    pub font_size: f64,
    /// Fill color as `[r, g, b]` in 0.0–1.0.
    pub color: [f64; 3],
    /// Opacity: 0.0 = invisible, 1.0 = opaque. Applied via `/ExtGState /ca`.
    pub opacity: f64,
    /// Counter-clockwise rotation in degrees (45.0 = diagonal).
    pub angle_degrees: f64,
    /// Tile the watermark across the whole page when `true`.
    pub repeat: bool,
    /// Distance between tile origins when `repeat` is `true`.
    pub tile_spacing: f64,
}

impl Default for TextWatermark {
    fn default() -> Self {
        Self {
            text: "WATERMARK".to_owned(),
            font_size: 36.0,
            color: [0.7, 0.7, 0.7],
            opacity: 0.4,
            angle_degrees: 45.0,
            repeat: false,
            tile_spacing: 200.0,
        }
    }
}

/// Parameters for an image watermark.
#[derive(Debug, Clone)]
pub struct ImageWatermark {
    /// Raw pixel image data.
    pub pixels: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Number of color channels: 1 = gray, 3 = RGB, 4 = CMYK.
    pub channels: u8,
    /// Placement rect `[x1, y1, x2, y2]` in PDF user-space points.
    pub rect: [f64; 4],
    /// Opacity: 0.0 = invisible, 1.0 = opaque. Applied via `/ExtGState /ca`.
    pub opacity: f64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Add a text watermark to page `page_index` (0-based).
///
/// Requires a Pro-tier license. Uses the standard Helvetica font.
pub fn add_text_watermark(
    editor: &mut PdfEditor,
    page_index: usize,
    wm: &TextWatermark,
) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "watermark")?;

    let (page_id, page_dict) = editor.get_page_dict(page_index)?;
    let (pw, ph) = page_dimensions(&page_dict);

    // Register Helvetica font and ExtGState for opacity.
    let font_id = crate::writer::font::write_standard_font("Helvetica", &mut editor.writer)?;
    let font_key = format!("F{}", font_id);
    let gs_id = make_opacity_gstate(editor, wm.opacity)?;
    let gs_key = format!("WmGs{}", (wm.opacity * 1000.0) as u32);

    let mut layer = begin_edit_page(editor, page_index)?;
    build_text_watermark_content(wm, pw, ph, &font_key, &gs_key, &mut layer.builder);
    layer.commit(editor)?;

    register_resource_entry(editor, page_id, "Font", &font_key, font_id)?;
    register_resource_entry(editor, page_id, "ExtGState", &gs_key, gs_id)?;
    Ok(())
}

/// Add a text watermark to every page in the document.
///
/// Requires a Pro-tier license.
pub fn add_watermark_all_pages(editor: &mut PdfEditor, wm: &TextWatermark) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "watermark")?;

    let page_count = editor.page_count()?;
    for i in 0..page_count {
        add_text_watermark(editor, i, wm)?;
    }
    Ok(())
}

/// Add an image watermark to page `page_index` (0-based).
///
/// `wm.rect` is `[x1, y1, x2, y2]` in PDF user-space points.
/// Requires a Pro-tier license.
pub fn add_image_watermark(
    editor: &mut PdfEditor,
    page_index: usize,
    wm: &ImageWatermark,
) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "watermark")?;

    let color_space = match wm.channels {
        1 => ImageColorSpace::DeviceGray,
        3 => ImageColorSpace::DeviceRGB,
        4 => ImageColorSpace::DeviceCMYK,
        _ => {
            return Err(crate::error::PdfError::invalid_structure(format!(
                "add_image_watermark: unsupported channel count {}",
                wm.channels
            )))
        }
    };

    let image = ImageData {
        pixels: wm.pixels.clone(),
        width: wm.width,
        height: wm.height,
        color_space,
        bits_per_component: 8,
    };

    let (page_id, _) = editor.get_page_dict(page_index)?;
    let img_id = write_image_xobject(&image, &mut editor.writer)?;
    let img_key = format!("WmImg{}", img_id);
    let gs_id = make_opacity_gstate(editor, wm.opacity)?;
    let gs_key = format!("WmGs{}", (wm.opacity * 1000.0) as u32);

    let x = wm.rect[0];
    let y = wm.rect[1];
    let w = wm.rect[2] - wm.rect[0];
    let h = wm.rect[3] - wm.rect[1];

    let mut layer = begin_edit_page(editor, page_index)?;
    layer
        .builder
        .save()
        .apply_gs(&gs_key)
        .concat_matrix(w, 0.0, 0.0, h, x, y)
        .do_xobject(&img_key)
        .restore();
    layer.commit(editor)?;

    register_resource_entry(editor, page_id, "XObject", &img_key, img_id)?;
    register_resource_entry(editor, page_id, "ExtGState", &gs_key, gs_id)?;
    Ok(())
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Build a `/ExtGState` object with `/ca opacity` (fill alpha) and return its ID.
fn make_opacity_gstate(editor: &mut PdfEditor, opacity: f64) -> Result<u32> {
    let mut gs = PdfDict::new();
    gs.insert("Type".to_owned(), PdfObject::Name("ExtGState".to_owned()));
    gs.insert("ca".to_owned(), PdfObject::Real(opacity));
    gs.insert("CA".to_owned(), PdfObject::Real(opacity));
    Ok(editor.add_object(PdfObject::Dictionary(gs)))
}

/// Return `(width, height)` from the page's `/MediaBox`, defaulting to A4/Letter.
fn page_dimensions(page_dict: &PdfDict) -> (f64, f64) {
    match page_dict.get("MediaBox") {
        Some(PdfObject::Array(a)) if a.len() == 4 => {
            (to_f64(&a[2]) - to_f64(&a[0]), to_f64(&a[3]) - to_f64(&a[1]))
        }
        _ => (612.0, 792.0),
    }
}

fn to_f64(o: &PdfObject) -> f64 {
    match o {
        PdfObject::Real(r) => *r,
        PdfObject::Integer(i) => *i as f64,
        _ => 0.0,
    }
}

/// Emit watermark operators directly into `cb`.
fn build_text_watermark_content(
    wm: &TextWatermark,
    pw: f64,
    ph: f64,
    font_key: &str,
    gs_key: &str,
    cb: &mut crate::writer::content_builder::ContentBuilder,
) {
    let angle_rad = wm.angle_degrees * std::f64::consts::PI / 180.0;
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();
    // Rough text width estimate: 0.55 × font_size per character.
    let approx_w = wm.font_size * 0.55 * wm.text.len() as f64;

    cb.save()
        .apply_gs(gs_key)
        .set_fill_rgb(wm.color[0], wm.color[1], wm.color[2]);

    if wm.repeat {
        let spacing = wm.tile_spacing;
        let mut y = 0.0_f64;
        while y < ph + spacing {
            let mut x = 0.0_f64;
            while x < pw + spacing {
                emit_watermark_text(
                    cb,
                    &wm.text,
                    wm.font_size,
                    font_key,
                    x,
                    y,
                    cos_a,
                    sin_a,
                    approx_w,
                );
                x += spacing;
            }
            y += spacing;
        }
    } else {
        emit_watermark_text(
            cb,
            &wm.text,
            wm.font_size,
            font_key,
            pw / 2.0,
            ph / 2.0,
            cos_a,
            sin_a,
            approx_w,
        );
    }

    cb.restore();
}

/// Emit operators for one watermark text instance centered on `(cx, cy)`.
#[allow(clippy::too_many_arguments)]
fn emit_watermark_text(
    cb: &mut crate::writer::content_builder::ContentBuilder,
    text: &str,
    font_size: f64,
    font_key: &str,
    cx: f64,
    cy: f64,
    cos_a: f64,
    sin_a: f64,
    approx_w: f64,
) {
    // Rotation matrix around origin, then translate to (cx, cy).
    cb.concat_matrix(cos_a, sin_a, -sin_a, cos_a, cx, cy)
        .begin_text()
        .set_font(font_key, font_size)
        .move_text_pos(-approx_w / 2.0, -font_size / 2.0)
        .show_text_str(text)
        .end_text()
        // Invert the rotation+translation to restore the CTM.
        .concat_matrix(
            cos_a,
            -sin_a,
            sin_a,
            cos_a,
            -cx * cos_a - cy * sin_a,
            cx * sin_a - cy * cos_a,
        );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfDocument;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    #[test]
    fn add_text_watermark_produces_parseable_pdf() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        let wm = TextWatermark {
            text: "CONFIDENTIAL".to_owned(),
            ..Default::default()
        };
        add_text_watermark(&mut editor, 0, &wm).unwrap();
        let saved = editor.save_append(&original).unwrap();
        PdfDocument::parse(saved).unwrap();
    }

    #[test]
    fn add_text_watermark_registers_font_and_gstate() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let wm = TextWatermark::default();
        add_text_watermark(&mut editor, 0, &wm).unwrap();
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        let resources = page_dict.get("Resources").unwrap().as_dict().unwrap();
        assert!(
            resources
                .get("Font")
                .and_then(|d| d.as_dict())
                .map_or(false, |d| !d.is_empty()),
            "Font resource must be registered"
        );
        assert!(
            resources
                .get("ExtGState")
                .and_then(|d| d.as_dict())
                .map_or(false, |d| !d.is_empty()),
            "ExtGState resource must be registered"
        );
    }

    #[test]
    fn add_watermark_all_pages_applies_to_each_page() {
        let data = load("multipage.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        let wm = TextWatermark::default();
        let page_count = editor.page_count().unwrap();
        add_watermark_all_pages(&mut editor, &wm).unwrap();
        let saved = editor.save_append(&original).unwrap();
        let doc = PdfDocument::parse(saved).unwrap();
        assert_eq!(doc.page_count().unwrap(), page_count);
    }

    #[test]
    fn add_image_watermark_produces_parseable_pdf() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        // 2×2 gray pixels
        let wm = ImageWatermark {
            pixels: vec![128, 128, 128, 128],
            width: 2,
            height: 2,
            channels: 1,
            rect: [100.0, 100.0, 300.0, 300.0],
            opacity: 0.5,
        };
        add_image_watermark(&mut editor, 0, &wm).unwrap();
        let saved = editor.save_append(&original).unwrap();
        PdfDocument::parse(saved).unwrap();
    }

    #[test]
    fn add_image_watermark_rejects_bad_channel_count() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let wm = ImageWatermark {
            pixels: vec![0, 0],
            width: 1,
            height: 1,
            channels: 2, // unsupported
            rect: [0.0, 0.0, 100.0, 100.0],
            opacity: 1.0,
        };
        assert!(add_image_watermark(&mut editor, 0, &wm).is_err());
    }

    #[test]
    fn add_text_watermark_repeat_produces_parseable_pdf() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        let wm = TextWatermark {
            repeat: true,
            tile_spacing: 150.0,
            ..Default::default()
        };
        add_text_watermark(&mut editor, 0, &wm).unwrap();
        let saved = editor.save_append(&original).unwrap();
        PdfDocument::parse(saved).unwrap();
    }
}
