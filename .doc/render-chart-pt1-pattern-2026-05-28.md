# render-chart-pt1-pattern — Implementation Report

**Date:** 2026-05-28
**Scope:** PDF chart rendering — PatternType 1 (tiling pattern) rendering + diagnostic logging fix

## What Was Implemented

### 1. Removed scale gate from diagnostic logs

**`src/render/page_renderer.rs`** — `stroke_path`, `fill_path`, `draw_image_xobject`

Removed `if self.scale >= 0.5` guards from all three `log::debug!` blocks. Added `scale={:.2}` to each message. Logs now emit at any scale when `RUST_LOG=debug` is set, enabling diagnosis at the typical thumbnail scale of 0.15.

### 2. Pattern lookup now returns the PdfStream

**`src/render/page_renderer.rs`** — `fill_path_with_pattern`

Changed the closure return type from `Option<PdfDict>` to `Option<(PdfDict, Option<PdfStream>)>`. For stream-backed pattern objects the stream is now preserved and passed to `render_tiling_pattern`.

### 3. `render_tiling_pattern` — PatternType 1 content stream rendering

**`src/render/page_renderer.rs`** — new `PageRenderer::render_tiling_pattern` method

Implements ONLYOFFICE `RendererOutputDev::tilingPatternFill` fast-path in Rust:

1. **Extract tile geometry** — `BBox`, `XStep`, `YStep`, optional pattern `Matrix` from the pattern dict.
2. **Build `tile_ctm`** — `pat_matrix.concat(&state.ctm)` maps pattern space → tile-local pixels.
3. **Compute cell size** — transform all 4 BBox corners through `tile_ctm`, take the device-space bounding box.
4. **Render the cell once** into a small off-screen `PixmapBuffer` (`cell_w × cell_h`) using a sub-`PageRenderer` + `ContentInterpreter` with `initial_ctm = cell_ctm` (origin-offset `tile_ctm`).
5. For **PaintType 2** (uncoloured), pre-set `fill_color` / `stroke_color` to the tint in the sub-interpreter's initial state.
6. **Compute tiling grid** — `xi` / `yi` index ranges via `tiling_index_range` that cover the fill path's device bounding box.
7. **Blit cell at each (xi, yi)** into a full-canvas-size transparent buffer using `blit_premultiplied` (premultiplied source-over).
8. **Clip mask** — rasterise the fill path into a tiny_skia mask, zero out pixels outside the path.
9. **Composite** — `composite_over` onto the main canvas.

### 4. New helper functions

**`src/render/page_renderer.rs`** — file-scope helpers:

| Function | Purpose |
|----------|---------|
| `pdf_f64` | Extract `f64` from Integer or Real |
| `extract_bbox` | Parse `/BBox` array from pattern dict |
| `extract_matrix` | Parse `/Matrix` array from pattern dict |
| `tiling_index_range` | Compute (lo, hi) index range covering a clip region |
| `blit_premultiplied` | Premultiplied source-over pixel blit |

## Design Decisions

- **Cell-once render**: Rendering the cell content stream once and blitting mirrors ONLYOFFICE's fast path. For deterministic (non-random) patterns this is correct and avoids O(tiles × operators) interpreter overhead.
- **Pattern resources fall back to page resources**: PPTX-exported patterns typically have no `/Resources` of their own, or an identical copy. The fallback to page resources covers both cases without extra logic.
- **Premultiplied blit instead of `blit_rgba`**: tiny_skia produces premultiplied RGBA. Using a dedicated `blit_premultiplied` function avoids the double-alpha multiplication that would occur if `blit_rgba` (which expects straight-alpha) were used.
- **Full-canvas tiled buffer then `composite_over`**: Consistent with how shading patterns are composited. Avoids the need to apply fill_alpha and blend_mode per-pixel during tiling.
- **4096 px cell guard**: Prevents accidental OOM from malformed pattern dictionaries.

## Test Coverage

| Test | What it covers |
|------|---------------|
| `test_tiling_index_range_positive_step` | Positive step covers expected xi range |
| `test_tiling_index_range_negative_step` | Negative step (Y-flip) produces valid range |
| `test_blit_premultiplied_opaque_pixel` | Fully-opaque pixel blits to correct position |
| `test_blit_premultiplied_out_of_bounds_safe` | Out-of-bounds coordinates do not panic |

## Known Limitations / Follow-up

- **Rotated patterns**: `tiling_index_range` uses only `step_x_dx` and `step_y_dy` (diagonal components). Rotated tiling patterns (non-zero `b`, `c` in the pattern matrix) may produce a slightly over- or under-sized tile grid. The `±1` margin in `tiling_index_range` compensates for simple cases.
- **Clip-path interaction**: The sub-renderer inherits no clip path from the parent; the fill path clip is applied after tiling via the mask. Patterns that depend on the parent clip for correct rendering may produce slightly oversized fills at edges.
- **PaintType 2 colour inheritance**: Only the initial `fill_color` / `stroke_color` are pre-set; colour operators inside the pattern cell can still override them. A full implementation would set `ignoreColorOps` in the sub-interpreter.
- **Caller side-effects of `render_tiling_pattern`**: The `font_bytes_cache` and `glyph_cache` in the sub-renderer are not shared with the parent, causing re-decompression of pattern fonts. Acceptable for now since patterns rarely embed fonts.
