//! Structured parser for PKCS#7 / CMS `SignedData` (RFC 5652).
//!
//! Companion to [`super::cms`] (the builder): walks a complete `ContentInfo`
//! DER and extracts the certificate pool, CRL pool and the first signer's
//! entry — everything the verifier needs for signature math, chain validation
//! and revocation checking. Handles both builder shapes (bare cert directly in
//! `[0]` IMPLICIT, and `SET OF Certificate` chains) plus the `[a1]`
//! `id-aa-timeStampToken` unsigned attribute (RFC 3161).

use super::cms::{
    der_parse, der_parse_len, OID_AA_TIMESTAMP_TOKEN, OID_CT_TST_INFO, OID_MESSAGE_DIGEST,
    OID_SIGNED_DATA,
};
use crate::error::{PdfError, Result};

/// A signer's entry from the `SignerInfos` SET of a parsed CMS `SignedData`.
pub(crate) struct ParsedSignerInfo<'a> {
    /// `digestAlgorithm` OID value bytes (content of the OID TLV) — determines
    /// the hash used for the `messageDigest` attribute.
    pub digest_algorithm: &'a [u8],
    /// `signatureAlgorithm` OID value bytes (content of the OID TLV).
    pub signature_algorithm: &'a [u8],
    /// Raw `signature` OCTET STRING value.
    pub signature: &'a [u8],
    /// Content of the `[0] IMPLICIT` signedAttrs — the value bytes of the SET
    /// whose complete TLV encoding is what the signer actually signed
    /// (RFC 5652 §5.4: `[0] IMPLICIT` replaces the SET tag; re-wrap with
    /// [`der_set`] to obtain the signed bytes). `None` when absent.
    pub signed_attrs: Option<&'a [u8]>,
    /// The `messageDigest` attribute value, if present in signedAttrs.
    pub message_digest: Option<&'a [u8]>,
    /// RFC 3161 `TimeStampToken` ContentInfo DER from the unsigned attrs, if
    /// an `id-aa-timeStampToken` attribute is present.
    pub timestamp_token: Option<&'a [u8]>,
}

/// A parsed CMS `ContentInfo` wrapping a detached `SignedData`.
pub(crate) struct ParsedCms<'a> {
    /// All certificates from the `[0]` block (signer + chain), as DER slices.
    pub certificates: Vec<&'a [u8]>,
    /// All CRLs from the `[1]` block, as DER slices. Consumed by the
    /// revocation check in the upcoming chain-validation phase (plan §7) —
    /// extracted now so re-parsing isn't needed then.
    #[allow(dead_code)]
    pub crls: Vec<&'a [u8]>,
    /// The first signer's entry.
    pub signer: ParsedSignerInfo<'a>,
}

/// Compare an OID *value* (content bytes of an OID TLV) with a constant OID
/// encoding from this module (which stores content bytes, not the TLV).
fn oid_value_eq(oid_value: &[u8], const_oid: &[u8]) -> bool {
    oid_value == const_oid
}

/// Walk the `[0] IMPLICIT` certificates block, yielding each certificate DER.
///
/// Handles both shapes the builder emits: a bare certificate TLV directly
/// inside `[0]` (legacy single-cert) and a `SET OF Certificate` TLV
/// (multi-cert chain). An `[0] EXPLICIT` wrapping (real-world producers) is
/// handled the same way — the outer value is one more TLV to unwrap.
fn collect_certificates<'a>(ctx_val: &'a [u8], out: &mut Vec<&'a [u8]>) -> Result<()> {
    let mut cur = ctx_val;
    while !cur.is_empty() {
        let (tag, val, rest) = der_parse(cur)?;
        // Push the *complete* TLV (tag + length + value) — consumers decode
        // the slice directly with x509, so the bytes must be self-contained
        // DER. `cur[..cur.len() - rest.len()]` is exactly that extent.
        let tlv = &cur[..cur.len() - rest.len()];
        cur = rest;
        match tag {
            0x31 => {
                // SET/SEQUENCE OF CertificateChoices — recurse one level.
                let mut inner = val;
                while !inner.is_empty() {
                    let (itag, _ival, irest) = der_parse(inner)?;
                    let itlv = &inner[..inner.len() - irest.len()];
                    inner = irest;
                    if itag == 0x30 {
                        out.push(itlv);
                    }
                }
            }
            0x30 => out.push(tlv), // Certificate SEQUENCE (complete TLV)
            0xa0..=0xa3 => {
                // Nested context block (e.g. [0] EXPLICIT inside [0] IMPLICIT).
                collect_certificates(val, out)?;
            }
            _ => {
                return Err(PdfError::write_error(format!(
                    "unexpected tag {tag:#04x} in certificates block"
                )))
            }
        }
    }
    Ok(())
}

