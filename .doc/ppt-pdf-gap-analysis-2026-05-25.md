# PPT-to-PDF Compatibility — Gap Analysis

**Date:** 2026-05-25
**Scope:** pdf-core feature gaps relative to PowerPoint-exported PDFs

---

## What a PowerPoint-exported PDF typically contains

| Feature | Generator pattern |
|---|---|
| Slide backgrounds | Axial/radial shading (`/ShadingType 2` or `3`) |
| Shadows / glow | Transparency groups (`/Group << /S /Transparency >>`) + `/SMask` in ExtGState |
| SmartArt / shapes | Dense Bézier paths, often filled via shading patterns |
| Embedded fonts | Subset TrueType with custom `ToUnicode` CMap |
| Images | JPEG (`DCTDecode`) or PNG-sourced raw pixels |
| Colors | `[/ICCBased <stream>]` (tagged sRGB), sometimes `DeviceCMYK` |
| Indexed images | `[/Indexed /DeviceRGB hival <lookup>]` for palette images |
| PDF version | 1.5–1.7 with object streams (XRef streams + compressed ObjStm) |
| Text | `TJ` with fine kerning; CID fonts; `ToUnicode` CMap |

---

## Confirmed gaps (from code audit 2026-05-25)

### 1. Color space handling in `decode_image` — `src/render/image.rs`

**Status:** Partial.

`decode_raw()` in `image.rs` handles `DeviceGray`, `DeviceRGB`, `DeviceCMYK` by name.
Falls through with `log::warn!` and treats as 3-channel RGB for anything else.

**Problem for PPT PDFs:**
- `draw_image_xobject` in `page_renderer.rs` calls `dict.get("ColorSpace").and_then(|o| o.as_name())` — this returns `None` for array color spaces like `[/ICCBased <ref>]` or `[/Indexed /DeviceRGB 255 <lookup>]`.
- ICCBased with N=3 (sRGB tagged) falls back to DeviceRGB by accident (3-channel), but ICCBased with N=1 (gray) or N=4 (CMYK) will be decoded with the wrong channel count.
- Indexed images decode as corrupt (1-byte indices treated as 3-channel RGB).

**Fix:** Add array color space handling in `draw_image_xobject`:
- `[/ICCBased ref]` → resolve stream, read `/N`, route to DeviceGray/DeviceRGB/DeviceCMYK
- `[/Indexed base hival lookup]` → apply lookup table, then decode in base color space
- `[/CalRGB ...]` → treat as DeviceRGB (3 channels, approximate)
- `[/CalGray ...]` → treat as DeviceGray (1 channel)

---

### 2. Shading (`sh` operator) — `src/content/interpreter.rs`

**Status:** Complete stub.

```rust
// Line 445 in interpreter.rs
"sh" => {}
```

The `sh` operator names a shading in the Resources `/Shading` dict. PDF generators use it for:
- Slide background gradients
- Shape gradient fills (via shading patterns)

**Fix:** Implement `src/render/shading.rs` with:
- `Shading::parse(dict, doc)` for types 2 (axial) and 3 (radial)
- `ShadingFunction` for types 2 (Exponential) and 3 (Stitching)
- `rasterize_axial` / `rasterize_radial` working in pixel space via CTM transform
- Add `paint_shading` method to `OutputDevice` trait (default no-op)
- `PageRenderer::paint_shading` calls the rasterizer

---

### 3. Transparency groups — `src/content/interpreter.rs` + `src/render/page_renderer.rs`

**Status:** Partial.

The `gs` operator correctly extracts `ca` (fill alpha), `CA` (stroke alpha), and `BM` (blend mode) into `GraphicsState`. Alpha is applied when drawing text glyphs (`blit_alpha_mask` multiplies alpha). Blend modes are stored but never applied.

**Problem:** Form XObjects with `/Group << /S /Transparency >>` are rendered directly onto the main canvas with no offscreen compositing. Result: all transparency effects (shadows, glow, semi-transparent shapes) render fully opaque.

**Fix:**
- `PixmapBuffer::new_transparent(w, h)` — new buffer with clear background (all-zero RGBA)
- `PixmapBuffer::composite_over(src, alpha, blend_mode)` — porter-duff over with blend modes
- `OutputDevice::begin_transparency_group` / `end_transparency_group` — new hooks
- `PageRenderer` maintains `transparency_stack: Vec<(PixmapBuffer, f64, BlendMode)>`. `begin_transparency_group` swaps `self.canvas` with a fresh transparent buffer; `end_transparency_group` composites and swaps back.
- `ContentInterpreter::handle_do_form` detects `/Group << /S /Transparency >>` and calls the new hooks instead of `begin_form_xobject`.
- Blend modes: implement Normal, Multiply, Screen, Darken, Lighten (covers 95%+ of PPT effects).

---

### 4. Soft masks (SMask in ExtGState) — `src/content/interpreter.rs`

**Status:** Not implemented.

`apply_ext_gstate` reads `CA`, `ca`, `BM` but does not handle `/SMask`. SMask is the mechanism behind PPT drop shadows and outer glow effects.

**Fix:**
- In `apply_ext_gstate`, detect `/SMask` dict with `/S /Alpha` or `/S /Luminosity` and `/G` (Form XObject ref).
- Render the mask Form XObject into a grayscale buffer.
- Store the mask in `GraphicsState::soft_mask`.
- Apply in `composite_over`: multiply effective alpha by mask pixel value.

---

### 5. Pattern color spaces — `src/render/color.rs`

**Status:** `Color::Pattern(_)` returns opaque black.

**Fix (partial):** When the current color space is `/Pattern` and the `scn`/`sc` operator sets a pattern name, resolve the pattern dict from Resources. If `/PatternType 2` (shading pattern), call the shading rasterizer clipped to the current path bounding box.

---

### 6. Object stream resolution (Type-2 XRef entries)

**Status:** ALREADY IMPLEMENTED — not a gap.

`src/parser/objects.rs` `get_from_obj_stream()` fully handles type-2 XRef entries with a per-stream cache. The simpler `xref.rs` parser skips them, but the full `PdfDocument::get_object()` path correctly resolves compressed objects.

---

## Implementation Priority

| Priority | Feature | Impact on PPT rendering | Complexity |
|---|---|---|---|
| P0 | Write this analysis doc | — | trivial |
| P1 | ICCBased + Indexed color spaces | Medium — fixes color distortion on indexed images | Low |
| P2 | Axial + radial shading | **High** — gradient backgrounds + fills | Medium |
| P3 | Transparency group compositing | **High** — shadows, layers, opacity | Medium-High |
| P4 | Blend modes (Multiply, Screen) | High — shapes with blend effects | Medium |
| P5 | Soft masks (SMask) | Medium — drop shadows, glow | High |
| P6 | Shading patterns (Pattern type 2) | Medium — gradient-filled shapes | Medium |

---

## Files to modify

| File | Change |
|---|---|
| `src/render/image.rs` | No change — `decode_image` already handles DeviceGray/RGB/CMYK correctly |
| `src/render/page_renderer.rs` | Fix color space extraction; add `paint_shading`; add transparency stack |
| `src/render/canvas.rs` | Add `new_transparent`, `composite_over` |
| `src/render/shading.rs` | **New file** — shading types, function eval, rasterizers |
| `src/render/mod.rs` | Add `pub mod shading` |
| `src/content/interpreter.rs` | Wire `sh` operator; detect transparency groups; update `begin/end_form_xobject` hooks |
| `src/content/interpreter.rs` | `OutputDevice`: add `paint_shading`, `begin_transparency_group`, `end_transparency_group` |
