//! Document Security Store (DSS) — ISO 32000-2 §12.8.4 (read-only).
//!
//! PDF 2.0 moved Long-Term Validation (LTV) — previously an Adobe extension —
//! into the normative base spec. A conforming signed PDF carries its
//! revocation material inside the document (`Catalog /DSS`) so a verifier can
//! validate signatures *offline*:
//!
//! - `/Certs` — DER `X.509` certificate streams
//! - `/OCSPs` — DER OCSP response streams (RFC 6960)
//! - `/CRLs` — DER CRL streams (RFC 5280)
//! - `/VRI` — per-signature supplemental material, keyed by the hex SHA-1
//!   digest of the signature's `/Contents`; each entry adds `/Cert`, `/OCSP`,
//!   `/CRL` arrays and `/TS` (signature time-stamp, RFC 3161).
//!
//! This module **parses and exposes** the envelopes only — lazily, so even
//! multi-MB CRL pools are not inflated unless the caller asks for a payload.
//! Cryptographic *validation* of OCSP/CRL contents is out of scope here
//! (plan §7: separate phase); the existing CMS verifier consumes these bytes
//! as optional revocation material at that later stage.
//!
//! No-Panic policy (R1): missing `/DSS` → `Ok(None)`; malformed *types* →
//! `Err` (callers attached to the harmless-open path should degrade to
//! `None` themselves — see `Catalog` doc in `.doc/pdf2-phase-d-2026-11-07.md`);
//! unknown dict keys are skipped with a warning.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

/// One DER payload referenced by the DSS.
///
/// Indirect object references stay lazy: multi-MB revocation caches
/// (CRLs especially) are resolved only when [`DssItem::der`] is called.
#[derive(Debug, Clone, PartialEq)]
pub enum DssPayload {
    /// An indirect stream object — the common case in real LTV documents.
    Reference { id: u32 },
    /// A direct inline stream — rare, but legal.
    Inline { der: Vec<u8> },
}

/// A lazily-resolved DER blob entry in `/Certs`, `/OCSPs`, `/CRLs` or a `VRI`.
#[derive(Debug, Clone, PartialEq)]
pub struct DssItem {
    /// Byte object's content bytes, stream object, or inline stream.
    pub payload: DssPayload,
}

impl DssItem {
    /// The DER bytes of this item: indirect streams via
    /// [`PdfDocument::get_stream_data`] (filters + encryption applied),
    /// inline streams decoded/carp already plain bytes.
    pub fn der(&self, doc: &PdfDocument) -> Result<Vec<u8>> {
        match &self.payload {
            DssPayload::Reference { id } => doc.get_stream_data(*id),
            DssPayload::Inline { der } => Ok(der.clone()),
        }
    }
}

/// Per-signature supplemental revocation material (`/VRI` entry, §12.8.4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VriInfo {
    /// The dict key: hex SHA-1 of the signature `/Contents` (uppercase, per
    /// convention; presented verbatim).
    pub signature_hash_hex: String,
    /// `/Cert` — additional certificates scoped to this signature.
    pub cert_refs: Vec<DssItem>,
    /// `/OCSP` — supplemental OCSP responses.
    pub ocsp_refs: Vec<DssItem>,
    /// `/CRL` — supplemental CRLs.
    pub crl_refs: Vec<DssItem>,
    /// `/TS` — RFC 3161 time-stamp token for the signature time.
    pub ts_ref: Option<DssItem>,
}

/// The parsed `Catalog /DSS` dict (ISO 32000-2 §12.8.4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DocumentSecurityStore {
    /// `/Certs` — pooled certificates shared by all signatures.
    pub certs: Vec<DssItem>,
    /// `/OCSPs` — pooled OCSP responses.
    pub ocsps: Vec<DssItem>,
    /// `/CRLs` — pooled CRLs.
    pub crls: Vec<DssItem>,
    /// `/VRI` — per-signature supplemental material (unordered).
    pub vri: Vec<VriInfo>,
}

