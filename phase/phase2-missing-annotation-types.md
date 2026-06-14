# Phase 2 — Missing Annotation Types + Appearance Streams

**Status:** Complete — 2026-06-14
**Effort:** ~3 weeks
**Tier gate:** Pro

## Context

Current annotation types: Text, Highlight, StrikeOut, Underline, Link, FreeText, Ink, Redact (8 types).
Missing: Stamp, Polygon, Polyline, FileAttachment, Caret, Watermark (6 types).
Also missing: complete appearance stream generation for all types — without `/AP` streams, annotations look broken in Adobe Reader and other viewers.

## Step 1 — Extend `AnnotationType` enum in `src/editor/annotation.rs`

```rust
// Add to existing AnnotationType enum:
Stamp {
    name: String,               // "Approved", "Draft", "Final", "Experimental", "NotApproved",
                                // "Departmental", "Confidential", "Expired", "Sold",
                                // "ForPublicRelease", "NotForPublicRelease", "TopSecret"
    color: [f64; 3],
},
Polygon {
    vertices: Vec<[f64; 2]>,   // list of [x, y] points
    closed: bool,
    stroke_color: [f64; 3],
    fill_color: Option<[f64; 3]>,
    line_width: f64,
},
Polyline {
    vertices: Vec<[f64; 2]>,
    stroke_color: [f64; 3],
    line_width: f64,
},
FileAttachment {
    file_data: Vec<u8>,
    filename: String,
    description: String,
    icon_name: String,          // "PushPin", "Graph", "Paperclip", "Tag"
},
Caret {
    symbol: String,             // "None" or "P" (paragraph)
},
```

## Step 2 — Extend `AnnotationBuilder::build()` for new types

In `src/editor/annotation.rs`, extend the `build()` method match arms:

```rust
AnnotationType::Stamp { name, color } => {
    dict.insert("Subtype".to_owned(), PdfObject::Name("Stamp".to_owned()));
    dict.insert("Name".to_owned(), PdfObject::Name(name.clone()));
    // /AP appearance stream
    let ap_bytes = appearance::stamp_appearance(name, rect, color);
    // store as AP/N stream (caller must write stream to writer separately)
    // ... store in dict
}
AnnotationType::Polygon { vertices, closed, stroke_color, fill_color, line_width } => {
    dict.insert("Subtype".to_owned(), PdfObject::Name("Polygon".to_owned()));
    dict.insert("Vertices".to_owned(), PdfObject::Array(
        vertices.iter().flat_map(|v| [PdfObject::Real(v[0]), PdfObject::Real(v[1])]).collect()
    ));
    dict.insert("C".to_owned(), color_array(stroke_color));
    if let Some(ic) = fill_color {
        dict.insert("IC".to_owned(), color_array(ic));
    }
    dict.insert("BS".to_owned(), border_style_dict(line_width));
}
AnnotationType::Polyline { vertices, stroke_color, line_width } => {
    dict.insert("Subtype".to_owned(), PdfObject::Name("PolyLine".to_owned()));
    dict.insert("Vertices".to_owned(), PdfObject::Array(
        vertices.iter().flat_map(|v| [PdfObject::Real(v[0]), PdfObject::Real(v[1])]).collect()
    ));
    dict.insert("C".to_owned(), color_array(stroke_color));
    dict.insert("BS".to_owned(), border_style_dict(line_width));
}
AnnotationType::FileAttachment { file_data, filename, description, icon_name } => {
    dict.insert("Subtype".to_owned(), PdfObject::Name("FileAttachment".to_owned()));
    dict.insert("FS".to_owned(), /* embedded file stream reference — see below */);
    dict.insert("Name".to_owned(), PdfObject::Name(icon_name.clone()));
    dict.insert("Contents".to_owned(), PdfObject::String(description.as_bytes().to_vec()));
}
AnnotationType::Caret { symbol } => {
    dict.insert("Subtype".to_owned(), PdfObject::Name("Caret".to_owned()));
    dict.insert("Sy".to_owned(), PdfObject::Name(symbol.clone()));
}
```

For `FileAttachment`, build the embedded file stream:
```rust
fn build_embedded_file_stream(data: &[u8], filename: &str, writer: &mut PdfWriter) -> u32 {
    let compressed = encode_flate(data).unwrap_or_else(|_| data.to_vec());
    let mut ef_dict = PdfDict::new();
    ef_dict.insert("Type".to_owned(), PdfObject::Name("EmbeddedFile".to_owned()));
    ef_dict.insert("Length".to_owned(), PdfObject::Integer(compressed.len() as i64));
    ef_dict.insert("Filter".to_owned(), PdfObject::Name("FlateDecode".to_owned()));
    let params = {
        let mut p = PdfDict::new();
        p.insert("Size".to_owned(), PdfObject::Integer(data.len() as i64));
        p
    };
    ef_dict.insert("Params".to_owned(), PdfObject::Dictionary(params));
    let ef_stream = PdfStream { dict: ef_dict, raw_data: compressed };
    let ef_id = writer.add_object(PdfObject::Stream(Box::new(ef_stream)));

    let mut filespec_dict = PdfDict::new();
    filespec_dict.insert("Type".to_owned(), PdfObject::Name("Filespec".to_owned()));
    filespec_dict.insert("F".to_owned(), PdfObject::String(filename.as_bytes().to_vec()));
    filespec_dict.insert("UF".to_owned(), PdfObject::String(filename.as_bytes().to_vec()));
    let mut ef_dict2 = PdfDict::new();
    ef_dict2.insert("F".to_owned(), PdfObject::Reference(ef_id, 0));
    filespec_dict.insert("EF".to_owned(), PdfObject::Dictionary(ef_dict2));
    writer.add_object(PdfObject::Dictionary(filespec_dict))
}
```

