# Phase 1 — Annotation Flatten

**Status:** Complete — 2026-06-05 (see `.doc/annotation-flatten-2026-06-05.md`)
**Effort:** ~4–5 days
**Tier gate:** Pro

## Context

Annotations in PDF are separate dict objects referenced in a page's `/Annots` array. They are *rendered* by viewer apps but not embedded in the page content stream. Flattening burns them into the content stream so they appear in any viewer and survive printing, and then removes `/Annots`.

The redaction system in `src/editor/redact.rs` already shows the exact pattern:
- `collect_content_bytes()` — gather `/Contents` streams from a page
- Serialize operators with `content/operators.rs::serialize_operations()`
- Compress and store as new stream object
- Update page `/Contents` reference

`src/writer/content_builder.rs` has all drawing primitives needed for each annotation type.

## New Functions in `src/editor/annotation.rs`

```rust
/// Flatten all annotations on a single page into the content stream.
/// After this call, the page has no /Annots and the annotation visuals
/// are part of the page content.
pub fn flatten_annotations(editor: &mut PdfEditor, page_index: usize) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "flatten_annotations")?;
    let (page_id, page_dict) = editor.get_page_dict(page_index)?;

    let annots_obj = match page_dict.get("Annots") {
        Some(o) => editor.get_object(match o { PdfObject::Reference(n,_) => *n, _ => return Ok(()) })
            .unwrap_or(o.clone()),
        None => return Ok(()),  // no annotations
    };
    let annots = match annots_obj {
        PdfObject::Array(a) => a,
        _ => return Ok(()),
    };

    if annots.is_empty() { return Ok(()); }

    // Build drawing ops for each annotation
    let mut cb = ContentBuilder::new();
    cb.save();
    for annot_ref in &annots {
        let annot_id = match annot_ref { PdfObject::Reference(n,_) => *n, _ => continue };
        let annot_obj = editor.get_object(annot_id)?;
        let annot_dict = match &annot_obj { PdfObject::Dictionary(d) => d, _ => continue };
        let subtype = match annot_dict.get("Subtype") {
            Some(PdfObject::Name(n)) => n.as_str(),
            _ => continue,
        };
        flatten_one_annotation(&mut cb, annot_dict, subtype);
    }
    cb.restore();

    // Compress and store as new stream object
    let drawing_bytes = cb.build();
    if !drawing_bytes.is_empty() {
        let stream = crate::writer::streams::make_flate_stream(&drawing_bytes, PdfDict::new())?;
        let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

        // Append to page /Contents
        let mut updated_page = page_dict.clone();
        let new_contents = match updated_page.get("Contents") {
            Some(PdfObject::Array(arr)) => {
                let mut a = arr.clone();
                a.push(PdfObject::Reference(stream_id, 0));
                PdfObject::Array(a)
            }
            Some(single) => PdfObject::Array(vec![single.clone(), PdfObject::Reference(stream_id, 0)]),
            None => PdfObject::Reference(stream_id, 0),
        };
        updated_page.insert("Contents".to_owned(), new_contents);
        updated_page.remove("Annots");
        editor.replace_object(page_id, PdfObject::Dictionary(updated_page));
    }
    Ok(())
}

/// Flatten annotations on all pages.
pub fn flatten_all_annotations(editor: &mut PdfEditor) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "flatten_annotations")?;
    let page_count = editor.page_count()?;
    for i in 0..page_count {
        flatten_annotations(editor, i)?;
    }
    Ok(())
}
```

## Helper Function `flatten_one_annotation`

