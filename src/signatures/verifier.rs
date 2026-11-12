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
    /// RFC 3161 timestamp `genTime` (string as encoded, e.g. GeneralizedTime),
    /// when the CMS carries an `id-aa-timeStampToken` unsigned attribute.
    /// Untrusted metadata — the TSA token's own signature is not validated
    /// until the chain-validation phase (plan §7).
    pub timestamp: Option<String>,
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
            timestamp: None,
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

    // Hash the two signed byte ranges (digest chosen from the signer's
    // `digestAlgorithm` inside `verify_cms_signed_data`).
    match verify_cms_signed_data(
        cms_der,
        &doc_bytes[r1s as usize..(r1s + r1l) as usize],
        &doc_bytes[r2s as usize..(r2s + r2l) as usize],
    ) {
        Ok((signer_name, timestamp)) => Ok(Some(SignatureVerification {
            field_name,
            signature_valid: true,
            covers_whole_file,
            signer_name,
            timestamp,
            error: None,
        })),
        Err(e) => Ok(Some(SignatureVerification {
            field_name,
            signature_valid: false,
            covers_whole_file,
            signer_name: None,
            timestamp: None,
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
        timestamp: None,
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

/// Verify the CMS `SignedData` `cms_der` over the two signed byte ranges
/// `range1`/`range2`.
///
/// Uses the structured CMS parser (`cms_parse`) instead of hand-walking DER.
/// The digest is selected from the signer's `digestAlgorithm` (SHA-256/384/512;
/// SHA-1 is rejected as deprecated). Returns the signer's common name and, when
/// present, the RFC 3161 timestamp `genTime` (untrusted until chain validation).
fn verify_cms_signed_data(
    cms_der: &[u8],
    range1: &[u8],
    range2: &[u8],
) -> Result<(Option<String>, Option<String>)> {
    use super::cms::{
        der_set, OID_RSA_ENCRYPTION, OID_SHA1WITH_RSA, OID_SHA256WITH_RSA, OID_SHA384WITH_RSA,
        OID_SHA512WITH_RSA,
    };
    use super::cms_parse::{parse_cms_signed_data, timestamp_gen_time};

    let parsed = parse_cms_signed_data(cms_der)?;

    // Digest from the signer's digestAlgorithm.
    use sha2::{Digest, Sha256, Sha384, Sha512};
    let embedded_hash: Vec<u8> = match parsed.signer.digest_algorithm {
        super::cms::OID_SHA256 => Sha256::digest([range1, range2].concat()).to_vec(),
        super::cms::OID_SHA384 => Sha384::digest([range1, range2].concat()).to_vec(),
        super::cms::OID_SHA512 => Sha512::digest([range1, range2].concat()).to_vec(),
        super::cms::OID_SHA1 => {
            return Err(PdfError::write_error(
                "signature uses deprecated SHA-1 digest — rejected",
            ))
        }
        _ => {
            return Err(PdfError::write_error(
                "unsupported digest algorithm in signerInfo",
            ))
        }
    };

    let signed_attrs_val = parsed
        .signer
        .signed_attrs
        .ok_or_else(|| PdfError::write_error("expected [0] signedAttrs tag in SignerInfo"))?;
    // RFC 5652 §5.4: replace [0] IMPLICIT with SET tag for hashing.
    let signed_attrs_set = der_set(signed_attrs_val);

    // Verify the embedded messageDigest matches our computed hash.
    let md = parsed
        .signer
        .message_digest
        .ok_or_else(|| PdfError::write_error("messageDigest attribute not found"))?;
    if md != embedded_hash {
        return Err(PdfError::write_error(
            "messageDigest does not match signed byte ranges hash",
        ));
    }

    // Signature-algorithm OID: our builder emits rsaEncryption; real-world
    // signers typically emit sha*WithRSAEncryption. Reject sha1WithRSA.
    let sig_alg = parsed.signer.signature_algorithm;
    if sig_alg == OID_SHA1WITH_RSA {
        return Err(PdfError::write_error(
            "signature uses deprecated sha1WithRSAEncryption — rejected",
        ));
    }
    if sig_alg != OID_RSA_ENCRYPTION
        && sig_alg != OID_SHA256WITH_RSA
        && sig_alg != OID_SHA384WITH_RSA
        && sig_alg != OID_SHA512WITH_RSA
    {
        return Err(PdfError::write_error(
            "unsupported signature algorithm (expected RSA PKCS#1v15)",
        ));
    }

    // Signer certificate: first entry of the parsed certificate pool (the
    // builder emits the signer's own cert first). Limitation logged: sid
    // (issuer/serial) matching against the pool is chain-validation work.
    let cert_der = parsed
        .certificates
        .first()
        .copied()
        .ok_or_else(|| PdfError::write_error("no certificate in SignedData [0] block"))?;
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
    let sig = rsa::pkcs1v15::Signature::try_from(parsed.signer.signature)
        .map_err(|e| PdfError::write_error(format!("RSA signature parse: {e}")))?;
    let sig_alg = sig_alg.to_vec();
    let digest_alg = parsed.signer.digest_algorithm.to_vec();
    match (sig_alg.as_slice(), digest_alg.as_slice()) {
        (OID_SHA384WITH_RSA, _) | (_, super::cms::OID_SHA384) => {
            VerifyingKey::<sha2::Sha384>::new(pub_key)
                .verify(&signed_attrs_set, &sig)
                .map_err(|e| PdfError::write_error(format!("RSA verify failed: {e}")))
        }
        (OID_SHA512WITH_RSA, _) | (_, super::cms::OID_SHA512) => {
            VerifyingKey::<sha2::Sha512>::new(pub_key)
                .verify(&signed_attrs_set, &sig)
                .map_err(|e| PdfError::write_error(format!("RSA verify failed: {e}")))
        }
        // rsaEncryption or sha256WithRSA → SHA-256 (the default digest here).
        _ => VerifyingKey::<sha2::Sha256>::new(pub_key)
            .verify(&signed_attrs_set, &sig)
            .map_err(|e| PdfError::write_error(format!("RSA verify failed: {e}"))),
    }?;

    // RFC 3161 timestamp token: surface genTime when present (untrusted until
    // the TSA token itself is validated in the chain-validation phase).
    let timestamp = match parsed.signer.timestamp_token {
        Some(token) => timestamp_gen_time(token).unwrap_or_else(|e| {
            log::debug!("timestamp token present but unparsable: {e}");
            None
        }),
        None => None,
    };

    Ok((signer_name, timestamp))
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
