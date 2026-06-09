# render-pptx-fix — Implementation Report

**Date:** 2026-05-27
**Scope:** Clip path correctness and diagnostic logging for PPTX-exported PDFs

## What Was Implemented

### `src/content/graphics_state.rs`
- Added `ClipEntry` struct `{ path: Path, rule: FillRule, ctm: Matrix }` — stores each clip layer together with the CTM frozen at the moment `W`/`W*` was applied.
- Changed `GraphicsState::clip_path` from `Option<(Path, FillRule)>` to `Vec<ClipEntry>` so multiple clip layers can be accumulated and intersected rather than the most-recent one replacing all prior clips.

### `src/content/interpreter.rs`
- `W` / `W*` operators now **push** a `ClipEntry` onto `gfx.current.clip_path` (preserving prior clips) instead of overwriting the field.
- Form XObject BBox clip (in `handle_do_form`) likewise pushes onto the stack instead of replacing, so the parent page/form clip is preserved when entering nested Form XObjects.
- `apply_ext_gstate` now detects `/SMask` in ExtGState dictionaries and emits a `warn!` log when one is found. Rendering behaviour is unchanged until full SMask support is implemented.
- Added `log::debug!` calls for `W`/`W*` operators logging the path segment count and existing clip depth.
- Added `log::debug!` for Form XObject entry logging name, BBox coordinates, and `is_transparency_group`.

### `src/render/path_render.rs`
- `build_clip_mask` rewritten: now takes `&[ClipEntry]` instead of `&GraphicsState`.
  - Starts with an all-255 (fully-open) mask.
  - For each `ClipEntry`, builds a separate tiny-skia mask using **the CTM stored in the entry** (not the paint-time CTM), then intersects with `min()` per byte.  This fixes both the intersection semantics (ISO 32000-1 §8.5.4 says clips are intersected, not replaced) and the coordinate-system bug (clip paths no longer drift when `cm` operators change the CTM after `W`).

### `src/render/page_renderer.rs`
- `fill_path` / `stroke_path` / `draw_image_xobject` now emit `log::debug!` with colour, alpha, user-space bounding box, and active clip-layer count.
- Added internal helper `path_bounding_box(path) -> (f64,f64,f64,f64)` for diagnostic bbox computation.

### `tests/real_pdf.rs`
- Added `render_pptx_fixtures_not_solid_color` test (gated behind `feature = "render"`) that renders page 0 of each PPTX-exported fixture (`Group-3.pdf`, `Laspeyres_and_Paasche.pdf`, `Unit_1.pdf`) and asserts the output is not a single flat colour — which would indicate that background shapes are obliterating all content due to a clip or compositing regression.

## Design Decisions

- **`Vec<ClipEntry>` instead of `Option<_>`**: PDF spec §8.5.4 says the current clipping path is the intersection of all active clip modifications.  Accumulating in a Vec mirrors the spec precisely and allows the renderer to intersect at mask-build time with no data loss.
- **CTM frozen in `ClipEntry`**: The clip path is expressed in user-space coordinates valid at `W` time.  Storing the CTM alongside each entry means `build_clip_mask` always applies the *right* transform even when `cm` modifies the CTM later in the same content stream.
- **`min()` intersection in `build_clip_mask`**: tiny-skia's `Mask` stores 8-bit coverage values; `min(a, b)` gives the correct intersection for anti-aliased coverage.  A bitwise AND would give wrong results for non-binary coverage.
- **SMask stub only**: Full soft-mask rendering requires rendering a grayscale Form XObject and multiplying per-pixel alpha.  This is deferred; the stub ensures SMask presence is visible in logs without silently ignoring it.

## Test Coverage

- `render_pptx_fixtures_not_solid_color` — happy-path: three PPTX-exported fixtures must not reduce to a flat colour at 1× scale.  Skips gracefully if a fixture is not present.
- All 303 existing unit + integration tests pass unchanged.

## Known Limitations / Follow-up

- **Full SMask support** not yet implemented.  PPTX backgrounds use `/SMask` for drop-shadows and soft transparency.  Without it, those shapes render fully opaque.  Requires: render mask Form XObject to 8bpp greyscale, store result, and multiply into `fill_alpha`/`stroke_alpha` during path painting.
- **Tiling pattern fills** (PatternType 1) are still unsupported and log a warning.
- **Clip interaction with transparency groups**: when a transparency group is composited back onto the canvas the clip mask is not re-applied at composite time — only at individual paint calls within the group.  This is usually correct but may produce edge artefacts for groups with non-rectangular clips.

---

