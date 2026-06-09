# Phase 1 — Full-Text Search

**Status:** Complete — 2026-06-06 (Steps 1–3 done; Step 4 web UI deferred)
**Effort:** ~4–5 days
**Tier gate:** Pro

## Context

`src/text/extractor.rs` already extracts `TextSpan` / `TextWord` items with full position metadata (`x, y, width, font_size`) per page. `src/wasm/document.rs` already has `extract_text_spans()` returning word-level JSON. The web editor `SearchBar.vue` component already exists but has no backend. Need substring search returning position rectangles, plus WASM exposure and UI wiring.

## Step 1 — New file `src/text/search.rs`

```rust
use crate::parser::PdfDocument;
use crate::document::{Catalog, Page};
use crate::text::TextExtractor;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub page_index: usize,
    pub text: String,
    pub bounds: [f64; 4],  // [x1, y1, x2, y2] in PDF user-space coords
}

/// Search all pages of `doc` for `query`. Returns one result per match instance.
pub fn search_document(
    doc: &PdfDocument,
    query: &str,
    case_sensitive: bool,
) -> Result<Vec<SearchResult>> {
    crate::license::require(crate::license::Tier::Pro, "search")?;
    let catalog = Catalog::from_document(doc)?;
    let page_count = catalog.page_count;
    let mut results = Vec::new();
    for i in 0..page_count {
        results.extend(search_page(doc, i, query, case_sensitive)?);
    }
    Ok(results)
}

/// Search a single page for all occurrences of `query`.
pub fn search_page(
    doc: &PdfDocument,
    page_index: usize,
    query: &str,
    case_sensitive: bool,
) -> Result<Vec<SearchResult>> {
    let catalog = Catalog::from_document(doc)?;
    let page_dict = catalog.get_page_dict(doc, page_index)?;
    let page = Page::from_dict(doc, &page_dict)?;
    let extractor = TextExtractor::extract_from_page(doc, &page)?;
    let words = extractor.words(); // Vec<TextWord> — each has text, x, y, width, font_size

    // Build concatenated text with space separators, tracking word start positions
    let mut full_text = String::new();
    let mut word_char_starts: Vec<usize> = Vec::with_capacity(words.len());
    for word in &words {
        word_char_starts.push(full_text.len());
        full_text.push_str(&word.text);
        full_text.push(' ');
    }

    let (haystack, needle) = if case_sensitive {
        (full_text.clone(), query.to_owned())
    } else {
        (full_text.to_lowercase(), query.to_lowercase())
    };

    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(pos) = haystack[search_from..].find(&needle) {
        let abs_pos = search_from + pos;
        // Find which words are covered by [abs_pos, abs_pos + needle.len())
        let match_end = abs_pos + needle.len();
        let mut x1 = f64::MAX;
        let mut y1 = f64::MAX;
        let mut x2 = f64::MIN;
        let mut y2 = f64::MIN;
        let mut matched_text = String::new();

        for (i, word) in words.iter().enumerate() {
            let wstart = word_char_starts[i];
            let wend = wstart + word.text.len();
            if wend > abs_pos && wstart < match_end {
                x1 = x1.min(word.x);
                y1 = y1.min(word.y);
                x2 = x2.max(word.x + word.width);
                y2 = y2.max(word.y + word.font_size);
                matched_text.push_str(&word.text);
            }
        }
        if x1 < f64::MAX {
            results.push(SearchResult {
                page_index,
                text: matched_text,
                bounds: [x1, y1, x2, y2],
            });
        }
        search_from = abs_pos + 1; // advance past start of match to find overlapping results
    }
    Ok(results)
}
```

## Step 2 — Update `src/text/mod.rs`

Add:
```rust
pub mod search;
pub use search::{SearchResult, search_document, search_page};
```

## Step 3 — WASM in `src/wasm/document.rs`

Add inside `impl WasmDocument`:
```rust
#[wasm_bindgen]
pub fn search_text(&self, query: &str, case_sensitive: bool) -> Result<String, JsError> {
    let results = crate::text::search_document(&self.doc, query, case_sensitive)
        .map_err(|e| JsError::new(&e.to_string()))?;
    // Serialize as JSON array: [{page_index, text, bounds: [x1,y1,x2,y2]}]
    let mut json = String::from("[");
    for (i, r) in results.iter().enumerate() {
        if i > 0 { json.push(','); }
        json.push_str(&format!(
            r#"{{"page_index":{},"text":{},"bounds":[{},{},{},{}]}}"#,
            r.page_index,
            serde_json_string(&r.text),
            r.bounds[0], r.bounds[1], r.bounds[2], r.bounds[3]
        ));
    }
    json.push(']');
    Ok(json)
}
```

Use the existing `serde_json_string()` helper already in `wasm/document.rs` for JSON string escaping.

## Step 4 — Web Editor in `web-editor/src/components/SearchBar.vue`

Wire the existing search bar UI to the WASM backend:
1. On submit or typing (debounced 300ms): call `wasmDoc.value.search_text(query, false)`.
2. Parse JSON result → store in `searchResults` ref.
3. For results on `currentPage`: emit to `AnnotationOverlay.vue` (already handles overlay divs).
4. The overlay should draw a yellow `rgba(255,220,0,0.4)` div at `bounds` coords (convert PDF user-space to screen pixels using `coords.ts::pdfToScreen()`).
5. Add prev/next buttons that scroll to and highlight successive results. Show "N of M" count.

## Tests

In `src/text/search.rs` add `#[cfg(test)]` module:
```rust
#[test]
fn search_finds_text_on_correct_page() {
    let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
    let doc = crate::parser::PdfDocument::parse(data).unwrap();
    let results = search_document(&doc, "Page 2", true).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].page_index, 1);
}

#[test]
fn search_case_insensitive() {
    let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
    let doc = crate::parser::PdfDocument::parse(data).unwrap();
    let results = search_document(&doc, "page 1", false).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn search_no_results_returns_empty() {
    let data = include_bytes!("../../tests/fixtures/minimal.pdf").to_vec();
    let doc = crate::parser::PdfDocument::parse(data).unwrap();
    let results = search_document(&doc, "XYZZY_NOT_PRESENT", true).unwrap();
    assert!(results.is_empty());
}

#[test]
fn search_result_bounds_are_positive() {
    let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
    let doc = crate::parser::PdfDocument::parse(data).unwrap();
    let results = search_document(&doc, "Page", true).unwrap();
    for r in &results {
        assert!(r.bounds[2] > r.bounds[0], "x2 > x1");
        assert!(r.bounds[3] > r.bounds[1], "y2 > y1");
    }
}
```

## Verification

```bash
cargo test text::search
cargo build --target wasm32-unknown-unknown --features wasm
```