/// Parse `/DSS` from the document catalog (document-root dict `doc` and
/// resolved catalog dict). Returns `Ok(None)` when the document carries no
/// DSS (the overwhelming default); broken `/DSS` *types* yield `Err`.
pub fn parse_dss(doc: &PdfDocument, catalog: &PdfDict) -> Result<Option<DocumentSecurityStore>> {
    let Some(dss_ref) = catalog.get("DSS") else {
        return Ok(None);
    };

    let resolved = doc.resolve(dss_ref)?;
    let Some(dss_dict) = resolved.as_dict().cloned() else {
        return Err(PdfError::invalid_structure(
            "/DSS does not resolve to a dictionary",
        ));
    };

    Ok(Some(parse_dss_dict(doc, &dss_dict)?))
}

/// Construct from a resolved `/DSS` dictionary itself.
fn parse_dss_dict(doc: &PdfDocument, dss: &PdfDict) -> Result<DocumentSecurityStore> {
    Ok(DocumentSecurityStore {
        certs: parse_dss_stream_array(doc, dss.get("Certs"))?,
        ocsps: parse_dss_stream_array(doc, dss.get("OCSPs"))?,
        crls: parse_dss_stream_array(doc, dss.get("CRLs"))?,
        vri: parse_vri(doc, dss.get("VRI"))?,
    })
}

/// Parse an array of stream refs into lazily-held [`DssItem`]s.
///
/// Entries that are neither references nor streams are skipped with a warning
/// (tolerated junk — revocation pool entries are freshly added on every update,
/// so partial corruption is common in the wild).
fn parse_dss_stream_array(doc: &PdfDocument, entry: Option<&PdfObject>) -> Result<Vec<DssItem>> {
    let Some(obj) = entry else {
        return Ok(Vec::new()); // absent pool key is legal
    };
    let resolved = doc.resolve(obj)?;
    let arr = match &resolved {
        PdfObject::Array(arr) => arr,
        PdfObject::Null => return Ok(Vec::new()),
        _ => {
            return Err(PdfError::invalid_structure(
                "DSS pool entry (/Certs, /OCSPs, /CRLs) is not an array",
            ))
        }
    };

    let mut items = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        // Capture the object id BEFORE resolving: a bare `Reference` must stay
        // lazy (multi-MB CRLs; also keeps `get_stream_data`'s filter+encrypt
        // pipeline for the `.der()` accessor). Only non-reference entries are
        // resolved to check for (rare) inline payloads.
        if let PdfObject::Reference(id, _) = item {
            items.push(DssItem {
                payload: DssPayload::Reference { id: *id },
            });
            continue;
        }
        let resolved_item = doc.resolve(item)?;
        match resolved_item {
            PdfObject::Stream(s) => items.push(DssItem {
                payload: DssPayload::Inline {
                    der: s.raw_data.clone(),
                },
            }),
            other => log::warn!(
                "DSS pool entry {} is not a stream ({:?}) — skipped",
                i,
                other
            ),
        }
    }
    Ok(items)
}

