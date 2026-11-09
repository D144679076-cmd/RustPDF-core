# XRef Recovery — Implementation Report

**Date:** 2026-11-09
**Scope:** `parser::objects::recovery` — Phase P1 (xref recovery/rebuild) of the rank-improvement plan (`.fix_plan/rank-improvement-plan-2026-11-09.md`). Closes gap §3.6 ("no xref recovery — corrupt/truncated xref = total parse failure").

## What Was Implemented
- New module `src/parser/objects/recovery.rs` (child of `objects` so it sees private `XRefEntry`; no new deps, WASM-safe — R2).
- Public-behavior entry point: `recovery::load_xref(data, Option<usize>) -> Result<(xref map, trailer)>`, hooked into all three load paths:
  - `PdfDocument::parse` (`#[cfg(not(feature = "crypto"))]`)
  - `PdfDocument::parse_with_password` (`#[cfg(feature = "crypto")]`)
  - `has_encryption_trailer`
- `find_startxref_offset` failure is no longer fatal there — a missing/unusable `startxref` now goes straight to reconstruction instead of aborting.

## Recovery ladder (first success wins)
1. **Normal path** with post-load verification: the `/Root` reference must exist in the built index and its mapped offset must hold a matching `N G obj` header (object-stream targets exempt — `Compressed` entries cannot be marker-checked).
2. **Rung 1:** retry every `startxref` token in the whole file (not just last 1024 B), newest first — recovers stale/truncated final trailers when earlier incremental sections survive.
3. **Rung 2:** attempt every XRef-section candidate, newest first: all `xref` keyword occurrences (boundary-checked) and backtracked `/Type /XRef` stream objects.
4. **Rung 3 (full reconstruction):**
   - body-marker scan of every `N G obj` header (`scan_object_footprints`), last occurrence wins (incremental-update order);
   - trailer synthesized from all `trailer` dicts + XRef-stream dicts carrying trailer keys (newest-wins merge with older gaps filled), preferring candidates whose `/Root` verifies;
   - last resort: `/Type /Catalog` object located directly and a minimal trailer synthesized from it.
All failures log via `log::warn!`/`log::debug!` (R9 — no print). No panic paths (R1). Errors carry offsets (R7). No new deps (R2). No unsafe (R8).

## Design Decisions
- **Verification of `/Root` was added to the *normal* path**, not just recovery. `build_xref` alone can "succeed" over wrong entry offsets (nothing bounds-checks them), which previously produced confusing resolver errors later. Marker verification is O(offset) and catches the classic cut-file-at-random-offset case.
- **Last-occurrence-wins in the body scan** matches incremental-update semantics; noted trade-off: a compressed stream's raw bytes could counterfeit a marker. Mitigated by engaging the scan only when nothing else works; a counterfeit surfaces as a parse failure at access time (same trade-off as pdf.js).
- **`startxref_value` rejects offsets ≥ file length** — an out-of-bounds value is itself evidence of damage and is exactly what rungs 2–3 exist for.
- **Deliberate behavior change (documented in tests):** *moderately* truncated PDFs (object bodies intact) now parse with recovered contents instead of hard-failing — pdf.js/MuPDF parity. *Severely* truncated files (no object survives) still fail. `tests/real_pdf.rs::truncated_pdf_fails` was replaced by `truncated_pdf_exists_but_recovers` + `severely_truncated_pdf_fails`.

## Test Coverage
Unit (recovery.rs, R5 happy+error): `scan_finds_last_object_footprint`, `obj_header_rejects_endobj`, `rebuild_from_no_startxref`, `rebuild_from_stale_startxref`, `rebuild_synthesizes_trailer_from_catalog`, `rebuild_fails_cleanly_on_garbage`, `startxref_value_out_of_bounds_is_rejected`, `root_marker_mismatch_not_resolved`, `root_marker_match_resolves`.
Integration (`tests/xref_recovery.rs` + `tests/fixtures/` corruption variants of minimal.pdf, R6): `corrupt_startxref.pdf`, `corrupt_offsets.pdf`, `no_xref.pdf` → `recovers_stale_startxref`, `recovers_wrong_table_offset`, `recovers_missing_xref_entirely`.
Real-doc regression: `real_pdf.rs` updated truncation pair (see above); full lib suite 819/819.

## Verification
- `cargo fmt --check` clean (module files formatted)
- `cargo test --lib --all-features` 819 passed + integration suites (real_pdf/pdf2/signatures/streaming/bookmarks/merge_redact/write_edit/xref_recovery) green
- `cargo build --target wasm32-unknown-unknown` green
- `cargo clippy --all-features -- -D warnings`: unchanged pre-existing errors only (uniffi fn-pointer + `&mut Vec`) — verified identical on HEAD via stash.

## Known Limitations / Follow-up
- Type-2 (object-stream) entries cannot be body-scanned if the objstm itself is damaged; only direct objects are recoverable. Noted in module docs.
- Body-scan is O(n) single pass — only engaged on broken files, so cost is amortized.
- `PdfDocument::startxref_offset` (editing append-position path, `editor/document_editor.rs`) still hard-fails with no `startxref`; editor-side opening of recovered files is future work.
- No fidelity guard/flag for *which* recovery rung engaged (R7 offset error unchanged); caller-visible diagnostics could be surfaced later through the error model.
- `/Encrypt` in a recovered trailer: parse paths read the rebuilt trailer as before, so encryption policy is preserved, but new byte-level corruption tests with encrypted fixtures should be added along with the crypto gate.
- Next plan item per ranking: render completeness (shading types 1/4/5, Type 3 fonts, CCITT).
