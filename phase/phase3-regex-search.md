# Phase 3 — Regex Search

**Status:** Complete — 2026-06-17
**Effort:** ~3–4 days (after Phase 1 full-text search is done)
**Tier gate:** Pro
**Prerequisite:** phase1-full-text-search.md must be complete

## Context

Extends the full-text search from Phase 1 to support regex patterns. The `regex` crate is pure Rust and WASM-compatible. Everything else (position mapping, result struct) is identical to substring search.

## Dependency — Add to `Cargo.toml`

```toml
[dependencies]
regex = { version = "1", optional = true, default-features = false, features = ["std", "unicode-perl"] }

[features]
search = ["dep:regex"]
wasm = ["...", "search"]
```

## Extend `src/text/search.rs`

```rust
#[cfg(feature = "search")]
pub fn search_document_regex(
    doc: &crate::parser::PdfDocument,
    pattern: &str,
    case_sensitive: bool,
) -> crate::error::Result<Vec<SearchResult>> {
    crate::license::require(crate::license::Tier::Pro, "regex_search")?;
    use regex::RegexBuilder;
    let re = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| crate::error::PdfError::invalid_structure(
            Box::leak(format!("invalid regex: {}", e).into_boxed_str())
        ))?;
    let catalog = crate::document::Catalog::from_document(doc)?;
    let page_count = catalog.page_count;
    let mut results = Vec::new();
    for i in 0..page_count {
        results.extend(search_page_regex_inner(doc, i, &re)?);
    }
    Ok(results)
}

#[cfg(feature = "search")]
pub fn search_page_regex(
    doc: &crate::parser::PdfDocument,
    page_index: usize,
    pattern: &str,
    case_sensitive: bool,
) -> crate::error::Result<Vec<SearchResult>> {
    crate::license::require(crate::license::Tier::Pro, "regex_search")?;
    use regex::RegexBuilder;
    let re = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| crate::error::PdfError::invalid_structure(
            Box::leak(format!("invalid regex: {}", e).into_boxed_str())
        ))?;
    search_page_regex_inner(doc, page_index, &re)
}

#[cfg(feature = "search")]
fn search_page_regex_inner(
    doc: &crate::parser::PdfDocument,
    page_index: usize,
    re: &regex::Regex,
) -> crate::error::Result<Vec<SearchResult>> {
    // Same word extraction as search_page()
    let catalog = crate::document::Catalog::from_document(doc)?;
    let page_dict = catalog.get_page_dict(doc, page_index)?;
    let page = crate::document::Page::from_dict(doc, &page_dict)?;
    let extractor = crate::text::TextExtractor::extract_from_page(doc, &page)?;
    let words = extractor.words();

    let mut full_text = String::new();
    let mut word_char_starts: Vec<usize> = Vec::with_capacity(words.len());
    for word in &words {
        word_char_starts.push(full_text.len());
        full_text.push_str(&word.text);
        full_text.push(' ');
    }

    let mut results = Vec::new();
    for m in re.find_iter(&full_text) {
        let abs_pos = m.start();
        let match_end = m.end();
        let mut x1 = f64::MAX; let mut y1 = f64::MAX;
        let mut x2 = f64::MIN; let mut y2 = f64::MIN;
        let mut matched_text = String::new();

        for (i, word) in words.iter().enumerate() {
            let wstart = word_char_starts[i];
            let wend = wstart + word.text.len();
            if wend > abs_pos && wstart < match_end {
                x1 = x1.min(word.x); y1 = y1.min(word.y);
                x2 = x2.max(word.x + word.width); y2 = y2.max(word.y + word.font_size);
                matched_text.push_str(&word.text);
            }
        }
        if x1 < f64::MAX {
            results.push(SearchResult { page_index, text: matched_text, bounds: [x1, y1, x2, y2] });
        }
    }
    Ok(results)
}
```

## Update `src/text/mod.rs`

```rust
#[cfg(feature = "search")]
pub use search::{search_document_regex, search_page_regex};
```

## WASM in `src/wasm/document.rs`

```rust
#[cfg(feature = "search")]
#[wasm_bindgen]
pub fn search_text_regex(&self, pattern: &str, case_sensitive: bool) -> Result<String, JsError> {
    let results = crate::text::search_document_regex(&self.doc, pattern, case_sensitive)
        .map_err(|e| JsError::new(&e.to_string()))?;
    // Same JSON serialization as search_text()
    Ok(serialize_search_results(&results))
}
```

## Tests in `src/text/search.rs`

```rust
#[cfg(feature = "search")]
#[test]
fn regex_finds_pattern() {
    let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
    let doc = crate::parser::PdfDocument::parse(data).unwrap();
    let results = search_document_regex(&doc, r"Page\s+\d+", true).unwrap();
    assert_eq!(results.len(), 3); // "Page 1", "Page 2", "Page 3"
}

#[cfg(feature = "search")]
#[test]
fn invalid_regex_returns_error() {
    let data = include_bytes!("../../tests/fixtures/minimal.pdf").to_vec();
    let doc = crate::parser::PdfDocument::parse(data).unwrap();
    let result = search_document_regex(&doc, r"[invalid", true);
    assert!(result.is_err());
}
```

## Verification

```bash
cargo test --features search -- regex
cargo build --target wasm32-unknown-unknown --features wasm,search
```
