//! PDF digital signature verifier (ISO 32000-1 §12.8, RFC 5652).
//!
//! Reads every `/Sig` field in the document's AcroForm, reconstructs the
//! signed byte ranges from `/ByteRange`, hashes them with SHA-256, and
//! verifies the embedded PKCS#7 CMS `SignedData` structure.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

/// Result of verifying a single signature field.
#[derive(Debug, Clone)]
pub struct SignatureVerification {
    /// PDF field name (`/T`) of the signature field.
    pub field_name: String,
    /// Whether the mathematical signature is valid.
    pub signature_valid: bool,
    /// Whether the signed byte ranges cover the entire file (except `/Contents`).
    pub covers_whole_file: bool,
    /// Subject common-name extracted from the signer certificate, if parseable.
    pub signer_name: Option<String>,
    /// Human-readable error, present when `signature_valid` is false.
    pub error: Option<String>,
}

/// Verify all digital signatures present in `doc_bytes`.
///
/// Returns one [`SignatureVerification`] per `/Sig` field found. Returns an
/// empty `Vec` when the document contains no AcroForm or no signature fields.
pub fn verify_signatures(doc_bytes: &[u8]) -> Result<Vec<SignatureVerification>> {
    let doc = PdfDocument::parse(doc_bytes.to_vec())?;

    let acroform = match get_acroform(&doc)? {
        Some(d) => d,
        None => return Ok(vec![]),
    };

    let fields = match acroform.get("Fields") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Ok(vec![]),
    };

    let mut results = Vec::new();
    for field_ref in &fields {
        if let Some(v) = verify_field(&doc, field_ref, doc_bytes)? {
            results.push(v);
        }
    }

    Ok(results)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn get_acroform(doc: &PdfDocument) -> Result<Option<PdfDict>> {
    let root_ref = match doc.trailer.get("Root") {
        Some(r) => r.clone(),
        None => return Ok(None),
    };
    let catalog = doc.resolve(&root_ref)?;
    let cat_dict = match catalog.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(None),
    };
    match cat_dict.get("AcroForm") {
        Some(PdfObject::Reference(id, _)) => {
            let id = *id;
            let obj = doc.get_object(id)?;
            Ok(obj.as_dict().cloned())
        }
        Some(PdfObject::Dictionary(d)) => Ok(Some(d.clone())),
        _ => Ok(None),
    }
}

