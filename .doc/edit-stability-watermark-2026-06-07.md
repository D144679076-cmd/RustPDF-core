# Edit Stability + Watermark Idempotency — Implementation Report

**Date:** 2026-06-07
**Scope:** `src/wasm/text_edit.rs`, `src/wasm/editor.rs` — fix intermittent delete/edit revert + file-size explosion

## Symptoms

1. **Intermittent revert.** After the flush-on-commit fix, deleting/editing a text
   box sometimes persisted and sometimes reverted ("delete some flush and revert";
   "edited text returns to origin on click-out").
2. **File-size explosion.** A few edits grew the saved file from 876 KB to 1.74 MB.

## Root Causes (from user console logs)

### Intermittent revert — per-commit block-list churn
- Block `id` is the positional `enumerate()` index in `build_text_model`
  (`text_model.rs:122`).
- Every text commit bumped the writer generation (the flush's `add_object` **and**
  the per-save trial watermark), so the next `text_edit_enter` took the
  FULL-REBUILD path.
- The rebuild **renumbered** blocks (a deleted/blanked frame shifts every later id;
  the log showed "and Its Role" id 8→7).
- The host kept `selectedRustId` from before the renumber → `selectedBlock` went
  `null` → `commitBlockEdit` hit `early-return: no selected/editing block` → the
  edit/delete was silently dropped → revert. Intermittent because it depended on
  whether the acted-on id survived the renumber.

### File bloat — watermark re-applied every save
`apply_trial_watermark` ran on **every** `save()`, appending a watermark content
stream to **every page's `/Contents`** with no dedup. The frontend calls
`editor.save()` on every commit (undo history), so watermark streams multiplied
(pages × saves).

## What Was Implemented

### A. Stable session across Tier-1 commits (`src/wasm/text_edit.rs`)
- New `keep_model_current(page_index)`: refreshes `edit_model_doc`
  (`rebuild_edit_model_doc`) and advances `text_edit_model_generation` to the
  post-flush `writer.generation()`. It does **not** rebuild `text_edit_model`.
- The Tier-1 success path of `text_edit_commit` now calls `keep_model_current`
  after `flush_and_cache` + the `text_edit_blocks[id].text` update.
- Effect: after a plain delete/edit, the next `text_edit_enter` takes the
  FAST-PATH and returns the kept-updated `text_edit_blocks` with **stable ids**
  (deleted blocks remain in the list with `text = ""`; the host filters them for
  display but their ids never shift). The host's `selectedRustId` stays valid →
  commits are no longer silently dropped.
- Tier-1 `commit_block` patches ops **in place** (op count unchanged), so the
  retained model's other-block `op_indices` stay valid across multiple commits in
  one session.
- **Multi-run / Tier-3** still let the next enter rebuild (they change op
  structure, which would invalidate retained `op_indices`). Documented limitation.

### B. Idempotent trial watermark (`src/wasm/editor.rs`)
- Added `watermarked: bool` to `WasmEditor` (initialised `false` in both
  constructors).
- `save()` applies the watermark only when `!self.watermarked`, then sets the
  flag. The watermark persists via the accumulated `original_bytes`/pool (and is
  baked into the collapsed main stream on the next flush), so later saves don't
  re-add it. This removes both the pages×saves stream multiplication **and** the
  per-save generation bump (which also contributed to the churn in A).

## Design Decisions

- **Keep the model rather than make ids stable in `build_text_model`.** Reusing the
  live session avoids both the renumber and a full CMap-inverting rebuild per
  commit; it's valid precisely because Tier-1 edits are op-count-preserving.
- **Watermark once, not per save.** Simpler and correct: the mark is already in the
  serialized bytes after the first save; re-applying only bloats and destabilises.

## Test Coverage

- `cargo test --features render,writer,crypto`: **648 passed, 6 ignored**.
- No new unit tests (the behaviour is in the WASM binding layer, which needs the
  wasm-bindgen runtime); verified via the app + console logs.

## Known Limitations / Follow-up

- **Multi-run / Tier-3 commits** still churn the block list (rebuild on next enter)
  and can desync host selection — same class as A but for formatting/font-embed
  edits. Fix later by making `build_text_model` assign stable ids or extending the
  keep-model path to op-structure-changing commits.
- **Orphan content streams.** Each Tier-1 flush still `add_object`s a fresh content
  stream; previous ones orphan in the pool. Bounded per session but grows the
  incremental file. Optional follow-up: reuse the previously-flushed stream id.
- **Diagnostic `log::warn!` traces** remain loud; downgrade to `debug!` once
  confirmed in the field.
- Decoration (underline/strike) drop and encrypted-write remain separate
  tracked items.
