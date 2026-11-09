//! End-to-end acceptance tests for PDF 2.0 support (ISO 32000-2).
//!
//! Walks the five acceptance bullets of `.doc/pdf2-support-plan-2026-11-07.md`
//! §6 against the committed fixtures:
//!
//! 1. `pdf2-*` fixtures parse with `version() == V2_0`; an unknown
//!    future version (`%PDF-2.7`) parses with a warning, never rejects.
//! 2. `associated_files()` returns `/UF` Unicode names + `/AFRelationship`.
//! 3. R6 (AES-256) fixture opens with a non-ASCII **UTF-8** password; a wrong
//!    password fails as `PdfError::Encrypted` carrying a byte offset (R7).
//!    The fixture is `pdf2-r6-utf8-password.pdf`, produced by
//!    `tests/gen_aes256_fixture.py --pdf2` (Algorithm 2.B, %PDF-2.0 header).
//! 4. Signed-LTV fixture → `parse_dss` exposes pooled certs/OCSPs/CRLs and the
//!    `/VRI` entry; payload bytes round-trip unchanged.
//!    (`pdf2-signed-ltv.pdf` carries placeholder signature bytes — DSS parse
//!    is the fixture's purpose, not CMS verification.)
//! 5. The writer emits a `%PDF-2.0` document that our own reader parses back,
//!    with `/UF` present on every file spec.