fn verify_field(
    doc: &PdfDocument,
    field_ref: &PdfObject,
    doc_bytes: &[u8],
) -> Result<Option<SignatureVerification>> {
    let field_obj = doc.resolve(field_ref)?;
    let field_dict = match field_obj.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(None),
    };

    // Only process /Sig fields.
    let ft = match field_dict.get("FT") {
        Some(PdfObject::Name(n)) => n.clone(),
        _ => return Ok(None),
    };
    if ft != "Sig" {
        return Ok(None);
    }

    let field_name = match field_dict.get("T") {
        Some(PdfObject::String(b)) => String::from_utf8_lossy(b).to_string(),
        _ => "<unnamed>".to_owned(),
    };

    // Resolve /V (the signature value dictionary).
    let sig_value_obj = match field_dict.get("V") {
        Some(PdfObject::Reference(id, _)) => {
            let id = *id;
            doc.get_object(id)?
        }
        Some(obj) => obj.clone(),
        None => {
            return Ok(Some(err_result(field_name, "missing /V in sig field")));
        }
    };
    let sig_dict = match sig_value_obj.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(Some(err_result(field_name, "/V is not a dictionary"))),
    };

    // /ByteRange: [r1_start r1_len r2_start r2_len]
    let byte_range = match sig_dict.get("ByteRange") {
        Some(PdfObject::Array(arr)) if arr.len() == 4 => arr.clone(),
        _ => {
            return Ok(Some(err_result(
                field_name,
                "missing or invalid /ByteRange",
            )))
        }
    };
    let r1s = extract_u64(&byte_range[0]);
    let r1l = extract_u64(&byte_range[1]);
    let r2s = extract_u64(&byte_range[2]);
    let r2l = extract_u64(&byte_range[3]);
    let file_len = doc_bytes.len() as u64;
    let covers_whole_file = r1s == 0 && (r2s + r2l) == file_len;

    if r1s + r1l > file_len || r2s + r2l > file_len {
        return Ok(Some(SignatureVerification {
            field_name,
            signature_valid: false,
            covers_whole_file,
            signer_name: None,
            error: Some("/ByteRange extends beyond file".to_owned()),
        }));
    }

    // /Contents: the raw DER bytes of the CMS SignedData (stored as a PDF string).
    let contents_bytes = match sig_dict.get("Contents") {
        Some(PdfObject::String(b)) => b.clone(),
        _ => return Ok(Some(err_result(field_name, "missing /Contents"))),
    };
    // Strip trailing zero-padding from the reserved slot.
    let cms_end = contents_bytes
        .iter()
        .rposition(|&b| b != 0)
        .map(|p| p + 1)
        .unwrap_or(0);
    let cms_der = &contents_bytes[..cms_end];

    // Hash the two signed byte ranges.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&doc_bytes[r1s as usize..(r1s + r1l) as usize]);
    hasher.update(&doc_bytes[r2s as usize..(r2s + r2l) as usize]);
    let computed_hash: [u8; 32] = hasher.finalize().into();

    match verify_cms_signed_data(cms_der, &computed_hash) {
        Ok(signer_name) => Ok(Some(SignatureVerification {
            field_name,
            signature_valid: true,
            covers_whole_file,
            signer_name,
            error: None,
        })),
        Err(e) => Ok(Some(SignatureVerification {
            field_name,
            signature_valid: false,
            covers_whole_file,
            signer_name: None,
            error: Some(e.to_string()),
        })),
    }
}

fn err_result(field_name: String, msg: &str) -> SignatureVerification {
    SignatureVerification {
        field_name,
        signature_valid: false,
        covers_whole_file: false,
        signer_name: None,
        error: Some(msg.to_owned()),
    }
}

fn extract_u64(obj: &PdfObject) -> u64 {
    match obj {
        PdfObject::Integer(n) => *n as u64,
        PdfObject::Real(f) => *f as u64,
        _ => 0,
    }
}

// ── CMS verification ──────────────────────────────────────────────────────────

