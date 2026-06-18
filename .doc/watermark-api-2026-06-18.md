# Watermark API — Implementation Report

**Date:** 2026-06-18
**Scope:** editor::watermark (Phase 3)

## What Was Implemented

### Types (`src/editor/watermark.rs`)
- `TextWatermark` — text, font_size, color [r,g,b], opacity, angle_degrees, repeat, tile_spacing
- `ImageWatermark` — pixels, width, height, channels (1/3/4), rect [x1,y1,x2,y2], opacity

### Public API functions
- `add_text_watermark(editor, page_index, wm)` — single-page text watermark
- `add_watermark_all_pages(editor, wm)` — text watermark on every page
- `add_image_watermark(editor, page_index, wm)` — single-page image watermark

### Module wiring (`src/editor/mod.rs`)
- Added `pub mod watermark` declaration and re-exports for all public items

### WASM bindings (`src/wasm/editor.rs`)
- `WasmEditor::add_text_watermark(page_index, options_json)` — JSON keys: text, font_size, color, opacity, angle_degrees, repeat, tile_spacing
- `WasmEditor::add_watermark_all_pages(options_json)`
- `WasmEditor::add_image_watermark(page_index, pixels, width, height, channels, rect_x1/y1/x2/y2, opacity)`
- `parse_text_watermark_json()` helper (reuses existing json_*_field parsers)

### Supporting fix (`src/editor/content_draw.rs`)
- Changed `register_resource_entry` from private to `pub(crate)` so watermark.rs can use it

## Design Decisions

- **`build_text_watermark_content` writes to `&mut ContentBuilder`** rather than returning `Vec<u8>`: avoids the need for a `buf_extend` method on `ContentBuilder` and removes an unnecessary heap allocation.
- **Opacity via `/ExtGState /ca`**: proper PDF opacity model; both fill alpha (`/ca`) and stroke alpha (`/CA`) set to keep behavior consistent for mixed-mode viewers.
- **License gate behind `#[cfg(feature = "crypto")]`**: matches pattern used across the codebase; without crypto feature the check is compiled out (useful for test builds).
- **Image watermark uses flat pixel args in WASM** rather than a JSON blob: raw byte arrays (`&[u8]`) cannot be represented in JSON; passing pixels separately as a `Uint8Array`/`&[u8]` is the idiomatic wasm_bindgen pattern.
- **`register_resource_entry` made `pub(crate)`**: the function already encapsulated the full resource-inheritance logic (resolving indirect `/Resources`, copying inherited values, merging sub-dicts). Duplicating it in `watermark.rs` would violate DRY.

## Test Coverage

All in `src/editor/watermark.rs` under `#[cfg(test)]`:

| Test | What it covers |
|------|----------------|
| `add_text_watermark_produces_parseable_pdf` | happy-path: output parses as valid PDF |
| `add_text_watermark_registers_font_and_gstate` | verifies Font and ExtGState resources added to page |
| `add_watermark_all_pages_applies_to_each_page` | all pages survive; page count unchanged |
| `add_image_watermark_produces_parseable_pdf` | happy-path image watermark |
| `add_image_watermark_rejects_bad_channel_count` | error-path: channels=2 returns Err |
| `add_text_watermark_repeat_produces_parseable_pdf` | tiled (repeat=true) output parses |

Run: `cargo test --features writer -- watermark` → 6 passed.

## Known Limitations / Follow-up

- **No opacity on image watermarks in older viewers**: `/ExtGState /ca` is PDF 1.4+. Viewers that don't support transparency will render the image at full opacity.
- **Text width estimate is approximate**: `0.55 × font_size × char_count` — centering may be off for non-ASCII or wide characters. A proper advance-width lookup via `font_metrics_for` would be more accurate but requires the render feature.
- **No `add_image_watermark_all_pages`**: not in the phase spec; easy to add if needed.
- **WASM wasm32 full build blocked by pre-existing rquickjs-sys C/stdio issue**: unrelated to watermark; `cargo check --target wasm32-unknown-unknown --features wasm` passes cleanly for Rust code.
