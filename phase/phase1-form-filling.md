# Phase 1 — Form Filling (Read + Write)

**Status:** Complete — 2026-06-06
**Effort:** ~2 weeks
**Tier gate:** Pro

## Context

`src/forms/acroform.rs` only creates new form fields (write-only). There is no code to read existing AcroForm fields from a document or to modify their values. `src/forms/appearance.rs` already has `checkbox_appearance()` and `radio_appearance()` which can be reused. The `/AcroForm` reference is accessible from `src/document/catalog.rs::Catalog::acroform()`.

## Step 1 — New file `src/forms/reader.rs`

```rust
use crate::parser::{PdfDocument, PdfObject, PdfDict};
use crate::error::{PdfError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum FieldType { Text, Checkbox, Radio, List, Combo, Signature, Unknown }

#[derive(Debug, Clone)]
pub struct FormField {
    pub id: u32,                    // object ID of the field widget dict
    pub name: String,               // /T partial name
    pub full_name: String,          // dot-joined: "parent.child"
    pub field_type: FieldType,
    pub value: String,              // /V as UTF-8 string
    pub default_value: String,      // /DV as UTF-8 string
    pub rect: [f64; 4],             // /Rect [x1 y1 x2 y2]
    pub page_index: usize,
    pub options: Vec<String>,       // /Opt for List/Combo
    pub checked: bool,              // for Checkbox: /AS == Yes-state-name
    pub readonly: bool,             // /Ff bit 0
    pub required: bool,             // /Ff bit 1
    pub multiline: bool,            // /Ff bit 12 (Tx only)
    pub max_len: Option<u32>,       // /MaxLen for Tx
}

/// Read all interactive form fields from a document.
pub fn read_form_fields(doc: &PdfDocument) -> Result<Vec<FormField>> {
    // 1. Resolve /Root → /AcroForm
    let trailer = &doc.trailer;
    let root_ref = trailer.get("Root").ok_or_else(|| PdfError::invalid_structure("no /Root"))?;
    let root = doc.resolve(root_ref)?;
    let root_dict = root.as_dict().ok_or_else(|| PdfError::invalid_structure("root not dict"))?;
    let acroform_obj = match root_dict.get("AcroForm") {
        Some(o) => doc.resolve(o)?,
        None => return Ok(vec![]),
    };
    let acroform = acroform_obj.as_dict().ok_or_else(|| PdfError::invalid_structure("AcroForm not dict"))?;
    let fields_obj = match acroform.get("Fields") {
        Some(o) => doc.resolve(o)?,
        None => return Ok(vec![]),
    };
    let fields_arr = match fields_obj {
        PdfObject::Array(a) => a,
        _ => return Err(PdfError::invalid_structure("/Fields not array")),
    };

    // Build page-to-index lookup
    let page_count = doc.page_count()?;
    let mut page_refs: Vec<PdfObject> = Vec::new();
    for i in 0..page_count {
        if let Some(r) = doc.cached_page_ref(i) { page_refs.push(r); }
    }

    let mut result = Vec::new();
    for field_ref in fields_arr {
        collect_fields(doc, &field_ref, "", &page_refs, &mut result)?;
    }
    Ok(result)
}

fn collect_fields(
    doc: &PdfDocument,
    field_ref: &PdfObject,
    parent_name: &str,
    page_refs: &[PdfObject],
    out: &mut Vec<FormField>,
) -> Result<()> {
    let obj = doc.resolve(field_ref)?;
    let id = match field_ref { PdfObject::Reference(n, _) => *n, _ => 0 };
    let dict = obj.as_dict().ok_or_else(|| PdfError::invalid_structure("field not dict"))?;

    let partial_name = dict.get("T")
        .and_then(|o| if let PdfObject::String(b) = o { Some(String::from_utf8_lossy(b).to_string()) } else { None })
        .unwrap_or_default();
    let full_name = if parent_name.is_empty() {
        partial_name.clone()
    } else {
        format!("{}.{}", parent_name, partial_name)
    };

    // If /Kids exists and no /FT → non-terminal node, recurse
    if let Some(PdfObject::Array(kids)) = dict.get("Kids") {
        let has_ft = dict.contains_key("FT");
        if !has_ft {
            for kid in kids.clone() {
                collect_fields(doc, &kid, &full_name, page_refs, out)?;
            }
            return Ok(());
        }
    }

    // Leaf field
    let ft_name = dict.get("FT").and_then(|o| if let PdfObject::Name(n) = o { Some(n.as_str()) } else { None });
    let field_type = match ft_name {
        Some("Tx") => FieldType::Text,
        Some("Btn") => {
            let ff = dict.get("Ff").and_then(|o| if let PdfObject::Integer(i) = o { Some(*i) } else { None }).unwrap_or(0);
            if ff & (1 << 15) != 0 { FieldType::Radio } else { FieldType::Checkbox }
        }
        Some("Ch") => {
            let ff = dict.get("Ff").and_then(|o| if let PdfObject::Integer(i) = o { Some(*i) } else { None }).unwrap_or(0);
            if ff & (1 << 17) != 0 { FieldType::Combo } else { FieldType::List }
        }
        Some("Sig") => FieldType::Signature,
        _ => FieldType::Unknown,
    };

    let value = pdf_obj_to_string(dict.get("V")).unwrap_or_default();
    let default_value = pdf_obj_to_string(dict.get("DV")).unwrap_or_default();

    let rect = dict.get("Rect")
        .and_then(|o| if let PdfObject::Array(a) = o {
            let nums: Vec<f64> = a.iter().filter_map(|x| if let PdfObject::Real(r) = x { Some(*r) } else if let PdfObject::Integer(i) = x { Some(*i as f64) } else { None }).collect();
            if nums.len() == 4 { Some([nums[0], nums[1], nums[2], nums[3]]) } else { None }
        } else { None })
        .unwrap_or([0.0, 0.0, 0.0, 0.0]);

    let page_index = dict.get("P")
        .and_then(|p| page_refs.iter().position(|r| objects_equal(r, p)))
        .unwrap_or(0);

    let ff = dict.get("Ff").and_then(|o| if let PdfObject::Integer(i) = o { Some(*i) } else { None }).unwrap_or(0);
    let readonly = ff & 1 != 0;
    let required = ff & 2 != 0;
    let multiline = ff & (1 << 12) != 0;
    let max_len = dict.get("MaxLen").and_then(|o| if let PdfObject::Integer(i) = o { Some(*i as u32) } else { None });

    let as_state = dict.get("AS").and_then(|o| if let PdfObject::Name(n) = o { Some(n.clone()) } else { None }).unwrap_or_default();
    let checked = as_state != "Off" && !as_state.is_empty();

    let options = if let Some(PdfObject::Array(opt)) = dict.get("Opt") {
        opt.iter().filter_map(|o| pdf_obj_to_string(Some(o))).collect()
    } else { vec![] };

    out.push(FormField { id, name: partial_name, full_name, field_type, value, default_value, rect, page_index, options, checked, readonly, required, multiline, max_len });
    Ok(())
}

fn pdf_obj_to_string(obj: Option<&PdfObject>) -> Option<String> {
    match obj? {
        PdfObject::String(b) => Some(String::from_utf8_lossy(b).to_string()),
        PdfObject::Name(n) => Some(n.clone()),
        _ => None,
    }
}

fn objects_equal(a: &PdfObject, b: &PdfObject) -> bool {
    match (a, b) {
        (PdfObject::Reference(an, ag), PdfObject::Reference(bn, bg)) => an == bn && ag == bg,
        _ => false,
    }
}
```