/// Parse a complete CMS `ContentInfo` wrapping a detached `SignedData`.
///
/// Extracts the certificate pool, CRL pool and the first signer's entry —
/// everything the verifier needs for signature math, chain validation and
/// revocation checking, without re-walking the DER by hand.
pub(crate) fn parse_cms_signed_data(cms_der: &[u8]) -> Result<ParsedCms<'_>> {
    // ContentInfo ::= SEQUENCE { contentType OID, [0] EXPLICIT content }
    let (tag, ci_val, _) =
        der_parse(cms_der).map_err(|e| PdfError::write_error(format!("CMS ContentInfo: {e}")))?;
    if tag != 0x30 {
        return Err(PdfError::write_error(format!(
            "CMS: expected ContentInfo SEQUENCE, got tag {tag:#04x}"
        )));
    }
    let (tag, oid_val, ci_rest) =
        der_parse(ci_val).map_err(|e| PdfError::write_error(format!("CMS contentType: {e}")))?;
    if tag != 0x06 || !oid_value_eq(oid_val, OID_SIGNED_DATA) {
        return Err(PdfError::write_error(
            "CMS: contentType is not id-signedData",
        ));
    }
    let (tag, ctx_val, _) =
        der_parse(ci_rest).map_err(|e| PdfError::write_error(format!("CMS [0] content: {e}")))?;
    if tag != 0xa0 {
        return Err(PdfError::write_error("CMS: missing [0] EXPLICIT content"));
    }

    // SignedData ::= SEQUENCE { version, digestAlgorithms, encapContentInfo,
    //   [0] certificates, [1] crls, signerInfos }
    let (tag, sd_val, _) = der_parse(ctx_val)
        .map_err(|e| PdfError::write_error(format!("CMS SignedData seq: {e}")))?;
    if tag != 0x30 {
        return Err(PdfError::write_error("CMS: SignedData is not a SEQUENCE"));
    }
    let mut sd = sd_val;
    // version INTEGER, digestAlgorithms SET, encapContentInfo SEQUENCE —
    // shape-checked, values unused.
    for what in ["version", "digestAlgorithms", "encapContentInfo"] {
        let (_, _, rest) =
            der_parse(sd).map_err(|e| PdfError::write_error(format!("SD {what}: {e}")))?;
        sd = rest;
    }

    // Optional [0] certificates and [1] crls.
    let mut certificates = Vec::new();
    let mut crls: Vec<&[u8]> = Vec::new();
    while !sd.is_empty() {
        let (tag, val, rest) =
            der_parse(sd).map_err(|e| PdfError::write_error(format!("SD fields: {e}")))?;
        sd = rest;
        match tag {
            0xa0 => collect_certificates(val, &mut certificates)?,
            0xa1 => {
                // [1] IMPLICIT SET OF CertificateRevocationLists (or
                // [1] EXPLICIT wrapping). Each CRL is a CertificateList
                // SEQUENCE (0x30) — pushed as a complete TLV.
                let mut cur = val;
                while !cur.is_empty() {
                    let (t, v, r) = der_parse(cur)
                        .map_err(|e| PdfError::write_error(format!("SD crls: {e}")))?;
                    let tlv = &cur[..cur.len() - r.len()];
                    cur = r;
                    match t {
                        0x31 => {
                            let mut inner = v;
                            while !inner.is_empty() {
                                let (it, _iv, ir) = der_parse(inner)
                                    .map_err(|e| PdfError::write_error(format!("crl set: {e}")))?;
                                let itlv = &inner[..inner.len() - ir.len()];
                                inner = ir;
                                if it == 0x30 {
                                    crls.push(itlv);
                                }
                            }
                        }
                        0x30 => crls.push(tlv),
                        _ => {}
                    }
                }
            }
            0x31 => {
                // signerInfos SET OF SignerInfo.
                let (tag, si_val, _) = der_parse(val)
                    .map_err(|e| PdfError::write_error(format!("signerInfo: {e}")))?;
                if tag != 0x30 {
                    return Err(PdfError::write_error("signerInfo is not a SEQUENCE"));
                }
                let signer = parse_signer_info(si_val)?;
                return Ok(ParsedCms {
                    certificates,
                    crls,
                    signer,
                });
            }
            _ => {}
        }
    }
    Err(PdfError::write_error("CMS: missing signerInfos"))
}

