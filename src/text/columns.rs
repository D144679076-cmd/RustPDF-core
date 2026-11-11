//! Column detection for text extraction.
//!
//! Multi-column pages are common in academic/scanned documents. The default
//! line grouping orders lines purely top-to-bottom, which interleaves the
//! columns' text. This module splits a page's words into vertical columns by
//! finding a "gutter": an X range that no word's body overlaps, separating
//! two or more clusters of words.
//!
//! Detection operates on *words*, not lines: aligned baselines across
//! columns would otherwise merge everything into one wide line that covers
//! the gutter.
//!
//! Algorithm (gap-driven):
//! 1. Project every word's `[x, x + width]` interval onto the X axis,
//!    skipping outlier-wide words (more than 1.5× the median word width —
//!    rules, headings) so they cannot cover the gutter.
//! 2. Merge the intervals; any hole between consecutive merged intervals
//!    wider than `min_gap` is a gutter.
//! 3. Use the gutter midpoints as column boundaries; each word joins the
//!    column its start X falls into.
//! 4. Columns are ordered left-to-right; words within a column keep
//!    top-to-bottom, left-to-right order.

use super::layout::TextWord;

/// Split a page's words into vertical columns.
///
/// `min_gap` is the minimum gutter width (in user-space units) that counts
/// as a column separator; a good default is `2 × body font size`.
///
/// Returns columns ordered left-to-right, each holding its words in
/// top-to-bottom then left-to-right order. A single-column page returns
/// one column.
pub fn split_columns(mut words: Vec<TextWord>, min_gap: f64) -> Vec<Vec<TextWord>> {
    if words.len() < 2 {
        return vec![words];
    }

    // Median word width; spanning elements (rules, headings) are outliers
    // and are excluded from gutter detection.
    let mut widths: Vec<f64> = words.iter().map(|w| w.width).collect();
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_w = widths[widths.len() / 2];

    let mut intervals: Vec<(f64, f64)> = words
        .iter()
        .filter(|w| w.width <= median_w * 1.5)
        .map(|w| (w.x, w.x + w.width))
        .collect();
    if intervals.len() < 2 {
        return vec![words];
    }
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // Merge intervals; holes wider than `min_gap` are gutters.
    let mut gutters: Vec<(f64, f64)> = Vec::new();
    let mut cur_end = intervals[0].1;
    for &(start, end) in intervals.iter().skip(1) {
        if start > cur_end + min_gap {
            gutters.push((cur_end, start));
        }
        cur_end = cur_end.max(end);
    }
    if gutters.is_empty() {
        return vec![words];
    }

    // Column boundaries: midpoints of the gutters.
    let bounds: Vec<f64> = gutters.iter().map(|(a, b)| (a + b) / 2.0).collect();

    // Assign each word to the column its start X falls into, then order
    // within the column: top-to-bottom (baseline descending), left-to-right.
    let mut columns: Vec<Vec<TextWord>> = vec![Vec::new(); bounds.len() + 1];
    for word in words.drain(..) {
        let idx = bounds
            .iter()
            .position(|&b| word.x < b)
            .unwrap_or(bounds.len());
        columns[idx].push(word);
    }
    for col in &mut columns {
        col.sort_by(|a, b| {
            b.y.partial_cmp(&a.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });
    }
    columns
}

/// Assemble the plain text of one column: words are clustered into lines by
/// baseline (within `font_size * 0.5`), joined by spaces; lines by `\n`.
pub fn column_text(words: &[TextWord]) -> String {
    let mut items: Vec<&TextWord> = words.iter().collect();
    items.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut lines: Vec<Vec<&TextWord>> = Vec::new();
    for word in items {
        let threshold = word.font_size * 0.5;
        let placed = lines
            .iter_mut()
            .find(|g| (word.y - g[0].y).abs() <= threshold);
        match placed {
            Some(group) => group.push(word),
            None => lines.push(vec![word]),
        }
    }
    lines
        .iter()
        .map(|g| {
            g.iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, x: f64, width: f64, y: f64) -> TextWord {
        TextWord {
            text: text.to_string(),
            x,
            y,
            width,
            font_size: 10.0,
            font_name: "F1".to_string(),
        }
    }

    #[test]
    fn single_column_page_returns_one_column() {
        let words = vec![
            word("one", 50.0, 60.0, 700.0),
            word("two", 120.0, 60.0, 700.0),
            word("three", 60.0, 80.0, 650.0),
        ];
        let cols = split_columns(words, 40.0);
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].len(), 3);
    }

    #[test]
    fn two_column_page_splits_at_gutter() {
        // Left column x ∈ [50, 250], right column x ∈ [350, 550].
        let words = vec![
            word("L1", 50.0, 40.0, 700.0),
            word("R1", 350.0, 40.0, 700.0),
            word("L2", 50.0, 38.0, 650.0),
            word("R2", 360.0, 38.0, 650.0),
        ];
        let cols = split_columns(words, 40.0);
        assert_eq!(cols.len(), 2);
        let left: Vec<&str> = cols[0].iter().map(|w| w.text.as_str()).collect();
        let right: Vec<&str> = cols[1].iter().map(|w| w.text.as_str()).collect();
        assert_eq!(left, vec!["L1", "L2"]);
        assert_eq!(right, vec!["R1", "R2"]);
    }

    #[test]
    fn wide_words_do_not_block_gutter_detection() {
        // A full-width rule (outlier width) would cover the gutter; it must
        // be skipped during detection but still assigned to a column.
        let words = vec![
            word("rule", 50.0, 500.0, 750.0), // outlier: 500 > 1.5 × ~40
            word("L1", 50.0, 40.0, 700.0),
            word("R1", 350.0, 40.0, 700.0),
        ];
        let cols = split_columns(words, 40.0);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].len(), 2); // rule + L1
        assert_eq!(cols[1].len(), 1); // R1
    }

    #[test]
    fn narrow_gaps_do_not_split() {
        let words = vec![
            word("one", 50.0, 60.0, 700.0),
            word("two", 120.0, 60.0, 650.0),
        ];
        let cols = split_columns(words, 40.0);
        assert_eq!(cols.len(), 1);
    }

    #[test]
    fn column_text_orders_lines_and_words() {
        let words = vec![
            word("second", 120.0, 60.0, 650.0),
            word("First", 50.0, 60.0, 700.0),
            word("line", 200.0, 60.0, 700.0),
        ];
        assert_eq!(column_text(&words), "First line\nsecond");
    }

    #[test]
    fn empty_input_yields_one_empty_column() {
        let cols = split_columns(Vec::new(), 40.0);
        assert_eq!(cols.len(), 1);
        assert!(cols[0].is_empty());
    }
}
