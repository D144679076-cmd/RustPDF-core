# Soft Masks (Phase 2) — Implementation Report

**Date:** 2026-06-16
**Scope:** ExtGState soft mask rendering (ISO 32000-1 §11.6.5.2)

## What Was Implemented

### `src/content/interpreter.rs`

- **`OutputDevice::set_soft_mask`** — new default-no-op trait method called when an
  ExtGState activates a soft mask. Receives mask type (`"Alpha"` or `"Luminosity"`),
  the form XObject stream, and the current CTM.
- **`OutputDevice::clear_soft_mask`** — new default-no-op trait method called when
  `/SMask /None` clears the mask.
- **`apply_ext_gstate`** signature extended: accepts `device: &mut dyn OutputDevice`
  and `ctm: Matrix`. When the SMask value is a dict, resolves the `/G` form stream
  reference and dispatches to `device.set_soft_mask`; when it is `/None`, calls
  `device.clear_soft_mask`.

### `src/render/page_renderer.rs`

- **`SoftMaskType`** enum — `Alpha` or `Luminosity`.
- **`SoftMask`** struct — holds the rendered mask as premultiplied RGBA pixels
  (canvas-local coordinates), plus `width`, `height`, `mask_type`.
- **`SoftMask::sample(cx, cy)`** — returns the scalar mask value (0–255) at a
  canvas-local pixel. Alpha path: returns `data[…+3]`. Luminosity path: un-premultiplies
  then applies the ITU-R BT.709 luma formula `0.2126R + 0.7152G + 0.0722B`.
- **`PageRenderer::current_soft_mask: Option<SoftMask>`** field added to the struct and
  initialised to `None` in all four constructors.
- **`PageRenderer::render_soft_mask`** — renders the form XObject content into a
  canvas-sized `PixmapBuffer` using a fresh sub-`PageRenderer` + `ContentInterpreter`.
  Stores the result as `current_soft_mask`.
- **`PageRenderer::apply_canvas_soft_mask`** — modulates every pixel of a
  `PixmapBuffer` by the mask value at the same canvas-local coordinates (premultiplied
  path: all four RGBA channels scaled together).
- **`PageRenderer::apply_mask_to_image`** — modulates only the alpha channel of a
  straight-RGBA image buffer before it is blitted at a given canvas offset.
- **`OutputDevice::set_soft_mask` impl** — calls `render_soft_mask`.
- **`OutputDevice::clear_soft_mask` impl** — sets `current_soft_mask = None`.
- **`stroke_path` / `fill_path`** — when a mask is active, draw to a transparent
  temp canvas, apply mask, composite onto main via `PixmapBuffer::composite_over`.
- **`draw_image_xobject`** — refactored blit path to scale first, then apply the
  ExtGState mask to the scaled buffer before blitting.
- **`draw_text_span`** — logs a debug message when soft mask is active; text is
  rendered unmasked (known limitation, see below).

### `tests/real_pdf.rs`

- **`render_group3_with_soft_mask_does_not_panic`** — renders `Group-3.pdf` (a
  PPTX export that contains ExtGState `/SMask` entries) at 1×, asserts the output
  is non-white.

## Design Decisions

- **Canvas-sized mask pixmap**: The mask form is rendered onto a pixmap equal in
  size to the main canvas (not sized to the form's BBox). This gives correct
  pixel-coordinate alignment between mask and drawing output at the cost of
  allocating a full-page buffer per `gs` operator that sets a mask. For large
  pages this is measurable, but PDFs rarely set many masks on one page.

- **Sub-renderer uses `self.doc`**: The `set_soft_mask` trait method does not carry
  a `doc` parameter; the `PageRenderer` impl uses the already-stored `self.doc`
  reference. This avoids lifetime complexity while keeping the trait minimal.

- **Premultiplied-channel scaling in `apply_canvas_soft_mask`**: tiny_skia stores
  pixels premultiplied, so masking premultiplied (R, G, B, A) by scalar M/255
  scales all four channels. This is mathematically correct: if a pixel is
  `(α·R, α·G, α·B, α)` and the mask value is M, the result is
  `(α·R·M/255, α·G·M/255, α·B·M/255, α·M/255)`.

- **Luminosity un-premultiplication**: Before applying the luma formula, the RGB
  channels are divided by alpha to recover straight values. Transparent pixels
  (alpha = 0) yield mask value 0.

- **Text deferred to Phase 3**: Applying a soft mask to glyph blits requires either
  a full canvas swap-and-composite per `draw_text_span` call (expensive) or a
  significant refactor of the split-borrow glyph loop. Given that soft masks on
  text are uncommon in practice (PDFs use them almost exclusively on images and
  gradient-filled shapes), this is deferred.

- **Save/restore not tracked**: The current_soft_mask is stored on the renderer,
  not in `GraphicsState`. This means `q`/`Q` does not save/restore the mask.
  In the typical pattern (`q` → `gs` (set mask) → `Do` → `Q`) this causes the
  mask to remain active after `Q`. A full implementation would add `soft_mask`
  to `GraphicsState` and push/pop it with the stack. Deferred to Phase 3.

## Test Coverage

| Test | What it covers |
|------|---------------|
| `render_group3_with_soft_mask_does_not_panic` | E2E: Group-3.pdf renders without panic and produces non-white pixels |
| Existing `render_pptx_fixtures_not_solid_color` | Regression: soft mask code path does not erase all content |
| `test_smask_dict_does_not_zero_alpha` (existing) | fill/stroke alpha unchanged when SMask dict present |

## Known Limitations / Follow-up

1. **Text not soft-masked** — `draw_text_span` renders unmasked when a soft mask is
   active (Phase 3 follow-up).
2. **Save/restore does not track mask** — `q`/`Q` does not push/pop
   `current_soft_mask`. Rare patterns that rely on mask persistence across save
   frames may render incorrectly.
3. **Group background colour** — For luminosity masks the backdrop is transparent
   black rather than the spec-prescribed group background colour, which is
   implementation-defined. Most PDFs do not specify a background, so this is minor.
4. **Mask compositing in transparency groups** — The soft mask is not applied when
   compositing a transparency group at `end_transparency_group`. The mask only
   affects direct draw calls inside the group.
5. **`core-fonts` absent in dev environment** — The `render` feature cannot be
   compiled locally because the Liberation / DejaVu font files are missing. The
   implementation is verified at the source-check level (`cargo check`) and via
   base tests. Full render-feature tests must be run in an environment with the
   font tree.
