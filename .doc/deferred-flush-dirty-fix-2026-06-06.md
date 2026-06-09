# Deferred-Flush Dirty-Flag Fix + Debug Logging — Implementation Report

**Date:** 2026-06-06
**Scope:** `src/editor/text_commit.rs`, `src/editor/text_commit_runs.rs`, `src/wasm/text_edit.rs` — fix lost text edits/deletes under deferred flush + add tracing logs

## Symptom

After the Phase-1 deferred-flush change, editing or deleting a text block had no
persistent effect: the change rendered briefly during the session but on save (or
the next re-enter) the original text returned and re-rendered.

## Root Cause

The two-phase design defers the writer-pool flush to `text_edit_exit`, which only
flushes when the session is dirty:

```rust
if let Some(model) = &self.text_edit_model {
    if model.session.dirty {            // ← gate
        commit_edit_session(...);
    }
}
```

But `commit_block` / `commit_block_with_font` / `commit_block_runs` patch
`model.session.streams[i].ops` **directly** and never set
`model.session.dirty = true`. Only the legacy `patch_frame`
(`edit_session.rs:326`) set it. So after a Tier-1/2 commit:

1. `dirty` stayed `false` → `text_edit_exit` skipped `commit_edit_session` → the
   patched stream never reached the writer pool → not saved.
2. After exit cleared the in-memory model, the writer pool was still empty, so the
   next `text_edit_enter` took the `writer.is_empty()` branch → read the pristine
   `editor.doc` → original text returned and rendered.

The per-stream `OpStream::changed()` check inside `commit_edit_session` was
correct; the bug was purely the missing `dirty` signal that gates whether the
flush runs at all.

## What Was Implemented

### Fix (the actual bug)

- **`commit_block`** — set `model.session.dirty = true` after a successful patch
  (after the `wrote_primary` guard).
- **`commit_block_with_font`** — set `model.session.dirty = true` after inserting
  the `Tf` op.
- **`commit_block_runs`** — set `model.session.dirty = true` after the run rewrite
  + decoration layer.

This mirrors the existing `patch_frame` contract: a real patch marks the session
dirty so the deferred flush fires.

### Debug logging (promoted to `log::warn!` so they surface at the FE's
`Level::Debug` console logger)

- **`text_edit_enter`** — logs FAST-PATH (with each block id:text) vs
  FULL-REBUILD (with page/generation), so stale-model issues are visible.
- **`text_edit_open`** — logs found/NOT-FOUND + the block text.
- **`text_edit_commit`** — logs session match/mismatch, `is_formatted`, and the
  Tier-1 / Tier-3 / multi-run outcome with the committed text.
- **`cache_committed_streams`** — logs the resolved stream id + byte count
  preloaded (or the failure branch).
- **`text_edit_exit`** — logs session dirty state, whether the flush ran, and the
  writer generation after flushing.

Frontend (`AnnotationOverlay.vue`) `console.warn` traces:
- `reenterForPage` — page → returned blocks (id:text).
- `openEngineForBlock` — rustId → ok.
- `deleteSelectedBlock` — rustId, engineReady, commit step.

### Incidental

- Added the re-exported `Measurer` trait to the top-level import in
  `text_edit.rs` (an externally-added `styled_offsets`/`run_metrics` pair called
  `m.advance(ch)` without the trait in scope, breaking the build).

## Test Coverage

- `commit_block_sets_session_dirty` (new) — fresh model not dirty; after
  `commit_block` the session is dirty. Directly guards the regression.
- `commit_block_failed_patch_leaves_session_clean` (new) — a failed commit
  (unknown id) must not set dirty.
- All existing tests still pass (290 default; 583 lib with all features).

## Known Limitations / Follow-up

- The `width` field in `text_edit_blocks` is still not updated on commit (only
  `text`); the dashed-border width refreshes on the next full rebuild. The FE
  separately updates `rustBlocks[i].width` from `liveWidthPts`, so this is cosmetic.
- The `log::warn!` traces are intentionally loud for this debug pass; downgrade to
  `log::debug!` once the flow is confirmed in the field.
