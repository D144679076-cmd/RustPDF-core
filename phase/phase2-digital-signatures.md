# Phase 2 — Digital Signatures (PKCS#7 / PAdES)

**Status:** Complete — 2026-06-12
**Effort:** ~3–4 months
**Tier gate:** Enterprise
**New Cargo.toml feature:** `signatures`

## Context

PDF digital signatures require: (1) building a `/Sig` field with a reserved byte range in `/Contents`, (2) computing SHA-256 over the non-signature bytes, (3) wrapping in PKCS#7 CMS SignedData DER, (4) patching the reserved bytes. This is complex but well-specified in ISO 32000-1 §12.8 and RFC 5652 (CMS).

## New Dependencies in `Cargo.toml`

```toml
[dependencies]
rsa = { version = "0.9", optional = true, features = ["sha2"] }
der = { version = "0.7", optional = true }
x509-cert = { version = "0.2", optional = true }

[features]
signatures = ["dep:rsa", "dep:der", "dep:x509-cert", "crypto"]
```

All three crates are WASM-compatible (pure Rust, no system deps).

## New Module Structure: `src/signatures/`

```
src/signatures/
  mod.rs
  signer.rs       — sign a PDF document
  verifier.rs     — verify signatures in an existing PDF
  cms.rs          — build PKCS#7 CMS SignedData DER
  appearance.rs   — visual appearance stream for signature field
```

## `src/signatures/cms.rs` — PKCS#7 CMS SignedData Builder

```rust
/// Build a PKCS#7 CMS SignedData DER structure for PDF signing.
/// `content_hash`: SHA-256 hash of the signed byte ranges.
/// `private_key_der`: PKCS#8 private key (RSA or ECDSA).
/// `cert_der`: Signer's X.509 certificate in DER.
/// Returns: DER-encoded CMS ContentInfo wrapping SignedData.
pub fn build_cms_signed_data(
    content_hash: &[u8; 32],
    private_key_der: &[u8],
    cert_der: &[u8],
    signing_time: Option<u64>,  // Unix timestamp
) -> Result<Vec<u8>>
```

**Algorithm:**
1. Parse private key with `rsa::RsaPrivateKey::from_pkcs8_der()`.
2. Build `SignedAttributes`:
   - `contentType`: OID 1.2.840.113549.1.7.1 (data)
   - `signingTime`: GeneralizedTime if provided
   - `messageDigest`: `content_hash` bytes (SHA-256)
3. DER-encode `SignedAttributes` using `der` crate.
4. Sign the DER-encoded `SignedAttributes` with RSA PKCS1v15-SHA256.
5. Build `SignerInfo`:
   - `version`: 1
   - `sid`: IssuerAndSerialNumber from cert
   - `digestAlgorithm`: SHA-256 OID
   - `signedAttrs`: from step 3
   - `signatureAlgorithm`: rsaEncryption OID
   - `signature`: from step 4
6. Build `SignedData`:
   - `version`: 1
   - `digestAlgorithms`: [SHA-256]
   - `encapContentInfo`: id-data, no content (detached signature)
   - `certificates`: [cert_der as ASN.1 Certificate]
   - `signerInfos`: [SignerInfo from step 5]
7. Wrap in `ContentInfo` with OID 1.2.840.113549.1.7.2 (signedData).
8. DER-encode and return.

## `src/signatures/signer.rs`

