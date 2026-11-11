//! Text extractor: collects [`TextSpan`] events and groups them into lines/words.
//!
//! Usage:
//! ```rust,ignore
//! let extractor = TextExtractor::extract_from_page(&doc, &page)?;
//! println!("{}", extractor.plain_text());
//! ```
//!
//! [`TextSpan`]: crate::content::text_state::TextSpan

use crate::content::graphics_state::GraphicsState;
use crate::content::interpreter::{ContentInterpreter, OutputDevice};
use crate::content::text_state::TextSpan;
use crate::document::page::Page;
use crate::error::Result;
use crate::parser::objects::PdfDocument;

use super::layout::{TextLine, TextWord};
/// Collects text spans from a page and groups them into lines and words.
pub struct TextExtractor {
    spans: Vec<TextSpan>,
}

impl OutputDevice for TextExtractor {
    fn draw_text_span(&mut self, span: &TextSpan, _state: &GraphicsState) {
        if !span.text.is_empty() {
            self.spans.push(span.clone());
        }
    }

    fn stroke_path(
        &mut self,
        _path: &crate::content::graphics_state::Path,
        _state: &GraphicsState,
    ) {
    }

    fn fill_path(
        &mut self,
        _path: &crate::content::graphics_state::Path,
        _state: &GraphicsState,
        _rule: crate::content::graphics_state::FillRule,
    ) {
    }

    fn draw_image(&mut self, _image_data: &[u8], _state: &GraphicsState) {}
}

impl TextExtractor {
    /// Create a new empty extractor.
    pub fn new() -> Self {
        TextExtractor { spans: Vec::new() }
    }

    /// Run the interpreter on a page's content and collect all text spans.
    ///
    /// Uses `interpret_with_doc` so that Form XObjects embedded in the page are
    /// also traversed and their text is included.
    pub fn extract_from_page(doc: &PdfDocument, page: &Page) -> Result<Self> {
        let content = page.decode_contents(doc)?;
        let mut extractor = TextExtractor::new();
        let mut interp = ContentInterpreter::new();
        // Pass the full resources dict (not just fonts) so XObjects and
        // indirect font references can be resolved during interpretation.
        interp.interpret_with_doc(&content, &mut extractor, doc, &page.resources.raw)?;
        Ok(extractor)
    }

