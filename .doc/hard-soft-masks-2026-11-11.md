# Hard `/Mask` + Soft-Mask-Under-Text — Implementation Report

**Date:** 2026-11-11
**Scope:** `render/image.rs` (hard-mask decode helpers) + `render/page_renderer.rs` (image `/Mask` wiring, text soft-mask routing)

## What Was Implemented

Plan §1.6 (rank-improvement-plan-2026-11-09): image hard masks and ExtGState
soft masks applied under text.

- **Hard `/Mask` — stencil stream form** (`render/image.rs`:
  `decode_hard_mask` + private `unpack_1bit_samples`): decodes an image
  XObject used as another image's `/Mask` entry (ISO 32000-1 §8.9.6.4) into a
  single-channel alpha vector (255 = base image visible, 0 = hidden).
  - Sample semantics: mask sample 0 → paint, so `alpha = 255 - sample`;
    `/Decode [1 0]` reverses the mapping.
  - Supported encodings: 1-bit packed stencil masks (MSB first, rows
    byte-padded per §8.9.6.2, missing bytes decode as 0) and 8-bit gray
    masks; other bpc values are rejected with a warning (no panic, R1).
  - Dimension mismatch with the base image handled by the existing
    nearest-neighbour `scale_gray_mask`.
- **Hard `/Mask` — chroma-key array form** (`render/image.rs`:
  `decode_chroma_key`): `[min max …]` inclusive per-component ranges; a pixel
  whose *every* component sample falls inside its range is masked out.
  Ranges are compared against the decoded component samples (post
  indexed-palette expansion); 8 bpc and simple colour spaces only —
  others warn and skip.
- **`draw_image_xobject` wiring** (`render/page_renderer.rs`): after the
  existing `/SMask` handling, an unmasked image now consults `/Mask`.
  Stream form resolves + decrypts the mask stream exactly like `/SMask`
  (encrypted PDFs covered via `get_stream_data` on the reference); the
  resulting alpha is applied with the existing `apply_smask`. `/SMask`
  takes precedence when both are present.
- **Soft mask under text** (`draw_text_span`): the "Phase 3 follow-up"
  gap is closed. When `current_soft_mask` is active the span's glyphs are
  painted into a transparent temp `PixmapBuffer` (same dimensions/origin as
  the page canvas), then `apply_canvas_soft_mask` multiplies the temp alpha
  and `composite_over` blends it onto the page — the identical
  masked-composite path already used by `stroke_path`/`fill_path`.
  Upright/rotated/sheared/emboldened/placeholder glyph blits all route
  through the same `cv` target, so every text render mode is covered.
  Temp allocation failure falls back to the direct path (warn, no panic).

## Design Decisions

- **Mask decode helpers live in `image.rs`, not `page_renderer.rs`** — the
  renderer file is already the largest in the render tree; `image.rs` keeps
  R4 headroom and the helpers are pure decode logic (no renderer state).
- **`/SMask` precedence** — the two mask forms are mutually exclusive in
  practice; checking `/Mask` only when no `/SMask` alpha was produced avoids
  double-masking and matches common viewer behaviour.
- **Chroma-key compares post-expansion samples** — after indexed-palette
  expansion the samples are base-colour values, which is what producers
  typically express ranges in. Index-value comparison (pre-expansion) is the
  literal reading of "sample values" for indexed images but is vanishingly
  rare; documented as a limitation.
- **1-bit mask unpack re-implemented (not shared with `decode_image_mask`)**
  — that function emits filled RGBA for ImageMask painting, while the hard
  mask needs raw 0/255 samples; a shared core would have coupled two
  different output shapes for ~10 lines.
- **Text soft mask via temp canvas reuses the fill/stroke pattern** rather
  than per-glyph masking — one mask multiply + composite per span instead of
  per glyph, and the composite path is already proven.

## Test Coverage

- **11 new unit tests** (`render/image.rs`, 508 lib total with `render`):
  - `unpack_1bit_samples`: MSB-first expansion + row padding; missing
    bytes → 0 (happy + edge).
  - `decode_hard_mask`: 1-bit stencil inversion per spec; `/Decode [1 0]`
    reversal; 8-bit gray; zero-dimension reject (error path); unsupported
    4-bpc reject (error path).
  - `decode_chroma_key`: all-components-inside hides pixel (RGB red key);
    partial range keeps pixel; odd-length array reject; more ranges than
    channels reject (error paths).
- Existing suites: 508 lib + 22 render integration + 3 jpx parity + 5
  MuPDF snapshot tests all pass — snapshot suite confirms no regression in
  unmasked rendering; fmt/clippy (`-D warnings`, with and without `render`)
  and wasm32 build clean.

## Known Limitations / Follow-up

- Chroma-key ranges are compared against expanded (base-colour) samples, not
  raw index values, for indexed images.
- Chroma-key requires 8 bpc; 2/4-bit packed images with `/Mask` arrays warn
  and skip (stencil-stream masks support 1/8 bpc only; 2/4-bit masks also
  warn and skip).
- `/Mask` on inline images (`draw_image`, not `draw_image_xobject`) is not
  handled — inline images are rare and the operator path would need the same
  block replicated; deferred until a real document needs it.
- No dedicated golden fixture for hard-mask images (the MuPDF snapshot suite
  has none today); unit tests cover the decode semantics, and the wiring
  mirrors the already-validated `/SMask` path. Adding a masked-image fixture
  to `gen_snapshot_refs.sh` is cheap follow-up work.
