//! Stateful single-line text-edit engine (caret, selection, insert/delete).
//!
//! The engine owns a character buffer plus a caret and optional selection
//! anchor, and exposes Word-style editing operations. Caret geometry (pixel x,
//! click hit-testing) is delegated to [`crate::editor::text_shape`] via a
//! [`Measurer`], so the engine is independent of any specific font.
//!
//! Phase 1 is single-line; multi-line reflow lands in a later phase.

use crate::editor::text_shape::{caret_offsets, hit_test, Measurer};
use crate::editor::text_style::{ActiveStyle, Align, CharStyle, FontChoice, StyleRun};

/// Caret movement direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
}

/// A live single-line rich-text editor over a character buffer.
///
/// `styles` is kept length-locked with `chars` (invariant
/// `styles.len() == chars.len()`): every splice on `chars` applies the same
/// splice on `styles`, so selection-scoped formatting stays in sync by
/// construction. `typing_style` is the style newly inserted characters inherit.
pub struct TextEditEngine {
    /// Buffer as Unicode scalars; caret/selection indices address this vector.
    chars: Vec<char>,
    /// Per-character style, one entry per `chars` element (length-locked).
    styles: Vec<CharStyle>,
    /// Caret position in `0..=chars.len()` (a gap between characters).
    caret: usize,
    /// Selection anchor; `Some(a)` means the range `[min(a,caret), max(a,caret))`
    /// is selected. `None` means no selection.
    anchor: Option<usize>,
    /// Style applied to the next inserted character(s); also the "active" style
    /// reported to the panel when there is no selection.
    typing_style: CharStyle,
    /// Block-level paragraph alignment.
    align: Align,
}

impl TextEditEngine {
    /// Create an engine seeded with `text`, caret at the end, using a default
    /// (size-0 black) style. Kept for tests/back-compat; real callers use
    /// [`new_styled`](Self::new_styled) so caret geometry and formatting use the
    /// block's font size.
    pub fn new(text: &str) -> Self {
        Self::new_styled(text, CharStyle::from_block(0.0))
    }

