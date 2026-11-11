# Phase 3 Core Text-Extraction Modules — Implementation Report

**Date:** 2026-11-11
**Scope:** `text/bidi.rs`, `text/columns.rs`, `text/dehyphenate.rs`, `text/rotated.rs` (new) + `text/extractor.rs` integration + `Cargo.toml`

## What Was Implemented

First slice of plan Phase 3 (text extraction): the four new submodules with
R5 test coverage, integrated into `TextExtractor`. Existing
`plain_text()`/`extract_text` behaviour is unchanged (additive APIs only,
R10).

- **`bidi.rs`** (UAX #9 visual→logical recovery, via `unicode-bidi` 0.3 —
  pure Rust, WASM-safe R2):
  - `contains_rtl` — strong-RTL detection (`R`/`AL` classes).
  - `reorder_visual_to_logical` — reverses contiguous RTL character runs
    (plus neutrals/`AN` embedded between RTL chars); digits (`EN`) and LTR
    text keep their order; trailing neutrals stay outside the reversed run.
  - `reorder_rtl_words` — index permutation reversing maximal runs of
    RTL-containing words (RTL phrases are laid out right-to-left).
- **`columns.rs`** — gap-driven column split:
  - `split_columns(words, min_gap)` operates on **words**, not lines:
    aligned baselines across columns would otherwise merge the page into one
    gutter-covering line. Outlier-wide words (> 1.5× median width — rules,
    headings) are excluded from gutter *detection* but still assigned.
    Gutter = merged-interval hole wider than `min_gap`; boundaries at
    gutter midpoints; columns left-to-right, words top-to-bottom/LTR.
  - `column_text` — per-column line clustering (baseline within
    `font_size * 0.5`) → plain text.
- **`dehyphenate.rs`** — end-of-line hyphen repair:
  - `dehyphenate(lines)` joins a line ending in `-`/U+2010 with the next
    line when the continuation starts with a lowercase letter (conservative
    heuristic; ranges like "pages 12-" followed by proper nouns stay split).
  - `remove_soft_hyphens` strips U+00AD unconditionally.
  - `should_join` exposes the decision for incremental callers.
- **`rotated.rs`** — rotated-run handling:
  - `orientation(span)` classifies the text advance direction from
    `render_matrix_2x2` into 0/90/180/270 (u16) or `OTHER` (skew, > 1° from
    a multiple of 90°).
  - `group_rotated(spans)` builds lines per rotated bucket by rotating span
    origins into a canonical horizontal frame (`(y,−x)`, `(−x,−y)`,
    `(−y,x)`) and applying the same baseline clustering; skewed spans emit
    as single ungrouped lines.
- **`extractor.rs`** integration:
  - `build_line` now applies bidi recovery: per-word character-run
    reordering, then word-run resequencing.
  - New APIs: `into_columns(min_gap)`, `plain_text_columns(min_gap)`,
    `plain_text_dehyphenated()`, `rotated_lines()`.

## Design Decisions

- **Word-based column detection** — first implementation used lines and the
  unit test with aligned baselines caught the flaw immediately; words give
  the projection fine granularity without changing line grouping elsewhere.
- **Conservative dehyphenation** — join only on lowercase continuation; the
  alternative (always join) mangles hyphenated compounds and ranges.
- **`unicode-bidi` as a plain (non-optional) dependency** — small, pure
  Rust, no transitive deps; extraction is a core (non-feature-gated)
  surface. wasm32 build verified after the Cargo.toml change (R2).
- **Orientations as `u16`** — first cut used `u8`, which cannot hold 270;
  caught by a range lint, switched before commit.
- **`plain_text()` unchanged** — the wasm/ffi/server wiring already calls
  it; new capabilities are additive methods so no caller behaviour changes
  in this slice.

## Test Coverage

- **35 new tests** (556 lib total with `render`; 47 in `text::*`):
  - bidi (11): LTR identity, RTL detection, Hebrew/Arabic visual→logical,
    mixed LTR/RTL, digits-inside-RTL, trailing neutrals, word-run
    permutation (incl. single-word identity).
  - columns (6): single column, 2/3-column splits, wide-word exclusion,
    narrow-gap non-split, `column_text` ordering, empty input.
  - dehyphenate (7): join, uppercase non-join, soft hyphen, U+2010,
    three-way break, `should_join` truth table, no orphan join.
  - rotated (6): 0°/90°/180°/270° classification, skew → `OTHER`,
    90° line building order, horizontal exclusion.
  - extractor (5 new): RTL logical output, word-order case, column reading
    order, dehyphenated plain text, rotated grouping.
- Full suite green; fmt/clippy (`-D warnings`, ± `render`) and wasm32
  build clean.

## Known Limitations / Follow-up

- **Acceptance fixtures not yet generated** — plan Phase 3 calls for RTL,
  2-column and rotated fixture PDFs. Synthetic-span unit tests cover the
  algorithms; real-document fixtures (Identity-H + ToUnicode for RTL,
  columned body text) are the next slice.
- **Wiring of the new APIs** into `wasm/document.rs` / REST handlers /
  `clients/` wrappers not started — `extract_text` still uses
  `plain_text()`.
- Bidi limitations (documented in module docs): no bracket mirroring
  (UAX #9 L4); Arabic-Indic digits (`AN`) inside a reversed run are reversed
  with it; mixed-direction lines are handled at run granularity only.
- Rotated line *words* are emitted as one merged word per line (no
  sub-word gap splitting in the rotated frame); rotated 180°/270° paths
  have classification tests but no line-building tests yet.
