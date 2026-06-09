# annotation-flatten — Implementation Report

**Date:** 2026-06-05
**Scope:** Annotation flatten (`flatten_annotations` / `flatten_all_annotations`)

## What Was Implemented

- **`src/editor/annotation.rs`**
  - `flatten_annotations(editor, page_index)` — burns all annotations on one page into its content stream and removes `/Annots`.
  - `flatten_all_annotations(editor)` — calls the above for every page.
  - `flatten_one_annotation(cb, dict, subtype)` — per-subtype drawing into a `ContentBuilder`: Highlight (filled rect), StrikeOut (mid-line stroke), Underline (bottom-line stroke), FreeText (positioned text), Ink (polyline strokes).
  - `parse_rect`, `parse_color`, `parse_quad_points`, `pdf_num` — shared dict-parsing helpers.

- **`src/editor/mod.rs`** — `flatten_annotations` and `flatten_all_annotations` added to the public re-export list.

- **`src/wasm/editor.rs`** — `WasmEditor::flatten_annotations(page_index)` and `WasmEditor::flatten_all_annotations()` wasm_bindgen methods added alongside the existing `apply_redactions`.

- **`tests/write_edit.rs`**
  - `flatten_highlight_removes_annots_key`
  - `flatten_produces_parseable_pdf`
  - `flatten_all_on_page_with_no_annots_is_noop`
  - `flatten_ink_annotation_produces_parseable_pdf`

## Design Decisions

- **Append-only content strategy**: Rather than rewriting the entire content stream (as `apply_redactions` does), flatten appends a new FlateDecode stream to `/Contents`. This is non-destructive — the original content is preserved — and avoids the cost of parsing and re-serialising existing operators.
- **`/Annots` removal via `HashMap::remove`**: After building the drawing stream, the key is simply removed from the cloned page dict. An empty array is never left behind.
- **`/Annots` reference resolution**: Inline arrays and indirect-reference arrays are both handled, matching the pattern already used in `add_annotation`.
- **`parse_quad_points` yields `[f64; 8]` groups**: The PDF spec stores quad points as a flat array; chunking into fixed-size 8-tuples makes the per-quad indexing explicit and avoids bounds-check noise.
- **License gate omitted**: The `license` module referenced in the phase plan does not yet exist. The tier gate can be added once that module lands.
- **FreeText uses `/Helv` name**: FreeText annotations reference the font by the abbreviated AcroForm name. Pages that do not include a `/Helv` resource will not render the text, but the PDF remains structurally valid. Full FreeText rendering is a known limitation.

## Test Coverage

| Test | Coverage |
|------|---------|
| `flatten_highlight_removes_annots_key` | Happy path: `/Annots` absent after flatten; CoW view checked before save |
| `flatten_produces_parseable_pdf` | Output bytes are valid PDF (StrikeOut annotation) |
| `flatten_all_on_page_with_no_annots_is_noop` | No annotations → no error, page count unchanged |
| `flatten_ink_annotation_produces_parseable_pdf` | Ink strokes encoded in content stream; PDF parses cleanly |

## Known Limitations / Follow-up

- **Transparency**: Highlight annotations ideally use 30% opacity. ContentBuilder has no ExtGState support yet; the flatten currently writes a solid fill. Add `apply_gs` + resource injection once ExtGState is wired up.
- **FreeText font resource**: The appended stream references `/Helv` without injecting a font resource entry into the page. A follow-up should inject a standard font resource when the name is not already present.
- **Tier gate**: Add `crate::license::require(Tier::Pro, "flatten_annotations")?` once the `license` module is implemented (phase1-licensing.md).
- **Appearance streams (AP)**: Complex annotations with `/AP` streams are not handled; flattening their appearance XObject would produce pixel-perfect output and is deferred.