```rust
#[derive(Debug)]
pub struct SignatureOptions {
    pub reason: Option<String>,
    pub location: Option<String>,
    pub contact_info: Option<String>,
    pub rect: [f64; 4],
    pub page_index: usize,
    pub field_name: String,
}

/// Sign a PDF document and return the signed bytes.
pub fn sign_document(
    doc_bytes: &[u8],
    private_key_der: &[u8],
    cert_der: &[u8],
    options: &SignatureOptions,
) -> Result<Vec<u8>> {
    crate::license::require(crate::license::Tier::Enterprise, "digital_signatures")?;

    const RESERVED_SIG_SIZE: usize = 8192;  // bytes reserved for DER signature

    // Step 1: Build unsigned PDF with signature field and placeholder
    let mut editor = crate::editor::PdfEditor::open(doc_bytes.to_vec())?;
    // Add signature field widget to page
    let sig_field_id = build_signature_field(&mut editor, options)?;
    // Serialize → get bytes with placeholder /Contents <000...0> and /ByteRange [0 0 0 0]
    let mut pdf_with_placeholder = editor.save_append()?;

    // Step 2: Find where the /Contents placeholder and /ByteRange are in the output
    let contents_offset = find_contents_placeholder(&pdf_with_placeholder)
        .ok_or_else(|| PdfError::write_error("could not locate /Contents placeholder"))?;
    let byterange_offset = find_byterange_placeholder(&pdf_with_placeholder)
        .ok_or_else(|| PdfError::write_error("could not locate /ByteRange placeholder"))?;

    // Step 3: Compute byte ranges
    // Signed bytes = everything EXCEPT the hex-encoded /Contents value itself
    let range1_start = 0u64;
    let range1_end = contents_offset as u64;
    let range2_start = (contents_offset + 1 + RESERVED_SIG_SIZE * 2 + 1) as u64; // skip <hex>
    let range2_end = pdf_with_placeholder.len() as u64;

    // Patch /ByteRange in the bytes
    let byterange_str = format!("[{} {} {} {}]",
        range1_start, range1_end, range2_start, range2_end - range2_start);
    patch_bytes(&mut pdf_with_placeholder, byterange_offset, byterange_str.as_bytes());

    // Step 4: Hash the signed ranges
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&pdf_with_placeholder[range1_start as usize..range1_end as usize]);
    hasher.update(&pdf_with_placeholder[range2_start as usize..range2_end as usize]);
    let hash: [u8; 32] = hasher.finalize().into();

    // Step 5: Build CMS signature
    let now_ts = None; // WASM has no system clock; use None
    let cms_der = cms::build_cms_signed_data(&hash, private_key_der, cert_der, now_ts)?;
    if cms_der.len() > RESERVED_SIG_SIZE {
        return Err(PdfError::write_error("CMS signature too large for reserved space"));
    }

    // Step 6: Hex-encode CMS and patch into /Contents placeholder
    let hex_cms = hex_encode(&cms_der);
    // Pad with zeros to fill reserved space
    let padded_hex = format!("{:0<width$}", hex_cms, width = RESERVED_SIG_SIZE * 2);
    patch_bytes(&mut pdf_with_placeholder, contents_offset + 1, padded_hex.as_bytes());

    Ok(pdf_with_placeholder)
}

fn build_signature_field(editor: &mut crate::editor::PdfEditor, options: &SignatureOptions) -> Result<u32> {
    // Build /Sig field dict
    let mut sig_dict = crate::parser::PdfDict::new();
    sig_dict.insert("FT".to_owned(), PdfObject::Name("Sig".to_owned()));
    sig_dict.insert("T".to_owned(), PdfObject::String(options.field_name.as_bytes().to_vec()));
    if let Some(r) = &options.reason {
        sig_dict.insert("Reason".to_owned(), PdfObject::String(r.as_bytes().to_vec()));
    }
    // /V is a signature value dict with ByteRange and Contents placeholders
    let mut sig_value = crate::parser::PdfDict::new();
    sig_value.insert("Type".to_owned(), PdfObject::Name("Sig".to_owned()));
    sig_value.insert("Filter".to_owned(), PdfObject::Name("Adobe.PPKLite".to_owned()));
    sig_value.insert("SubFilter".to_owned(), PdfObject::Name("adbe.pkcs7.detached".to_owned()));
    // Placeholder ByteRange — will be patched post-serialization
    sig_value.insert("ByteRange".to_owned(), PdfObject::Array(vec![
        PdfObject::Integer(0), PdfObject::Integer(0), PdfObject::Integer(0), PdfObject::Integer(0)
    ]));
    // Placeholder Contents — 8192 zero bytes as hex string
    sig_value.insert("Contents".to_owned(), PdfObject::HexString(vec![0u8; 8192]));
    let sig_value_id = editor.add_object(PdfObject::Dictionary(sig_value));
    sig_dict.insert("V".to_owned(), PdfObject::Reference(sig_value_id, 0));
    // Widget appearance
    sig_dict.insert("Rect".to_owned(), PdfObject::Array(options.rect.iter().map(|&r| PdfObject::Real(r)).collect()));
    sig_dict.insert("Subtype".to_owned(), PdfObject::Name("Widget".to_owned()));
    sig_dict.insert("P".to_owned(), {
        let (page_id, _) = editor.get_page_dict(options.page_index)?;
        PdfObject::Reference(page_id, 0)
    });
    let field_id = editor.add_object(PdfObject::Dictionary(sig_dict));
    // Add to AcroForm /Fields
    // ... (add_to_acroform helper)
    // Add to page /Annots
    crate::editor::annotation::add_annotation(editor, options.page_index, {
        let mut d = std::collections::HashMap::new(); d.insert("Type".to_owned(), PdfObject::Reference(field_id, 0)); d
    })?;
    Ok(field_id)
}
```

