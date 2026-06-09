//! Text layout types: word, line, block.
//!
//! These are the output structures produced by [`TextExtractor`] after grouping
//! raw [`TextSpan`] events collected from a page content stream.
//!
//! [`TextExtractor`]: super::extractor::TextExtractor
//! [`TextSpan`]: crate::content::text_state::TextSpan

/// A single word with its position and font metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct TextWord {
    /// The Unicode text of this word (no trailing whitespace).
    pub text: String,
    /// X coordinate of the word's left edge in user space.
    pub x: f64,
    /// Y coordinate of the word's baseline in user space.
    pub y: f64,
    /// Total rendered width of the word in user space.
    pub width: f64,
    /// Font size of the first span that contributed to this word.
    pub font_size: f64,
    /// Font resource name of the first span in this word (e.g. `"F1"`).
    pub font_name: String,
}

/// A line of text consisting of one or more words on the same baseline.
#[derive(Debug, Clone)]
pub struct TextLine {
    /// Words in reading order (left to right).
    pub words: Vec<TextWord>,
    /// Y coordinate of the shared baseline in user space.
    pub baseline_y: f64,
    /// X coordinate of the line's leftmost edge.
    pub x: f64,
    /// Total width of the line (from left edge to right edge of last word).
    pub width: f64,
}

impl TextLine {
    /// Plain text of the line with words joined by a single space.
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A rectangular block of text made up of one or more lines.
#[derive(Debug, Clone)]
pub struct TextBlock {
    /// Lines in top-to-bottom reading order.
    pub lines: Vec<TextLine>,
}

impl TextBlock {
    /// Plain text of the block with lines joined by `\n`.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
