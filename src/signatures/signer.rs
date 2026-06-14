//! PDF digital signature writer (ISO 32000-1 §12.8).
//!
//! Signs a PDF document by embedding a PKCS#7 / CMS `SignedData` structure in
//! an AcroForm `/Sig` field.  The approach:
//!
//! 1. Parse the document and add the signature field with placeholder values.
//! 2. Serialize the updated document (incremental update).
//! 3. Locate the `/ByteRange` and `/Contents` placeholders in the output bytes.
//! 4. Compute the real byte ranges; patch them in place.
//! 5. Hash the signed ranges with SHA-256.
//! 6. Build the CMS `SignedData`; hex-encode it into the `/Contents` slot.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};

/// Bytes reserved in `/Contents` for the DER-encoded CMS signature.
///
/// 8 192 bytes covers a 4 096-bit RSA key (512-byte sig) plus the full
/// certificate chain (≈ 2–4 KB), with headroom for additional attributes.
pub(crate) const RESERVED_SIG_SIZE: usize = 8192;

/// Fixed-width ByteRange placeholder written into the serialised output.
///
/// Exactly 53 bytes — enough for four 12-digit decimal numbers, which handles
/// PDF files up to ≈ 2 TB.  The real values are patched in-place, padded with
/// spaces (valid PDF whitespace between array elements).
const BYTERANGE_PLACEHOLDER: &[u8] = b"[999999999999 999999999999 999999999999 999999999999]";
const BYTERANGE_PLACEHOLDER_LEN: usize = BYTERANGE_PLACEHOLDER.len(); // 53

// ── Public types ──────────────────────────────────────────────────────────────

/// Options controlling signature field placement and metadata.
#[derive(Debug, Clone)]
pub struct SignatureOptions {
    /// Visible signature field rectangle `[x1, y1, x2, y2]` in PDF user space.
    pub rect: [f64; 4],
    /// 0-based page index on which to place the signature widget.
    pub page_index: usize,
    /// PDF field name (`/T`).
    pub field_name: String,
    /// Optional reason for signing (`/Reason`).
    pub reason: Option<String>,
    /// Optional signing location (`/Location`).
    pub location: Option<String>,
    /// Optional contact info (`/ContactInfo`).
    pub contact_info: Option<String>,
}

// ── Public signing API ────────────────────────────────────────────────────────

/// Sign a PDF document and return the signed bytes.
///
/// - `doc_bytes` — Original PDF to sign (read-only reference).
/// - `private_key_der` — PKCS#8 RSA private key in DER.
/// - `cert_der` — Signer X.509 certificate in DER.
/// - `options` — Signature field placement and metadata.
///
/// Requires an `Enterprise` license.  Returns the fully signed PDF bytes.
pub fn sign_document(
    doc_bytes: &[u8],
    private_key_der: &[u8],
    cert_der: &[u8],
    options: &SignatureOptions,
) -> Result<Vec<u8>> {
    crate::license::require(crate::license::Tier::Enterprise, "digital_signatures")?;

    // Step 1: Build the unsigned PDF with signature field placeholders.
    let mut editor = crate::editor::PdfEditor::open(doc_bytes.to_vec())?;
    build_signature_field(&mut editor, options)?;
    let mut pdf = editor.save_append(doc_bytes)?;

    // Step 2: Find the placeholder byte positions.
    let contents_offset = find_contents_placeholder(&pdf)
        .ok_or_else(|| PdfError::write_error("cannot locate /Contents placeholder in output"))?;
    let byterange_offset = find_byterange_placeholder(&pdf)
        .ok_or_else(|| PdfError::write_error("cannot locate /ByteRange placeholder in output"))?;

    // Step 3: Compute byte ranges.
    // The signed region covers everything EXCEPT the hex-encoded /Contents value.
    let r1_start: u64 = 0;
    let r1_end: u64 = contents_offset as u64; // up to (not including) '<'
    let hex_field_len = 1 + RESERVED_SIG_SIZE * 2 + 1; // '<' + zeros + '>'
    let r2_start: u64 = (contents_offset + hex_field_len) as u64;
    let r2_len: u64 = pdf.len() as u64 - r2_start;

    // Step 4: Patch /ByteRange in place.
    let byterange_str = format_byterange(r1_start, r1_end, r2_start, r2_len);
    patch_bytes(&mut pdf, byterange_offset, byterange_str.as_bytes());

    // Step 5: Hash the signed byte ranges with SHA-256.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&pdf[r1_start as usize..r1_end as usize]);
    hasher.update(&pdf[r2_start as usize..(r2_start + r2_len) as usize]);
    let hash: [u8; 32] = hasher.finalize().into();

    // Step 6: Build CMS SignedData (no signing time — WASM has no system clock).
    let cms_der = super::cms::build_cms_signed_data(&hash, private_key_der, cert_der, None)?;
    if cms_der.len() > RESERVED_SIG_SIZE {
        return Err(PdfError::write_error(format!(
            "CMS signature ({} B) exceeds reserved space ({} B)",
            cms_der.len(),
            RESERVED_SIG_SIZE,
        )));
    }

    // Step 7: Hex-encode CMS and patch into /Contents placeholder.
    let hex_sig = hex_encode(&cms_der);
    // Pad with zeros to fill the reserved hex slot exactly.
    let padded: String = format!("{:0<width$}", hex_sig, width = RESERVED_SIG_SIZE * 2);
    patch_bytes(&mut pdf, contents_offset + 1, padded.as_bytes()); // +1 skips '<'

    Ok(pdf)
}

