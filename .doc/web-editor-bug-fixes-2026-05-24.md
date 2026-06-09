# web-editor Bug Fixes & WASM API Extensions — Implementation Report

**Date:** 2026-05-24
**Scope:** `web-editor` frontend (Phase 1) + `pdf-editor-rust-core` WASM layer (Phase 2) + frontend wiring (Phase 3)

---

## Background

A cross-project audit compared `web-editor` (Vue 3 + Rust WASM) against `web-apps` (ONLYOFFICE reference implementation) to identify bugs and missing capabilities. Nine confirmed bugs and a set of missing WASM-level features were found. This report documents the full fix implementation across both repositories.

---

## What Was Implemented

### Phase 1 — Frontend-Only Bug Fixes (`web-editor/src/`)

#### 1.1 Outline / Bookmarks navigation broken
- **Files:** `src/types/pdf.ts`, `src/components/OutlinePanel.vue`
- **Root cause:** `OutlineNode.page` was the TypeScript field name, but the WASM `get_outline()` JSON returns `dest_page`. All bookmark clicks resolved to `undefined`, setting the page to `NaN`.
- **Fix:** Renamed interface field to `dest_page: number | null` and added `open: boolean`. Updated `OutlinePanel.vue` to read `item.dest_page` and fall back to a safe `0` when `dest_page` is `null`.

#### 1.2 Insert → Link button opened Metadata dialog
- **File:** `src/layouts/MainLayout.vue`
- **Root cause:** Copy-paste error — the Insert tab's Link button was bound to `@click="openMetadata()"` instead of activating the link tool.
- **Fix:** Changed to `@click="store.setTool('link')"` and added `:class="{ active: store.tool === 'link' }"` for visual feedback.

#### 1.3 `printPdf()` leaked a Blob URL when popup is blocked
- **File:** `src/stores/usePdfStore.ts`
- **Root cause:** `window.open()` returns `null` if the browser blocks popups. The `URL.revokeObjectURL()` call was inside a `load` event listener that never fired in that case.
- **Fix:** Added an early return after a null check that immediately revokes the URL.

#### 1.4 Blurry rendering on HiDPI / Retina displays
- **File:** `src/composables/usePageRenderer.ts`
- **Root cause:** The WASM renderer was called at `store.scale` (1.0 = 72 DPI). On a 2× display the bitmap was stretched to double physical pixels, causing blur.
- **Fix:** Render at `store.scale × devicePixelRatio` for a physically sharp bitmap. Set `canvas.width/height` to the full physical pixel size, then CSS-pin the element at the logical (CSS) pixel size. Publish logical dimensions to the store so `useAnnotations` coordinate math remains correct.

#### 1.5 WASM objects never freed — progressive memory leak
- **Files:** `src/stores/usePdfStore.ts`, `src/composables/usePageRenderer.ts`
- **Root cause:** `WasmEditor`, `WasmDocument`, and `RenderResult` are Rust heap objects exposed through `wasm-bindgen`. They each have an explicit `free()` method that must be called to release memory. The code never called it.
- **Fix:**
  - In `openFile()` and `newPdf()`: `editor.value?.free()` and `doc.value?.free()` before reassigning.
  - In `undo()` and `redo()`: same, before loading the snapshot.
  - In `refreshDoc()`: `doc.value?.free()` before re-parsing.
  - In `usePageRenderer`: `result.free()` after consuming `rgba_bytes()`.
  - `WasmPdfWriter` freed after `build()` in `newPdf()`.

#### 1.6 Annotation opacity darkened color instead of simulating transparency
- **File:** `src/stores/usePdfStore.ts`
- **Root cause:** The WASM annotation API takes RGB with no alpha channel. The old code multiplied `r * opacity`, which just made the color darker (e.g. yellow at 50% became brownish-yellow). Additionally, `addStrikeout` applied no opacity at all — inconsistent with `addHighlight`.
- **Fix:** Added `blendWithWhite(hex, opacity)` which pre-multiplies the color against a white background:
  ```typescript
  r_out = r * opacity + (1 - opacity)  // simulates overlay on white page
  ```
  Both `addHighlight` and `addStrikeout` now use this function for consistent behaviour.

#### 1.7 `fitPage` / `fitWidth` hardcoded A4 dimensions
- **File:** `src/layouts/MainLayout.vue`
- **Root cause:** `fitPage()` and `fitWidth()` hardcoded `842` and `595` (A4 points). Any non-A4 document would zoom to the wrong level.
- **Fix (Phase 1 interim):** Derived page dimensions from `store.canvasWidth / store.scale` and `store.canvasHeight / store.scale`, which gives the correct logical page size from the already-rendered canvas. Replaced by the accurate Phase 3.4 approach using `doc.page_size()`.

#### 1.8 Search match counter misleading for multiple hits on one page
- **File:** `src/components/SearchBar.vue`
- **Root cause:** Matches were stored as `{ page: number }`. Multiple occurrences on the same page all had the same data, so "Next Match" advanced the counter but did not navigate anywhere.
- **Fix:** Added `charOffset: number` to each match entry. Matches are now distinguishable even when they share a page, and the counter accurately reflects forward progress.

