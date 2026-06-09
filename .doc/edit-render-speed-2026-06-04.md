# Edit-render speed: O(1) page table + block-tile preview — Implementation Report

**Date:** 2026-06-04
**Scope:** `parser::objects` (page table), `document::catalog` (page lookup), `render::page_renderer`
(block-tile render), `wasm::text_edit` (edit preview)

## Problem

Text-edit reaction slowed as the page **number** grew (fine on page 1, laggy by ~page 10).
Two independent per-keystroke costs, both in `text_edit_render_block` (called once per
rAF-coalesced keystroke):

1. **Ordinal O(N):** `Catalog::get_page_dict(doc, page_index)` walked the page tree on every
   call; for a flat page tree it resolved N+1 page dicts to reach page N, and `get_object`
   re-parses object bytes (no parsed-object cache). → cost scaled with page index.
2. **Constant whole-page raster:** the preview rendered the *entire page* then cropped the
   block, because a sub-tile mis-mapped (see Fix B). → full-page pixmap alloc/clear per keystroke.

## What Was Implemented

### Fix A — O(1) page table (storage fix, helps every caller)
- `parser/objects.rs`: `PdfDocument` gains `page_refs: RefCell<Option<Vec<PdfObject>>>` plus
  `has_page_table()`, `set_page_table(refs)`, `cached_page_ref(index)`. The table maps page
  index → that page's **reference** (resolution stays lazy, honoring `overrides`).
- `document/catalog.rs`: `get_page_dict` builds the table once (new
  `collect_page_refs_iterative`, mirroring the trusted `collect_pages_iterative` document-order
  walk) and then resolves `cached_page_ref(index)` in O(1). The original tree walk remains as a
  fallback when a cached ref doesn't resolve to a page dict (malformed trees).

### Fix B — render only the block's tile (removes the whole-page raster)
- `render/page_renderer.rs`: new `render_block_tile(doc, page, scale, block_tile, content)`.
  It allocates a **block-sized** canvas with origin `(0,0)` and a *tile-relative* CTM that maps
  the block's own rectangle into `[0,w]×[0,h]`. Returns `((origin_x, origin_y), buffer)` where
  the origin is the block's full-page pixel position.
- `wasm/text_edit.rs`: `text_edit_render_block` now calls `render_block_tile` with the block
  tile instead of rendering `full_tile` and cropping; the manual full-page crop loop collapses
  to a straight un-premultiply of the small block buffer. `EditBlockRender.x/y` = the returned
  origin (unchanged blit semantics). No web-editor changes.

## Design Decisions
- **Cache page *references*, not resolved dicts.** Keeps resolution lazy and keeps the Part-2
  override layer working (an overridden page-content object is honored on `resolve`; the
  index→ref map is unaffected by content overrides). The pristine doc is immutable and
  structural page edits reparse into a fresh doc, so the table never goes stale — no
  invalidation needed.
- **Origin-(0,0) block canvas — the actual sub-tile bug fix.** The renderer is only *partly*
  origin-aware: glyph blits subtract `canvas.origin` ([page_renderer.rs](../src/render/page_renderer.rs)
  glyph transform; `canvas.blit_rgba`), but vector fills go straight to tiny-skia pixmap coords
  (`path_render::fill_path_with_rule` does **not** subtract origin). So a `new_tile(origin≠0)`
  canvas with a tile-relative CTM double-offsets glyphs while paths ignore the offset → mis-map
  (the documented "flip-cm" failure). Using origin `(0,0)` with a tile-relative CTM makes *both*
  paths land block-local consistently — the same convention `render_tile` uses for a full page
  (tile == whole page → origin 0). This needs **no** change to the shared `fill_path`/glyph code,
  so the proven full-page renderer is untouched.
- **Kept the page-tree walk as a fallback** rather than deleting it — preserves exact behavior
  for unusual/malformed trees the flat table can't represent.

## Test Coverage
- `document::catalog::tests::page_table_matches_walk_and_caches` — every page via the table-backed
  `get_page_dict` equals the independent `all_page_dicts` collector (order/identity), the table is
  lazy (not built until first lookup) then cached, and per-page refs are distinct. (`multipage.pdf`)
- `render::page_renderer::tests::block_tile_matches_full_crop` — `render_block_tile` output is
  **bit-exact** to cropping a full-page `render_tile_content` at the block rectangle (scale 1.0,
  integer coords), with a non-vacuous guard that the region has real content. (`with_stream.pdf`)
- Gate: `cargo fmt --check`, `cargo clippy --features wasm-render --no-default-features` clean;
  `cargo test --features render` (318) and `--features writer` (449) green; `make wasm` builds;
  `vue-tsc --noEmit` clean.

## Known Limitations / Follow-up
- `render_block_tile` does not apply page `/Rotate` (matches the prior edit-preview behavior,
  which also ignored it — `render_tile_content`). Rotated-page edit preview orientation is a
  separate, pre-existing gap.
- Bit-exactness vs a full-page crop holds when `block.x·scale` / `block.y·scale` land on integer
  pixels; otherwise sub-pixel antialiasing at glyph edges may differ by ≤1 level — imperceptible,
  and the page is re-rendered authoritatively on commit (Part 2) anyway.
- `path_render` vector fills remain origin-unaware; only made irrelevant here by the origin-0
  approach. A general origin-aware renderer (so `new_tile(origin≠0)` works everywhere) is a
  possible future cleanup but was intentionally avoided to keep this change low-risk.
