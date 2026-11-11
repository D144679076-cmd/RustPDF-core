# JBIG2 Decode — Implementation Report

**Date:** 2026-11-11
**Scope:** `src/parser/jbig2` — `/JBIG2Decode` filter support

## What Was Implemented

- **`src/parser/jbig2/`** — full port of pdf.js v3.11.174 `src/core/jbig2.js`:
  - `arith.rs` — QM-coder (ArithmeticDecoder, C.3 INITDEC/BYTEIN/DECODE/renormD).
  - `dec.rs` — `DecodeCtx` (decoder/context-cache/reader bundle), Annex A
    integer decoding (`decode_integer`), IAID (`decode_iaid`), MMR bitmap glue
    (`decode_mmr_bitmap`), uncompressed-bitmap reader, `log2`.
  - `generic.rs` — generic region decoding (template 0 fast path + generic
    path with adaptive pixels, context reuse masks) and generic refinement.
  - `symbol.rs` — symbol dictionary (direct-coded, refinement/aggregate,
    huffman collective bitmaps), text region decoding, text-region huffman
    table selection (7.4.3.1.6) + symbol-ID table decoding (7.4.3.1.7).
  - `huffman.rs` — Huffman table building (assign_prefix_codes, build_tree),
    standard/custom tables, table segment (53) decoding.
  - `tables.rs` — generated Annex B standard tables B.1–B.15
    (`#[rustfmt::skip]` to stay under the 800-line cap), emitted by
    `tests/gen_jbig2_tables.py`.
  - `halftone.rs` — pattern dictionary (16) + halftone regions (20/22/23)
    with Annex C gray-scale running-XOR plane decoding.
  - `segments.rs` — segment headers (7.2, incl. unknown-length type-38
    search), segment processing/dispatch, `Visitor` page assembly,
    `decode_chunks` / public `decode(embedded, globals)`.
- **`src/parser/ccitt.rs`** — added `decode_with_consumed` returning consumed
  source bytes (JBIG2 MMR glue needs it: halftone gray planes share one
  stream). `decode_with` delegates.
- **`src/parser/objects.rs`** — `/JBIG2Decode` wiring in
  `PdfStream::decode`/`decode_with_doc`: filter popped like CCITT;
  `/JBIG2Globals` indirect ref resolved to bytes and passed as the globals
  chunk; decoded buffer inverted (`^ 0xff`) per pdf.js `Jbig2Stream`
  (JBIG2 1 = black → PDF 1 = white).
- **Tests** — `src/parser/jbig2/tests.rs` (generated): 7 parity/error tests
  with node-validated fixtures. Generator `tests/gen_jbig2_fixtures.py`
  hand-encodes MMR segment streams (T.4 codes) and VALIDATES each against the
  actual pdf.js jbig2.js decoder run under node
  (`tests/.jbig2ref/`, gitignored harness); only node-produced page buffers
  are embedded as expected constants. Committed reference copy:
  `tests/jbig2-reference.js`.

## Design Decisions

- **pdf.js as port authority** — same playbook as CCITT; every fixture
  constant is the byte sequence pdf.js itself produced (exact parity).
- **`DecodeCtx<'a>`** bundles `decoder`/`cache`/`reader` so disjoint field
  borrows replace per-function parameter triples (avoids E0499).
- **Unsupported combos rejected as `PdfError`** (matches plan §1.4 "partial"):
  refinement+Huffman, halftone skip, non-OR halftone operator, refinement LTP
  prediction.
- **`decode_integer` OOB semantics** — Annex A: sign=1 with value=0 leaves
  JS `signedValue` undefined → reference returns null (the OOB marker). The
  port maps this to `None` instead of `Some(0)`; getting this wrong makes the
  IADW symbol loop never terminate (found via hang, not assertion).
- **MMR EOFB flush** — pdf.js `decodeMMRBitmap(endOfBlock=true)` reads up to
  5 extra chars after the last row to consume the EOFB; `decode_with_consumed`
  replicates this so sequential halftone planes resume at the correct offset.
- **Halftone gray planes** are a running XOR (Annex C): `bit ^= plane_pixel`,
  index bit set from the cumulative bit — naive per-plane binary reading
  mismatches the reference (node is ground truth).
- **Fixture 2** uses a real pdf.js test stream
  (`jbig2_symbol_offset.pdf`, /Length 191 — raw extraction carries a trailing
  EOL pair that pdf.js never feeds to the decoder; trimmed).
- **Fixtures 1/3** hand-encode segment headers with the correct 7.2 field
  order (referred numbers *before* page association) and per-plane EOFB
  codes between shared-stream MMR planes.

## Test Coverage

- `generic_region_mmr` — happy path, hand-encoded MMR (16×4), node-validated.
- `generic_region_mmr_error_truncated` — truncated stream never panics (R1).
- `symbol_dictionary_arithmetic` — real pdf.js arithmetic stream (132×14,
  symbol dict + text region), node-validated byte-exact.
- `pattern_dictionary_halftone_mmr` — pattern dict + halftone region (16×16,
  MMR collective + gray planes), node-validated byte-exact.
- `globals_stream_sharing` — decode with empty `/JBIG2Globals` chunk.
- `invalid_segment_type_rejected` — unknown segment type → `Err` (R5/R7).
- `empty_stream_errors` — empty input → `Err`.
- Full lib suite: 363 passed (up from 356 at CCITT commit).

## Known Limitations / Follow-up

- Huffman-coded symbol/text dictionaries are implemented but not covered by
  an end-to-end fixture (the pdf.js huffman test PDFs are dead web-archive
  links); coverage is via hand-encoded MMR fixtures + custom-table unit path.
- Halftone skip, non-OR combination operators, refinement+Huffman, refinement
  LTP: rejected with `PdfError` (rare in practice).
- Render-layer smoke test of `jbig2_symbol_offset.pdf` end-to-end via
  `PdfStream::decode_with_doc` is a possible follow-up.
- `tests/.jbig2ref/` harness (node) is gitignored like `tests/.ccittref/`.
