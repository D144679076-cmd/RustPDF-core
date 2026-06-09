# Phase 2 — FDF / XFDF Import & Export

**Status:** Not started
**Effort:** ~3 weeks
**Tier gate:** Pro

## Context

FDF (Forms Data Format) and XFDF (XML Forms Data Format) are the standard interchange formats for PDF form field data. Both can be imported/exported independently of the full PDF. The existing `src/forms/reader.rs` (Phase 1) and `src/forms/filler.rs` (Phase 1) provide the primitives needed.

## New File `src/forms/fdf.rs`

### FDF Export

FDF format:
```
%FDF-1.2
1 0 obj
<< /FDF << /Fields [
    << /T (field_name) /V (field_value) >>
    ...
] >> >>
endobj
trailer << /Root 1 0 R >>
%%EOF
```

```rust
/// Export all form field values as FDF bytes.
pub fn export_fdf(doc: &PdfDocument) -> Result<Vec<u8>> {
    let fields = crate::forms::read_form_fields(doc)?;
    let mut out = Vec::new();
    out.extend_from_slice(b"%FDF-1.2\n");
    out.extend_from_slice(b"1 0 obj\n");
    out.extend_from_slice(b"<< /FDF << /Fields [\n");
    for field in &fields {
        let v = fdf_value_for_field(field);
        out.extend_from_slice(format!("  << /T ({}) /V {} >>\n",
            escape_pdf_string(&field.full_name), v).as_bytes());
    }
    out.extend_from_slice(b"] >> >>\n");
    out.extend_from_slice(b"endobj\n");
    out.extend_from_slice(b"trailer << /Root 1 0 R >>\n");
    out.extend_from_slice(b"%%EOF\n");
    Ok(out)
}

fn fdf_value_for_field(field: &crate::forms::FormField) -> String {
    use crate::forms::FieldType;
    match field.field_type {
        FieldType::Checkbox => {
            if field.checked { "/Yes".to_owned() } else { "/Off".to_owned() }
        }
        _ => format!("({})", escape_pdf_string(&field.value))
    }
}

fn escape_pdf_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)")
}
```

### FDF Import

```rust
/// Parse FDF bytes and fill matching form fields in the editor.
pub fn import_fdf(editor: &mut crate::editor::PdfEditor, fdf_bytes: &[u8]) -> Result<()> {
    let fields_data = parse_fdf(fdf_bytes)?;
    let existing_fields = crate::forms::read_form_fields(&editor.doc)?;

    for (name, value) in fields_data {
        if let Some(field) = existing_fields.iter().find(|f| f.full_name == name || f.name == name) {
            match field.field_type {
                crate::forms::FieldType::Text => {
                    crate::forms::set_text_field(editor, field, &value)?;
                }
                crate::forms::FieldType::Checkbox => {
                    let checked = value == "Yes" || value == "/Yes";
                    crate::forms::set_checkbox(editor, field, checked)?;
                }
                crate::forms::FieldType::List | crate::forms::FieldType::Combo => {
                    crate::forms::set_combo_or_list(editor, field, &value)?;
                }
                _ => {} // skip signature fields etc.
            }
        }
    }
    Ok(())
}

/// Parse FDF bytes → list of (field_full_name, value) pairs.
fn parse_fdf(data: &[u8]) -> Result<Vec<(String, String)>> {
    let text = std::str::from_utf8(data)
        .map_err(|_| PdfError::invalid_structure("FDF not valid UTF-8"))?;
    // Use the existing PDF lexer/parser: FDF objects are a valid PDF subset
    // 1. Find "obj" → parse dictionary → find /FDF /Fields array
    // 2. For each dict in the array: extract /T (name) and /V (value)
    let bytes = data.to_vec();
    let doc = crate::parser::PdfDocument::parse(bytes)?; // FDF is parseable as PDF
    // Traverse the /FDF /Fields array
    let trailer = &doc.trailer;
    let root = doc.resolve(trailer.get("Root").ok_or_else(|| PdfError::invalid_structure("no root"))?)?;
    let root_dict = root.as_dict().ok_or_else(|| PdfError::invalid_structure("root not dict"))?;
    let fdf_dict = doc.resolve(root_dict.get("FDF").ok_or_else(|| PdfError::invalid_structure("no /FDF"))?)?.into_dict()?;
    let fields_arr = doc.resolve(fdf_dict.get("Fields").ok_or_else(|| PdfError::invalid_structure("no /Fields"))?)?.into_array()?;

    let mut result = Vec::new();
    for field_ref in fields_arr {
        let field_obj = doc.resolve(&field_ref)?;
        let field_dict = field_obj.as_dict().unwrap_or(&crate::parser::PdfDict::new()).clone();
        let name = match field_dict.get("T") {
            Some(crate::parser::PdfObject::String(b)) => String::from_utf8_lossy(b).to_string(),
            Some(crate::parser::PdfObject::Name(n)) => n.clone(),
            _ => continue,
        };
        let value = match field_dict.get("V") {
            Some(crate::parser::PdfObject::String(b)) => String::from_utf8_lossy(b).to_string(),
            Some(crate::parser::PdfObject::Name(n)) => n.clone(),
            _ => String::new(),
        };
        result.push((name, value));
    }
    Ok(result)
}
```

### XFDF Export

XFDF format (XML):
```xml
<?xml version="1.0" encoding="UTF-8"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">
  <fields>
    <field name="field_name">
      <value>field_value</value>
    </field>
  </fields>
</xfdf>
```

