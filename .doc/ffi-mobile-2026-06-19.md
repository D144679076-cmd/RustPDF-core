# FFI Mobile SDK — Implementation Report

**Date:** 2026-06-19
**Scope:** Phase 4 — Mobile SDK integration (C FFI + uniffi Swift/Kotlin bindings)

## What Was Implemented

### Phase 4a — C FFI layer (`src/ffi/c_api.rs`)

Functions exported via `extern "C"` + `#[no_mangle]`:

| Function | Description |
|---|---|
| `pdf_document_parse(data, len, error_out)` | Parse PDF bytes → opaque handle |
| `pdf_document_parse_with_password(data, len, password, error_out)` | Parse encrypted PDF (requires `crypto` feature) |
| `pdf_document_free(handle)` | Drop the document allocation |
| `pdf_document_page_count(handle, error_out)` | Return page count or -1 |
| `pdf_document_extract_text(handle, page_index, error_out)` | Plain-text extraction, JSON-adjacent string |
| `pdf_document_get_metadata(handle, error_out)` | JSON object string with all /Info fields |
| `pdf_document_search(handle, query, case_sensitive, error_out)` | JSON array of `{page_index, text, bounds}` (requires `search` feature) |
| `pdf_free_string(s)` | Free any string returned by the above |

Enabled by: `--features ffi`.

### Phase 4b/4c — uniffi bridge (`src/ffi/uniffi_bridge.rs`)

Types and exports for auto-generating idiomatic Swift (iOS) and Kotlin (Android) wrappers:

- `PdfError` — `#[derive(uniffi::Error)]` enum with five variants
- `SearchResult` — `#[derive(uniffi::Record)]` dictionary
- `PdfDocument` — `#[derive(uniffi::Object)]` interface backed by `Arc<CoreDocument>`
- `#[uniffi::export]` on `PdfDocument` methods and three namespace functions

Enabled by: `--features mobile` (which implies `ffi`).

### UDL reference file (`src/ffi/pdf_core.udl`)

Human-readable API description; also the input to `uniffi-bindgen generate`
when generating from the UDL file directly rather than the compiled library.

### Cargo changes

- New optional dep: `uniffi = { version = "0.25", optional = true }`
- New features: `ffi` (C FFI, implies `search`), `mobile` (C FFI + uniffi)
- `lib.rs`: `uniffi::setup_scaffolding!("pdf_core")` gated on `mobile + !wasm32`

## Design Decisions

**C FFI uses opaque void pointers** — avoids exposing Rust type layouts across the ABI boundary. Callers treat the handle as a black box and pass it back for every operation.

**Error-out pattern** — mirrors the C pattern used by Core Foundation, libpdf, and other system libraries: return a sentinel (NULL / -1) on failure, set `*error_out` to a heap-allocated message the caller must free. Avoids exceptions or setjmp/longjmp.

**Hand-rolled JSON serialization** — `Metadata` and `SearchResult` have fixed, small schemas. Pulling in `serde_json` for two small serializers would add ~200 KB to the native binary and ~50 KB to the `ffi` feature dep tree. The hand-rolled approach is 30 lines and covers all required escaping.

**proc-macro approach for uniffi** — uniffi 0.25 supports two modes: UDL scaffolding (old) and proc macros (new). Proc macros (`#[derive(uniffi::Object)]`, `#[uniffi::export]`) are chosen because:
- No build.rs UDL codegen step required — reduces CI surface
- Language bindings still generated via `uniffi-bindgen generate --library`
- The UDL file is kept as human-readable reference (same format, not compiled)

**`parse_with_password` gated on `crypto`** — matches the core type's `#[cfg(feature = "crypto")]` annotation. The C and uniffi variants are both gated.

**`search` implied by `ffi`** — the search FFI function is a key mobile use case; the `ffi` feature implies `search` so callers get it without a separate flag. Gated by `#[cfg(feature = "search")]` inside the function so the symbol only exists when the feature is active.

## Test Coverage

### `c_api.rs`

| Test | Path |
|---|---|
| `parse_and_free_happy_path` | valid parse → handle → free |
| `parse_invalid_bytes_returns_null_and_sets_error` | bad bytes → NULL + error string |
| `page_count_returns_positive` | page count > 0 on minimal.pdf |
| `extract_text_page_zero_does_not_crash` | text or null, no panic |
| `get_metadata_returns_json_object` | starts `{`, ends `}` |
| `search_empty_query_returns_json_array` | starts `[`, ends `]` (search feature) |
| `free_string_null_is_noop` | null → no crash |
| `free_document_null_is_noop` | null → no crash |
| `json_string_escapes_special_chars` | `"` and `\n` are correctly escaped |
| `metadata_to_json_all_null` | all-None Metadata → `"title":null,...` |

### `uniffi_bridge.rs`

| Test | Path |
|---|---|
| `parse_document_happy_path` | parse + page_count ≥ 1 |
| `parse_document_bad_bytes_returns_err` | bad bytes → `Err` |
| `get_metadata_json_is_valid_object` | starts `{`, contains `"title"` |
| `extract_text_page_zero` | no panic |
| `extract_text_out_of_range_returns_err` | page 9999 → `Err` |
| `search_empty_query_returns_empty_vec` | empty query → empty `Vec` (search feature) |

## Language Binding Generation

```bash
# Build the native library:
cargo build --release --features mobile

# Swift (iOS XCFramework):
uniffi-bindgen generate --library target/release/libpdf_core.dylib \
    --language swift --out-dir mobile/ios/Sources/PdfCore

# Kotlin (Android AAR):
uniffi-bindgen generate --library target/release/libpdf_core.so \
    --language kotlin \
    --out-dir mobile/android/src/main/kotlin/com/example/pdfcore
```

Outputs: `PdfCore.swift` + `pdf_coreFFI.h` (iOS) or `pdf_core.kt` (Android).
These files are generated at SDK-build time and are not committed to this repo.

## Known Limitations / Follow-up

- **Phase 4d (WASM-in-WebView)** is not Rust code; the integration snippets are in the phase doc (`phase/phase4-mobile-sdks.md`). No changes needed in this crate.
- **uniffi warning** — `uniffi::setup_scaffolding!("pdf_core")` emits one `unpredictable_function_pointer_comparisons` warning on Rust ≥ 1.79; this originates inside the uniffi macro and cannot be suppressed without `#![allow(...)]` at crate level. Track the upstream uniffi issue.
- **Header generation** — a C header file (`pdf_core.h`) is not yet generated. Add `cbindgen` as a build dep to auto-generate it from `c_api.rs` for JNI / Swift manual-binding callers.
- **Android JNI wrapper** — the C FFI functions are callable from Kotlin via JNI but require a thin `PdfCoreJni.kt` glue class. Follow-up in Phase 4c build pipeline.
- **Thread safety audit** — `CoreDocument` is assumed `Send + Sync` via `parking_lot` RwLock internals. Confirm with `static_assertions::assert_impl_all!(PdfDocument: Send, Sync)`.
