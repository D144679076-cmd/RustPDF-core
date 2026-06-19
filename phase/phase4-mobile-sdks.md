# Phase 4 — Mobile SDKs (iOS + Android)

**Status:** Complete — 2026-06-19
**Effort:** ~6–12 months each
**Tier gate:** Enterprise (mobile SDK license)
**Prerequisites:** Phase 3 REST API complete; all core features stable

## Strategy

Rather than shipping native binaries that must be updated per platform, the recommended approach is:

1. **WASM in WebView** (easiest, cross-platform): Ship the WASM binary inside a WKWebView (iOS) or WebView (Android). The same JavaScript API runs on mobile. Rendering happens in a `<canvas>` element. This gives full feature parity with zero extra Rust code.

2. **Native Rust via C FFI** (best performance): Expose Rust library via C ABI (`extern "C"` + `#[no_mangle]`). Generate Swift bindings with `uniffi` (iOS) or Kotlin bindings with `uniffi`/`jni` (Android).

**Recommended starting point:** WASM-in-WebView first (fast to ship), then native Rust FFI for performance-sensitive operations (rendering, large files).

## Phase 4a — C FFI Layer `src/ffi/`

```rust
// src/ffi/mod.rs — exposed via cdylib
#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_ulong};

/// Parse a PDF from bytes. Returns an opaque handle, or NULL on error.
/// Caller must free with pdf_document_free().
#[no_mangle]
pub unsafe extern "C" fn pdf_document_parse(
    data: *const u8,
    len: c_ulong,
    error_out: *mut *mut c_char,
) -> *mut std::ffi::c_void {
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec();
    match crate::parser::PdfDocument::parse(bytes) {
        Ok(doc) => Box::into_raw(Box::new(doc)) as *mut std::ffi::c_void,
        Err(e) => {
            if !error_out.is_null() {
                let msg = CString::new(e.to_string()).unwrap_or_default();
                unsafe { *error_out = msg.into_raw(); }
            }
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn pdf_document_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        drop(Box::from_raw(handle as *mut crate::parser::PdfDocument));
    }
}

#[no_mangle]
pub unsafe extern "C" fn pdf_document_page_count(handle: *const std::ffi::c_void) -> c_int {
    let doc = &*(handle as *const crate::parser::PdfDocument);
    doc.page_count().unwrap_or(0) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn pdf_free_string(s: *mut c_char) {
    if !s.is_null() { drop(CString::from_raw(s)); }
}

// ... additional functions for all major operations
```

## Phase 4b — Swift Bindings (iOS) via `uniffi`

```toml
[dependencies]
uniffi = { version = "0.25", optional = true }

[build-dependencies]
uniffi = { version = "0.25", features = ["build"] }
```

Define UDL (Universal Definition Language) file `src/ffi/pdf_core.udl`:

```udl
namespace pdf_core {
    [Throws=PdfError]
    PdfDocument parse_document(bytes data);

    [Throws=PdfError]
    sequence<SearchResult> search_document(PdfDocument doc, string query, boolean case_sensitive);
};

[Error]
enum PdfError {
    "ParseError", "WriteError", "LicenseRequired", "Encrypted"
};

interface PdfDocument {
    [Throws=PdfError]
    u64 page_count();

    [Throws=PdfError]
    string extract_text(u64 page_index);

    [Throws=PdfError]
    string get_metadata();
};

dictionary SearchResult {
    u64 page_index;
    string text;
    sequence<double> bounds; // [x1, y1, x2, y2]
};
```

Generate Swift bindings:
```bash
uniffi-bindgen generate src/ffi/pdf_core.udl --language swift
```

Produces `PdfCore.swift` + `pdf_coreFFI.h` for XCFramework packaging.

## Phase 4c — Kotlin Bindings (Android) via `uniffi`

Same UDL file, generate Kotlin:
```bash
uniffi-bindgen generate src/ffi/pdf_core.udl --language kotlin
```

Produces `pdf_core.kt` for AAR packaging.

## Phase 4d — WASM-in-WebView (Fastest Path to Ship)

**iOS (WKWebView):**
```swift
import WebKit

class PdfEditorView: UIViewController, WKNavigationDelegate {
    let webView = WKWebView()

    func loadPdfEditor() {
        // Load the web editor bundle (Vue3 + WASM)
        let url = Bundle.main.url(forResource: "index", withExtension: "html", subdirectory: "WebEditor")!
        webView.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
    }

    func openPdf(_ data: Data) {
        // Send PDF bytes to JavaScript
        let base64 = data.base64EncodedString()
        webView.evaluateJavaScript("window.pdfEditor.openBase64('\(base64)')")
    }
}
```

**Android (WebView):**
```kotlin
class PdfEditorActivity : AppCompatActivity() {
    private lateinit var webView: WebView

    override fun onCreate(savedInstanceState: Bundle?) {
        webView.settings.javaScriptEnabled = true
        webView.settings.allowFileAccess = true
        webView.loadUrl("file:///android_asset/web_editor/index.html")
    }

    fun openPdf(bytes: ByteArray) {
        val base64 = android.util.Base64.encodeToString(bytes, android.util.Base64.DEFAULT)
        webView.evaluateJavascript("window.pdfEditor.openBase64('$base64')", null)
    }
}
```

This approach ships the existing web-editor (Vue3 + WASM) inside the mobile app with zero additional Rust code. Performance is slightly worse than native but acceptable for most use cases.

## Recommended Shipping Order

1. **Week 1**: WASM-in-WebView iOS demo
2. **Week 2**: WASM-in-WebView Android demo
3. **Month 2–3**: C FFI layer + Swift bindings (iOS)
4. **Month 4–5**: Kotlin bindings (Android)
5. **Month 6+**: Performance optimization (tile rendering, lazy page loading)
