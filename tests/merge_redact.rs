//! Integration tests for PDF merge and redaction.

/// Activate a Pro license once for the process so the gated merge/redact
/// functions pass the license check. Idempotent — safe to call from every test.
#[cfg(feature = "crypto")]
fn ensure_pro_license() {
    use pdf_core::license::{activate, encode_license_key, validate_license_key, Tier};
    let key = encode_license_key(Tier::Pro, 0, "integration-test");
    let license = validate_license_key(&key).expect("test key must be valid");
    let _ = activate(license); // ignore error if already activated
}

#[cfg(not(feature = "crypto"))]
fn ensure_pro_license() {}

#[cfg(feature = "writer")]
mod redact_tests {
    use pdf_core::editor::{apply_redactions, PdfEditor, RedactZone};
    use pdf_core::parser::objects::PdfDocument;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .expect("fixture not found")
    }

    #[test]
    fn redact_produces_parseable_pdf() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let zones = [RedactZone::new(0, [0.0, 0.0, 100.0, 100.0])];
        let out = apply_redactions(&mut editor, &zones).unwrap();
        PdfDocument::parse(out).expect("redacted PDF must be parseable");
    }

    #[test]
    fn redacted_pdf_starts_with_header() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let zones = [RedactZone::new(0, [0.0, 0.0, 100.0, 100.0])];
        let out = apply_redactions(&mut editor, &zones).unwrap();
        assert!(out.starts_with(b"%PDF-1.7"));
    }

    #[test]
    fn redacted_page_count_unchanged() {
        super::ensure_pro_license();
        let data = load("multipage.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(data).unwrap();
        let zones = [RedactZone::new(0, [50.0, 50.0, 200.0, 200.0])];
        let out = apply_redactions(&mut editor, &zones).unwrap();
        let doc = PdfDocument::parse(out).unwrap();
        assert_eq!(doc.page_count().unwrap(), before);
    }

    #[test]
    fn save_new_round_trips_page_count() {
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(data).unwrap();
        let out = editor.save_new().unwrap();
        let doc = PdfDocument::parse(out).unwrap();
        assert_eq!(doc.page_count().unwrap(), before);
    }
}

#[cfg(feature = "writer")]
mod extract_tests {
    use pdf_core::editor::extract_pages;
    use pdf_core::parser::objects::PdfDocument;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .expect("fixture not found")
    }

    #[test]
    fn extract_one_page_from_multipage() {
        super::ensure_pro_license();
        let data = load("multipage.pdf");
        let result = extract_pages(data, 0..1).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn extract_range_correct_count() {
        super::ensure_pro_license();
        let data = load("multipage.pdf");
        let result = extract_pages(data, 0..2).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), 2);
    }

    #[test]
    fn extracted_pdf_starts_with_pdf_header() {
        super::ensure_pro_license();
        let data = load("multipage.pdf");
        let result = extract_pages(data, 1..2).unwrap();
        assert!(result.starts_with(b"%PDF-"));
    }

    #[test]
    fn extract_out_of_bounds_errors() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let result = extract_pages(data, 0..5);
        assert!(result.is_err());
    }

    #[test]
    fn extract_all_pages_preserves_count() {
        super::ensure_pro_license();
        let data = load("multipage.pdf");
        let total = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let result = extract_pages(data, 0..total).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), total);
    }

    #[test]
    fn extract_empty_range_errors() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let result = extract_pages(data, 0..0);
        assert!(result.is_err());
    }

    #[test]
    fn extract_single_page_from_single_page_pdf() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let result = extract_pages(data, 0..1).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }
}

#[cfg(feature = "writer")]
mod merge_tests {
    use pdf_core::editor::MergeBuilder;
    use pdf_core::parser::objects::PdfDocument;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .expect("fixture not found")
    }

    #[test]
    fn merge_empty_sources_errors() {
        super::ensure_pro_license();
        let err = MergeBuilder::new().merge().unwrap_err();
        assert!(
            format!("{}", err).contains("at least one source")
                || matches!(err, pdf_core::PdfError::InvalidStructure { .. })
        );
    }

    #[test]
    fn merge_single_source_preserves_page_count() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let merged = MergeBuilder::new()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        let doc = PdfDocument::parse(merged).unwrap();
        assert_eq!(doc.page_count().unwrap(), before);
    }

    #[test]
    fn merge_two_copies_doubles_page_count() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let merged = MergeBuilder::new()
            .add_source(data.clone())
            .unwrap()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        let doc = PdfDocument::parse(merged).unwrap();
        assert_eq!(doc.page_count().unwrap(), before * 2);
    }

    #[test]
    fn merge_three_copies_correct_count() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let merged = MergeBuilder::new()
            .add_source(data.clone())
            .unwrap()
            .add_source(data.clone())
            .unwrap()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        let doc = PdfDocument::parse(merged).unwrap();
        assert_eq!(doc.page_count().unwrap(), before * 3);
    }

    #[test]
    fn merged_pdf_starts_with_header() {
        super::ensure_pro_license();
        let data = load("minimal.pdf");
        let merged = MergeBuilder::new()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        assert!(merged.starts_with(b"%PDF-1.7"));
    }

    #[test]
    fn merged_pdf_has_valid_xref() {
        super::ensure_pro_license();
        // A parseable result implies a valid startxref + xref table.
        let data = load("minimal.pdf");
        let merged = MergeBuilder::new()
            .add_source(data.clone())
            .unwrap()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        // Should parse without error.
        PdfDocument::parse(merged).expect("merged PDF must be parseable");
    }
}
