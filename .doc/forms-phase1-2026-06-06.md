# Forms Phase 1 — Implementation Report

**Date:** 2026-06-06
**Scope:** Form filling (read + write) — Phase 1 of the interactive AcroForm roadmap

## What Was Implemented

### New files
- `src/forms/reader.rs` — `FieldType` enum, `FormField` struct, `read_form_fields(doc)`, recursive `collect_fields`, UTF-16BE/UTF-8 string normaliser, page-ref table builder.
- `src/forms/filler.rs` — `set_text_field`, `set_checkbox`, `set_combo_or_list`; shared helpers `get_field_dict`, `add_form_xobject`, `set_normal_appearance`.

### Modified files
- `src/forms/appearance.rs` — added `text_field_appearance(value, rect, max_len)` and `escape_pdf_string` helper.
- `src/forms/mod.rs` — declared `filler` and `reader` modules; re-exported all public symbols.
- `src/wasm/editor.rs` — added `get_form_fields() -> String`, `set_field_value(name, value)`, `get_field_value(name)` to `WasmEditor`.
- `src/editor/edit_session.rs` — added `#[allow(dead_code)]` to pre-existing unused function to keep `-D warnings` clean.
- `tests/write_edit.rs` — three new integration tests in `forms_tests`.
- `tests/fixtures/form.pdf` — new hand-crafted minimal AcroForm fixture (text field + checkbox).

## Design Decisions

**Page-ref table via `Catalog`**: `read_form_fields` needs a page-index lookup to resolve each field's `/P` pointer. Rather than duplicating the page-tree traversal, it triggers `Catalog::get_page_dict(doc, 0)` which lazily populates `PdfDocument`'s internal `page_refs` cache as a side-effect, then reads back all refs with `cached_page_ref`. This avoids touching the private `collect_page_refs_iterative` function in `catalog.rs`.

**`checkbox_appearance` signature**: The existing `checkbox_appearance(rect, checked)` in `appearance.rs` already takes `rect` (unlike the phase spec's pseudocode which omitted it). `filler.rs` passes `field.rect` directly to match the actual signature.

**No license guard**: The codebase has no `license` module. The "Tier gate: Pro" note in the phase spec is aspirational; the guard calls in the pseudocode were omitted. They can be added later without touching the public API.

**`escape_pdf_string` visibility**: Made `fn` (private to module), since no external caller needs it.

**Form XObject compression**: Appearance streams are FlateDecode-compressed via `make_flate_stream` (same as existing `build_checkbox`). The BBox is always `[0 0 w h]` in field-local space.

**`set_combo_or_list` — no appearance**: The phase spec and existing `build_combo_field` do not write an appearance stream for combo/list fields (they rely on the viewer's built-in rendering). This is unchanged.

## Test Coverage

### Unit tests (inside modules)
| Test | What it covers |
|---|---|
| `reader::read_form_fields_no_acroform_returns_empty` | No `/AcroForm` → empty result, happy-path |
| `reader::refs_equal_*` | Reference comparison helper |
| `reader::pdf_string_to_utf8_plain_ascii` | Latin-1 / ASCII string decoding |
| `reader::pdf_string_to_utf8_utf16be` | UTF-16BE string decoding |
| `filler::set_text_field_updates_v_and_adds_ap` | `/V` written, `/AP` entry created |
| `filler::set_checkbox_checked_sets_yes_state` | `/V = /Yes`, `/AS = /Yes` |
| `filler::set_checkbox_unchecked_sets_off_state` | `/V = /Off` |
| `filler::set_combo_or_list_updates_v_and_i` | `/V` + `/I` index updated |
| `appearance::text_field_appearance_contains_value` | Content stream includes value and BT/ET |
| `appearance::text_field_appearance_truncates_to_max_len` | `MaxLen` truncation works |
| `appearance::escape_pdf_string_roundtrip` | Special chars escaped correctly |

### Integration tests (`tests/write_edit.rs`)
| Test | What it covers |
|---|---|
| `form_fields_read_from_fixture_pdf` | `read_form_fields` on real fixture; at least one text field |
| `form_set_text_field_round_trips` | Text value survives save + re-parse |
| `form_set_checkbox_round_trips` | Checkbox state survives save + re-parse |

## Known Limitations / Follow-up

- **Radio buttons**: `set_field_value` dispatches only Text/Checkbox/List/Combo. Radio buttons share a parent field with a `/Kids` array of widget annotations — updating them correctly requires propagating the `/V` on the parent. This can be added in a follow-up.
- **Multi-select lists**: `set_combo_or_list` only handles single-value selection (one `/I` index). Multi-select (`/Ff` bit 22) would require passing a `&[&str]` list.
- **Appearance font resources**: The generated text appearance stream references `/Helv` but does not inject a `/DR` (Default Resources) entry into `/AcroForm`. Some strict viewers may not display the text if `/Helv` is absent from the page's resource dict. A follow-up should wire the standard Helvetica alias into `/AcroForm /DR /Font`.
- **Inherited field attributes**: `collect_fields` reads `/FT` and `/Ff` only from the leaf dict. PDF allows these to be inherited from parent nodes. A future pass can walk the parent chain for missing keys.
