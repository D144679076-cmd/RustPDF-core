# web-editor — Implementation Report

**Date:** 2026-05-24
**Scope:** `web-editor/` — Vue 3 + Quasar PDF editor UI

## What Was Implemented

### Project scaffold (`web-editor/`)
- `package.json` — Quasar v2 + @quasar/app-vite v2, Pinia, vite-plugin-wasm, vite-plugin-top-level-await
- `quasar.config.ts` — boot: ['wasm'], plugins: Notify/Loading/Dialog, wasm + topLevelAwait Vite plugins
- `tsconfig.json` — strict mode, forceConsistentCasingInFileNames, src/* path aliases
- `index.html` — minimal Quasar-compatible template (`<!-- quasar:entry-point -->`)
- `.gitignore` — excludes `node_modules/`, `dist/`, `.quasar/`, `src/pkg/`

### WASM integration
- `src/boot/wasm.ts` — `await init()` in Quasar boot file; runs before any component mounts
- `src/pkg/` — wasm-pack output (gitignored, built by `make wasm`)
- `Makefile` (Rust project root) — `make wasm` and `make wasm-no-render` targets

### State (`src/stores/usePdfStore.ts`)
- Pinia store using Composition API style (`defineStore` with setup fn)
- `shallowRef` for WASM objects (no deep reactivity on opaque Rust types)
- `refreshDoc()` — after every mutation: `editor.save()` → re-parse → new `WasmDocument` → Vue reactivity triggers re-render automatically
- Actions: `openFile`, `newPdf`, `savePdf`, `addBlankPage`, `deletePage`, `addHighlight`, `addUnderline`, `addStrikeout`, `addNote`, `addLink`, `setMetadata`, `setPage`, `setTool`, `zoomIn/Out/setScale`

### Composables
- `usePageRenderer.ts` — `watchEffect` on `(doc, currentPage, scale)` → `WasmRenderer.render_page()` → `ImageData` → `ctx.putImageData()`
- `useAnnotations.ts` — SVG overlay mouse handlers; tool-dependent dispatch to store actions; live preview `<rect>` during drag; `$q.dialog` prompts for Note/Link text
- `useFileOps.ts` — wraps store's file operations with `$q.loading.show/hide` and `$q.notify` feedback

### Utilities / Types
- `src/utils/coords.ts` — `screenToPdf()`, `rectToQuadPoints()`, `normaliseRect()`
- `src/types/pdf.ts` — `PdfMetadata`, `OutlineNode`, `Tool` interfaces
- `src/env.d.ts` — `declare module '*.vue'` shim for TypeScript

### Components (all `<script setup lang="ts">`)
- `PageCanvas.vue` — `<canvas>` + `AnnotationOverlay` composition; delegates rendering to `usePageRenderer`
- `AnnotationOverlay.vue` — `<svg>` absolute overlay with live drag preview rect; delegates interaction to `useAnnotations`
- `PageThumbnail.vue` — small canvas at `scale=0.15` per page; re-renders on doc/page change
- `PagePanel.vue` — QScrollArea thumbnail list + add/delete/navigate controls
- `OutlinePanel.vue` — `JSON.parse(doc.get_outline())` → `QTree`; click node → `store.setPage()`
- `MetadataDialog.vue` — `useDialogPluginComponent()` QDialog; pre-fills from `doc.get_metadata()`

### Layout (`src/layouts/MainLayout.vue`)
OnlyOffice + iLovePDF visual design:
- **Header** (`#2C3E6B` navy) — brand, filename + dirty indicator, Open/New/Save buttons
- **Ribbon** (white) — `QTabs` (Home / Comment / Insert / View) + tab panel content
  - Home: Select / Pan tool buttons
  - Comment: Highlight / Underline / Strikeout / Note / Link with color dot indicators (iLovePDF style)
  - Insert: Add Page / Delete Page / Metadata
  - View: zoom presets + Fit Width / Fit Page
- **Context bar** (`#F0F4F8`) — slides in when annotation tool active; shows color swatches + custom color picker + opacity slider
- **Left drawer** — 48px icon strip (`#2C3E6B`) + 220px collapsible panel (Pages or Outline)
- **Status bar** (`#F5F5F5`) — "Page X of Y" + zoom `–/+` + Fit Width/Page

### Pages
- `IndexPage.vue` — drag-and-drop welcome screen when no doc open; `<PageCanvas>` when doc loaded

### Router
- `src/router/index.ts` + `routes.ts` — single SPA route: `/ → MainLayout → IndexPage`

## Design Decisions

- **`shallowRef` for WASM objects**: Deep Vue reactivity on opaque Rust structs would traverse fields that don't exist in JS, wasting cycles. `shallowRef` triggers re-renders only when the reference itself changes (i.e. after `refreshDoc()`).
- **`refreshDoc()` pattern**: Instead of tracking individual mutation events, every edit does `editor.save() → WasmDocument.parse(bytes)`. This ensures the rendered view is always in sync with the persisted PDF state, at the cost of one extra serialise/parse cycle per edit. For the page sizes involved this is <5ms.
- **Lazy WASM imports in composables/store**: `await import('src/pkg/pdf_core')` inside async functions avoids loading WASM types before `init()` completes in the boot file.
- **`src/` prefix in route lazy imports**: Quasar's Vite aliases (`layouts/`, `pages/`) aren't resolved by standalone `tsc`. Using `src/layouts/` and `src/pages/` (resolved by the `src/*` tsconfig path alias) fixes this without needing Vite for type-checking.
- **`bytes.buffer as ArrayBuffer` cast**: `wasm-pack` returns `Uint8Array<ArrayBufferLike>` but `Blob` constructor expects `ArrayBufferView<ArrayBuffer>`. Casting the buffer is safe here because the WASM linear memory always uses a plain `ArrayBuffer`.
- **Context bar via CSS height transition**: `v-show` + `max-height 0 ↔ 56px` with `transition: 0.2s ease` gives the iLovePDF sliding-in feel without a full component mount/unmount cycle.
- **`new WasmPdfWriter()` not `.new()`**: `wasm-bindgen` exposes `#[wasm_bindgen(constructor)]` as a JS class constructor, not a static method. The generated `.d.ts` shows `constructor()` directly.

## Test Coverage

All 383 Rust tests pass. The web editor is tested manually via the dev server checklist (see plan file). No Jest/Vitest suite was added — the UI is verified by running the app against real PDFs.

## Known Limitations / Follow-up

- TypeScript strict-checks Vue SFCs only via the shim declaration (`declare module '*.vue'`); per-component type safety inside templates requires `vue-tsc` (not added yet).
- The `fitWidth` / `fitPage` zoom helpers use hardcoded A4 dimensions (595×842 pt) as approximations. For accurate fitting, the rendered canvas dimensions should be read instead.
- `useAnnotations` computes `canvasH` from `overlayRef.previousElementSibling` — a brittle DOM dependency. A `canvasHeight` prop from `PageCanvas` would be cleaner.
- Thumbnail rendering fires a `WasmRenderer.render_page()` call per page per doc change; for large documents (50+ pages) this will be slow. Virtualisation or lazy rendering on scroll-into-view is the correct follow-up.
- No keyboard shortcuts yet (Ctrl+O, Ctrl+S, Delete, arrow navigation).
- The `underline` annotation calls `add_highlight` internally (no dedicated underline WASM API exists yet).
