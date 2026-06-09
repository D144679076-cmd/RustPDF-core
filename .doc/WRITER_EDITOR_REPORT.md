# PDF Writer & Editor — Implementation Report

**Date:** 2026-05-23  
**Crate:** `pdf-core`  
**Features added:** `writer`, `forms`

---

## What Was Built

Phases G (Writer), H (Editor), and I (Forms) of the PDF construction plan — translating the ONLYOFFICE CPdfWriter / CPdfEditor architecture into idiomatic Rust.

---

## Deliverables Summary

| Category | Files | Lines |
|----------|-------|-------|
| Writer (`src/writer/`) | 9 | 2,360 |
| Editor (`src/editor/`) | 5 | 1,349 |
| Forms (`src/forms/`) | 3 | 388 |
| Integration tests (`tests/write_edit.rs`) | 1 | ~305 |
| **Total (new)** | **18** | **~3,897** |

### Test counts

| Suite | Tests |
|-------|-------|
| Unit tests (lib) | 280 |
| Integration tests (`write_edit.rs`) | 13 |
| Total | **308** |

All pass. Zero clippy warnings. WASM builds clean.

---

## Architecture

### Core Invariant

Original file bytes are **never modified**. All changes are appended as an incremental update section: new/modified objects at the end, followed by a new XRef table that shadows originals via the `/Prev` chain.

### Module Map

```
src/writer/
  serializer.rs    PdfObject → bytes (name escaping, real formatting, hex/literal strings)
  streams.rs       FlateDecode encode; raw/flate PdfStream constructors
  xref.rs          20-byte XRef entries, subsection grouping, trailer dict, startxref
  document.rs      PdfWriter — object pool, ID allocation, full serialize_all()
  content_builder.rs  ContentBuilder — all PDF drawing operators (q/Q, rg/RG, re, BT/ET, Tj/TJ, …)
  font.rs          Standard 14 Type1 dicts; TrueType embedding with FontDescriptor
  image.rs         FlateDecode and DCTDecode Image XObjects
  page.rs          PageBuilder — assembles Resources dict, Contents array, page dict
  mod.rs           Re-exports

src/editor/
  document_editor.rs  PdfEditor — copy-on-write bridge over PdfDocument + PdfWriter
  page_editor.rs      add_blank_page / delete_page / ContentLayer (begin_edit_page + commit)
  annotation.rs       AnnotationBuilder + add/delete_annotation
  metadata_editor.rs  set_metadata via MetadataFields struct
  mod.rs              Re-exports

src/forms/
  acroform.rs      AcroFormBuilder; build_text_field; build_checkbox (with On/Off appearance streams)
  appearance.rs    Highlight, text-note, checkbox appearance stream generators
  mod.rs           Re-exports
```

---

## Key Design Decisions

### Copy-on-Write Object Resolution

`PdfEditor::get_object(id)` checks the writer pool first, then falls back to the original `PdfDocument`. This means:
- `replace_object(id, new_obj)` queues a new version under the same ID — it shadows the original on the next read.
- `add_object(obj)` assigns `next_id` (always `> max_existing_id`) — no collision with the original file.

### Incremental Update Structure

`save_append(original_bytes)`:
1. Serializes all pending writer objects with absolute byte offsets (base = `original_bytes.len()`).
2. Writes an XRef section covering only the changed/new IDs.
3. Builds a trailer with `/Prev = original_xref_offset` to chain back to the original XRef.
4. Returns `concat(original_bytes, new_section)`.

A conforming PDF reader finds the new `startxref`, loads the incremental XRef, and uses `/Prev` for all IDs not overridden. Object 4 at the new offset wins over object 4 at the original offset.

### save_new vs save_append

