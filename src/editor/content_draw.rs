//! High-level drawing helpers for page content editing.
//!
//! Each function opens a [`ContentLayer`] on the target page, emits the
//! appropriate PDF operators, and commits the layer. The result is a new
//! content stream appended to the page's `/Contents` array.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::font::write_standard_font;
use crate::writer::image::{write_image_xobject, write_jpeg_xobject, ImageColorSpace, ImageData};

use super::document_editor::PdfEditor;
use super::page_editor::begin_edit_page;

// ── Text drawing ─────────────────────────────────────────────────────────────

/// Style parameters for [`draw_text`].
pub struct TextStyle<'a> {
    /// One of the 14 standard PDF font names (e.g. `"Helvetica"`).
    pub font_name: &'a str,
    /// Font size in PDF points.
    pub font_size: f64,
    /// Fill color as `[r, g, b]` in 0.0–1.0.
    pub color: [f64; 3],
}

impl<'a> TextStyle<'a> {
    /// Create a text style with the given font, size, and color.
    pub fn new(font_name: &'a str, font_size: f64, color: [f64; 3]) -> Self {
        Self {
            font_name,
            font_size,
            color,
        }
    }
}

/// Draw a single line of text on page `page_index` at position `(x, y)`.
///
/// Registers the font in the page's `/Resources` if not already present.
/// Coordinates are in PDF user-space points (origin bottom-left).
pub fn draw_text(
    editor: &mut PdfEditor,
    page_index: usize,
    x: f64,
    y: f64,
    text: &str,
    style: &TextStyle,
) -> Result<()> {
    let font_id = write_standard_font(style.font_name, &mut editor.writer)?;
    let font_key = format!("F{}", font_id);

    let mut layer = begin_edit_page(editor, page_index)?;
    layer
        .builder
        .save()
        .set_fill_rgb(style.color[0], style.color[1], style.color[2])
        .begin_text()
        .set_font(&font_key, style.font_size)
        .move_text_pos(x, y)
        .show_text_str(text)
        .end_text()
        .restore();
    let page_id = layer.page_id;
    layer.commit(editor)?;

    // Register the font in the page's /Resources/Font dict.
    register_resource_entry(editor, page_id, "Font", &font_key, font_id)?;
    Ok(())
}

// ── Rectangle drawing ────────────────────────────────────────────────────────

/// Style parameters for [`draw_rect`].
pub struct RectStyle {
    /// Fill color `[r, g, b]` (0.0–1.0). `None` = no fill.
    pub fill: Option<[f64; 3]>,
    /// Stroke color `[r, g, b]` (0.0–1.0). `None` = no stroke.
    pub stroke: Option<[f64; 3]>,
    /// Stroke line width in points.
    pub line_width: f64,
}

impl RectStyle {
    /// Filled rectangle with no stroke.
    pub fn filled(color: [f64; 3]) -> Self {
        Self {
            fill: Some(color),
            stroke: None,
            line_width: 1.0,
        }
    }

    /// Stroked rectangle with no fill.
    pub fn stroked(color: [f64; 3], line_width: f64) -> Self {
        Self {
            fill: None,
            stroke: Some(color),
            line_width,
        }
    }

    /// Both filled and stroked.
    pub fn filled_stroked(fill: [f64; 3], stroke: [f64; 3], line_width: f64) -> Self {
        Self {
            fill: Some(fill),
            stroke: Some(stroke),
            line_width,
        }
    }
}

/// Draw a rectangle on page `page_index`.
///
/// `x`, `y` is the bottom-left corner; `width` and `height` are in points.
pub fn draw_rect(
    editor: &mut PdfEditor,
    page_index: usize,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    style: &RectStyle,
) -> Result<()> {
    let mut layer = begin_edit_page(editor, page_index)?;
    layer.builder.save().set_line_width(style.line_width);

    if let Some([r, g, b]) = style.fill {
        layer.builder.set_fill_rgb(r, g, b);
    }
    if let Some([r, g, b]) = style.stroke {
        layer.builder.set_stroke_rgb(r, g, b);
    }

    layer.builder.rect(x, y, width, height);

    match (style.fill.is_some(), style.stroke.is_some()) {
        (true, true) => {
            layer.builder.fill_stroke();
        }
        (true, false) => {
            layer.builder.fill();
        }
        (false, true) => {
            layer.builder.stroke();
        }
        (false, false) => {
            layer.builder.no_op();
        }
    };

    layer.builder.restore();
    layer.commit(editor)?;
    Ok(())
}

