# Phase 2 — Soft Masks

**Status:** Complete — 2026-06-16
**Effort:** ~3 weeks
**Scope:** Renderer only — no public API change needed

## Context

Many modern PDFs use soft masks (`/SMask` in extended graphics state) for smooth transparency and compositing. Without soft mask support, content gated behind a soft mask is either fully visible or fully invisible, producing incorrect rendering. This is a pure renderer fix.

Soft masks are specified in ISO 32000-1 §11.6.5.2. Two types:
- **Alpha soft mask** (`/S /Alpha`): the mask XObject's alpha channel determines per-pixel opacity.
- **Luminosity soft mask** (`/S /Luminosity`): the mask XObject's luminosity determines per-pixel opacity.

## Files to Modify

### `src/render/page_renderer.rs`

In the extended graphics state handler (where `/BM`, `/ca`, `/CA`, `/SMask` are applied):

```rust
// Find where ExtGState dict keys are applied — typically in a function like:
// fn apply_ext_gstate(&mut self, dict: &PdfDict, doc: &PdfDocument) {

if let Some(smask_obj) = gstate_dict.get("SMask") {
    match smask_obj {
        PdfObject::Name(n) if n == "None" => {
            // Clear any current soft mask
            self.current_soft_mask = None;
        }
        PdfObject::Dictionary(smask_dict) | PdfObject::Reference(..) => {
            let smask = doc.resolve(smask_obj)?;
            let smask_dict = smask.as_dict().unwrap_or(&PdfDict::new()).clone();
            let subtype = smask_dict.get("S").and_then(|o| if let PdfObject::Name(n) = o { Some(n.as_str()) } else { None });
            let g_ref = smask_dict.get("G"); // XObject form stream reference
            if let Some(g_obj) = g_ref {
                let g_stream = doc.resolve(g_obj)?;
                // Render the mask form XObject into a separate pixmap
                let mask_pixmap = self.render_form_xobject_to_pixmap(&g_stream, doc)?;
                let mask_type = match subtype { Some("Alpha") => SoftMaskType::Alpha, _ => SoftMaskType::Luminosity };
                self.current_soft_mask = Some(SoftMask { pixmap: mask_pixmap, mask_type });
            }
        }
        _ => {}
    }
}
```

### New Types in `src/render/page_renderer.rs`

```rust
enum SoftMaskType { Alpha, Luminosity }

struct SoftMask {
    pixmap: tiny_skia::Pixmap,  // rendered mask image
    mask_type: SoftMaskType,
}
```

Add `current_soft_mask: Option<SoftMask>` field to the renderer struct.

### Applying the Soft Mask When Drawing

When drawing pixels (in `fill_path`, `stroke_path`, `draw_text_span`, `draw_image`):

```rust
// After rasterizing the content into a temporary pixmap:
if let Some(ref mask) = self.current_soft_mask {
    apply_soft_mask(&mut temp_pixmap, &mask.pixmap, mask.mask_type);
}
// Composite temp_pixmap onto the main canvas
self.canvas.draw_pixmap(x, y, temp_pixmap.as_ref(), &tiny_skia::PixmapPaint::default(), transform, None);
```

```rust
fn apply_soft_mask(
    content: &mut tiny_skia::Pixmap,
    mask: &tiny_skia::Pixmap,
    mask_type: SoftMaskType,
) {
    // Iterate all pixels, modulate alpha by mask value
    let content_data = content.data_mut();
    let mask_data = mask.data();
    let len = content_data.len() / 4;
    for i in 0..len {
        let mask_alpha = match mask_type {
            SoftMaskType::Alpha => mask_data[i * 4 + 3], // use mask's alpha channel
            SoftMaskType::Luminosity => {
                // Luminosity = 0.2126*R + 0.7152*G + 0.0722*B
                let r = mask_data[i * 4] as f32;
                let g = mask_data[i * 4 + 1] as f32;
                let b = mask_data[i * 4 + 2] as f32;
                (0.2126 * r + 0.7152 * g + 0.0722 * b) as u8
            }
        };
        // Modulate content pixel alpha by mask_alpha
        let content_alpha = content_data[i * 4 + 3] as u16;
        content_data[i * 4 + 3] = ((content_alpha * mask_alpha as u16) / 255) as u8;
    }
}
```

### Rendering a Form XObject to a Pixmap

Add helper method to the renderer:
```rust
fn render_form_xobject_to_pixmap(
    &mut self,
    form_stream: &crate::parser::PdfStream,
    doc: &crate::parser::PdfDocument,
) -> Result<tiny_skia::Pixmap> {
    // 1. Get form XObject bbox from stream dict (/BBox)
    let bbox = get_bbox_from_dict(&form_stream.dict); // [x1, y1, x2, y2]
    let width = (bbox[2] - bbox[0]).abs().ceil() as u32;
    let height = (bbox[3] - bbox[1]).abs().ceil() as u32;
    let width = width.max(1).min(4096);
    let height = height.max(1).min(4096);

    // 2. Create fresh pixmap
    let mut mask_pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| PdfError::write_error("cannot create mask pixmap"))?;

    // 3. Get form resources from stream dict
    let resources = form_stream.dict.get("Resources")
        .and_then(|r| doc.resolve(r).ok())
        .and_then(|o| o.into_dict())
        .unwrap_or_default();

    // 4. Decode form content stream
    let form_content = form_stream.decode_with_doc(doc)?;

    // 5. Create sub-renderer for the mask pixmap, run interpreter
    let mut sub_renderer = PageRenderer::new_for_mask(mask_pixmap);
    sub_renderer.render_content(&form_content, doc, &resources)?;

    Ok(sub_renderer.into_pixmap())
}
```

This reuses the existing `PageRenderer` and `ContentInterpreter` — the renderer already recurses into form XObjects; this is the same pattern applied to a fresh pixmap.

## Tests

Since soft masks affect visual output, use the existing rendering test pattern in `tests/real_pdf.rs`:

```rust
#[cfg(feature = "render")]
#[test]
fn pdf_with_soft_mask_renders_without_panic() {
    // Use a PDF known to contain soft masks (e.g., Group-3.pdf or any PPTX export)
    let data = include_bytes!("fixtures/Group-3.pdf").to_vec();
    let doc = PdfDocument::parse(data).unwrap();
    let catalog = Catalog::from_document(&doc).unwrap();
    let page_dict = catalog.get_page_dict(&doc, 0).unwrap();
    let page = Page::from_dict(&doc, &page_dict).unwrap();
    // Should not panic and should produce non-zero pixels
    let rgba = render_page(&doc, &page, 1.0).unwrap();
    assert!(!rgba.iter().all(|&b| b == 255)); // not all white
}
```

## Verification

```bash
cargo test --features render -- soft_mask render_pptx
cargo build --target wasm32-unknown-unknown --features wasm,render
```