```rust
fn flatten_one_annotation(cb: &mut ContentBuilder, dict: &PdfDict, subtype: &str) {
    let rect = parse_rect(dict);  // helper: extract [f64;4] from /Rect
    let color = parse_color(dict, "C");  // helper: extract [f64;3] from /C, default [0,0,0]

    match subtype {
        "Highlight" => {
            // Semi-transparent yellow fill over quad points
            let quads = parse_quad_points(dict);
            cb.set_fill_rgb(color[0], color[1], color[2]);
            // Use /ca for opacity would need ExtGState — approximate with solid 30% fill
            for quad in quads {
                // quad = [x1,y1, x2,y2, x3,y3, x4,y4] (bottom-left, bottom-right, top-right, top-left)
                let x = quad[0].min(quad[2]).min(quad[4]).min(quad[6]);
                let y = quad[1].min(quad[3]).min(quad[5]).min(quad[7]);
                let w = quad[0].max(quad[2]).max(quad[4]).max(quad[6]) - x;
                let h = quad[1].max(quad[3]).max(quad[5]).max(quad[7]) - y;
                cb.rect(x, y, w, h).fill();
            }
        }
        "StrikeOut" => {
            let quads = parse_quad_points(dict);
            cb.set_stroke_rgb(color[0], color[1], color[2]).set_line_width(1.0);
            for quad in quads {
                let mid_y = (quad[1] + quad[7]) / 2.0;
                cb.move_to(quad[0], mid_y).line_to(quad[2], mid_y).stroke();
            }
        }
        "Underline" => {
            let quads = parse_quad_points(dict);
            cb.set_stroke_rgb(color[0], color[1], color[2]).set_line_width(0.8);
            for quad in quads {
                cb.move_to(quad[0], quad[1]).line_to(quad[2], quad[3]).stroke();
            }
        }
        "FreeText" => {
            if let Some(PdfObject::String(contents)) = dict.get("Contents") {
                let text = String::from_utf8_lossy(contents);
                let font_size = 10.0_f64;
                cb.set_fill_rgb(0.0, 0.0, 0.0)
                  .begin_text()
                  .set_text_font("Helv", font_size)
                  .set_text_position(rect[0] + 2.0, rect[1] + 2.0)
                  .show_text(text.as_bytes())
                  .end_text();
            }
        }
        "Ink" => {
            if let Some(PdfObject::Array(ink_list)) = dict.get("InkList") {
                cb.set_stroke_rgb(color[0], color[1], color[2]).set_line_width(1.5);
                for stroke_obj in ink_list {
                    if let PdfObject::Array(pts) = stroke_obj {
                        let coords: Vec<f64> = pts.iter().filter_map(|p| match p {
                            PdfObject::Real(r) => Some(*r),
                            PdfObject::Integer(i) => Some(*i as f64),
                            _ => None,
                        }).collect();
                        if coords.len() >= 2 {
                            cb.move_to(coords[0], coords[1]);
                            let mut i = 2;
                            while i + 1 < coords.len() {
                                cb.line_to(coords[i], coords[i+1]);
                                i += 2;
                            }
                            cb.stroke();
                        }
                    }
                }
            }
        }
        // Link, Text (sticky-note icon), Redact, Widget — skip (non-visual or already handled)
        _ => {}
    }
}

fn parse_rect(dict: &PdfDict) -> [f64; 4] {
    match dict.get("Rect") {
        Some(PdfObject::Array(a)) => {
            let n: Vec<f64> = a.iter().filter_map(|x| match x {
                PdfObject::Real(r) => Some(*r), PdfObject::Integer(i) => Some(*i as f64), _ => None
            }).collect();
            if n.len() == 4 { [n[0], n[1], n[2], n[3]] } else { [0.0; 4] }
        }
        _ => [0.0; 4],
    }
}

fn parse_color(dict: &PdfDict, key: &str) -> [f64; 3] {
    match dict.get(key) {
        Some(PdfObject::Array(a)) if a.len() >= 3 => {
            let r = to_f64(&a[0]); let g = to_f64(&a[1]); let b = to_f64(&a[2]);
            [r, g, b]
        }
        _ => [0.0, 0.0, 0.0],
    }
}

fn parse_quad_points(dict: &PdfDict) -> Vec<Vec<f64>> {
    match dict.get("QuadPoints") {
        Some(PdfObject::Array(a)) => {
            let nums: Vec<f64> = a.iter().filter_map(|x| match x {
                PdfObject::Real(r) => Some(*r), PdfObject::Integer(i) => Some(*i as f64), _ => None
            }).collect();
            nums.chunks(8).map(|c| c.to_vec()).collect()
        }
        _ => vec![],
    }
}

fn to_f64(o: &PdfObject) -> f64 {
    match o { PdfObject::Real(r) => *r, PdfObject::Integer(i) => *i as f64, _ => 0.0 }
}
```

## WASM in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn flatten_annotations(&mut self, page_index: usize) -> Result<(), JsError> {
    crate::editor::annotation::flatten_annotations(&mut self.editor, page_index)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn flatten_all_annotations(&mut self) -> Result<(), JsError> {
    crate::editor::annotation::flatten_all_annotations(&mut self.editor)
        .map_err(|e| JsError::new(&e.to_string()))
}
```

## Tests in `tests/write_edit.rs`

```rust
#[test]
fn flatten_highlight_removes_annots_key() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    // Add a highlight annotation first
    let annot = AnnotationBuilder::new(
        AnnotationType::Highlight { color: [1.0,1.0,0.0], quad_points: vec![[100.0,700.0,200.0,700.0,100.0,720.0,200.0,720.0]] },
        [100.0, 700.0, 200.0, 720.0],
    ).build();
    add_annotation(&mut editor, 0, annot).unwrap();
    // Flatten
    flatten_annotations(&mut editor, 0).unwrap();
    let saved = editor.save_append().unwrap();
    // Reparse — /Annots should be absent or empty
    let doc2 = PdfDocument::parse(saved).unwrap();
    let page_dict = Catalog::from_document(&doc2).unwrap().get_page_dict(&doc2, 0).unwrap();
    assert!(!page_dict.contains_key("Annots") || matches!(page_dict.get("Annots"), Some(PdfObject::Array(a)) if a.is_empty()));
}

#[test]
fn flatten_produces_parseable_pdf() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    flatten_all_annotations(&mut editor).unwrap();
    let saved = editor.save_append().unwrap();
    assert!(PdfDocument::parse(saved).is_ok());
}
```

## Verification

```bash
cargo test -- flatten
cargo build --target wasm32-unknown-unknown --features wasm
```