## Step 2 — New file `src/forms/filler.rs`

```rust
use crate::editor::PdfEditor;
use crate::parser::{PdfObject, PdfDict};
use crate::writer::streams::make_flate_stream;
use crate::error::Result;
use super::reader::FormField;
use super::appearance;

/// Update a text field's value and regenerate its appearance stream.
pub fn set_text_field(editor: &mut PdfEditor, field: &FormField, value: &str) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "form_fill")?;
    let mut field_dict = match editor.get_object(field.id)? {
        crate::parser::PdfObject::Dictionary(d) => d,
        _ => return Err(crate::error::PdfError::invalid_structure("field not dict")),
    };
    // Update /V
    field_dict.insert("V".to_owned(), PdfObject::String(value.as_bytes().to_vec()));
    // Generate appearance stream
    let ap_bytes = appearance::text_field_appearance(value, field.rect, field.max_len);
    let ap_stream = make_flate_stream(&ap_bytes, {
        let mut d = PdfDict::new();
        d.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
        d.insert("BBox".to_owned(), PdfObject::Array(vec![
            PdfObject::Real(0.0), PdfObject::Real(0.0),
            PdfObject::Real(field.rect[2] - field.rect[0]),
            PdfObject::Real(field.rect[3] - field.rect[1]),
        ]));
        d
    })?;
    let ap_id = editor.add_object(PdfObject::Stream(Box::new(ap_stream)));
    let mut n_dict = PdfDict::new();
    n_dict.insert("N".to_owned(), PdfObject::Reference(ap_id, 0));
    field_dict.insert("AP".to_owned(), PdfObject::Dictionary(n_dict));
    editor.replace_object(field.id, PdfObject::Dictionary(field_dict));
    Ok(())
}

/// Update a checkbox field value and appearance.
pub fn set_checkbox(editor: &mut PdfEditor, field: &FormField, checked: bool) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "form_fill")?;
    let mut field_dict = match editor.get_object(field.id)? {
        PdfObject::Dictionary(d) => d,
        _ => return Err(crate::error::PdfError::invalid_structure("field not dict")),
    };
    let state = if checked { "Yes" } else { "Off" };
    field_dict.insert("V".to_owned(), PdfObject::Name(state.to_owned()));
    field_dict.insert("AS".to_owned(), PdfObject::Name(state.to_owned()));
    // Regenerate appearance
    let ap_yes = appearance::checkbox_appearance(true);
    let ap_off = appearance::checkbox_appearance(false);
    let bbox = vec![PdfObject::Real(0.0), PdfObject::Real(0.0),
        PdfObject::Real(field.rect[2]-field.rect[0]), PdfObject::Real(field.rect[3]-field.rect[1])];
    let mk_stream = |bytes: Vec<u8>| -> Result<u32> {
        let mut d = PdfDict::new();
        d.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
        d.insert("BBox".to_owned(), PdfObject::Array(bbox.clone()));
        let s = make_flate_stream(&bytes, d)?;
        Ok(editor.add_object(PdfObject::Stream(Box::new(s))))
    };
    let yes_id = mk_stream(ap_yes)?;
    let off_id = mk_stream(ap_off)?;
    let mut n_dict = PdfDict::new();
    n_dict.insert("Yes".to_owned(), PdfObject::Reference(yes_id, 0));
    n_dict.insert("Off".to_owned(), PdfObject::Reference(off_id, 0));
    let mut ap_dict = PdfDict::new();
    ap_dict.insert("N".to_owned(), PdfObject::Dictionary(n_dict));
    field_dict.insert("AP".to_owned(), PdfObject::Dictionary(ap_dict));
    editor.replace_object(field.id, PdfObject::Dictionary(field_dict));
    Ok(())
}

/// Update a combo or list field selection.
pub fn set_combo_or_list(editor: &mut PdfEditor, field: &FormField, selected_value: &str) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "form_fill")?;
    let mut field_dict = match editor.get_object(field.id)? {
        PdfObject::Dictionary(d) => d,
        _ => return Err(crate::error::PdfError::invalid_structure("field not dict")),
    };
    field_dict.insert("V".to_owned(), PdfObject::String(selected_value.as_bytes().to_vec()));
    if let Some(idx) = field.options.iter().position(|o| o == selected_value) {
        field_dict.insert("I".to_owned(), PdfObject::Array(vec![PdfObject::Integer(idx as i64)]));
    }
    editor.replace_object(field.id, PdfObject::Dictionary(field_dict));
    Ok(())
}
```