    /// Create an engine seeded with `text` and a baseline `base` style applied to
    /// every seed character (and used as the initial typing style).
    pub fn new_styled(text: &str, base: CharStyle) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let caret = chars.len();
        let styles = vec![base.clone(); chars.len()];
        Self {
            chars,
            styles,
            caret,
            anchor: None,
            typing_style: base,
            align: Align::Left,
        }
    }

    /// Current buffer contents.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Number of characters in the buffer.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Current caret index (character offset).
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Active selection as `(start, end)` character indices, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        match self.anchor {
            Some(a) if a != self.caret => Some((a.min(self.caret), a.max(self.caret))),
            _ => None,
        }
    }

    /// Set the caret to `idx` (clamped), optionally extending the selection.
    pub fn set_caret(&mut self, idx: usize, extend: bool) {
        let idx = idx.min(self.chars.len());
        self.update_anchor(extend);
        self.caret = idx;
        if !extend {
            self.anchor = None;
        }
        if self.selection().is_none() {
            self.refresh_typing_style();
        }
    }

    /// Place the caret nearest to local x-offset `x` (from the text origin).
    pub fn click(&mut self, m: &dyn Measurer, x: f64, extend: bool) {
        let text = self.text();
        let idx = hit_test(m, &text, x);
        self.set_caret(idx, extend);
    }

    /// Select the entire buffer (anchor at 0, caret at end).
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.chars.len();
    }

    /// Select the "word" under character index `idx` (double-click): the maximal
    /// run of same-class characters (non-whitespace word, or whitespace run)
    /// containing `idx`. Sets the selection to that `[start, end)`. `idx` is
    /// clamped into range; no-op on an empty buffer.
    pub fn select_word_at(&mut self, idx: usize) {
        let n = self.chars.len();
        if n == 0 {
            return;
        }
        let i = idx.min(n - 1);
        let word = !self.chars[i].is_whitespace();
        let mut s = i;
        while s > 0 && self.chars[s - 1].is_whitespace() != word {
            s -= 1;
        }
        let mut e = i + 1;
        while e < n && self.chars[e].is_whitespace() != word {
            e += 1;
        }
        self.anchor = Some(s);
        self.caret = e;
    }

    /// Insert `s` at the caret, replacing any active selection. New characters
    /// inherit the current [`typing_style`](Self::typing_style).
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let ins: Vec<char> = s.chars().collect();
        let n = ins.len();
        let st = self.typing_style.clone();
        self.chars.splice(self.caret..self.caret, ins);
        self.styles.splice(self.caret..self.caret, vec![st; n]);
        self.caret += n;
        self.anchor = None;
        self.debug_assert_lens();
    }

    /// Delete the selection if any, else the character before the caret.
    pub fn delete_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.caret > 0 {
            self.caret -= 1;
            self.chars.remove(self.caret);
            self.styles.remove(self.caret);
            self.refresh_typing_style();
        }
        self.debug_assert_lens();
    }

    /// Delete the selection if any, else the character after the caret.
    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.caret < self.chars.len() {
            self.chars.remove(self.caret);
            self.styles.remove(self.caret);
        }
        self.debug_assert_lens();
    }

    /// Clear the entire buffer: removes all characters and styles, resets the
    /// caret to 0, and clears any selection.
    pub fn delete_all(&mut self) {
        self.chars.clear();
        self.styles.clear();
        self.caret = 0;
        self.anchor = None;
        self.refresh_typing_style();
        self.debug_assert_lens();
    }

    /// Move the caret one character left/right, optionally extending selection.
    ///
    /// With no selection extension, a left/right move over an existing selection
    /// collapses it to the appropriate edge (Word/standard behaviour).
    pub fn move_caret(&mut self, dir: Dir, extend: bool) {
        if !extend {
            if let Some((s, e)) = self.selection() {
                self.caret = if dir == Dir::Left { s } else { e };
                self.anchor = None;
                return;
            }
        }
        self.update_anchor(extend);
        match dir {
            Dir::Left => {
                if self.caret > 0 {
                    self.caret -= 1;
                }
            }
            Dir::Right => {
                if self.caret < self.chars.len() {
                    self.caret += 1;
                }
            }
        }
        if !extend {
            self.anchor = None;
        }
        if self.selection().is_none() {
            self.refresh_typing_style();
        }
    }

    /// Move the caret to the start of the line.
    pub fn home(&mut self, extend: bool) {
        self.set_caret(0, extend);
    }

    /// Move the caret to the end of the line.
    pub fn end(&mut self, extend: bool) {
        self.set_caret(self.chars.len(), extend);
    }

    /// Pixel x-offset of the caret from the text origin, in user-space points.
    pub fn caret_x(&self, m: &dyn Measurer) -> f64 {
        let offsets = caret_offsets(m, &self.text());
        offsets.get(self.caret).copied().unwrap_or(0.0)
    }

    /// Selection bounds as pixel x-offsets `(start_x, end_x)`, if a selection
    /// is active. Useful for drawing the selection highlight.
    pub fn selection_x(&self, m: &dyn Measurer) -> Option<(f64, f64)> {
        let (s, e) = self.selection()?;
        let offsets = caret_offsets(m, &self.text());
        Some((offsets[s], offsets[e]))
    }

    // ── Formatting ────────────────────────────────────────────────────────────

    /// Block-level paragraph alignment.
    pub fn align(&self) -> Align {
        self.align
    }

    /// Set the block's paragraph alignment.
    pub fn set_align(&mut self, a: Align) {
        self.align = a;
    }

    /// Apply a fill colour to the selection, or set the pending typing colour
    /// when there is no selection.
    pub fn apply_color(&mut self, rgb: [f64; 3]) {
        self.mutate_range(|s| s.color = rgb);
    }

    /// Set the font family for the selection (or pending typing style).
    pub fn set_font(&mut self, family: &str) {
        let choice = FontChoice::Family(family.to_owned());
        self.mutate_range(|s| s.font = choice.clone());
    }

    /// Set the font size (points) for the selection (or pending typing style).
    pub fn set_size(&mut self, size: f64) {
        self.mutate_range(|s| s.font_size = size);
    }

    /// Toggle bold for the selection (Word semantics: clear only when the whole
    /// selection is already bold, else set all). No selection → flip the pending
    /// typing style.
    pub fn toggle_bold(&mut self) {
        let on = !self.selection_all(|c| c.bold);
        self.mutate_range(|s| s.bold = on);
    }

    /// Toggle italic for the selection (or pending typing style).
    pub fn toggle_italic(&mut self) {
        let on = !self.selection_all(|c| c.italic);
        self.mutate_range(|s| s.italic = on);
    }

    /// Toggle underline for the selection (or pending typing style).
    pub fn toggle_underline(&mut self) {
        let on = !self.selection_all(|c| c.underline);
        self.mutate_range(|s| s.underline = on);
    }

    /// Toggle strikethrough for the selection (or pending typing style).
    pub fn toggle_strike(&mut self) {
        let on = !self.selection_all(|c| c.strike);
        self.mutate_range(|s| s.strike = on);
    }

    /// Coalesce the per-char styles into maximal equal-style runs over the whole
    /// buffer. Empty buffer → empty vec. Consumed by commit and live preview.
    pub fn style_runs(&self) -> Vec<StyleRun> {
        let mut runs: Vec<StyleRun> = Vec::new();
        let mut i = 0usize;
        while i < self.styles.len() {
            let start = i;
            i += 1;
            while i < self.styles.len() && self.styles[i] == self.styles[start] {
                i += 1;
            }
            runs.push(StyleRun {
                start,
                end: i,
                style: self.styles[start].clone(),
            });
        }
        runs
    }

    /// Overwrite the per-character styles from a run sequence, then resync the
    /// typing style from the caret. Used to restore a block's committed
    /// formatting (e.g. underline/strike) when it is reopened within the same
    /// edit session — the seed style alone carries only intrinsic bold/italic,
    /// so without this a committed decoration would not preview live on reopen.
    ///
    /// Runs are clamped to the current buffer length; runs past the end are
    /// ignored (the buffer is authoritative for length).
    pub fn apply_style_runs(&mut self, runs: &[StyleRun]) {
        for run in runs {
            let end = run.end.min(self.styles.len());
            for i in run.start..end {
                self.styles[i] = run.style.clone();
            }
        }
        self.refresh_typing_style();
        self.debug_assert_lens();
    }

    /// Resolved style of the current selection (or the pending typing style when
    /// there is no selection). A field is `None` when the selection spans
    /// multiple values ("mixed"). `align` is always concrete.
    pub fn active_style(&self) -> ActiveStyle {
        let range: &[CharStyle] = match self.selection() {
            Some((s, e)) => &self.styles[s..e],
            None => std::slice::from_ref(&self.typing_style),
        };
        let mut a = ActiveStyle {
            align: self.align,
            ..ActiveStyle::default()
        };
        let Some(first) = range.first() else {
            return a; // empty buffer with empty selection: report only align
        };
        let uniform =
            |pred: &dyn Fn(&CharStyle, &CharStyle) -> bool| range.iter().all(|c| pred(c, first));
        if uniform(&|c, f| c.color == f.color) {
            a.color = Some(first.color);
        }
        if uniform(&|c, f| c.font == f.font) {
            a.font = Some(first.font.clone());
        }
        if uniform(&|c, f| c.font_size == f.font_size) {
            a.font_size = Some(first.font_size);
        }
        if uniform(&|c, f| c.bold == f.bold) {
            a.bold = Some(first.bold);
        }
        if uniform(&|c, f| c.italic == f.italic) {
            a.italic = Some(first.italic);
        }
        if uniform(&|c, f| c.underline == f.underline) {
            a.underline = Some(first.underline);
        }
        if uniform(&|c, f| c.strike == f.strike) {
            a.strike = Some(first.strike);
        }
        a
    }

    // ── Internal ────────────────────────────────────────────────────────────

    /// Ensure an anchor exists when starting to extend a selection.
    fn update_anchor(&mut self, extend: bool) {
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
    }

    /// Remove the selected range if one is active; returns whether it deleted.
    fn delete_selection(&mut self) -> bool {
        if let Some((s, e)) = self.selection() {
            self.chars.drain(s..e);
            self.styles.drain(s..e);
            self.caret = s;
            self.anchor = None;
            self.refresh_typing_style();
            true
        } else {
            false
        }
    }

    /// Apply `f` to every selected char's style (and the typing style), or only
    /// the typing style when there is no selection.
    fn mutate_range(&mut self, f: impl Fn(&mut CharStyle)) {
        match self.selection() {
            Some((s, e)) => {
                for st in &mut self.styles[s..e] {
                    f(st);
                }
                f(&mut self.typing_style);
            }
            None => f(&mut self.typing_style),
        }
    }

    /// Whether `pred` holds for the whole (non-empty) selection, or for the
    /// typing style when there is no selection.
    fn selection_all(&self, pred: impl Fn(&CharStyle) -> bool) -> bool {
        match self.selection() {
            Some((s, e)) => self.styles[s..e].iter().all(&pred),
            None => pred(&self.typing_style),
        }
    }

    /// Recompute the typing style from the char left of the caret (else the char
    /// at the caret), so typing continues the adjacent run's style. No-op on an
    /// empty buffer (keeps the existing typing style).
    fn refresh_typing_style(&mut self) {
        if self.caret > 0 {
            if let Some(s) = self.styles.get(self.caret - 1) {
                self.typing_style = s.clone();
                return;
            }
        }
        if let Some(s) = self.styles.get(self.caret) {
            self.typing_style = s.clone();
        }
    }

    /// Debug-only check that the style buffer stays length-locked with `chars`.
    #[inline]
    fn debug_assert_lens(&self) {
        debug_assert_eq!(
            self.chars.len(),
            self.styles.len(),
            "styles must stay length-locked with chars"
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::text_shape::Measurer;

    struct Mono(f64);
    impl Measurer for Mono {
        fn advance(&self, _ch: char) -> f64 {
            self.0
        }
    }

    #[test]
    fn new_places_caret_at_end() {
        let e = TextEditEngine::new("hello");
        assert_eq!(e.caret(), 5);
        assert_eq!(e.text(), "hello");
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn insert_at_caret() {
        let mut e = TextEditEngine::new("ad");
        e.set_caret(1, false);
        e.insert("bc");
        assert_eq!(e.text(), "abcd");
        assert_eq!(e.caret(), 3);
    }

    #[test]
    fn insert_unicode_counts_scalars() {
        let mut e = TextEditEngine::new("");
        e.insert("héllo");
        assert_eq!(e.caret(), 5);
        assert_eq!(e.text(), "héllo");
    }

    #[test]
    fn backspace_removes_char_before_caret() {
        let mut e = TextEditEngine::new("abc");
        e.set_caret(2, false);
        e.delete_back();
        assert_eq!(e.text(), "ac");
        assert_eq!(e.caret(), 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut e = TextEditEngine::new("abc");
        e.home(false);
        e.delete_back();
        assert_eq!(e.text(), "abc");
        assert_eq!(e.caret(), 0);
    }

    #[test]
    fn delete_forward_removes_char_after_caret() {
        let mut e = TextEditEngine::new("abc");
        e.home(false);
        e.delete_forward();
        assert_eq!(e.text(), "bc");
        assert_eq!(e.caret(), 0);
    }

    #[test]
    fn delete_all_clears_buffer() {
        let mut e = TextEditEngine::new("hello world");
        e.delete_all();
        assert!(e.text().is_empty());
        assert_eq!(e.caret(), 0);
        assert!(e.selection().is_none());
    }

    #[test]
    fn delete_all_on_empty_is_noop() {
        let mut e = TextEditEngine::new("");
        e.delete_all();
        assert!(e.text().is_empty());
        assert_eq!(e.caret(), 0);
    }

    #[test]
    fn selection_then_insert_replaces() {
        let mut e = TextEditEngine::new("abcd");
        e.set_caret(1, false);
        e.set_caret(3, true); // select "bc"
        assert_eq!(e.selection(), Some((1, 3)));
        e.insert("X");
        assert_eq!(e.text(), "aXd");
        assert_eq!(e.caret(), 2);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn selection_then_backspace_deletes_range() {
        let mut e = TextEditEngine::new("abcd");
        e.select_all();
        e.delete_back();
        assert!(e.is_empty());
    }

    #[test]
    fn move_left_collapses_selection_to_start() {
        let mut e = TextEditEngine::new("abcd");
        e.set_caret(1, false);
        e.set_caret(3, true); // select "bc"
        e.move_caret(Dir::Left, false);
        assert_eq!(e.caret(), 1);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn shift_right_extends_selection() {
        let mut e = TextEditEngine::new("abcd");
        e.home(false);
        e.move_caret(Dir::Right, true);
        e.move_caret(Dir::Right, true);
        assert_eq!(e.selection(), Some((0, 2)));
    }

    #[test]
    fn caret_x_uses_measurer() {
        let mut e = TextEditEngine::new("abcd");
        e.set_caret(2, false);
        assert!((e.caret_x(&Mono(10.0)) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn click_places_caret_by_x() {
        let mut e = TextEditEngine::new("abcd");
        e.click(&Mono(10.0), 16.0, false); // nearest caret index 2
        assert_eq!(e.caret(), 2);
    }

    #[test]
    fn select_word_at_selects_word_run() {
        let mut e = TextEditEngine::new("foo bar baz");
        e.select_word_at(5); // inside "bar" (indices 4..7)
        assert_eq!(e.selection(), Some((4, 7)));
        // On a space: selects the whitespace run.
        e.select_word_at(3);
        assert_eq!(e.selection(), Some((3, 4)));
        // Clamps past the end.
        e.select_word_at(999);
        assert_eq!(e.selection(), Some((8, 11))); // "baz"
    }

    #[test]
    fn select_word_at_empty_is_noop() {
        let mut e = TextEditEngine::new("");
        e.select_word_at(0);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn click_then_extend_click_selects() {
        // The mouse drag path: press places the anchor (extend=false), drag
        // extends (extend=true) — must yield a non-empty selection.
        let mut e = TextEditEngine::new("abcd");
        e.click(&Mono(10.0), 4.0, false); // nearest caret index 0
        assert_eq!(e.selection(), None);
        e.click(&Mono(10.0), 34.0, true); // drag to ~index 3, extending
        assert_eq!(e.caret(), 3);
        assert_eq!(e.selection(), Some((0, 3)));
        assert_eq!(e.selection_x(&Mono(10.0)), Some((0.0, 30.0)));
    }

    #[test]
    fn selection_x_bounds() {
        let mut e = TextEditEngine::new("abcd");
        e.set_caret(1, false);
        e.set_caret(3, true);
        assert_eq!(e.selection_x(&Mono(10.0)), Some((10.0, 30.0)));
    }

    // ── Style-sync tests ────────────────────────────────────────────────────

    use crate::editor::text_style::CharStyle;

    fn base() -> CharStyle {
        CharStyle::from_block(10.0)
    }

    #[test]
    fn styles_stay_length_locked_through_edits() {
        let mut e = TextEditEngine::new_styled("abc", base());
        assert_eq!(e.styles.len(), 3);
        e.insert("XY");
        assert_eq!(e.styles.len(), e.chars.len());
        e.delete_back();
        assert_eq!(e.styles.len(), e.chars.len());
        e.set_caret(0, false);
        e.delete_forward();
        assert_eq!(e.styles.len(), e.chars.len());
    }

    #[test]
    fn apply_color_only_colors_selection() {
        let mut e = TextEditEngine::new_styled("abcd", base());
        e.set_caret(1, false);
        e.set_caret(3, true); // select "bc"
        e.apply_color([1.0, 0.0, 0.0]);
        let runs = e.style_runs();
        // a | bc(red) | d → 3 runs
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].style.color, [0.0, 0.0, 0.0]);
        assert_eq!(runs[1].style.color, [1.0, 0.0, 0.0]);
        assert_eq!((runs[1].start, runs[1].end), (1, 3));
        assert_eq!(runs[2].style.color, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn insert_inherits_typing_style_after_format() {
        let mut e = TextEditEngine::new_styled("ab", base());
        e.select_all();
        e.toggle_bold(); // whole buffer bold + typing_style bold
        e.end(false); // caret to end; typing style = bold (from left char)
        e.insert("c");
        let runs = e.style_runs();
        assert_eq!(runs.len(), 1, "all one bold run: {runs:?}");
        assert!(runs[0].style.bold);
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn caret_into_plain_text_types_plain() {
        let mut e = TextEditEngine::new_styled("ab", base());
        e.select_all();
        e.apply_color([1.0, 0.0, 0.0]); // all red
                                        // New plain char appended after an explicit color reset of typing style:
        e.end(false);
        e.set_caret(0, false); // caret before 'a'; left char none → style[0]=red
                               // Move to a hypothetical plain region by inserting plain via reset:
        e.apply_color([0.0, 0.0, 0.0]); // no selection → only typing style black
        e.insert("Z");
        let runs = e.style_runs();
        // Z(black) | ab(red)
        assert_eq!(runs[0].style.color, [0.0, 0.0, 0.0]);
        assert_eq!(runs[1].style.color, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn toggle_bold_clears_when_uniform_sets_when_mixed() {
        let mut e = TextEditEngine::new_styled("abcd", base());
        e.select_all();
        e.toggle_bold();
        assert!(e.style_runs().iter().all(|r| r.style.bold));
        e.select_all();
        e.toggle_bold(); // already all bold → clears
        assert!(e.style_runs().iter().all(|r| !r.style.bold));
    }

    #[test]
    fn active_style_reports_mixed_as_none() {
        let mut e = TextEditEngine::new_styled("abcd", base());
        e.set_caret(0, false);
        e.set_caret(2, true);
        e.apply_color([1.0, 0.0, 0.0]); // "ab" red, "cd" black
        e.set_caret(0, false);
        e.set_caret(4, true); // select all → mixed color
        let a = e.active_style();
        assert_eq!(a.color, None, "mixed color must be None");
        assert_eq!(a.bold, Some(false), "uniform bold must be Some(false)");
    }

    #[test]
    fn active_style_no_selection_reports_typing_style() {
        let mut e = TextEditEngine::new_styled("ab", base());
        e.toggle_bold(); // no selection → typing style bold
        let a = e.active_style();
        assert_eq!(a.bold, Some(true));
        assert_eq!(a.color, Some([0.0, 0.0, 0.0]));
    }

    #[test]
    fn style_runs_empty_buffer_is_empty() {
        let e = TextEditEngine::new_styled("", base());
        assert!(e.style_runs().is_empty());
    }

    #[test]
    fn selection_insert_replaces_styles_with_typing() {
        let mut e = TextEditEngine::new_styled("abcd", base());
        e.select_all();
        e.apply_color([0.0, 1.0, 0.0]); // all green, typing green
        e.set_caret(1, false);
        e.set_caret(3, true); // select "bc"
        e.insert("X"); // replaces with one char in typing style (green)
        assert_eq!(e.text(), "aXd");
        assert_eq!(e.styles.len(), 3);
        assert!(e
            .style_runs()
            .iter()
            .all(|r| r.style.color == [0.0, 1.0, 0.0]));
    }

    #[test]
    fn apply_style_runs_restores_underline() {
        let mut e = TextEditEngine::new_styled("abc", CharStyle::from_block(12.0));
        // No underline initially.
        assert!(e.style_runs().iter().all(|r| !r.style.underline));
        // Build a run sequence with underline=true across the whole buffer.
        let mut style = CharStyle::from_block(12.0);
        style.underline = true;
        let runs = vec![StyleRun {
            start: 0,
            end: 3,
            style,
        }];
        e.apply_style_runs(&runs);
        assert!(e.style_runs().iter().all(|r| r.style.underline));
    }
}
