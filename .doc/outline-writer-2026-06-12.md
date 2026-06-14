# outline-writer — Implementation Report

**Date:** 2026-06-12
**Scope:** Phase 2 — Bookmarks Write API (`src/document/outline_writer.rs`)

## What Was Implemented

- `OutlineEntry` struct — public type describing a single bookmark: title, page index, y-position, open/bold/italic flags, optional RGB colour, and recursive children.
- `set_document_outline(editor, entries)` — replaces the entire document outline in one call; writes all linked-list item objects plus the root /Outlines dict into the incremental update pool, then patches the catalog's /Outlines pointer and /PageMode.
- `remove_outlines(editor)` — internal helper called when `entries` is empty; shifts /Outlines out of the catalog dict.
- `build_outline_items(editor, entries, parent_id)` — recursive builder that pre-allocates all sibling IDs in one pass (so /Prev and /Next can be back-filled without a second pass), then writes each item dict with /Dest `[page_ref /XYZ null y null]`, optional /F (bold/italic) and /C (colour) entries, and recurses into children.
- `resolve_page_ref(editor, page_index)` — resolves a page index to the PDF page reference object, using the cached page table (O(1)) when available, otherwise triggering a build via `get_page_dict`.
- WASM binding `WasmEditor::set_outline(outline_json)` in `src/wasm/editor.rs` — parses the JSON array and calls `set_document_outline`.
- Hand-rolled JSON parser (`parse_outline_json`, `parse_outline_array`, `parse_outline_object`, and field helpers) — avoids adding `serde_json` as a dependency, keeping binary size small and WASM compliance guaranteed.
- Integration tests in `tests/bookmarks.rs` (5 tests) and unit tests inside `outline_writer.rs` (4 tests).

## Design Decisions

- **`writer` feature gate** — `outline_writer` depends on `PdfEditor` which lives behind `#[cfg(feature = "writer")]`. Module declaration and re-exports are gated identically so the crate still compiles in minimal (parse-only) mode.
- **Pre-allocate IDs** — all sibling item IDs are reserved via `writer.reserve_id()` before any dict is written. This allows filling /Prev and /Next links in the same forward pass, matching the ISO 32000-1 §12.3.3 structure without needing a second pass or a post-processing fixup.
- **`catalog_id` field** — used directly instead of walking the trailer, because `PdfEditor` already caches it on open. Simpler and avoids a trait-boundary issue with `&PdfEditor` vs `&mut PdfEditor`.
- **`checkpoint()` before mutation** — honours the editor's undo/redo contract; a call to `undo()` after `set_document_outline` reverts all outline objects atomically.
- **`resolve_page_ref` strategy** — tries `doc.cached_page_ref(idx)` first (hot path, O(1)). If the page table hasn't been built yet, falls back to `editor.get_page_dict(idx)` which builds it as a side effect. Returns a `PdfObject::Reference` in both cases.
- **Hand-rolled JSON parser** — avoids `serde_json` / `serde` dependency which adds ~200 KB to the WASM binary. The format is narrow and controlled, so a simple recursive descent parser suffices.
- **Null left/zoom in /Dest** — `[page_ref /XYZ null y null]` lets the PDF viewer preserve the horizontal scroll position and current zoom level, which is the least-surprising default for a bookmark editor.

## Test Coverage

### Unit tests (`src/document/outline_writer.rs`)
- `set_outline_creates_bookmarks` — two top-level entries with one nested child; round-trips through `save_append` + `PdfDocument::parse` and verifies titles and tree shape.
- `set_outline_styling` — bold+italic+colour entry; confirms document parses and title is preserved.
- `remove_outlines_works` — add then remove; confirms document parses and outline list is empty.
- `set_outline_idempotent` — two consecutive `set_document_outline` calls; confirms the second replaces the first (count = 1).

### Integration tests (`tests/bookmarks.rs`)
- `set_outline_two_chapters_with_child` — full tree shape, open flag propagation.
- `set_outline_catalog_has_outlines_ref` — asserts `/Outlines` key is present in the saved catalog.
- `remove_outlines_clears_catalog_entry` — add then remove; asserts empty outline list.
- `set_outline_deep_nesting` — three levels (L1 → L2 → L3).
- `set_outline_replace_existing` — two sequential calls; second result has two entries.

## Known Limitations / Follow-up

- **Title encoding** — titles are stored as raw UTF-8 byte strings (`PdfObject::String`). The ISO standard recommends PDF text strings (PDFDocEncoding or UTF-16BE with BOM) for non-ASCII content. A future `encode_pdf_text_string` pass would handle CJK, emoji, etc.
- **Named destinations** — only direct array destinations `/Dest [page /XYZ ...]` are written. Named destination dicts (`/Dests` in the catalog name tree) are not produced.
- **y_position coordinate space** — the caller supplies PDF user-space y (bottom = 0). Viewer UIs typically show page-top coordinates; the TS layer must convert before calling `set_outline`.
- **Old outline items not freed** — incremental update mode can't delete the previous outline item objects (PDF doesn't support deletion); they become unreferenced dead objects. A linearise/rewrite pass would reclaim the space.