---

### Phase 2 — WASM API Extensions (`pdf-editor-rust-core/src/wasm/mod.rs`)

All four additions use Rust types already defined in `src/editor/annotation.rs` or `src/editor/redact.rs`; only the `#[wasm_bindgen]` surface was missing.

#### 2.1 `WasmEditor::add_underline`
- **Rust type used:** `AnnotationType::Underline { color, quad_points }`
- **Signature:** `add_underline(page_index, quad_points: &[f64], r, g, b) -> Result<(), JsError>`
- **Why needed:** `addUnderline` in the store was calling `add_highlight` as a workaround, silently producing wrong annotation subtype (`/Highlight` instead of `/Underline`).

#### 2.2 `WasmEditor::add_redact` + `WasmEditor::apply_redactions`
- **Rust types used:** `AnnotationType::Redact`, `editor::RedactZone`, `editor::apply_redactions()`
- **`add_redact` signature:** `add_redact(page_index, x, y, width, height, r, g, b) -> Result<(), JsError>`
- **`apply_redactions` signature:** `apply_redactions() -> Result<(), JsError>`
- **Design:** `WasmEditor` gained a `pending_redact_zones: Vec<RedactZone>` field. `add_redact` adds both a visual `/Redact` annotation (for preview) and a `RedactZone` to the queue. `apply_redactions` consumes the queue, calls the existing `editor::apply_redactions()` which permanently rewrites all affected content streams, then re-opens the editor from the clean bytes so further edits are possible.
- **Why two-step:** This mirrors the industry-standard PDF redaction workflow (mark → review → apply) and avoids a destructive one-shot operation without user confirmation.

#### 2.3 `WasmEditor::add_ink`
- **Rust type used:** `AnnotationType::Ink { ink_list }`
- **Signature:** `add_ink(page_index, points: &[f64], r, g, b, line_width) -> Result<(), JsError>`
- **Input format:** Flat `[x0, y0, x1, y1, …]` array (one stroke). The WASM method packs it into `ink_list: Vec<Vec<[f64; 2]>>` with a single stroke entry.
- **Bounding box:** Computed from min/max of all points, padded by `line_width / 2.0`.
- **Style storage:** Color and line width are embedded as a `/Subj` annotation field (e.g. `"w=2 r=0.8 g=0.2 b=0.1"`) so the data survives round-trips pending a proper appearance stream implementation.

#### 2.4 `WasmDocument::page_size`
- **Signature:** `page_size(page_index) -> Result<Float64Array, JsError>`
- **Returns:** `[width_pt, height_pt]` in PDF user-space points from the page's `MediaBox`.
- **Implementation:** Reuses the existing `Page::from_dict()` → `page.media_box.width()/height()` path already used by the renderer.
- **Used by:** `fitPage`, `fitWidth`, `addBlankPage` in the frontend.

---

### Phase 3 — Frontend Wiring for New WASM APIs

#### 3.1 `addUnderline` store action corrected
- **File:** `src/stores/usePdfStore.ts`
- Changed `editor.value.add_highlight(...)` call to `editor.value.add_underline(...)`.

#### 3.2 Redact tab fully wired
- **Files:** `src/types/pdf.ts`, `src/stores/usePdfStore.ts`, `src/composables/useAnnotations.ts`, `src/layouts/MainLayout.vue`
- Added `'redact'` to the `Tool` union type.
- `usePdfStore` gained `addRedact(x, y, w, h)` and `applyRedactions()` actions.
- `useAnnotations` handles the `redact` tool as a rect-drag (same as highlight/link).
- "Mark for Redaction" button sets tool to `'redact'`. "Apply Redactions" button triggers a confirmation dialog then calls `store.applyRedactions()` and resets tool to `'select'`.

#### 3.3 Freehand draw tool wired
- **Files:** `src/types/pdf.ts`, `src/stores/usePdfStore.ts`, `src/composables/useAnnotations.ts`, `src/layouts/MainLayout.vue`
- Added `'draw'` to the `Tool` union type.
- `usePdfStore` gained `addInk(points: Float64Array)` action.
- `useAnnotations` accumulates screen-space points in `drawPoints[]` on `mousemove` when tool is `'draw'`. On `mouseup` the points are batch-converted to PDF space via `screenToPdf()` and submitted as a single ink stroke.
- Comment tab "Draw" and "Freehand" buttons both activate the `draw` tool.

#### 3.4 `fitPage` / `fitWidth` use real page dimensions
- **File:** `src/layouts/MainLayout.vue`
- Both functions now call `store.doc.page_size(store.currentPage)` and use the returned `[w, h]` for scaling. A catch block falls back to canvas-derived dimensions if the call fails.

#### 3.5 `addBlankPage` uses the current page's actual size
- **File:** `src/stores/usePdfStore.ts`
- Calls `doc.value.page_size(currentPage.value)` to determine width/height. Falls back to A4 (595 × 842) on error.

---

## Design Decisions

