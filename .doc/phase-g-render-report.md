# Phase G — Rendering Implementation Report

**Date:** 2026-05-23  
**Tests:** 212 passing, 0 failing  
**New code:** ~1,600 lines across 8 files in `src/render/`  
**WASM:** `cargo build --target wasm32-unknown-unknown --features render` clean

---

## What Was Built

A lazy, tile-based PDF rendering pipeline inspired by ONLYOFFICE's streaming interpreter.
The renderer is gated behind the `render` Cargo feature and adds zero weight to the default build.

### New files

| File | Lines | Purpose |
|------|------:|---------|
| `src/render/mod.rs` | 32 | Feature gate, module declarations, public re-exports |
| `src/render/canvas.rs` | 151 | `PixmapBuffer` — tile-aware RGBA pixel buffer |
| `src/render/color.rs` | 86 | Color space converters (Gray, RGB, CMYK → RGBA) |
| `src/render/path_render.rs` | 206 | Vector path fill/stroke via tiny-skia |
| `src/render/glyph_cache.rs` | 162 | TrueType glyph rasterizer via ab_glyph, with LRU cache |
| `src/render/image.rs` | 236 | Image decoder — JPEG (zune-jpeg), raw Gray/RGB/CMYK |
| `src/render/tile.rs` | 308 | `TileRect`, `TileKey`, `TileCache` (LRU memory-capped) |
| `src/render/page_renderer.rs` | 446 | `PageRenderer` (OutputDevice impl), `render_tile`, `render_page` |

### Modified files

| File | Change |
|------|--------|
| `src/content/operators.rs` | Added `ContentStreamIter<'a>` — streaming one-pass parser |
| `src/content/interpreter.rs` | Added `interpret_iter()` generic over any `Iterator<Item=Result<Operation>>` |
| `Cargo.toml` | Added `tiny-skia`, `ab_glyph`, `zune-jpeg` under `[features] render` |
| `src/lib.rs` | Added `#[cfg(feature = "render")] pub mod render` |

---

## Architecture Overview

```
render_tile(doc, page, scale, tile_rect)          ← primary public API
  │
  ├─ ContentStreamIter::new(raw_bytes)            ← streaming parser, O(operand stack) memory
  │
  ├─ ContentInterpreter::interpret_iter(iter, device)   ← one-pass execution
  │        └─ fires OutputDevice callbacks per operation
  │
  └─ PageRenderer  (OutputDevice impl)
       ├─ canvas: PixmapBuffer  [tile_w × tile_h pixels only]
       ├─ path_render  → tiny-skia
       ├─ glyph_cache → ab_glyph
       └─ image       → zune-jpeg / raw

TileCache::get_or_render(key, tile, closure)      ← LRU cache wrapping render_tile
  └─ evicts oldest tiles when total memory > max_bytes (default 64 MiB)
```

---

## Comparison with ONLYOFFICE

### The Core Problem

ONLYOFFICE handles large PDFs (multi-hundred-page engineering drawings, scanned books) through
a **streaming one-pass interpreter** — `Gfx::display()` reads one token, executes it, fires
the renderer callback, then discards it. Memory is O(operand stack depth ≤ 6), never O(page
operation count).

The old design in this codebase did the opposite: `parse_content_stream()` collected the
**entire page** into `Vec<Operation>` before executing anything. For a 200-page technical PDF
with 50,000 operations per page that is megabytes of parsed AST per page.

### Memory Model: Before vs. After

```
BEFORE:
  parse_content_stream() → Vec<Operation>  [50k items, ~5 MB per page]
                         ↓
  interpret()            → executes all 50k at once

AFTER:
  render_tile(tile_rect):
    ContentStreamIter → yields 1 Operation at a time
    interpret_iter()  → execute → OutputDevice callback → discard

    Memory at any moment:
      operand_stack  ≤ 6 PdfObjects
      pixel buffer   tile_w × tile_h × 4 bytes
      (for 256pt tile at 2× scale: 512 × 512 × 4 = 1 MB)
      — regardless of page complexity
```

---

### Component-by-Component

#### Streaming Parser — `ContentStreamIter`

| | ONLYOFFICE | This implementation |
|---|---|---|
| Mechanism | `Gfx::display()` reads one token via `Lexer::getObj()` | `ContentStreamIter<'a>` implements `Iterator<Item=Result<Operation>>` |
| Memory | One operand stack, bounded | `operand_stack: Vec<PdfObject>`, max ~6 items |
| Compatibility | Single codepath | Parallel path — old `parse_content_stream()` and all 188 existing tests unchanged |

#### Streaming Interpreter — `interpret_iter()`

| | ONLYOFFICE | This implementation |
|---|---|---|
| Loop | `while (obj = lexer->getObj()) { dispatch(obj, renderer) }` | `for op_result in operations { dispatch(&op, device) }` |
| OutputDevice | Abstract C++ class with virtual callbacks | Rust `trait OutputDevice`: `fill_path`, `stroke_path`, `draw_text_span`, `draw_image`, `draw_image_xobject` |
| Renderer plug-in | `SplashOutputDevice`, `TextOutputDevice` | `PageRenderer` (render), `TextExtractor` (pre-existing) — same plug-in pattern |

The old `interpret()` now delegates to `interpret_iter()` internally, keeping all existing
tests passing without modification.

#### Tile Cache — `TileRect` + `TileCache`

