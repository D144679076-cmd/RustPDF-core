# Edit-Preview Crop Fix + Log Quieting — Implementation Report

**Date:** 2026-06-02
**Scope:** wasm::text_edit (text_edit_render_block), render/content debug logs

## Problem
The edit-text preview rendered the **wrong page region**: clicking the title
("Vietnam: Banking System") showed the Group-Members student-ID text. Proven via
a headless test + PNG dumps of `Group-3.pdf` page 0.

Root cause: `text_edit_render_block` rendered a **sub-tile** through
`render_tile_content`. On pages with a top-level flip `cm`
(`[0.75 0 0 -0.75 0 792]` here), the sub-tile CTM double-applies the flip, so the
crop captures the wrong vertical band. Confirmed shared with stock `render_tile`
(a sub-tile of the title rendered the student-IDs; a sub-tile of the student-IDs
rendered blank). Block coordinates themselves were correct.

## What Was Implemented
- **`text_edit_render_block`** now renders the **full page** (the same proven
  path the page display uses) via `render_tile_content`, then **crops** the
  block's rectangle out of the full buffer. Crop rect uses the
  `TileRect::to_pixel_space` convention: `crop_y = (page_h - tile.y -
  tile.height) * scale`. Crop window is clamped to page bounds; RGBA is
  un-premultiplied for `ImageData`. Returns the crop's device-px top-left as
  `EditBlockRender { x, y }` so the web layer blits it at the right place.
- **Debug-log quieting** (user request: keep edit-render log, silence normal
  per-render spam). Commented out the high-frequency traces, leaving the code as
  documentation to re-enable:
  - `content/interpreter.rs`: `[text-span]`, `[clip] W`, `[clip] W*`
  - `render/page_renderer.rs`: `[draw-span]`, `[draw-char-0]`, `[stroke]`,
    `[fill]`, `[render_tile]`
  - Kept: new `[edit-render]` one-line-per-click log in `text_edit_render_block`.

## Design Decisions
- **Full-page render + crop, not a sub-tile CTM fix.** The sub-tile flip bug
  lives in the shared `render_tile` CTM; fixing that risks the whole renderer.
  Full-page render is already correct for display, so cropping from it is safe and
  identical to output. Cost: one full-page raster per preview render — acceptable
  for interactive single-block editing; a cache can be added later if needed.
- **Logs commented, not deleted.** They are valuable for future text-positioning
  debugging; left in-place behind comments with a re-enable hint.

## Test Coverage
- `tests/edit_render_block.rs::title_block_crop_contains_title_ink_in_one_band` —
  mirrors the production crop path on `Group-3.pdf`, asserts the title crop is one
  line wide+short, has substantial dark ink, and inks a contiguous band (not
  blank, not the whole page). This is the regression guard for the wrong-region
  bug.
- Full suite: 517 passed / 0 failed (`--features "render writer"`). fmt clean;
  clippy `-D warnings` clean for `writer` and `wasm-render`. `make` rebuilt the
  pkg (default `init`, `text_edit_render_block`, `EditBlockRender` all present).

## Known Limitations / Follow-up
- Composite/CID (CJK) blocks still render their **original** text (no live typing
  preview) until Phase 3 font subsetting; XObject-sourced blocks likewise. Those
  paths now at least crop the correct region.
- Per-keystroke full-page raster; add a full-page render cache if large pages lag.
- Write-back on commit still uses the operand-replace path (Phase 2 = surgical).