    /// Group the collected spans into text lines (sorted top-to-bottom).
    ///
    /// Spans with baselines within `font_size * 0.5` of each other are merged
    /// into the same line.  Within each line, spans closer than `font_size * 0.3`
    /// in X are merged into a word; wider gaps start a new word.
    pub fn into_lines(self) -> Vec<TextLine> {
        if self.spans.is_empty() {
            return Vec::new();
        }

        // Sort by y descending (PDF: higher y = higher on page), then x ascending.
        let mut spans = self.spans;
        spans.sort_by(|a, b| {
            b.y.partial_cmp(&a.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });

        // Cluster spans into lines.
        let mut line_groups: Vec<Vec<TextSpan>> = Vec::new();
        for span in spans {
            let threshold = span.font_size * 0.5;
            let placed = line_groups.iter_mut().find(|g| {
                let baseline = g[0].y;
                (span.y - baseline).abs() <= threshold
            });
            match placed {
                Some(group) => group.push(span),
                None => line_groups.push(vec![span]),
            }
        }

        // Sort each line's spans left-to-right, then build words.
        let mut lines: Vec<TextLine> = line_groups
            .into_iter()
            .map(|mut group| {
                group.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
                build_line(group)
            })
            .collect();

        // Sort lines top-to-bottom (descending baseline_y).
        lines.sort_by(|a, b| {
            b.baseline_y
                .partial_cmp(&a.baseline_y)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        lines
    }

    /// Return all words across all lines in reading order.
    pub fn words(&self) -> Vec<TextWord> {
        TextExtractor {
            spans: self.spans.clone(),
        }
        .into_lines()
        .into_iter()
        .flat_map(|l| l.words)
        .collect()
    }

    /// Plain UTF-8 text: lines separated by `\n`, words by ` `.
    pub fn plain_text(&self) -> String {
        TextExtractor {
            spans: self.spans.clone(),
        }
        .into_lines()
        .iter()
        .map(|l| l.text())
        .collect::<Vec<_>>()
        .join("\n")
    }

    /// Split the page's words into vertical columns and return them
    /// (see [`columns::split_columns`] for the gutter algorithm).
    pub fn into_columns(self, min_gap: f64) -> Vec<Vec<TextWord>> {
        super::columns::split_columns(self.words(), min_gap)
    }

    /// Plain text in column-aware reading order: columns left-to-right,
    /// lines top-to-bottom within each column.
    pub fn plain_text_columns(&self, min_gap: f64) -> String {
        TextExtractor {
            spans: self.spans.clone(),
        }
        .into_columns(min_gap)
        .iter()
        .map(|col| super::columns::column_text(col))
        .collect::<Vec<_>>()
        .join("\n")
    }

    /// Plain text with end-of-line hyphenation repaired (see
    /// [`dehyphenate::dehyphenate`]).
    pub fn plain_text_dehyphenated(&self) -> String {
        let lines: Vec<String> = TextExtractor {
            spans: self.spans.clone(),
        }
        .into_lines()
        .iter()
        .map(|l| l.text())
        .collect();
        super::dehyphenate::dehyphenate(&lines).join("\n")
    }

    /// Lines built from rotated text runs (90°/180°/270°), grouped per
    /// orientation. Horizontal (0°) text is *not* included — use
    /// [`TextExtractor::into_lines`] for the normal flow.
    pub fn rotated_lines(&self) -> Vec<(u16, Vec<TextLine>)> {
        super::rotated::group_rotated(&self.spans)
    }
}

impl Default for TextExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a `TextLine` from a left-to-right sorted slice of spans.
///
/// Consecutive spans are merged into the same word when the gap between them
/// is less than `prev.font_size * 0.3`; otherwise a new word begins.
///
/// Bidirectional fix-up (see [`bidi::reorder_visual_to_logical`]): PDFs emit
/// glyphs in visual order, so RTL character runs inside each word are
/// reversed into logical order, and runs of RTL words are re-sequenced.
fn build_line(spans: Vec<TextSpan>) -> TextLine {
    debug_assert!(!spans.is_empty());

    let baseline_y = spans[0].y;
    let line_x = spans[0].x;
    let mut words: Vec<TextWord> = Vec::new();

    for span in &spans {
        let gap_threshold = span.font_size * 0.3;
        let merge = words.last().is_some_and(|prev: &TextWord| {
            let gap = span.x - (prev.x + prev.width);
            gap < gap_threshold
        });

        if merge {
            let word = words.last_mut().unwrap();
            word.text.push_str(&span.text);
            word.width = (span.x + span.width) - word.x;
        } else {
            words.push(TextWord {
                text: span.text.clone(),
                x: span.x,
                y: span.y,
                width: span.width,
                font_size: span.font_size,
                font_name: span.font_name.clone(),
            });
        }
    }

    // Visual→logical bidi recovery: per-word character runs first, then
    // word order for RTL phrases (which are laid out right-to-left).
    for word in &mut words {
        word.text = super::bidi::reorder_visual_to_logical(&word.text);
    }
    let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let order = super::bidi::reorder_rtl_words(&texts);
    let words: Vec<TextWord> = order.into_iter().map(|i| words[i].clone()).collect();

    let last = spans.last().unwrap();
    let line_width = (last.x + last.width) - line_x;

    TextLine {
        words,
        baseline_y,
        x: line_x,
        width: line_width,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, x: f64, y: f64, width: f64, font_size: f64) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width,
            font_size,
            font_size_px: font_size,
            font_name: "F1".to_string(),
            char_advances: vec![],
            char_advances_y: vec![],
            char_cids: vec![],
            is_type3: false,
            render_matrix_2x2: [1.0, 0.0, 0.0, -1.0],
            stroke_text: false,
        }
    }

    #[test]
    fn test_extract_empty_page() {
        let extractor = TextExtractor::new();
        let lines = extractor.into_lines();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_extract_single_span() {
        let mut extractor = TextExtractor::new();
        extractor.spans.push(span("Hello", 10.0, 700.0, 50.0, 12.0));
        let lines = extractor.into_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "Hello");
    }

    #[test]
    fn test_extract_two_lines() {
        let mut extractor = TextExtractor::new();
        // Two spans far apart in y — different lines.
        extractor.spans.push(span("First", 10.0, 700.0, 40.0, 12.0));
        extractor
            .spans
            .push(span("Second", 10.0, 650.0, 45.0, 12.0));
        let lines = extractor.into_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words[0].text, "First");
        assert_eq!(lines[1].words[0].text, "Second");
    }