## `src/signatures/verifier.rs`

```rust
#[derive(Debug)]
pub struct SignatureVerification {
    pub field_name: String,
    pub signer_name: Option<String>,
    pub signing_time: Option<String>,
    pub reason: Option<String>,
    pub valid: bool,
    pub error: Option<String>,
}

pub fn verify_signatures(doc: &PdfDocument) -> Result<Vec<SignatureVerification>>
// 1. Find all /Sig fields in /AcroForm
// 2. For each: read /ByteRange, /Contents (DER signature)
// 3. Hash the byte ranges with SHA-256
// 4. Parse CMS SignedData from /Contents
// 5. Verify RSA signature over SignedAttributes
// 6. Verify messageDigest in SignedAttributes matches computed hash
// 7. Return verification result
```

## WASM in `src/wasm/editor.rs`

```rust
#[cfg(feature = "signatures")]
#[wasm_bindgen]
pub fn sign_document(
    &mut self,
    private_key_der: &[u8],
    cert_der: &[u8],
    options_json: &str,   // JSON: {reason, location, contact, rect:[x1,y1,x2,y2], page_index, field_name}
) -> Result<Vec<u8>, JsError>

#[cfg(feature = "signatures")]
#[wasm_bindgen]
pub fn verify_signatures(&self) -> Result<String, JsError>
// Returns JSON array of SignatureVerification
```

## Signature UI (`web-editor/src/components/SignaturePanel.vue`)

- Canvas drawing area (capture ink strokes as Ink annotation) OR
- Upload an image (PNG/JPG) to use as visual signature appearance.
- Type name → render as stylized text using a script font.
- Drag to position on page.
- On "Sign": call `wasmEditor.sign_document(keyDer, certDer, options)` → download signed PDF.

## Tests

```rust
#[cfg(feature = "signatures")]
#[test]
fn sign_and_verify_round_trip() {
    let pdf_bytes = include_bytes!("fixtures/minimal.pdf").to_vec();
    let key_der = include_bytes!("fixtures/test_key.der");
    let cert_der = include_bytes!("fixtures/test_cert.der");
    let options = SignatureOptions { field_name: "Sig1".to_owned(), page_index: 0, rect: [100.0,100.0,300.0,150.0], reason: None, location: None, contact_info: None };
    let signed = sign_document(&pdf_bytes, key_der, cert_der, &options).unwrap();
    let doc = PdfDocument::parse(signed).unwrap();
    let verifications = verify_signatures(&doc).unwrap();
    assert_eq!(verifications.len(), 1);
    assert!(verifications[0].valid);
}
```

## Verification

```bash
cargo test --features signatures -- sign
cargo build --target wasm32-unknown-unknown --features wasm,signatures
```
