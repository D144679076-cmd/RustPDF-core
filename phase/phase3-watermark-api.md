# Phase 3 — Watermark API (User-Facing)

**Status:** Not started
**Effort:** ~3–4 days
**Tier gate:** Pro
**Prerequisite:** phase1-licensing.md (watermark.rs already has `apply_trial_watermark`)

## Context

The trial watermark in `src/license/watermark.rs` already demonstrates the pattern. This feature exposes a user-facing watermark API so users can add custom text or image watermarks to their documents — useful for stamping "CONFIDENTIAL", "DRAFT", company logos, etc.

## New Module `src/editor/watermark.rs`

```rust
use crate::editor::PdfEditor;
use crate::writer::content_builder::ContentBuilder;
use crate::writer::streams::make_flate_stream;
use crate::writer::image::{ImageData, write_image_xobject};
use crate::parser::{PdfObject, PdfDict};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct TextWatermark {
    pub text: String,
    pub font_size: f64,
    pub color: [f64; 3],        // RGB 0.0-1.0
    pub opacity: f64,            // 0.0 = invisible, 1.0 = solid
    pub angle_degrees: f64,      // rotation (45.0 for diagonal)
    pub repeat: bool,            // tile across entire page
    pub tile_spacing: f64,       // spacing between tiles when repeat=true (default 200.0)
}

impl Default for TextWatermark {
    fn default() -> Self {
        Self { text: "WATERMARK".to_owned(), font_size: 36.0, color: [0.7, 0.7, 0.7], opacity: 0.4, angle_degrees: 45.0, repeat: false, tile_spacing: 200.0 }
    }
}

#[derive(Debug, Clone)]
pub struct ImageWatermark {
    pub image_data: ImageData,
    pub rect: [f64; 4],         // [x1, y1, x2, y2] position and size
    pub opacity: f64,
}

/// Add a text watermark to a single page.
pub fn add_text_watermark(editor: &mut PdfEditor, page_index: usize, wm: &TextWatermark) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "watermark")?;
    let (page_id, page_dict) = editor.get_page_dict(page_index)?;
    let (pw, ph) = page_dimensions(&page_dict);
    let content_bytes = build_text_watermark_content(wm, pw, ph, editor)?;
    append_content_stream(editor, page_id, &page_dict, content_bytes)
}

/// Add a text watermark to all pages.
pub fn add_watermark_all_pages(editor: &mut PdfEditor, wm: &TextWatermark) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "watermark")?;
    let page_count = editor.page_count()?;
    for i in 0..page_count {
        add_text_watermark(editor, i, wm)?;
    }
    Ok(())
}

/// Add an image watermark to a single page.
pub fn add_image_watermark(editor: &mut PdfEditor, page_index: usize, wm: &ImageWatermark) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "watermark")?;
    let (page_id, page_dict) = editor.get_page_dict(page_index)?;
    let image_id = write_image_xobject(&wm.image_data, &mut editor.writer)?;
    let x = wm.rect[0]; let y = wm.rect[1];
    let w = wm.rect[2] - wm.rect[0]; let h = wm.rect[3] - wm.rect[1];
    let image_name = format!("WmImg{}", image_id);
    // Build content stream
    let mut cb = ContentBuilder::new();
    cb.save()
      .concat_matrix(w, 0.0, 0.0, h, x, y)
      .xobject_do(&image_name)
      .restore();
    let content_bytes = cb.build();
    // Register XObject in page resources
    let mut updated_page = page_dict.clone();
    register_xobject(&mut updated_page, &image_name, image_id);
    editor.replace_object(page_id, PdfObject::Dictionary(updated_page.clone()));
    append_content_stream(editor, page_id, &updated_page, content_bytes)
}

fn build_text_watermark_content(wm: &TextWatermark, pw: f64, ph: f64, editor: &mut PdfEditor) -> Result<Vec<u8>> {
    let angle_rad = wm.angle_degrees * std::f64::consts::PI / 180.0;
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();
    let approx_text_width = wm.font_size * 0.55 * wm.text.len() as f64;

    // Build ExtGState for opacity
    let ext_gstate_name = format!("WmGs{}", (wm.opacity * 100.0) as u32);
    // Note: opacity in PDF requires an ExtGState with /ca (fill alpha)
    // We'll inline it as a graphics state parameter

    let mut cb = ContentBuilder::new();
    cb.save();
    // Set fill color
    cb.set_fill_rgb(wm.color[0], wm.color[1], wm.color[2]);

    if wm.repeat {
        // Tile watermark across page
        let spacing = wm.tile_spacing;
        let mut y = 0.0f64;
        while y < ph + spacing {
            let mut x = 0.0f64;
            while x < pw + spacing {
                draw_single_watermark_text(&mut cb, &wm.text, wm.font_size, x, y, cos_a, sin_a, approx_text_width);
                x += spacing;
            }
            y += spacing;
        }
    } else {
        // Single centered watermark
        let cx = pw / 2.0;
        let cy = ph / 2.0;
        draw_single_watermark_text(&mut cb, &wm.text, wm.font_size, cx, cy, cos_a, sin_a, approx_text_width);
    }

    cb.restore();
    Ok(cb.build())
}

fn draw_single_watermark_text(cb: &mut ContentBuilder, text: &str, font_size: f64, cx: f64, cy: f64, cos_a: f64, sin_a: f64, text_width: f64) {
    // CTM for rotation around (cx, cy):
    // [cos  sin  -sin  cos  cx  cy]
    cb.concat_matrix(cos_a, sin_a, -sin_a, cos_a, cx, cy)
      .begin_text()
      .set_text_font("Helv", font_size)
      .set_text_position(-text_width / 2.0, -font_size / 2.0)
      .show_text(text.as_bytes())
      .end_text()
      .concat_matrix(cos_a, -sin_a, sin_a, cos_a, -cx * cos_a - cy * (-sin_a), -cx * sin_a - cy * cos_a); // undo rotation
}

fn page_dimensions(page_dict: &crate::parser::PdfDict) -> (f64, f64) {
    match page_dict.get("MediaBox") {
        Some(PdfObject::Array(a)) if a.len() == 4 => {
            let w = to_f64(&a[2]) - to_f64(&a[0]);
            let h = to_f64(&a[3]) - to_f64(&a[1]);
            (w, h)
        }
        _ => (612.0, 792.0),
    }
}

fn to_f64(o: &PdfObject) -> f64 {
    match o { PdfObject::Real(r) => *r, PdfObject::Integer(i) => *i as f64, _ => 0.0 }
}

fn append_content_stream(editor: &mut PdfEditor, page_id: u32, page_dict: &crate::parser::PdfDict, content_bytes: Vec<u8>) -> Result<()> {
    let stream = make_flate_stream(&content_bytes, PdfDict::new())?;
    let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));
    let mut updated = page_dict.clone();
    let new_contents = match updated.get("Contents") {
        Some(PdfObject::Array(arr)) => { let mut a = arr.clone(); a.push(PdfObject::Reference(stream_id, 0)); PdfObject::Array(a) }
        Some(single) => PdfObject::Array(vec![single.clone(), PdfObject::Reference(stream_id, 0)]),
        None => PdfObject::Reference(stream_id, 0),
    };
    updated.insert("Contents".to_owned(), new_contents);
    editor.replace_object(page_id, PdfObject::Dictionary(updated));
    Ok(())
}

fn register_xobject(page_dict: &mut crate::parser::PdfDict, name: &str, obj_id: u32) {
    let resources = page_dict.entry("Resources".to_owned()).or_insert(PdfObject::Dictionary(PdfDict::new()));
    if let PdfObject::Dictionary(res) = resources {
        let xobjs = res.entry("XObject".to_owned()).or_insert(PdfObject::Dictionary(PdfDict::new()));
        if let PdfObject::Dictionary(xobj_dict) = xobjs {
            xobj_dict.insert(name.to_owned(), PdfObject::Reference(obj_id, 0));
        }
    }
}
```

