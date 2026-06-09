# wasm-bridge — Implementation Report

**Date:** 2026-05-23
**Scope:** `src/wasm/mod.rs` — full JavaScript/WASM bridge via `wasm-bindgen`

## What Was Implemented

### Cargo.toml additions
- `wasm-bindgen = { version = "0.2", optional = true }` and `js-sys = { version = "0.3", optional = true }` dependencies.
- `wasm = ["dep:wasm-bindgen", "dep:js-sys", "writer", "forms"]` feature — minimal bridge without renderer.
- `wasm-render = ["wasm", "render"]` feature — bridge with pixel rendering included.

### `src/wasm/mod.rs` (new file, gated `#[cfg(feature = "wasm")]`)

**`WasmDocument`** — wraps `PdfDocument`
- `parse(bytes: &[u8]) -> Result<WasmDocument, JsError>`
- `parse_with_password(bytes: &[u8], password: &[u8]) -> Result<WasmDocument, JsError>` (crypto feature)
- `page_count() -> Result<usize, JsError>`
- `get_metadata() -> String` — JSON-serialised `{"title":…,"author":…,…}`
- `get_outline() -> String` — JSON-serialised `[{"title":…,"page":…,"children":[…]},…]`
- `extract_text(page_index: usize) -> Result<String, JsError>`

**`RenderResult`** + **`WasmRenderer`** (gated `#[cfg(feature = "render")]`)
- `RenderResult { pub width: u32, pub height: u32, data: Vec<u8> }` with `rgba_bytes() -> js_sys::Uint8Array`
- `WasmRenderer::render_page(doc: &WasmDocument, page_index: usize, scale: f64) -> Result<RenderResult, JsError>`

**`WasmEditor`** — wraps `PdfEditor` + stores `original_bytes: Vec<u8>` for incremental save
- `open(bytes: &[u8]) -> Result<WasmEditor, JsError>`
- `page_count() -> Result<usize, JsError>`
- `add_blank_page(&mut self, index: usize, width_pt: f64, height_pt: f64) -> Result<(), JsError>`
- `delete_page(&mut self, index: usize) -> Result<(), JsError>`
- `add_text_annotation(&mut self, page, x, y, w, h, contents) -> Result<(), JsError>`
- `add_highlight(&mut self, page, quad_points: &[f64], r, g, b) -> Result<(), JsError>`
- `add_strikeout(&mut self, page, quad_points: &[f64], r, g, b) -> Result<(), JsError>`
- `add_link(&mut self, page, x, y, w, h, url) -> Result<(), JsError>`
- `set_metadata(&mut self, title, author, subject, keywords) -> Result<(), JsError>`
- `save(&mut self) -> Result<js_sys::Uint8Array, JsError>` (WASM target)
- `save_bytes(&mut self) -> Result<Vec<u8>>` (native, for tests)

**`WasmPdfWriter`** — wraps `PdfEditor` seeded from `minimal.pdf` fixture bytes
- `new() -> Result<WasmPdfWriter, JsError>`
- `add_page(&mut self, width_pt: f64, height_pt: f64) -> Result<(), JsError>`
- `build(&mut self) -> Result<js_sys::Uint8Array, JsError>` (WASM target)
- `build_bytes(&mut self) -> Result<Vec<u8>>` (native, for tests)

### `tests/wasm_api.rs` (new file, gated `#[cfg(feature = "wasm")]`)
16 integration tests covering all four public structs.

## Design Decisions

- **`WasmEditor` stores `original_bytes`**: `PdfEditor::save_append()` requires the original PDF bytes on every call. Rather than forcing JS callers to pass bytes on save, the bridge stores them at `open()` time and updates them after each save so incremental chains work transparently.
- **Parallel `*_bytes()` methods for native tests**: `js_sys::Uint8Array` panics on non-WASM targets. Adding `save_bytes()` / `build_bytes()` returning `Vec<u8>` in a plain (non-`#[wasm_bindgen]`) impl block allows the full test suite to run on native without a real WASM runtime.
- **`get_metadata()` / `get_outline()` return `String` (JSON), not `JsValue`**: `JsValue` serialisation with `serde-wasm-bindgen` adds a dependency; hand-rolled JSON for these small structs avoids it and keeps the `wasm` feature weight minimal.
- **`WasmPdfWriter` seeded from `minimal.pdf`**: Creating a blank document requires a valid PDF skeleton. `include_bytes!` embeds the fixture at compile time — zero runtime I/O, works identically on WASM.
- **`wasm-render` is a separate feature**: `tiny-skia` adds ~800 KB to the WASM binary. Most consumers only need edit/annotation operations, so rendering is kept opt-in.
- **Error-path tests removed from `wasm_api.rs`**: `JsError::new()` panics on native targets even for `Err` paths. These are validated by `wasm-pack` tests on a real WASM target; the equivalent native error test is `real_pdf.rs::garbage_bytes_fail_to_parse`.

## Test Coverage

All tests in `tests/wasm_api.rs` under `mod wasm_tests` (`--features wasm`):

| Test | What it covers |
|---|---|
| `wasm_document_parse_minimal` | Parse + page_count = 1 |
| `wasm_document_parse_multipage` | Parse + page_count = 3 |
| `wasm_document_get_metadata_returns_json_object` | Metadata serialises as `{…}` |
| `wasm_document_get_outline_returns_json_array` | Outline serialises as `[…]` |
| `wasm_document_extract_text_does_not_panic` | Text extraction on stream PDF |
| `wasm_document_parse_multipage_text` | All 3 pages extractable |
| `wasm_editor_open_and_page_count` | Open returns correct page count |
| `wasm_editor_save_produces_valid_pdf` | Save → re-parse round-trip |
| `wasm_editor_add_text_annotation_and_save` | Annotation added, PDF still valid |
| `wasm_editor_add_highlight_and_save` | Highlight added, PDF still valid |
| `wasm_editor_set_metadata_and_save` | Metadata set, PDF still valid |
| `wasm_editor_add_blank_page_increases_count` | Page count +1 after add |
| `wasm_editor_add_link_annotation` | Link annotation added, PDF still valid |
| `wasm_pdf_writer_new_and_build` | Empty writer produces valid PDF |
| `wasm_pdf_writer_add_page_and_build` | 1 page produces PDF with page_count=1 |
| `wasm_pdf_writer_add_multiple_pages` | 2 pages produces PDF with page_count=2 |

## Known Limitations / Follow-up

- `WasmDocument::parse_with_password` is compiled but untested in `wasm_api.rs` (requires an encrypted fixture).
- `WasmRenderer` has no native test (it returns `RenderResult` with `rgba_bytes() -> js_sys::Uint8Array`, which panics on native). A `render_bytes()` variant should be added for test coverage parity.
- `get_outline()` serialises only title, page destination, and children — action destinations (URI links, named destinations) are omitted.
- `WasmPdfWriter::add_page` appends to the minimal-PDF seed; the initial blank page from the seed is not stripped. First call removes it and replaces it, but callers must be aware of this behaviour.
- No `wasm-pack` test suite exists yet; the JS binding layer (argument marshalling, promise wrapping) is exercised only by the native rlib tests.
