# Render caching + editor-box width — Implementation Report

**Date:** 2026-06-03
**Scope:** `render::glyph_cache`, `render::image`, `render::page_renderer`, web overlay `composeBlock`

## What Was Implemented

Editing on page 2+ was slow (editor box per keystroke and full-page re-render) and the box
didn't resize to fit the text until re-opened. Root cause for the speed: `PageRenderer` is
built fresh per render and did no cross-call caching — embedded fonts were re-parsed and
images re-decoded on every render, repaid per commit (each commit reparses the document).

### Part 1A — Cross-render parsed-font cache (`render::glyph_cache`)
- Added a `thread_local! PARSED_FONTS: HashMap<u64, Rc<FdFont>>` keyed by a hash of the
  font-program bytes, plus `get_or_parse_font(bytes) -> Option<Rc<FdFont>>` (parse only on
  miss; crude 32-entry cap).
- `GlyphCache.fonts` now holds `Rc<FdFont>` resolved via `get_or_parse_font`; `rasterize` /
  `rasterize_by_gid` use it. The face is hashed once per font per render (callers hold the
  `Rc`), not per glyph.
- Effect: the block's CID font is parsed **once** ever (survives reparse via the content
  key), so `text_edit_render_block` no longer re-parses it each keystroke → editor box is
  responsive on any page.

### Part 1B — Cross-render decoded-image cache (`render::image`)
- Added `decode_image_cached(...) -> Result<Rc<RgbaImage>>` backed by a `thread_local`
  LRU (`ImageCache`) keyed by `hash(raw) + filter + w + h + colorspace + bpc`. Bounded by
  entry count (64) and total bytes (192 MB), LRU eviction.
- `draw_image_xobject` decodes the main image and the SMask through it; an SMask copies the
  shared pixels before `apply_smask`, otherwise the cached `Rc` pixels are blitted directly.
- Effect: each distinct image is decoded once and reused across every later
  render/scroll/commit → full-page renders on image-heavy pages stop re-decoding.

### Part 2 — Editor box hugs the text (web `composeBlock`)
- `web-editor/src/components/AnnotationOverlay.vue`: the white-cover width is now driven by
  the **live** engine text width (`liveWidthPts * scale`), `max`'d with the rendered glyph
  bitmap width; `glyphCoverW` no longer floors at the static `block.svgW`, and a
  `glyphClearW` running-max governs the erase so shrinking on delete leaves no leftover.
  Reset on open. The box now grows/shrinks per keystroke with no re-click.

## Design Decisions
- **Content-addressed keys** (hash of font/image bytes) so caches survive the per-commit
  document reparse and can't collide across documents — no explicit invalidation needed.
- **`thread_local` + `Rc`** (WASM is single-threaded; native test threads each get their own
  cache) — no locking, no atomics; cache hits share pixels/faces without cloning.
- Hash the font bytes **once per render per font** (via the per-render `fonts` map), never
  per glyph, to keep full-page renders cheap.
- Caches are pure memoization → byte-identical output (verified by test).

## Test Coverage
- `image::tests::image_cache_hit_reuses_rc_and_is_byte_identical` — repeated key returns the
  same `Rc` (no re-decode) and equals an uncached `decode_image`; different params don't
  collide.
- `glyph_cache::tests::parsed_font_cache_reuses_same_rc` (fixture-gated) and
  `parsed_font_cache_rejects_garbage`.
- Full suites green: `cargo test --features render` (315), `--features writer` (464).

## Known Limitations / Follow-up
- Glyph *rasters* are still per-`PageRenderer` (only the parsed face is shared); full-page
  renders re-rasterize glyphs (cheap relative to font parse + image decode). Could be
  content-addressed later if needed.
- The first (cache-miss) render of a heavy page still runs on the main thread; a Web Worker
  renderer (deferred) would remove that last freeze.
- End-to-end smoothness on page 2+ to be confirmed in the running app.
