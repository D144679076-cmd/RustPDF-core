# Core-Fonts Fallback — Implementation Report

**Date:** 2026-05-23  
**Tests:** 217 passing (+5 new), 0 failing  
**New code:** `src/render/font_resolver.rs` (~400 lines)  
**WASM:** `cargo build --target wasm32-unknown-unknown --features render` clean, 0 warnings

---

## Problem

PDFs that reference fonts without embedding them — including all 14 Standard PDF fonts
(Helvetica, Times, Courier, Symbol, ZapfDingbats) — rendered every character as a filled
placeholder rectangle. The `core-fonts` repository (249 MB, 186 TTF/OTF files) contains
open-source substitutes, but couldn't be naively bundled due to WASM binary size limits.

---

## Solution: Three-Tier Font Resolution

```
Font resolution order in draw_text_span():

  1. Embedded TTF/OTF from the PDF itself           ← unchanged, always tried first

  2. FontResolver::resolve(name, bold, italic)
       │
       ├─ RuntimeFontRegistry  (all targets)        ← thread-local HashMap
       │   register_font(name, bytes)               ← JS host injects bytes at runtime
       │   pdf_register_font() WASM C export        ← covers any font, zero binary cost
       │
       ├─ EmbeddedFontResolver  (all targets)       ← Liberation + DejaVu via include_bytes!()
       │   ~2 MB added to binary                    ← covers all 14 Standard PDF fonts always
       │   Always compiled, WASM-safe
       │
       └─ DirectoryFontResolver  (native only)      ← walks core-fonts/ directory at runtime
           cfg(not(wasm32))                         ← 186 fonts available, zero binary impact
           compiled OUT of WASM entirely            ← no WASM breakage

  3. Placeholder rectangle                          ← only if all above fail
```

---

## Standard Font Substitution Table

| Standard PDF Font(s) | Substitute |
|---|---|
| Helvetica, Helvetica-Bold, Helvetica-Oblique, Helvetica-BoldOblique | Liberation Sans (4 variants) |
| Times-Roman, Times-Bold, Times-Italic, Times-BoldItalic | Liberation Serif (4 variants) |
| Courier, Courier-Bold, Courier-Oblique, Courier-BoldOblique | Liberation Mono (4 variants) |
| Symbol, ZapfDingbats | DejaVu Sans (best-effort) |
| Arial, TimesNewRoman, CourierNew | same as above via alias mapping |

---

## New Files

| File | Lines | Purpose |
|------|------:|---------|
| `src/render/font_resolver.rs` | ~400 | `FontResolver` trait, `RuntimeFontRegistry`, `EmbeddedFontResolver`, `DirectoryFontResolver`, WASM export |

## Modified Files

| File | Change |
|------|--------|
| `src/render/page_renderer.rs` | `font_resolver: Box<dyn FontResolver>` field on `PageRenderer`; resolver fallback in `draw_text_span()`; new `render_*_with_resolver` public fns |
| `src/render/mod.rs` | Re-exports `FontResolver`, `EmbeddedFontResolver`, `DirectoryFontResolver` (native), `register_font`, new render fns |

---

## Public API

```rust
// Default render fns — unchanged, use EmbeddedFontResolver automatically
render_page(doc, page, scale) -> Result<PixmapBuffer>
render_tile(doc, page, scale, tile) -> Result<PixmapBuffer>

// Custom resolver variants (WASM + native)
render_page_with_resolver(doc, page, scale, resolver: Box<dyn FontResolver>) -> Result<PixmapBuffer>
render_tile_with_resolver(doc, page, scale, tile, resolver: Box<dyn FontResolver>) -> Result<PixmapBuffer>

// Runtime font registration (WASM: JS calls pdf_register_font C export)
render::register_font(name: String, data: Vec<u8>)

// Native-only: full core-fonts directory
DirectoryFontResolver::new(core_fonts_path: &Path) -> DirectoryFontResolver
```

### Native usage (all 186 core-fonts)

```rust
use pdf_core::render::{render_page_with_resolver, DirectoryFontResolver};

let resolver = DirectoryFontResolver::new(Path::new("/path/to/core-fonts"));
let buf = render_page_with_resolver(&doc, &page, 2.0, Box::new(resolver))?;
```

### WASM usage (inject extra fonts from JS)

```javascript
// Fetch a font and register it with the WASM module
const fontBytes = new Uint8Array(
  await fetch('/fonts/NotoSans-Regular.ttf').then(r => r.arrayBuffer())
);
const nameBytes = new TextEncoder().encode('NotoSans-Regular');
const namePtr = module._malloc(nameBytes.length);
const dataPtr = module._malloc(fontBytes.length);
module.HEAPU8.set(nameBytes, namePtr);
module.HEAPU8.set(fontBytes, dataPtr);
module._pdf_register_font(namePtr, nameBytes.length, dataPtr, fontBytes.length);
module._free(namePtr);
module._free(dataPtr);
// Now render_page() will use NotoSans-Regular as a fallback
```

---

## WASM Binary Impact

| Component | WASM binary addition |
|---|---|
| `EmbeddedFontResolver` (Liberation × 12 + DejaVu × 1) | ~2 MB |
| `DirectoryFontResolver` | 0 bytes (compiled out) |
| `RuntimeFontRegistry` | ~0 bytes (just a thread-local HashMap) |

---

## Remaining Gap: CJK Fonts

Noto CJK, Nanum, Takao Gothic, WQY ZenHei are present in `core-fonts` and will be indexed
by `DirectoryFontResolver`, but CJK PDFs use **composite (Type 0/CID) fonts** whose character
codes require CID-to-GID mapping that `ContentInterpreter` does not yet decode. These fonts
will become useful automatically once CID rendering is added — no changes to `font_resolver.rs`
will be needed.

---

## Verification

```
cargo fmt --check                                              ✓
cargo clippy --features render -- -D warnings                 ✓  0 warnings
cargo test --features render                                  ✓  217 passed, 0 failed
cargo build --target wasm32-unknown-unknown --features render ✓  0 warnings
```
