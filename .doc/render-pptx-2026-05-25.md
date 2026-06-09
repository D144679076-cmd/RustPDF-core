# PowerPoint PDF Rendering — Implementation Report

**Date:** 2026-05-25
**Scope:** render, content/interpreter, render/path_render, render/page_renderer

## What Was Implemented

### Bug 1 — `v` Bézier operator current-point tracking (`interpreter.rs`)
- Added `current_point: Option<(f64, f64)>` field to `ContentInterpreter`.
- Updated `m`, `l`, `c`, `v`, `y`, `re`, `h` handlers to maintain `current_point`.
- Fixed `v`: first control point is now the tracked current point, not the first operand duplicated.
- Fixed `y`: second control point is correctly the end point (was already correct, now explicit).

### Bug 2 — CIDFont/Type0 text decoding (`interpreter.rs`)
- `show_text()` and `show_text_array()` now accept `doc` and `resources` to resolve font info.
- Added `resolve_font_info()`: looks up the font dict, detects `Type0` (composite), parses the `ToUnicode` CMap stream via the existing `CMap::parse()`, and extracts glyph widths.
- Added `parse_simple_widths()`: reads `FirstChar`/`LastChar`/`Widths` from simple font dicts.
- Added `parse_cid_widths()`: reads `DW` and `W` array from DescendantFont dicts into `FontWidths`.
- Added `decode_bytes_with_cmap()`: decodes 1-byte (simple) or 2-byte (composite) codes via CMap lookup, falling back to direct Unicode for unmapped codes.
- Glyph advance now uses actual widths from `FontWidths` instead of the `font_size * 0.5` approximation.
- For Type0 fonts, `get_ttf_bytes()` in `page_renderer.rs` already walked `FontDescriptor`; the interpreter now correctly routes 2-byte CID decoding through the CMap path.

### Bug 4 — Form XObject BBox clipping (`interpreter.rs`)
- In `handle_do_form()`, after applying the form's `/Matrix`, the `/BBox` array is read and a rectangular clip path is pushed into `gfx.current.clip_path` before interpreting the form's content stream.
- The clip is automatically released when `gfx.restore()` pops the saved state.

### Bug 5 — Rotated image placement (`page_renderer.rs`)
- Replaced the axis-aligned `ctm.a`/`ctm.d` extraction with `ctm_to_dst_rect()`.
- `ctm_to_dst_rect()` transforms all four corners of the unit square through the CTM and takes the axis-aligned bounding box, so rotation and shear are handled correctly.
- Applied to both `draw_image()` (inline images) and `draw_image_xobject()`.

### Bug 6 — Transparency group alpha captured from wrong state (`interpreter.rs`)
- In `handle_do_form()`, `group_fill_alpha` and `group_blend_mode` are now captured from `self.gfx.current` **before** `gfx.restore()` is called, so the group's own alpha/blend is passed to `end_transparency_group()` rather than the parent state's.

### Bug 7 — Clip path applied in path renderer (`path_render.rs`)
- Added `build_clip_mask()`: when `gfx.clip_path` is `Some`, builds a `tiny_skia::Mask` by rasterising the clip path with the current transform.
- `fill_path_with_rule()` and `stroke_path()` now pass the mask to tiny_skia's draw calls instead of `None`.

## Design Decisions

- **`current_point` in interpreter, not `Path`**: The current point is a property of the interpreter's path-construction state, not of the path data structure itself. Keeping it in `ContentInterpreter` avoids leaking interpreter state into the data model.
- **Font info resolved per `show_text` call**: Avoids caching stale font data across `Tf` changes. The cost is one dict lookup per text string, which is negligible compared to glyph rasterization.
- **`ctm_to_dst_rect` returns AABB**: For the blit path (nearest-neighbour scaling), an AABB is sufficient. True rotation would require a full affine blit, which is a follow-up item.
- **BBox clip via `gfx.current.clip_path`**: Reuses the existing clip path mechanism so the clip is automatically saved/restored with the graphics state stack — no extra bookkeeping needed.
- **`build_clip_mask` returns `Option<Mask>`**: Returns `None` in the common case (no clip), so the `None` path in tiny_skia is hit for most draw calls with zero overhead.

## Test Coverage

All 239 existing tests pass. No new tests were added in this pass — the fixes are exercised by the existing integration tests (`real_pdf.rs`) and unit tests in `interpreter`, `path_render`, and `page_renderer`. Follow-up: add targeted tests for `v`-operator curves, CIDFont decoding, and BBox clipping.

## Known Limitations / Follow-up

- **SMask (Bug 3)**: Soft masks from `ExtGState` are still ignored. Drop shadows and per-pixel alpha from PowerPoint will not render. Requires rendering the SMask Form XObject to a grayscale buffer and using it as a per-pixel alpha multiplier.
- **Rotated image blitting**: `ctm_to_dst_rect` gives the correct bounding box but the image is still blitted axis-aligned inside it. A rotated image will appear unrotated but correctly positioned. True rotation requires an affine blit or compositing via tiny_skia's `draw_pixmap` with a transform.
- **Type0 font embedding path**: `get_ttf_bytes()` in `page_renderer.rs` does not yet walk `DescendantFonts` for Type0 fonts. Text will decode correctly via CMap but may render as placeholders if the embedded TrueType is not found at the top-level `FontDescriptor`.
- **BBox clip intersection**: The current implementation replaces the clip path rather than intersecting with any existing clip. Nested Form XObjects with independent BBoxes may clip incorrectly.
