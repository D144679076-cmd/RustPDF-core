# Phase 2 — PDF/A Compliance (1b, 2b, 3b)

**Status:** Not started
**Effort:** ~2–3 months
**Tier gate:** Enterprise

## Context

PDF/A is the ISO standard for long-term archival of electronic documents. Three common levels:
- **PDF/A-1b** (ISO 19005-1): strictest; no transparency, no JavaScript, all fonts embedded, no encryption, sRGB ICC profile required, XMP metadata required.
- **PDF/A-2b** (ISO 19005-2): based on PDF 1.7; allows transparency (with ICC constraints), JPEG2000, optional content.
- **PDF/A-3b** (ISO 19005-3): same as 2b but allows embedding any file type.

## New Module Structure: `src/compliance/`

```
src/compliance/
  mod.rs
  pdfa.rs      — validation and conversion
  xmp.rs       — XMP metadata builder
  icc.rs       — embedded sRGB ICC profile
```

## `src/compliance/icc.rs`

```rust
/// Returns a minimal sRGB ICC profile as bytes.
/// This is a pre-compiled static byte array (embed at compile time).
pub fn srgb_icc_profile() -> &'static [u8] {
    include_bytes!("../../assets/sRGB_IEC61966-2-1.icc")
}
```

Download `sRGB_IEC61966-2-1.icc` from ICC (public domain, ~3KB) and place in `pdf-editor-rust-core/assets/`.

## `src/compliance/xmp.rs`

```rust
/// Build a minimal XMP metadata XML string for PDF/A conformance.
pub fn build_pdfa_xmp(
    title: Option<&str>,
    author: Option<&str>,
    part: u8,        // 1, 2, or 3
    conformance: char, // 'B' or 'U'
) -> String {
    format!(r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
        xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
      <dc:title><rdf:Alt><rdf:li xml:lang="x-default">{}</rdf:li></rdf:Alt></dc:title>
      <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>
      <pdfaid:part>{}</pdfaid:part>
      <pdfaid:conformance>{}</pdfaid:conformance>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        title.unwrap_or(""),
        author.unwrap_or(""),
        part, conformance
    )
}
```

## `src/compliance/pdfa.rs`

### Violation Type

```rust
#[derive(Debug, Clone)]
pub struct PdfAViolation {
    pub rule: String,            // e.g. "6.2.2" (font embedding rule)
    pub description: String,
    pub obj_id: Option<u32>,     // which object violated the rule
}
```

### PDF/A-1b Validation

