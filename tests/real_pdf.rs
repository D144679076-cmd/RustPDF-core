//! Integration tests against real PDF fixture files.

use pdf_core::{PdfDocument, PdfError, PdfObject};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn fixture_path(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

fn load_fixture(name: &str) -> PdfDocument {
    let path = fixture_path(name);
    let data = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e));
    PdfDocument::parse(data).unwrap_or_else(|e| panic!("failed to parse fixture {}: {}", name, e))
}

// ---------------------------------------------------------------------------
// Auto-discovery: every .pdf in tests/fixtures/ must parse without panic
// ---------------------------------------------------------------------------

#[test]
fn all_fixtures_parse_successfully() {
    let dir = fixtures_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read fixtures dir {}: {}", dir.display(), e));

    let mut count = 0;
    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("pdf") {
            let data = std::fs::read(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            match PdfDocument::parse(data) {
                Ok(doc) => {
                    let pages = doc.page_count().unwrap_or(0);
                    eprintln!("  OK: {} ({} pages)", name, pages);
                }
                Err(PdfError::Encrypted { .. }) => {
                    eprintln!("  SKIP (encrypted): {}", name);
                }
                Err(e) => {
                    panic!("FAILED to parse {}: {}", name, e);
                }
            }
            count += 1;
        }
    }
    assert!(count > 0, "no .pdf files found in {}", dir.display());
    eprintln!("  Total: {} fixture PDFs parsed successfully", count);
}

// ---------------------------------------------------------------------------
// minimal.pdf — single page, no content streams
// ---------------------------------------------------------------------------

#[test]
fn minimal_pdf_parses_successfully() {
    let _doc = load_fixture("minimal.pdf");
}

#[test]
fn minimal_pdf_has_one_page() {
    let doc = load_fixture("minimal.pdf");
    assert_eq!(doc.page_count().unwrap(), 1);
}

#[test]
fn minimal_pdf_has_catalog() {
    let doc = load_fixture("minimal.pdf");
    let root_ref = doc.trailer.get("Root").expect("trailer missing /Root");
    let catalog = doc.resolve(root_ref).unwrap();
    let dict = catalog.as_dict().expect("catalog is not a dict");
    assert_eq!(dict.get("Type"), Some(&PdfObject::Name("Catalog".into())));
}

#[test]
fn minimal_pdf_page_has_mediabox() {
    let doc = load_fixture("minimal.pdf");
    let root = doc.resolve(doc.trailer.get("Root").unwrap()).unwrap();
    let pages_ref = root.as_dict().unwrap().get("Pages").unwrap();
    let pages = doc.resolve(pages_ref).unwrap();
    let kids = match pages.as_dict().unwrap().get("Kids").unwrap() {
        PdfObject::Array(a) => a,
        _ => panic!("Kids is not an array"),
    };
    let page = doc.resolve(&kids[0]).unwrap();
    let mediabox = page
        .as_dict()
        .unwrap()
        .get("MediaBox")
        .expect("no MediaBox");
    match mediabox {
        PdfObject::Array(arr) => {
            assert_eq!(arr.len(), 4);
            assert_eq!(arr[0], PdfObject::Integer(0));
            assert_eq!(arr[1], PdfObject::Integer(0));
            assert_eq!(arr[2], PdfObject::Integer(612));
            assert_eq!(arr[3], PdfObject::Integer(792));
        }
        _ => panic!("MediaBox is not an array"),
    }
}

// ---------------------------------------------------------------------------
// multipage.pdf — 3 pages with uncompressed content streams
// ---------------------------------------------------------------------------

#[test]
fn multipage_pdf_parses_successfully() {
    let _doc = load_fixture("multipage.pdf");
}

#[test]
fn multipage_pdf_has_three_pages() {
    let doc = load_fixture("multipage.pdf");
    assert_eq!(doc.page_count().unwrap(), 3);
}

#[test]
fn multipage_pdf_content_streams_decode() {
    let doc = load_fixture("multipage.pdf");
    for stream_id in 7..=9 {
        let data = doc.get_stream_data(stream_id).unwrap();
        let text = String::from_utf8(data).expect("content stream is not UTF-8");
        assert!(
            text.contains("Tj"),
            "stream {} missing text operator",
            stream_id
        );
    }
}

#[test]
fn multipage_pdf_page1_content_says_page_1() {
    let doc = load_fixture("multipage.pdf");
    let data = doc.get_stream_data(7).unwrap();
    let text = String::from_utf8(data).unwrap();
    assert!(text.contains("Page 1"));
}

