# render-perf-2 — Implementation Report

**Date:** 2026-05-30
**Scope:** rendering engine — structural performance optimizations (phase 2)

## What Was Implemented

### Change 1 — Document-level decoded stream cache (`Opt 1`)
- Added `decoded_stream_cache: RefCell<HashMap<u32, Vec<u8>>>` field to `PdfDocument` in `src/parser/objects.rs`.
- Added helper `decode_stream_uncached(id)` (private) containing the original decode logic.
- Rewrote `get_stream_data(id)` to check-then-populate the cache: O(1) on cache hit (clone of already-decoded bytes), FlateDecode only on first access per object per document lifetime.
- Modified `decode_contents` in `src/document/page.rs` to match on `content_refs` directly: `Reference(id, _gen)` arm calls `doc.get_stream_data(id)` (cached); direct-stream fallback still calls `stream.decode_with_doc(doc)`.
- **Effect:** For a tiled render of N tiles per page, FlateDecode runs once regardless of N (was N times).

### Change 2 — `FontBytesCache` + `RenderCache` cross-tile API (`Opt 2`)
- Added `FontBytesCache` struct to `src/render/glyph_cache.rs`: `HashMap<String, Option<Vec<u8>>>` with `contains`, `insert`, `get` (returns `Option<&[u8]>` directly) methods.
- Added `RenderCache { glyphs: GlyphCache, font_bytes: FontBytesCache }` struct to the same file.
- Changed `PageRenderer.font_bytes_cache` from `HashMap<String, Option<Vec<u8>>>` to `FontBytesCache`.
- Added `PageRenderer::new_with_render_cache` constructor.
- Added public functions `render_tile_with_render_cache` and `render_page_with_render_cache` (ownership-passing pattern identical to `render_tile_with_cache`).
- Re-exported `FontBytesCache`, `RenderCache`, and new entry points from `src/render/mod.rs`.
- **Effect:** Font TTF stream bytes are decoded once per font per document per render session (not once per tile). Callers holding a `RenderCache` across tiles for the same page see the full benefit.

### Change 3 — `TileCache` `Vec` → `VecDeque` (`Opt 3`)
- In `src/render/tile.rs`: swapped `order: Vec<TileKey>` to `order: VecDeque<TileKey>`.
- `push_back` instead of `push`; `pop_front` (O(1)) instead of `remove(0)` (O(n)) in `evict_until_under_limit`.
- `retain` for LRU promotion on `get` is still O(n) — unchanged, acceptable at typical cache sizes.
- **Effect:** Tile eviction under memory pressure is O(1) instead of O(n). Insert is also O(1) instead of O(n amortized + O(n) for the remove call on duplicates).

### Change 4 — Unconditional `path_device_bbox` call (`Opt 4`)
- In `src/render/page_renderer.rs`, `stroke_path` (line ~131) and `fill_path` (line ~148): wrapped the `path_device_bbox(…)` call inside `if log::log_enabled!(log::Level::Debug) { … }`.
- **Effect:** `path_device_bbox` (iterates all path segments, applies CTM) no longer runs on every stroke/fill in production builds with debug logging disabled.

### Change 5 — `resources_raw` double clone → `Arc<PdfDict>` (`Opt 5`)
- Changed `PageRenderer.resources_raw` from `PdfDict` to `Arc<PdfDict>`.
- Updated all three existing constructors and the new `new_with_render_cache` to accept `Arc<PdfDict>`.
- All three `render_tile*` functions now: `Arc::new(page.resources.raw.clone())` (one clone) then `Arc::clone(&resources_raw)` for the renderer (O(1)).
- Pattern-fill sub-renderer at line ~767: `Arc::clone(&self.resources_raw)` instead of full dict clone.
- `pat_resources` changed from `PdfDict` to `Arc<PdfDict>`; fallback branches use `Arc::clone(&self.resources_raw)` (O(1)) instead of cloning the dict.
- **Effect:** Resources dictionary clones per tile reduced from 2× to 1×. Pattern-fill sub-renders save one full dict clone.

### Change 6 — Criterion benchmark suite (`Opt 6`)
- Added `criterion = "0.5"` to `[dev-dependencies]` in `Cargo.toml`.
- Added `[[bench]] name = "render_bench" required-features = ["render"]`.
- Created `benches/render_bench.rs` with five benchmarks:
  - `render_page_cold` — full page, fresh doc per iteration (measures baseline including FlateDecode).
  - `render_page_warm_stream_cache` — full page, warm `decoded_stream_cache` (measures FlateDecode savings).
  - `render_tiled/{no_cache,glyph_cache_only,render_cache_full}` — tiled pass comparing three cache strategies.
  - `decode_contents/{cold_doc,warm_stream_cache}` — isolates content-stream decode cost.
  - `tile_cache_1000_insert_get` — TileCache LRU throughput.

### Incidental fix — `unused_mut` in interpreter test
- Removed `mut` from `let mut interp` in `test_smask_dict_does_not_zero_alpha` (`src/content/interpreter.rs`).

## Design Decisions

- **`decoded_stream_cache` on `PdfDocument`** — mirrors the existing `obj_stream_cache` pattern. `PdfDocument` is immutable after load so no invalidation is needed; the cache is always correct.
- **`Vec<u8>` in stream cache (no `Arc`)** — `get_stream_data` returns owned `Vec<u8>` per existing API. A clone (fast memcpy) on cache hit is vastly cheaper than FlateDecode. Using `Arc` would add atomic overhead and complicate callers without measurable benefit.
- **`FontBytesCache` / `RenderCache` ownership-passing** — follows the established `render_tile_with_cache` / `GlyphCache` pattern. Avoids second lifetime parameter on `PageRenderer`. WASM-safe (no `Send` required).
- **`Arc<PdfDict>` for resources** — The resources dictionary is read-only during rendering. Wrapping in `Arc` makes sharing O(1). Adding a lifetime parameter to `PageRenderer` (alternative) would propagate to all public API signatures.
- **`VecDeque` for LRU** — stdlib, no new dependency. True O(1) LRU requires a crate (`indexmap`); deferred as unnecessary at current cache sizes.

## Test Coverage

No new tests added — all 330 existing tests pass unchanged (including render integration tests). Observable RGBA output is identical to pre-optimization code. The benchmark suite provides empirical coverage of the new code paths.

## Known Limitations / Follow-up

- **`resources_raw` still cloned once per tile** — The first `Arc::new(page.resources.raw.clone())` in each `render_tile*` call still allocates. Eliminating this would require `Page` to hold `Arc<PdfDict>` natively; deferred.
- **`FontBytesCache` not `Clone`** — `GlyphCache` isn't either. The `bench_render_page_warm_render_cache` benchmark is therefore omitted (can't pre-warm and clone). A future `Clone` impl on both types would unlock that benchmark.
- **Opt 7 (parallel tiles) not implemented** — requires `RefCell` → `Mutex` swap on `PdfDocument` caches and a `parallel` feature; user confirmation pending.
- **Radial tight ellipse culling** and **axial column culling** — deferred from phase 1, still open.
- **No formal benchmark baseline** — no numbers to compare against until `cargo bench` is run on representative PDFs.
