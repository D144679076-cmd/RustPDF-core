# render-text-rotation — Implementation Report

**Date:** 2026-05-27
**Scope:** Text matrix rotation and page /Rotate rendering

## What Was Implemented

### `src/content/text_state.rs`
- Added `char_advances_y: Vec<f64>` to `TextSpan` — per-character y-advance in pixel space
- Added `render_matrix_2x2: [f64; 4]` to `TextSpan` — the `[a, b, c, d]` components of the full render matrix, used by the renderer to rotate glyph bitmaps

### `src/content/interpreter.rs`
- Updated `show_text` to capture both `pre.e / post.e` (x) and `pre.f / post.f` (y) advances per character
- `char_advances_y` is populated in both composite and simple font paths
- `TextSpan` construction now fills `char_advances_y` and `render_matrix_2x2`

### `src/render/page_renderer.rs`
- `draw_text_span`: changed fixed `baseline_y` to mutable `pen_y`; advances both `pen_x` and `pen_y` per character
- `draw_text_span`: extracts `cos_t / sin_t` from `render_matrix_2x2`; when `|sin_t| > 0.01` routes to the new `blit_glyph_rotated` helper
- New `blit_glyph_rotated`: creates a tiny-skia `Pixmap` from the glyph alpha mask, then uses `draw_pixmap` with a `Transform::from_row(cos_t, sin_t, -sin_t, cos_t, tx, ty)` that rotates the bitmap around the pen position while accounting for bearing offsets
- `render_tile`: computes a `rot_matrix` from `page.rotate` (0/90/180/270) and pre-multiplies it into the initial CTM before executing the content stream
- Added `#[allow(clippy::too_many_arguments)]` to `blit_glyph_rotated` and the pre-existing `render_tiling_pattern`

### Other construction sites updated
- `src/display/mod.rs` (3 test `TextSpan` literals)
- `src/text/extractor.rs` (1 test helper)

## Design Decisions

- **y-advance tracking**: `char_advances_y = post.f - pre.f` (signed, not abs) because upward movement is negative in pixel space — the sign encodes direction
- **Rotation detection threshold `|sin_t| > 0.01`**: avoids the tiny-skia overhead for upright text (the common case); catches any meaningful rotation
- **`blit_glyph_rotated` uses tiny-skia `draw_pixmap`** instead of hand-rotating the pixel buffer, because tiny-skia already has an optimised affine-transform compositing pipeline
- **Bearing offset rotation formula**: the top-left of the rotated bitmap in canvas space is `(pen_x + cos·bearing_x + sin·bearing_y, pen_y + sin·bearing_x − cos·bearing_y)` derived from the standard "rotate around pen" affine composition
- **Page /Rotate matrix**: follows ONLYOFFICE `GfxState` constructor (`GfxState.cc:5049–5085`) — the four canonical matrices for 90°/180°/270° are pre-multiplied into the initial CTM before the Y-flip/scale base

## Test Coverage

All 260 existing tests continue to pass. No new tests were added in this commit; the change is a rendering fix best validated visually with the affected PDF.

## Known Limitations / Follow-up

- **Glyph cache does not key on rotation**: each rotated glyph is rasterised upright by fontdue, then the resulting `Pixmap` is created and transformed per draw call. For documents with heavy rotated text, adding `rotation_key` to the glyph cache would reduce allocations
- **Tile-render coordinate bug** (pre-existing): `blit_rgba` takes page-pixel coordinates and subtracts `origin`, but span positions are tile-local. The `blit_glyph_rotated` function correctly subtracts `canvas.origin` before building the transform; the upright `blit_alpha_mask` path retains the pre-existing behaviour
- **Skew / non-orthogonal transforms**: the current code handles pure rotation (orthogonal text matrices). General shear (`c ≠ 0, d ≠ 0` simultaneously) is not separately handled but will produce a reasonable approximation
