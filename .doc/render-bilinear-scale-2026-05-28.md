# render-bilinear-scale — Implementation Report

**Date:** 2026-05-28
**Scope:** Bilinear image scaling (Phase 5 — fix blurry chart rendering)

## What Was Implemented

### `scale_rgba_bilinear` — `src/render/page_renderer.rs`

Replaced `scale_rgba_nearest` with `scale_rgba_bilinear`. The new function maps each destination pixel centre to fractional source coordinates using the formula `(dx + 0.5) * src_w / dst_w - 0.5`, then blends the four surrounding source pixels with bilinear weights. A private `rgba_pixel` helper fetches a single RGBA tuple by (x, y) coordinate.

Both call sites in `draw_image_xobject` and `draw_inline_image` were updated from `scale_rgba_nearest` to `scale_rgba_bilinear`.

## Root Cause of Blurry Rendering

`scale_rgba_nearest` used integer-division nearest-neighbour sampling (`dx * src_w / dst_w`). When a PDF image's native dimensions differ from the rendered destination rectangle (common when rendering at non-1x scale or when DPI differs from PDF point size), nearest-neighbour produces:

- Pixel gaps / duplicated rows due to integer rounding
- Staircase artifacts on diagonal lines
- Perceived blurriness because aliased edges lower local contrast

OnlyOffice's C++ renderer delegates image placement to AGG/Skia which applies affine-filtered sampling automatically via the CTM matrix, producing smooth results for both upscaling and downscaling.

## Design Decisions

- **Pixel-centre sampling** (`(dx+0.5)*src_w/dst_w - 0.5`): aligns the centres of source and destination grids so the output is not shifted by half a pixel relative to the input. Standard practice in image resamplers.
- **Clamp-to-edge boundary handling**: fractional coordinates outside `[0, src_w-1]` × `[0, src_h-1]` are clamped so border pixels are replicated instead of producing index-out-of-bounds panics.
- **f32 arithmetic**: sufficient precision for typical image dimensions (up to ~16 k pixels), avoids `f64` overhead in a hot inner loop.
- **`rgba_pixel` helper**: extracted to avoid index arithmetic duplication across the four corner fetches.

## Test Coverage

| Test | What it covers |
|------|---------------|
| `test_bilinear_scale_identity` | src_w==dst_w, src_h==dst_h → output identical to input |
| `test_bilinear_scale_2x2_to_1x1` | Four identical pixels downscaled to 1×1 → colour preserved |
| `test_bilinear_scale_upscale_1x1` | Single pixel upscaled to 2×2 → all four outputs equal source |
| `test_bilinear_scale_horizontal_blend` | Black/white 2×1 → 1×1 blend ≈ 128 (50/50 centre sample) |

## Known Limitations / Follow-up

- **Downscaling with area averaging**: bilinear picks one source point per destination pixel. For extreme downscale ratios (e.g., 10:1) this misses source detail and can still produce aliasing. A proper area-average (box filter) or Lanczos kernel would handle this, but bilinear is a significant improvement over nearest-neighbour for the chart images in question.
- **16-bit images**: `rgba_pixel` returns `[u8; 4]`; images decoded as 16-bit per channel would need a separate scaling path. Not relevant for the current 8-bit chart images.