```rust
pub fn validate_pdfa_1b(doc: &PdfDocument) -> Result<Vec<PdfAViolation>> {
    let mut violations = Vec::new();
    check_encryption(doc, &mut violations)?;
    check_font_embedding(doc, &mut violations)?;
    check_no_javascript(doc, &mut violations)?;
    check_output_intents(doc, &mut violations)?;
    check_xmp_metadata(doc, 1, &mut violations)?;
    check_no_transparency(doc, &mut violations)?;
    check_no_external_streams(doc, &mut violations)?;
    Ok(violations)
}

fn check_encryption(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    if doc.trailer.contains_key("Encrypt") {
        v.push(PdfAViolation {
            rule: "6.1.1".to_owned(),
            description: "PDF/A-1b does not permit encryption".to_owned(),
            obj_id: None,
        });
    }
    Ok(())
}

fn check_font_embedding(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    // Walk all pages → /Resources /Font → for each font dict:
    //   check /FontDescriptor has /FontFile, /FontFile2, or /FontFile3
    // Standard 14 fonts (Helvetica, Times, Courier, etc.) are exempt
    let standard_fonts = ["Helvetica", "Times-Roman", "Courier", /* ... all 14 */];
    let page_count = doc.page_count()?;
    let catalog = crate::document::Catalog::from_document(doc)?;
    for i in 0..page_count {
        let page_dict = catalog.get_page_dict(doc, i)?;
        if let Some(resources_obj) = page_dict.get("Resources") {
            let resources = doc.resolve(resources_obj)?;
            if let Some(font_dict_obj) = resources.as_dict().and_then(|d| d.get("Font")) {
                let font_dict = doc.resolve(font_dict_obj)?.into_dict().unwrap_or_default();
                for (font_name, font_ref) in &font_dict {
                    let font = doc.resolve(font_ref)?;
                    let font_d = font.as_dict().unwrap_or(&PdfDict::new()).clone();
                    let base_font = match font_d.get("BaseFont") {
                        Some(PdfObject::Name(n)) => n.as_str(),
                        _ => "",
                    };
                    if standard_fonts.contains(&base_font) { continue; }
                    let has_descriptor = font_d.contains_key("FontDescriptor");
                    if !has_descriptor {
                        v.push(PdfAViolation {
                            rule: "6.2.2".to_owned(),
                            description: format!("Font '{}' on page {} is not embedded", font_name, i),
                            obj_id: match font_ref { PdfObject::Reference(n,_) => Some(*n), _ => None },
                        });
                    }
                    // Also check FontDescriptor has /FontFile, /FontFile2, or /FontFile3
                    if let Some(PdfObject::Reference(desc_id, _)) = font_d.get("FontDescriptor") {
                        let desc = doc.get_object(*desc_id)?;
                        let desc_dict = desc.as_dict().unwrap_or(&PdfDict::new()).clone();
                        let has_file = desc_dict.contains_key("FontFile") || desc_dict.contains_key("FontFile2") || desc_dict.contains_key("FontFile3");
                        if !has_file {
                            v.push(PdfAViolation {
                                rule: "6.2.2".to_owned(),
                                description: format!("Font '{}' FontDescriptor missing FontFile", font_name),
                                obj_id: Some(*desc_id),
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_no_javascript(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    // Check /Names /JavaScript in catalog
    let trailer = &doc.trailer;
    let root = doc.resolve(trailer.get("Root").unwrap())?;
    let root_dict = root.as_dict().unwrap_or(&PdfDict::new()).clone();
    if let Some(names_ref) = root_dict.get("Names") {
        let names = doc.resolve(names_ref)?.into_dict().unwrap_or_default();
        if names.contains_key("JavaScript") {
            v.push(PdfAViolation { rule: "6.6.1".to_owned(), description: "JavaScript is not permitted in PDF/A-1b".to_owned(), obj_id: None });
        }
    }
    Ok(())
}

fn check_output_intents(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    // /Root /OutputIntents must be present with at least one ICC-based entry
    let root = doc.resolve(doc.trailer.get("Root").unwrap())?;
    let root_dict = root.as_dict().unwrap_or(&PdfDict::new()).clone();
    if !root_dict.contains_key("OutputIntents") {
        v.push(PdfAViolation { rule: "6.2.3".to_owned(), description: "/OutputIntents with ICC profile required".to_owned(), obj_id: None });
    }
    Ok(())
}

fn check_xmp_metadata(doc: &PdfDocument, part: u8, v: &mut Vec<PdfAViolation>) -> Result<()> {
    let root = doc.resolve(doc.trailer.get("Root").unwrap())?;
    let root_dict = root.as_dict().unwrap_or(&PdfDict::new()).clone();
    if !root_dict.contains_key("Metadata") {
        v.push(PdfAViolation { rule: "6.7.2".to_owned(), description: "XMP metadata stream (/Metadata) required in document catalog".to_owned(), obj_id: None });
        return Ok(());
    }
    // Check pdfaid:part and pdfaid:conformance are present in XMP
    // (Parse the XMP stream and look for these elements)
    // ...
    Ok(())
}

fn check_no_transparency(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    // Check all ExtGState dicts for /BM != Normal, /ca != 1.0, /CA != 1.0
    // Walk all pages → /Resources /ExtGState → for each entry
    // ... (omitted for brevity)
    Ok(())
}

fn check_no_external_streams(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    // Walk all stream objects: if stream dict has /F (external file stream), it's a violation
    // ...
    Ok(())
}
```

