# Adaptive pre-rendering: device-aware budget + two-tier prefetch — Implementation Report

**Date:** 2026-06-04
**Scope:** web-editor render queue / lazy renderer / scroll view (no Rust changes)

## Problem
The continuous-scroll viewer prefetched a **fixed ±6** page window into a fixed **20-entry /
200 MB** LRU. The 200 MB cap was identical on every machine (overloads small devices, wastes big
ones), the window was symmetric (ignored scroll direction), and far pages showed a skeleton until
fully rendered. Goal: keep a wide band of pages "ready" for smooth scrolling while staying
memory-bounded on large PDFs.

## What Was Implemented
- **Device-aware memory budget** (`useRenderQueue.ts`): `MAX_BYTES = clamp(deviceMemory·64MB,
  128MB, 512MB)` (fallback 4 GB when `navigator.deviceMemory` is absent). `MAX_ENTRIES` raised to 64.
- **Two-tier resolution**: a `Quality = 'full' | 'low'` dimension threaded through
  `cacheKey`/`enqueue`/`getCached`/`putPage`/`renderOne` (all default `'full'`, so existing callers
  are unchanged). `low` renders at `scale·dpr·0.5` (¼ the bytes). `useLazyRender` paints a cached
  `low` bitmap **immediately** as a placeholder (no skeleton/spinner) while the `full` render is
  queued, then swaps it in.
- **Direction-aware, budget-bounded adaptive prefetch** (`buildPrefetchPlan` in `useRenderQueue.ts`):
  replaces the fixed `[1,2,3,-1,-2,-3]`/`[-6..6]` lists. A small full-res near band (3 ahead / 1
  behind, skipped while flinging) plus a wide low-res band sized to fit the remaining byte budget
  (`maxLow = floor((budget·0.9 − nearBandBytes) / lowBytes)`, capped 40), spent ~70 % ahead / 30 %
  behind in the scroll direction. `pruneQueues` now cancels work beyond the adaptive reach.
- **Scroll hint** (`PageScrollView.vue` → `renderQueue.setScrollHint`): an rAF-throttled scroll
  listener reports direction + a `fast` (flinging) flag that auto-clears ~150 ms after scrolling
  settles; the prefetch watcher also falls back to the `currentPage` delta for direction.
- **Correct per-page sizing** (`usePdfStore.pageSize` memoized over `WasmDocument.page_size`,
  cleared on doc swap): `LazyPageCanvas` skeletons and `applyBitmap`'s **canvas CSS size** now come
  from the page's true dimensions, not the bitmap — so a low-res bitmap displays at the right size
  (upscaled) and the low→full swap doesn't reflow; non-A4 pages no longer cause scrollbar jank.

## Design Decisions
- **Cache page *references*… (N/A here).** Budget vs. fixed: device memory is the only signal that
  scales the footprint to the machine; clamped so it never starves or OOMs.
- **`quality` defaults to `'full'`** so the Part-2 commit path (`commitRenderPage` → `putPage`) and
  the commit-paint repaint keep working untouched; only prefetch opts into `'low'`.
- **Low-res band starts *past* the full-res band** (offset `fullReach+1`) so a page isn't cached at
  both tiers when not flinging; while flinging the full band is skipped, so the low band becomes
  contiguous from ±1 — instant placeholders exactly when they're needed.
- **CSS size from `page_size`, backing store from the bitmap** — decouples display size from render
  resolution, the prerequisite for a low-res tier and correct overlay/editor-canvas sizing.

## Test Coverage
- `npx vue-tsc --noEmit` clean (only the pre-existing `baseUrl` tsconfig deprecation). All
  `renderQueue.*` and `store.pageSize` call sites audited for the new signatures.
- Manual (to run in `quasar dev`): large PDF, fast scroll → wide low-res band appears instantly and
  upgrades to crisp; cache bytes stay ≤ budget (emulate a small `deviceMemory` → band shrinks, no
  OOM); zoom re-renders; edit + commit still paints instantly; non-A4 pages reserve correct space.

## Known Limitations / Follow-up
- **DOM virtualization deferred**: every page still mounts a `LazyPageCanvas` + IntersectionObserver
  + overlay. For thousands-of-pages PDFs this remains overhead independent of bitmap memory — the
  planned next step (window-mount with `page_size`-sized spacers).
- Low-res tier is a uniform 0.5×; could be made distance-adaptive (e.g. 0.33× for the far edge).
- `fast` detection is a per-frame pixel-delta threshold; a velocity/easing model could be smoother.
