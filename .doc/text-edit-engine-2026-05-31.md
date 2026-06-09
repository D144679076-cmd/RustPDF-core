# Text-Edit Engine (Word-style) — Phase 1 Implementation Report

**Date:** 2026-05-31
**Scope:** editor::text_shape, editor::text_edit_engine, editor::text_model,
wasm::text_edit, web-editor edit-text canvas wiring

## What Was Implemented

### Rust core (`pdf-editor-rust-core/src`)
- **`editor/text_shape.rs`** — `Measurer` trait; pure layout helpers
  `caret_offsets`, `text_width`, `hit_test`, `wrap_lines` (greedy word-wrap,
  space-break + hard-break); `PdfFontMetrics::from_font_info` (char→advance table
  for codes 0..=255 from the `(ToUnicode CMap, FontWidths)` pair that
  `resolve_font_info` returns, 1/1000-em × size) + `PdfFontMetrics::fallback`;
  `font_metrics_for(doc,page,key,size)` (returns `None` for composite/CID so the
  caller uses the fallback — proper CID metrics are Phase 3).
- **`editor/text_edit_engine.rs`** — `TextEditEngine` (single line): char buffer,
  caret, selection anchor; `insert`/`delete_back`/`delete_forward`/
  `move_caret(Dir,extend)`/`home`/`end`/`click`/`select_all`, `caret_x`,
  `selection_x`. `Dir` enum. Unicode-scalar indices.
- **`editor/text_model.rs`** — `EditBlock`/`TextModel`; `build_text_model`
  (extends `build_edit_session`, measures each frame, groups into blocks);
  pure `group_blocks` clustering (same stream+font, baseline ±0.4·size, gap
  ≤2·size) mirroring the web overlay rule. Records `op_range` per block for the
  Phase-2 surgical writer.
- **`wasm/text_edit.rs`** — `ActiveTextEdit` + `#[wasm_bindgen] impl WasmEditor`:
  `text_edit_enter` (→ blocks JSON), `text_edit_open`, `_insert`, `_backspace`,
  `_delete_forward`, `_move`, `_home`, `_end`, `_click`, `_select_all`, `_state`
  (JSON incl. `caret_x`, `sel_start_x`/`sel_end_x`), `_text`, `_frame_ids`,
  `_cancel`. New `pub(crate)` fields on `WasmEditor`.
- Registered in `editor/mod.rs` + `wasm/mod.rs`.

### Web (`web-editor/src`)
- **`types/pdf.ts`** — `EditBlockData`, `TextEditState` interfaces.
- **`stores/usePdfStore.ts`** — `enterTextEdit`, `openTextBlock`, and thin
  `textEdit*` wrappers (insert/backspace/deleteForward/move/home/end/click/
  selectAll/cancel/getState) over the WASM API.
- **`components/AnnotationOverlay.vue`** — edit-text path now routes all caret /
  selection / insert / delete / arrows / Home/End / Ctrl+A / IME / click through
  the Rust engine (`afterEngineEdit` re-syncs + redraws). Caret drawn at the
  engine's `caret_x` (mid-text, not block-end); selection highlight drawn from
  `sel_*_x`. Falls back to whole-string edit when no engine block matches.

## Design Decisions
- **Rust owns all layout/measurement** (single source of truth) so the on-screen
  edit bitmap is identical to the eventual PDF output — critical for CJK later.
- **Engine is font-agnostic** via `Measurer` → fully unit-testable, no fixtures.
- **Selection x-bounds come from Rust** (`sel_*_x` in state JSON) so JS never
  needs font metrics.
- **Canvas, not SVG**, for the edit layer: WASM RGBA blits straight via
  `render_edit_block`; SVG kept only for annotation chrome.
- **Reuse, not rewrite:** model wraps `build_edit_session`; commit still uses the
  existing `commitTextEdit` operand path (Phase 2 upgrades to surgical op
  replacement using `EditBlock::op_range`).

## Test Coverage
- 33 new/covered unit tests pass (`cargo test --features writer editor::text_`):
  caret offsets/width/hit-test/word-wrap; engine insert/backspace/delete-forward/
  selection-replace/move-collapse/shift-extend/caret_x/click/selection_x/unicode;
  block grouping (join, baseline/font/gap split, span+concat, empty).
- Full suite: 0 failures. `cargo fmt --check` clean. `cargo clippy
  --features writer` and `--target wasm32-unknown-unknown --features wasm-render`
  both clean with `-D warnings`. wasm + wasm-render targets build.
- Web: ESLint clean on the 3 changed files. (Project-wide `vue-tsc` not run —
  times out on the full quasar project; runtime check pending in the app.)

## Known Limitations / Follow-up
- **WASM pkg rebuild required:** the `sel_start_x`/`sel_end_x` state fields were
  added after the last `wasm-pack` build, so selection-highlight needs `make` in
  `pdf-editor-rust-core` to regenerate `web-editor/src/pkg`. All other edit ops
  are already in the built pkg. (Caret/insert/delete/arrows work pre-rebuild;
  only the highlight rectangle needs the new field.)
- **Single-line only.** Multi-line paragraph reflow / word-wrap in the live
  editor is Phase 4 (`wrap_lines` already exists for it).
- **Write-back still operand-replace** via `commitTextEdit`; surgical op
  replacement using `op_range` is Phase 2.
- **CID/Type0/CJK** measurement falls back to a proportional estimate; real CID
  metrics + font subsetting are Phase 3.
- Browser end-to-end verification (caret placement, typing, selection, save→
  reopen) still to be done in the running app.