| | ONLYOFFICE | This implementation |
|---|---|---|
| Tile request | Viewport sends rect in page coordinates | `TileRect { x, y, width, height }` in PDF user-space points |
| Cache key | Page index + scale + tile offset | `TileKey { page_index, scale_x64, tile_x100, tile_y100 }` — integer fields, hashable |
| Eviction | Tiles scrolled off-screen are freed | LRU via `Vec<TileKey>` insertion-order tracker; evicts front (oldest) when over memory cap |
| Memory cap | Configurable per viewer instance | `TileCache::new(max_bytes)`, default 64 MiB |
| On-demand render | Viewer calls `displayPage()` for visible tiles | `TileCache::get_or_render(key, tile, closure)` — hit returns immediately, miss calls closure |

#### Tile Pixel Buffer — `PixmapBuffer`

| | ONLYOFFICE | This implementation |
|---|---|---|
| Buffer | `SplashBitmap` — owns pixel data | `PixmapBuffer` wrapping `tiny_skia::Pixmap` |
| Tile allocation | Allocates only tile bitmap, not full page | `PixmapBuffer::new_tile(tile_w, tile_h, origin)` |
| Tile origin | Absolute page-pixel offset tracked per tile | `TileOrigin { x, y }` embedded in buffer; `blit_rgba` auto-subtracts origin |
| Compositing | AGG source-over | Porter-Duff source-over in `blit_rgba`, per-pixel |

#### Coordinate System / Y-Flip

| | ONLYOFFICE | This implementation |
|---|---|---|
| PDF y=0 (bottom) → screen y=0 (top) | CTM contains Y-flip baked in at start | Initial CTM set before `interpret_iter`: `[scale, 0, 0, -scale, -(tile.x·scale), (tile.y+tile.h)·scale]` |
| Tile offset | Clip region or translate in display matrix | Baked into the same initial CTM `e` and `f` components |
| Subsequent `cm` operators | Stack on top of initial CTM | Same — `Matrix::concat()` in interpreter |

PDF point `(x, y)` maps to tile-local pixel `(x·s − tile_x·s, (tile_y+tile_h−y)·s)`.

#### Path Rendering

| | ONLYOFFICE | This implementation |
|---|---|---|
| Rasterizer | Anti-Grain Geometry (AGG), C++ | tiny-skia, pure Rust, WASM-safe |
| Primitives | Cubic Béziers, fill + stroke | `PathSegment::{MoveTo, LineTo, CurveTo, ClosePath}` → `tiny_skia::PathBuilder` |
| Fill rules | Even-odd / non-zero winding | `FillRule::{EvenOdd, NonZero}` → `tiny_skia::FillRule::{EvenOdd, Winding}` |
| Line style | Cap, join, miter limit, dash pattern | `tiny_skia::Stroke { line_cap, line_join, miter_limit, dash }` fully mapped |
| Anti-aliasing | Yes | `Paint { anti_alias: true, .. }` |

#### Glyph Rendering

| | ONLYOFFICE | This implementation |
|---|---|---|
| Rasterizer | FreeType, C library | ab_glyph, pure Rust, WASM-safe |
| Cache | Per-font glyph bitmap cache | `HashMap<(font_name, char, size_px×64), GlyphBitmap>` |
| Output | 8-bit alpha mask | `GlyphBitmap { pixels: Vec<u8>, width, height, bearing_x, bearing_y, advance_x }` |
| Compositing | Alpha-blend over background with fill color | `blit_alpha_mask()`: coverage × fill_color → Porter-Duff into canvas |
| Fallback (no TTF) | Standard font metrics for advance | Returns `None`; renderer draws placeholder rect, advances by `size × 0.5` |

#### Image Decoding

| | ONLYOFFICE | This implementation |
|---|---|---|
| JPEG | libjpeg-turbo | zune-jpeg, pure Rust, WASM-safe |
| Raw pixels | Direct buffer copy | `decode_raw()` handles Gray/RGB/CMYK, 8-bit and 16-bit (down-sampled) |
| CMYK | Full ICC profile support | Direct formula: `R = (1−C)(1−K)·255` |
| Unsupported filters | JPX, JBIG2, CCITTFax decoders | `log::warn!` + gray placeholder |
| Image scaling | Bilinear or nearest-neighbour | Nearest-neighbour `scale_rgba_nearest()` |

---

## New Dependencies (all WASM-safe)

```toml
tiny-skia = { version = "0.11", optional = true, default-features = false, features = ["std"] }
ab_glyph  = { version = "0.2",  optional = true }
zune-jpeg = { version = "0.4",  optional = true }
```

All three are pure Rust with no C FFI, compile to `wasm32-unknown-unknown` without flags.

---

## Verification

```
cargo fmt --check                                        ✓
cargo clippy --features render -- -D warnings            ✓  (0 warnings)
cargo test --features render                             ✓  212 passed, 0 failed
cargo build --target wasm32-unknown-unknown --features render  ✓
```

---

## Known Gaps vs. ONLYOFFICE (Additive — No Structural Changes Required)

| Feature | ONLYOFFICE | Current status |
|---|---|---|
| Background tile pre-fetch | Adjacent tiles rendered in background thread | Not yet — `get_or_render` is synchronous |
| ICC color profiles | Full ICC engine | CMYK direct formula only |
| Type 3 fonts | Glyph programs executed as mini content streams | Not implemented |
| Transparency groups | Full PDF 1.4 soft-mask compositing | Not implemented |
| Standard 14 fonts (shapes) | FreeType renders built-in metrics | Placeholder rect (text position is correct, glyph shape invisible) |
| Inline images | Full BI/EI dict parsing | Position-only 1×1 blit |

All of these are additive features on top of the existing `OutputDevice` callbacks and tile
system. They do not require structural changes to the architecture.