    #[test]
    fn test_extract_word_grouping() {
        let mut extractor = TextExtractor::new();
        // Two spans close together → same word.
        extractor.spans.push(span("Hel", 10.0, 700.0, 18.0, 12.0));
        extractor.spans.push(span("lo", 28.0, 700.0, 12.0, 12.0)); // gap = 0 < 3.6
                                                                   // Span far to the right → new word.
        extractor.spans.push(span("World", 80.0, 700.0, 40.0, 12.0)); // gap = 40 > 3.6
        let lines = extractor.into_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].text, "Hello");
        assert_eq!(lines[0].words[1].text, "World");
    }

    #[test]
    fn test_plain_text_output() {
        let mut extractor = TextExtractor::new();
        extractor.spans.push(span("Hello", 10.0, 700.0, 40.0, 12.0));
        extractor.spans.push(span("World", 70.0, 700.0, 40.0, 12.0));
        extractor.spans.push(span("Line2", 10.0, 650.0, 40.0, 12.0));
        let text = extractor.plain_text();
        assert_eq!(text, "Hello World\nLine2");
    }

    #[test]
    fn test_reading_order() {
        let mut extractor = TextExtractor::new();
        // Provide spans in wrong order — extractor must sort them.
        extractor
            .spans
            .push(span("Bottom", 10.0, 100.0, 50.0, 12.0));
        extractor.spans.push(span("Top", 10.0, 700.0, 30.0, 12.0));
        extractor
            .spans
            .push(span("Middle", 10.0, 400.0, 55.0, 12.0));
        let lines = extractor.into_lines();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].words[0].text, "Top");
        assert_eq!(lines[1].words[0].text, "Middle");
        assert_eq!(lines[2].words[0].text, "Bottom");
    }

    #[test]
    fn rtl_visual_text_comes_out_logical() {
        // Hebrew "שלום" emitted visually (leftmost glyph first) in one span.
        let mut extractor = TextExtractor::new();
        extractor.spans.push(span("םולש", 10.0, 700.0, 40.0, 12.0));
        let text = extractor.plain_text();
        assert_eq!(text, "שלום");
    }

    #[test]
    fn rtl_word_order_is_reversed_across_words() {
        // Visual X order: "world" left, Hebrew word middle-right, "hello" right.
        let mut extractor = TextExtractor::new();
        extractor.spans.push(span("world", 10.0, 700.0, 40.0, 12.0));
        extractor.spans.push(span("םולש", 200.0, 700.0, 40.0, 12.0));
        extractor
            .spans
            .push(span("hello", 400.0, 700.0, 40.0, 12.0));
        let text = extractor.plain_text();
        // RTL word runs reverse: the two RTL-adjacent... "hello" has no RTL,
        // so only the Hebrew run (one word) stays; LTR words keep order.
        assert_eq!(text, "world שלום hello");
    }

    #[test]
    fn columns_split_reading_order() {
        let mut extractor = TextExtractor::new();
        // Left column lines (x ≈ 50) and right column lines (x ≈ 350),
        // interleaved in collection order.
        extractor.spans.push(span("L1", 50.0, 700.0, 200.0, 12.0));
        extractor.spans.push(span("R1", 350.0, 700.0, 200.0, 12.0));
        extractor.spans.push(span("L2", 50.0, 650.0, 190.0, 12.0));
        extractor.spans.push(span("R2", 350.0, 650.0, 190.0, 12.0));
        let text = extractor.plain_text_columns(40.0);
        assert_eq!(text, "L1\nL2\nR1\nR2");
    }

    #[test]
    fn plain_text_columns_no_gutter_is_normal_order() {
        let mut extractor = TextExtractor::new();
        extractor.spans.push(span("one", 50.0, 700.0, 200.0, 12.0));
        extractor.spans.push(span("two", 55.0, 650.0, 190.0, 12.0));
        let text = extractor.plain_text_columns(40.0);
        assert_eq!(text, "one\ntwo");
    }

    #[test]
    fn dehyphenated_plain_text_joins_broken_word() {
        let mut extractor = TextExtractor::new();
        extractor
            .spans
            .push(span("over the la-", 10.0, 700.0, 80.0, 12.0));
        extractor
            .spans
            .push(span("zy dog.", 10.0, 650.0, 60.0, 12.0));
        let text = extractor.plain_text_dehyphenated();
        assert_eq!(text, "over the lazy dog.");
    }

    #[test]
    fn rotated_spans_group_separately() {
        let vertical = |text: &str, x: f64, y: f64| {
            let mut s = span(text, x, y, 24.0, 12.0);
            // 90° advance direction (0, 1) in user space.
            s.render_matrix_2x2 = [0.0, 1.0, 1.0, 0.0];
            s
        };
        // Horizontal line — goes through the normal flow only.
        let mut horizontal = TextExtractor::new();
        horizontal.spans.push(span("flat", 10.0, 700.0, 30.0, 12.0));
        // 90° run: two spans stacked along -x (consecutive vertical lines).
        let mut rotated_ex = TextExtractor::new();
        rotated_ex.spans.push(vertical("UP1", 100.0, 10.0));
        rotated_ex.spans.push(vertical("UP2", 100.0, 40.0));
        let rotated = rotated_ex.rotated_lines();
        assert_eq!(rotated.len(), 1);
        assert_eq!(rotated[0].0, 90);
        assert_eq!(rotated[0].1.len(), 1);
        assert_eq!(rotated[0].1[0].words[0].text, "UP1UP2");
        // Horizontal text unaffected.
        assert_eq!(horizontal.plain_text(), "flat");
    }
}