### PDF/A-1b Conversion

```rust
pub fn convert_to_pdfa_1b(editor: &mut PdfEditor) -> Result<()> {
    crate::license::require(crate::license::Tier::Enterprise, "pdfa")?;
    // 1. Add sRGB ICC profile as /OutputIntents
    add_output_intents(editor)?;
    // 2. Add/update XMP metadata with pdfaid tags
    add_xmp_metadata(editor, 1, 'B')?;
    // 3. Remove JavaScript
    remove_javascript(editor)?;
    // 4. Embed any non-embedded fonts (complex — see note below)
    // Note: full font embedding requires the TTF/OTF font data for each non-embedded font.
    // For non-embedded standard-14 fonts: either embed Helvetica/Times/Courier from bundled data,
    // or flag as a violation and skip.
    embed_missing_fonts(editor)?;
    Ok(())
}

fn add_output_intents(editor: &mut PdfEditor) -> Result<()> {
    let icc_data = crate::compliance::icc::srgb_icc_profile();
    let mut icc_dict = PdfDict::new();
    icc_dict.insert("N".to_owned(), PdfObject::Integer(3)); // 3 components (RGB)
    let icc_stream = crate::writer::streams::make_flate_stream(icc_data, icc_dict)?;
    let icc_id = editor.add_object(PdfObject::Stream(Box::new(icc_stream)));

    let mut intent_dict = PdfDict::new();
    intent_dict.insert("Type".to_owned(), PdfObject::Name("OutputIntent".to_owned()));
    intent_dict.insert("S".to_owned(), PdfObject::Name("GTS_PDFA1".to_owned()));
    intent_dict.insert("OutputConditionIdentifier".to_owned(), PdfObject::String(b"sRGB".to_vec()));
    intent_dict.insert("DestOutputProfile".to_owned(), PdfObject::Reference(icc_id, 0));
    let intent_id = editor.add_object(PdfObject::Dictionary(intent_dict));

    // Add to /Root /OutputIntents
    let (root_id, mut root_dict) = get_root(editor)?;
    root_dict.insert("OutputIntents".to_owned(), PdfObject::Array(vec![PdfObject::Reference(intent_id, 0)]));
    editor.replace_object(root_id, PdfObject::Dictionary(root_dict));
    Ok(())
}

fn add_xmp_metadata(editor: &mut PdfEditor, part: u8, conformance: char) -> Result<()> {
    let (root_id, mut root_dict) = get_root(editor)?;
    let info = crate::editor::MetadataFields { /* read existing metadata */ ..Default::default() };
    let xmp_str = crate::compliance::xmp::build_pdfa_xmp(
        info.title.as_deref(), info.author.as_deref(), part, conformance
    );
    let mut xmp_dict = PdfDict::new();
    xmp_dict.insert("Type".to_owned(), PdfObject::Name("Metadata".to_owned()));
    xmp_dict.insert("Subtype".to_owned(), PdfObject::Name("XML".to_owned()));
    // XMP must NOT be compressed per PDF/A spec
    let xmp_stream = crate::writer::streams::make_raw_stream(xmp_str.into_bytes(), xmp_dict);
    let xmp_id = editor.add_object(PdfObject::Stream(Box::new(xmp_stream)));
    root_dict.insert("Metadata".to_owned(), PdfObject::Reference(xmp_id, 0));
    editor.replace_object(root_id, PdfObject::Dictionary(root_dict));
    Ok(())
}
```

### PDF/A-2b and 3b

