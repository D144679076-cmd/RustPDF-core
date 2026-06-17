# form-flatten — Implementation Report

**Date:** 2026-06-17
**Scope:** `src/forms/filler.rs` — form flattening (phase 3)

## What Was Implemented

- `flatten_form_fields(editor, page_index)` — flattens Widget annotations on one page
- `flatten_all_form_fields(editor)` — calls above for every page
- Private helpers: `extract_rect`, `escape_pdf_string`, `register_xobject_in_page`, `remove_fields_from_acroform`
- `src/forms/mod.rs` — re-exports both public fns
- `src/wasm/editor.rs` — `WasmEditor::flatten_form_fields` and `WasmEditor::flatten_all_form_fields`

## Design Decisions

- **AP/N preferred over synthesis.** When a widget has `/AP/N`, it is embedded as a Form XObject via the `Do` operator with a `cm` transform derived from `/Rect`. Synthesis (text/checkbox operators) is used only as fallback. Real appearance streams are pixel-perfect and viewer-generated; synthesis handles simple cases only.
- **License gate via `#[cfg(feature = "crypto")]`.** Matches the existing pattern in `set_text_field` etc. — gate only compiles when crypto feature is present, keeping the free build functional.
- **`shift_remove` not `remove` for PdfDict.** `PdfDict` is an `IndexMap`; `remove` is deprecated in favour of order-preserving `shift_remove`.
- **AcroForm /Fields cleanup.** After flattening, widget object IDs are filtered out of `/AcroForm/Fields` so the document is structurally clean (viewers won't try to display non-existent fields).
- **Content stream appended, not replaced.** New flattened operators go into a fresh compressed stream appended to `/Contents`. This avoids rewriting the original content stream and works for both single-reference and array `/Contents`.
- **Empty pages return early.** If `/Annots` is absent or contains no Widgets, the function returns `Ok(())` immediately with no mutations.

## Test Coverage

| Test | What it covers |
|------|---------------|
| `flatten_form_fields_removes_widget_annots` | Widget removed from /Annots after flatten (happy-path) |
| `flatten_form_fields_appends_content_stream` | /Contents becomes array with ≥2 entries |
| `flatten_all_form_fields_produces_parseable_pdf` | Output round-trips through parser (structural validity) |
| `flatten_page_with_no_annots_is_noop` | No /Annots → early return, no error |

All tests use a synthetic in-memory PDF built with `PdfWriter` (no fixture dependency beyond existing suite).

## Known Limitations / Follow-up

- Synthesis fallback renders `/Helv` (Helvetica) for text fields — font must be available in viewer's standard fonts. Non-latin values may render as tofu.
- Checkbox synthesis draws a simple three-point checkmark; does not replicate the viewer's symbol choice (✓ vs ✗ vs filled square).
- Radio buttons (`/Btn` with `/Ff` Pushbutton bit clear) are handled by the generic `"Btn"` branch — multiple-state appearance not synthesised.
- Indirect `/Annots` reference is resolved once; heavily nested indirect chains are not followed recursively (not valid PDF per spec, but encountered in some generators).
- `/AP/D` (down) and `/AP/R` (rollover) appearances are ignored — only `/AP/N` (normal) is embedded.