// ── Line drawing ─────────────────────────────────────────────────────────────

/// Draw a straight line from `(x1, y1)` to `(x2, y2)` on page `page_index`.
///
/// `color` is `[r, g, b]` in 0.0–1.0. `line_width` is in points.
#[allow(clippy::too_many_arguments)]
pub fn draw_line(
    editor: &mut PdfEditor,
    page_index: usize,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    color: [f64; 3],
    line_width: f64,
) -> Result<()> {
    let mut layer = begin_edit_page(editor, page_index)?;
    layer
        .builder
        .save()
        .set_line_width(line_width)
        .set_stroke_rgb(color[0], color[1], color[2])
        .move_to(x1, y1)
        .line_to(x2, y2)
        .stroke()
        .restore();
    layer.commit(editor)?;
    Ok(())
}

// ── Circle / ellipse ─────────────────────────────────────────────────────────

/// Draw an ellipse inscribed in the rectangle `(cx - rx, cy - ry, cx + rx, cy + ry)`.
///
/// Uses four cubic Bézier curves (the standard PDF approximation).
pub fn draw_ellipse(
    editor: &mut PdfEditor,
    page_index: usize,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    style: &RectStyle,
) -> Result<()> {
    let mut layer = begin_edit_page(editor, page_index)?;
    layer.builder.save().set_line_width(style.line_width);

    if let Some([r, g, b]) = style.fill {
        layer.builder.set_fill_rgb(r, g, b);
    }
    if let Some([r, g, b]) = style.stroke {
        layer.builder.set_stroke_rgb(r, g, b);
    }

    // Bézier approximation of an ellipse (kappa ≈ 0.5522847498).
    let k: f64 = 0.5522847498;
    let kx = rx * k;
    let ky = ry * k;

    layer.builder.move_to(cx + rx, cy);
    layer
        .builder
        .curve_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
    layer
        .builder
        .curve_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
    layer
        .builder
        .curve_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
    layer
        .builder
        .curve_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
    layer.builder.close_path();

    match (style.fill.is_some(), style.stroke.is_some()) {
        (true, true) => {
            layer.builder.fill_stroke();
        }
        (true, false) => {
            layer.builder.fill();
        }
        (false, true) => {
            layer.builder.stroke();
        }
        (false, false) => {
            layer.builder.no_op();
        }
    };

    layer.builder.restore();
    layer.commit(editor)?;
    Ok(())
}

// ── Image placement ──────────────────────────────────────────────────────────

/// Place a raw-pixel image on page `page_index` at position `(x, y)` with
/// display size `(display_w, display_h)` in points.
///
/// `pixels` is row-major top-to-bottom, `channels` = 1 (gray), 3 (RGB), or 4 (CMYK).
#[allow(clippy::too_many_arguments)]
pub fn place_image(
    editor: &mut PdfEditor,
    page_index: usize,
    x: f64,
    y: f64,
    display_w: f64,
    display_h: f64,
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: u8,
) -> Result<()> {
    let color_space = match channels {
        1 => ImageColorSpace::DeviceGray,
        3 => ImageColorSpace::DeviceRGB,
        4 => ImageColorSpace::DeviceCMYK,
        _ => {
            return Err(PdfError::invalid_structure(format!(
                "place_image: unsupported channel count {}",
                channels
            )))
        }
    };

    let image = ImageData {
        pixels: pixels.to_vec(),
        width,
        height,
        color_space,
        bits_per_component: 8,
    };

    let img_id = write_image_xobject(&image, &mut editor.writer)?;
    let img_key = format!("Im{}", img_id);

    let mut layer = begin_edit_page(editor, page_index)?;
    let page_id = layer.page_id;
    layer
        .builder
        .save()
        .concat_matrix(display_w, 0.0, 0.0, display_h, x, y)
        .do_xobject(&img_key)
        .restore();
    layer.commit(editor)?;

    register_resource_entry(editor, page_id, "XObject", &img_key, img_id)?;
    Ok(())
}

