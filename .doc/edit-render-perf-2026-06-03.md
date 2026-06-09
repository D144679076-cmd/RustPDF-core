# Edit-Render Performance — Implementation Report

**Date:** 2026-06-03
**Scope:** per-keystroke text-edit preview render (`wasm::text_edit`, `editor::edit_session`, web overlay)

## What Was Implemented

Editing a text box on a content-heavy page (page 2+) lagged and went stale on each
add/delete. Each keystroke synchronously re-rendered the **whole page** (all text frames +
all images) and cropped the block out, and re-inverted the font CMap every time. Three
changes remove that cost.

### Opt 1 — Block-only edit render (`pdf-core`)
- `src/editor/edit_session.rs`: added `pub(crate) fn edit_render_content_ops(ops, block_op_idx)`
  — drops every text show op (`Tj`/`TJ`) not in the edited block plus all image `Do` ops,
  keeping all state ops (`cm`/`q`/`Q`/`rg`/`Tf`/`Tm`/…) so the CTM and the full-page crop
  math are unchanged.
- `src/wasm/text_edit.rs` (`text_edit_render_block`): the override path now runs the cloned
  page ops through `edit_render_content_ops` before serializing. Per-keystroke cost drops
  from O(whole page incl. images) to O(the edited run). Visually identical — the host
  white-covers the block and overlays only its cropped region, so other content was never
  shown in the preview.

### Opt 2 — Reuse the open block's metrics (`pdf-core`)
- `PdfFontMetrics` got `#[derive(Clone)]`.
- `ActiveTextEdit` got `render_metrics: Option<PdfFontMetrics>` (the raw `font_metrics_for`
  result), populated in `text_edit_open`. `text_edit_render_block` reuses it for the active
  block instead of re-inverting the ToUnicode CMap on every keystroke; non-active blocks
  still resolve via `font_metrics_for`.

### Opt 3 — Coalesced, single-flight glyph render (web)
- `web-editor/src/components/AnnotationOverlay.vue`: `afterEngineEdit` keeps the instant
  `composeBlock` (caret/selection from cache) but routes the WASM re-raster through
  `scheduleGlyphRender` → `runGlyphRender`: at most one render in flight, rAF-coalesced, and
  re-rendered once on completion if the text changed meanwhile (latest-wins). Pending
  renders are cancelled in `cancelBlockEdit`/`onUnmounted` so a late render can't blank the
  held commit preview.

## Design Decisions
- **Keep the full-page CTM + crop path**, only feeding fewer ops — avoids re-opening the
  flip-`cm` sub-tile mis-map bug documented in `text_edit_render_block`, while removing the
  image-decode and other-glyph-shaping cost.
- **Keep all state ops (not just the block's)** so the block inherits exactly the same text
  state as in a full-page render → pixel-identical preview.
- Helper placed in `editor::edit_session` (writer-gated) rather than the wasm module so it's
  covered by the standard `cargo test --features writer` gate.

## Test Coverage
- `edit_render_keeps_block_run_drops_others_and_images` (edit_session): asserts a crafted op
  stream keeps the block's `Tj` + all state ops, drops the other `Tj` and the image `Do`,
  and the surviving show op carries the block's bytes.

## Known Limitations / Follow-up
- The full-page-sized pixmap is still allocated/cleared per render (cheap memset vs. the
  removed op work); a correct sub-tile render would remove it but risks the flip-`cm`
  mis-map — deferred.
- Verified by build + unit test; end-to-end smoothness on page 2+ to be confirmed in the
  running app (fast type / hold-backspace).
