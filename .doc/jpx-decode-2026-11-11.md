# JPX (JPEG 2000) Wavelet-Lite Decoder — Implementation Report

**Date:** 2026-11-11
**Scope:** `src/parser/jpx/` (new module, 8 files) + `render/image.rs` JPXDecode wiring

## What Was Implemented

Pure-Rust JPEG 2000 codestream decoder ("wavelet-lite" subset per plan §1.5):
5-3 reversible wavelet only, no ROI, no 9-7 irreversible, no precinct
partitions. Unsupported features are detected and rejected with
`PdfError::unsupported` / warned via `log::warn!` (R1/R9).

- **`codestream.rs`** — marker-segment parser (SOC/SIZ/COD/COC/QCD/QCC/SOT/SOD,
  EOC); rejects 9-7 (`Sqcd` guard/transform check), precinct partitions, ROI,
  PPx/PPy != 15; SOT length + tile-index validation; JP2 container box
  detection with clear error (raw `.j2k` codestreams only).
- **`packets.rs`** — T2 packet header parser: tag trees (`tgt_*` port of
  `tgt.c`), zero-length packets, packet-header bit order, code-block segment
  assembly (first/next pass, `numpasses`/`Lblock` coding).
- **`bio.rs`** — raw-bit IO, port of 2.5.4 `bio.c` (`bio_bytein` incl. the
  `buf == 0xff00 → ct = 7` bit-stuffing detail).
- **`mqc.rs`** — MQ arithmetic decoder, port of OpenJPEG 1.5.2 `mqc.c`:
  `INITDEC`, BYTEIN, LPS/MPS exchange, `mqc_renormd` do-while renorm.
  Validated **decision-exact** against a compiled 1.5.2 C harness over a
  synthetic segment.
- **`t1.rs`** — Tier-1 decoder: Z-scanner, significance propagation /
  refinement / cleanup passes, context state machine, dequantisation,
  `d += (v ^ (d<0)) ? poshalf : -poshalf` refpass midpoint tracking,
  final `datap /= 2` compensation (matches 2.5.4 `t1.c` both paths).
  LUTs in generated `t1_luts.rs` (script: `tests/gen_jpx_luts.py`).
- **`wavelet.rs`** — 5-3 reversible IDWT (lifting + interleave, cas=0/1);
  horizontal-then-vertical per resolution, shared mem buffer sized to the
  largest resolution dimension.
- **`mod.rs`** — image assembly: geometry build (resolution/band/code-block
  grids, `log2(size)-2` COD exponent convention), tile decode loop,
  component deinterleaving (`interleaved_u8`), RCT/MCT inverse transforms.
- **`render/image.rs`** — `Some("JPXDecode") => decode_jpx(raw)` arm in
  `decode_image()`; n≥4 components assumed CMYK with `log::warn!`.

Public API: `jpx::decode(data: &[u8]) -> Result<JpxImage, PdfError>`
(all offsets carried per R7; no `unwrap` outside tests per R1).

## Design Decisions

- **Subset, not full codec** — per plan sign-off: scan blobs are the target;
  9-7/ROI/precincts are rare in PDFs and logged, not silently mis-decoded.
- **OpenJPEG 1.5.2 as port authority** for MQ/T2 semantics (cleaner C than
  2.x), cross-checked against 2.5.4 sources where behaviour changed
  (bio bytein, refpass macro, `datap /= 2`).
- **MQ LPS renorm after exchange** — the 1.5.2 `mqc_decode` renormalises on
  *both* LPS and MPS-exchange paths; initial port renormalised only on MPS,
  which was the core pixel-corruption bug. Found by decision-level diff
  against the compiled C harness.
- **Code-block exponents** — COD stores `log2(cbw)-2`; geometry build uses
  `(cbw_exp + 2).min(cbg_exp)` (second core bug: was producing 4×4 grids).
- **QCD band count = `3*num_decomps + 1`** (LL0 included) — missing `+1`
  broke SPqcd parsing on all fixtures.
- **Wavelet cas=1 layout** — S occupies even positions (dn counts), D odd
  (sn counts); counts swap relative to cas=0.
- **glymur + system OpenJPEG 2.5.4** as an independent encoder for fixtures
  (`tests/gen_jpx_fixtures.py`), avoiding same-author bias in parity tests.
- Module lives in the parser unconditionally (not feature-gated) — it is
  WASM-safe (R2) and only ~2k lines; render wiring is in the existing
  `render` feature.

## Test Coverage

- **37 unit tests** (lib, `--features render`): mqc (5, incl. decision-exact
  C-harness vector), bio (8, incl. 0xff bit-stuffing), t1 (6), wavelet (8,
  incl. cas=1 and odd sizes), codestream (10: 9-7, precinct, quantized,
  bad-marker, SOT, JP2-box rejects + happy paths), mod (3, incl. full 1×1
  end-to-end decode). Every parser has happy + error paths (R5).
- **`tests/jpx_parity.rs`** (3 integration tests) — **pixel-exact (0 diffs)
  vs OpenJPEG 2.5.4 reference raw dumps** on 7 fixtures:
  `jpx_gray_64` (1 comp, 2 decomps), `jpx_rgb_64` (3 comps, RCT+MCT),
  `jpx_gray_tiles` (128×64, two 64×64 tiles), plus bisect fixtures
  `step_const_r1/r3`, `step_rnd_r1/r2` used during debugging.
- Full suite: 497 lib tests pass (460 baseline + 37 new); fmt/clippy
  (`-D warnings`, with and without `render`) and wasm32 build clean.

## Known Limitations / Follow-up

- No 9-7 irreversible (rejects), no ROI, no precinct partitions (rejects),
  no progression-order variants beyond LRCP/RLCP/… permissive parsing
  (packets are read in file order per tile-part).
- JP2 container (`.jp2` boxes, palette, channel defs) rejected — raw
  codestreams only, matching how PDF `/JPXDecode` streams are embedded.
- Sub-8-bit / >16-bit precision components not exercised by fixtures.
- cmake/LUT regeneration scripts (`tests/gen_jpx_luts.py`,
  `tests/gen_jpx_fixtures.py`) need `glymur` + OpenJPEG ≥ 2.5; committed
  fixtures make them optional for test runs.
