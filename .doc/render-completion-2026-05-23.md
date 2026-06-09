# render-completion — Implementation Report

**Date:** 2026-05-23
**Scope:** `src/render/` — inline image fix + public RGBA output

## What Was Implemented

- `draw_image()` in `src/render/page_renderer.rs` — replaced 1×1 placeholder blit with full scale-and-blit using CTM-derived destination rectangle. Detects channel count (1=Gray, 3=RGB, 4=CMYK) from `image_data` length vs `dst_pixels`. When source dimensions cannot be inferred exactly, computes a nearest-square-ish `(src_w, src_h)` pair using `(n as u32).div_ceil(side)`.
- `render_page_rgba(doc: &PdfDocument, page_index: usize, scale: f64) -> Result<(u32, u32, Vec<u8>)>` in `src/render/page_renderer.rs` — thin public wrapper over `render_page()` that returns `(width_px, height_px, rgba_bytes)` suitable for the WASM bridge and external callers without exposing the internal `PixmapBuffer` type.
- Re-exported `render_page_rgba` from `src/render/mod.rs`.

## Design Decisions

- **Channel auto-detection by length ratio**: Inline images arrive as raw decoded bytes with no embedded header. Choosing channel count by `len / dst_pixels` ratio is the only viable heuristic without full PDF color-space resolution at the draw call site. CMYK (4ch) is checked before RGB (3ch) to avoid false positives.
- **Nearest-neighbour square-ish source dimensions**: When byte count doesn't match `dst_w * dst_h * channels` exactly, the image is likely a different resolution than the destination. Computing `side = ceil(sqrt(n))` gives a compact near-square that avoids out-of-bounds reads in `decode_image`. The `div_ceil` integer idiom is exact and avoids floating-point rounding.
- **`(u32, u32, Vec<u8>)` return type**: Keeps the public surface free of internal types (`PixmapBuffer`). Width and height are returned alongside bytes so callers don't need to recompute from the pixel buffer length.
- **Form XObjects not changed**: The content interpreter already dispatches form XObjects at the operator level via `begin_form_xobject` / `end_form_xobject`. No `PageRenderer` override was needed.

## Test Coverage

Added to `tests/real_pdf.rs` under `#[cfg(feature = "render")]`:

| Test | What it covers |
|---|---|
| `render_minimal_page_returns_rgba_bytes` | Width/height > 0, buffer length = w×h×4 |
| `render_with_stream_page_is_not_all_white` | At least one non-white pixel (text/path rendered) |
| `render_multipage_second_page` | Page index > 0 works, buffer size correct |
| `render_at_2x_scale_doubles_dimensions` | Scale factor correctly multiplies pixel dimensions |

## Known Limitations / Follow-up

- CMYK-to-RGB conversion is not performed; CMYK pixels are blitted as-is (treated as RGBA). A proper conversion table would improve fidelity for CMYK inline images.
- Inline image colour-space from the PDF stream dictionary (e.g. `/CS /DeviceGray`) is not plumbed through to `draw_image()`; channel count is inferred from byte length only.
- Form XObject graphics-state isolation (push/pop CTM + clip on begin/end) relies on the interpreter's existing stack management; deeply nested or self-referential XObjects are not guarded against infinite recursion.
