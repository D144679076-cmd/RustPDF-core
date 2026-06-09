# Edit-Text Drag-to-Select — Implementation Report

**Date:** 2026-06-04
**Scope:** Make mouse-drag text selection (highlight a character range) work inside an open edit-text block, plus keep dragging responsive.

## What Was Implemented

### Frontend — `web-editor/src/components/AnnotationOverlay.vue` (the core fix)
- **Stable selection identity by Rust block id.** Replaced the `selectedBlock` *ref* with `selectedRustId: Ref<number|null>` + a derived `selectedBlock = computed(() => textBlocks.value.find(b => b.rustId === selectedRustId.value) ?? null)`. Converted the four assignment sites (`onBlockClick` ×2, `onBlockDblClick`, `cancelBlockEdit`) to set `selectedRustId`.
- **`pointer-events="all"`** on the text-block `<rect>` so its interior is hit-testable even when `fill="none"`.
- **Responsiveness:** new `updateSelectionOnly(block)` = `syncEngineState()` + `composeBlock(block)` only (no `scheduleGlyphRender`). `onDragSelectMove` now stashes the latest `clientX` and coalesces via `requestAnimationFrame` (`processDragFrame`); `onBlockMouseDown`, `placeCaretFromEvent`, and the caret/selection branches of `onEditorKeydown` (Arrows/Home/End/Ctrl+A) use `updateSelectionOnly`. Deletions/typing still use `afterEngineEdit` (text changed → re-raster). `blockLocalXPts` now takes `clientX`. rAF cancelled on `onDragSelectUp` and `onUnmounted`.

### Rust — hardening
- `src/wasm/text_edit.rs::text_edit_state`: clamp the page-space scale to `1.0` when `scale_x.abs() <= 1e-6`, so a degenerate CTM can't collapse caret/selection x to 0 (invisible highlight).
- `src/editor/text_edit_engine.rs`: added `click_then_extend_click_selects` test (the drag path: `click(false)` then `click(true)` → non-empty `selection()` + correct `selection_x`).
- Rebuilt the WASM pkg into `web-editor/src/pkg` (`make wasm`).

## Design Decisions
- **Root cause was stale object identity, not a missing handler.** `textBlocks` is a `computed` that rebuilds fresh `TextBlock` objects whenever `rustBlocks` is reassigned; `openEditor`→`reenterForPage` reassigns it, so a stored `selectedBlock` object ref went stale and every `selectedBlock.value === block` check (incl. `isEditingBlock` and the drag guard) became `false`. A `fill:none` rect with `pointer-events:visiblePainted` then wasn't even hit-testable. Keying selection by the stable `rustId` and deriving the object from the live array restores reference equality with the v-for items, so no other comparison needed changing.
- **No glyph re-raster during selection.** Text is unchanged while dragging/extending, so only the cheap canvas highlight is redrawn; the expensive WASM tile render is reserved for actual text edits. rAF coalescing caps work at one update per frame.

## Test Coverage
- `text_edit_engine`: `click_then_extend_click_selects` (drag selection happy path).
- Full suite: `cargo fmt --check`, `cargo clippy --features wasm-render -D warnings`, `cargo test --features writer,render` → **556 passed, 5 ignored**; `cargo build --target wasm32-unknown-unknown` clean; web-editor `vue-tsc` clean + `quasar build` succeeds.

## Known Limitations / Follow-up
- **Single line/block only.** A multi-line title is several blocks (`group_blocks` splits per baseline); drag selects within one line. Cross-line selection needs a page-level multi-line model (ONLYOFFICE `Page/Line/Glyph` ranges + quads) — deferred.
- **Two-step entry retained** (click to select → click-again/double-click to edit → drag). Unifying mousedown to enter-edit-and-drag in one motion is a possible future UX improvement.

## Flow correction (per user)
Selection must NOT be possible before entering the editor. The engine drag-select was already gated (`onBlockMouseDown` returns unless `editingBlock`), but the browser's **native marquee** still painted a highlight when dragging the overlay pre-edit (the blue box in the original screenshot). Fixed by `user-select: none` on `.annotation-overlay` ([app.scss](web-editor/src/css/app.scss#L498)). Confirmed flow: click → select (handles); click-again/double-click → edit (caret); **then** press-drag → highlight. Dragging before entering the editor now does nothing.
