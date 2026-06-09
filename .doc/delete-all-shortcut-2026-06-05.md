# Delete-All Shortcut — Implementation Report

**Date:** 2026-06-05
**Scope:** editor/text_edit_engine + wasm/text_edit + web-editor

## What Was Implemented

- `TextEditEngine::delete_all()` (`src/editor/text_edit_engine.rs`) — atomically clears `chars`, `styles`, resets caret to 0, clears selection, refreshes typing style.
- `WasmEditor::text_edit_delete_all()` (`src/wasm/text_edit.rs`) — thin WASM binding delegating to `engine.delete_all()`.
- `textEditDeleteAll()` store function (`web-editor/src/stores/usePdfStore.ts`) — calls `editor.text_edit_delete_all()`, exported in return object.
- **Ctrl+Shift+Delete** keyboard shortcut (`web-editor/src/components/AnnotationOverlay.vue`, `onEditorKeydown`) — calls `store.textEditDeleteAll()` then `afterEngineEdit(block)` to trigger glyph re-raster.

## Design Decisions

- **Atomic method instead of `select_all` + `delete_back`:** A dedicated `delete_all` avoids two round-trip mutations and eliminates the brief "all selected" intermediate state observable from JS between the two WASM calls.
- **Ctrl+Shift+Delete shortcut:** Unambiguous — Ctrl+Backspace is "delete word" on most OSes, Ctrl+Delete is "delete word forward". Ctrl+Shift+Delete is "clear field" convention in several editors.
- **`afterEngineEdit` not `updateSelectionOnly`:** Text content changes require a full glyph re-raster, not just a caret/selection recompose.

## Test Coverage

- `delete_all_clears_buffer` — happy path: engine with text → `delete_all()` → empty text, caret 0, no selection.
- `delete_all_on_empty_is_noop` — error path: empty buffer → `delete_all()` must not panic, state remains valid.

## Known Limitations / Follow-up

- No undo support (same as all other edit operations at this stage — undo is a deferred Phase 4 item).
