//! Full-text search over PDF documents.
//!
//! Searches all pages of a [`PdfDocument`] for a query string and returns
//! one [`SearchResult`] per match with its page index and bounding rectangle
//! in PDF user-space coordinates.

use crate::document::catalog::Catalog;
use crate::document::page::Page;
use crate::error::Result;
use crate::parser::objects::PdfDocument;
use crate::text::TextExtractor;

/// A single text-search match: page location and bounding rectangle.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 0-based page index where the match was found.
    pub page_index: usize,
    /// The matched text (concatenated from contributing words).
    pub text: String,
    /// Bounding box `[x1, y1, x2, y2]` in PDF user-space (origin bottom-left).
    pub bounds: [f64; 4],
}

/// Search every page of `doc` for occurrences of `query`.
///
/// Returns one [`SearchResult`] per match instance across all pages, in
/// page order. Set `case_sensitive` to `false` for case-insensitive matching.
pub fn search_document(
    doc: &PdfDocument,
    query: &str,
    case_sensitive: bool,
) -> Result<Vec<SearchResult>> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "search_document")?;
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let catalog = Catalog::from_document(doc)?;
    let page_count = catalog.page_count;
    let mut results = Vec::new();
    for i in 0..page_count {
        results.extend(search_page(doc, i, query, case_sensitive)?);
    }
    Ok(results)
}

/// Search a single page (0-based `page_index`) of `doc` for all occurrences
/// of `query`.
///
/// Text is reconstructed by joining words with spaces. Each match spans the
/// union of the matched portions of the words it overlaps. When a query
/// touches only part of a word, the bounding box covers just that fraction of
/// the word's width (approximated by char count), so a short query does not
/// highlight an entire merged word.
pub fn search_page(
    doc: &PdfDocument,
    page_index: usize,
    query: &str,
    case_sensitive: bool,
) -> Result<Vec<SearchResult>> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "search_page")?;
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let catalog = Catalog::from_document(doc)?;
    let page_dict = catalog.get_page_dict(doc, page_index)?;
    let page = Page::from_dict(doc, &page_dict)?;
    let extractor = TextExtractor::extract_from_page(doc, &page)?;
    let words = extractor.words();

    // Build a concatenated string with space separators, recording where
    // each word starts in that string so we can map match positions back
    // to word bounding boxes.
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
                // Highlight only the matched portion of this word, not the whole
                // box. Word grouping can merge several visual words into one
                // `TextWord` (see `build_line`), so a short query touching a
                // merged word would otherwise light up the entire run. We map
                // the match's byte range within the word to a fraction of its
                // rendered width by char count — boundary-safe (no slicing, no
                // panic) and correct for multibyte text. The result is an
                // approximation that assumes uniform per-char advance.
                let m_start_b = abs_pos.saturating_sub(wstart);
                let m_end_b = (match_end - wstart).min(word.text.len());
                let total = word.text.chars().count().max(1) as f64;
                let before = word
                    .text
                    .char_indices()
                    .take_while(|(b, _)| *b < m_start_b)
                    .count() as f64;
                let through = word
                    .text
                    .char_indices()
                    .take_while(|(b, _)| *b < m_end_b)
                    .count() as f64;
                let sub_x1 = word.x + word.width * (before / total);
                let sub_x2 = word.x + word.width * (through / total);
                x1 = x1.min(sub_x1);
                y1 = y1.min(word.y);
                x2 = x2.max(sub_x2);
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

        // Advance by 1 past the match start so overlapping matches are found.
        search_from = abs_pos + 1;
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Regex search (Pro tier, requires `search` feature)
// ---------------------------------------------------------------------------

/// Search every page of `doc` for matches of the regex `pattern`.
///
/// Returns one [`SearchResult`] per match in page order. Requires the `search`
/// feature and a Pro-tier license. Returns an error when `pattern` is not a
/// valid regex.
#[cfg(feature = "search")]
pub fn search_document_regex(
    doc: &PdfDocument,
    pattern: &str,
    case_sensitive: bool,
) -> crate::error::Result<Vec<SearchResult>> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "regex_search")?;
    use regex::RegexBuilder;
    let re = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| {
            crate::error::PdfError::invalid_structure(Box::leak(
                format!("invalid regex: {e}").into_boxed_str(),
            ))
        })?;
    let catalog = Catalog::from_document(doc)?;
    let page_count = catalog.page_count;
    let mut results = Vec::new();
    for i in 0..page_count {
        results.extend(search_page_regex_inner(doc, i, &re)?);
    }
    Ok(results)
}

