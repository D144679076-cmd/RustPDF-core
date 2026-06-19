//! uniffi bridge — Phase 4b (Swift / iOS) and Phase 4c (Kotlin / Android).
//!
//! Uses uniffi proc-macro annotations to expose pdf-core to Swift and Kotlin.
//! Language bindings are generated at SDK-build time with:
//!
//! ```bash
//! # Build the native library first:
//! cargo build --release --features mobile
//!
//! # Generate Swift bindings (XCFramework step):
//! uniffi-bindgen generate --library target/release/libpdf_core.dylib \
//!     --language swift --out-dir mobile/ios/Sources/PdfCore
//!
//! # Generate Kotlin bindings (AAR step):
//! uniffi-bindgen generate --library target/release/libpdf_core.so \
//!     --language kotlin --out-dir mobile/android/src/main/kotlin/com/example/pdfcore
//! ```
//!
//! The UDL file (`src/ffi/pdf_core.udl`) provides a human-readable API
//! reference and can also be used directly with `uniffi-bindgen generate`
//! against the UDL file instead of the compiled library.

use std::sync::Arc;

use crate::document::catalog::Catalog;
use crate::document::page::Page;
use crate::parser::objects::PdfDocument as CoreDocument;
use crate::text::extractor::TextExtractor;
#[cfg(feature = "search")]
use crate::text::search::search_document as core_search;

// ── error ──────────────────────────────────────────────────────────────────

/// Cross-language error type exposed via uniffi.
///
/// Matches the `[Error] enum PdfError` in `pdf_core.udl`.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PdfError {
    #[error("PDF parse error: {message}")]
    ParseError { message: String },
    #[error("PDF write error: {message}")]
    WriteError { message: String },
    #[error("feature requires a Pro or Enterprise license")]
    LicenseRequired,
    #[error("document is encrypted")]
    Encrypted,
    #[error("{message}")]
    Other { message: String },
}

impl From<crate::error::PdfError> for PdfError {
    fn from(e: crate::error::PdfError) -> Self {
        match e {
            crate::error::PdfError::Encrypted { .. } => PdfError::Encrypted,
            crate::error::PdfError::WriteError { message } => PdfError::WriteError { message },
            crate::error::PdfError::LicenseRequired { .. } => PdfError::LicenseRequired,
            other => PdfError::ParseError {
                message: other.to_string(),
            },
        }
    }
}

// ── SearchResult ───────────────────────────────────────────────────────────

/// A single search match. Matches `dictionary SearchResult` in `pdf_core.udl`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SearchResult {
    /// 0-based index of the page containing this match.
    pub page_index: u64,
    /// The matched text.
    pub text: String,
    /// Bounding box `[x1, y1, x2, y2]` in PDF user-space (origin bottom-left).
    pub bounds: Vec<f64>,
}

// ── PdfDocument ────────────────────────────────────────────────────────────

/// A parsed PDF document. Matches `interface PdfDocument` in `pdf_core.udl`.
///
/// uniffi wraps this in `Arc<PdfDocument>` at the FFI boundary so it is
/// reference-counted across Swift and Kotlin.
#[derive(uniffi::Object)]
pub struct PdfDocument {
    inner: Arc<CoreDocument>,
}

#[uniffi::export]
impl PdfDocument {
    /// Return the total number of pages.
    pub fn page_count(&self) -> Result<u64, PdfError> {
        let catalog = Catalog::from_document(&self.inner)?;
        Ok(catalog.page_count as u64)
    }

    /// Extract plain text from the page at `page_index` (0-based).
    pub fn extract_text(&self, page_index: u64) -> Result<String, PdfError> {
        let catalog = Catalog::from_document(&self.inner)?;
        let page_dict = catalog.get_page_dict(&self.inner, page_index as usize)?;
        let page = Page::from_dict(&self.inner, &page_dict)?;
        let extractor = TextExtractor::extract_from_page(&self.inner, &page)?;
        Ok(extractor.plain_text())
    }

    /// Return document metadata as a compact JSON object string.
    ///
    /// Keys: `title`, `author`, `subject`, `keywords`, `creator`, `producer`,
    /// `creation_date`, `mod_date`, `trapped`. Values are strings or `null`.
    pub fn get_metadata_json(&self) -> Result<String, PdfError> {
        let meta = crate::document::metadata::Metadata::from_document(&self.inner)?;
        Ok(super::c_api::metadata_to_json(&meta))
    }
}

// ── namespace functions ────────────────────────────────────────────────────

/// Parse a PDF from raw bytes. Matches `parse_document` in `pdf_core.udl`.
#[uniffi::export]
pub fn parse_document(data: Vec<u8>) -> Result<Arc<PdfDocument>, PdfError> {
    let doc = CoreDocument::parse(data)?;
    Ok(Arc::new(PdfDocument {
        inner: Arc::new(doc),
    }))
}

/// Parse an encrypted PDF with a password. Requires the `crypto` feature.
/// Matches `parse_document_with_password` in `pdf_core.udl`.
#[cfg(feature = "crypto")]
#[uniffi::export]
pub fn parse_document_with_password(
    data: Vec<u8>,
    password: Vec<u8>,
) -> Result<Arc<PdfDocument>, PdfError> {
    let doc = CoreDocument::parse_with_password(data, &password)?;
    Ok(Arc::new(PdfDocument {
        inner: Arc::new(doc),
    }))
}

/// Search all pages of `doc` for `query`.
/// Matches `search_document` in `pdf_core.udl`.
#[uniffi::export]
pub fn search_document(
    doc: Arc<PdfDocument>,
    query: String,
    case_sensitive: bool,
) -> Result<Vec<SearchResult>, PdfError> {
    #[cfg(feature = "search")]
    {
        let results = core_search(&doc.inner, &query, case_sensitive)?;
        Ok(results
            .into_iter()
            .map(|r| SearchResult {
                page_index: r.page_index as u64,
                text: r.text,
                bounds: r.bounds.to_vec(),
            })
            .collect())
    }
    #[cfg(not(feature = "search"))]
    {
        let _ = (doc, query, case_sensitive);
        Err(PdfError::LicenseRequired)
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_bytes() -> Vec<u8> {
        include_bytes!("../../tests/fixtures/minimal.pdf").to_vec()
    }

    #[test]
    fn parse_document_happy_path() {
        let doc = parse_document(minimal_bytes()).expect("parse failed");
        let count = doc.page_count().expect("page_count failed");
        assert!(count >= 1);
    }

    #[test]
    fn parse_document_bad_bytes_returns_err() {
        assert!(parse_document(b"not a pdf".to_vec()).is_err());
    }

    #[test]
    fn get_metadata_json_is_valid_object() {
        let doc = parse_document(minimal_bytes()).expect("parse failed");
        let json = doc.get_metadata_json().expect("metadata failed");
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"title\""));
    }

    #[test]
    fn extract_text_page_zero() {
        let doc = parse_document(minimal_bytes()).expect("parse failed");
        // minimal.pdf may or may not have content streams — either outcome is acceptable.
        let _ = doc.extract_text(0);
    }

    #[test]
    fn extract_text_out_of_range_returns_err() {
        let doc = parse_document(minimal_bytes()).expect("parse failed");
        assert!(doc.extract_text(9999).is_err());
    }

    #[cfg(feature = "search")]
    #[test]
    fn search_empty_query_returns_empty_vec() {
        let doc = parse_document(minimal_bytes()).expect("parse");
        let results = search_document(doc, String::new(), false).expect("search");
        assert!(results.is_empty());
    }
}
