# Renderer Font Resolution — Implementation Report

**Date:** 2026-05-26
**Scope:** `src/render/page_renderer.rs`

## What Was Implemented

- **`get_base_font_name(resource_key: &str) -> Option<String>`** — new helper on `PageRenderer`.
  Looks up `resource_key` (e.g. `"F1"`) in `/Resources/Font`, resolves the font object, and returns the `/BaseFont` name (e.g. `"Helvetica-Bold"`).

- **Fixed fallback in `draw_text_span`** — when `get_ttf_bytes` returns `None` (no embedded font), the fallback now:
  1. Calls `get_base_font_name` to translate the resource key to the real font name.
  2. Passes the real name to `normalize_font_name` + `font_resolver.resolve`.
  3. Logs `[renderer] font key=... base_font=...` at DEBUG level.
  4. Logs `[renderer] no font data for key=...` at WARN level when both paths fail.

- **Two clippy fixes in `edit_session.rs`**:
  - `sort_by(|a,b| b.1.cmp(&a.1))` → `sort_by_key(|b| Reverse(b.1))`
  - `.map(|o| std::mem::discriminant(o))` → `.map(std::mem::discriminant)`

## Root Cause

`font_resolver.resolve("F1", false, false)` always fails because `"F1"` is a PDF resource key, not a font family name. `normalize_font_name("F1")` returns `("f1", false, false)` — no bundled font matches. The fix resolves the real `/BaseFont` name first.

## Design Decisions

- **Read `/BaseFont` not `/FontDescriptor/FontName`**: `/BaseFont` is always a Name in the font dict and is present even for standard 14 fonts. `/FontDescriptor` is optional and absent for standard fonts — using it would break simple cases.
- **Fallback only**: Path 1 (`get_ttf_bytes`) is unchanged. Embedded fonts continue to work as before; only the bundled-font fallback path was broken.
- **`unwrap_or_else` not `?`**: `get_base_font_name` returns `Option`, not `Result`. On failure the original `span.font_name` is used as before — no regression vs. the old behavior.

## Test Coverage

No new unit tests added — the fix is at the font-resolver boundary which requires a live `PdfDocument` and embedded test PDFs. The change is verified by:
- `cargo test` — 254 passed, no regressions
- `cargo clippy --features wasm-render -- -D warnings` — 0 warnings
- Browser test with a real word-processor PDF: console shows `[renderer] font key="F1" base_font="Helvetica"` and text renders as characters

## Known Limitations / Follow-up

1. **Standard 14 font metrics** — Even if `font_resolver` returns TTF bytes for `"Helvetica"`, glyph advance widths may differ from the PDF's `/Widths` array. For accurate layout, `draw_text_span` should use the PDF's width table, not the TrueType advance metrics.
2. **CID fonts** — `/BaseFont` for CIDFont subsets is a mangled name like `"ABCDEF+ArialMT"`. `normalize_font_name` strips the prefix, but resolver matching may still fail for uncommon variants.
3. **Type3 fonts** — These have no `/BaseFont` and no TTF equivalent; `get_base_font_name` returns `None` and placeholder rendering is the correct fallback.
