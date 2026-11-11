//! End-of-line dehyphenation for text extraction.
//!
//! Typeset text frequently breaks words across lines with a hyphen:
//!
//! ```text
//!    The quick brown fox
//!    jumps over the lazy
//!    dog.
//! ```
//! becomes, in real documents,
//!
//! ```text
//!    The quick brown fox
//!    jumps over the la-
//!    zy dog.
//! ```
//!
//! Line-based extraction yields `"... the la-\nzy dog."`. This module joins
//! such fragments back into whole words when assembling plain text.
//!
//! Heuristic (conservative, in the spirit of `pdftotext`):
//! - A line ending in a hard hyphen `-` or U+2010 HYPHEN joins the next line
//!   *only if* the next line starts with a lowercase letter (soft break).
//! - Soft hyphens (U+00AD) are always removed.
//! - The last line is never joined with anything.
//! - An uppercase next word, or a hyphen at a true line end (e.g. ranges
//!   like "pages 3-"), is left alone.

/// Characters that mark a breakable line-end hyphen.
const HYPHENS: [char; 2] = ['-', '\u{2010}'];

/// True if `c` is an ASCII lowercase letter.
fn is_lowercase(c: char) -> bool {
    c.is_ascii_lowercase()
}

/// Join hyphen-broken lines into whole-word lines.
///
/// Returns a new vector of line strings; joined pairs become one entry.
pub fn dehyphenate(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let merged = match out.last_mut() {
            Some(prev) if should_join(prev, line) => {
                // Strip the trailing hyphen (plus any whitespace before it)
                // and append the next line's text directly — no space.
                let mut base = prev
                    .trim_end()
                    .trim_end_matches(['-', '\u{2010}'])
                    .to_string();
                base.push_str(remove_soft_hyphens(line.trim_start()).as_str());
                *prev = base;
                true
            }
            _ => false,
        };
        if !merged {
            out.push(remove_soft_hyphens(line));
        }
    }
    out
}

/// Strip soft hyphens (U+00AD) from a line — they are pure formatting hints.
pub fn remove_soft_hyphens(line: &str) -> String {
    if line.contains('\u{00AD}') {
        line.replace('\u{00AD}', "")
    } else {
        line.to_string()
    }
}

/// Decide whether a line ending should join the following line.
///
/// Exposed for tests and for callers that assemble text incrementally.
pub fn should_join(prev_line: &str, next_line: &str) -> bool {
    let trimmed = prev_line.trim_end();
    let ends_hyphen = HYPHENS.iter().any(|h| trimmed.ends_with(*h));
    ends_hyphen
        && next_line
            .trim_start()
            .chars()
            .next()
            .is_some_and(is_lowercase)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphen_broken_word_is_joined() {
        let lines = vec![
            "The quick brown".to_string(),
            "fox jumps over the la-".to_string(),
            "zy dog.".to_string(),
        ];
        let out = dehyphenate(&lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], "fox jumps over the lazy dog.");
    }

    #[test]
    fn uppercase_continuation_is_not_joined() {
        // "3-" is a range dash followed by a proper noun — keep separate.
        let lines = vec![
            "see pages 12-".to_string(),
            "Berlin for details.".to_string(),
        ];
        let out = dehyphenate(&lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "see pages 12-");
        assert_eq!(out[1], "Berlin for details.");
    }

    #[test]
    fn soft_hyphen_is_always_removed() {
        assert_eq!(
            remove_soft_hyphens("inter\u{00AD}national"),
            "international"
        );
    }

    #[test]
    fn unicode_hyphen_joins_too() {
        let lines = vec!["con\u{2010}".to_string(), "text.".to_string()];
        let out = dehyphenate(&lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], "context.");
    }

    #[test]
    fn three_way_break_joins_twice() {
        let lines = vec![
            "an inter-".to_string(),
            "esting ex-".to_string(),
            "ample.".to_string(),
        ];
        let out = dehyphenate(&lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], "an interesting example.");
    }

    #[test]
    fn should_join_true_only_for_lowercase_continuation() {
        assert!(should_join("la-", "zy"));
        assert!(!should_join("la-", "Zy"));
        assert!(should_join("end-", "  ok"), "leading whitespace is skipped");
        assert!(!should_join("no hyphen", "here"));
    }

    #[test]
    fn last_line_never_creates_orphan_join() {
        let lines = vec!["ends with hyphen-".to_string()];
        let out = dehyphenate(&lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], "ends with hyphen-");
    }
}