## Phase 2 — SMask zero-alpha fallback + scale-gated logging (2026-05-27)

### What Was Added

#### `src/render/page_renderer.rs`
- `[fill]` and `[stroke]` debug logs now guarded by `if self.scale >= 0.5`.  Thumbnail renders (scale=0.15 from `PageThumbnail.vue`) no longer emit fill/stroke logs, preventing them from flooding the browser console before the main-page render logs appear.
- `[image]` log in `draw_image_xobject` similarly guarded.

#### `src/content/interpreter.rs`
- SMask stub upgraded to a behavioral fallback: when a dict-valued `/SMask` entry (not "None") is found in an ExtGState, both `fill_alpha` and `stroke_alpha` are set to `0.0`.  This makes SMask shapes invisible instead of fully opaque, preventing them from covering page content until full SMask rendering is implemented.

### Design Decisions

- **Scale threshold 0.5**: thumbnails use `THUMB_SCALE=0.15`; main renders use `scale * dpr ≥ 1.0`.  Any value in (0.15, 1.0) works; 0.5 is a clear midpoint with no risk of catching intermediate zoom levels.
- **Zero-alpha vs skip**: zeroing alpha rather than skipping the paint call keeps the graphics state changes (path construction etc.) consistent with the spec, and the zero-alpha path through tiny-skia is a no-op at the pixel level.

### Known Limitations (updated)

- **Zero-alpha SMask** is a conservative fallback: shapes with SMask become invisible rather than semi-transparent.  Visual fidelity requires full SMask: render the `/G` Form XObject to 8bpp greyscale, store in `GraphicsState`, multiply into per-pixel alpha at paint time.

---

## Phase 3 — Root cause: font size in device pixels (2026-05-27)

### Root Cause Identified

After Phase 2, `[fill]` logs showed **only white fills** on the main page — the colored blobs were confirmed to originate from **text rendering**, not path fills.

The bug was in `src/render/page_renderer.rs` `draw_text_span`:

```rust
// WRONG — misses text matrix scale factor
let size_px = (span.font_size * self.scale as f64).abs() as f32;
```

- `span.font_size` = raw PDF Tf value (user-space units, e.g. 12.0)
- `self.scale` = DPI/tile scale only (e.g. 1.0)
- **Missing**: text matrix scale embedded via `cm` operators in the content stream

Meanwhile `char_advances` were computed using the full render matrix delta (`get_render_matrix().e` before/after `advance_glyph`), which correctly applies `font_size × text_matrix_scale × CTM_scale`. The mismatch (e.g. `size_px=49px` vs `advance=2px`) caused glyphs to render ~23× too large, overlapping completely into solid colored blobs.

### What Was Fixed

#### `src/content/text_state.rs`
- Added `pub font_size_px: f64` to `TextSpan` — device-space font height computed at span creation time.

#### `src/content/interpreter.rs`
- At TextSpan creation, compute `font_size_px` as the magnitude of the Y-basis vector of the render matrix (handles rotated text correctly):
  ```rust
  let font_size_px = (render_matrix.b.powi(2) + render_matrix.d.powi(2)).sqrt();
  ```
- Also added `char_cids: Vec<u32>` initialization to the TextSpan creation (field already existed in struct but was missing from the initializer).

#### `src/render/page_renderer.rs`
- Replaced `span.font_size * self.scale` with `span.font_size_px` — the pre-computed device-space size already includes the DPI scale via the initial CTM, so no further multiplication is needed.

#### `src/display/mod.rs` + `src/text/extractor.rs`
- Updated three test-only TextSpan initializers to include `font_size_px`.

### Design Decisions

- **Store in TextSpan, not recompute at render time**: the render matrix at draw time differs from the span-creation matrix if the CTM changed between span creation and rendering (unlikely but possible in transparency groups). Freezing the value at creation is simpler and more correct.
- **Y-basis vector magnitude (`hypot(b, d)`)**: for non-rotated text, `|d|` suffices; using `hypot` is costless and handles rotated text without a special case.
- **`font_size` kept as-is**: it remains useful for text extraction and metadata. Only the rasterization path was changed.

### Test Coverage

- All 303 existing unit + integration tests pass.
- Test-only TextSpan initializers in `display/mod.rs` and `text/extractor.rs` set `font_size_px = font_size` (identity — correct for unit tests which do not exercise WASM rendering).

### Known Limitations (updated)

- **SMask zero-alpha** makes masked shapes fully invisible instead of semi-transparent. Full SMask implementation is deferred.
- **Tiling pattern fills** (PatternType 1) not yet supported.
- **Clip × transparency group** edge case deferred.

