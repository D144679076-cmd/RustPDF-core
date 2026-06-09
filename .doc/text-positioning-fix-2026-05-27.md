# Text Positioning Diagnostic — Implementation Report

**Date:** 2026-05-27
**Scope:** text character spacing & position diagnostics

## What Was Implemented

- Added `log::debug!("[text-span] ...")` in `src/content/interpreter.rs:show_text()` — emits per-span: font name, size_pt, size_px, position (x,y), is_composite, char count, first-5 char_advances (or `[fontdue-fallback]` when empty).
- Added `log::debug!("[draw-span] ...")` in `src/render/page_renderer.rs:draw_text_span()` — emits per-span: font name, size_px, pixel position, char count, whether PDF advances are available, embedded/bundled font presence.
- Added `log::debug!("[draw-char-0] ...")` for the first character of each span — actual advance used (PDF or fontdue), pen position after.

## Design Decisions

- **No behavior change**: Only logging added. All fixes to the actual algorithms were deferred after analysis showed no algorithmic bugs.
- **Why logging before fixes**: Extensive analysis of the Rust code vs C++ ONLYOFFICE reference (Gfx.cc) found NO algorithmic divergence in TJ sign, /DW scaling, or Td/TD leading. The Explore agent analysis that identified these as bugs was incorrect. Before making blind changes, the logging will reveal whether the issue is in char_advance values, position coordinates, or glyph rasterization.

## Analysis of Proposed Fixes (Not Implemented)

| Fix | Status | Reason |
|-----|--------|--------|
| TJ sign flip (`-(displacement/1000)`) | **NOT a bug** | C++ also negates: `textShift(-value * 0.001 * ...)`. Both move pen left for positive TJ, right for negative. Removing negation would break TJ. |
| /DW × 0.001 at parse time | **NOT a bug** | Rust stores raw value (e.g., 1000.0), divides by 1000 at use (line 597). C++ stores pre-scaled (1.0) and multiplies by font_size directly. End result identical. Adding × 0.001 would make widths 1000× too small. |
| TD leading | **Already correct** | `interpreter.rs:435`: `self.text.leading = -ty` before `move_text_position`. Matches C++ `setLeading(-ty)` followed by position update. |
| Form XObject CTM order | **Already correct** | `m.concat(&ctm)` applies m first (XObject space → user space), then ctm (user → device). Matches PDF spec and C++. |

## How to Use Logging

```bash
RUST_LOG=pdf_core=debug cargo test -- --nocapture 2>&1 | grep '\[text-span\]\|\[draw-span\]\|\[draw-char-0\]'
```

**What to look for:**
- Wide spacing: `char_advances` values should be ~`size_px * 0.3` to `size_px * 0.8` per character. Values equal to `size_px` (1.0× full-em) indicate default width fallback.
- Overlapping text: `pos=(x,y)` values should differ between spans on separate lines. Same y for all spans indicates a Td/TD parsing failure or wrong line-matrix initialization.
- `[fontdue-fallback]` tag: char_advances mismatch fired — fontdue advance used instead.
- `has_pdf_advances=false`: char_advances cleared due to mismatch.

## Test Coverage

No new tests (logging-only change). Existing suite passes.

## Known Limitations / Follow-up

- Root cause of visual bugs still unknown — logging is the next diagnostic step.
- Potential areas to investigate once log data is available:
  1. `resolve_font_info` failure when /Font is an indirect reference (currently only handles inline dict)
  2. `char_advances` mismatch frequency for composite fonts in Word/PPT PDFs
  3. Whether `font_widths = None` path occurs in practice (fallback to 500 may be too narrow)
