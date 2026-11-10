# CCITTFaxDecode Filter — Implementation Report

**Date:** 2026-11-10
**Scope:** `src/parser/ccitt.rs`, `src/parser/ccitt_parms.rs`, `src/parser/ccitt_tests.rs`, `src/parser/ccitt_tables.rs` (Phase 1.3 of `.fix_plan/rank-improvement-plan-2026-11-09.md`)

## What Was Implemented

- **`src/parser/ccitt_tables.rs`** (generated, 1091 lines of table data): the six Huffman
  tables from pdf.js v3.11.174 `src/core/ccitt.js` — `TWODIMTABLE` (128),
  `WHITETABLE1` (32), `WHITETABLE2` (512), `BLACKTABLE1` (128),
  `BLACKTABLE2` (192), `BLACKTABLE3` (64) — plus a `RunCode = (i16, i16)`
  alias. Regenerate with `python3 tests/gen_ccitt_tables.py <out>`.
- **`src/parser/ccitt_parms.rs`**: `FaxParms` + `FaxParms::from_dict` — `/K`, `/Columns`,
  `/Rows`, `/BlackIs1`, `/EncodedByteAlign`, `/EndOfLine`, `/EndOfBlock` with
  ISO 32000-1 §7.4.6 defaults (K=0, Columns=1728, Rows=0, EndOfBlock=true).
  Split from the decoder for the 800-line cap (R4); re-exported as
  `ccitt::FaxParms` so the public path is unchanged.
- **`src/parser/ccitt.rs`** (723 lines):
  - `decode(data, Option<&PdfDict>)` / `decode_with(data, &FaxParms)` public
    entry points (256 MiB `MAX_OUTPUT_BYTES` safety cap).
  - `Decoder` — bit-accurate port of pdf.js `CCITTFaxDecoder`: 1-D (K=0),
    mixed 2-D (K>0, EOL + extension bit), pure 2-D MMR (K<0 / G4) with PASS /
    HORZ / V0 / VR1–3 / VL1–3 modes; `look_bits`/`eat_bits` zero-padded EOF
    semantics; `add_pixels`/`add_pixels_neg` boundary bookkeeping; EOL
    scanning, byte alignment, EOFB fast-table path (`eoblock=true`) and
    linear `find_table_code` fallback; Return-To-Control (RTC) sequence.
  - Wiring: `PdfStream::decode` / `decode_with_doc` strip a trailing
    `CCITTFaxDecode`/`CCF` from the filter pipeline (CCITT must be last) and
    call `ccitt::decode` with the resolved `/DecodeParms`; all other filters
    and predictors unchanged. `parser/mod.rs` declares the new modules.
- **`src/parser/ccitt_tests.rs`** (260 lines): the 14 R5 tests, split out of
  the decoder file for the same R4 cap.
- **Test infrastructure** (`tests/`):
  - `ccitt-reference.js` — verbatim pdf.js v3.11.174 `ccitt.js` (Apache 2.0
    header intact; import line stubbed by the generator at runtime only).
  - `gen_ccitt_tables.py` — extracts the six tables from the reference and
    emits `src/parser/ccitt_tables.rs`.
  - `gen_ccitt_fixtures.py` — recovers run-code pairs by brute-force
    simulation of pdf.js's exact lookup logic (including band offsets like
    `blackTable2[(code>>1)-64]`), builds encoded streams for the vectors
    below, and **validates each one by running the real pdf.js decoder under
    node**; the node-emitted bytes are what the Rust tests assert.
  - `.gitignore` entry for the node harness dir `tests/.ccittref/`.

## Design Decisions

- **pdf.js as the authoritative algorithm** (not MuPDF/libtiff): libtiff
  encodes tables as state machines and MuPDF as prefix arrays; pdf.js's
  `(bits, value)` tables are directly transcribable and its decoder is
  battle-tested against real-world damaged files. Master pdf.js moved CCITT
  to WASM, so v3.11.174 (last JS version) is pinned.
- **Test vectors = node ground truth.** Hand-computing expected rows proved
  error-prone (phantom trailing rows, `eof` set mid-row still emitting a
  byte, G4 ref-line bookkeeping). Instead, expected bytes are exactly what
  pdf.js emitted — the Rust port is validated for parity, not against a
  possibly-wrong mental model.