// ---------------------------------------------------------------------------
// with_stream.pdf — single page with FlateDecode content stream
// ---------------------------------------------------------------------------

#[test]
fn with_stream_pdf_parses_successfully() {
    let _doc = load_fixture("with_stream.pdf");
}

#[test]
fn with_stream_pdf_has_one_page() {
    let doc = load_fixture("with_stream.pdf");
    assert_eq!(doc.page_count().unwrap(), 1);
}

#[test]
fn with_stream_pdf_decodes_flate_content() {
    let doc = load_fixture("with_stream.pdf");
    let data = doc.get_stream_data(4).unwrap();
    let text = String::from_utf8(data).expect("decoded stream is not UTF-8");
    assert!(text.contains("Hello, PDF World!"));
}

#[test]
fn with_stream_pdf_has_font_resource() {
    let doc = load_fixture("with_stream.pdf");
    let font = doc.get_object(5).unwrap();
    let dict = font.as_dict().expect("font obj is not a dict");
    assert_eq!(dict.get("Type"), Some(&PdfObject::Name("Font".into())));
    assert_eq!(
        dict.get("BaseFont"),
        Some(&PdfObject::Name("Helvetica".into()))
    );
}

// ---------------------------------------------------------------------------
// Rendering — requires `render` feature
// ---------------------------------------------------------------------------

#[cfg(feature = "render")]
mod render_tests {
    use super::*;
    use pdf_core::render::render_page_rgba;

    #[test]
    fn render_minimal_page_returns_rgba_bytes() {
        let doc = load_fixture("minimal.pdf");
        let (w, h, rgba) = render_page_rgba(&doc, 0, 1.0).unwrap();
        assert!(w > 0, "rendered width must be positive");
        assert!(h > 0, "rendered height must be positive");
        assert_eq!(
            rgba.len(),
            (w * h * 4) as usize,
            "rgba buffer must be width * height * 4 bytes"
        );
    }

    #[test]
    fn render_with_stream_page_is_not_all_white() {
        let doc = load_fixture("with_stream.pdf");
        let (w, h, rgba) = render_page_rgba(&doc, 0, 1.0).unwrap();
        assert!(w > 0 && h > 0);
        // At least one non-white pixel expected (text or path was rendered).
        let all_white = rgba
            .chunks_exact(4)
            .all(|px| px[0] == 255 && px[1] == 255 && px[2] == 255);
        assert!(!all_white, "rendered page should have non-white content");
    }

    #[test]
    fn render_multipage_second_page() {
        let doc = load_fixture("multipage.pdf");
        let (w, h, rgba) = render_page_rgba(&doc, 1, 1.0).unwrap();
        assert_eq!(rgba.len(), (w * h * 4) as usize);
    }

    #[test]
    fn render_at_2x_scale_doubles_dimensions() {
        let doc = load_fixture("minimal.pdf");
        let (w1, h1, _) = render_page_rgba(&doc, 0, 1.0).unwrap();
        let (w2, h2, _) = render_page_rgba(&doc, 0, 2.0).unwrap();
        assert_eq!(w2, w1 * 2);
        assert_eq!(h2, h1 * 2);
    }

    /// Group-3.pdf contains ExtGState /SMask entries (PPTX-exported gradients).
    /// Verify: renders without panic, produces non-white pixels.
    #[test]
    fn render_group3_with_soft_mask_does_not_panic() {
        let path = fixtures_dir().join("Group-3.pdf");
        if !path.exists() {
            return; // fixture absent in CI; skip
        }
        let data = std::fs::read(&path).unwrap();
        let doc = PdfDocument::parse(data).unwrap();
        let (w, h, rgba) = render_page_rgba(&doc, 0, 1.0).unwrap();
        assert!(w > 0 && h > 0, "zero-size output from Group-3.pdf");
        // Page must have non-white pixels — soft mask transparency makes content visible.
        let all_white = rgba
            .chunks_exact(4)
            .all(|px| px[0] == 255 && px[1] == 255 && px[2] == 255);
        assert!(
            !all_white,
            "Group-3.pdf rendered all-white (soft mask may have erased content)"
        );
    }