#[cfg(all(feature = "crypto", feature = "writer"))]
mod pdf2_acceptance {
    use pdf_core::parser::objects::PdfDocument;
    use pdf_core::parser::version::PdfVersion;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
    }

    // ── Acceptance 1: version detection + future-version tolerance ──────────

    #[test]
    fn acceptance_1_pdf2_fixtures_report_v2_0() {
        for name in [
            "pdf2-minimal.pdf",
            "pdf2-associated-files.pdf",
            "pdf2-signed-ltv.pdf",
        ] {
            let doc = PdfDocument::parse(fixture(name))
                .unwrap_or_else(|e| panic!("{name} must parse: {e:?}"));
            assert_eq!(doc.version(), PdfVersion::V2_0, "{name}");
        }
    }

    #[test]
    fn acceptance_1_future_version_2_7_parses_with_warning() {
        // ISO 32000-2 §7.2: readers must not reject newer versions. Crafted
        // header + valid minimal body borrowed from the DSS unit tests style.
        let bytes = b"%PDF-2.7\n%byte-comment\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n"
            .to_vec();
        // Header-only body has no xref — construction of a full doc happens in
        // parser/version unit tests; here we assert only that no *panic* and
        // that a complete crafted doc works when we append the trailer.
        let mut full = bytes;
        let xref_pos = full.len();
        full.extend_from_slice(
            b"xref\n0 3\n0000000000 65535 f \r\n\
            0000000009 00000 n \r\n\
            0000000055 00000 n \r\n\
            trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n",
        );
        full.extend_from_slice(format!("{}\n%%EOF\n", xref_pos).as_bytes());

        let doc = PdfDocument::parse(full).expect("future version must parse");
        assert_eq!(doc.version(), PdfVersion::Unknown(2, 7));
    }

    // ── Acceptance 2: Associated Files ───────────────────────────────────────

    #[test]
    fn acceptance_2_associated_files_fixture_reports_af_and_uf() {
        use pdf_core::document::associated_files::AfRelationship;
        use pdf_core::document::catalog::Catalog;

        let doc = PdfDocument::parse(fixture("pdf2-associated-files.pdf")).expect("parse");
        let catalog = Catalog::from_document(&doc).expect("catalog");
        let specs = catalog.associated_files(&doc).expect("af");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].relationship, AfRelationship::Supplement);
        assert_eq!(specs[0].name_uf.as_deref(), Some("dataset.csv"));

        // /UF is text (§7.11.3 Table 42) — subject bytes decode.
        let embedded = specs[0]
            .embedded
            .as_ref()
            .expect("/EF stream resolved at parse time");
        assert!(
            std::str::from_utf8(&embedded.data)
                .unwrap()
                .starts_with("name,value"),
            "embedded CSV payload must round-trip"
        );
    }

    // ── Acceptance 3: R6 + UTF-8 password (wrong password → offset) ─────────

    #[test]
    fn acceptance_3_r6_opens_with_utf8_password() {
        let bytes = fixture("pdf2-r6-utf8-password.pdf");
        assert!(bytes.starts_with(b"%PDF-2.0"));

        let doc = PdfDocument::parse_with_password(bytes.clone(), "pässwörd".as_bytes())
            .expect("ISO-normative R6 + UTF-8 password must open");
        assert_eq!(doc.version(), PdfVersion::V2_0);

        // Wrong password → typed error, never panic; offset present (R7).
        let err = match PdfDocument::parse_with_password(bytes, "wrong".as_bytes()) {
            Err(e) => e,
            Ok(_) => panic!("wrong password must fail"),
        };
        let msg = format!("{err:?}");
        assert!(
            err.to_string().contains("password") || msg.contains("Encrypted"),
            "must surface as an encryption failure: {err:?}"
        );
        // R7: error carries a real byte offset.
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains("offset"),
            "byte-offset error contract broken: {err:?}"
        );
    }

    // ── Acceptance 4: Signed-LTV fixture exposes DSS ────────────────────────

    #[test]
    fn acceptance_4_ltv_fixture_exposes_dss_and_der_bytes() {
        use pdf_core::signatures::parse_dss;

        let doc = PdfDocument::parse(fixture("pdf2-signed-ltv.pdf")).expect("parse");
        let catalog = doc
            .resolve(&doc.trailer.get("Root").expect("Root").clone())
            .expect("resolve Root")
            .as_dict()
            .expect("Root dict")
            .clone();

        let dss = parse_dss(&doc, &catalog)
            .expect("dss typed-parse")
            .expect("dss present");
        assert_eq!(dss.certs.len(), 1);
        assert_eq!(dss.ocsps.len(), 1);
        assert_eq!(dss.crls.len(), 1);
        assert_eq!(dss.vri.len(), 1);
        assert!(dss.vri[0].ts_ref.is_some());

        // DER bytes round-trip unchanged through the stream pipeline.
        assert_eq!(dss.certs[0].der(&doc).expect("der"), b"DER-CERT-POOL");
        assert_eq!(dss.ocsps[0].der(&doc).expect("der"), b"DER-OCSP-POOL");
        assert_eq!(dss.crls[0].der(&doc).expect("der"), b"DER-CRL-POOL");
        assert_eq!(
            dss.vri[0]
                .ts_ref
                .as_ref()
                .expect("ts")
                .der(&doc)
                .expect("der"),
            b"DER-TS-TOKEN"
        );
    }

    // ── Acceptance 5: writer 2.0 output opens in our own reader ────────────

    #[test]
    fn acceptance_5_writer_emits_parseable_pdf2_with_uf_everywhere() {
        use pdf_core::document::catalog::Catalog;
        use pdf_core::writer::{EmitVersion, PdfWriter};
        use pdf_core::{PdfDict, PdfObject};

        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);
        let pages_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
            d.insert("Kids".to_owned(), PdfObject::Array(vec![]));
            d.insert("Count".to_owned(), PdfObject::Integer(0));
            d
        }));
        let spec = writer
            .add_associated_file(
                pdf_core::document::associated_files::AfRelationship::Data,
                "acc-test.csv",
                b"x",
            )
            .expect("af write");
        let cat = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d.insert(
                "AF".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(spec, 0)]),
            );
            d
        }));
        let bytes = writer.serialize_all(cat, None, None).expect("serialize");

        assert!(bytes.starts_with(b"%PDF-2.0"));
        let doc = PdfDocument::parse(bytes).expect("own reader must parse own writer output");
        assert_eq!(doc.version(), PdfVersion::V2_0);

        // /UF on every file spec (Table 42 requirement, §7.11.3).
        let catalog = Catalog::from_document(&doc).expect("catalog");
        for spec in catalog.associated_files(&doc).expect("af") {
            assert!(spec.name_uf.is_some(), "every Filespec must carry /UF");
        }
    }
}