/// Search a single page (0-based `page_index`) for matches of the regex `pattern`.
///
/// Requires the `search` feature and a Pro-tier license. Returns an error when
/// `pattern` is not a valid regex.
#[cfg(feature = "search")]
pub fn search_page_regex(
    doc: &PdfDocument,
    page_index: usize,
    pattern: &str,
    case_sensitive: bool,
) -> crate::error::Result<Vec<SearchResult>> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "regex_search")?;
    use regex::RegexBuilder;
    let re = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| {
            crate::error::PdfError::invalid_structure(Box::leak(
                format!("invalid regex: {e}").into_boxed_str(),
            ))
        })?;
    search_page_regex_inner(doc, page_index, &re)
}

#[cfg(feature = "search")]
fn search_page_regex_inner(
    doc: &PdfDocument,
    page_index: usize,
    re: &regex::Regex,
) -> crate::error::Result<Vec<SearchResult>> {
    let catalog = Catalog::from_document(doc)?;
    let page_dict = catalog.get_page_dict(doc, page_index)?;
    let page = Page::from_dict(doc, &page_dict)?;
    let extractor = TextExtractor::extract_from_page(doc, &page)?;
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
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_finds_text_on_correct_page() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let results = search_document(&doc, "Page 2", true).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].page_index, 1);
    }

    #[test]
    fn search_case_insensitive() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let results = search_document(&doc, "page 1", false).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn search_no_results_returns_empty() {
        let data = include_bytes!("../../tests/fixtures/minimal.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let results = search_document(&doc, "XYZZY_NOT_PRESENT", true).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_result_bounds_are_positive() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let results = search_document(&doc, "Page", true).unwrap();
        for r in &results {
            assert!(r.bounds[2] > r.bounds[0], "x2 > x1");
            assert!(r.bounds[3] > r.bounds[1], "y2 > y1");
        }
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let results = search_document(&doc, "", true).unwrap();
        assert!(results.is_empty());
    }

    #[cfg(feature = "search")]
    #[test]
    fn regex_finds_pattern() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let results = super::search_document_regex(&doc, r"Page\s+\d+", true).unwrap();
        assert_eq!(results.len(), 3, "expected 'Page N' on each of the 3 pages");
    }

    #[cfg(feature = "search")]
    #[test]
    fn regex_case_insensitive() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let results = super::search_document_regex(&doc, r"page\s+\d+", false).unwrap();
        assert!(!results.is_empty());
    }

    #[cfg(feature = "search")]
    #[test]
    fn invalid_regex_returns_error() {
        let data = include_bytes!("../../tests/fixtures/minimal.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();
        let result = super::search_document_regex(&doc, r"[invalid", true);
        assert!(result.is_err());
    }

    #[test]
    fn search_prefix_match_is_narrower_than_full_word() {
        // A prefix query ("Pag") must highlight a strictly narrower box than the
        // full word ("Page"), anchored at the same left edge — proving the match
        // bbox covers only the matched portion of the word, not the whole word.
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let doc = PdfDocument::parse(data).unwrap();

        let full = search_document(&doc, "Page", true).unwrap();
        let prefix = search_document(&doc, "Pag", true).unwrap();
        assert!(!full.is_empty(), "expected 'Page' matches");
        assert!(!prefix.is_empty(), "expected 'Pag' matches");

        let f = &full[0].bounds;
        let p = &prefix[0].bounds;
        // Same left edge (both start at the word's left).
        assert!(
            (p[0] - f[0]).abs() < 1e-6,
            "x1 should match: {} vs {}",
            p[0],
            f[0]
        );
        // Prefix is strictly narrower than the full word.
        assert!(
            p[2] < f[2],
            "prefix x2 {} should be < full x2 {}",
            p[2],
            f[2]
        );
    }
}