/// Place a JPEG image on page `page_index` at position `(x, y)` with
/// display size `(display_w, display_h)` in points.
///
/// The JPEG bytes are embedded as-is (DCTDecode pass-through).
#[allow(clippy::too_many_arguments)]
pub fn place_jpeg(
    editor: &mut PdfEditor,
    page_index: usize,
    x: f64,
    y: f64,
    display_w: f64,
    display_h: f64,
    jpeg_data: &[u8],
    pixel_width: u32,
    pixel_height: u32,
) -> Result<()> {
    let img_id = write_jpeg_xobject(jpeg_data, pixel_width, pixel_height, &mut editor.writer)?;
    let img_key = format!("Im{}", img_id);

    let mut layer = begin_edit_page(editor, page_index)?;
    let page_id = layer.page_id;
    layer
        .builder
        .save()
        .concat_matrix(display_w, 0.0, 0.0, display_h, x, y)
        .do_xobject(&img_key)
        .restore();
    layer.commit(editor)?;

    register_resource_entry(editor, page_id, "XObject", &img_key, img_id)?;
    Ok(())
}

// ── Resource registration helpers ────────────────────────────────────────────

/// Ensure the page's `/Resources/Font` dict contains an entry for `font_key → font_id`.
/// Resolve a [`PdfObject`] that may be an inline dictionary or an indirect
/// reference into an owned [`PdfDict`], using the editor's copy-on-write object
/// view. Returns `None` for anything that isn't (or doesn't resolve to) a dict.
fn resolve_dict(editor: &PdfEditor, obj: &PdfObject) -> Option<PdfDict> {
    match obj {
        PdfObject::Dictionary(d) => Some(d.clone()),
        PdfObject::Reference(id, _) => editor
            .get_object(*id)
            .ok()
            .and_then(|o| o.as_dict().cloned()),
        _ => None,
    }
}

/// Resolve a page's effective `/Resources` dictionary into an owned copy.
///
/// Handles an inline dict, an indirect reference, and inheritance from an
/// ancestor `/Pages` node (walking `/Parent`, depth-limited at 64). Returns an
/// empty dict when no `/Resources` is found anywhere in the chain. The first
/// node carrying a `/Resources` key wins (standard PDF inheritance semantics).
fn effective_resources(editor: &PdfEditor, page_dict: &PdfDict) -> PdfDict {
    let mut current = page_dict.clone();
    for _ in 0..64 {
        if let Some(res) = current.get("Resources") {
            return resolve_dict(editor, res).unwrap_or_default();
        }
        match current.get("Parent") {
            Some(PdfObject::Reference(id, _)) => match editor.get_object(*id) {
                Ok(obj) => match obj.as_dict() {
                    Some(d) => current = d.clone(),
                    None => return PdfDict::new(),
                },
                Err(_) => return PdfDict::new(),
            },
            _ => return PdfDict::new(),
        }
    }
    PdfDict::new()
}