// ── Internal: build signature field in the editor ────────────────────────────

fn build_signature_field(
    editor: &mut crate::editor::PdfEditor,
    options: &SignatureOptions,
) -> Result<()> {
    // Build the Sig value dict (/Contents placeholder + /ByteRange placeholder).
    let mut sig_value = PdfDict::new();
    sig_value.insert("Type".to_owned(), PdfObject::Name("Sig".to_owned()));
    sig_value.insert(
        "Filter".to_owned(),
        PdfObject::Name("Adobe.PPKLite".to_owned()),
    );
    sig_value.insert(
        "SubFilter".to_owned(),
        PdfObject::Name("adbe.pkcs7.detached".to_owned()),
    );
    // ByteRange placeholder: four large integers fill exactly BYTERANGE_PLACEHOLDER_LEN bytes.
    sig_value.insert(
        "ByteRange".to_owned(),
        PdfObject::Array(vec![PdfObject::Integer(999_999_999_999); 4]),
    );
    // Contents placeholder: RESERVED_SIG_SIZE zero bytes → serialised as <000…0>.
    sig_value.insert(
        "Contents".to_owned(),
        PdfObject::String(vec![0u8; RESERVED_SIG_SIZE]),
    );
    if let Some(r) = &options.reason {
        sig_value.insert(
            "Reason".to_owned(),
            PdfObject::String(r.as_bytes().to_vec()),
        );
    }
    if let Some(l) = &options.location {
        sig_value.insert(
            "Location".to_owned(),
            PdfObject::String(l.as_bytes().to_vec()),
        );
    }
    if let Some(c) = &options.contact_info {
        sig_value.insert(
            "ContactInfo".to_owned(),
            PdfObject::String(c.as_bytes().to_vec()),
        );
    }
    let sig_value_id = editor.add_object(PdfObject::Dictionary(sig_value));

    // Build the Sig field / Widget annotation dict.
    let (page_id, _) = editor.get_page_dict(options.page_index)?;
    let mut sig_field = PdfDict::new();
    sig_field.insert("Type".to_owned(), PdfObject::Name("Annot".to_owned()));
    sig_field.insert("Subtype".to_owned(), PdfObject::Name("Widget".to_owned()));
    sig_field.insert("FT".to_owned(), PdfObject::Name("Sig".to_owned()));
    sig_field.insert(
        "T".to_owned(),
        PdfObject::String(options.field_name.as_bytes().to_vec()),
    );
    sig_field.insert("V".to_owned(), PdfObject::Reference(sig_value_id, 0));
    sig_field.insert(
        "Rect".to_owned(),
        PdfObject::Array(options.rect.iter().map(|&v| PdfObject::Real(v)).collect()),
    );
    sig_field.insert("P".to_owned(), PdfObject::Reference(page_id, 0));
    sig_field.insert("Flags".to_owned(), PdfObject::Integer(4)); // Print
    let sig_field_id = editor.add_object(PdfObject::Dictionary(sig_field));

    // Add to page /Annots.
    add_to_page_annots(editor, options.page_index, sig_field_id)?;

    // Add to or create /AcroForm in the catalog.
    add_to_acroform(editor, sig_field_id)?;

    Ok(())
}

fn add_to_page_annots(
    editor: &mut crate::editor::PdfEditor,
    page_index: usize,
    annot_id: u32,
) -> Result<()> {
    let (page_id, mut page_dict) = editor.get_page_dict(page_index)?;
    let mut annots: Vec<PdfObject> = match page_dict.get("Annots") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        Some(PdfObject::Reference(id, _)) => match editor.get_object(*id)? {
            PdfObject::Array(arr) => arr,
            _ => vec![],
        },
        _ => vec![],
    };
    annots.push(PdfObject::Reference(annot_id, 0));
    page_dict.insert("Annots".to_owned(), PdfObject::Array(annots));
    editor.replace_object(page_id, PdfObject::Dictionary(page_dict));
    Ok(())
}

