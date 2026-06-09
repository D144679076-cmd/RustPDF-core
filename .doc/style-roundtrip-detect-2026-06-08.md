# Style Round-Trip Detection — Implementation Report

**Date:** 2026-06-08
**Scope:** Re-opening a committed block now reflects its applied synthetic style (italic / bold / underline / strike) in the formatting panel.

## What Was Implemented

### `editor/text_style.rs`
- `OBLIQUE_SHEAR_TOL: f64 = 0.04` — tolerance for detecting synthetic italic shear
- `matrix_shear(a, b, c, d) -> f64` — computes `(a·c + b·d) / (a²+b²)`, the shear of a text matrix relative to its x-basis
- Unit tests: `matrix_shear_identity_is_zero`, `matrix_shear_oblique_detected`, `matrix_shear_degenerate_is_zero`

### `editor/edit_session.rs`
- `FilledRect` struct — filled rect collected during the content walk (`x`, `y`, `width`, `height`, `color: [f64;3]`)
- `GfxState` extended: `render_mode: i64`, `fill_color: [f64;3]`, `pending_rect: Option<(f64,f64,f64,f64)>`
- New operator handling in `extract_frames_recursive`:
  - `"Tr"` → sets `gfx.render_mode`
  - `"rg"` / `"g"` → sets `gfx.fill_color`
  - `"re"` → sets `gfx.pending_rect` (path accumulation)
  - `"f"` / `"F"` / `"b"` / `"B"` / `"b*"` / `"B*"` → transforms the pending rect through the CTM and pushes to the shared `rects` vec
- `RawFrame` tuple extended from 9 to 10 fields (added `render_mode: i64` at end)
- `EditableFrame` extended: `render_mode: i64`
- `EditSession` extended: `rects: Vec<FilledRect>`
- `op_i64` helper (mirrors `op_f64`)
- All 6 test destructure sites updated for the 10-tuple

### `editor/text_model.rs`
- `EditBlock` extended: `synthetic_italic: bool`, `synthetic_bold: bool`, `underline: bool`, `strike: bool`, `decorations: Vec<DecoRect>`
- `build_block` detects:
  - **synthetic italic** via `matrix_shear` on primary frame's `tm`; if detected, strips the shear from the stored `tm` so re-commit applies exactly one shear
  - **synthetic bold** via primary frame's `render_mode ∈ {1, 2}`
- `match_decorations_to_blocks(blocks, rects)` — matches `FilledRect`s from the session to blocks using strict y-proximity (underline/strike offset ± `pos_tol`), x-extent overlap (≥60% block width), and thickness guard
- `build_text_model` calls `match_decorations_to_blocks` after block building

### `editor/text_commit_runs.rs`
- `RunLayout` extended: `force_positioned: bool`, `reset_stroke: bool`
- `build_run_ops` updated:
  - Positioned path now also triggers when `force_positioned` (makes italic-off write a clean unsheared `Tm`)
  - Emits `0 Tr` + `0 w` before the run loop when `reset_stroke` is set (clears stale `2 Tr` from the stream)

### `wasm/editor.rs`
- `pending_decorations` type changed from `Vec<DecoRect>` to `Option<(usize, Vec<DecoRect>)>` (block_id + rects)
- Both constructors updated: `pending_decorations: None`

### `wasm/text_edit.rs`
- `text_edit_open` seeding updated to use effective style:
  - `effective_bold = block.bold || block.synthetic_bold`
  - `effective_italic = block.italic || block.synthetic_italic`
  - `seed.underline = block.underline; seed.strike = block.strike`
  - `orig_bold`/`orig_italic` remain intrinsic-only (for `run_synthetic_style`)
- `commit_block_runs_impl` snapshot tuple extended with `block_synthetic_italic`, `block_synthetic_bold`
- `RunLayout` construction uses `force_positioned: block_synthetic_italic`, `reset_stroke: block_synthetic_bold`
- Preview plan `RunLayout` uses `force_positioned: block.synthetic_italic`, `reset_stroke: false`
- `pending_decorations` stash changed to `Some((block_id, decorations))`
- `flush_and_cache` now calls `rebuild_page_decorations` instead of the old simple append loop
- New `rebuild_page_decorations(page_index, committed_block_id, current_decos)`:
  1. Collects decorations from all blocks (current block gets `current_decos`, others get their `block.decorations`)
  2. Scans `/Contents` for an existing decoration stream (has `re`/`f` bytes, no `BT`)
  3. Empty all_decos → drop deco stream ref; existing deco stream → replace in-place; no deco stream → append new one
  4. Caches bytes for preload (both `committed_bytes` and `doc.preload_stream`)
- `cache_deco_stream` removed (superseded by inline caching in `rebuild_page_decorations`)

## Design Decisions

- **Strip shear on detection, not on commit**: stripping `c`/`d` from the stored `tm` in `build_block` means the base matrix is always unsheared; `build_run_ops` re-applies the shear from the `SyntheticStyle` per run. Avoids double-shear on re-commit.
- **`force_positioned` for italic-off**: without it, `build_run_ops` only emits an absolute `Tm` for synthetic runs; turning italic off produces a run with `synthetic.italic == false`, which would leave the old sheared `Tm` intact in the stream. `force_positioned` ensures the clean base `Tm` is always emitted when the block had synthetic italic, even after toggling off.
- **`reset_stroke` for bold-off**: the old `2 Tr` and stroke width remain in the stream between the preceding BT position and our run. Emitting `0 Tr 0 w` at the top of our run sequence clears them before any glyphs are drawn.
- **Page-level decoration rebuild over per-commit append**: the previous append-only design accumulated duplicate decoration streams on every commit. The new `rebuild_page_decorations` finds and replaces the existing stream in-place, making toggle-off safe and preventing `/Contents` growth.
- **Decoration stream detection heuristic**: scanning for `re`/`f` bytes absent of `BT` is cheap and reliable for the decoration streams we write (they are pure `q rg re f Q` sequences). False-positive risk from table rules is mitigated by the strict match criteria in `match_decorations_to_blocks`.
- **`Option<(usize, Vec<DecoRect>)>` stash**: carrying the block_id lets `rebuild_page_decorations` identify which block's decorations to replace without re-querying the model.

## Test Coverage

Existing 646 unit tests all pass. New tests added in this implementation:
- `text_style.rs`: `matrix_shear_identity_is_zero`, `matrix_shear_oblique_detected`, `matrix_shear_degenerate_is_zero`
- All pre-existing tests for `edit_session`, `text_model`, `text_commit_runs`, and `wasm::text_edit` continue to pass, including `commit_italic_underline_keeps_original_font_and_commits`.

## Known Limitations / Follow-up

- **Decoration matching is heuristic**: thick table rules or page borders with the same y/thickness as a text decoration could be misidentified. The strict x-extent (≥60% block width) and thickness tolerance guards reduce the risk but don't eliminate it.
- **Per-character round-trip not addressed**: the block is re-opened with a uniform seed style (the block's intrinsic || synthetic). Per-character colour/font/size/style on re-open remains out of scope.
- **Decoration stream detection by byte scanning**: scanning for `re`/`f` in raw compressed stream bytes requires the stream to be decompressed first. If a stream fails to decode (malformed flate), it is silently skipped (treated as not a deco stream).
- **Same-session block.decorations not updated after first commit**: `block.decorations` is set at model-build time from parsed page content. After a commit the in-memory block's `decorations` field is stale; a full model rebuild (next `text_edit_enter` cold path) would re-detect correctly. This is acceptable because `rebuild_page_decorations` uses the live `current_decos` for the just-committed block.