/// Parse the `/VRI` sub-dictionary: keys are hex digests, values are dicts
/// with optional `/Cert`, `/OCSP`, `/CRL`, `/TS` arrays.
fn parse_vri(doc: &PdfDocument, vri_entry: Option<&PdfObject>) -> Result<Vec<VriInfo>> {
    let Some(obj) = vri_entry else {
        return Ok(Vec::new()); // /VRI absent is fine — pooled material only
    };
    let resolved = doc.resolve(obj)?;
    let Some(vri_dict) = resolved.as_dict() else {
        if matches!(resolved, PdfObject::Null) {
            return Ok(Vec::new());
        }
        return Err(PdfError::invalid_structure("/VRI is not a dictionary"));
    };

    let mut out = Vec::with_capacity(vri_dict.len());
    for (hash, entry_obj) in vri_dict.iter() {
        let entry_resolved = doc.resolve(entry_obj)?;
        let entry = match entry_resolved.as_dict() {
            Some(d) => d,
            None => {
                log::warn!("/VRI entry '{}' is not a dictionary — skipped", hash);
                continue;
            }
        };
        let mut info = VriInfo {
            signature_hash_hex: hash.clone(),
            cert_refs: parse_dss_stream_array(doc, entry.get("Cert"))?,
            ocsp_refs: parse_dss_stream_array(doc, entry.get("OCSP"))?,
            crl_refs: parse_dss_stream_array(doc, entry.get("CRL"))?,
            ts_ref: None,
        };
        if let Some(ts) = entry.get("TS") {
            // /TS is a single stream ref (RFC 3161 token), NOT an array.
            // Pre-resolve check keeps the reference lazy (same rule as pools).
            info.ts_ref = match ts {
                PdfObject::Reference(id, _) => Some(DssItem {
                    payload: DssPayload::Reference { id: *id },
                }),
                ts_obj => match doc.resolve(ts_obj) {
                    Ok(PdfObject::Stream(s)) => Some(DssItem {
                        payload: DssPayload::Inline {
                            der: s.raw_data.clone(),
                        },
                    }),
                    Ok(other) => {
                        log::warn!("/VRI '/TS' of wrong kind — skipped");
                        let _ = other;
                        None
                    }
                    Err(e) => {
                        log::warn!("/VRI '/TS' unresolved: {}", e);
                        None
                    }
                },
            };
        }
        out.push(info);
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal PDF; objects must be numbered sequentially 1..n.
    /// Stream bodies are raw bytes with explicit /Length (no filters).
    fn build_pdf(objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut pdf = Vec::from(b"%PDF-2.0\n" as &[u8]);
        let mut offsets = Vec::new();
        for (i, (num, body)) in objects.iter().enumerate() {
            assert_eq!(*num, (i + 1) as u32, "objects numbered 1..n");
            offsets.push((*num, pdf.len()));
            pdf.extend_from_slice(format!("{} 0 obj\n", num).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let max_id = offsets.last().map(|(n, _)| *n).unwrap_or(0);
        let xref_pos = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", max_id + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \r\n");
        for (_, off) in &offsets {
            pdf.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                max_id + 1,
                xref_pos
            )
            .as_bytes(),
        );
        pdf
    }

    /// A raw/unfiltered stream object body.
    fn stream(dict_extras: &str, data: &str) -> String {
        format!(
            "<< {} /Length {} >>\nstream\n{}endstream",
            dict_extras,
            data.len(),
            data
        )
    }

    /// Full DSS fixture: pools + one VRI entry. Payloads are ASCII stand-ins
    /// for DER blobs (the parser treats them as opaque bytes).
    fn dss_pdf() -> Vec<u8> {
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /DSS 3 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec()),
            (
                3,
                b"<< /Certs [4 0 R] /OCSPs [5 0 R] /CRLs [6 0 R] /VRI << /A1F9BB0098CC7A3FF7DB2F9CC77D39DC720B87D6 << /Cert [4 0 R 7 0 R] /OCSP [5 0 R] /CRL [6 0 R] /TS 8 0 R >> >> >>"
                    .to_vec(),
            ),
            (4, stream("/Type /Cert", "CERT-POOL-DER").into_bytes()),
            (5, stream("/Type /OCSP", "OCSP-POOL-DER").into_bytes()),
            (6, stream("/Type /CRL", "CRL-POOL-DER").into_bytes()),
            (7, stream("/Type /Cert", "VRI-SPECIFIC-DER").into_bytes()),
            (8, stream("/Type /TimeStamp", "TS-TOKEN-DER").into_bytes()),
        ])
    }

    fn catalog_dict(doc: &PdfDocument) -> PdfDict {
        doc.resolve(&doc.trailer.get("Root").unwrap().clone())
            .unwrap()
            .as_dict()
            .unwrap()
            .clone()
    }

    // --- happy path -----------------------------------------------------------

    #[test]
    fn test_pools_and_vri_parsed_lazily() {
        let doc = PdfDocument::parse(dss_pdf()).unwrap();
        let cat = catalog_dict(&doc);
        let dss = parse_dss(&doc, &cat).unwrap().expect("DSS present");

        assert_eq!(dss.certs.len(), 1);
        assert_eq!(dss.ocsps.len(), 1);
        assert_eq!(dss.crls.len(), 1);
        assert_eq!(dss.vri.len(), 1);
        let vri = &dss.vri[0];
        assert_eq!(
            vri.signature_hash_hex,
            "A1F9BB0098CC7A3FF7DB2F9CC77D39DC720B87D6"
        );
        assert_eq!(
            vri.cert_refs.len(),
            2,
            "shared pool cert + one VRI-scoped cert"
        );
        assert_eq!(vri.ocsp_refs.len(), 1);
        assert_eq!(vri.crl_refs.len(), 1);
        assert!(vri.ts_ref.is_some());
        // Streams are lazily-held references — no inflation yet.
        assert!(
            matches!(dss.certs[0].payload, DssPayload::Reference { id: 4 }),
            "pool entry stays a lazy reference until .der() is called"
        );
    }

    #[test]
    fn test_der_bytes_decode_through_stream_pipeline() {
        let doc = PdfDocument::parse(dss_pdf()).unwrap();
        let dss = parse_dss(&doc, &catalog_dict(&doc)).unwrap().unwrap();
        assert_eq!(dss.certs[0].der(&doc).unwrap(), b"CERT-POOL-DER");
        assert_eq!(dss.ocsps[0].der(&doc).unwrap(), b"OCSP-POOL-DER");
        assert_eq!(dss.crls[0].der(&doc).unwrap(), b"CRL-POOL-DER");
        let vri_ts = dss.vri[0].ts_ref.clone().unwrap();
        assert_eq!(vri_ts.der(&doc).unwrap(), b"TS-TOKEN-DER");
    }

    #[test]
    fn test_inline_stream_entry() {
        // TODO(follow-up): inline stream entries (direct stream objects inside
        // the pool array) are rare; the parser handles them via the
        // `PdfObject::Stream` arm of `parse_dss_stream_array`, but the fixture
        // lexer has no way to place a bare stream token inside an array
        // literal. Cover via real-world LTV fixture in Phase F.
    }

    // --- missing / degradation paths -------------------------------------------

    #[test]
    fn test_no_dss_is_none() {
        // Same fixture without /DSS on the catalog.
        let doc = PdfDocument::parse(build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec()),
        ]))
        .unwrap();
        assert!(parse_dss(&doc, &catalog_dict(&doc)).unwrap().is_none());
    }

    #[test]
    fn test_dss_not_a_dictionary_is_err() {
        // /DSS → integer: typed error so callers choosing strictness can see it.
        let doc = PdfDocument::parse(build_pdf(&[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /DSS [3 0 R] >>".to_vec(),
            ),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec()),
            (3, b"<< /Certs [3 0 R] >>".to_vec()),
        ]))
        .unwrap();
        // Array resolves to itself — still not a dict.
        let result = parse_dss(&doc, &catalog_dict(&doc));
        assert!(result.is_err());
    }

    #[test]
    fn test_pool_not_an_array_is_err() {
        let doc = PdfDocument::parse(build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /DSS 3 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec()),
            (3, b"<< /Certs (not-an-array) /VRI << >> >>".to_vec()),
        ]))
        .unwrap();
        let result = parse_dss(&doc, &catalog_dict(&doc));
        assert!(result.is_err());
    }

    #[test]
    fn test_vri_entry_not_a_dict_skipped() {
        // One good VRI entry + one bad (integer) — bad skipped, good kept.
        let doc = PdfDocument::parse(build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /DSS 3 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec()),
            (3, b"<< /VRI << /A1F9BB0098CC7A3FF7DB2F9CC77D39DC720B87D6 << /Certs [4 0 R] >> /DEADBEEF 99 >> >>".to_vec()),
            (4, stream("", "X").into_bytes()),
        ]))
        .unwrap();
        let dss = parse_dss(&doc, &catalog_dict(&doc)).unwrap().unwrap();
        assert_eq!(dss.vri.len(), 1);
        assert_eq!(
            dss.vri[0].signature_hash_hex,
            "A1F9BB0098CC7A3FF7DB2F9CC77D39DC720B87D6"
        );
    }

    #[test]
    fn test_absent_pool_keys_legal() {
        // Pools absent entirely — only VRI with empty sub-arrays.
        let doc = PdfDocument::parse(build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /DSS 3 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec()),
            (3, b"<< /VRI << >> >>".to_vec()),
        ]))
        .unwrap();
        let dss = parse_dss(&doc, &catalog_dict(&doc)).unwrap().unwrap();
        assert!(
            dss.certs.is_empty()
                && dss.ocsps.is_empty()
                && dss.crls.is_empty()
                && dss.vri.is_empty()
        );
    }
}