/// Verify the CMS `SignedData` `cms_der` against `content_hash`.
///
/// Returns the signer's common name on success.
fn verify_cms_signed_data(cms_der: &[u8], content_hash: &[u8; 32]) -> Result<Option<String>> {
    use super::cms::{der_parse, der_set, OID_RSA_ENCRYPTION, OID_SIGNED_DATA};

    // ContentInfo ::= SEQUENCE { OID id-signedData, [0] EXPLICIT SignedData }
    let (_, ci_content, _) =
        der_parse(cms_der).map_err(|e| PdfError::write_error(format!("CMS ContentInfo: {e}")))?;
    let (oid_tag, oid_val, ci_rest) =
        der_parse(ci_content).map_err(|e| PdfError::write_error(format!("CMS OID: {e}")))?;
    if oid_tag != 0x06 || oid_val != OID_SIGNED_DATA {
        return Err(PdfError::write_error("not a SignedData ContentInfo"));
    }
    // [0] EXPLICIT { SignedData SEQUENCE }
    let (_, ctx0_val, _) =
        der_parse(ci_rest).map_err(|e| PdfError::write_error(format!("CMS ctx0: {e}")))?;
    // ctx0_val is the SignedData SEQUENCE bytes.
    let (_, sd_body, _) = der_parse(ctx0_val)
        .map_err(|e| PdfError::write_error(format!("CMS SignedData seq: {e}")))?;

    // Walk SignedData body: version, digestAlgorithms, encapContentInfo, [0]?, signerInfos.
    let (_, _, sd_rest) = der_parse(sd_body) // version
        .map_err(|e| PdfError::write_error(format!("SD version: {e}")))?;
    let (_, _, sd_rest) =
        der_parse(sd_rest) // digestAlgorithms
            .map_err(|e| PdfError::write_error(format!("SD digestAlgorithms: {e}")))?;
    let (_, _, sd_rest) =
        der_parse(sd_rest) // encapContentInfo
            .map_err(|e| PdfError::write_error(format!("SD encapContentInfo: {e}")))?;
    let sd_rest = skip_context_tags(sd_rest); // skip optional [0] certs and [1] crls

    // signerInfos SET
    let (_, si_set_val, _) =
        der_parse(sd_rest).map_err(|e| PdfError::write_error(format!("SD signerInfos: {e}")))?;
    let (_, si_body, _) = der_parse(si_set_val)
        .map_err(|e| PdfError::write_error(format!("signerInfo SEQUENCE: {e}")))?;

    // SignerInfo: version, sid, digestAlgorithm, [0] signedAttrs, signatureAlgorithm, signature.
    let (_, _, si_rest) = der_parse(si_body) // version
        .map_err(|e| PdfError::write_error(format!("SI version: {e}")))?;
    let (_, _, si_rest) = der_parse(si_rest) // sid
        .map_err(|e| PdfError::write_error(format!("SI sid: {e}")))?;
    let (_, _, si_rest) =
        der_parse(si_rest) // digestAlgorithm
            .map_err(|e| PdfError::write_error(format!("SI digestAlgorithm: {e}")))?;

    // [0] IMPLICIT signedAttrs — tag 0xa0.
    let (ctx_tag, signed_attrs_val, si_rest) =
        der_parse(si_rest).map_err(|e| PdfError::write_error(format!("SI signedAttrs: {e}")))?;
    if ctx_tag != 0xa0 {
        return Err(PdfError::write_error(
            "expected [0] signedAttrs tag in SignerInfo",
        ));
    }

    // RFC 5652 §5.4: replace [0] IMPLICIT with SET tag for hashing.
    let signed_attrs_set = der_set(signed_attrs_val);

    // Verify the embedded messageDigest matches our computed hash.
    let embedded_hash = find_message_digest(signed_attrs_val)?;
    if &embedded_hash != content_hash {
        return Err(PdfError::write_error(
            "messageDigest does not match signed byte ranges hash",
        ));
    }

    // signatureAlgorithm SEQUENCE
    let (_, sig_alg_val, si_rest) = der_parse(si_rest)
        .map_err(|e| PdfError::write_error(format!("SI signatureAlgorithm: {e}")))?;
    let (_, rsa_oid_val, _) =
        der_parse(sig_alg_val).map_err(|e| PdfError::write_error(format!("SI sigAlg OID: {e}")))?;
    if rsa_oid_val != OID_RSA_ENCRYPTION {
        return Err(PdfError::write_error(
            "unsupported signature algorithm (expected RSA)",
        ));
    }

    // signature OCTET STRING
    let (_, sig_bytes, _) =
        der_parse(si_rest).map_err(|e| PdfError::write_error(format!("SI signature: {e}")))?;

    // Locate the signer certificate stored in SignedData [0].
    let cert_der = find_certificate(ctx0_val)?;
    let signer_name = extract_common_name(cert_der);

    // Extract the RSA public key from the certificate and verify.
    use x509_cert::der::{Decode, Encode};
    let cert = x509_cert::Certificate::from_der(cert_der)
        .map_err(|e| PdfError::write_error(format!("cert parse: {e}")))?;
    let spki_der = cert
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|e| PdfError::write_error(format!("SPKI encode: {e}")))?;

    use rsa::pkcs8::DecodePublicKey;
    let pub_key = rsa::RsaPublicKey::from_public_key_der(&spki_der)
        .map_err(|e| PdfError::write_error(format!("RSA public key: {e}")))?;

    use rsa::pkcs1v15::VerifyingKey;
    use rsa::signature::Verifier;
    let verifying_key = VerifyingKey::<sha2::Sha256>::new(pub_key);
    let sig = rsa::pkcs1v15::Signature::try_from(sig_bytes)
        .map_err(|e| PdfError::write_error(format!("RSA signature parse: {e}")))?;
    verifying_key
        .verify(&signed_attrs_set, &sig)
        .map_err(|e| PdfError::write_error(format!("RSA verify failed: {e}")))?;

    Ok(signer_name)
}

