# Editor Deferred Flush — Implementation Report

**Date:** 2026-06-06  
**Scope:** Text editor buffered write-back layer (Rust core + Vue frontend)

## What Was Implemented

### Rust core

- **`src/editor/text_commit.rs`**  
  - `commit_block`: removed `commit_edit_session` call at end. Now patches `model.session.streams` ops only. `editor` and `page_index` params renamed with `_` prefix (unused in body; kept for API stability).  
  - `commit_block_with_font`: same — removed `commit_edit_session`. `register_page_font` (Tier-3 pool write) retained; only the content-stream flush is deferred.  
  - Module doc updated to describe the two-phase design.  
  - Removed `use crate::editor::edit_session::commit_edit_session` import.

- **`src/editor/text_commit_runs.rs`**  
  - `commit_block_runs`: removed `commit_edit_session` call after `stream.ops = new_ops`. Decoration-layer writes (`begin_edit_page` / `layer.commit`) remain immediate.  
  - Module doc + function doc updated.  
  - Removed `use crate::editor::edit_session::commit_edit_session` import.

- **`src/editor/edit_session.rs`**  
  - `commit_edit_session` doc comment rewritten: now described as the explicit **flush** step, not an implicit part of per-block commit.

- **`src/wasm/text_edit.rs`**  
  - `cache_committed_streams`: changed `self.editor.writer.get_object(page_id)` (pool-only) to `self.editor.get_object(page_id)` (CoW). Now correctly resolves the stream ID before flush (original doc, original stream ID) and after flush (pool, new stream ID).  
  - Added `text_edit_exit() -> String`: flushes dirty streams via `commit_edit_session`, refreshes `committed_bytes` to new stream IDs, clears `text_edit_model` / `text_edit_blocks` / `edit_model_doc`. Returns `"{}"` or `{"error":"..."}`.

### Frontend

- **`web-editor/src/stores/usePdfStore.ts`**  
  - Added `textEditExit()` action wrapping `editor.text_edit_exit()`.  
  - `setTool()`: calls `textEditExit()` when switching away from `'edit-text'`.  
  - `setPage()`: calls `textEditExit()` when navigating to a different page in `'edit-text'` mode.  
  - `onPageScrolledIntoView()`: same — flushes before page change.  
  - Exported `textEditExit` from the store return.

- **`web-editor/src/components/AnnotationOverlay.vue`**  
  - `onUnmounted`: added `store.textEditExit()` call to flush on component teardown.

## Design Decisions

**Two-phase split (patch now, flush on exit):** `commit_block`/`commit_block_runs` patch `OpStream.ops` in memory. `commit_edit_session` is called once via `text_edit_exit`. This keeps `writer.generation` constant across Tier-1/2 commits, making the `text_edit_enter` fast-path (generation check) hit every time — eliminating O(file_size) `PdfDocument::clone` per block commit.

**CoW fix for `cache_committed_streams`:** The function previously used `writer.get_object` (pool-only) to find the Contents stream ID. Before flush, the page dict isn't in the pool, so it silently fell through and preloaded nothing. Changed to `editor.get_object` (CoW: pool first, then original doc) so preview preloading works in both states.

**Decoration layers stay immediate:** `begin_edit_page` in `commit_block_runs` writes a new append-layer stream to the pool immediately (not deferred). These are structurally separate from the main content stream and always need pool writes (they add to `/Contents` array); deferring them would require a more complex buffering mechanism. This is acceptable since underline/strike decorations are less common.

**Tier-3 pool writes kept:** `register_page_font` (font resource dict update) still writes to the pool immediately. Font object IDs must exist in the pool before `commit_edit_session` runs so the `Tf` op references a valid object. Generation bumps for Tier-3 are unavoidable but rare.

**`committed_bytes` not cleared on exit:** kept so `rebuild_edit_model_doc` (called on next `text_edit_enter` after exit) can preload stream bytes into the cloned doc, skipping flate-decompress.

## Test Coverage

### Updated tests (now explicitly flush before save):
- `commit_block_replaces_show_text_simple_font` — patch → `commit_edit_session` → `save_append` → assert `(World)` present
- `commit_block_unknown_id_errors` — no change needed (no save step)
- `commit_block_with_font_embeds_and_retargets` — patch → `commit_edit_session` → `save_append` → assert Type0/Identity-H/Ed0
- `commit_block_runs_two_run_block_saves_expected_sequence` — runs commit → flush → assert ordering
- `commit_block_runs_underline_emits_filled_rect` — runs commit → flush → assert `re` / `f` ops (decoration layer flushed immediately, main stream via explicit flush)
- `commit_block_runs_embedded_bold_run_persists` — embed + runs → flush → assert Type0 + bold key

All 290 tests pass.

## Known Limitations / Follow-up

- **Individual block-level undo lost for Tier-1/2:** `checkpoint()` in `text_edit_commit` snapshots the pool before `commit_block`, but the pool isn't mutated until `text_edit_exit`. Restoring a checkpoint therefore restores identical state. Undo within a session (before exit) requires an in-memory undo stack over the model's `OpStream.ops` — follow-up work.
- **Tier-3 decoration layers bump generation:** if `commit_block_runs` emits decorations, the generation increments immediately (decoration pool write). Next `text_edit_enter` will rebuild `edit_model_doc` once. Acceptable for the rare case.
- **`wasm-pack build` needed:** run `wasm-pack build --features wasm` and copy `pkg/` to `web-editor/src/pkg/` to expose `text_edit_exit` in TypeScript bindings.
