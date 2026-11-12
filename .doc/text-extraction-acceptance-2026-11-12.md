# Phase 3 Text-Extraction Acceptance + API Wiring — Implementation Report

**Date:** 2026-11-12
**Scope:** `tests/gen_text_fixtures.py` + `tests/text_extraction.rs` (new); `wasm/document.rs`, `server/handlers.rs`; `clients/{python/java,dotnet}` wrappers

## What Was Implemented

Completes plan Phase 3 acceptance and wiring (core modules: previous report).

- **Acceptance fixtures** (`tests/fixtures/`, generator
  `tests/gen_text_fixtures.py` — raw-byte PDF construction with a correct
  xref table, no external deps):
  - `text_rtl.pdf` — Hebrew "שלום" emitted in *visual* order through a
    Type0/Identity-H font with a 4-entry `bfchar` ToUnicode CMap (CID 1→ש …
    4→final mem; stream emits CIDs 4,3,2,1). No embedded font program needed:
    extraction decodes via ToUnicode only.
  - `text_columns.pdf` — two columns (x = 50 / x = 350, aligned baselines),
    absolute positioning via `Tm` (a first cut used `Td`, whose *cumulative*
    offset semantics put every span at the wrong place — caught by the
    integration test and fixed in the generator).
  - `text_rotated.pdf` — `Horizontal` (0°), `Upward` (90°: `0 1 −1 0`),
    `Flipped` (180°: `−1 0 0 −1`), `Downward` (270°: `0 −1 1 0`). A first
    cut passed 10 operands to `Tm` (copied a CTM-concat sketch); fixed to
    exactly 6.
- **Acceptance tests** (`tests/text_extraction.rs`, 3 tests):
  - RTL: `plain_text() == "שלום"` — bidi visual→logical recovery verified
    end-to-end on a real parsed PDF.
  - Columns: `plain_text_columns(40)` == columns left-to-right, lines
    top-to-bottom; also asserts the plain flow interleaves (proving the
    fixture really has aligned baselines).
  - Rotated: `rotated_lines()` returns exactly three buckets (90/180/270)
    with the right texts; horizontal flow keeps `Upward`/`Downward` in one
    baseline cluster (they share y = 50).
- **API wiring:**
  - wasm: `extract_text_columns(page_index, min_gap)` and
    `extract_text_dehyphenated(page_index)` on `PdfDocument`, mirroring the
    existing `extract_text` permission gate (`can_copy_text`, crypto).
  - REST: `/api/v1/extract-text` gained `mode=plain|columns|dehyphenated`
    and `min_gap` query params (validated; OpenAPI annotations updated).
  - Clients: python `extract_text(pdf, page, mode=…, min_gap=…)`, java
    `extractText(byte[], String, double)`, dotnet
    `ExtractTextAsync(byte[], string, double)` — all additive overloads.

## Design Decisions

- **Hand-built fixtures instead of writer-produced ones** — extraction only
  needs content streams + ToUnicode, so minimal raw-byte PDFs are
  deterministic, tiny (≤ 1.2 KB) and audit-able; same philosophy as the
  jpx fixture generator.
- **No embedded font for the RTL fixture** — the interpreter's ToUnicode
  path decodes CIDs without a font program; keeps the fixture byte-small
  and license-clean.
- **REST `mode` parameter** (rather than three endpoints) — one extraction
  surface, additive and backward-compatible; unknown modes are rejected
  with a 422.

## Test Coverage

- 3 new acceptance tests + 1 REST validation test (601 lib-equivalent
  total across binaries with `render`); server feature tests run under
  `--features server,render`.
- Full verification: fmt/clippy (`-D warnings`, ± `render`), all
  integration binaries, wasm32 build — clean.

## Known Limitations / Follow-up

- `rotated_lines()` is not exposed over wasm/REST yet (Rust-level API only)
  — next slice when a consumer needs it.
- Bidi limitations from the core report carry over (no bracket mirroring,
  `AN` digits inside reversed runs).
- Column detection thresholds (`min_gap`, 1.5× median width) are
  heuristics tuned on synthetic fixtures; real-corpus tuning may follow.
