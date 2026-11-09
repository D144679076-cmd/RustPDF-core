# Changelog

All notable changes to `pdf-core` are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- **PDF 2.0 Phase A** (`.doc/pdf2-support-plan-2026-11-07.md`): `parser::version` module — `PdfVersion` enum (`V1_0…V2_0` + `Unknown`) and `detect_version()` scanning the first 1024 bytes for the `%PDF-m.n` header (§7.2); future versions (e.g. `%PDF-2.7`) warn + parse, never reject. `PdfDocument.version` field + `PdfDocument::version()` accessor expose it on both crypto and non-crypto parse paths
- **PDF 2.0 Phase B** — Unicode passwords for R6 encryption (ISO 32000-2 §7.6.4.3.2): new `crypto::password` module decodes the password text string (UTF-16BE/UTF-8 BOM, bare UTF-8, PDFDocEncoding fallback), applies the RFC 3454 SASLprep mapping step (soft hyphens/zero-width chars removed, non-ASCII spaces → U+0020), and re-encodes UTF-8; R6 verify/derive try normalised-then-raw candidates so ASCII-pass-through and legacy extension-era R6 files both keep working
- **Encryption policy gate** (Phase B): `EncryptionHandler::from_trailer` now takes the document `PdfVersion`; RC4 (V1/V2/V3) in a file claiming PDF 2.0+ is rejected gracefully as `Err(PdfError::Encrypted)` per ISO 32000-2 (removed cipher), while v1.x files keep RC4 support — dual-version policy honored
- **PDF 2.0 Phase C** — Associated Files (ISO 32000-2 §7.11.3/§7.11.4): new `document::associated_files` module — `FileSpec` (`/F` + required `/UF`, `/Desc`), `AfRelationship` enum (unknown names degrade to `Unspecified` + warn), `EmbeddedFile` + `EmbeddedFileParams` (`/Size`, dates, `/CheckSum`); `/AF` parsed at document level (`Catalog::associated_files`, unarrayed single-spec tolerated) and page level (`page_associated_files`); broken/badly-typed `/AF` entries skip + warn, never fail document open. Writer: `PdfWriter::add_associated_file(rel, name, data)` — FlateDecode `/EmbeddedFile` stream with `/Params /Size`, `/Filespec` with `/UF` (text string, UTF-16BE when non-ASCII) + ASCII-safe `/F` fallback. Missing header fix: `%FDF-m.n` headers now accepted by `detect_version` (FDF shares the PDF object grammar; forms/fdf.rs round-trips)
- **PDF 2.0 Phase D** — Document Security Store, read-only (ISO 32000-2 §12.8.4): new `signatures::dss` module — `parse_dss(doc, catalog_dict) -> Option<DocumentSecurityStore>` exposes pooled `/Certs`, `/OCSPs`, `/CRLs` and per-signature `/VRI` entries (hex-signature-hash keys with `/Cert`, `/OCSP`, `/CRL`, single `/TS` RFC 3161 token); payloads stay **lazy** (`DssPayload::Reference` object ids, `.der(doc)` inflates through the filter+encryption pipeline on demand) so multi-MB CRL pools cost nothing until requested; unknown VRI keys and non-stream pool entries skip + warn, missing `/DSS` → `None`, wrong-typed `/DSS`/pools → typed `Err`
- **PDF 2.0 Phase E** — writer opt-in: `EmitVersion` enum (`V1_7` default, byte-compatible today; `V2_0` via `PdfWriter::set_emit_version`) — V2_0 emits `%PDF-2.0` header, defaults the catalog to `/Version 2.0`, autofills required `/UF` on every `/Type /Filespec` missing it (from `/F`, Table 42), fail-closed `WriteError` when a Filespec has neither `/F` nor `/UF`, and rejects any RC4-form Standard-handler dictionary in the pool (ISO 32000-2 removed RC4); incremental updates skip the header but still apply pool normalization
- **PDF 2.0 Phase F** — fixtures + docs: committed writer-generated fixtures `tests/fixtures/pdf2-minimal.pdf`, `pdf2-associated-files.pdf`, `pdf2-signed-ltv.pdf` (DSS fixture, placeholder signature bytes) with reproducible generator `examples/generate_pdf2_fixtures.rs`, plus `tests/pdf2_fixtures.rs` integration tests (version round-trip, `/AF` + `/UF` read-back, DSS pool/VRI + `.der()` bytes); `EmitVersion` re-exported from `writer` mod; README/plan/competitive-report pointers updated. Deferred: `pdf2-encrypted-r6.pdf` fixture (writer-side R6 encryption is a documented follow-up)

### Changed
- **BREAKING (pre-1.0, minor bump)**: `PdfError` gains a `Header { offset }` variant — exhaustive `match`es over `PdfError` must be updated; missing/corrupt file headers now fail as `Header` (was `InvalidToken` at advisory offsets). `EncryptionHandler::from_trailer` signature gains a `PdfVersion` parameter (policy gate input)

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
