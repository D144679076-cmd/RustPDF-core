# extract_pages — Implementation Report

**Date:** 2026-06-06
**Scope:** `editor::merge::extract_pages` (Phase 1 – Page Split/Extract)

## What Was Implemented

### `src/editor/merge.rs`
- `pub fn extract_pages(source_data: Vec<u8>, page_range: std::ops::Range<usize>) -> Result<Vec<u8>>` — main entry point
- `fn collect_refs(obj: &PdfObject, queue: &mut Vec<u32>)` — recursively enqueues all indirect reference IDs reachable from an object
- `fn remap_object_with_map(obj: &PdfObject, id_map: &HashMap<u32, u32>) -> PdfObject` — rewrites all `Reference(n, g)` using a caller-supplied mapping
- `fn remap_dict_with_map(dict: &PdfDict, id_map: &HashMap<u32, u32>) -> PdfDict` — dict-level wrapper around `remap_object_with_map`

### `src/editor/mod.rs`
- Re-exported `extract_pages` alongside `MergeBuilder`.

### `src/wasm/editor.rs`
- `WasmEditor::extract_pages(&self, start: usize, end: usize) -> Result<Vec<u8>, JsError>` — WASM binding; uses `self.original_bytes` (already stored on `WasmEditor`) to avoid adding a `raw_bytes()` getter to `PdfDocument`.

### `tests/merge_redact.rs`
- New `mod extract_tests` with 7 integration tests (see below).

## Design Decisions

**Modified page dicts as the closure seed.** The transitive-closure walk starts from refs found in the *modified* page dicts (after flattening inherited `MediaBox` and `Resources`, and removing `/Parent`), not from the original page objects. If a page inherits `Resources` from its parent `Pages` node, the parent is not extracted but its resource objects (fonts, images) must be. Seeding from the modified dict captures those refs; seeding from the original `page_id` and following `/Parent` would have pulled in the whole page tree.

**Pre-marking page IDs as visited.** All extracted page IDs are inserted into `visited` before the BFS begins. This prevents the BFS from following them as plain objects (which would use the original, unmodified dicts with stale `/Parent` refs). Pages are written separately, after the BFS, using the modified dicts.

**ID assignment before writing.** All IDs are reserved in one pass over `visited` so that `id_map` is complete when we remap individual objects. The alternative (reserve-on-demand during remapping) would require two passes anyway or produce non-deterministic ordering.

**`original_bytes` over `raw_bytes()`.** The WASM binding uses `self.original_bytes.clone()` rather than adding a `pub fn raw_bytes() -> &[u8]` getter to `PdfDocument`. The bytes are already available on `WasmEditor` and the function consumes owned bytes, so cloning is correct and the change is confined to the WASM layer.

**License gate mirrors `merge`.** The `#[cfg(feature = "crypto")]` guard matches the pattern in `MergeBuilder::merge` so non-crypto builds (dev/test) are always open; production builds with `crypto` enforce `Tier::Pro`.

## Test Coverage

| Test | What it covers |
|------|----------------|
| `extract_one_page_from_multipage` | happy-path single page; page count = 1 |
| `extract_range_correct_count` | multi-page range (0..2); page count = 2 |
| `extracted_pdf_starts_with_pdf_header` | output starts with `%PDF-` |
| `extract_out_of_bounds_errors` | range exceeds total pages → `Err` |
| `extract_all_pages_preserves_count` | full document round-trip; count unchanged |
| `extract_empty_range_errors` | empty range (0..0) → `Err` |
| `extract_single_page_from_single_page_pdf` | single-page source; happy-path |

All 290 tests pass; WASM build is clean.

## Known Limitations / Follow-up

- **Non-contiguous page ranges** are not supported; the API accepts only `Range<usize>`. A future phase could accept a `Vec<usize>` or `RangeSet`.
- **Annotations with inter-page links** (e.g. `GoTo` actions pointing to pages outside the extracted range) are copied verbatim; the destination reference will dangle in the output. A production-grade implementation would strip or rewrite such actions.
- **Outline trees** are not forwarded to the extracted document. The merge path (`build_merged_outlines`) preserves bookmarks; extract does not. A follow-up could filter the original outline to include only items pointing at extracted pages.
- **WASM binding extracts from original bytes**, not from any pending edits. If the caller has made edits before calling `extract_pages`, those edits are not reflected. This matches the spec's intent; if needed, calling `save()` first and re-opening would be the workaround.