    /// Render the first page of each PPTX-exported fixture and verify the output
    /// is not a single solid colour (which would indicate that background shapes
    /// are covering all content due to clip or compositing bugs).
    #[test]
    fn render_pptx_fixtures_not_solid_color() {
        let pptx_fixtures = ["Group-3.pdf", "Laspeyres_and_Paasche.pdf", "Unit_1.pdf"];
        for name in &pptx_fixtures {
            let path = fixtures_dir().join(name);
            if !path.exists() {
                continue;
            }
            let data = std::fs::read(&path).unwrap();
            let doc = PdfDocument::parse(data)
                .unwrap_or_else(|e| panic!("parse failed for {}: {}", name, e));
            let (w, h, rgba) = render_page_rgba(&doc, 0, 1.0)
                .unwrap_or_else(|e| panic!("render failed for {}: {}", name, e));
            assert!(w > 0 && h > 0, "{}: zero-size output", name);
            // Check that the page is not a single flat colour.  A solid-colour
            // page means background shapes have obliterated all content.
            let first = [rgba[0], rgba[1], rgba[2]];
            let all_one_color = rgba.chunks_exact(4).all(|px| {
                (px[0] as i32 - first[0] as i32).abs() < 8
                    && (px[1] as i32 - first[1] as i32).abs() < 8
                    && (px[2] as i32 - first[2] as i32).abs() < 8
            });
            assert!(
                !all_one_color,
                "{}: page rendered as a single flat colour (likely clip/compositing bug)",
                name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// AES-256 encrypted PDF — requires `crypto` feature
// ---------------------------------------------------------------------------

#[cfg(feature = "crypto")]
#[test]
fn aes256_encrypted_pdf_opens_with_password() {
    let data = include_bytes!("fixtures/encrypted_aes256.pdf").to_vec();
    let doc = PdfDocument::parse_with_password(data, b"test").unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);
}

#[cfg(feature = "crypto")]
#[test]
fn aes256_encrypted_pdf_wrong_password_fails() {
    let data = include_bytes!("fixtures/encrypted_aes256.pdf").to_vec();
    let result = PdfDocument::parse_with_password(data, b"wrong");
    assert!(result.is_err());
}

#[cfg(all(feature = "crypto", feature = "writer"))]
#[test]
fn aes256_encrypted_pdf_build_edit_session_succeeds() {
    // Regression test: build_edit_session must decrypt content streams before
    // decompressing them. Previously it called s.decode() on still-encrypted
    // bytes, producing a flate error that silently broke text edit mode.
    let data = include_bytes!("fixtures/encrypted_aes256.pdf").to_vec();
    let doc = PdfDocument::parse_with_password(data, b"test").unwrap();
    let result = pdf_core::editor::build_edit_session(&doc, 0);
    assert!(
        result.is_ok(),
        "build_edit_session failed on encrypted PDF: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[cfg(feature = "crypto")]
#[test]
fn unencrypted_pdf_has_no_permission_restrictions() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let doc = PdfDocument::parse(data).unwrap();
    // Unencrypted → no Encrypt dict → permissions() is None (no restrictions).
    assert!(doc.permissions().is_none());
}

#[cfg(feature = "crypto")]
#[test]
fn restricted_pdf_permissions_parsed() {
    let data = include_bytes!("fixtures/restricted.pdf").to_vec();
    let doc = PdfDocument::parse_with_password(data, b"user").unwrap();
    let perms = doc
        .permissions()
        .expect("encrypted doc must have permissions");
    // P = -3904: all permission bits clear.
    assert!(!perms.can_modify, "modify should be denied");
    assert!(!perms.can_annotate, "annotate should be denied");
    assert!(!perms.can_copy_text, "copy_text should be denied");
    assert!(!perms.can_fill_forms, "fill_forms should be denied");
    assert!(!perms.can_assemble, "assemble should be denied");
    assert!(!perms.can_print, "print should be denied");
}

#[cfg(feature = "crypto")]
#[test]
fn aes256_encrypted_pdf_permissions_parsed() {
    // encrypted_aes256.pdf was generated with P_FLAGS = -3904 (all deny).
    let data = include_bytes!("fixtures/encrypted_aes256.pdf").to_vec();
    let doc = PdfDocument::parse_with_password(data, b"test").unwrap();
    let perms = doc
        .permissions()
        .expect("encrypted doc must have permissions");
    assert!(!perms.can_modify);
    assert!(!perms.can_annotate);
    assert!(!perms.can_copy_text);
    assert!(!perms.can_fill_forms);
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn garbage_bytes_fail_to_parse() {
    let data = b"this is not a pdf file at all".to_vec();
    assert!(PdfDocument::parse(data).is_err());
}

#[test]
fn truncated_pdf_fails() {
    let path = fixture_path("minimal.pdf");
    let full = std::fs::read(&path).unwrap();
    let truncated = full[..full.len() / 2].to_vec();
    assert!(PdfDocument::parse(truncated).is_err());
}
