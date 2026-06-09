# Search Highlight bbox — Implementation Report

**Date:** 2026-06-09
**Scope:** text/search — match bounding box precision + frontend search UX

## What Was Implemented
- `pdf-editor-rust-core/src/text/search.rs` — `search_page`: replaced the whole-word
  bbox union with a **proportional sub-word** bbox. For each word a match overlaps,
  the byte range of the match within that word is mapped to a fraction of the word's
  rendered width (by char count) and only that slice is unioned into the result rect.
- `web-editor/src/components/SearchBar.vue` — debounce raised to 350 ms; duplicate-query
  guard (`lastQuery`) so the whole-document scan does not re-run for an unchanged term;
  loading spinner ("Searching…") now yields one animation frame before the synchronous
  WASM scan so it actually paints.
- Rebuilt the WASM package into `web-editor/src/pkg` via `make wasm`.

## Design Decisions
- **Fix in `search.rs`, not `build_line`.** The wide highlight came from `build_line`
  ([extractor.rs:155](../pdf-editor-rust-core/src/text/extractor.rs)) merging adjacent
  spans into one `TextWord` when the gap `< font_size * 0.3` — large title fonts collapse
  a whole line into one "word". Retuning that threshold would ripple into edit-text and
  plain-text extraction, so the bbox is narrowed at the search layer instead.
- **Char-fraction interpolation, boundary-safe.** Byte offsets are converted to char
  fractions via `char_indices().take_while(...)` — no string slicing, so no panic (R1)
  and correct for multibyte Vietnamese text. Uniform per-char advance is an approximation,
  but the match start anchors exactly at the word's left edge and the box is dramatically
  tighter than the full line.
- **rAF before scan.** `search_text` blocks the main thread; setting `searching=true`
  alone never paints. Awaiting one `requestAnimationFrame` lets the spinner render first.

## Test Coverage
- `search_prefix_match_is_narrower_than_full_word` (happy-path): on `multipage.pdf`,
  asserts `"Pag"` and `"Page"` share the same `x1` but `"Pag"`'s `x2` is strictly smaller
  — proves the box covers only the matched portion.
- Existing tests retained and green: `search_finds_text_on_correct_page`,
  `search_case_insensitive`, `search_no_results_returns_empty`,
  `search_result_bounds_are_positive`, `search_empty_query_returns_empty`.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test search` (6 passed),
  `cargo build --target wasm32-unknown-unknown` all clean.

## Known Limitations / Follow-up
- Sub-word x-positions assume uniform character advance. When a merged word contains
  large positional gaps (kerning/justification), mid-word matches may be a few points off.
  Exact per-glyph positioning would need `char_advances` carried through to `TextWord`
  (currently only on `TextSpan`) — deferred.
- The whole-document scan remains synchronous; debounce + dedupe mask the cost but a very
  large document could still stutter. Moving search off the main thread is future work.