/// Parse one `SignerInfo` SEQUENCE value.
fn parse_signer_info(si_val: &[u8]) -> Result<ParsedSignerInfo<'_>> {
    let mut cur = si_val;
    // version, sid — shape-checked, values unused.
    for what in ["version", "sid"] {
        let (_, _, rest) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("SignerInfo {what}: {e}")))?;
        cur = rest;
    }
    // digestAlgorithm (AlgorithmIdentifier SEQUENCE containing the digest OID).
    let (da_tag, da_val, rest) =
        der_parse(cur).map_err(|e| PdfError::write_error(format!("digestAlgorithm: {e}")))?;
    cur = rest;
    if da_tag != 0x30 {
        return Err(PdfError::write_error("digestAlgorithm is not a SEQUENCE"));
    }
    let (oid_tag, digest_oid, _) = der_parse(da_val)
        .map_err(|e| PdfError::write_error(format!("digestAlgorithm OID: {e}")))?;
    if oid_tag != 0x06 {
        return Err(PdfError::write_error("digestAlgorithm: missing OID"));
    }
    // Optional [0] IMPLICIT signedAttrs (tag 0xa0).
    let mut signed_attrs: Option<&[u8]> = None;
    if matches!(cur.first(), Some(&0xa0)) {
        let (_, val, rest) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("signedAttrs: {e}")))?;
        cur = rest;
        signed_attrs = Some(val);
    }
    // signatureAlgorithm (SEQUENCE containing the OID).
    let (tag, alg_val, rest) =
        der_parse(cur).map_err(|e| PdfError::write_error(format!("signatureAlgorithm: {e}")))?;
    cur = rest;
    if tag != 0x30 {
        return Err(PdfError::write_error(
            "signatureAlgorithm is not a SEQUENCE",
        ));
    }
    let (oid_tag, oid_val, _) = der_parse(alg_val)
        .map_err(|e| PdfError::write_error(format!("signatureAlgorithm OID: {e}")))?;
    if oid_tag != 0x06 {
        return Err(PdfError::write_error("signatureAlgorithm: missing OID"));
    }
    // signature OCTET STRING.
    let (sig_tag, sig_val, rest) =
        der_parse(cur).map_err(|e| PdfError::write_error(format!("signature: {e}")))?;
    cur = rest;
    if sig_tag != 0x04 {
        return Err(PdfError::write_error("signature is not an OCTET STRING"));
    }
    // Optional [1] IMPLICIT unsignedAttrs (tag 0xa1).
    let mut timestamp_token: Option<&[u8]> = None;
    if matches!(cur.first(), Some(&0xa1)) {
        let (_, val, _) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("unsignedAttrs: {e}")))?;
        timestamp_token = find_timestamp_token(val)?;
    }

    let message_digest = match signed_attrs {
        Some(attrs) => find_message_digest_value(attrs)?,
        None => None,
    };

    Ok(ParsedSignerInfo {
        digest_algorithm: digest_oid,
        signature_algorithm: oid_val,
        signature: sig_val,
        signed_attrs,
        message_digest,
        timestamp_token,
    })
}