fn add_to_acroform(editor: &mut crate::editor::PdfEditor, sig_field_id: u32) -> Result<()> {
    // Resolve or create the /AcroForm dict.
    let catalog_id = editor.catalog_id;
    let catalog = editor.get_object(catalog_id)?;
    let mut cat_dict = catalog
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("catalog is not a dict"))?
        .clone();

    let (acroform_id, mut acroform_dict) = match cat_dict.get("AcroForm") {
        Some(PdfObject::Reference(id, _)) => {
            let id = *id;
            let obj = editor.get_object(id)?;
            let d = obj
                .as_dict()
                .ok_or_else(|| PdfError::invalid_structure("AcroForm is not a dict"))?
                .clone();
            (Some(id), d)
        }
        Some(PdfObject::Dictionary(d)) => (None, d.clone()),
        _ => (None, PdfDict::new()),
    };

    // Add the sig field to /Fields.
    let mut fields: Vec<PdfObject> = match acroform_dict.get("Fields") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => vec![],
    };
    fields.push(PdfObject::Reference(sig_field_id, 0));
    acroform_dict.insert("Fields".to_owned(), PdfObject::Array(fields));

    // /SigFlags bit 0 = SignaturesExist, bit 1 = AppendOnly.
    acroform_dict.insert("SigFlags".to_owned(), PdfObject::Integer(3));

    // Write updated AcroForm back.
    let new_acroform_id = match acroform_id {
        Some(id) => {
            editor.replace_object(id, PdfObject::Dictionary(acroform_dict));
            id
        }
        None => editor.add_object(PdfObject::Dictionary(acroform_dict)),
    };

    // Update catalog to reference the AcroForm.
    cat_dict.insert(
        "AcroForm".to_owned(),
        PdfObject::Reference(new_acroform_id, 0),
    );
    editor.replace_object(catalog_id, PdfObject::Dictionary(cat_dict));

    Ok(())
}

// ── Byte-level helpers ────────────────────────────────────────────────────────

/// Find the offset of `<` that starts the `/Contents` hex-string placeholder.
///
/// The placeholder is `<` + `RESERVED_SIG_SIZE * 2` ASCII zeros + `>`, which
/// is unique in any well-formed PDF (no other value is that long and all-zero).
fn find_contents_placeholder(bytes: &[u8]) -> Option<usize> {
    let zeros = RESERVED_SIG_SIZE * 2;
    if bytes.len() < zeros + 2 {
        return None;
    }
    'outer: for i in 0..bytes.len() - zeros - 1 {
        if bytes[i] != b'<' {
            continue;
        }
        if bytes.get(i + 1 + zeros) != Some(&b'>') {
            continue;
        }
        for &b in &bytes[i + 1..i + 1 + zeros] {
            if b != b'0' {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

/// Find the offset of `[` that starts the `/ByteRange` placeholder array.
fn find_byterange_placeholder(bytes: &[u8]) -> Option<usize> {
    let pl = BYTERANGE_PLACEHOLDER;
    bytes.windows(pl.len()).position(|w| w == pl)
}

/// Format the real byte-range as a fixed-width 53-byte string, padding with
/// spaces before the closing `]` so the patch is exactly `BYTERANGE_PLACEHOLDER_LEN`.
fn format_byterange(r1s: u64, r1l: u64, r2s: u64, r2l: u64) -> String {
    let nums = format!("{} {} {} {}", r1s, r1l, r2s, r2l);
    let inner_cap = BYTERANGE_PLACEHOLDER_LEN - 2; // capacity inside '[' and ']'
    assert!(
        nums.len() <= inner_cap,
        "ByteRange values too long for placeholder ({} > {})",
        nums.len(),
        inner_cap
    );
    format!("[{:<width$}]", nums, width = inner_cap)
}

/// Overwrite `dst[at..at+src.len()]` with `src` bytes.
fn patch_bytes(dst: &mut [u8], at: usize, src: &[u8]) {
    dst[at..at + src.len()].copy_from_slice(src);
}

/// Encode bytes as uppercase ASCII hex (no prefix, no spaces).
fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02X}", b)).collect()
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_byterange_correct_length() {
        let s = format_byterange(0, 12345678, 12345695, 98765432);
        assert_eq!(
            s.len(),
            BYTERANGE_PLACEHOLDER_LEN,
            "padded byterange must match placeholder length"
        );
        assert!(s.starts_with('['));
        assert!(s.ends_with(']'));
    }

    #[test]
    fn format_byterange_parses_back() {
        let s = format_byterange(0, 100, 16487, 200);
        // Trim and parse back the four numbers.
        let inner = s.trim_matches(|c| c == '[' || c == ']').trim();
        let parts: Vec<u64> = inner
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!(parts, vec![0, 100, 16487, 200]);
    }

    #[test]
    fn find_contents_placeholder_found() {
        let mut bytes: Vec<u8> = b"prefix<".to_vec();
        bytes.extend(vec![b'0'; RESERVED_SIG_SIZE * 2]);
        bytes.extend_from_slice(b">suffix");
        let offset = find_contents_placeholder(&bytes).unwrap();
        assert_eq!(offset, 6); // "prefix<" is 7 bytes; '<' is at index 6
        assert_eq!(bytes[offset], b'<');
    }

    #[test]
    fn find_byterange_placeholder_found() {
        let mut bytes: Vec<u8> = b"abc ".to_vec();
        bytes.extend_from_slice(BYTERANGE_PLACEHOLDER);
        bytes.extend_from_slice(b" xyz");
        let offset = find_byterange_placeholder(&bytes).unwrap();
        assert_eq!(offset, 4);
    }

    #[test]
    fn hex_encode_smoke() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "DEADBEEF");
    }
}