- **Damaged data tolerated, invalid parameters are errors.** Matches the
  reference: bad codes clamp runs (`/Columns` cap), set the internal error
  flag, and resync via EOL when `/EndOfLine` is set. Only nonsensical
  `/DecodeParms` (out-of-range `/Columns`/`/Rows`, wrong-typed bools) return
  `PdfError` (R1/R7: offsets are 0 because the failure is in the dict, not
  the data).
- **`/Rows` padding convention:** when `/Rows` is given, missing rows are
  white-filled in the stream's own convention (`0x00` if `/BlackIs1` else
  `0xff`) so consumers see uniform data. Without `/Rows`, output ends at EOF
  exactly like pdf.js (an earlier draft padded to the internal 256 MiB cap —
  caught by the test hang, fixed before commit).
- **`eof` mid-row still emits the current call's byte** (JS `readNextChar`
  parity), which produces the phantom trailing `0xaa` bytes seen in the
  vectors; callers stop at the next `None`.
- **Brute-force code recovery** instead of prefix-run inversion: pdf.js's
  tables are indexed with band-offset tricks (`code>>5`, `(code>>1)-64`,
  `code>>7`) that make naive inversion ambiguous; simulating
  `look_bits(n) = code << (max_bits - n)` over all lengths reproduces the
  decoder's exact view, and every black code was additionally round-tripped
  through the node decoder.
- **Caps:** `/Columns` ≤ 2^16 and output ≤ 256 MiB (`MAX_OUTPUT_BYTES`) so a
  hostile `/DecodeParms` cannot trigger unbounded allocation.

## Test Coverage

14 unit tests in `ccitt_tests.rs` (all node-validated vectors, `*_STREAM` /
`*_EXPECT` constants):

| Test | Covers |
|---|---|
| `one_d_rows_decode_runs` | 1-D MH, two rows + trailing EOL, phantom row byte |
| `group4_vertical_right_and_horizontal` | G4 VR1 + HORZ with makeup/terminating codes |
| `group4_vertical_left` | G4 VL1 (negative boundary via `add_pixels_neg`) |
| `group4_pass_mode` | G4 PASS mode |
| `group4_v0_two_rows` | G4 V0 chains, two rows |
| `k1_with_eol_and_extension_bits` | K=1, EOL + 1-D/2-D tag bits |
| `encoded_byte_align` | `/EncodedByteAlign` row re-alignment |
| `black_is1_inverts_output` | `/BlackIs1` output inversion |
| `white_makeup_codes` | white makeup codes (75 = 64+11, 182 = 128+54) |
| `black_long_run` | long black terminating run (14) + white makeup (113) |
| `rows_param_limits_and_pads` | `/Rows` output truncation + white padding |
| `truncated_stream_does_not_panic` | error-path: truncated & garbage input, bounded output, no panic |
| `invalid_parms_are_errors` | error-path: bad `/Columns`, `/Rows`, non-bool `/BlackIs1`; defaults from `None` |
| `stream_decode_uses_ccitt_parms` | `PdfStream::decode` wiring with `/DecodeParms` |

Happy-path + error-path per R5; `cargo test --lib --all-features` = 859/859.

## Known Limitations / Follow-up

- `/EndOfLine` + damaged-row resync path (`err && eoline`) is ported but has
  no dedicated vector; behaviour mirrors the reference and is exercised
  implicitly by the garbage-input test.
- RTC detection only fires in the `eoblock && byte_align` combination, as in
  the reference.
- `find_table_code` linear-search path (non-`EndOfBlock` streams) is ported
  but untested with real data — pdf.js's own test corpus for it is one
  synthetic PDF (`ccitt_EndOfBlock_false.pdf`); worth an end-to-end fixture.
- Optional follow-up from the plan: end-to-end snapshot fixture (CCITT image
  PDF + `mutool` reference render) — deferred as plan-optional.
- Next phase 1.4: JBIG2 (partial jbig2dec port; segment types 4/11/20;
  `/JBIG2Globals` in `render/image.rs:34`; shared bitreader util).
