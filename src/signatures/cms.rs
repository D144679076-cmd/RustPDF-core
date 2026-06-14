//! PKCS#7 / CMS SignedData builder for PDF digital signatures (RFC 5652).
//!
//! Constructs a DER-encoded CMS `ContentInfo` wrapping `SignedData` using
//! RSA-PKCS#1v15-SHA256 as the signature algorithm.

use crate::error::{PdfError, Result};

// ── Well-known OID encodings ──────────────────────────────────────────────────

/// SHA-256 digest algorithm OID: 2.16.840.1.101.3.4.2.1
pub(crate) const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
/// RSA encryption OID: 1.2.840.113549.1.1.1
pub(crate) const OID_RSA_ENCRYPTION: &[u8] =
    &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
/// id-data OID: 1.2.840.113549.1.7.1
pub(crate) const OID_ID_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01];
/// id-signedData OID: 1.2.840.113549.1.7.2
pub(crate) const OID_SIGNED_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];
/// id-contentType attribute OID: 1.2.840.113549.1.9.3
pub(crate) const OID_CONTENT_TYPE: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x03];
/// id-messageDigest attribute OID: 1.2.840.113549.1.9.4
pub(crate) const OID_MESSAGE_DIGEST: &[u8] =
    &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x04];
/// id-signingTime attribute OID: 1.2.840.113549.1.9.5
pub(crate) const OID_SIGNING_TIME: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x05];

// ── DER primitive encoding ────────────────────────────────────────────────────

/// Encode a DER length field (short or long form).
pub(crate) fn der_len(len: usize) -> Vec<u8> {
    if len < 128 {
        vec![len as u8]
    } else if len < 256 {
        vec![0x81, len as u8]
    } else if len < 65536 {
        vec![0x82, (len >> 8) as u8, (len & 0xff) as u8]
    } else {
        let b = (len as u32).to_be_bytes();
        let start = b.iter().position(|&x| x != 0).unwrap_or(3);
        let n = 4 - start;
        let mut v = vec![0x80 | n as u8];
        v.extend_from_slice(&b[start..]);
        v
    }
}

/// Encode a DER TLV (tag, length, value).
pub(crate) fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend(der_len(content.len()));
    v.extend_from_slice(content);
    v
}

pub(crate) fn der_seq(c: &[u8]) -> Vec<u8> {
    der_tlv(0x30, c)
}
pub(crate) fn der_set(c: &[u8]) -> Vec<u8> {
    der_tlv(0x31, c)
}
pub(crate) fn der_oid(bytes: &[u8]) -> Vec<u8> {
    der_tlv(0x06, bytes)
}
pub(crate) fn der_octet(data: &[u8]) -> Vec<u8> {
    der_tlv(0x04, data)
}
pub(crate) fn der_null() -> Vec<u8> {
    vec![0x05, 0x00]
}

/// Encode a positive integer from its big-endian magnitude bytes.
pub(crate) fn der_int_pos(bytes: &[u8]) -> Vec<u8> {
    let start = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len().saturating_sub(1));
    let bytes = &bytes[start..];
    let mut c = Vec::new();
    if bytes.first().copied().unwrap_or(0) >= 0x80 {
        c.push(0x00);
    }
    c.extend_from_slice(bytes);
    der_tlv(0x02, &c)
}

/// Encode a small non-negative integer.
pub(crate) fn der_int_u32(val: u32) -> Vec<u8> {
    der_int_pos(&val.to_be_bytes())
}

/// Encode with context tag [0] constructed (0xa0).
///
/// Used for both `[0] IMPLICIT` (pass the untagged content bytes) and
/// `[0] EXPLICIT` (pass the complete inner TLV). At the byte level the
/// encoding is identical; the semantic distinction is purely in what you supply.
pub(crate) fn der_ctx0(content: &[u8]) -> Vec<u8> {
    der_tlv(0xa0, content)
}

