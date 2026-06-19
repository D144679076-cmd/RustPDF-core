//! Raw C FFI layer — Phase 4a.
//!
//! Exposes pdf-core as a C-ABI library callable from Swift, Kotlin/JNI, C, or
//! any language with a C foreign-function interface.
//!
//! ## Ownership contract
//! - `pdf_document_parse` allocates a `Box<PdfDocument>` and returns it as an
//!   opaque `*mut c_void` handle.
//! - The caller **must** pass that handle to `pdf_document_free` exactly once.
//! - Error strings returned through `*error_out` **must** be freed with
//!   `pdf_free_string`.
//! - JSON strings returned by `pdf_document_extract_text`,
//!   `pdf_document_get_metadata`, and `pdf_document_search` **must** be freed
//!   with `pdf_free_string`.
//!
//! ## Thread safety
//! `PdfDocument` is `Send + Sync` (via `parking_lot` RwLock internals), so
//! handles may be shared across threads, but each handle must be freed from
//! exactly one thread.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_ulong};

use crate::document::catalog::Catalog;
use crate::document::metadata::Metadata;
use crate::document::page::Page;
use crate::parser::objects::PdfDocument;
use crate::text::extractor::TextExtractor;
#[cfg(feature = "search")]
use crate::text::search::{search_document, SearchResult};

// ── internal helpers ─────────────────────────────────────────────────────────

/// Write an error message through `error_out` (if non-null) and return null.
///
/// The written `*mut c_char` is a `CString`-allocated string; caller must free
/// with `pdf_free_string`.
unsafe fn write_error(error_out: *mut *mut c_char, msg: impl std::fmt::Display) {
    if !error_out.is_null() {
        let c = CString::new(msg.to_string()).unwrap_or_else(|_| {
            // SAFETY: literal is valid UTF-8 with no interior NUL.
            unsafe { CString::from_vec_unchecked(b"<error message contained NUL byte>".to_vec()) }
        });
        // SAFETY: error_out is non-null (checked above).
        unsafe { *error_out = c.into_raw() };
    }
}

/// Serialize a `Vec<SearchResult>` to a compact JSON array string.
#[cfg(feature = "search")]
fn search_results_to_json(results: &[SearchResult]) -> String {
    let mut out = String::from('[');
    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"page_index\":{},\"text\":{},\"bounds\":[{},{},{},{}]}}",
            r.page_index,
            json_string(&r.text),
            r.bounds[0],
            r.bounds[1],
            r.bounds[2],
            r.bounds[3],
        ));
    }
    out.push(']');
    out
}

/// Serialize `Metadata` to a compact JSON object string.
pub(super) fn metadata_to_json(m: &Metadata) -> String {
    let mut fields: Vec<String> = Vec::new();
    macro_rules! field {
        ($key:expr, $val:expr) => {
            if let Some(v) = $val {
                fields.push(format!("\"{}\":{}", $key, json_string(v)));
            } else {
                fields.push(format!("\"{}\":null", $key));
            }
        };
    }
    field!("title", m.title.as_deref());
    field!("author", m.author.as_deref());
    field!("subject", m.subject.as_deref());
    field!("keywords", m.keywords.as_deref());
    field!("creator", m.creator.as_deref());
    field!("producer", m.producer.as_deref());
    field!("creation_date", m.creation_date.as_deref());
    field!("mod_date", m.mod_date.as_deref());
    field!("trapped", m.trapped.as_deref());
    format!("{{{}}}", fields.join(","))
}

/// Escape a Rust `&str` as a JSON string literal (with surrounding quotes).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── public C API ─────────────────────────────────────────────────────────────