```rust
pub fn validate_pdfa_2b(doc: &PdfDocument) -> Result<Vec<PdfAViolation>> {
    let mut v = Vec::new();
    // PDF/A-2b relaxes: allows transparency (with ICC), JPEG2000, optional content
    // Shares most checks with 1b:
    check_font_embedding(doc, &mut v)?;
    check_no_javascript(doc, &mut v)?;
    check_output_intents(doc, &mut v)?;
    check_xmp_metadata(doc, 2, &mut v)?;
    // PDF/A-2b specific: check /Encryption absent (same as 1b)
    check_encryption(doc, &mut v)?;
    Ok(v)
}

pub fn validate_pdfa_3b(doc: &PdfDocument) -> Result<Vec<PdfAViolation>> {
    // Same as 2b but allows /AF (associated files) — no additional restrictions
    validate_pdfa_2b(doc)
}

pub fn convert_to_pdfa_2b(editor: &mut PdfEditor) -> Result<()> {
    crate::license::require(crate::license::Tier::Enterprise, "pdfa")?;
    add_output_intents(editor)?;
    add_xmp_metadata(editor, 2, 'B')?;
    remove_javascript(editor)?;
    Ok(())
}

pub fn convert_to_pdfa_3b(editor: &mut PdfEditor) -> Result<()> {
    // PDF/A-3b same as 2b for conversion
    convert_to_pdfa_2b(editor)
}
```

## Update `src/compliance/mod.rs`

```rust
pub mod pdfa;
pub mod xmp;
pub mod icc;
pub use pdfa::{PdfAViolation, validate_pdfa_1b, validate_pdfa_2b, validate_pdfa_3b, convert_to_pdfa_1b, convert_to_pdfa_2b, convert_to_pdfa_3b};
```

## Update `src/lib.rs`

```rust
pub mod compliance;
```

## WASM

```rust
#[wasm_bindgen]
pub fn validate_pdfa(&self, level: &str) -> Result<String, JsError> {
    // level: "1b", "2b", "3b"
    let violations = match level {
        "1b" => crate::compliance::validate_pdfa_1b(&self.doc),
        "2b" => crate::compliance::validate_pdfa_2b(&self.doc),
        "3b" => crate::compliance::validate_pdfa_3b(&self.doc),
        _ => return Err(JsError::new("unknown PDF/A level")),
    }.map_err(|e| JsError::new(&e.to_string()))?;
    // Serialize violations as JSON
    // ...
    Ok(json)
}

#[wasm_bindgen]
pub fn convert_to_pdfa(&mut self, level: &str) -> Result<(), JsError> {
    match level {
        "1b" => crate::compliance::convert_to_pdfa_1b(&mut self.editor),
        "2b" => crate::compliance::convert_to_pdfa_2b(&mut self.editor),
        "3b" => crate::compliance::convert_to_pdfa_3b(&mut self.editor),
        _ => return Err(JsError::new("unknown PDF/A level")),
    }.map_err(|e| JsError::new(&e.to_string()))
}
```

## Tests

```rust
#[test]
fn minimal_pdf_fails_pdfa_1b_no_output_intents() {
    let doc = PdfDocument::parse(include_bytes!("fixtures/minimal.pdf").to_vec()).unwrap();
    let violations = validate_pdfa_1b(&doc).unwrap();
    assert!(violations.iter().any(|v| v.rule == "6.2.3"));
}

#[test]
fn convert_to_pdfa_1b_adds_output_intents() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    convert_to_pdfa_1b(&mut editor).unwrap();
    let saved = editor.save_new().unwrap();
    let doc2 = PdfDocument::parse(saved).unwrap();
    let root = doc2.resolve(doc2.trailer.get("Root").unwrap()).unwrap();
    let root_dict = root.as_dict().unwrap().clone();
    assert!(root_dict.contains_key("OutputIntents"));
    assert!(root_dict.contains_key("Metadata"));
}
```

## Verification

```bash
cargo test -- pdfa
cargo build --target wasm32-unknown-unknown --features wasm
```
