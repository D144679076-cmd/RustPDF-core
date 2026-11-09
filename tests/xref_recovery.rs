//! Integration tests for XRef recovery (`parser::objects::recovery`).
//!
//! Fixtures are corrupted variants of `minimal.pdf`:
//! - `corrupt_startxref.pdf` — `startxref` points past end of file.
//! - `corrupt_offsets.pdf`   — XRef table parses but one entry offset is wrong;
//!   `/Root` marker verification rejects the built index.
//! - `no_xref.pdf`           — XRef table, `startxref` and `trailer` all
//!   missing; full body-scan reconstruction is the only path.

use pdf_core::{PdfDocument, PdfObject};

#[test]
fn recovers_stale_startxref() {
    let data = include_bytes!("fixtures/corrupt_startxref.pdf");
    let doc = PdfDocument::parse(data.to_vec()).expect("stale startxref must be recovered");
    assert_eq!(
        doc.trailer.get("Root"),
        Some(&PdfObject::Reference(1, 0)),
        "trailer /Root must survive recovery"
    );
    assert!(doc.get_object(1).is_ok(), "catalog object missing");
}

#[test]
fn recovers_wrong_table_offset() {
    let data = include_bytes!("fixtures/corrupt_offsets.pdf");
    let doc = PdfDocument::parse(data.to_vec())
        .expect("bad entry offset must be recovered via body scan");
    assert_eq!(
        doc.trailer.get("Root"),
        Some(&PdfObject::Reference(1, 0)),
        "trailer /Root must survive recovery"
    );
    assert!(doc.get_object(1).is_ok(), "catalog object missing");
}

#[test]
fn recovers_missing_xref_entirely() {
    let data = include_bytes!("fixtures/no_xref.pdf");
    let doc = PdfDocument::parse(data.to_vec())
        .expect("missing xref table must be recovered via body scan");
    // Object 1 (catalog) and object 2 (pages) must both be indexed.
    assert!(doc.get_object(1).is_ok(), "catalog object missing");
    assert!(doc.get_object(2).is_ok(), "pages object missing");
}
