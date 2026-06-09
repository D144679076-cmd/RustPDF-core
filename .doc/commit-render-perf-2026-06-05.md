# Commit Render & Undo/Redo Performance — Implementation Report

**Date:** 2026-06-05
**Scope:** commit render fast path, undo/redo cache scoping

## What Was Implemented

### Rust (`src/wasm/text_edit.rs`)
- `WasmEditor::render_committed_block_tile(block_id, scale) -> EditBlockRender` — renders only the committed block's tile from the writer-pool content stream. Applies the same CoW overrides as `render_page`, builds a `TileRect` from the block's **full original width** (not the current text width), decodes the committed content, and calls `render_block_tile`. Returns the same `EditBlockRender` struct as `text_edit_render_block`.

### TypeScript (`web-editor/src/stores/usePdfStore.ts`)
- `commitRenderPage(pageIndex, blockId?)` — new fast path: when `blockId` is supplied, calls `render_committed_block_tile` → composites the tile onto the existing full-page cached bitmap via `OffscreenCanvas`. Falls back to `editorRenderPage` (full page) if no cached bitmap exists yet. Reduces commit render cost from O(full page) to O(block tile area).
- `textEditCommit` — passes `blockId` to `commitRenderPage` and `pageIndex` to `pushSnapshot`.
- `replaceText` — passes `pageIndex` to `pushSnapshot`.
- All single-page annotation/draw actions — pass `currentPage.value` to `pushSnapshot`.
- `undo()` / `redo()` — extract `pageIndex` from history result; call `renderQueue.markPageScope(pageIndex)` before `_restoreSnapshot` so the doc-swap watcher only invalidates that page instead of the entire cache.

### TypeScript (`web-editor/src/composables/useHistory.ts`)
- `HistoryEntry.pageIndex?: number` — optional page affected by the action.
- `pushSnapshot(bytes, label, pageIndex?)` — stores `pageIndex` in the entry.
- `popUndo` / `popRedo` return `{ bytes, pageIndex? }` instead of `Uint8Array | null`.

## Design Decisions

- **Full original block width for the tile**: after `delete_all`, the current text width is 0. Using `block.width` as the tile width ensures the entire old text area is covered when composited over the stale page bitmap.
- **OffscreenCanvas composite**: old-page bitmap + new tile drawn on an `OffscreenCanvas` gives a correct composited result without a full re-render. Falls back to `editorRenderPage` when no existing bitmap is cached (cold start).
- **`markPageScope` before doc swap**: the doc-watcher in `useRenderQueue.ts` checks `pendingPageScope` to decide between `invalidatePage` and `invalidateAll`. Calling it immediately before `_restoreSnapshot` (which swaps `doc.value`) ensures the flag is consumed by the right watcher fire.
- **Structural edits excluded**: `addBlankPage`, `deletePage`, `rotatePage`, `movePage`, `applyRedactions` leave `pageIndex` as `undefined` so `invalidateAll()` still fires correctly.
- **`jumpToHistory` unchanged**: multi-entry jumps may span structural changes; full cache clear is safe.

## Test Coverage

No new automated tests added (WASM rendering tests require a browser runtime). Manual smoke tests:
- Text edit commit: tile renders immediately with no full-page lag.
- Delete whole text box: block area clears instantly via tile composite.
- Undo text edit: only edited page redraws; adjacent pages keep cached bitmaps.
- Redo: same scoped behaviour.
- Add blank page → full cache clear still fires.

## Known Limitations / Follow-up

- **Tile height**: uses `font_size * 1.15` (ascent + descent without pad). If a block has multi-line text or large ascenders the tile may clip slightly. This matches the existing `text_edit_render_block` sizing.
- **Transparent block backgrounds**: if the PDF has a transparent region in the block area, compositing the tile over the old bitmap may leave ghost artifacts. Opaque white backgrounds (the common case) are handled correctly.
- **PdfDocument clone copies raw bytes**: `borrow_doc()` clones `PdfDocument` which copies `data: Vec<u8>` (the raw PDF). This is still cheaper than `parse()` (no XRef traversal), but if memory pressure matters for very large PDFs an `Arc<PdfDocument>` sharing approach would be the next step.
