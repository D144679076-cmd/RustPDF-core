# render-chart-fix — Implementation Report

**Date:** 2026-05-28
**Scope:** PDF chart rendering — black rectangle fix for PPTX-exported PDFs

## What Was Implemented

### Bug fixes

1. **`src/content/interpreter.rs`** — `apply_ext_gstate` (SMask handling, lines ~771–788)
   - Removed the alpha-zeroing fallback for dict-valued `/SMask` entries.
   - Previously: `fill_alpha = 0.0; stroke_alpha = 0.0` → all chart fills become invisible.
   - Now: no-op with a `log::warn!` — content renders without the mask applied.

2. **`src/render/color.rs`** — `color_to_rgba` (line 42)
   - Changed `Color::Pattern(..)` arm from `[0, 0, 0, opaque_alpha]` → `[0, 0, 0, 0]` (fully transparent).
   - `stroke_path` calls `color_to_rgba` directly (no pattern guard). Returning transparent avoids black strokes when the stroke colour is a pattern.

3. **`src/content/graphics_state.rs`** — `Color::Pattern` variant
   - Added `Option<Vec<f64>>` tint field: `Pattern(String, Option<Vec<f64>>)`.
   - The tint stores the numeric prefix operands from `scn`/`SCN` for uncoloured tiling patterns.

4. **`src/content/interpreter.rs`** — `color_from_operands_or_pattern`
   - Now collects numeric operands before the trailing Name into `tint` and stores in `Color::Pattern`.

5. **`src/render/page_renderer.rs`** — `fill_path_with_pattern`
   - Added `tint: Option<Vec<f64>>` parameter.
   - For PatternType 1 (tiling):
     - PaintType 2 (uncoloured) with tint → converts tint to a solid `Color` and calls `fill_path_with_rule`.
     - PaintType 2 without tint → warns and skips (was already the behaviour, now explicit).
     - PaintType 1 (coloured) → warns and skips (full tiling render is future work).
   - PatternType 2 (shading) path unchanged.

6. **`src/render/page_renderer.rs`** — `render_page_rgba`
   - Unpremultiplies RGBA pixels before returning.
   - `tiny_skia` stores premultiplied RGBA; JavaScript `ImageData` expects straight RGBA.
   - Only semi-transparent pixels (alpha 1–254) are converted; fully opaque and transparent are left as-is.

## Design Decisions

- **SMask → no-op instead of zeroing**: Zeroing alpha is strictly worse than ignoring SMask — invisible content is a harder regression than content rendered without a mask. A proper SMask implementation can be layered on later.
- **Pattern → transparent in `color_to_rgba`**: Consistent with how unresolved pattern fills behave (`fill_path_with_pattern` returns early without painting). An invisible stroke is preferable to a spurious black one.
- **Tint stored in `Color::Pattern`**: Avoids threading a separate tint parameter through the graphics state. The tint is only meaningful when the colour is a pattern, so co-locating it is natural.
- **Tiling pattern fallback uses solid tint colour**: A correctly tiled pattern would require a content-stream interpreter loop and pixmap tiling, which is substantial work. Using the tint colour as a flat fill gives visually acceptable results for the common uncoloured-tiling case (most chart fills in PPTX exports).
- **Unpremultiply in `render_page_rgba` not at the JS boundary**: Keeping the conversion in Rust avoids duplicating it for every WASM caller and keeps the JS compositable.py.

## Test Coverage

| Test | Location | What it covers |
|------|----------|---------------|
| `test_smask_dict_does_not_zero_alpha` | `interpreter.rs` | Verifies default alphas are 1.0 (SMask zeroing removed) |
| `test_scn_with_tint_produces_pattern_with_tint` | `interpreter.rs` | Happy path: `0.5 0.3 0.1 /P1 scn` → `Pattern("P1", Some([0.5,0.3,0.1]))` |
| `test_scn_name_only_produces_pattern_no_tint` | `interpreter.rs` | `/P1 scn` → `Pattern("P1", None)` |
| `test_color_to_rgba_pattern_is_transparent` | `color.rs` | Both tinted and untinted Pattern → `[0,0,0,0]` |

## Known Limitations / Follow-up

- **Full soft-mask (SMask) rendering** is still not implemented. Content renders without the mask; shadows and semi-transparent chart elements will look incorrect but won't disappear.
- **Coloured tiling patterns (PatternType 1, PaintType 1)** are silently skipped. Chart fills using this type remain invisible. A future pass should rasterise the pattern's content stream into a tile buffer and repeat it.
- **ICCBased and Separation colour spaces** are treated as DeviceRGB/Gray by component count. Colour accuracy depends on the source data coincidentally mapping well to sRGB.
- **Premultiplied output from `render_tile`** — only `render_page_rgba` unpremultiplies. Any direct caller of `render_tile` receives premultiplied pixels; callers should be aware.
