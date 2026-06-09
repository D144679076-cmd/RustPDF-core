# render-fontdue-cff — Implementation Report

**Date:** 2026-05-27
**Scope:** Replace `ab_glyph` with `fontdue` to fix CFF/OTF embedded-font rendering and text positioning

## What Was Implemented

- **`Cargo.toml`**: Replaced `ab_glyph = "0.2"` with `fontdue = "0.9"` in the `render` feature.
- **`src/render/glyph_cache.rs`**: Full rewrite — `FontArc` replaced with `fontdue::Font`.  `rasterize()` now uses `fontdue::Font::from_bytes` (handles TTF and CFF) and `font.rasterize(ch, size_px)` for alpha-mask generation.  `bearing_y` is computed as `metrics.ymin + metrics.height as i32` to get the top-of-glyph offset from the baseline.
- **`src/render/font_resolver.rs`**: Changed the `embedded_font_bytes` wildcard arm from `return None` to `_ => DEJAVU_SANS`, so unrecognised family names (including CID-generated names like `"CIDFont+F1"`) always return a valid Unicode-complete fallback instead of `None`.
- **`src/render/page_renderer.rs`**: Removed the dead `.or_else(|| self.font_resolver.resolve("helvetica", bold, italic))` chain; the resolver now always returns `Some` for any input.

## Design Decisions

- **fontdue over ab_glyph**: `ab_glyph` only handles TrueType outlines; it silently returns `None` for every glyph in a CFF font (the format produced by LibreOffice and Word). `fontdue` uses `ttf-parser` internally and handles both TTF and CFF. This matches what ONLYOFFICE does with FreeType.
- **DejaVu as universal wildcard**: DejaVu Sans covers Latin Extended, Vietnamese, Greek, and Cyrillic — far wider than Liberation Sans. Using it as the wildcard fallback means any unrecognised font (custom names, CID-prefix names) still renders legible Unicode text rather than blocks.
- **`#[cfg(feature = "render")]` guard on `rasterize`**: `fontdue` is only included under the `render` feature. The `GlyphCache` struct compiles in all configurations but the rasterize method (and the `fonts` HashMap field) are gated so the crate stays minimal without the feature.

## Test Coverage

| Test | What it covers |
|------|----------------|
| `test_rasterize_letter_a` | Happy-path: loads a TTF fixture, rasterizes 'A' at 24 px, asserts non-zero coverage |
| `test_rasterize_invalid_font_returns_none` | Error-path: `b"not a ttf"` → `fontdue` parse fails → returns `None` |

Existing `font_resolver` tests (`test_normalize_font_name_standard`, `test_embedded_resolver_covers_all_14_standard_fonts`, etc.) continue to pass — the wildcard change only adds DejaVu to the set of resolved fonts, it does not break existing matches.

## Known Limitations / Follow-up

- **Space and zero-extent glyphs**: `rasterize` returns `None` for spaces and control characters (zero `metrics.width/height`). The caller in `page_renderer.rs` must advance by the PDF `/W` array width for these characters; this is already handled by the existing placeholder-advance path.
- **Ligature and OpenType layout**: `fontdue` does not apply GSUB/GPOS tables. Complex Arabic/Indic scripts and ligatures will not render correctly. A future improvement would be to integrate `rustybuzz` for shaping before rasterization.
- **Per-glyph bitmaps not batched**: Each character is rasterized and cached individually. For very large text blocks this could be memory-intensive; a glyph atlas would be a more efficient structure.