/// Scan an unsignedAttrs SET body for the `id-aa-timeStampToken` attribute;
/// return the TimeStampToken ContentInfo DER (the element inside the attr's
/// SET OF value).
fn find_timestamp_token(attrs_body: &[u8]) -> Result<Option<&[u8]>> {
    let mut cur = attrs_body;
    while !cur.is_empty() {
        let (tag, attr_val, rest) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("unsignedAttrs: {e}")))?;
        cur = rest;
        if tag != 0x30 {
            continue;
        }
        // Attribute ::= SEQUENCE { attrType OID, attrValues SET }
        let (oid_tag, oid_val, attr_rest) =
            der_parse(attr_val).map_err(|e| PdfError::write_error(format!("attr OID: {e}")))?;
        if oid_tag != 0x06 || !oid_value_eq(oid_val, OID_AA_TIMESTAMP_TOKEN) {
            continue;
        }
        let (set_tag, set_val, _) =
            der_parse(attr_rest).map_err(|e| PdfError::write_error(format!("attr values: {e}")))?;
        if set_tag != 0x31 {
            continue;
        }
        // The value is one TimeStampToken (ContentInfo TLV) per RFC 3161.
        // Return the complete TLV: tag byte + parsed length prefix + value.
        if set_val.is_empty() {
            continue;
        }
        let ci_tag = set_val[0];
        let (ci_len, ci_skip) = der_parse_len(&set_val[1..])?;
        let ci_end = 1 + ci_skip + ci_len;
        if ci_tag == 0x30 && ci_end <= set_val.len() {
            return Ok(Some(&set_val[..ci_end]));
        }
    }
    Ok(None)
}

/// Scan a signedAttrs SET body for the `messageDigest` OCTET STRING value.
fn find_message_digest_value(attrs_body: &[u8]) -> Result<Option<&[u8]>> {
    let mut cur = attrs_body;
    while !cur.is_empty() {
        let (tag, attr_val, rest) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("attrs parse: {e}")))?;
        cur = rest;
        if tag != 0x30 {
            continue;
        }
        let (oid_tag, oid_val, attr_rest) =
            der_parse(attr_val).map_err(|e| PdfError::write_error(format!("attr OID: {e}")))?;
        if oid_tag != 0x06 || !oid_value_eq(oid_val, OID_MESSAGE_DIGEST) {
            continue;
        }
        let (set_tag, set_val, _) =
            der_parse(attr_rest).map_err(|e| PdfError::write_error(format!("attr values: {e}")))?;
        if set_tag != 0x31 {
            continue;
        }
        let (oct_tag, oct_val, _) = der_parse(set_val)
            .map_err(|e| PdfError::write_error(format!("messageDigest value: {e}")))?;
        if oct_tag == 0x04 {
            return Ok(Some(oct_val));
        }
    }
    Ok(None)
}

// ─────────────────────────────────────────────────────────────────────────────

