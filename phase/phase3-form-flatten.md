# Phase 3 — Form Flatten (Burn Form Fields Into Content)

**Status:** Complete — 2026-06-17
**Effort:** ~3–4 days
**Tier gate:** Pro
**Prerequisites:** phase1-form-filling.md, phase1-annotation-flatten.md

## Context

Form flattening converts interactive AcroForm fields into static page content. After flattening the PDF is no longer fillable — the field values become visual content in the page's content stream. This is needed before printing, archiving, or sharing final documents.

Widget annotations (form fields) appear in the page's `/Annots` array just like regular annotations. The difference is they have `Subtype = Widget` and `/FT` (field type). The appearance stream in `/AP/N` already contains the rendered visual — we just need to embed it into the content stream.

## New Function in `src/forms/filler.rs`

```rust
use crate::editor::PdfEditor;
use crate::parser::{PdfObject, PdfDict};
use crate::writer::content_builder::ContentBuilder;
use crate::writer::streams::make_flate_stream;
use crate::error::Result;

/// Flatten all AcroForm fields on a single page into the content stream.
/// After this call, fields are no longer interactive.
pub fn flatten_form_fields(editor: &mut PdfEditor, page_index: usize) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "flatten_forms")?;
    let (page_id, page_dict) = editor.get_page_dict(page_index)?;

    // Find widget annotations (form fields) in /Annots
    let annots_obj = match page_dict.get("Annots") {
        Some(o) => editor.get_object(match o { PdfObject::Reference(n,_) => *n, _ => { return Ok(()); } }).unwrap_or(o.clone()),
        None => return Ok(()),
    };
    let annots = match annots_obj { PdfObject::Array(a) => a, _ => return Ok(()) };

    let mut widget_ids: Vec<u32> = Vec::new();
    let mut non_widget_annots: Vec<PdfObject> = Vec::new();

    for annot_ref in &annots {
        let annot_id = match annot_ref { PdfObject::Reference(n,_) => *n, _ => { non_widget_annots.push(annot_ref.clone()); continue; } };
        let annot_obj = editor.get_object(annot_id)?;
        let annot_dict = match &annot_obj { PdfObject::Dictionary(d) => d, _ => { non_widget_annots.push(annot_ref.clone()); continue; } };
        let subtype = match annot_dict.get("Subtype") {
            Some(PdfObject::Name(n)) if n == "Widget" => "Widget",
            _ => { non_widget_annots.push(annot_ref.clone()); continue; }
        };
        let _ = subtype;
        widget_ids.push(annot_id);
    }

    if widget_ids.is_empty() { return Ok(()); }

    // For each widget: embed its /AP/N appearance stream into content stream
    let mut content_bytes = Vec::new();
    for widget_id in &widget_ids {
        let widget_obj = editor.get_object(*widget_id)?;
        let widget_dict = match widget_obj { PdfObject::Dictionary(d) => d, _ => continue };

        // Get /Rect for positioning
        let rect = extract_rect(&widget_dict);
        let x = rect[0]; let y = rect[1];
        let w = rect[2] - rect[0]; let h = rect[3] - rect[1];

        // Try to get /AP /N appearance stream
        if let Some(ap_ref) = widget_dict.get("AP")
            .and_then(|ap| if let PdfObject::Dictionary(d) = ap { d.get("N") } else { None }) {

            let ap_stream_obj = editor.get_object(match ap_ref { PdfObject::Reference(n,_) => *n, _ => continue })?;

            if let PdfObject::Stream(_) = &ap_stream_obj {
                // Embed form XObject via `Do` operator
                let xobj_name = format!("WFld{}", widget_id);
                // Add XObject to page resources
                // Write: q {w} 0 0 {h} {x} {y} cm /{name} Do Q
                let segment = format!(
                    "q {} 0 0 {} {} {} cm /{} Do Q\n",
                    w, h, x, y, xobj_name
                );
                content_bytes.extend_from_slice(segment.as_bytes());
                // Register XObject in page resources (done below)
            }
        } else {
            // No appearance stream — generate from field value
            let field_type = widget_dict.get("FT").and_then(|o| if let PdfObject::Name(n) = o { Some(n.as_str()) } else { None }).unwrap_or("");
            let value = widget_dict.get("V").and_then(|o| match o {
                PdfObject::String(b) => Some(String::from_utf8_lossy(b).to_string()),
                PdfObject::Name(n) => Some(n.clone()),
                _ => None,
            }).unwrap_or_default();

            let segment = match field_type {
                "Tx" => {
                    let font_size = (h * 0.7).min(12.0).max(6.0);
                    format!("q BT /Helv {} Tf {} {} Td ({}) Tj ET Q\n",
                        font_size, x + 2.0, y + (h - font_size) / 2.0,
                        escape_pdf_string(&value))
                }
                "Btn" => {
                    let as_state = widget_dict.get("AS").and_then(|o| if let PdfObject::Name(n) = o { Some(n.clone()) } else { None }).unwrap_or_default();
                    let checked = as_state != "Off" && !as_state.is_empty();
                    if checked {
                        // Draw a checkmark
                        let cx = x + w / 2.0; let cy = y + h / 2.0;
                        format!("q 0 0 0 RG 1.5 w {} {} m {} {} l {} {} l S Q\n",
                            cx - w*0.3, cy, cx - w*0.1, cy - h*0.2, cx + w*0.3, cy + h*0.3)
                    } else { String::new() }
                }
                _ => String::new(),
            };
            content_bytes.extend_from_slice(segment.as_bytes());
        }
    }

    // Append content to page
    if !content_bytes.is_empty() {
        let stream = make_flate_stream(&content_bytes, PdfDict::new())?;
        let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

        // Also register any form XObject resources
        let mut updated_page = page_dict.clone();
        // Register XObjects for AP streams
        for widget_id in &widget_ids {
            let widget_obj = editor.get_object(*widget_id).ok();
            if let Some(PdfObject::Dictionary(widget_dict)) = widget_obj {
                if let Some(ap_ref) = widget_dict.get("AP").and_then(|ap| if let PdfObject::Dictionary(d) = ap { d.get("N") } else { None }) {
                    if let PdfObject::Reference(ap_id, _) = ap_ref {
                        let xobj_name = format!("WFld{}", widget_id);
                        register_xobject_in_page(&mut updated_page, &xobj_name, *ap_id);
                    }
                }
            }
        }

        let new_contents = match updated_page.get("Contents") {
            Some(PdfObject::Array(arr)) => { let mut a = arr.clone(); a.push(PdfObject::Reference(stream_id, 0)); PdfObject::Array(a) }
            Some(single) => PdfObject::Array(vec![single.clone(), PdfObject::Reference(stream_id, 0)]),
            None => PdfObject::Reference(stream_id, 0),
        };
        updated_page.insert("Contents".to_owned(), new_contents);

        // Replace /Annots with non-widget annotations only
        if non_widget_annots.is_empty() {
            updated_page.remove("Annots");
        } else {
            updated_page.insert("Annots".to_owned(), PdfObject::Array(non_widget_annots));
        }

        editor.replace_object(page_id, PdfObject::Dictionary(updated_page));
    }

    // Remove widget fields from AcroForm /Fields
    remove_fields_from_acroform(editor, &widget_ids)?;

    Ok(())
}

/// Flatten form fields on all pages.
pub fn flatten_all_form_fields(editor: &mut PdfEditor) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "flatten_forms")?;
    let page_count = editor.page_count()?;
    for i in 0..page_count {
        flatten_form_fields(editor, i)?;
    }
    Ok(())
}

fn extract_rect(dict: &PdfDict) -> [f64; 4] {
    match dict.get("Rect") {
        Some(PdfObject::Array(a)) if a.len() == 4 => {
            let n: Vec<f64> = a.iter().filter_map(|x| match x { PdfObject::Real(r) => Some(*r), PdfObject::Integer(i) => Some(*i as f64), _ => None }).collect();
            if n.len() == 4 { [n[0], n[1], n[2], n[3]] } else { [0.0; 4] }
        }
        _ => [0.0; 4],
    }
}

fn escape_pdf_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)")
}

fn register_xobject_in_page(page_dict: &mut PdfDict, name: &str, obj_id: u32) {
    let resources = page_dict.entry("Resources".to_owned()).or_insert(PdfObject::Dictionary(PdfDict::new()));
    if let PdfObject::Dictionary(res) = resources {
        let xobjs = res.entry("XObject".to_owned()).or_insert(PdfObject::Dictionary(PdfDict::new()));
        if let PdfObject::Dictionary(xobj_dict) = xobjs {
            xobj_dict.insert(name.to_owned(), PdfObject::Reference(obj_id, 0));
        }
    }
}

fn remove_fields_from_acroform(editor: &mut PdfEditor, widget_ids: &[u32]) -> Result<()> {
    // Get AcroForm from root
    let root_id = match editor.doc.trailer.get("Root") { Some(PdfObject::Reference(n,_)) => *n, _ => return Ok(()) };
    let root_obj = editor.get_object(root_id)?;
    let mut root_dict = match root_obj { PdfObject::Dictionary(d) => d, _ => return Ok(()) };
    let acroform_id = match root_dict.get("AcroForm") { Some(PdfObject::Reference(n,_)) => *n, _ => return Ok(()) };
    let acroform_obj = editor.get_object(acroform_id)?;
    let mut acroform_dict = match acroform_obj { PdfObject::Dictionary(d) => d, _ => return Ok(()) };
    let fields = match acroform_dict.get("Fields") { Some(PdfObject::Array(a)) => a.clone(), _ => return Ok(()) };
    let new_fields: Vec<PdfObject> = fields.into_iter().filter(|f| {
        match f { PdfObject::Reference(n,_) => !widget_ids.contains(n), _ => true }
    }).collect();
    acroform_dict.insert("Fields".to_owned(), PdfObject::Array(new_fields));
    editor.replace_object(acroform_id, PdfObject::Dictionary(acroform_dict));
    Ok(())
}
```

