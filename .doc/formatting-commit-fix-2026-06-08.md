# Formatting-commit fix + decoration layer persistence — Implementation Report

**Date:** 2026-06-08
**Scope:** Fix formatting-only edits being discarded on click-out; fix underline/strike never persisting after save

## What Was Implemented

### Fix A — Commit gate now detects formatting changes (web layer)

`web-editor/src/components/AnnotationOverlay.vue`:
- Added module-level `let formatTickAtOpen = 0` to track the formatting baseline per editing session.
- `openEditor`: captures `formatTickAtOpen = store.textEditFormatTick` when a block opens.
- `commitBlockEdit`: widened the no-op gate from `if (newText === group.text)` to `if (newText === group.text && !formattingChanged)`, where `formattingChanged = store.textEditFormatTick !== formatTickAtOpen`. Formatting-only changes (italic/bold/underline/color/size/align) now correctly commit.
- `cancelBlockEdit`: resets `formatTickAtOpen = store.textEditFormatTick` to prevent stale baseline carry-over to the next block.

### Fix B — Decoration layer survives the commit flush (Rust)

Root cause: `commit_block_runs` was drawing underline/strike via `begin_edit_page` BEFORE `commit_edit_session` rewrites `/Contents` to a single reference — clobbering the appended decoration layer.

`pdf-editor-rust-core/src/editor/text_commit_runs.rs`:
- Removed the `decorations: &[DecoRect]` parameter from `commit_block_runs`.
- Removed the immediate `begin_edit_page` / `layer.commit` decoration draw from inside `commit_block_runs`.
- Removed the now-unused `use crate::editor::page_editor::begin_edit_page` import.
- Updated module doc comment.
- Updated all test call sites (lines 607, 672, 717 — removed `&[]` arg).
- Removed the `#[ignore]`d test `commit_block_runs_underline_emits_filled_rect` (which tested old now-removed behavior; decoration persistence is tested via the WASM path in `wasm/text_edit.rs`).

`pdf-editor-rust-core/src/wasm/editor.rs`:
- Added `pending_decorations: Vec<crate::editor::DecoRect>` field to `WasmEditor` struct.
- Added `pending_decorations: Vec::new()` to both constructors (`open` and `open_with_password`).

`pdf-editor-rust-core/src/wasm/text_edit.rs`:
- `commit_block_runs_impl`: after calling `commit_block_runs` (now without decorations), stashes `self.pending_decorations = decorations`.
- `flush_and_cache`: after `commit_edit_session` rewrites `/Contents` to a single reference and before `cache_committed_streams`, draws any stashed decorations via `begin_edit_page` + builder, then clears `pending_decorations`. This ensures the decoration stream is appended AFTER the flush → `/Contents` becomes `[stream0, decorations]`.

### Fix C — U/strike button parity in Add-Text mode (web layer)

`web-editor/src/layouts/MainLayout.vue`:
- Removed `:disabled="!editing"` from the U and A (strike) buttons.
- `underlineActive` / `strikeActive` computed now fall back to `store.textStyle` when not editing (matching Bold/Italic pattern).
- `toggleUnderline` / `toggleStrike` now have the `else store.textStyle.underline/strike = !…` fallback for Add-Text mode.

## Design Decisions

- **Per-session `formatTickAtOpen` baseline**: `textEditFormatTick` is already the single source of truth for "a formatting op happened" (bumped on every toggle/color/size/align call). A delta since `openEditor` is the correct scoping — no new Rust API needed. Toggling on then off still commits (harmless/idempotent — rewrites identical style).
- **Decoration draw moved, not folded**: decoration rects are kept in a separate identity-CTM layer (not folded into stream 0, which has a leading `cm` scale). Only the ordering changed. After the flush, `/Contents` is an array — `render_page` concatenates both streams correctly; `cache_committed_streams` skips the fast-path for arrays (acceptable).
- **`commit_block_runs` signature simplified**: removing `editor` and `decorations` params cleanly expresses the function's new contract (model-only mutation, no side effects). The `_editor` stub was dropped entirely since no body code needs it.

## Test Coverage

- 642 lib tests pass (features `writer,render,wasm`) — no regressions.
- All existing `commit_block_runs_*` tests updated to new 5-arg signature and pass.
- Decoration persistence is verified end-to-end by the existing `commit_italic_underline_keeps_original_font_and_commits` integration test in `wasm/text_edit.rs` (which now exercises the `flush_and_cache` decoration path).

## Known Limitations / Follow-up

- Array `/Contents` disables the no-reparse byte cache for that page (one extra reparse after commit with decorations). Follow-up: teach `cache_committed_streams` to cache array contents by concatenating streams.
- `tests/wasm_api.rs` still won't compile under `--features wasm` (pre-existing; missing re-exports in `src/wasm/mod.rs`). Lib tests run via `--lib`.
- Browser E2E still needed: italic+underline should persist on click-out and after save+reopen; console should no longer log "no-op" for formatting-only edits.