## Step 3 — Appearance Streams for All Types in `src/forms/appearance.rs`

```rust
/// Generate appearance stream for a stamp annotation.
pub fn stamp_appearance(name: &str, rect: [f64; 4], color: [f64; 3]) -> Vec<u8> {
    let w = rect[2] - rect[0];
    let h = rect[3] - rect[1];
    let font_size = (h * 0.6).max(8.0).min(24.0);
    let cx = w / 2.0;
    let cy = h / 2.0 - font_size / 2.0;
    let approx_text_width = font_size * 0.55 * name.len() as f64;
    format!(
        "q {} {} {} {} re S \
         BT /Helv {} Tf {} {} Td ({}) Tj ET Q",
        2.0, 2.0, w - 4.0, h - 4.0,
        font_size, cx - approx_text_width / 2.0, cy, name
    ).into_bytes()
}

/// Appearance for FreeText annotation.
pub fn freetext_appearance(text: &str, rect: [f64; 4], font_size: f64, color: [f64; 3]) -> Vec<u8> {
    let h = rect[3] - rect[1];
    format!(
        "q {} {} {} rg BT /Helv {} Tf 2 {} Td ({}) Tj ET Q",
        color[0], color[1], color[2], font_size,
        h - font_size - 2.0,
        escape_pdf_string(text)
    ).into_bytes()
}

/// Appearance for Ink annotation (freehand strokes).
pub fn ink_appearance(ink_list: &[Vec<[f64; 2]>], bbox: [f64; 4], color: [f64; 3]) -> Vec<u8> {
    let mut cb = crate::writer::content_builder::ContentBuilder::new();
    let ox = bbox[0]; let oy = bbox[1];
    cb.save().set_stroke_rgb(color[0], color[1], color[2]).set_line_width(1.5);
    for stroke in ink_list {
        if stroke.len() < 2 { continue; }
        cb.move_to(stroke[0][0] - ox, stroke[0][1] - oy);
        for pt in &stroke[1..] {
            cb.line_to(pt[0] - ox, pt[1] - oy);
        }
        cb.stroke();
    }
    cb.restore().build()
}

/// Appearance for Highlight annotation using QuadPoints.
pub fn highlight_appearance_quad(quad_points: &[[f64; 8]], bbox: [f64; 4], color: [f64; 3]) -> Vec<u8> {
    let ox = bbox[0]; let oy = bbox[1];
    let mut cb = crate::writer::content_builder::ContentBuilder::new();
    cb.save().set_fill_rgb(color[0], color[1], color[2]);
    for quad in quad_points {
        let x = quad[0].min(quad[2]).min(quad[4]).min(quad[6]) - ox;
        let y = quad[1].min(quad[3]).min(quad[5]).min(quad[7]) - oy;
        let w = (quad[0].max(quad[2]).max(quad[4]).max(quad[6]) - ox) - x;
        let h = (quad[1].max(quad[3]).max(quad[5]).max(quad[7]) - oy) - y;
        cb.rect(x, y, w, h).fill();
    }
    cb.restore().build()
}
```

## Step 4 — Wire Appearance Streams into `add_annotation()`

Modify `src/editor/annotation.rs::add_annotation()` to generate and store appearance streams automatically:

```rust
pub fn add_annotation(
    editor: &mut PdfEditor,
    page_index: usize,
    mut annot_dict: HashMap<String, PdfObject>,
) -> Result<()> {
    // After building dict, generate /AP if not already present
    if !annot_dict.contains_key("AP") {
        if let Some(ap_bytes) = generate_appearance(&annot_dict, editor) {
            let bbox = extract_rect(&annot_dict);
            let mut stream_dict = PdfDict::new();
            stream_dict.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
            stream_dict.insert("BBox".to_owned(), PdfObject::Array(vec![
                PdfObject::Real(0.0), PdfObject::Real(0.0),
                PdfObject::Real(bbox[2]-bbox[0]), PdfObject::Real(bbox[3]-bbox[1])
            ]));
            let stream = make_flate_stream(&ap_bytes, stream_dict)?;
            let ap_id = editor.add_object(PdfObject::Stream(Box::new(stream)));
            let mut ap_dict = PdfDict::new();
            ap_dict.insert("N".to_owned(), PdfObject::Reference(ap_id, 0));
            annot_dict.insert("AP".to_owned(), PdfObject::Dictionary(ap_dict));
        }
    }
    // ... existing add_annotation logic
}
```

## WASM in `src/wasm/editor.rs`

The existing `add_annotation(page_index, annot_json)` WASM method already works. Extend the JSON schema to accept the new annotation types. Add WASM convenience methods:
```rust
pub fn add_stamp(&mut self, page_index: usize, name: &str, rect_json: &str, color_json: &str) -> Result<(), JsError>
pub fn add_file_attachment(&mut self, page_index: usize, file_bytes: &[u8], filename: &str, rect_json: &str) -> Result<(), JsError>
```

## Tests in `tests/write_edit.rs`

```rust
#[test] fn add_stamp_annotation_parseable() { /* add Stamp "Draft", save, reparse → /Annots has Stamp */ }
#[test] fn add_polygon_annotation_parseable() { /* add Polygon, save, reparse → /Annots has Polygon */ }
#[test] fn add_file_attachment_parseable() { /* add FileAttachment, save, reparse → /Annots has FileAttachment */ }
#[test] fn annotations_have_ap_streams() { /* add Highlight → saved PDF has /AP in annot dict */ }
```

## Verification

```bash
cargo test -- annotation
cargo build --target wasm32-unknown-unknown --features wasm
```
