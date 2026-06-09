# text-editor — Implementation Report

**Date:** 2026-05-25
**Scope:** In-place text replacement in PDF content streams

## What Was Implemented

### Rust core (`pdf-editor-rust-core`)

- `src/editor/text_editor.rs` (new module)
  - `TextEditTarget` — struct identifying a span by position + content
  - `replace_text_in_page` — walks page content stream, finds matching `Tj`/`TJ` operator, replaces string operand in-place, rewrites page `/Contents`
  - `patch_operations` — internal: tracks text matrix state (`Tm`, `Td`, `TD`, `T*`, `TL`) and patches the first matching operator
  - `serialize_operations` — serializes `Vec<Operation>` back to PDF content stream bytes
  - `encode_pdf_string` — encodes UTF-8 to Latin-1 or UTF-16BE with BOM
  - `decode_pdf_string` — decodes PDF string bytes (UTF-16BE or Latin-1) to Rust `String`
  - `resolve_font_name` — walks `/Resources/Font/<key>/BaseFont` to resolve resource key to actual font name
  - `decode_page_contents` — decodes single or array `/Contents` into a flat byte buffer

- `src/editor/mod.rs` — added `pub mod text_editor` + re-exports

- `src/wasm/editor.rs` — added `replace_text_in_stream` WASM binding

- `src/wasm/document.rs` — added `resolve_font_name` WASM binding

### Frontend (`web-editor`)

- `MainLayout.vue` — added `console.log` in `activateEditMode()`
- `usePageRenderer.ts` — added logs at watchEffect entry and render completion
- `usePdfStore.ts` — added logs in `refreshDoc`; updated `replaceText` to call `replace_text_in_stream` first, fall back to cover-and-redraw only if no match
- `AnnotationOverlay.vue` — added logs in watch, `svgTextSpans`, `onTextSpanClick`; updated `commitEdit` to call `resolve_font_name` and pass `oldText` to `replaceText`

## Design Decisions

- **In-place over append**: Rather than appending a white-rect + draw_text layer (which is visible on non-white backgrounds and loses font fidelity), we modify the original `BT...ET` block. The modified stream replaces the page's `/Contents` entirely as a new compressed stream object — this is still an incremental update (the old stream object is shadowed, not deleted).

- **`TJ` → `Tj` promotion**: When a `TJ` array matches, we replace the whole array with a single `Tj` string. This is valid PDF and simpler than reconstructing a TJ array with the new text.

- **Position tolerance**: x ±2pt, y ±(font_size × 0.6). Tight enough to avoid false matches on dense pages, loose enough to handle sub-point rounding differences between the extractor and the content stream.

- **Fallback to cover-and-redraw**: If `replace_text_in_stream` returns `false` (scanned PDF, image-only page, or encoding mismatch), the old white-rect approach is used as a fallback so the feature still works partially.

- **Font resolution**: `resolve_font_name` walks `/Resources/Font/<key>/BaseFont` so the FE can pass the real font name to the fallback `draw_text` path instead of always using Helvetica.

## Test Coverage

- `encode_latin1_roundtrip` — Latin-1 encode/decode round-trip
- `encode_utf16_roundtrip` — UTF-16BE encode/decode round-trip (BOM check)
- `serialize_simple_ops` — BT/Tm/Tj/ET serializes to valid content stream text
- `patch_tj_replaces_matching_span` — Tj at exact Tm position is replaced
- `patch_tj_no_match_returns_false` — wrong position returns false
- `patch_tj_replaces_via_td` — position tracked through Td offset

## Known Limitations / Follow-up

- **Multi-line text**: Only replaces the first matching span. Multi-line words split across multiple `Tj` calls are not handled.
- **CIDFont / Type0 encoding**: `decode_pdf_string` handles Latin-1 and UTF-16BE. CIDFont glyph-index strings (common in modern PDFs) will not decode correctly — the extractor may return garbled text and the match will fail, falling back to cover-and-redraw.
- **Scaled/rotated text**: `Tm` with non-identity scale/rotation (a≠1 or b/c≠0) — position is taken from e/f components only; the actual rendered position may differ. Tolerance helps but won't cover large transforms.
- **Form XObjects**: Text inside Form XObjects has its own content stream. `replace_text_in_page` only patches the page's direct content stream, not embedded XObjects.
- **WASM rebuild needed**: The `.wasm` and `.js` bindings in `web-editor/src/pkg/` must be regenerated with `wasm-pack build` after this Rust change before the FE can use the new APIs.
