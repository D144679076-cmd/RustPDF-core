# Missing Annotation Types + Appearance Streams — Implementation Report

**Date:** 2026-06-14
**Scope:** Phase 2 — Missing Annotation Types + Appearance Streams

## What Was Implemented

### `src/editor/annotation.rs`
- **New `AnnotationType` variants:** `Stamp`, `Polygon`, `Polyline`, `FileAttachment`, `Caret`
- **`AnnotationBuilder`:** Added `prebuilt_ref: Option<u32>` field to carry a pre-built Filespec object ID into `build()` for `FileAttachment`
- **`AnnotationBuilder::build()`:** Added match arms for all 5 new types; `Polygon`/`Polyline` emit `/Vertices`, `/C`, `/BS`; `FileAttachment` wires `/FS` from `prebuilt_ref`; `Stamp` emits `/Name`; `Caret` emits `/Sy`
- **`build_embedded_file_stream()`:** New private helper — compresses file bytes with FlateDecode, writes an `EmbeddedFile` stream and a `Filespec` dictionary into the editor's object pool, returns the Filespec object ID
- **`color_array()` / `border_style_dict()`:** New private helpers for compact color and border style dict construction
- **`generate_ap_bytes()`:** New private function (gated on `forms` feature) — dispatches to `crate::forms::appearance::*` to produce an appearance content stream for each annotation type that has a visual representation
- **`add_annotation()`:** Now `mut`-takes the builder; pre-processes `FileAttachment` to build the embedded file before calling `build()`; after building the dict, generates and attaches an `/AP /N` form XObject stream (when `forms` feature is active)
- **`flatten_one_annotation()`:** Added `"Stamp"`, `"Polygon"` / `"PolyLine"`, `"Caret"`, `"FileAttachment"` arms with direct `ContentBuilder` drawing ops

### `src/forms/appearance.rs`
- **`stamp_appearance(name, rect, color)`** — bordered rectangle with centred label text
- **`freetext_appearance(text, rect, font_size, color)`** — single-line text using `/Helv`
- **`ink_appearance(ink_list, bbox, color)`** — multi-stroke polyline using `ContentBuilder`; offsets coordinates by `bbox` origin
- **`highlight_appearance_quad(quad_points, bbox, color)`** — fills one rect per quad, offset by `bbox` origin; used by `generate_ap_bytes` for Highlight
- **`polygon_appearance(vertices, rect, stroke_color, fill_color, line_width)`** — closed path with optional fill
- **`polyline_appearance(vertices, rect, stroke_color, line_width)`** — open path stroke
- **`caret_appearance(rect)`** — `^` chevron shape
- **`file_attachment_appearance(rect, icon_name)`** — simplified pin icon

### `src/forms/mod.rs`
- Re-exported all 8 new appearance functions

### `src/wasm/editor.rs`
- **`add_stamp(page_index, name, x, y, width, height, r, g, b)`** — WASM convenience wrapper
- **`add_file_attachment(page_index, file_bytes, filename, description, x, y, width, height)`** — WASM convenience wrapper with hardcoded `PushPin` icon

### `src/wasm/text_edit.rs`
- Added `#[allow(dead_code)]` to `render_metrics` field (field is used under `render` feature but clippy's cross-feature analysis flags it in the `wasm`-only build)

## Design Decisions

- **`prebuilt_ref` on builder vs. separate parameter to `build()`:** `build()` is `&self` / consuming `self` — passing an object ID via the builder field keeps `add_annotation`'s signature stable and avoids a public API change
- **`generate_ap_bytes` gated on `forms`:** The `annotation` module is in the `writer` feature; appearance generation depends on `ContentBuilder` helpers from `forms`. Gating avoids a circular or unwanted dependency and keeps the minimal `writer` build small
- **AP stream only added when missing:** The `add_annotation` call checks for pre-existing `/AP` (FileAttachment already has none; future callers won't accidentally overwrite a custom AP)
- **`flatten_one_annotation` uses page-space coordinates directly:** Flatten drawing emits in the page content stream, so coordinates are page-space — no BBox offset needed (unlike AP form XObjects which use local space)
- **`FileAttachment` AP stream:** Generates a simple pin icon regardless of `icon_name`; full per-icon rendering deferred to follow-up

## Test Coverage

All tests in `tests/write_edit.rs` (editor_tests module):

| Test | What it covers |
|---|---|
| `add_stamp_annotation_parseable` | Stamp dict has `/Subtype /Stamp`; saved PDF re-parses |
| `add_polygon_annotation_parseable` | Polygon dict has `/Subtype /Polygon`; saved PDF re-parses |
| `add_file_attachment_parseable` | FileAttachment dict has `/Subtype /FileAttachment` and `/FS`; saved PDF re-parses |
| `annotations_have_ap_streams` (forms_tests) | Highlight annotation has `/AP` dict after `add_annotation` |

All 662 tests pass; WASM build succeeds.

## Known Limitations / Follow-up

- **Per-icon FileAttachment rendering:** `file_attachment_appearance` ignores `icon_name`; "Graph", "Paperclip", "Tag" icons all render as the same pin shape
- **StrikeOut / Underline AP streams:** `generate_ap_bytes` returns `None` for these (visual rendering works through `flatten_one_annotation`; AP stream generation deferred)
- **Font resources in AP streams:** The stamp and free-text AP streams reference `/Helv` but do not inject it into the form XObject's `/Resources` dict — viewers relying on the form's own resources dict (rather than inheriting from the page) may not render text. This is the same limitation as the existing `text_field_appearance` path
- **Caret `Sy=P` paragraph symbol:** The appearance stream draws the same chevron regardless of symbol; proper paragraph mark rendering would require a glyph or Type3 font
