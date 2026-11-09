# Render Snapshot Harness — Implementation Report

**Date:** 2026-11-09
**Scope:** `tests/snapshot.rs` + `tests/gen_snapshot_refs.sh` — golden-master render verification against MuPDF renders (Option A, user decision).

## What Was Implemented

- **`tests/snapshot.rs`** — snapshot harness (`#[cfg(feature = "render")]`):
  - `parse_ppm` — dependency-free binary PPM (P6, maxval 255) reader (`#` comments supported); truncation/bad-maxval/garbage-header error paths.
  - `compare` — RGB-channel comparison (alpha ignored): per-pixel delta budget, divergent-pixel fraction, mean absolute delta per page.
  - `snapshots_match_mupdf_reference` — auto-discovers every `tests/fixtures/*.pdf` plus matching MuPDF reference; per-page skip when no reference (asserts ≥ 1 compared page); prints per-page `mean Δ` / `divergent %` stats to stderr; fails with per-page divergence list.
  - Thresholds calibrated on the first parity run: PIXEL_TOLERANCE 16, MAX_DIVERGENT_FRACTION 25%, MAX_MEAN_DELTA 16 (baseline worst: mean 10.54 / divergent 13.52%).
- **`tests/gen_snapshot_refs.sh`** — reference generation: `mutool draw -o tests/fixtures/ref/<stem>-%d.ppm -r 72` per fixture (1-based page numbering matched in the harness); default scans all fixture PDFs, explicit paths supported; encrypted fixtures skipped with message.
- **`tests/TESTING.md`** — new "Render Snapshot Comparisons" section: usage, threshold rationals, license note.
- **`.gitignore`** — `tests/fixtures/ref/` (≈84 MB uncompressed; regenerated on demand, and keeps MuPDF-rendered images out of the repo).

## Design Decisions

- **Option A confirmed by user** — MuPDF renders as golden masters. License position: only raster *output* is consumed at test time; no MuPDF code linked, no generated images committed → clean.
- **PPM (P6) instead of PNG**: crate has no image-decode dep; PPM parse is ~40 lines, zero new deps (rules R2/R3). Trade-off: refs are gitignored rather than committed because 84 MB of uncompressed images would bloat the repo; regeneration is one deterministic command (mutool 1.27).
- **`mutool draw`, not `convert`**: `mutool convert` in MuPDF 1.27 dropped `-r`; `draw` is the raster path.
- **72 DPI (`-r 72` vs `render_page_rgba(..., 1.0)`)**: exact integer pixel alignment — both sides round the MediaBox the same way for Letter/A4 fixtures; verified by size-equality assertions passing across 60 pages.
- **Tolerance-based, not exact**: two independent rasterizers legitimately differ in glyph covering, AA and stochastic ink placement. Aggregate bars make regressions visible (the eprintln ranks OVER pages) while keeping C1 noise-free.
- Sync scope: harness only checks *existing* fixtures today; extending coverage = drop PDF in `fixtures/`, run script.

## Test Coverage

- `parse_ppm`: happy path, comment header, truncated data error, bad-maxval error.
- First full parity run (60 pages, 9 documents): all pass — Group-3/Laspeyres/Unit_1/multipage/minimal/form/with_stream/pdf2-* (pdf2 fixtures exact 0.00 Δ on flat pages).
- `cargo test --test snapshot --features render` green; wasm32 build unaffected (test-only changes).

## Known Limitations / Follow-up

- Reference generation needs network-free local `mutool` ≥ 1.27 — CI must run `gen_snapshot_refs.sh` before the snapshot test (or cache the refs in a CI artifact).
- Encrypted fixtures (restricted, encrypted_aes256, pdf2-r6-utf8-password) are never rendered: MuPDF skips them (wrong password) — renderer parity for decryption stays covered by unit/integration tests elsewhere.
- CMYK/transparency-heavy pages not yet in fixtures — add when 1.6 masks / 1.7 blend modes land.
- Thresholds are generous by design; tighten as render-completeness phases (1.2–1.7) land.
- No image-diff artifact saved on failure (only aggregate numbers) — consider dumping worst-N rows or a diff PPM on failure later.
