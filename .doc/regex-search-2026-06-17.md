# Regex Search — Implementation Report

**Date:** 2026-06-17
**Scope:** text/search — regex search (Phase 3)

## What Was Implemented

- `search_document_regex(doc, pattern, case_sensitive)` — searches all pages via compiled regex
- `search_page_regex(doc, page_index, pattern, case_sensitive)` — single-page regex search
- `search_page_regex_inner(doc, page_index, re)` — shared inner implementation
- All three gated behind `#[cfg(feature = "search")]`
- WASM binding `search_text_regex(pattern, case_sensitive)` in `src/wasm/document.rs`
- Re-exports in `src/text/mod.rs`
- `regex` optional dep added to `Cargo.toml` with features `std`, `unicode-perl`, `unicode-case`
- New `search` feature enables `dep:regex`; `wasm` feature now includes `search`

## Design Decisions

- Added `unicode-case` to `regex` features: required for case-insensitive matching on non-ASCII text; `unicode-perl` alone is insufficient.
- Bounding-box logic uses word-level union (no sub-word fractional clipping) — regex matches can span arbitrary byte ranges, making per-char fraction math fragile. Word-level union is conservative and correct.
- `Box::leak` for the invalid-regex error message: avoids a `'static` lifetime requirement on the `PdfError` message without pulling in an `Arc<str>` or `Cow`.
- Pro-tier license gate mirrors the existing `search_document` gating pattern (`#[cfg(feature = "crypto")]` guard around `crate::license::require`).

## Test Coverage

- `regex_finds_pattern` — happy path: `Page\s+\d+` finds exactly 3 results across multipage.pdf
- `regex_case_insensitive` — lowercase pattern with `case_sensitive=false` finds results
- `invalid_regex_returns_error` — malformed pattern returns `Err`

All 311 tests pass with `--features search`.

## Known Limitations / Follow-up

- Bounding boxes are word-level unions. A regex matching part of a merged `TextWord` highlights the full word, unlike the substring search which approximates sub-word bounds via char-fraction math.
- No `search_page_regex` WASM binding exposed (only `search_text_regex` which searches all pages). Add a `search_page_text_regex` WASM method if per-page regex is needed by the JS layer.
