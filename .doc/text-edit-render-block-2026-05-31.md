# Edit-Text Real-Renderer Preview (Option A) — Implementation Report

**Date:** 2026-05-31
**Scope:** render::render_tile_content, wasm::text_edit (EditBlockRender +
text_edit_render_block), web edit-text preview

## Problem
The edit-text on-screen preview diverged from normal page rendering: it used a
Canvas2D `ctx.fillText` with a guessed system/embedded font, so the **face, size
and width** did not match the real PDF glyphs (the original `render_edit_block`
WASM method the overlay called never existed). Box widths also overran the page
because simple-font metrics fell back to a 1-em default.

## What Was Implemented
- **`render/page_renderer.rs::render_tile_content`** — renders a `TileRect`
  region from caller-supplied content bytes (full content interpreted so colour/
  CTM/clips are preserved; only the tile is kept). Returns
  `((origin_x_px, origin_y_px), PixmapBuffer)`. Exported from `render/mod.rs`.
- **`wasm/text_edit.rs::EditBlockRender`** — wasm struct `{x, y, width, height}`
  + `rgba_bytes()`; **`WasmEditor::text_edit_render_block(block_id, scale)`**
  (gated `#[cfg(feature="render")]`) — builds the block's user-space bbox, and:
  - simple resolvable font + stream-0 block + open caret session → serialises the
    page ops with the block's show-text operand replaced by the engine's current
    text, renders that;
  - otherwise renders the **original** page content (pixel-identical positioning
    preview). Unpremultiplies RGBA for `ImageData`.
- **`WasmEditor.text_edit_model`** field retains the full `TextModel` (blocks +
  parsed streams) so the renderer can patch + re-render; set in `text_edit_enter`.
- **Web** ([AnnotationOverlay.vue], [usePdfStore.ts]) — `renderEditBlock` store
  wrapper; `drawEditBlock` now blits the Rust tile (device-px, dpr-scaled editor
  canvas), white-covers the original text beneath, and keeps the Canvas2D path
  only as a fallback. Caret/selection redrawn in device px. `activeRustBlockId`
  tracks the open block.

## Design Decisions
- **Reuse the page renderer, render the whole content cropped to a tile** rather
  than rasterising glyphs standalone — preserves all graphics state (colour set
  before `BT`, CTM, clips), so preview == saved output.
- **Override only simple, stream-0 fonts.** Composite/CID text re-encoding needs
  Phase 3 (font subsetting); those blocks render their original text so typing
  isn't previewed yet, but positioning/face/size are correct.
- **White cover before blit** — the tile is transparent (glyphs only); without a
  cover the original page text would show through edited text.
- `render_tile_content` returns the device-px origin so JS places the crop on the
  dpr-scaled editor canvas without re-deriving coordinates.

## Test Coverage
- Existing suite green (`cargo test --features writer`, 0 failures). fmt clean;
  clippy `-D warnings` clean for `writer`, `wasm`, `wasm-render`. wasm-render
  builds; `make` regenerates the pkg (`text_edit_render_block` + `EditBlockRender`
  present, default `init` export intact). Web `tsc --noEmit` clean.
- No new Rust unit tests: `text_edit_render_block` is a thin WASM/render
  orchestration over already-tested `render_tile`/`serialize_operations`/metrics.

## Known Limitations / Follow-up
- **Composite/CID (CJK) typing not previewed** — renders original text until
  Phase 3 CID re-encoding lands.
- **XObject-sourced blocks** (`stream_idx != 0`) render original text only.
- White cover assumes a white page background.
- Per-keystroke (and per-blink) full-content interpretation of the page; fine for
  typical pages, but a content/glyph cache could be added if large pages lag.
- Write-back on commit still uses the operand-replace path (Phase 2 = surgical).
