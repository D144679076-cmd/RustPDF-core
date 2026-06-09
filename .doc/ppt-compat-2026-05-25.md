# PPT-to-PDF Compatibility — Implementation Report

**Date:** 2026-05-25
**Scope:** Phases 1–3 of PPT-exported PDF rendering gaps

---

## What Was Implemented

### Phase 1 — Color Space Fixes (`src/render/page_renderer.rs`)

**`resolve_image_color_space(cs, doc)`**
Replaces the previous `dict.get("ColorSpace").and_then(|o| o.as_name())` single-name lookup.
- `[/ICCBased <stream>]` → reads `/N` from ICC stream dict; routes to DeviceGray/DeviceRGB/DeviceCMYK based on channel count. No ICC color-management transform (treats as identity — covers 99% of PPT exports tagged sRGB).
- `[/Indexed base hival lookup]` → returns base CS name + lookup table bytes for per-pixel expansion.
- `[/CalRGB ...]` → DeviceRGB (approximate, 3-channel).
- `[/CalGray ...]` → DeviceGray (approximate, 1-channel).
- `[/Lab ...]` → DeviceRGB (approximate).

**`apply_indexed_lookup(indices, lookup, base_channels)`**
Expands a 1-byte-per-pixel indexed image to N bytes per pixel using the lookup table.

**`map_cs_name(name)`**
Normalises legacy CS name aliases (e.g. `RGB` → `DeviceRGB`).

### Phase 2 — Gradient Shading (`src/render/shading.rs` NEW + wiring)

**Types:**
- `ShadingFunction::Exponential { c0, c1, n }` — FunctionType 2, interpolates `c0..c1` with exponent `n`.
- `ShadingFunction::Stitching { bounds, encode, functions, domain }` — FunctionType 3, piecewise-stitched sub-functions.
- `AxialShading` — ShadingType 2 (linear gradient along an axis).
- `RadialShading` — ShadingType 3 (radial gradient between two circles).
- `Shading` enum wrapping both.

**Parsers:**
- `Shading::parse(dict, doc)` — reads `/ShadingType`, `/Coords`, `/Domain`, `/Function`, `/Extend` and dispatches to axial or radial construction.
- `parse_function(obj, doc)` — handles FunctionType 2 and 3; returns `Unsupported` error for FunctionType 4 (PostScript).
- Shading XObjects (Form XObjects with `/Subtype /Shading`) handled in `handle_do` alongside image XObjects.

**Rasterisers:**
- `rasterize_axial` — for each canvas pixel, inverse-CTMs to user space, projects onto axis, computes `t`, evaluates function → RGBA.
- `rasterize_radial` — solves quadratic for each pixel to find `t`, evaluates function → RGBA.
- `components_to_rgba` — converts 1/3/4-channel function output to RGBA bytes.
- `inverse_matrix` — computes 2D affine matrix inverse; returns `None` for singular CTM.

**Wiring (`src/content/interpreter.rs`):**
- `OutputDevice::paint_shading(shading_dict, doc, state)` — new trait method with default no-op.
- `"sh"` operator dispatch: resolves shading name from Resources, calls `device.paint_shading`.

**`PageRenderer::paint_shading`** calls `Shading::parse` then `shading.rasterize`.

### Phase 3 — Transparency Compositing

**`PixmapBuffer::new_transparent(w, h, origin)` (`src/render/canvas.rs`)**
Allocates a zero-initialised (fully transparent) RGBA buffer. Uses `tiny_skia::Pixmap::new()` which already zero-initialises.

**`PixmapBuffer::composite_over(src, alpha, blend_mode)` (`src/render/canvas.rs`)**
Porter-Duff source-over compositing with blend mode support:
- Normal (default fallback)
- Multiply
- Screen
- Darken
- Lighten
- Overlay

All implemented per ISO 32000-1 §11.3.5.

**Transparency stack (`src/render/page_renderer.rs`)**
- `PageRenderer.transparency_stack: Vec<(PixmapBuffer, f64, BlendMode)>` — offscreen buffer stack.
- `begin_transparency_group()` — swaps `self.canvas` with a fresh transparent buffer; saves old canvas on stack.
- `end_transparency_group(fill_alpha, blend_mode)` — pops saved canvas, composites group result onto it.