## Step 3 — Extend `src/forms/appearance.rs`

Add `text_field_appearance()`:
```rust
pub fn text_field_appearance(value: &str, rect: [f64; 4], max_len: Option<u32>) -> Vec<u8> {
    let w = rect[2] - rect[0];
    let h = rect[3] - rect[1];
    let font_size = (h * 0.7).min(12.0).max(6.0);
    let padding = 2.0;
    let display = if let Some(max) = max_len {
        value.chars().take(max as usize).collect::<String>()
    } else { value.to_owned() };
    // Simple single-line text appearance
    format!(
        "q BT /Helv {} Tf {} {} Td ({}) Tj ET Q",
        font_size, padding, (h - font_size) / 2.0,
        escape_pdf_string(&display)
    ).into_bytes()
}

fn escape_pdf_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)")
}
```

## Step 4 — Update `src/forms/mod.rs`

```rust
pub mod reader;
pub mod filler;
pub use reader::{FormField, FieldType, read_form_fields};
pub use filler::{set_text_field, set_checkbox, set_combo_or_list};
```

## Step 5 — WASM in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn get_form_fields(&self) -> Result<String, JsError> {
    let fields = crate::forms::read_form_fields(&self.editor.doc)
        .map_err(|e| JsError::new(&e.to_string()))?;
    // Serialize to JSON array manually (same pattern as existing metadata JSON)
    let mut json = String::from("[");
    for (i, f) in fields.iter().enumerate() {
        if i > 0 { json.push(','); }
        json.push_str(&format!(
            r#"{{"id":{},"name":{},"full_name":{},"field_type":{},"value":{},"checked":{},"readonly":{},"required":{},"rect":[{},{},{},{}],"options":[{}]}}"#,
            f.id, jstr(&f.name), jstr(&f.full_name),
            jstr(&format!("{:?}", f.field_type)),
            jstr(&f.value), f.checked, f.readonly, f.required,
            f.rect[0], f.rect[1], f.rect[2], f.rect[3],
            f.options.iter().map(|o| jstr(o)).collect::<Vec<_>>().join(",")
        ));
    }
    json.push(']');
    Ok(json)
}

