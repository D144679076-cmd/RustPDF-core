# text-search — Implementation Report

**Date:** 2026-06-06
**Scope:** Full-text search (`src/text/search.rs`)

## What Was Implemented

- `SearchResult` struct — `page_index: usize`, `text: String`, `bounds: [f64; 4]`
- `search_document(doc, query, case_sensitive)` — searches all pages, returns all matches in page order
- `search_page(doc, page_index, query, case_sensitive)` — searches a single page
- `src/text/mod.rs` — added `pub mod search` and re-exports
- `WasmDocument::search_text(&self, query, case_sensitive)` in `src/wasm/document.rs` — serializes results as a JSON array `[{page_index, text, bounds:[x1,y1,x2,y2]}, …]`

## Design Decisions

- **Word-join approach**: words are joined with spaces into one string; `find()` matches substrings across word boundaries. This is a simple O(n·m) scan that handles multi-word queries naturally.
- **Bounding-box union**: for each match, the result bounds are the union of every `TextWord` whose character range overlaps the match substring. Height is approximated as `word.y` to `word.y + word.font_size` (consistent with `extract_text_spans`).
- **Advance by 1**: after each match `search_from` moves to `abs_pos + 1`, so overlapping matches (e.g. "aa" in "aaa") are found.
- **Empty query short-circuit**: returns `Ok(Vec::new())` immediately to avoid vacuous matches that would touch every word.
- **No license gate**: `crate::license` does not exist in this codebase. The gate was omitted and left as a follow-up once the license module is implemented.

## Test Coverage

| Test | What it covers |
|------|---------------|
| `search_finds_text_on_correct_page` | happy-path: match on page 1 of multipage.pdf |
| `search_case_insensitive` | lowercase query matches mixed-case document text |
| `search_no_results_returns_empty` | query not present → empty result |
| `search_result_bounds_are_positive` | x2 > x1 and y2 > y1 for all results |
| `search_empty_query_returns_empty` | empty string returns empty without panic |

All 5 tests pass. Full test suite unaffected (285 existing tests pass).

## Known Limitations / Follow-up

- **License gate omitted** — add `crate::license::require(Tier::Pro, "search")?` once the license module exists.
- **Cross-line queries not supported** — the word-join string only reflects in-page word order; a query spanning a line break will match only if the words are adjacent in the concatenated string.
- **Web editor UI wiring (Phase 1 Step 4)** not implemented — `SearchBar.vue`, `AnnotationOverlay.vue`, prev/next navigation deferred to frontend work.
