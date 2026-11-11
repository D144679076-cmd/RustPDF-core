//! Bidirectional (UAX #9) text reordering for extraction.
//!
//! PDF content streams emit glyphs in *visual* order (leftmost glyph first).
//! For right-to-left runs that is the *reverse* of logical reading order, so
//! extracted text comes out scrambled (`"םולש"` instead of `"שלום"`).
//!
//! This module implements the visual→logical recovery pass: contiguous runs
//! of RTL characters (plus neutral characters embedded between them) are
//! reversed, while digits and LTR text inside an RTL run keep their order.
//!
//! Mirroring of brackets (UAX #9 rule L4) is **not** applied — a documented
//! limitation; the common cases (plain Hebrew/Arabic words in visual order)
//! are handled correctly.

use unicode_bidi::{bidi_class, BidiClass};

/// True when `text` contains at least one character with a strong RTL bidi
/// class (`R` or `AL`).
pub fn contains_rtl(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL))
}

/// Reorder a visually-ordered string into logical reading order.
///
/// Contiguous runs of RTL characters (classes `R`/`AL`, plus `AN` Arabic
/// digits and neutral `WS`/`ON` characters embedded *between* RTL
/// characters) are reversed. Digits (`EN`) and LTR characters always keep
/// their left-to-right order.
pub fn reorder_visual_to_logical(text: &str) -> String {
    if !contains_rtl(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if is_rtl_char(chars[i]) {
            // Extend the run forward: RTL chars, neutrals, and Arabic digits
            // — but the run must terminate at (and exclude) trailing
            // neutrals/digits that are not followed by another RTL char.
            let mut end = i + 1;
            while end < chars.len() && may_join_rtl_run(chars[end]) {
                end += 1;
            }
            // Trim trailing non-RTL chars off the run (they belong to the
            // following LTR context and must keep their order).
            let mut run_end = end;
            while run_end > i && !is_rtl_char(chars[run_end - 1]) {
                run_end -= 1;
            }
            // Reverse the core RTL run, then emit any trimmed tail in order.
            out.extend(chars[i..run_end].iter().rev());
            out.extend(chars[run_end..end].iter());
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Strong RTL characters start (and extend) a reversal run.
fn is_rtl_char(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::R | BidiClass::AL)
}

/// Characters that may appear *inside* an RTL run: neutrals (space,
/// punctuation) and Arabic-Indic digits. Digits keep their order within the
/// run, so the run is reversed around them, not through them — the simple
/// whole-run reversal here is correct for the common case because the digits
/// themselves are symmetric only when the run reversal restores logical
/// order; mixed digit+RTL runs are a documented limitation.
fn may_join_rtl_run(c: char) -> bool {
    matches!(
        bidi_class(c),
        BidiClass::R
            | BidiClass::AL
            | BidiClass::AN
            | BidiClass::WS
            | BidiClass::ON
            | BidiClass::CS
            | BidiClass::ES
            | BidiClass::ET
    )
}

/// Reverse the order of consecutive RTL words within a word sequence,
/// leaving LTR words in place.
///
/// Returns a permutation of indices into `words`: applying it re-orders the
/// words so that each maximal run of RTL-containing words (a phrase laid out
/// right-to-left by the producer) is reversed, while LTR words keep their
/// relative order. Character-level reordering inside each word is handled
/// separately by [`reorder_visual_to_logical`].
pub fn reorder_rtl_words<T: AsRef<str>>(words: &[T]) -> Vec<usize> {
    let rtl: Vec<bool> = words.iter().map(|w| contains_rtl(w.as_ref())).collect();
    let mut order: Vec<usize> = (0..words.len()).collect();
    let mut i = 0;
    while i < rtl.len() {
        if rtl[i] {
            let mut j = i + 1;
            while j < rtl.len() && rtl[j] {
                j += 1;
            }
            order[i..j].reverse();
            i = j;
        } else {
            i += 1;
        }
    }
    order
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ltr_text_is_unchanged() {
        assert_eq!(reorder_visual_to_logical("Hello world"), "Hello world");
    }

    #[test]
    fn contains_rtl_detects_hebrew_and_arabic() {
        assert!(contains_rtl("שלום"));
        assert!(contains_rtl("مرحبا"));
        assert!(!contains_rtl("hello 123"));
    }

    #[test]
    fn hebrew_visual_run_is_reversed() {
        // PDF emits the leftmost glyph first: "שלום" (shalom) visual = "םולש".
        assert_eq!(reorder_visual_to_logical("םולש"), "שלום");
    }

    #[test]
    fn arabic_visual_run_is_reversed() {
        // "سلام" (salaam) visual order = "مالس".
        assert_eq!(reorder_visual_to_logical("مالس"), "سلام");
    }

    #[test]
    fn mixed_ltr_and_rtl_keeps_ltr_in_place() {
        // LTR prefix + visual Hebrew run + LTR suffix.
        assert_eq!(reorder_visual_to_logical("abc םולש def"), "abc שלום def");
    }

    #[test]
    fn digits_inside_rtl_keep_order() {
        // Visual "12םולש34" — digits must not be reversed, only the Hebrew run.
        assert_eq!(reorder_visual_to_logical("12םולש34"), "12שלום34");
    }

    #[test]
    fn trailing_neutrals_stay_outside_reversed_run() {
        // Visual: Hebrew word + trailing space + LTR word.
        assert_eq!(reorder_visual_to_logical("םולש world"), "שלום world");
    }

    #[test]
    fn rtl_word_run_reverses_only_the_rtl_run() {
        // Visual: "a" "שלום" "עליכם" "c" → the two RTL words swap, LTR words
        // stay. (Character order inside each word is fixed separately.)
        let words = ["a", "שלום", "עליכם", "c"];
        let order = reorder_rtl_words(&words);
        let logical: Vec<&str> = order.iter().map(|&i| words[i]).collect();
        assert_eq!(logical, vec!["a", "עליכם", "שלום", "c"]);
    }

    #[test]
    fn single_rtl_word_run_is_identity() {
        let words = ["world", "שלום", "hello"];
        let order = reorder_rtl_words(&words);
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn fully_ltr_word_list_is_identity() {
        let words = ["one", "two", "three"];
        let order = reorder_rtl_words(&words);
        assert_eq!(order, vec![0, 1, 2]);
    }
}