```rust
/// Export all form field values as XFDF string.
pub fn export_xfdf(doc: &PdfDocument) -> Result<String> {
    let fields = crate::forms::read_form_fields(doc)?;
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n");
    xml.push_str("  <fields>\n");
    for field in &fields {
        let value = xml_escape(&field.value);
        xml.push_str(&format!(
            "    <field name=\"{}\">\n      <value>{}</value>\n    </field>\n",
            xml_attr_escape(&field.full_name), value
        ));
    }
    xml.push_str("  </fields>\n</xfdf>\n");
    Ok(xml)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
fn xml_attr_escape(s: &str) -> String {
    xml_escape(s).replace('"', "&quot;")
}
```

### XFDF Import

```rust
/// Parse XFDF string and fill form fields.
pub fn import_xfdf(editor: &mut crate::editor::PdfEditor, xfdf_str: &str) -> Result<()> {
    let fields_data = parse_xfdf(xfdf_str)?;
    // Reuse same dispatch as import_fdf
    let existing_fields = crate::forms::read_form_fields(&editor.doc)?;
    for (name, value) in fields_data {
        if let Some(field) = existing_fields.iter().find(|f| f.full_name == name || f.name == name) {
            match field.field_type {
                crate::forms::FieldType::Text => { crate::forms::set_text_field(editor, field, &value)?; }
                crate::forms::FieldType::Checkbox => { crate::forms::set_checkbox(editor, field, value == "Yes")?; }
                crate::forms::FieldType::List | crate::forms::FieldType::Combo => { crate::forms::set_combo_or_list(editor, field, &value)?; }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Hand-rolled minimal XFDF parser: extract <field name="X"><value>Y</value></field> pairs.
fn parse_xfdf(xml: &str) -> Result<Vec<(String, String)>> {
    let mut result = Vec::new();
    // Simple state-machine parser — no external XML dep needed for this simple format
    let mut pos = 0;
    while let Some(field_start) = xml[pos..].find("<field ") {
        let field_start = pos + field_start;
        let name_start = xml[field_start..].find("name=\"")
            .map(|i| field_start + i + 6)
            .ok_or_else(|| PdfError::invalid_structure("XFDF field missing name attribute"))?;
        let name_end = xml[name_start..].find('"')
            .map(|i| name_start + i)
            .ok_or_else(|| PdfError::invalid_structure("XFDF field name not closed"))?;
        let name = xml_unescape(&xml[name_start..name_end]);

        let value_start = xml[field_start..].find("<value>")
            .map(|i| field_start + i + 7);
        let value = if let Some(vs) = value_start {
            let ve = xml[vs..].find("</value>").map(|i| vs + i)
                .ok_or_else(|| PdfError::invalid_structure("XFDF unclosed <value>"))?;
            xml_unescape(&xml[vs..ve])
        } else {
            String::new()
        };

        result.push((name, value));
        pos = field_start + 7; // advance past "<field "
    }
    Ok(result)
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
}
```

## Update `src/forms/mod.rs`

```rust
pub mod fdf;
pub use fdf::{export_fdf, import_fdf, export_xfdf, import_xfdf};
```

## WASM in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn export_fdf(&self) -> Result<Vec<u8>, JsError> {
    crate::forms::export_fdf(&self.editor.doc)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn import_fdf(&mut self, fdf_bytes: &[u8]) -> Result<(), JsError> {
    crate::forms::import_fdf(&mut self.editor, fdf_bytes)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn export_xfdf(&self) -> Result<String, JsError> {
    crate::forms::export_xfdf(&self.editor.doc)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn import_xfdf(&mut self, xfdf_str: &str) -> Result<(), JsError> {
    crate::forms::import_xfdf(&mut self.editor, xfdf_str)
        .map_err(|e| JsError::new(&e.to_string()))
}
```

## Tests

```rust
#[cfg(feature = "forms")]
#[test]
fn fdf_export_round_trips() {
    let data = include_bytes!("fixtures/form.pdf").to_vec();
    let doc = PdfDocument::parse(data.clone()).unwrap();
    let fdf = export_fdf(&doc).unwrap();
    assert!(fdf.starts_with(b"%FDF-1.2"));
    // Re-import into fresh editor
    let mut editor = PdfEditor::open(data).unwrap();
    import_fdf(&mut editor, &fdf).unwrap();
}

#[cfg(feature = "forms")]
#[test]
fn xfdf_export_is_valid_xml() {
    let data = include_bytes!("fixtures/form.pdf").to_vec();
    let doc = PdfDocument::parse(data).unwrap();
    let xfdf = export_xfdf(&doc).unwrap();
    assert!(xfdf.contains("<xfdf"));
    assert!(xfdf.contains("</xfdf>"));
}

#[cfg(feature = "forms")]
#[test]
fn fdf_import_updates_field_values() {
    let data = include_bytes!("fixtures/form.pdf").to_vec();
    let fdf = b"%FDF-1.2\n1 0 obj\n<< /FDF << /Fields [\n << /T (Name) /V (John) >> ] >> >>\nendobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";
    let mut editor = PdfEditor::open(data).unwrap();
    import_fdf(&mut editor, fdf).unwrap();
    let saved = editor.save_append().unwrap();
    let doc2 = PdfDocument::parse(saved).unwrap();
    let fields = read_form_fields(&doc2).unwrap();
    let name_field = fields.iter().find(|f| f.name == "Name").unwrap();
    assert_eq!(name_field.value, "John");
}
```

## Verification

```bash
cargo test --features forms -- fdf
cargo build --target wasm32-unknown-unknown --features wasm,forms
```