### Why `blendWithWhite` instead of ignoring opacity for highlights?
The WASM `add_highlight` writes RGB values into the PDF `/C` color key — no alpha. Simply dropping opacity would make the slider in the UI feel broken. Pre-blending against white is a reasonable approximation for typical white-background PDFs and preserves the UX contract that the slider visually changes the annotation appearance.

### Why store `pending_redact_zones` inside `WasmEditor` instead of in the Vue store?
Redact zones are tightly coupled to the editor's internal byte state — if the user undoes before applying, the zones must also be rolled back. Keeping them inside `WasmEditor` ensures the history snapshot mechanism (which saves and restores raw editor bytes via `WasmEditor.open`) automatically discards any queued zones when the state is restored.

### Why re-open the editor after `apply_redactions`?
`apply_redactions` calls `save_new()` internally, which produces a fully self-contained PDF (no byte from the original survives). The incremental update model (`save_append`) can't be used on top of a `save_new` output — re-opening with `WasmEditor::open` from the new bytes resets the baseline correctly.

### Why flatten `add_ink` points to a single stroke per call?
A multi-stroke ink annotation (user lifts and re-presses) would require batching strokes across multiple mousedown/mouseup cycles. That's a more complex state machine in `useAnnotations`. A single stroke per drag covers the most common freehand use case and keeps the API simple. Multi-stroke can be composed by calling `add_ink` multiple times.

### Why embed style in `/Subj` for ink annotations?
The PDF `Ink` annotation spec uses a `/BS` (Border Style) dictionary for line width and `/C` for color. Adding these correctly requires an appearance stream (`/AP`). That's a non-trivial rendering task deferred as a known limitation. Storing the values in `/Subj` preserves them in the PDF bytes for a future implementation without blocking the feature now.

---

## Test Coverage

No new automated tests were added in this session (frontend changes are UI-only; Rust changes reuse existing `AnnotationBuilder` paths already covered by `tests/write_edit.rs`). Manual verification:

- `cargo fmt --check` → clean after `cargo fmt`
- `cargo clippy -- -D warnings` → no warnings
- `cargo test` → all tests pass
- `npx tsc --noEmit` (web-editor) → no type errors
- `make wasm` → WASM built successfully; all five new methods confirmed in generated `pdf_core.d.ts`

---

## Files Changed

### `pdf-editor-rust-core/`
| File | Change |
|------|--------|
| `src/wasm/mod.rs` | Added `pending_redact_zones` field to `WasmEditor`; added `add_underline`, `add_redact`, `apply_redactions`, `add_ink` to `WasmEditor`; added `page_size` to `WasmDocument` |

### `web-editor/src/`
| File | Change |
|------|--------|
| `types/pdf.ts` | Fixed `OutlineNode` fields (`dest_page`, `open`); added `'redact'` and `'draw'` to `Tool` type |
| `components/OutlinePanel.vue` | Read `item.dest_page` instead of `item.page`; safe label fallback |
| `stores/usePdfStore.ts` | Fixed `addUnderline` method; fixed `printPdf` URL leak; added `free()` calls in `openFile`, `newPdf`, `undo`, `redo`, `refreshDoc`; fixed `addBlankPage` page size; added `blendWithWhite` helper; added `addRedact`, `applyRedactions`, `addInk` actions |
| `composables/usePageRenderer.ts` | Added `devicePixelRatio` scaling; call `result.free()` after use |
| `composables/useAnnotations.ts` | Added `draw` tool point accumulation; added `redact` and `draw` dispatch in `onMouseUp` |
| `layouts/MainLayout.vue` | Fixed Insert→Link button handler; wired Redact tab (Mark + Apply); wired Draw/Freehand buttons; added `handleApplyRedactions()`; fixed `fitPage`/`fitWidth` with `doc.page_size()` |
| `components/SearchBar.vue` | Added `charOffset` to match entries |
| `pkg/pdf_core.d.ts` | Auto-generated by `wasm-pack`; includes all new method signatures |

---

## Known Limitations / Follow-up

| Item | Detail |
|------|--------|
| Ink annotation appearance | Color and line width stored in `/Subj` for now; a proper `/AP` appearance stream would render the stroke visibly in external PDF viewers |
| Search highlight overlay | Search navigates to the correct page but does not visually highlight the matched text on the canvas; needs a coordinate-aware hit-test once `extract_text` exposes character bounding boxes |
| Annotation selection/delete | No way to click an existing annotation to select or delete it; requires `list_annotations(page_index)` and `delete_annotation(page_index, id)` WASM APIs (the Rust `delete_annotation` function exists, needs a WASM wrapper) |
| Password-protected PDFs | `WasmDocument::parse_with_password` exists behind the `crypto` Cargo feature but is not compiled into the current WASM build; needs `--features wasm-render,crypto` and a password prompt in the UI |
| PDF form fields | `src/forms/` module exists in Rust but has no WASM surface; Forms tab remains fully disabled |
| Multi-stroke ink | Each drag creates one ink annotation; lifting the pen and continuing is separate annotations rather than one multi-stroke object |
| Redact zone persistence through undo | `pending_redact_zones` is stored in `WasmEditor` struct which is replaced on undo/redo; queued-but-unapplied redact marks are lost if the user undoes past the point they were added |