/// Ensure the page's `/Resources/<category>` sub-dictionary contains
/// `key → obj_id`, where `category` is `"Font"` or `"XObject"`.
///
/// Resolves an indirect `/Resources` and an indirect sub-dictionary, and copies
/// inherited resources down onto the page, so existing fonts/xobjects are never
/// dropped. The merged resources are inlined on the page dict, isolating the
/// change from any `/Resources` object shared by other pages.
fn register_resource_entry(
    editor: &mut PdfEditor,
    page_id: u32,
    category: &str,
    key: &str,
    obj_id: u32,
) -> Result<()> {
    let page_obj = editor.get_object(page_id)?;
    let mut page_dict = page_obj
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("page is not a dict"))?
        .clone();

    let mut resources = effective_resources(editor, &page_dict);

    let mut sub = match resources.get(category) {
        Some(obj) => resolve_dict(editor, obj).unwrap_or_default(),
        None => PdfDict::new(),
    };

    sub.insert(key.to_owned(), PdfObject::Reference(obj_id, 0));
    resources.insert(category.to_owned(), PdfObject::Dictionary(sub));
    page_dict.insert("Resources".to_owned(), PdfObject::Dictionary(resources));
    editor.replace_object(page_id, PdfObject::Dictionary(page_dict));
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::page_editor::add_blank_page;
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

    fn editor_with_blank_page() -> (PdfEditor, Vec<u8>) {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        (editor, original)
    }

    #[test]
    fn draw_text_produces_parseable_pdf() {
        let (mut editor, original) = editor_with_blank_page();
        let style = TextStyle::new("Helvetica", 14.0, [0.0, 0.0, 0.0]);
        draw_text(&mut editor, 0, 72.0, 720.0, "Hello World", &style).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn draw_text_registers_font_in_resources() {
        let (mut editor, _) = editor_with_blank_page();
        let style = TextStyle::new("Courier", 12.0, [1.0, 0.0, 0.0]);
        draw_text(&mut editor, 0, 10.0, 10.0, "test", &style).unwrap();
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        let resources = page_dict.get("Resources").unwrap().as_dict().unwrap();
        let fonts = resources.get("Font").unwrap().as_dict().unwrap();
        assert!(!fonts.is_empty(), "font dict must have at least one entry");
    }

    #[test]
    fn draw_rect_filled_produces_parseable_pdf() {
        let (mut editor, original) = editor_with_blank_page();
        let style = RectStyle::filled([1.0, 0.0, 0.0]);
        draw_rect(&mut editor, 0, 50.0, 50.0, 200.0, 100.0, &style).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn draw_rect_stroked_produces_parseable_pdf() {
        let (mut editor, original) = editor_with_blank_page();
        let style = RectStyle::stroked([0.0, 0.0, 1.0], 2.0);
        draw_rect(&mut editor, 0, 10.0, 10.0, 100.0, 50.0, &style).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn draw_line_produces_parseable_pdf() {
        let (mut editor, original) = editor_with_blank_page();
        draw_line(&mut editor, 0, 0.0, 0.0, 595.0, 842.0, [0.0, 0.0, 0.0], 1.0).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn draw_ellipse_produces_parseable_pdf() {
        let (mut editor, original) = editor_with_blank_page();
        let style = RectStyle::filled_stroked([0.0, 1.0, 0.0], [0.0, 0.0, 0.0], 1.5);
        draw_ellipse(&mut editor, 0, 200.0, 400.0, 80.0, 50.0, &style).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn place_image_registers_xobject() {
        let (mut editor, original) = editor_with_blank_page();
        // 2×2 red RGB image
        let pixels = vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0];
        place_image(&mut editor, 0, 100.0, 100.0, 200.0, 200.0, &pixels, 2, 2, 3).unwrap();
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        let resources = page_dict.get("Resources").unwrap().as_dict().unwrap();
        let xobjects = resources.get("XObject").unwrap().as_dict().unwrap();
        assert!(!xobjects.is_empty(), "XObject dict must have an entry");
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn place_jpeg_produces_parseable_pdf() {
        let (mut editor, original) = editor_with_blank_page();
        let fake_jpeg = b"\xFF\xD8\xFF\xE0dummy\xFF\xD9";
        place_jpeg(&mut editor, 0, 50.0, 50.0, 100.0, 100.0, fake_jpeg, 10, 10).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn place_image_invalid_channels_errors() {
        let (mut editor, _) = editor_with_blank_page();
        let pixels = vec![0u8; 8];
        let err = place_image(&mut editor, 0, 0.0, 0.0, 10.0, 10.0, &pixels, 2, 2, 2).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    /// Helper: a one-key dictionary `{ "Type": /<type_name> }`.
    fn typed_dict(type_name: &str) -> PdfObject {
        let mut d = PdfDict::new();
        d.insert("Type".to_owned(), PdfObject::Name(type_name.to_owned()));
        PdfObject::Dictionary(d)
    }

    /// Regression: when `/Resources/Font` is an *indirect* reference, drawing a
    /// standard font must merge into the existing fonts, not replace them.
    #[test]
    fn draw_text_preserves_indirect_font_dict() {
        let (mut editor, _) = editor_with_blank_page();
        let (page_id, mut page_dict) = editor.get_page_dict(0).unwrap();

        // Existing CID font F1 behind an indirect /Font dictionary object.
        let cid_font_id = editor.add_object(typed_dict("Font"));
        let mut font_dict = PdfDict::new();
        font_dict.insert("F1".to_owned(), PdfObject::Reference(cid_font_id, 0));
        let font_dict_id = editor.add_object(PdfObject::Dictionary(font_dict));

        let mut resources = PdfDict::new();
        resources.insert("Font".to_owned(), PdfObject::Reference(font_dict_id, 0));
        page_dict.insert("Resources".to_owned(), PdfObject::Dictionary(resources));
        editor.replace_object(page_id, PdfObject::Dictionary(page_dict));

        let style = TextStyle::new("Helvetica", 12.0, [0.0, 0.0, 0.0]);
        draw_text(&mut editor, 0, 10.0, 10.0, "hi", &style).unwrap();

        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        let resources = page_dict.get("Resources").unwrap().as_dict().unwrap();
        let fonts = resources.get("Font").unwrap().as_dict().unwrap();
        assert!(
            fonts.contains_key("F1"),
            "existing CID font F1 must survive"
        );
        assert!(
            fonts.len() >= 2,
            "new standard font must be added alongside F1"
        );
    }

    /// Regression: a page with no own `/Resources` inherits from the `/Pages`
    /// node; drawing must copy those resources down (fonts *and* xobjects),
    /// not wipe them with a fresh page-level dict.
    #[test]
    fn draw_text_inherits_and_preserves_pages_resources() {
        let (mut editor, _) = editor_with_blank_page();
        let pages_id = editor.pages_id;

        let f1_id = editor.add_object(typed_dict("Font"));
        let im1_id = editor.add_object(typed_dict("XObject"));
        let mut fonts = PdfDict::new();
        fonts.insert("F1".to_owned(), PdfObject::Reference(f1_id, 0));
        let mut xobjects = PdfDict::new();
        xobjects.insert("Im1".to_owned(), PdfObject::Reference(im1_id, 0));
        let mut res = PdfDict::new();
        res.insert("Font".to_owned(), PdfObject::Dictionary(fonts));
        res.insert("XObject".to_owned(), PdfObject::Dictionary(xobjects));

        let mut pages_dict = editor
            .get_object(pages_id)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        pages_dict.insert("Resources".to_owned(), PdfObject::Dictionary(res));
        editor.replace_object(pages_id, PdfObject::Dictionary(pages_dict));

        // Strip the page's own /Resources so it must inherit from /Pages.
        let (page_id, mut page_dict) = editor.get_page_dict(0).unwrap();
        page_dict.shift_remove("Resources");
        page_dict.insert("Parent".to_owned(), PdfObject::Reference(pages_id, 0));
        editor.replace_object(page_id, PdfObject::Dictionary(page_dict));

        let style = TextStyle::new("Helvetica", 12.0, [0.0, 0.0, 0.0]);
        draw_text(&mut editor, 0, 10.0, 10.0, "hi", &style).unwrap();

        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        let resources = page_dict
            .get("Resources")
            .expect("page must gain its own inlined /Resources")
            .as_dict()
            .unwrap();
        let fonts = resources.get("Font").unwrap().as_dict().unwrap();
        assert!(
            fonts.contains_key("F1"),
            "inherited font F1 must be copied down"
        );
        assert!(fonts.len() >= 2, "new font must be added");
        let xobjects = resources.get("XObject").unwrap().as_dict().unwrap();
        assert!(
            xobjects.contains_key("Im1"),
            "inherited image Im1 must survive"
        );
    }

    /// Regression: an indirect `/Resources/XObject` reference must be resolved
    /// and merged when placing a new image.
    #[test]
    fn place_image_preserves_indirect_xobject_dict() {
        let (mut editor, _) = editor_with_blank_page();
        let (page_id, mut page_dict) = editor.get_page_dict(0).unwrap();

        let existing_img_id = editor.add_object(typed_dict("XObject"));
        let mut xobj_dict = PdfDict::new();
        xobj_dict.insert("Im1".to_owned(), PdfObject::Reference(existing_img_id, 0));
        let xobj_dict_id = editor.add_object(PdfObject::Dictionary(xobj_dict));

        let mut resources = PdfDict::new();
        resources.insert("XObject".to_owned(), PdfObject::Reference(xobj_dict_id, 0));
        page_dict.insert("Resources".to_owned(), PdfObject::Dictionary(resources));
        editor.replace_object(page_id, PdfObject::Dictionary(page_dict));

        let pixels = vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0];
        place_image(&mut editor, 0, 100.0, 100.0, 200.0, 200.0, &pixels, 2, 2, 3).unwrap();

        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        let resources = page_dict.get("Resources").unwrap().as_dict().unwrap();
        let xobjects = resources.get("XObject").unwrap().as_dict().unwrap();
        assert!(
            xobjects.contains_key("Im1"),
            "existing image Im1 must survive"
        );
        assert!(xobjects.len() >= 2, "new image must be added alongside Im1");
    }
}
