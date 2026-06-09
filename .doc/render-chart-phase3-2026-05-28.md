# render-chart-phase3 — Implementation Report

**Date:** 2026-05-28
**Scope:** PDF chart rendering — image SMask support + transparency group spec fix

## What Was Implemented

### 1. `apply_smask` helper — `src/render/image.rs`

New public function that applies a decoded grayscale SMask to an RGBA pixel buffer in-place. Each grayscale byte becomes the alpha of the corresponding RGBA pixel. Called from `draw_image_xobject` after the color image is decoded.

### 2. SMask handling in `draw_image_xobject` — `src/render/page_renderer.rs`

After decoding the main image, the function now:
1. Checks `dict.get("SMask")` for an indirect reference to a grayscale alpha stream.
2. Resolves the reference via `self.doc.resolve(...)`.
3. Decodes the SMask stream as `DeviceGray` using the same `decode_image` pipeline.
4. Verifies dimensions match the color image.
5. Calls `apply_smask(&mut img.data, &gray)` to write the gray values as alpha.

This makes previously-opaque chart images transparent where the SMask is dark, exposing the white chart background.

### 3. `/ImageMask` early-return guard — `src/render/page_renderer.rs`

Added a check for the `ImageMask` boolean entry before any decode. Stencil images (1-bit masks painted with fill color) are skipped with a `log::warn!` — they were previously decoded as garbage pixels.

### 4. `handle_do_form` fill_alpha capture order — `src/content/interpreter.rs`

Per PDF spec §11.6.6, transparency groups are composited using the **calling context's** alpha and blend mode, not the group's internal end-state. Moved capture of `fill_alpha`/`blend_mode` to before `gfx.save()` and removed the post-interpret capture lines. The captured `caller_fill_alpha`/`caller_blend_mode` are now passed to `device.end_transparency_group(...)`.

## Design Decisions

- **SMask via `decode_image` reuse**: The SMask stream is a proper PDF image (width/height/bpc/filter). Reusing `decode_image` with `"DeviceGray"` handles both raw and DCT-compressed SMasks without a separate decode path.
- **Gray extraction from RGBA output**: `decode_image` on DeviceGray produces `(G, G, G, 255)` RGBA. Extracting `p[0]` (red = gray) as the alpha byte avoids a separate gray-only decode path.
- **Dimension mismatch → skip**: If SMask dimensions differ from the color image, silently skip rather than abort. Malformed PDFs should not crash rendering.
- **ImageMask → skip for now**: A correct stencil implementation needs the current fill color, which is not accessible in `draw_image_xobject`. Skipping is better than rendering garbage pixels.
- **Caller alpha in transparency groups**: Using end-of-group state was a spec violation that could produce incorrect compositing when the form modifies alpha internally. The parent's alpha is the correct compositing parameter.

## Test Coverage

| Test | Location | What it covers |
|------|----------|---------------|
| `test_apply_smask_full_alpha` | `image.rs` | smask=255 → rgba alpha unchanged at 255 |
| `test_apply_smask_zero_alpha` | `image.rs` | smask=0 → rgba alpha becomes 0 (transparent) |
| `test_apply_smask_partial` | `image.rs` | smask=128 → rgba alpha becomes 128, RGB unchanged |

## Known Limitations / Follow-up

- **`/ImageMask` stencil rendering** is still skipped. A future pass should paint using the current fill color where the 1-bit mask is 1, transparent where 0.
- **`/Mask` (color key masking)** is not yet implemented. Arrays of min/max per component that make matching pixels transparent are silently ignored.
- **SMask `Matte` pre-blending** (PDF spec §11.6.5.3) is ignored. Pre-multiplied SMask images will composit slightly incorrectly — acceptable for now.
- **JPEG SMasks**: SMask streams with `DCTDecode` are handled by `decode_image`'s JPEG path, but this is rare and untested.