/// Scan `attrs` (body of a signedAttrs SET) for the `messageDigest` attribute.
fn find_message_digest(attrs: &[u8]) -> Result<[u8; 32]> {
    use super::cms::{der_parse, OID_MESSAGE_DIGEST};
    let mut cur = attrs;
    while !cur.is_empty() {
        let (tag, attr_val, rest) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("attrs parse: {e}")))?;
        cur = rest;
        if tag != 0x30 {
            continue;
        }
        let (_, oid_bytes, attr_rest) =
            der_parse(attr_val).map_err(|e| PdfError::write_error(format!("attr OID: {e}")))?;
        if oid_bytes == OID_MESSAGE_DIGEST {
            // SET { OCTET STRING(hash) }
            let (_, set_val, _) =
                der_parse(attr_rest).map_err(|e| PdfError::write_error(format!("md SET: {e}")))?;
            let (_, hash_val, _) =
                der_parse(set_val).map_err(|e| PdfError::write_error(format!("md OCTET: {e}")))?;
            if hash_val.len() != 32 {
                return Err(PdfError::write_error(
                    "messageDigest is not 32 bytes (SHA-256)",
                ));
            }
            let mut out = [0u8; 32];
            out.copy_from_slice(hash_val);
            return Ok(out);
        }
    }
    Err(PdfError::write_error("messageDigest attribute not found"))
}

/// Find the first certificate DER stored in the SignedData `[0] IMPLICIT` block.
///
/// `signed_data_seq` is the complete SignedData SEQUENCE bytes (tag + length + body).
/// In our builder, `[0]` stores the raw cert DER directly (not inside a SET).
fn find_certificate(signed_data_seq: &[u8]) -> Result<&[u8]> {
    use super::cms::der_parse;
    let (_, sd_body, _) =
        der_parse(signed_data_seq).map_err(|e| PdfError::write_error(format!("SD seq: {e}")))?;
    let mut cur = sd_body;
    while !cur.is_empty() {
        let (tag, val, rest) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("SD field: {e}")))?;
        cur = rest;
        if tag == 0xa0 {
            // val is the cert DER directly (our builder does `der_ctx0(cert_der)`).
            return Ok(val);
        }
    }
    Err(PdfError::write_error(
        "no [0] certificates block in SignedData",
    ))
}

/// Skip over optional context-tagged fields (tags 0xa0 and 0xa1) in a stream.
fn skip_context_tags(data: &[u8]) -> &[u8] {
    use super::cms::der_parse;
    let mut cur = data;
    while let Some(&tag) = cur.first() {
        if tag != 0xa0 && tag != 0xa1 {
            break;
        }
        match der_parse(cur) {
            Ok((_, _, rest)) => cur = rest,
            Err(_) => break,
        }
    }
    cur
}

/// Best-effort extraction of the Subject CN from a certificate DER.
fn extract_common_name(cert_der: &[u8]) -> Option<String> {
    use x509_cert::der::Decode;
    let cert = x509_cert::Certificate::from_der(cert_der).ok()?;
    for rdn in cert.tbs_certificate.subject.0.iter() {
        for atv in rdn.0.iter() {
            // CN OID is 2.5.4.3
            if atv.oid.to_string() == "2.5.4.3" {
                // Try UTF-8 string first, then PrintableString.
                use x509_cert::der::asn1::{PrintableStringRef, Utf8StringRef};
                if let Ok(s) = atv.value.decode_as::<Utf8StringRef<'_>>() {
                    return Some(s.as_str().to_owned());
                }
                if let Ok(s) = atv.value.decode_as::<PrintableStringRef<'_>>() {
                    return Some(s.as_str().to_owned());
                }
            }
        }
    }
    None
}
