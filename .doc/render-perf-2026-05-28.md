# render-perf — Implementation Report

**Date:** 2026-05-28
**Scope:** rendering performance optimisations

## What Was Implemented

### Change 1 — Eliminate per-glyph pixel clone + heap allocation
- Added `blit_scratch: Vec<u8>` field to `PageRenderer` (initialised as `Vec::new()`).
- Rewrote `draw_text_span` to split field borrows before the per-character loop:
  `embedded_bytes_ref: Option<&[u8]>` borrows `self.font_bytes_cache` (immutable);
  `gc`, `cv`, `sk` hold exclusive borrows of `glyph_cache`, `canvas`, `blit_scratch`.
  Rust's disjoint-field-borrow rule allows all four simultaneously.
- Removed `g.pixels.clone()` calls (lines 213, 229 in the original) that cloned the full glyph bitmap on every cached hit.
- Changed `blit_alpha_mask` signature to `(mask, params, canvas, scratch: &mut Vec<u8>)`.
  The function now calls `scratch.clear(); scratch.reserve(n*4)` instead of `Vec::with_capacity`.
  After the first glyph per tile the buffer is already sized — all subsequent calls reuse capacity.

### Change 2 — Per-renderer font TTF bytes cache
- Added `font_bytes_cache: HashMap<String, Option<Vec<u8>>>` field to `PageRenderer`.
- `draw_text_span` now populates the cache on first use per font resource name (`contains_key` + `insert`).
- `get_ttf_bytes` (which walks the XRef, navigates the font descriptor, and calls `PdfStream::decode()`) is called at most once per font per tile, not once per text span.

### Change 3 — Cross-tile GlyphCache (new public API)
- Added `PageRenderer::new_with_external_cache` constructor (private).
- Added public functions:
  - `render_tile_with_cache(doc, page, scale, tile, cache: GlyphCache) -> Result<(PixmapBuffer, GlyphCache)>`
  - `render_page_with_cache(doc, page, scale, cache: GlyphCache) -> Result<(PixmapBuffer, GlyphCache)>`
- Callers pass in a `GlyphCache` and receive it back on success; the caller holds it across tiles.
- `GlyphCache` re-exported from `src/render/mod.rs`.
- Ownership-passing variant chosen over `&mut GlyphCache` to avoid lifetime annotations at the public API boundary and remain WASM-safe.

### Change 4 — Shading bounding-box culling
**Axial (`rasterize_axial`)**:
- Pre-computes the linear t function: `t(px, py) = t00 + dt_dx*px + dt_dy*py`.
- When neither extend flag is set, solves for the active row range [start_row, end_row] algebraically.
- Allocates `w * active_rows * 4` bytes instead of `w * h * 4`.
- Inner loop increments `t_raw` by `dt_dx` per column, avoiding one multiply per pixel.
- Blits at `(origin.x, origin.y + start_row)` with height `active_rows`.

**Radial (`rasterize_radial`)**:
- Computes device-space bounding circle(s): forward-transforms centres, scales radii by `max(scale_x, scale_y)`.
- Clips row iteration to the y-extent of the bounding circles.
- Allocates `w * active_rows * 4` bytes and blits with y offset.

## Design Decisions

- **Ownership-passing for GlyphCache** — avoids a second lifetime parameter on `PageRenderer` and is idiomatic for WASM contexts where the caller manages JS-side state.
- **Field borrow split instead of `Arc<[u8]>`** — zero overhead; Arc would add atomic operations and change the GlyphBitmap layout. The borrow split is a pure compile-time mechanism.
- **Scratch buffer for blit** — a single reused buffer eliminates one heap allocation per glyph after the first. The buffer grows to fit the largest glyph and stays sized.
- **Axial BBox via linear algebra** — t is provably linear in pixel coordinates; algebraic row-range derivation is exact and O(1) vs. O(h) sampling.
- **Radial BBox via forward CTM** — conservative (uses max of x/y CTM scales), but cheap and always correct. Prefer correctness over tightness for a circle bound.
- **Only cull when extend = [false, false]** — extend flags make the gradient fill to infinity in one or both directions; skipping culling there avoids edge-case bugs.

## Test Coverage

No new tests added — existing 306-test suite (including render integration tests) passes unchanged. The changes are internal to the rendering pipeline; observable output (RGBA pixels) is identical to the pre-optimisation code.

## Known Limitations / Follow-up

- **Cross-tile `font_bytes_cache`** — the cache is per-renderer (discarded after each tile). The same font stream is still decoded once per tile. A document-level `RefCell<HashMap<…>>` cache (similar to `obj_stream_cache`) would reduce this to once per document load.
- **Content stream decode cache** — `page.decode_contents(doc)` is called fresh per tile, re-applying FlateDecode to all content streams. A page-level cache indexed by content stream object numbers would eliminate this.
- **Axial column culling** — the active-row BBox skips entire rows. Per-row column culling (finding the first and last px where t ∈ [0,1]) is possible but adds per-row O(1) overhead; not implemented.
- **Radial culling is conservative** — the bounding circle uses `max(scale_x, scale_y)`, which over-estimates for non-uniform CTMs. The actual visible region may be an ellipse.
- **No benchmarks** — formal criterion benchmarks would quantify the gains; recommended as follow-up.
