# Glyph Shear/Rotation Anti-Aliasing — Implementation Report

**Date:** 2026-06-09
**Scope:** `render::page_renderer` — synthetic-italic / rotated glyph rasterization quality

## What Was Implemented

Fixed blurry text in the edit-preview tile (and anywhere on a page) when a glyph is drawn
with a shear (synthetic italic) or rotation. Changes in
[`src/render/page_renderer.rs`](../src/render/page_renderer.rs):

- Added `const GLYPH_SUPERSAMPLE: f32 = 3.0`.
- `draw_text_span`: compute a per-span supersample factor `ss` (= `GLYPH_SUPERSAMPLE` when
  `has_rotation || has_shear`, else `1.0`) and `raster_px = size_px * ss`. Both tier-1
  (embedded) and tier-2 (bundled) glyph lookups now rasterize at `raster_px`. The fontdue
  fallback advance is normalized back to 1× (`glyph.advance_x / ss`). Synthetic-bold
  `embolden_radius` now scales off `raster_px`.
- `blit_glyph_sheared` / `blit_glyph_rotated`: added an `ss: f32` parameter; fold a `1/ss`
  downscale into the affine `Transform` (`from_row(inv,0,-skew*inv,inv,…)` for shear;
  `from_row(cos*inv,sin*inv,-sin*inv,cos*inv,…)` for rotation), divide the supersampled
  bearings back to 1×, and switch the `PixmapPaint` from the default `FilterQuality::Nearest`
  to `FilterQuality::Bilinear`.

Upright glyphs are untouched: `ss == 1.0`, they keep the direct, integer-snapped
`blit_alpha_mask` path and are byte-for-byte identical to before.

## Root Cause (why the change was needed)

The edit-preview tile renders through the same renderer and the same `scale` as the page, and
is blitted 1:1 in the browser — so it is *not* a resolution/DPI problem. The only difference
for a sheared (synthetic-italic) block was the glyph blit branch: upright glyphs use the crisp
direct `blit_alpha_mask`, but sheared/rotated glyphs were rasterized **upright** by fontdue and
then re-sampled through `tiny_skia::draw_pixmap` with an affine transform. Re-sampling an
already-anti-aliased 1× coverage mask smears the edges — the blur the user saw after toggling
Italic. Rasterizing a supersampled master and folding the downscale into the affine lets the
resample read from high-resolution source, so edges stay crisp.

## Design Decisions

- **Supersample + bilinear downscale, not outline path-fill.** fontdue exposes no outline/
  transform API, so the clean "transform-then-rasterize" fix would require adding `ttf-parser`
  outlines + `tiny_skia` path fill — a larger change. Supersampling reuses the existing
  fontdue + tiny_skia stack, is localized to the two affine-blit paths, and preserves geometry
  exactly (verified by test).
- **SS = 3.** 9× source area only on sheared/rotated/emboldened spans (rare); the common
  upright per-keystroke preview is unaffected. The glyph cache key already quantizes on
  `size_px*64`, so the supersampled master caches under a distinct key automatically — no new
  cache map.
- **Applied to the page renderer, not just the preview.** Both share `draw_text_span`, so
  genuinely italic/rotated PDF content on any page now renders crisply too.

## Test Coverage

`src/render/page_renderer.rs` (`#[cfg(feature = "render")]` tests):
- `blit_glyph_sheared_leans_top_right` (updated): unchanged behaviour at `ss = 1.0` — the
  sheared bar still leans right at the top.
- `blit_glyph_sheared_supersampled_matches_geometry` (new): the same logical glyph blitted at
  `ss = 1.0` (1× mask) and `ss = 3.0` (3× mask) lands in the same on-screen bounding box
  (±2 px) — proving the `1/ss` fold introduces no size/position drift, only crisper edges.

Verification run: `cargo fmt --check` (clean), `cargo clippy --features render -- -D warnings`
(clean), `cargo test --features render` (357 passed), `cargo build --target
wasm32-unknown-unknown --features render,wasm` (ok), `make wasm` (pkg rebuilt into
`web-editor/src/pkg`).

## Known Limitations / Follow-up

- Supersampling reduces but does not perfectly equal true outline-level hinting; for the
  crispest possible synthetic italic/bold, the deferred outline path-fill (`ttf-parser` +
  `tiny_skia`) remains the ideal long-term fix.
- Synthetic **bold** (`embolden_glyph` max-filter) still softens slightly on its own; it is
  only supersampled when combined with shear/rotation. Not exercised in the reported case
  (the block's bold is intrinsic), but a candidate for the same outline-fill treatment later.
- SS = 3 increases glyph-cache memory and raster cost for rotated/sheared text; negligible for
  typical documents but worth revisiting if a page has heavy rotated text.
