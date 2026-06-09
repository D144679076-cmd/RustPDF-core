//! Integration tests for the WASM bridge (native rlib target).
//!
//! These tests exercise `WasmDocument`, `WasmEditor`, and `WasmPdfWriter`
//! through the same Rust code paths that JS callers use at the WASM boundary.
//! Methods that return `js_sys::Uint8Array` are tested via the `*_bytes()`
//! Rust variants which avoid the `js-sys` dependency on native.
//!
//! Enable with `--features wasm`.

#[cfg(feature = "wasm")]
mod wasm_tests {
    use pdf_core::wasm::{WasmDocument, WasmEditor, WasmPdfWriter};
    use std::path::PathBuf;

    fn fixture(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {}", path.display(), e))
    }

    // -----------------------------------------------------------------------
    // WasmDocument
    // -----------------------------------------------------------------------

    #[test]
    fn wasm_document_parse_minimal() {
        let bytes = fixture("minimal.pdf");
        let doc = WasmDocument::parse(&bytes).expect("parse should succeed");
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn wasm_document_parse_multipage() {
        let bytes = fixture("multipage.pdf");
        let doc = WasmDocument::parse(&bytes).expect("parse should succeed");
        assert_eq!(doc.page_count().unwrap(), 3);
    }

    #[test]
    fn wasm_document_get_metadata_returns_json_object() {
        let bytes = fixture("minimal.pdf");
        let doc = WasmDocument::parse(&bytes).unwrap();
        let meta = doc.get_metadata();
        assert!(
            meta.starts_with('{'),
            "metadata must be a JSON object, got: {}",
            meta
        );
        assert!(meta.ends_with('}'));
    }

    #[test]
    fn wasm_document_get_outline_returns_json_array() {
        let bytes = fixture("minimal.pdf");
        let doc = WasmDocument::parse(&bytes).unwrap();
        let outline = doc.get_outline();
        assert!(
            outline.starts_with('['),
            "outline must be a JSON array, got: {}",
            outline
        );
        assert!(outline.ends_with(']'));
    }

    #[test]
    fn wasm_document_extract_text_does_not_panic() {
        let bytes = fixture("with_stream.pdf");
        let doc = WasmDocument::parse(&bytes).unwrap();
        // extract_text may return empty string if no text can be decoded;
        // the important thing is it must not panic or error.
        let _ = doc.extract_text(0).expect("text extraction must not error");
    }

    // NOTE: WasmDocument::parse() error paths call JsError::new() which panics
    // on non-WASM targets.  The error-path behaviour is verified via wasm-pack
    // tests on a real WASM target.  The equivalent Rust-native error test lives
    // in real_pdf.rs::garbage_bytes_fail_to_parse.

    #[test]
    fn wasm_document_parse_multipage_text() {
        let bytes = fixture("multipage.pdf");
        let doc = WasmDocument::parse(&bytes).unwrap();
        // All three pages must be extractable without error.
        for i in 0..3 {
            doc.extract_text(i)
                .unwrap_or_else(|e| panic!("extract_text page {} failed: {:?}", i, e));
        }
    }

    // -----------------------------------------------------------------------
    // WasmEditor
    // -----------------------------------------------------------------------

    #[test]
    fn wasm_editor_open_and_page_count() {
        let bytes = fixture("minimal.pdf");
        let editor = WasmEditor::open(&bytes).expect("open should succeed");
        assert_eq!(editor.page_count().unwrap(), 1);
    }

    #[test]
    fn wasm_editor_save_produces_valid_pdf() {
        let bytes = fixture("minimal.pdf");
        let mut editor = WasmEditor::open(&bytes).unwrap();
        let saved = editor.save_bytes().expect("save_bytes should succeed");
        let doc = pdf_core::PdfDocument::parse(saved).expect("re-parse should succeed");
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn wasm_editor_add_text_annotation_and_save() {
        let bytes = fixture("minimal.pdf");
        let mut editor = WasmEditor::open(&bytes).unwrap();
        editor
            .add_text_annotation(0, 50.0, 700.0, 200.0, 50.0, "Test note")
            .expect("add annotation should succeed");
        let saved = editor.save_bytes().unwrap();
        let doc = pdf_core::PdfDocument::parse(saved).expect("saved PDF should be valid");
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn wasm_editor_add_highlight_and_save() {
        let bytes = fixture("minimal.pdf");
        let mut editor = WasmEditor::open(&bytes).unwrap();
        let quad_points = [100.0f64, 700.0, 300.0, 700.0, 100.0, 715.0, 300.0, 715.0];
        editor
            .add_highlight(0, &quad_points, 1.0, 1.0, 0.0)
            .expect("add highlight should succeed");
        let saved = editor.save_bytes().unwrap();
        pdf_core::PdfDocument::parse(saved).expect("saved PDF should be valid");
    }

    #[test]
    fn wasm_editor_set_metadata_and_save() {
        let bytes = fixture("minimal.pdf");
        let mut editor = WasmEditor::open(&bytes).unwrap();
        editor
            .set_metadata("My Title", "My Author", "", "")
            .expect("set metadata should succeed");
        let saved = editor.save_bytes().unwrap();
        pdf_core::PdfDocument::parse(saved).expect("saved PDF should be valid");
    }

    #[test]
    fn wasm_editor_add_blank_page_increases_count() {
        let bytes = fixture("minimal.pdf");
        let mut editor = WasmEditor::open(&bytes).unwrap();
        let before = editor.page_count().unwrap();
        editor
            .add_blank_page(before, 595.0, 842.0)
            .expect("add blank page should succeed");
        let saved = editor.save_bytes().unwrap();
        let doc = pdf_core::PdfDocument::parse(saved).unwrap();
        assert_eq!(doc.page_count().unwrap(), before + 1);
    }

    #[test]
    fn wasm_editor_add_link_annotation() {
        let bytes = fixture("minimal.pdf");
        let mut editor = WasmEditor::open(&bytes).unwrap();
        editor
            .add_link(0, 100.0, 700.0, 150.0, 20.0, "https://example.com")
            .expect("add link should succeed");
        let saved = editor.save_bytes().unwrap();
        pdf_core::PdfDocument::parse(saved).expect("saved PDF should be valid");
    }

    // -----------------------------------------------------------------------
    // WasmPdfWriter
    // -----------------------------------------------------------------------

    #[test]
    fn wasm_pdf_writer_new_and_build() {
        let mut writer = WasmPdfWriter::new().expect("new writer should succeed");
        let bytes = writer.build_bytes().expect("build_bytes should succeed");
        pdf_core::PdfDocument::parse(bytes).expect("built PDF should be valid");
    }

    #[test]
    fn wasm_pdf_writer_add_page_and_build() {
        let mut writer = WasmPdfWriter::new().unwrap();
        writer
            .add_page(595.0, 842.0)
            .expect("add page should succeed");
        let bytes = writer.build_bytes().unwrap();
        let doc = pdf_core::PdfDocument::parse(bytes).expect("built PDF should be valid");
        // Template page was removed, one A4 page added → expect 1 page.
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn wasm_pdf_writer_add_multiple_pages() {
        let mut writer = WasmPdfWriter::new().unwrap();
        writer.add_page(595.0, 842.0).unwrap(); // A4
        writer.add_page(612.0, 792.0).unwrap(); // Letter
        let bytes = writer.build_bytes().unwrap();
        let doc = pdf_core::PdfDocument::parse(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 2);
    }
}
