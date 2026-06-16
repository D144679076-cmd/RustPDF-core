# PDF/A Compliance — Implementation Report

**Date:** 2026-06-16
**Scope:** `src/compliance/` — PDF/A-1b, 2b, 3b validation and conversion

## What Was Implemented

### New module: `src/compliance/`
- **`icc.rs`** — `srgb_icc_profile()` returning `&'static [u8]` via `include_bytes!` from `assets/sRGB_IEC61966-2-1.icc`.
- **`xmp.rs`** — `build_pdfa_xmp(title, author, part, conformance)` producing a well-formed XMP XML string with `pdfaid:part` and `pdfaid:conformance` declarations.
- **`pdfa.rs`** — Main module:
  - `PdfAViolation` struct (`rule`, `description`, `obj_id`)
  - Validation: `validate_pdfa_1b`, `validate_pdfa_2b`, `validate_pdfa_3b`
  - Conversion: `convert_to_pdfa_1b`, `convert_to_pdfa_2b`, `convert_to_pdfa_3b` (behind `writer` feature)
  - Private check helpers: `check_encryption`, `check_font_embedding`, `check_no_javascript`, `check_output_intents`, `check_xmp_metadata`, `check_no_transparency`, `check_no_external_streams`
  - Private conversion helpers: `add_output_intents`, `add_xmp_metadata`, `remove_javascript`, `embed_missing_fonts` (stub)
- **`mod.rs`** — Re-exports public API.

### New file: `assets/sRGB_IEC61966-2-1.icc`
Copied from `/usr/share/color/icc/colord/sRGB.icc` (22944 bytes, system-provided).

### Updated files
- **`src/lib.rs`** — Added `pub mod compliance;` (unconditional; validation functions need no features).
- **`src/wasm/editor.rs`** — Added `validate_pdfa(&self, level)` and `convert_to_pdfa(&mut self, level)` on `WasmEditor`, returning JSON-serialised violations or `JsError`.

## Design Decisions

- **Module gated selectively**: `compliance` module is always declared in `lib.rs`. Validation functions use only `PdfDocument` (always available). Conversion functions and their helpers are gated behind `#[cfg(feature = "writer")]`. License check inside convert functions is gated behind `#[cfg(feature = "crypto")]`.
- **No `into_dict()` on `PdfObject`**: `PdfObjectExt::into_dict()` is a local trait in `redact.rs`, not public. Used pattern matching directly throughout.
- **`editor.catalog_id` for root access**: `PdfEditor` exposes `catalog_id: u32` directly; used instead of walking the trailer in conversion helpers.
- **Font embedding stubbed**: Full font embedding requires external TTF/OTF binary data per font. Implemented as a logged warning (`embed_missing_fonts`). Callers should pre-validate with `validate_pdfa_1b` and embed fonts before converting.
- **XMP uncompressed**: XMP stream uses `make_raw_stream` (no FlateDecode), as required by PDF/A §6.7.3.
- **Transparency check (PDF/A-1b only)**: Walks ExtGState dicts per page checking `/BM != Normal` and `/ca`/`/CA != 1.0`. Not called by `validate_pdfa_2b` (2b allows transparency with ICC constraints).
- **External stream check**: Walks all object IDs checking for `/F` key in stream dicts. Uses `doc.max_object_id()` as upper bound.
- **WASM JSON serialisation**: Manual string building (no serde dependency) to keep WASM binary small.

## Test Coverage

| Test | What It Covers |
|------|----------------|
| `xmp_contains_pdfa_tags` | XMP output has correct pdfaid tags and content fields |
| `xmp_handles_none_fields` | XMP works with no title/author |
| `minimal_pdf_fails_pdfa_1b_no_output_intents` | Rule 6.2.3 detected on bare minimal.pdf |
| `minimal_pdf_fails_pdfa_1b_no_xmp` | Rule 6.7.2 detected on bare minimal.pdf |
| `convert_to_pdfa_1b_adds_output_intents_and_metadata` | Conversion adds both /OutputIntents and /Metadata |
| `convert_to_pdfa_2b_adds_output_intents` | 2b conversion adds /OutputIntents |
| `converted_doc_passes_output_intents_check` | Round-trip: convert then validate clears 6.2.3 and 6.7.2 |
| `validate_pdfa_3b_delegates_to_2b` | 3b returns same violations as 2b |

All 308 tests pass. WASM build succeeds.

## Known Limitations / Follow-up

1. **Font embedding not implemented** (`embed_missing_fonts` is a stub). Follow-up: embed Standard-14 from bundled font data; for arbitrary fonts, require caller to supply TTF bytes.
2. **XMP content not parsed for validation** — `check_xmp_metadata` only verifies `/Metadata` key exists in the catalog; it does not parse the XMP stream to confirm `pdfaid:part` / `pdfaid:conformance` are present.
3. **PDF/A-2b transparency rules** — ICC-constrained transparency allowed in 2b is not positively validated. The check simply skips the transparency walk for 2b/3b.
4. **Output intent ICC validation** — `check_output_intents` only checks key presence, not that the entry references a valid ICC profile.
5. **`sRGB_IEC61966-2-1.icc` source** — Currently copied from the system colord package. For reproducible builds, the file should either be vendored in the repo or downloaded deterministically via a build script.