/// Parse a PDF from raw bytes.
///
/// Returns an opaque `*mut c_void` handle on success, or `NULL` on failure.
/// On failure, `*error_out` is set to a NUL-terminated UTF-8 error message
/// that the caller must free with `pdf_free_string`.
///
/// # Safety
/// - `data` must point to at least `len` readable bytes.
/// - `error_out` may be null; if non-null it must be a valid `*mut *mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn pdf_document_parse(
    data: *const u8,
    len: c_ulong,
    error_out: *mut *mut c_char,
) -> *mut std::ffi::c_void {
    // SAFETY: caller guarantees data points to len valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec();
    match PdfDocument::parse(bytes) {
        Ok(doc) => Box::into_raw(Box::new(doc)) as *mut std::ffi::c_void,
        Err(e) => {
            // SAFETY: error_out validity is caller's responsibility; write_error
            // checks for null internally.
            unsafe { write_error(error_out, e) };
            std::ptr::null_mut()
        }
    }
}

/// Parse an encrypted PDF from raw bytes using the provided password.
///
/// Returns an opaque handle or `NULL` on failure (sets `*error_out`).
///
/// Requires the `crypto` feature.
///
/// # Safety
/// - `data` must point to at least `len` readable bytes.
/// - `password` must be a NUL-terminated UTF-8 string.
/// - `error_out` may be null.
#[cfg(feature = "crypto")]
#[no_mangle]
pub unsafe extern "C" fn pdf_document_parse_with_password(
    data: *const u8,
    len: c_ulong,
    password: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut std::ffi::c_void {
    // SAFETY: caller guarantees data points to len valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec();
    // SAFETY: password is a NUL-terminated C string (caller guarantee).
    let pwd = match unsafe { CStr::from_ptr(password) }.to_str() {
        Ok(s) => s.as_bytes().to_vec(),
        Err(e) => {
            // SAFETY: write_error checks error_out for null.
            unsafe { write_error(error_out, e) };
            return std::ptr::null_mut();
        }
    };
    match PdfDocument::parse_with_password(bytes, &pwd) {
        Ok(doc) => Box::into_raw(Box::new(doc)) as *mut std::ffi::c_void,
        Err(e) => {
            // SAFETY: write_error checks error_out for null.
            unsafe { write_error(error_out, e) };
            std::ptr::null_mut()
        }
    }
}

/// Free a document handle returned by `pdf_document_parse`.
///
/// Passing `NULL` is a no-op. Passing the same handle twice is undefined
/// behaviour.
///
/// # Safety
/// - `handle` must be a value returned by `pdf_document_parse` that has not
///   previously been freed.
#[no_mangle]
pub unsafe extern "C" fn pdf_document_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        // SAFETY: handle was created by Box::into_raw(Box::new(PdfDocument)).
        drop(unsafe { Box::from_raw(handle as *mut PdfDocument) });
    }
}

/// Return the number of pages in the document, or `-1` on error.
///
/// # Safety
/// - `handle` must be a valid handle returned by `pdf_document_parse` that
///   has not been freed.
#[no_mangle]
pub unsafe extern "C" fn pdf_document_page_count(
    handle: *const std::ffi::c_void,
    error_out: *mut *mut c_char,
) -> c_int {
    // SAFETY: handle is a valid PdfDocument pointer (caller guarantee).
    let doc = unsafe { &*(handle as *const PdfDocument) };
    match Catalog::from_document(doc) {
        Ok(cat) => cat.page_count as c_int,
        Err(e) => {
            // SAFETY: write_error checks error_out for null.
            unsafe { write_error(error_out, e) };
            -1
        }
    }
}