#[wasm_bindgen]
pub fn set_field_value(&mut self, field_name: &str, value: &str) -> Result<(), JsError> {
    let fields = crate::forms::read_form_fields(&self.editor.doc)
        .map_err(|e| JsError::new(&e.to_string()))?;
    let field = fields.iter().find(|f| f.full_name == field_name || f.name == field_name)
        .ok_or_else(|| JsError::new(&format!("field '{}' not found", field_name)))?;
    match field.field_type {
        crate::forms::FieldType::Text => crate::forms::set_text_field(&mut self.editor, field, value),
        crate::forms::FieldType::Checkbox => crate::forms::set_checkbox(&mut self.editor, field, value == "true" || value == "Yes"),
        crate::forms::FieldType::List | crate::forms::FieldType::Combo => crate::forms::set_combo_or_list(&mut self.editor, field, value),
        _ => Err(crate::error::PdfError::invalid_structure("unsupported field type for set_field_value")),
    }.map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn get_field_value(&self, field_name: &str) -> Result<String, JsError> {
    let fields = crate::forms::read_form_fields(&self.editor.doc)
        .map_err(|e| JsError::new(&e.to_string()))?;
    fields.iter()
        .find(|f| f.full_name == field_name || f.name == field_name)
        .map(|f| f.value.clone())
        .ok_or_else(|| JsError::new(&format!("field '{}' not found", field_name)))
}
```

Where `jstr()` is the JSON string escaping helper already used elsewhere in `wasm/document.rs`.

## Test Fixture

Add `tests/fixtures/form.pdf` — a simple AcroForm with at least one text field and one checkbox. Create with:
```python
# Python one-liner using fpdf2:
# pip install fpdf2
from fpdf import FPDF
# Or use: pdftk form_template.pdf fill_form data.fdf output form.pdf
```
Or use an existing form PDF from the web.

## Tests in `tests/write_edit.rs`

```rust
#[cfg(feature = "forms")]
#[test]
fn form_fields_read_from_existing_pdf() {
    let data = include_bytes!("fixtures/form.pdf").to_vec();
    let doc = PdfDocument::parse(data).unwrap();
    let fields = pdf_core::forms::read_form_fields(&doc).unwrap();
    assert!(!fields.is_empty());
}

#[cfg(feature = "forms")]
#[test]
fn form_set_text_field_round_trips() {
    let data = include_bytes!("fixtures/form.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    let fields = pdf_core::forms::read_form_fields(&editor.doc).unwrap();
    let text_field = fields.iter().find(|f| f.field_type == pdf_core::forms::FieldType::Text).unwrap();
    pdf_core::forms::set_text_field(&mut editor, text_field, "Hello World").unwrap();
    let saved = editor.save_append().unwrap();
    let doc2 = PdfDocument::parse(saved).unwrap();
    let fields2 = pdf_core::forms::read_form_fields(&doc2).unwrap();
    let updated = fields2.iter().find(|f| f.full_name == text_field.full_name).unwrap();
    assert_eq!(updated.value, "Hello World");
}
```

## Verification

```bash
cargo test --features forms -- form
cargo build --target wasm32-unknown-unknown --features wasm,forms
```
