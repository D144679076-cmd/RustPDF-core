# Phase 2 — Document Permissions Enforcement

**Status:** Not started
**Effort:** ~2 weeks

## Context

Encrypted PDFs carry a `/P` integer in the `/Encrypt` dict that encodes what operations are allowed (ISO 32000-1 §7.6.3.3 Table 22). Currently the value is parsed and discarded. Need to: (1) parse permissions into a typed struct, (2) store on `PdfDocument`, (3) check permissions before editing operations in the WASM layer.

## Step 1 — New Type in `src/crypto/handler.rs`

```rust
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    pub can_print: bool,                  // bit 3 (value 4)
    pub can_modify: bool,                 // bit 4 (value 8)
    pub can_copy_text: bool,              // bit 5 (value 16)
    pub can_annotate: bool,               // bit 6 (value 32)
    pub can_fill_forms: bool,             // bit 9 (value 256)
    pub can_extract_accessibility: bool,  // bit 10 (value 512)
    pub can_assemble: bool,               // bit 11 (value 1024)
    pub can_print_high_quality: bool,     // bit 12 (value 2048)
}

pub fn parse_permissions(p: i32) -> Permissions {
    Permissions {
        can_print:                  p & 4    != 0,
        can_modify:                 p & 8    != 0,
        can_copy_text:              p & 16   != 0,
        can_annotate:               p & 32   != 0,
        can_fill_forms:             p & 256  != 0,
        can_extract_accessibility:  p & 512  != 0,
        can_assemble:               p & 1024 != 0,
        can_print_high_quality:     p & 2048 != 0,
    }
}
```

## Step 2 — Store Permissions on `PdfDocument`

In `src/parser/objects.rs`, add to `PdfDocument`:
```rust
pub struct PdfDocument {
    // ... existing fields ...
    pub permissions: std::cell::Cell<Option<crate::crypto::handler::Permissions>>,
}
```

In `src/parser/objects.rs::PdfDocument::parse_with_password()`, after successful decryption:
```rust
if let Some(encrypt_dict) = trailer.get("Encrypt") {
    let encrypt = doc.resolve(encrypt_dict)?;
    if let Some(dict) = encrypt.as_dict() {
        if let Some(PdfObject::Integer(p)) = dict.get("P") {
            let perms = crate::crypto::handler::parse_permissions(*p as i32);
            doc.permissions.set(Some(perms));
        }
    }
}
```

Add getter:
```rust
impl PdfDocument {
    pub fn permissions(&self) -> Option<crate::crypto::handler::Permissions> {
        self.permissions.get()
    }
}
```

## Step 3 — Permission Checks in WASM Layer (`src/wasm/editor.rs`)

Add a helper:
```rust
fn check_permission(doc: &crate::parser::PdfDocument, permission: fn(&crate::crypto::Permissions) -> bool, feature: &str) -> Result<(), JsError> {
    if let Some(perms) = doc.permissions() {
        if !permission(&perms) {
            return Err(JsError::new(&format!("Document permissions deny '{}'", feature)));
        }
    }
    Ok(())
}
```

Add permission checks at the top of these WASM methods:

| WASM Method | Permission Check |
|-------------|-----------------|
| `add_annotation` | `can_annotate` |
| `delete_annotation` | `can_annotate` |
| `set_field_value` | `can_fill_forms` |
| `import_fdf` | `can_fill_forms` |
| `add_page` | `can_assemble` |
| `delete_page` | `can_assemble` |
| `move_page` | `can_assemble` |
| `merge` | `can_assemble` |
| `extract_pages` | `can_assemble` |
| `extract_text` | `can_copy_text` |
| `search_text` | `can_copy_text` |
| `set_metadata` | `can_modify` |

Example:
```rust
#[wasm_bindgen]
pub fn add_annotation(&mut self, page_index: usize, annot_json: &str) -> Result<(), JsError> {
    check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
    // ... existing logic
}
```

## Step 4 — Expose Permissions to JavaScript

```rust
#[wasm_bindgen]
pub fn get_permissions(&self) -> String {
    // Returns JSON object with all permission flags
    let perms = self.doc.permissions().unwrap_or_default();
    format!(
        r#"{{"can_print":{},"can_modify":{},"can_copy_text":{},"can_annotate":{},"can_fill_forms":{},"can_assemble":{}}}"#,
        perms.can_print, perms.can_modify, perms.can_copy_text,
        perms.can_annotate, perms.can_fill_forms, perms.can_assemble
    )
}
```

## Tests

```rust
#[cfg(feature = "crypto")]
#[test]
fn restricted_pdf_permissions_parsed() {
    // Create a PDF with print-only permission using qpdf:
    // qpdf --encrypt user owner 128 --print=low -- minimal.pdf restricted.pdf
    let data = include_bytes!("fixtures/restricted.pdf").to_vec();
    let doc = PdfDocument::parse_with_password(data, b"user").unwrap();
    let perms = doc.permissions().unwrap();
    assert!(!perms.can_modify);
    assert!(!perms.can_annotate);
}

#[cfg(feature = "crypto")]
#[test]
fn unencrypted_pdf_has_no_permission_restrictions() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let doc = PdfDocument::parse(data).unwrap();
    assert!(doc.permissions().is_none()); // no restrictions
}
```

Add `tests/fixtures/restricted.pdf` using:
```bash
qpdf --encrypt user owner 128 --modify=none --annotate=n --extract=n -- tests/fixtures/minimal.pdf tests/fixtures/restricted.pdf
```

## Verification

```bash
cargo test --features crypto -- permissions
cargo build --target wasm32-unknown-unknown --features wasm,crypto
```