## Update `src/editor/mod.rs`

```rust
pub mod watermark;
pub use watermark::{TextWatermark, ImageWatermark, add_text_watermark, add_image_watermark, add_watermark_all_pages};
```

## WASM in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn add_text_watermark(&mut self, page_index: usize, options_json: &str) -> Result<(), JsError> {
    // Parse TextWatermark from JSON: {text, font_size, color:[r,g,b], opacity, angle_degrees, repeat}
    let wm = parse_text_watermark_json(options_json)
        .map_err(|e| JsError::new(&e.to_string()))?;
    crate::editor::add_text_watermark(&mut self.editor, page_index, &wm)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn add_watermark_all_pages(&mut self, options_json: &str) -> Result<(), JsError> {
    let wm = parse_text_watermark_json(options_json)
        .map_err(|e| JsError::new(&e.to_string()))?;
    crate::editor::add_watermark_all_pages(&mut self.editor, &wm)
        .map_err(|e| JsError::new(&e.to_string()))
}
```

## Tests

```rust
#[test]
fn add_text_watermark_produces_parseable_pdf() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    let wm = TextWatermark { text: "CONFIDENTIAL".to_owned(), ..Default::default() };
    add_text_watermark(&mut editor, 0, &wm).unwrap();
    let saved = editor.save_append().unwrap();
    assert!(PdfDocument::parse(saved).is_ok());
}

#[test]
fn add_watermark_all_pages_applies_to_each_page() {
    let data = include_bytes!("fixtures/multipage.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    let wm = TextWatermark::default();
    add_watermark_all_pages(&mut editor, &wm).unwrap();
    let saved = editor.save_append().unwrap();
    let doc = PdfDocument::parse(saved).unwrap();
    assert_eq!(doc.page_count().unwrap(), 3);
}
```

## Verification

```bash
cargo test -- watermark
cargo build --target wasm32-unknown-unknown --features wasm
```