/// Extract plain text from page `page_index` (0-based).
///
/// Returns a NUL-terminated UTF-8 string the caller must free with
/// `pdf_free_string`, or `NULL` on error (sets `*error_out`).
///
/// # Safety
/// - `handle` must be a valid, un-freed document handle.
/// - `error_out` may be null.
#[no_mangle]
pub unsafe extern "C" fn pdf_document_extract_text(
    handle: *const std::ffi::c_void,
    page_index: c_ulong,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    // SAFETY: handle is a valid PdfDocument pointer (caller guarantee).
    let doc = unsafe { &*(handle as *const PdfDocument) };
    let result = (|| -> crate::error::Result<String> {
        let catalog = Catalog::from_document(doc)?;
        let page_dict = catalog.get_page_dict(doc, page_index as usize)?;
        let page = Page::from_dict(doc, &page_dict)?;
        let extractor = TextExtractor::extract_from_page(doc, &page)?;
        Ok(extractor.plain_text())
    })();
    match result {
        Ok(text) => CString::new(text).unwrap_or_default().into_raw(),
        Err(e) => {
            // SAFETY: write_error checks error_out for null.
            unsafe { write_error(error_out, e) };
            std::ptr::null_mut()
        }
    }
}

/// Retrieve document metadata as a JSON object string.
///
/// Returns a NUL-terminated UTF-8 JSON string the caller must free with
/// `pdf_free_string`, or `NULL` on error.
///
/// JSON shape:
/// ```json
/// {"title":null,"author":"Jane","subject":null,...}
/// ```
///
/// # Safety
/// - `handle` must be a valid, un-freed document handle.
/// - `error_out` may be null.
#[no_mangle]
pub unsafe extern "C" fn pdf_document_get_metadata(
    handle: *const std::ffi::c_void,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    // SAFETY: handle is a valid PdfDocument pointer (caller guarantee).
    let doc = unsafe { &*(handle as *const PdfDocument) };
    match Metadata::from_document(doc) {
        Ok(meta) => CString::new(metadata_to_json(&meta))
            .unwrap_or_default()
            .into_raw(),
        Err(e) => {
            // SAFETY: write_error checks error_out for null.
            unsafe { write_error(error_out, e) };
            std::ptr::null_mut()
        }
    }
}

/// Search all pages of the document for `query`.
///
/// Returns a NUL-terminated UTF-8 JSON array string the caller must free with
/// `pdf_free_string`, or `NULL` on error.
///
/// JSON shape:
/// ```json
/// [{"page_index":0,"text":"hello","bounds":[x1,y1,x2,y2]},...]
/// ```
///
/// # Safety
/// - `handle` must be a valid, un-freed document handle.
/// - `query` must be a NUL-terminated UTF-8 string.
/// - `error_out` may be null.
#[cfg(feature = "search")]
#[no_mangle]
pub unsafe extern "C" fn pdf_document_search(
    handle: *const std::ffi::c_void,
    query: *const c_char,
    case_sensitive: c_int,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    // SAFETY: handle is a valid PdfDocument pointer (caller guarantee).
    let doc = unsafe { &*(handle as *const PdfDocument) };
    // SAFETY: query is a NUL-terminated C string (caller guarantee).
    let q = match unsafe { CStr::from_ptr(query) }.to_str() {
        Ok(s) => s,
        Err(e) => {
            // SAFETY: write_error checks error_out for null.
            unsafe { write_error(error_out, e) };
            return std::ptr::null_mut();
        }
    };
    match search_document(doc, q, case_sensitive != 0) {
        Ok(results) => CString::new(search_results_to_json(&results))
            .unwrap_or_default()
            .into_raw(),
        Err(e) => {
            // SAFETY: write_error checks error_out for null.
            unsafe { write_error(error_out, e) };
            std::ptr::null_mut()
        }
    }
}