/// Extract the `genTime` from an RFC 3161 `TimeStampToken` ContentInfo, when
/// present and shaped as expected.
///
/// Best-effort walk: ContentInfo → [0] SignedData → encapContentInfo →
/// eContent (OCTET STRING wrapping the TSTInfo DER) → TSTInfo fields up to
/// `genTime` (GeneralizedTime, tag `0x18`). The TSA's signature is *not*
/// validated here (plan §7: chain-validation phase); callers must treat the
/// returned time as untrusted metadata until then.
pub(crate) fn timestamp_gen_time(token_der: &[u8]) -> Result<Option<String>> {
    // ContentInfo ::= SEQUENCE { contentType OID, [0] EXPLICIT content }
    let (tag, ci_val, _) =
        der_parse(token_der).map_err(|e| PdfError::write_error(format!("TST ContentInfo: {e}")))?;
    if tag != 0x30 {
        return Err(PdfError::write_error("TST: not a ContentInfo SEQUENCE"));
    }
    let (oid_tag, _oid_val, ci_rest) =
        der_parse(ci_val).map_err(|e| PdfError::write_error(format!("TST contentType: {e}")))?;
    if oid_tag != 0x06 {
        return Err(PdfError::write_error("TST: missing contentType OID"));
    }
    let (tag, ctx_val, _) =
        der_parse(ci_rest).map_err(|e| PdfError::write_error(format!("TST [0]: {e}")))?;
    if tag != 0xa0 {
        return Err(PdfError::write_error("TST: missing [0] content"));
    }
    // SignedData SEQUENCE.
    let (tag, sd_val, _) =
        der_parse(ctx_val).map_err(|e| PdfError::write_error(format!("TST SignedData: {e}")))?;
    if tag != 0x30 {
        return Err(PdfError::write_error("TST: SignedData not a SEQUENCE"));
    }
    let mut sd = sd_val;
    // version, digestAlgorithms — skipped.
    for what in ["version", "digestAlgorithms"] {
        let (_, _, rest) =
            der_parse(sd).map_err(|e| PdfError::write_error(format!("TST SD {what}: {e}")))?;
        sd = rest;
    }
    // encapContentInfo ::= SEQUENCE { eContentType OID, [0] EXPLICIT eContent }
    let (tag, eci_val, _) =
        der_parse(sd).map_err(|e| PdfError::write_error(format!("TST encapContentInfo: {e}")))?;
    if tag != 0x30 {
        return Err(PdfError::write_error(
            "TST: encapContentInfo not a SEQUENCE",
        ));
    }
    let (ct_tag, ct_oid, eci_rest) =
        der_parse(eci_val).map_err(|e| PdfError::write_error(format!("TST eContentType: {e}")))?;
    if ct_tag != 0x06 || ct_oid != OID_CT_TST_INFO {
        // id-data or unknown content type — no TSTInfo to walk.
        return Ok(None);
    }
    let (tag, ec_val, _) =
        der_parse(eci_rest).map_err(|e| PdfError::write_error(format!("TST eContent: {e}")))?;
    if tag != 0xa0 {
        return Err(PdfError::write_error("TST: missing eContent"));
    }
    // eContent is [0] EXPLICIT wrapping the OCTET STRING that holds TSTInfo.
    let (tag, tst_der, _) =
        der_parse(ec_val).map_err(|e| PdfError::write_error(format!("TST eContent inner: {e}")))?;
    if tag != 0x04 {
        return Err(PdfError::write_error("TST: eContent not an OCTET STRING"));
    }
    // TSTInfo ::= SEQUENCE { version, policy, messageImprint, serialNumber, genTime, ... }
    let (tag, tst_val, _) =
        der_parse(tst_der).map_err(|e| PdfError::write_error(format!("TSTInfo: {e}")))?;
    if tag != 0x30 {
        return Err(PdfError::write_error("TSTInfo is not a SEQUENCE"));
    }
    let mut cur = tst_val;
    for what in ["version", "policy", "messageImprint", "serialNumber"] {
        let (_, _, rest) =
            der_parse(cur).map_err(|e| PdfError::write_error(format!("TSTInfo {what}: {e}")))?;
        cur = rest;
    }
    let (gt_tag, gt_val, _) =
        der_parse(cur).map_err(|e| PdfError::write_error(format!("TSTInfo genTime: {e}")))?;
    if gt_tag == 0x18 || gt_tag == 0x17 {
        std::str::from_utf8(gt_val)
            .map(|s| Some(s.to_owned()))
            .map_err(|_| PdfError::write_error("TSTInfo genTime is not UTF-8"))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signatures::cms::{
        der_ctx0, der_int_u32, der_octet, der_oid, der_seq, der_set, der_tlv,
    };

    const OID_SHA256_DIGEST: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
    const OID_ID_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01];
    const OID_ID_SIGNED_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];
    const OID_CT: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x03];
    const OID_MD: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x04];

    /// Minimal AlgorithmIdentifier: SEQ { OID(oid), NULL }.
    fn alg_id(oid: &[u8]) -> Vec<u8> {
        let mut v = der_oid(oid);
        v.extend([0x05, 0x00]);
        der_seq(&v)
    }

    /// Attribute ::= SEQ { OID, SET { value } }.
    fn attr(oid: &[u8], value: &[u8]) -> Vec<u8> {
        let mut a = der_oid(oid);
        a.extend(der_set(value));
        der_seq(&a)
    }

    fn wrap_content_info(content: &[u8], content_type: &[u8]) -> Vec<u8> {
        let mut ci = der_oid(content_type);
        ci.extend(der_ctx0(content));
        der_seq(&ci)
    }

    /// Minimal detached SignedData with one signer (attrs, no timestamp),
    /// one dummy cert, no CRLs — same shape the builder emits.
    fn build_test_cms() -> Vec<u8> {
        let cert = der_seq(b"dummy-certificate-body"); // never decoded here
        let sid = der_seq(b"issuer-and-serial");
        let attrs = {
            let mut a = attr(OID_CT, b"id-data");
            a.extend(attr(OID_MD, &der_octet(&[0xabu8; 32])));
            a
        };
        let signer = {
            let mut c = der_int_u32(1);
            c.extend(&sid);
            c.extend(&alg_id(OID_SHA256_DIGEST));
            c.extend(der_ctx0(&attrs));
            c.extend(&alg_id(OID_RSA));
            c.extend(der_octet(&[0x11u8; 16]));
            der_seq(&c)
        };
        let signed_data = {
            let mut c = der_int_u32(1);
            c.extend(der_set(&alg_id(OID_SHA256_DIGEST)));
            c.extend(der_seq(&der_oid(OID_ID_DATA)));
            c.extend(der_ctx0(&cert));
            c.extend(der_set(&signer));
            der_seq(&c)
        };
        wrap_content_info(&signed_data, OID_ID_SIGNED_DATA)
    }

    #[test]
    fn parse_happy_path() {
        let cms = build_test_cms();
        let parsed = parse_cms_signed_data(&cms).expect("parse should succeed");
        assert_eq!(parsed.certificates.len(), 1);
        assert!(parsed.crls.is_empty());
        assert_eq!(parsed.signer.message_digest, Some(&[0xabu8; 32][..]));
        assert_eq!(parsed.signer.timestamp_token, None);
        assert!(parsed.signer.signed_attrs.is_some());
        assert_eq!(parsed.signer.signature, &[0x11u8; 16]);
        assert_eq!(parsed.signer.digest_algorithm, OID_SHA256_DIGEST);
        assert_eq!(parsed.signer.signature_algorithm, OID_RSA);
    }

    #[test]
    fn parse_chain_shape_collects_all_certs() {
        // [0] IMPLICIT SET OF Certificate with two members (builder chain mode).
        let set_c = {
            let mut s = der_seq(b"cert-one-body");
            s.extend(der_seq(b"cert-two-body"));
            s
        };
        let signer = {
            let mut c = der_int_u32(1);
            c.extend(der_seq(b"sid"));
            c.extend(&alg_id(OID_SHA256_DIGEST));
            c.extend(&alg_id(OID_RSA));
            c.extend(der_octet(&[0x22u8; 16]));
            der_seq(&c)
        };
        let signed_data = {
            let mut c = der_int_u32(1);
            c.extend(der_set(&alg_id(OID_SHA256_DIGEST)));
            c.extend(der_seq(&der_oid(OID_ID_DATA)));
            c.extend(der_ctx0(&der_set(&set_c)));
            c.extend(der_set(&signer));
            der_seq(&c)
        };
        let cms = wrap_content_info(&signed_data, OID_ID_SIGNED_DATA);

        let parsed = parse_cms_signed_data(&cms).expect("parse chain shape");
        assert_eq!(parsed.certificates.len(), 2);
        assert!(parsed.signer.signed_attrs.is_none()); // no-attrs shape OK
        assert_eq!(parsed.signer.message_digest, None);
    }

    #[test]
    fn parse_timestamp_token_and_gen_time() {
        // TSTInfo with genTime 2026-11-12T10:00:00Z.
        let tst_info = {
            let mut c = der_int_u32(1);
            c.extend(der_oid(b"policy-oid"));
            c.extend(der_seq(b"message-imprint")); // SEQ { algId, OCTET }
            c.extend(der_int_u32(7));
            c.extend(der_tlv(0x18, b"20261112100000Z")); // GeneralizedTime
            der_seq(&c)
        };
        // Token SignedData: encapContentInfo carries OID_CT_TST_INFO.
        let signed_data = {
            let mut c = der_int_u32(1);
            c.extend(der_set(&alg_id(OID_SHA256_DIGEST)));
            let mut eci = der_oid(OID_CT_TST_INFO);
            eci.extend(der_ctx0(&der_octet(&tst_info)));
            c.extend(der_seq(&eci));
            c.extend(der_set(b"")); // empty signerInfos (not walked here)
            der_seq(&c)
        };
        let token = wrap_content_info(&signed_data, OID_ID_SIGNED_DATA);

        // Embed as unsignedAttrs ([1] IMPLICIT) on a signer.
        let signer = {
            let mut c = der_int_u32(1);
            c.extend(der_seq(b"sid"));
            c.extend(&alg_id(OID_SHA256_DIGEST));
            c.extend(&alg_id(OID_RSA));
            c.extend(der_octet(&[0x33u8; 16]));
            c.extend(der_tlv(0xa1, &attr(OID_AA_TIMESTAMP_TOKEN, &token)));
            der_seq(&c)
        };
        let signed_data2 = {
            let mut c = der_int_u32(1);
            c.extend(der_set(&alg_id(OID_SHA256_DIGEST)));
            c.extend(der_seq(&der_oid(OID_ID_DATA)));
            c.extend(der_ctx0(&der_seq(b"cert")));
            c.extend(der_set(&signer));
            der_seq(&c)
        };
        let cms = wrap_content_info(&signed_data2, OID_ID_SIGNED_DATA);

        let parsed = parse_cms_signed_data(&cms).expect("parse");
        let token = parsed
            .signer
            .timestamp_token
            .expect("timestamp token found");
        let gen = timestamp_gen_time(token)
            .expect("genTime walk")
            .expect("genTime value");
        assert_eq!(gen, "20261112100000Z");
    }

    #[test]
    fn parse_error_not_signed_data() {
        let cms = wrap_content_info(b"payload", OID_ID_DATA);
        let err = match parse_cms_signed_data(&cms) {
            Err(e) => e,
            Ok(_) => panic!("must reject non-SignedData"),
        };
        assert!(err.to_string().contains("id-signedData"));
    }

    #[test]
    fn parse_error_missing_signer_infos() {
        let signed_data = {
            let mut c = der_int_u32(1);
            c.extend(der_set(&alg_id(OID_SHA256_DIGEST)));
            c.extend(der_seq(&der_oid(OID_ID_DATA)));
            c.extend(der_ctx0(&der_seq(b"cert")));
            der_seq(&c)
        };
        let cms = wrap_content_info(&signed_data, OID_ID_SIGNED_DATA);
        let err = match parse_cms_signed_data(&cms) {
            Err(e) => e,
            Ok(_) => panic!("must reject missing signerInfos"),
        };
        assert!(err.to_string().contains("signerInfos"));
    }

    #[test]
    fn parse_error_truncated() {
        let cms = build_test_cms();
        assert!(
            parse_cms_signed_data(&cms[..cms.len() - 8]).is_err(),
            "truncated CMS must fail"
        );
    }

    #[test]
    fn gen_time_error_truncated_token() {
        let err = timestamp_gen_time(&[0x30, 0x82, 0x99, 0x99, 0x01, 0x02, 0x03])
            .expect_err("truncated token must fail");
        assert!(!err.to_string().is_empty());
    }
}
