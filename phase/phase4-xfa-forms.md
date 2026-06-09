# Phase 4 — XFA Dynamic Forms

**Status:** Not started
**Effort:** ~6–12 months
**Tier gate:** Enterprise

## Context

XFA (XML Forms Architecture) is an Adobe proprietary format embedded in some PDFs (usually from LiveCycle Designer). XFA forms are stored as an XML stream in the `/XFA` key of the AcroForm dictionary. They use a completely different rendering model from AcroForm — XFA defines the layout, data model, and scripts in XML, not PDF content streams.

XFA is deprecated in PDF 2.0 (ISO 32000-2). Most modern PDFs do not use it. However, some enterprise and government forms still require XFA support.

## Scope Warning

Full XFA support is a separate document engine (essentially a web browser layout engine for XML). Do NOT attempt to implement this from scratch. Options:

1. **Server-side proxy**: Use LibreOffice or a commercial XFA renderer on the server. The REST API (Phase 3) can forward XFA PDFs to a LibreOffice instance for rendering/flattening.
2. **Detection + fallback**: Detect XFA forms, return a structured error with a helpful message.
3. **Partial support**: Parse XFA data model, extract field values, ignore layout. Useful for data extraction only.

## Phase 4a — XFA Detection

**File `src/forms/reader.rs`** — add detection:

```rust
pub fn is_xfa_form(doc: &PdfDocument) -> Result<bool> {
    let catalog = Catalog::from_document(doc)?;
    let acroform = match catalog.dict.get("AcroForm") {
        Some(o) => doc.resolve(o)?,
        None => return Ok(false),
    };
    let acroform_dict = acroform.as_dict().unwrap_or(&PdfDict::new()).clone();
    Ok(acroform_dict.contains_key("XFA"))
}
```

**WASM:**
```rust
pub fn is_xfa_form(&self) -> bool {
    crate::forms::is_xfa_form(&self.editor.doc).unwrap_or(false)
}
```

## Phase 4b — XFA Data Extraction

```rust
pub fn extract_xfa_data(doc: &PdfDocument) -> Result<String> {
    // Returns XFA XML data as a string
    let acroform_dict = get_acroform_dict(doc)?;
    let xfa_ref = acroform_dict.get("XFA")
        .ok_or_else(|| PdfError::invalid_structure("not an XFA form"))?;
    let xfa_obj = doc.resolve(xfa_ref)?;
    match xfa_obj {
        PdfObject::Array(arr) => {
            // XFA can be a list of [name, stream] pairs
            let mut xml = String::new();
            let mut i = 0;
            while i + 1 < arr.len() {
                if let PdfObject::Stream(s) = doc.resolve(&arr[i+1])? {
                    xml.push_str(&String::from_utf8_lossy(&s.decode_with_doc(doc)?));
                }
                i += 2;
            }
            Ok(xml)
        }
        PdfObject::Stream(s) => {
            Ok(String::from_utf8_lossy(&s.decode_with_doc(doc)?).to_string())
        }
        _ => Err(PdfError::invalid_structure("XFA not a stream or array")),
    }
}
```

## Phase 4c — Server-Side XFA Rendering (via LibreOffice)

In the REST API server (`src/bin/pdf_server.rs`):

```rust
/// POST /api/v1/xfa/flatten
/// Flattens an XFA form by converting to PDF/A using LibreOffice
pub async fn flatten_xfa(body: Bytes) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || {
        // Write to temp file
        let tmp = tempfile::NamedTempFile::with_suffix(".pdf")?;
        std::fs::write(tmp.path(), &body)?;
        // Call LibreOffice headless
        let output = std::process::Command::new("libreoffice")
            .args(["--headless", "--convert-to", "pdf", "--outdir", "/tmp", tmp.path().to_str().unwrap()])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "LibreOffice conversion failed"));
        }
        // Read output file
        let out_path = format!("/tmp/{}.pdf", tmp.path().file_stem().unwrap().to_str().unwrap());
        Ok(std::fs::read(out_path)?)
    }).await.unwrap();

    match result {
        Ok(pdf) => (StatusCode::OK, [("content-type", "application/pdf")], pdf).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
```

## WASM Behavior for XFA

Since WASM cannot run LibreOffice, return a structured error with guidance:
```rust
pub fn flatten_xfa_or_error(&mut self) -> Result<Vec<u8>, JsError> {
    if crate::forms::is_xfa_form(&self.editor.doc).unwrap_or(false) {
        return Err(JsError::new(
            "XFA forms require server-side processing. Use the REST API endpoint /api/v1/xfa/flatten."
        ));
    }
    // Not XFA — proceed with regular AcroForm flatten
    crate::forms::flatten_all_form_fields(&mut self.editor)
        .map_err(|e| JsError::new(&e.to_string()))?;
    self.save()
}
```
