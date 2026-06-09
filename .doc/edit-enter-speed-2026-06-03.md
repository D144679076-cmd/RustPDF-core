# Enter-edit-mode speed (heavier pages) — Implementation Report

**Date:** 2026-06-03
**Scope:** `wasm::text_edit` (model reuse), web overlay/store (drop redundant build)

## What Was Implemented

Entering edit mode (opening a text box) was slow on later, content-heavy pages because the
page's edit model was rebuilt **two–three times** per entry, and each build decodes the page
content and inverts every font's ToUnicode CMap — cost that scales with page content.

### 1 — Drop the redundant legacy frame build (web)
The Word-style overlay already builds its block model via `text_edit_enter`. The legacy
`enter_edit_mode`/`editFrames` path was built in parallel (a second `build_edit_session` per
page) yet its only consumer was box metadata in `textBlocks`:
- box height is clamped to `font_size × scale`, so the real ascent/descent are irrelevant;
- `group.color/alpha` are unused (the Canvas2D text path is gone);
- `group.frames[0]` was used only by the `replaceText` fallback — rewired to use the
  block's own `group.x/y/font_name/font_size` (from `text_edit_enter`).
- `web-editor/src/stores/usePdfStore.ts`: removed the `enterEditMode` calls in `setPage`,
  `onPageScrolledIntoView`, and `setTool` (kept `exitEditMode` for cleanup).
- `web-editor/src/components/AnnotationOverlay.vue`: `commitBlockEdit` fallback no longer
  needs `editFrames`.

### 2 — Reuse the built text model on re-entry (`pdf-core`)
`text_edit_enter` runs again on every `openEditor` (`reenterForPage`) and on page display.
Added `WasmEditor.text_edit_model_pool_len`; `text_edit_enter` now returns the existing
model's blocks when re-entering the **same page with the same writer-pool size** (i.e. no
new edits since), skipping the decode + per-font CMap inversion. A commit changes the pool
size, so the model is correctly rebuilt after an edit.

## Design Decisions
- Removing `enterEditMode` is safe because the box height is clamped regardless and the only
  live datum (`f.*`) is duplicated on the Word-style block (`group.*`).
- The reuse key is `(page, writer_pool_len)` — content changes (commits, draws) bump the
  pool size and invalidate it, so reuse can never serve a stale model.
- Single-entry reuse (the current model) targets the common case — opening several blocks on
  one page — without holding multiple large models in memory.

## Test Coverage
- `cargo test --features writer` (464) and `--features render` (315) green; no behavior
  change to the model contents (pure reuse), so existing edit-session/text-model tests cover
  correctness.

## Known Limitations / Follow-up
- The **first** entry of a page still builds once (decode + metrics). A content-addressed
  font-metrics cache (like the font/image caches) would speed that first build too — deferred.
- Single-entry reuse can thrash if other pages' overlays enter between opens; a small
  multi-page model cache would remove that, at some memory cost.
