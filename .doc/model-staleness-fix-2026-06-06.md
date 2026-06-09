# Model Staleness Fix — Implementation Report

**Date:** 2026-06-06
**Scope:** `src/wasm/text_edit.rs` — stale fast-path in `text_edit_enter` + `text_edit_blocks` sync

## What Was Implemented

### Changed functions

- **`text_edit_enter` fast-path** — changed `blocks_to_json(&model.blocks)` to
  `blocks_to_json(&self.text_edit_blocks)`. `text_edit_blocks` is now the live,
  up-to-date list; `model.blocks` is only the snapshot from session start.

- **`text_edit_commit` — three success paths** — after each successful commit
  (Tier-1 surgical, multi-run, Tier-3 embed), the committed text is written back
  to the corresponding entry in `self.text_edit_blocks`:
  ```rust
  if let Some(b) = self.text_edit_blocks.iter_mut().find(|b| b.id == block_id) {
      b.text = text.clone();
  }
  ```
  For an empty-text delete (`text == ""`), the entry gets `text: ""`. The FE
  overlay filters `rb.text.trim().length > 0`, so the block disappears and stays
  gone on subsequent re-enters.

### Pre-existing clippy fix (unrelated)

`parse_object_at_offset` in `src/parser/objects.rs` was missing a
`#[cfg(feature = "crypto")]` gate. Added it to suppress the `dead_code` warning.

## Design Decisions

**Why `text_edit_blocks` instead of `model.blocks`:**  
`model.blocks` is populated by `build_text_model` at session start and is never
mutated by `commit_block` / `commit_block_runs` (those only patch
`model.session.streams[i].ops`). With deferred flush (Phase 1) the writer
generation never changes during a Tier-1/2 session, so `text_edit_enter` always
fast-paths and would return the stale `model.blocks` indefinitely.
`text_edit_blocks` was already a mutable `Vec` set once per full enter; updating
it after each commit costs O(#blocks) per commit (typically <20 items) and keeps
the live list consistent without a full model rebuild.

**`text.clone()` inside the commit paths:**  
`text` is already owned and extracted at the top of `text_edit_commit`
(`a.engine.text()`). The clone is unavoidable since `self.active_text_edit`
borrows `self` but `self.text_edit_blocks` needs a mutable borrow.
Split-field borrowing via the already-owned `text` variable avoids the conflict.

## Test Coverage

- All 290 existing tests pass — commit_block, commit_block_runs, edit_session
  flush paths all exercised.
- No new unit tests for the WASM binding layer (WasmEditor requires wasm-bindgen
  target infrastructure; the fix is a 3-line pattern repeated 3×).

## Known Limitations / Follow-up

- The fast-path still returns `text_edit_blocks` even after Tier-3 commits. Tier-3
  writes to the writer pool (generation bumps), so the *next* `text_edit_enter`
  after a Tier-3 commit will do a full rebuild anyway and reset `text_edit_blocks`
  to the fresh model. The update done here is redundant but harmless.
- The `width` field in `text_edit_blocks` is not updated after commit (only `text`
  is). The FE derives `svgW` from `rb.width` for the dashed border. If the new
  text is significantly shorter or longer, the border will not resize until the
  next full model rebuild (on `text_edit_exit` + re-enter). This is acceptable —
  the FE also tracks `newWidthPts` from `liveWidthPts` in the commit callback and
  updates `rustBlocks[i].width` there.