/// Free a string returned by any `pdf_document_*` function.
///
/// Passing `NULL` is a no-op.
///
/// # Safety
/// - `s` must be a pointer previously returned by a pdf-core C API function
///   and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn pdf_free_string(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: s was created by CString::into_raw (caller guarantee).
        drop(unsafe { CString::from_raw(s) });
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    fn minimal_pdf_bytes() -> Vec<u8> {
        include_bytes!("../../tests/fixtures/minimal.pdf").to_vec()
    }

    #[test]
    fn parse_and_free_happy_path() {
        let bytes = minimal_pdf_bytes();
        let handle =
            unsafe { pdf_document_parse(bytes.as_ptr(), bytes.len() as c_ulong, ptr::null_mut()) };
        assert!(!handle.is_null(), "expected valid handle");
        unsafe { pdf_document_free(handle) };
    }

    #[test]
    fn parse_invalid_bytes_returns_null_and_sets_error() {
        let bad = b"not a pdf".to_vec();
        let mut err_ptr: *mut c_char = ptr::null_mut();
        let handle =
            unsafe { pdf_document_parse(bad.as_ptr(), bad.len() as c_ulong, &mut err_ptr) };
        assert!(handle.is_null(), "expected null for bad input");
        assert!(!err_ptr.is_null(), "expected error string");
        unsafe { pdf_free_string(err_ptr) };
    }

    #[test]
    fn page_count_returns_positive() {
        let bytes = minimal_pdf_bytes();
        let handle =
            unsafe { pdf_document_parse(bytes.as_ptr(), bytes.len() as c_ulong, ptr::null_mut()) };
        assert!(!handle.is_null());
        let count = unsafe { pdf_document_page_count(handle as *const _, ptr::null_mut()) };
        assert!(count > 0, "expected at least one page");
        unsafe { pdf_document_free(handle) };
    }

    #[test]
    fn extract_text_page_zero_does_not_crash() {
        let bytes = minimal_pdf_bytes();
        let handle =
            unsafe { pdf_document_parse(bytes.as_ptr(), bytes.len() as c_ulong, ptr::null_mut()) };
        assert!(!handle.is_null());
        let text = unsafe { pdf_document_extract_text(handle as *const _, 0, ptr::null_mut()) };
        // minimal.pdf may have no text; either null (error) or a string is ok.
        if !text.is_null() {
            unsafe { pdf_free_string(text) };
        }
        unsafe { pdf_document_free(handle) };
    }

    #[test]
    fn get_metadata_returns_json_object() {
        let bytes = minimal_pdf_bytes();
        let handle =
            unsafe { pdf_document_parse(bytes.as_ptr(), bytes.len() as c_ulong, ptr::null_mut()) };
        assert!(!handle.is_null());
        let meta = unsafe { pdf_document_get_metadata(handle as *const _, ptr::null_mut()) };
        assert!(!meta.is_null(), "expected metadata JSON");
        let s = unsafe { CStr::from_ptr(meta) }.to_str().unwrap();
        assert!(s.starts_with('{') && s.ends_with('}'));
        unsafe { pdf_free_string(meta) };
        unsafe { pdf_document_free(handle) };
    }

    #[cfg(feature = "search")]
    #[test]
    fn search_empty_query_returns_json_array() {
        let bytes = minimal_pdf_bytes();
        let handle =
            unsafe { pdf_document_parse(bytes.as_ptr(), bytes.len() as c_ulong, ptr::null_mut()) };
        assert!(!handle.is_null());
        let query = CString::new("").unwrap();
        let results =
            unsafe { pdf_document_search(handle as *const _, query.as_ptr(), 0, ptr::null_mut()) };
        assert!(!results.is_null(), "expected JSON array");
        let s = unsafe { CStr::from_ptr(results) }.to_str().unwrap();
        assert!(s.starts_with('[') && s.ends_with(']'));
        unsafe { pdf_free_string(results) };
        unsafe { pdf_document_free(handle) };
    }

    #[test]
    fn free_string_null_is_noop() {
        unsafe { pdf_free_string(ptr::null_mut()) };
    }

    #[test]
    fn free_document_null_is_noop() {
        unsafe { pdf_document_free(ptr::null_mut()) };
    }

    #[test]
    fn json_string_escapes_special_chars() {
        let s = json_string("he said \"hi\"\nline2");
        assert_eq!(s, r#""he said \"hi\"\nline2""#);
    }

    #[test]
    fn metadata_to_json_all_null() {
        let m = Metadata::default();
        let j = metadata_to_json(&m);
        assert!(j.contains("\"title\":null"));
        assert!(j.contains("\"author\":null"));
    }
}
