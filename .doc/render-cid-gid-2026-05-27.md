# render-cid-gid — Implementation Report

**Date:** 2026-05-27
**Scope:** CID-based glyph rasterization for correct CFF/OpenType font rendering

## What Was Implemented

### `src/content/text_state.rs`
- Added `font_size_px: f64` to `TextSpan` — device-pixel font height pre-computed from the full
  render matrix (font_size × text_matrix_scale × CTM_scale), removing the renderer's dependence on
  a stored `self.scale` value.
- Added `char_cids: Vec<u32>` to `TextSpan` — original CID (character identifier) per Unicode
  character, non-empty only for composite (Type0/CIDFont) fonts.

### `src/content/interpreter.rs`
- `show_text()` now computes `font_size_px` as the length of the Y-basis vector of the render matrix
  (`sqrt(b² + d²)`), giving the correct pixel height regardless of text matrix scaling or rotation.
- Populates `char_cids` in the composite font branch alongside `char_advances`. One CID entry per
  Unicode character produced by that CID (multi-char ToUnicode mappings push the CID for each char).
- Mismatch guard now clears both `char_advances` and `char_cids` together to keep them consistent.

### `src/render/glyph_cache.rs`
- Added `GlyphGidKey = (String, u16, u32)` type alias for GID-indexed cache.
- Added `gid_bitmaps: HashMap<GlyphGidKey, GlyphBitmap>` to `GlyphCache`.
- Added `rasterize_by_gid(font_name, font_bytes, gid: u16, size_px)` — calls
  `fontdue::Font::rasterize_indexed(gid, size_px)` directly, bypassing the font's internal cmap
  table. Follows the same zero-extent / cache-first pattern as `rasterize()`.

### `src/render/page_renderer.rs`
- `draw_text_span()` now uses `span.font_size_px.abs()` instead of `span.font_size * self.scale`.
- Per-character rasterization: when `span.char_cids[i]` is present, calls
  `rasterize_by_gid(cid as u16, size_px)` on the embedded font instead of `rasterize(ch, size_px)`.
- Bundled fallback (DejaVu/Liberation) still uses Unicode-based `rasterize()` since it is not the
  embedded CFF font.

## Design Decisions

- **CID == GID assumption**: For the vast majority of PDFs (Identity-H encoding), the CID value is
  the direct glyph index. Parsing CIDToGIDMap streams is deferred as a known follow-up.
- **Separate `gid_bitmaps` cache**: Avoids key collisions between char-based and GID-based cache
  entries for the same font+size.
- **Bundled font stays Unicode**: Bundled fonts (DejaVu, Liberation) have standard cmaps and must be
  looked up by Unicode char, not by the PDF's CID which belongs to the embedded font's index space.
- **`font_size_px` in TextSpan**: Moving size computation to the interpreter (where the full CTM is
  available) is more accurate and removes a fragile per-renderer scale field.
- **ONLYOFFICE reference**: `RendererOutputDev.cpp` uses `getCIDToGID(cid)` and passes GID directly
  to FreeType — this confirms the pattern.

## Test Coverage

- 254 existing tests pass, covering text state, interpreter, display list, text extraction, parsing,
  rendering, and writer modules.
- No new dedicated tests added for `rasterize_by_gid` since it is structurally identical to
  `rasterize()` (same font fixture requirements); existing `test_rasterize_letter_a` covers the
  font-parsing path.

## Known Limitations / Follow-up

- **CIDToGIDMap streams not parsed**: PDFs with a non-Identity CIDToGIDMap (stream where
  GID = stream[2*CID] | stream[2*CID+1]) will still render incorrectly. Affects rare PDFs with
  remapped CID→GID tables.
- **Vertical fonts (WMode=1)**: Not handled; vertical advance (`advance_y`) is ignored.
- **Type1/Type3 fonts**: `rasterize_by_gid` is only useful for CFF/TTF. Type1 and Type3 fonts are
  not composite fonts in the PDF sense, so `char_cids` will be empty for them.