## Update `src/forms/mod.rs`

```rust
pub use filler::{set_text_field, set_checkbox, set_combo_or_list, flatten_form_fields, flatten_all_form_fields};
```

## WASM in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn flatten_form_fields(&mut self, page_index: usize) -> Result<(), JsError> {
    crate::forms::flatten_form_fields(&mut self.editor, page_index)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn flatten_all_form_fields(&mut self) -> Result<(), JsError> {
    crate::forms::flatten_all_form_fields(&mut self.editor)
        .map_err(|e| JsError::new(&e.to_string()))
}
```

## Tests

```rust
#[cfg(feature = "forms")]
#[test]
fn flatten_form_removes_widget_annotations() {
    let data = include_bytes!("fixtures/form.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    flatten_all_form_fields(&mut editor).unwrap();
    let saved = editor.save_new().unwrap();
    let doc = PdfDocument::parse(saved).unwrap();
    // Widget annotations should be gone from all pages
    let catalog = Catalog::from_document(&doc).unwrap();
    for i in 0..doc.page_count().unwrap() {
        let page_dict = catalog.get_page_dict(&doc, i).unwrap();
        if let Some(PdfObject::Array(annots)) = page_dict.get("Annots") {
            for a in annots {
                let a_obj = doc.resolve(a).unwrap();
                let a_dict = a_obj.as_dict().unwrap_or(&PdfDict::new()).clone();
                assert_ne!(a_dict.get("Subtype"), Some(&PdfObject::Name("Widget".to_owned())));
            }
        }
    }
}

#[cfg(feature = "forms")]
#[test]
fn flattened_form_is_parseable() {
    let data = include_bytes!("fixtures/form.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    flatten_all_form_fields(&mut editor).unwrap();
    let saved = editor.save_new().unwrap();
    assert!(PdfDocument::parse(saved).is_ok());
}
```

## Verification

```bash
cargo test --features forms -- flatten_form
cargo build --target wasm32-unknown-unknown --features wasm,forms
```