| Operation | Mode |
|-----------|------|
| Add/remove/reorder pages | `WriteAppend` |
| Add text overlay to page | `WriteAppend` |
| Add annotation | `WriteAppend` |
| Edit metadata | `WriteAppend` |
| Redaction | `WriteNew` (old content stream must be replaced, not overlaid) |
| Merge two PDFs | `WriteNew` (second doc's IDs need renumbering) |

### Content Layer Pattern

Drawing new content onto an existing page uses `begin_edit_page` + `ContentLayer::commit`:
1. `begin_edit_page` reads the page's existing `/Contents` references.
2. The caller emits operators into `ContentBuilder`.
3. `commit` compresses the new stream, writes it as a new object, and updates the page dict's `/Contents` to `[...original_refs, new_stream_ref]`.

New content paints **on top of** existing content because PDF renders arrays in order.

### String Serialization

`serialize_string` auto-selects literal `(...)` vs hex `<...>` strings:
- Literal if all bytes are printable ASCII and the paren nesting is balanced.
- Hex for binary content (font data, compressed bytes, non-ASCII strings).

### Real Number Formatting

`format_real(f)` formats to 6 decimal places then strips trailing zeros and a trailing decimal point — `1.000000` → `1`, `3.141590` → `3.14159`, `-0.500000` → `-0.5`. This keeps file sizes compact.

### MetadataFields Struct

`set_metadata` takes a `&MetadataFields` struct rather than 8 positional arguments, satisfying the `clippy::too_many_arguments` lint while keeping the API ergonomic.

---

## API Usage Examples

### Create a PDF from scratch

```rust
use pdf_core::writer::{ContentBuilder, PageBuilder, PdfWriter, write_standard_font};

let mut writer = PdfWriter::new();
let font_id = write_standard_font("Helvetica", &mut writer)?;

let mut content = ContentBuilder::new();
content.begin_text().set_font("F1", 14.0).move_text_pos(72.0, 720.0)
       .show_text_str("Hello PDF").end_text();

let pages_id = writer.reserve_id();
let mut page = PageBuilder::new(595.0, 842.0);
page.add_font("F1", font_id).add_content(content.build());
let page_id = page.build(pages_id, &mut writer)?;

// ... assemble pages node + catalog, then:
let bytes = writer.serialize_all(cat_id, None, None)?;
```

### Add an annotation to an existing PDF

```rust
use pdf_core::editor::{PdfEditor, add_annotation, add_blank_page, AnnotationBuilder, AnnotationType};

let original = std::fs::read("doc.pdf")?;
let mut editor = PdfEditor::open(original.clone())?;

let annot = AnnotationBuilder::new(
    AnnotationType::Highlight {
        color: [1.0, 1.0, 0.0],
        quad_points: vec![10.0, 700.0, 200.0, 700.0, 10.0, 712.0, 200.0, 712.0],
    },
    [10.0, 700.0, 200.0, 712.0],
).author("Alice");

add_annotation(&mut editor, 0, annot)?;
let result = editor.save_append(&original)?;
```

### Draw on an existing page

```rust
use pdf_core::editor::{PdfEditor, begin_edit_page};

let mut editor = PdfEditor::open(original.clone())?;
let mut layer = begin_edit_page(&editor, 0)?;
layer.builder.save().set_fill_rgb(1.0, 0.0, 0.0).rect(10.0, 10.0, 100.0, 50.0).fill().restore();
layer.commit(&mut editor)?;
let result = editor.save_append(&original)?;
```

---

## Verification

```
cargo fmt --check          → OK
cargo clippy --features writer,forms -- -D warnings  → OK (0 warnings)
cargo test --features writer,forms    → 308 tests, 0 failures
cargo build --target wasm32-unknown-unknown --features writer  → OK
```

---

## What Is Not Yet Implemented

| Item | Notes |
|------|-------|
| Font subsetting | Full TTF binary embedded; glyph subsetting deferred |
| `save_new()` full rewrite | Skeleton present; full object graph traversal not wired |
| Redaction | Needs `save_new()` complete |
| PDF merge | Needs ID renumbering pass |
| XObject form streams | Appearance streams use raw bytes; full XObject wrapper deferred |
| Linearization | Not planned |