/// Encode a UTCTime DER value from a Unix timestamp.
pub(crate) fn der_utctime(unix_ts: u64) -> Vec<u8> {
    let mut s = unix_ts;
    let sec = s % 60;
    s /= 60;
    let min = s % 60;
    s /= 60;
    let hour = s % 24;
    s /= 24;
    let (yr, mo, day) = days_since_epoch(s as u32);
    let t = format!(
        "{:02}{:02}{:02}{:02}{:02}{:02}Z",
        yr % 100,
        mo,
        day,
        hour,
        min,
        sec
    );
    der_tlv(0x17, t.as_bytes())
}

fn days_since_epoch(mut d: u32) -> (u32, u32, u32) {
    let mut y = 1970u32;
    loop {
        let leap = is_leap(y);
        let yd = if leap { 366 } else { 365 };
        if d < yd {
            break;
        }
        d -= yd;
        y += 1;
    }
    let md: [u32; 12] = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u32;
    for &m in &md {
        if d < m {
            break;
        }
        d -= m;
        mo += 1;
    }
    (y, mo, d + 1)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ── DER parsing primitives ────────────────────────────────────────────────────

/// Parse a DER TLV, returning `(tag, value, remaining)`.
pub(crate) fn der_parse(data: &[u8]) -> Result<(u8, &[u8], &[u8])> {
    if data.len() < 2 {
        return Err(PdfError::write_error("DER: truncated TLV"));
    }
    let tag = data[0];
    let (len, skip) = der_parse_len(&data[1..])?;
    let start = 1 + skip;
    if data.len() < start + len {
        return Err(PdfError::write_error("DER: value exceeds buffer"));
    }
    Ok((tag, &data[start..start + len], &data[start + len..]))
}

pub(crate) fn der_parse_len(data: &[u8]) -> Result<(usize, usize)> {
    if data.is_empty() {
        return Err(PdfError::write_error("DER: empty length"));
    }
    if data[0] < 0x80 {
        return Ok((data[0] as usize, 1));
    }
    let n = (data[0] & 0x7f) as usize;
    if n == 0 || n > 4 || data.len() < 1 + n {
        return Err(PdfError::write_error("DER: invalid long-form length"));
    }
    let mut len = 0usize;
    for i in 0..n {
        len = (len << 8) | data[1 + i] as usize;
    }
    Ok((len, 1 + n))
}

// ── Public signing API ────────────────────────────────────────────────────────

/// Build a detached PKCS#7 CMS `SignedData` DER for embedding in a PDF.
///
/// - `content_hash` — SHA-256 of the signed byte ranges (excluding `/Contents`).
/// - `private_key_der` — PKCS#8 RSA private key in DER.
/// - `cert_der` — Signer X.509 certificate in DER.
/// - `signing_time` — Optional Unix timestamp; pass `None` on WASM (no clock).
///
/// Returns a DER-encoded CMS `ContentInfo` wrapping `SignedData`.
pub fn build_cms_signed_data(
    content_hash: &[u8; 32],
    private_key_der: &[u8],
    cert_der: &[u8],
    signing_time: Option<u64>,
) -> Result<Vec<u8>> {
    // 1. Parse the PKCS#8 RSA private key.
    use rsa::pkcs8::DecodePrivateKey;
    let private_key = rsa::RsaPrivateKey::from_pkcs8_der(private_key_der)
        .map_err(|e| PdfError::write_error(format!("invalid private key: {e}")))?;

    // 2. Parse the certificate to extract issuer Name DER and serial number.
    use x509_cert::der::{Decode, Encode};
    let cert = x509_cert::Certificate::from_der(cert_der)
        .map_err(|e| PdfError::write_error(format!("invalid certificate: {e}")))?;
    let issuer_der = cert
        .tbs_certificate
        .issuer
        .to_der()
        .map_err(|e| PdfError::write_error(format!("encode issuer: {e}")))?;
    let serial_int_der = der_int_pos(cert.tbs_certificate.serial_number.as_bytes());

    // 3. Build the SignedAttributes content (what goes inside [0]/SET).
    let sha256_alg_id = {
        let mut v = der_oid(OID_SHA256);
        v.extend(der_null());
        der_seq(&v)
    };
    // contentType attribute
    let ct_attr = {
        let val = der_set(&der_oid(OID_ID_DATA));
        let mut a = der_oid(OID_CONTENT_TYPE);
        a.extend(val);
        der_seq(&a)
    };
    // messageDigest attribute
    let md_attr = {
        let val = der_set(&der_octet(content_hash));
        let mut a = der_oid(OID_MESSAGE_DIGEST);
        a.extend(val);
        der_seq(&a)
    };

    let mut attrs = Vec::new();
    attrs.extend(ct_attr);
    if let Some(ts) = signing_time {
        let time_attr = {
            let val = der_set(&der_utctime(ts));
            let mut a = der_oid(OID_SIGNING_TIME);
            a.extend(val);
            der_seq(&a)
        };
        attrs.extend(time_attr);
    }
    attrs.extend(md_attr);

    // 4. DER-encode as a SET for signing (RFC 5652 §5.4 — replace [0] with SET).
    let signed_attrs_set = der_set(&attrs);

    // 5. Sign with RSA PKCS#1v15-SHA256 (deterministic).
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::Signer;
    use sha2::Sha256;
    let signing_key = SigningKey::<Sha256>::new(private_key);
    let sig_obj = signing_key.sign(&signed_attrs_set);
    use rsa::signature::SignatureEncoding;
    let sig_bytes = sig_obj.to_bytes().to_vec();

    // 6. Build IssuerAndSerialNumber.
    let iss_and_serial = {
        let mut c = issuer_der;
        c.extend(serial_int_der);
        der_seq(&c)
    };

    let rsa_alg_id = {
        let mut v = der_oid(OID_RSA_ENCRYPTION);
        v.extend(der_null());
        der_seq(&v)
    };

    // 7. Build SignerInfo (version 1, issuerAndSerialNumber sid).
    let signer_info = {
        let mut c = der_int_u32(1);
        c.extend(iss_and_serial);
        c.extend(sha256_alg_id.clone());
        c.extend(der_ctx0(&attrs)); // [0] IMPLICIT signedAttrs
        c.extend(rsa_alg_id.clone());
        c.extend(der_octet(&sig_bytes));
        der_seq(&c)
    };

    // 8. Build SignedData.
    let encap_ci = der_seq(&der_oid(OID_ID_DATA)); // no eContent — detached
    let signed_data = {
        let mut c = der_int_u32(1); // version 1
        c.extend(der_set(&sha256_alg_id)); // digestAlgorithms
        c.extend(encap_ci); // encapContentInfo
        c.extend(der_ctx0(cert_der)); // [0] IMPLICIT certificates
        c.extend(der_set(&signer_info)); // signerInfos
        der_seq(&c)
    };

    // 9. Wrap in ContentInfo.
    let content_info = {
        let mut ci = der_oid(OID_SIGNED_DATA);
        ci.extend(der_ctx0(&signed_data)); // [0] EXPLICIT content
        der_seq(&ci)
    };

    Ok(content_info)
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_len_short() {
        assert_eq!(der_len(0), vec![0x00]);
        assert_eq!(der_len(127), vec![0x7f]);
    }

    #[test]
    fn der_len_long() {
        assert_eq!(der_len(128), vec![0x81, 0x80]);
        assert_eq!(der_len(256), vec![0x82, 0x01, 0x00]);
    }

    #[test]
    fn der_int_pos_positive() {
        // 0x80 needs a 0x00 prefix to stay positive
        assert_eq!(der_int_pos(&[0x80]), vec![0x02, 0x02, 0x00, 0x80]);
        // 0x01 does not
        assert_eq!(der_int_pos(&[0x01]), vec![0x02, 0x01, 0x01]);
    }

    #[test]
    fn der_parse_roundtrip() {
        let encoded = der_seq(b"hello");
        let (tag, val, rest) = der_parse(&encoded).unwrap();
        assert_eq!(tag, 0x30);
        assert_eq!(val, b"hello");
        assert!(rest.is_empty());
    }

    #[test]
    fn utctime_smoke() {
        // Unix timestamp 0 → 700101000000Z
        let t = der_utctime(0);
        assert_eq!(t[0], 0x17); // UTCTime tag
        let s = std::str::from_utf8(&t[2..]).unwrap();
        assert!(s.ends_with('Z'));
    }
}