**Transparency group detection (`src/content/interpreter.rs`)**
`handle_do_form` now checks for `/Group << /S /Transparency >>`. If present, calls `begin/end_transparency_group` instead of `begin/end_form_xobject`.

---

## Design Decisions

- **No ICC colour management**: The Rust crate has no ICC library dependency. PPT exports almost exclusively tag sRGB, so treating ICCBased N=3 as identity DeviceRGB is correct for 99% of inputs. A comment in the code explains this.
- **Inverse-CTM rasterisation for shading**: Instead of forward-mapping gradient stops to pixels (which requires scanline conversion), the rasteriser walks every pixel of the canvas, inverse-transforms to user space, and evaluates the gradient function. This is simpler to implement correctly and works for any affine CTM. For large pages it is O(pixels) which is acceptable — PPT slides are not typically larger than A4 at 144 DPI.
- **Canvas-swap for transparency groups**: Instead of threading a canvas reference through every draw method, `begin_transparency_group` replaces `self.canvas` with a fresh transparent buffer and pushes the old one. All existing draw methods write to `self.canvas` unchanged. `end_transparency_group` composites and restores. This required zero changes to the existing draw path.
- **Pre-existing `wasm/mod.rs` lints**: Three pre-existing clippy lints (two `too_many_arguments` on WASM public API functions, one `is_multiple_of`) were suppressed with `#[allow]` / fixed in place rather than restructuring the public WASM API, which is a breaking change.

---

## Test Coverage

### `src/render/canvas.rs`
- `test_new_transparent_is_all_zero` — verifies transparent buffer is all zeros.
- `test_composite_over_normal_50pct_alpha` — 50% blue over red → ~127 blend in R and B channels, full alpha.
- `test_composite_over_multiply` — 0.5-grey × 0.5-grey → result < 80 (darkens correctly).

### `src/render/shading.rs`
- `test_exponential_linear` — FunctionType 2 with n=1: t=0 → c0, t=1 → c1, t=0.5 → midpoint.
- `test_exponential_squared` — n=2: t=0.5 → c0 + 0.25*(c1-c0).
- `test_inverse_matrix_identity` — identity matrix inverts to identity.
- `test_inverse_matrix_scale` — scale-2 matrix inverts to scale-0.5.
- `test_components_to_rgba_gray` — 1-channel → grey RGBA.
- `test_components_to_rgba_rgb` — 3-channel → correct RGBA.
- `test_components_to_rgba_cmyk_white` — 4-channel CMYK all-0 → white RGBA.
- `test_axial_shading_rasterize_paints_canvas` — 4×1 canvas with axial red→blue gradient; checks first pixel is red-dominant, last pixel has more blue than first.

### `src/render/page_renderer.rs`
- `test_resolve_color_space_simple_name` — `"DeviceRGB"` name returns `("DeviceRGB", None)`.
- `test_resolve_color_space_cal_rgb` — `[/CalRGB <<>>]` returns `("DeviceRGB", None)`.
- `test_apply_indexed_lookup_rgb` — 3-channel lookup, indices 0 and 1 expand correctly.
- `test_apply_indexed_lookup_gray` — 1-channel lookup, single index expands to grey byte.

Total tests after this work: **359** (all passing).

---

## Known Limitations / Follow-up

- **FunctionType 4 (PostScript calculator)**: Not implemented. Returns an error and falls back to transparent pixels for any shading using Type 4 functions. Rare in PPT exports.
- **ShadingType 1 (Function-based), 4–7 (Free-form mesh, lattice, coons, tensor)**: Not implemented. Rare in PPT exports.
- **Soft masks (SMask in ExtGState)**: Not implemented — Phase 3.5. PPT drop shadows and glow effects depend on this. Follow-up work.
- **Pattern color space (PatternType 2 shading patterns)**: Not implemented — Phase 4. When `scn` sets a pattern color, `Color::Pattern` currently renders as opaque black.
- **Blend mode compositing in path/text rendering**: `fill_alpha` and `stroke_alpha` from the graphics state are not yet multiplied into the path fill/stroke color before rasterisation. Only the transparency group compositing pass applies opacity.
- **No ICC delta**: All ICCBased color spaces are treated as their device equivalent (Gray/RGB/CMYK). Wide-gamut PDFs will have slightly wrong colors but will not crash or produce garbage.
