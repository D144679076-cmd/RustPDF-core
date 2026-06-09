# Render Committed Page Without Reparse — Implementation Report

**Date:** 2026-06-04
**Scope:** `WasmEditor::render_page` + `PdfDocument` override layer (Rust); commit-paint render path (web-editor)

## What Was Implemented

### Rust (`pdf-core`)
- **`PdfDocument` override layer** (`src/parser/objects.rs`):
  - New field `overrides: RefCell<HashMap<u32, PdfObject>>` (initialised empty in both
    `parse` and `parse_with_password` constructors).
  - `set_overrides(map)` / `clear_overrides()` — install / drop a temporary object overlay.
    Both purge the affected ids from `decoded_stream_cache` (a stream id may be overridden,
    so a stale decoded body must not survive).
  - `get_object(id)` now returns `overrides[id].clone()` first when present, so every
    consumer that routes through it (`resolve`, stream decoding, the renderer) transparently
    sees the overlaid objects.
  - Unit test `overrides_shadow_then_restore_objects` (uses `tests/fixtures/minimal.pdf`):
    a high id resolves to `Null` pristinely, an installed override is returned, and
    `clear_overrides` restores the pristine `Null`.
- **`RenderResult::new`** (`src/wasm/document.rs`): `pub(crate)` constructor (the `data`
  field is private) plus a `#[cfg(test)]` `bytes()` accessor, behind `#[cfg(feature = "render")]`.
- **`WasmEditor::render_page(page_index, scale) -> RenderResult`** (`src/wasm/editor.rs`,
  `#[cfg(feature = "render")]`): collects the writer-pool objects into an override map,
  installs it on the pristine `editor.doc`, renders via `render_page_rgba`, then clears the
  overrides (always, even on render error) and returns the RGBA `RenderResult`.

### Frontend (`web-editor`)
- **`renderQueue.putPage(pageIndex, scale, bitmap)`** (`src/composables/useRenderQueue.ts`):
  inserts an externally-produced bitmap under the *current* epoch cache key (replacing/closing
  any prior entry) so a later `getCached` returns it.
- **`editorRenderPage(pageIndex, scale)`** (`src/stores/usePdfStore.ts`): wraps
  `editor.render_page` → `ImageData` → `createImageBitmap` (mirrors the queue's `renderOne`).
- **`commitRenderPage(pageIndex)`** (store): renders the committed page from the editor,
  `putPage`s it, pulses a new `commitPaintTick`/`commitPaintPage` signal, and schedules a
  **debounced** (`400 ms`) `refreshPage` to reconcile `store.doc` off the critical path.
- **Commit-paint watch** (`src/composables/useLazyRender.ts`): a watch on
  `store.commitPaintTick`, guarded to `store.commitPaintPage === pageIndex`, repaints that one
  page from cache and calls `notifyPageRendered` (which the overlay's held-preview watch uses
  to release the preview).
- **Rewire** (store): `textEditCommit` (success) and `replaceText` now call
  `commitRenderPage(pageIndex)` instead of `refreshPage(pageIndex)`.

## Design Decisions
- **Override overlay instead of a parser-level patch API.** The renderer already resolves
  every object lazily through `get_object`; a thin `RefCell` overlay makes the pristine doc
  render the edited page with zero changes to the render code and no byte reparse. A surgical
  commit only adds a handful of writer-pool objects (page dict + new content stream); unchanged
  fonts/images still resolve from the pristine doc and hit the existing thread-local caches.
- **`clear_overrides` purges `decoded_stream_cache`.** Stream object ids can be overridden, so
  a previously-cached decoded body for that id would otherwise leak across the overlay boundary.
- **Separate `commitPaintTick` signal (not `renderTick`).** `useLazyRender`'s main watch bumps
  `renderTick` itself; watching it there would feed back into an infinite loop. A dedicated,
  page-guarded signal repaints exactly the committed page once.
- **Debounced `refreshPage`, not on the critical path.** The page is already correct from the
  editor render; the reparse+swap that keeps `store.doc` authoritative runs `400 ms` later and
  coalesces across a burst of commits. `refreshPage` (single-page scope) keeps every other
  page's cached bitmap; the re-render of the committed page produces pixel-identical output, so
  the eventual swap doesn't flicker.

## Test Coverage
- `parser::objects::tests::overrides_shadow_then_restore_objects` — happy path (override
  shadows an id) + restore path (clear returns the pristine resolution). All 316 lib tests pass
  under `--features render --no-default-features`.
- Verified: `cargo fmt --check` clean, `cargo clippy --features wasm-render --no-default-features`
  warning-free, `make wasm` builds and exports `wasmeditor_render_page`, and `vue-tsc --noEmit`
  reports no type errors in the changed files (only a pre-existing `baseUrl` tsconfig deprecation).

## Known Limitations / Follow-up
- A planned Rust pixel-equality integration test (`render_page` overlay vs.
  `WasmDocument::parse(editor.save())` then `render_page`) was not added — the `WasmEditor`
  native test path (`tests/wasm_api.rs`) currently fails to compile because `src/wasm/mod.rs`
  no longer re-exports `WasmDocument`/`WasmEditor`/`WasmPdfWriter` (a gap in the in-progress
  wasm-module refactor, independent of this change). Equality is currently covered by manual
  app verification; restore the re-exports to re-enable that integration test.
- The overlay clones each overridden `PdfObject` on `set_overrides` and per `get_object` hit.
  For surgical commits (a few small objects) this is negligible; large overrides would warrant
  `Rc` sharing.
- The held-preview + 8 s fallback in `AnnotationOverlay.vue` is now a thin safety net. Because
  the page updates from the editor render near-instantly, it can be shortened later; left as-is
  to avoid changing tuned behaviour in this change.
