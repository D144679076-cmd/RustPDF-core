# Changelog

All notable changes to `pdf-core` are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- **Undo/redo** for the editor: `PdfEditor::checkpoint/undo/redo/can_undo/can_redo` (writer-pool snapshots) and the WASM surface `WasmEditor::undo/redo/can_undo/can_redo`; `text_edit_commit` auto-checkpoints (TD-6)
- `PdfWriter::generation()` — monotonic mutation counter; WASM text-edit caches now key on it instead of pool length, so in-place `set_object` replacements and undo/redo invalidate correctly (TD-5)
- `PdfDocument::is_signed()` + `WasmEditor::is_signed()` — detect AcroForm `/SigFlags`; `text_edit_enter` warns that editing a signed PDF invalidates its signature (TD-4)
- `content::operator::Operator` — typed operator vocabulary; the interpreter now dispatches on it, so the compiler enforces exhaustive operator handling (TD-3)
- `filters::apply_pipeline_cow()` — zero-copy `Cow` decode for unfiltered streams (TD-8)
- `RedactZone` struct and `apply_redactions()` for permanent, forensic-safe content removal (`writer` feature)
- `AnnotationType::Redact` variant for marking rectangular areas for redaction (ISO 32000-2 §12.5.6.23)
- `PdfDocument::all_object_ids()` — enumerate all xref object IDs, excluding the free head
- `serialize_operations()` in `content::operators` — round-trip `Vec<Operation>` back to content-stream bytes (`writer` feature)

### Changed
- **`PdfDict` is now an `IndexMap`** (was `HashMap`): dictionary key order is preserved through parse→serialize, keeping `/Filter`↔`/DecodeParms` pairing and signed-dict byte layout stable. `serialize_dict` emits insertion order instead of sorting alphabetically (TD-1)
- **`PdfDocument` is now `Send + Sync`**: the four interior caches use `parking_lot::RwLock` instead of `RefCell` (single-threaded shim on WASM, real lock on native), enabling parallel tile rendering; `get_stream_data` uses double-checked locking to avoid a decode stampede (TD-2)
- Edit commits only rewrite content streams that actually changed; untouched page/XObject streams keep their original bytes (TD-4)
- `StandardFont::from_name` strips subset prefixes (`ABCDEF+`), so subsetted standard fonts resolve to correct metrics (TD-9)

### Fixed
- Inline image (`BI/ID/EI`) data length is now computed deterministically (explicit `/L`, JPEG EOI, or unfiltered raster geometry) instead of scanning for `EI`, which could terminate early when raw image bytes spelled a whitespace-`EI` sequence (TD-7)
- `filters::apply_pipeline` no longer makes a wasted up-front copy of the input before the first filter runs (TD-8)
- `PdfEditor::save_new()` previously serialised only the writer pool, silently dropping all original document objects; it now copies every object from the original xref before applying copy-on-write overrides
