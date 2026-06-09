//! Per-character style model for Word-style rich-text PDF editing.
//!
//! The [`crate::editor::text_edit_engine::TextEditEngine`] keeps a `Vec<CharStyle>`
//! length-locked with its character buffer, so a selection can be restyled
//! independently of the rest of the block. Commit and live-preview coalesce the
//! per-char styles into maximal [`StyleRun`]s and emit one PDF operator group
//! (`rg` colour / `Tf` font / `Tj` show) per run; underline/strikethrough render
//! as thin filled rectangles spanning each decorated run.
//!
//! Alignment is *block-level* (one [`Align`] per block, not per char) because the
//! single-line block is positioned as a whole.

/// Font family choice for a run.
///
/// `Original` keeps the block's own PDF font resource (no re-embedding needed);
/// `Family(name)` is a family the user picked, resolved to a bundled face and
/// embedded at commit time (bold/italic come from [`CharStyle`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontChoice {
    /// Keep the block's original PDF font key.
    Original,
    /// Switch this run to the named family (e.g. `"Helvetica"`).
    Family(String),
}

/// Paragraph alignment for a (single-line) block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// Left-aligned: keep the block's original x-origin.
    #[default]
    Left,
    /// Centred within the block's original box width.
    Center,
    /// Right-aligned to the block's original right edge.
    Right,
}

impl Align {
    /// Parse from the host string (`"left"`/`"center"`/`"right"`); unknown → Left.
    pub fn parse(s: &str) -> Self {
        match s {
            "center" => Align::Center,
            "right" => Align::Right,
            _ => Align::Left,
        }
    }

    /// Host string form, for the `text_edit_state` JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }
}

/// Resolved formatting for a single character.
///
/// `font_size`/`color` are `f64`, so this is `PartialEq` (bit-exact) but not
/// `Eq`. Run coalescing relies on that bit-exact compare, which is correct
/// because sizes/colours come from discrete UI values, never arithmetic.
#[derive(Debug, Clone, PartialEq)]
pub struct CharStyle {
    /// Fill colour as `[r, g, b]` in 0.0–1.0.
    pub color: [f64; 3],
    /// Font family for this char.
    pub font: FontChoice,
    /// Font size in points.
    pub font_size: f64,
    /// Bold variant.
    pub bold: bool,
    /// Italic variant.
    pub italic: bool,
    /// Underline decoration.
    pub underline: bool,
    /// Strikethrough decoration.
    pub strike: bool,
}

impl CharStyle {
    /// Baseline style when a block is opened: black, the block's own font at
    /// `font_size`, no bold/italic/decoration.
    pub fn from_block(font_size: f64) -> Self {
        Self {
            color: [0.0, 0.0, 0.0],
            font: FontChoice::Original,
            font_size,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
        }
    }

    /// Baseline style seeded with the block font's *intrinsic* bold/italic (read
    /// from the PDF FontDescriptor), so the panel reflects the real style on open
    /// and "no-op" formatting keeps the original embedded glyphs. Underline isn't a
    /// PDF text property, so it stays off.
    pub fn from_block_styled(font_size: f64, bold: bool, italic: bool) -> Self {
        Self {
            bold,
            italic,
            ..Self::from_block(font_size)
        }
    }

    /// Whether this run needs an embedded substitute font: a chosen family, or a
    /// bold/italic variant the original font can't provide directly.
    pub fn needs_embedded_font(&self) -> bool {
        !matches!(self.font, FontChoice::Original) || self.bold || self.italic
    }
}

/// Synthetic (faked) styling to apply to a run that keeps its **original
/// embedded glyphs** because no real bold/italic face is available for that font.
///
/// Computed by [`run_synthetic_style`]: a run that switched to a bundled
/// [`FontChoice::Family`] gets the real face (never synthetic), but a
/// [`FontChoice::Original`] run whose requested bold/italic *exceeds* the font's
/// intrinsic style is faked on the original glyphs — bold via a stroked outline
/// (`2 Tr` + line width), italic via a text-matrix shear. Turning bold/italic
/// *off* on a font that is intrinsically bold/italic can't thin or un-slant the
/// embedded glyphs, so it produces no synthetic styling (the original glyphs are
/// kept verbatim — no font swap, no breakage).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyntheticStyle {
    /// Fake bold by stroking the glyph outline.
    pub bold: bool,
    /// Fake italic by shearing the text matrix.
    pub italic: bool,
}

impl SyntheticStyle {
    /// Whether any synthetic styling is requested.
    pub fn any(self) -> bool {
        self.bold || self.italic
    }
}

/// Decide the synthetic styling for a run, given the block font's *intrinsic*
/// bold/italic (`orig_bold`/`orig_italic`, read from the FontDescriptor on open).
///
/// - A [`FontChoice::Family`] run resolves to a real bundled face → no synthetic.
/// - A [`FontChoice::Original`] run fakes only the styling the embedded font lacks:
///   bold when requested but `!orig_bold`, italic when requested but `!orig_italic`.
///   Bold/italic *off* on an intrinsically bold/italic font yields nothing (keep
///   the original glyphs as-is).
pub fn run_synthetic_style(
    style: &CharStyle,
    orig_bold: bool,
    orig_italic: bool,
) -> SyntheticStyle {
    match style.font {
        FontChoice::Family(_) => SyntheticStyle::default(),
        FontChoice::Original => SyntheticStyle {
            bold: style.bold && !orig_bold,
            italic: style.italic && !orig_italic,
        },
    }
}

/// Shear factor for synthetic italic: `tan(12°)` ≈ a 12-degree slant, matching the
/// obliquing common PDF viewers apply when faking italic.
pub const OBLIQUE_SHEAR: f64 = 0.213;

/// Tolerance when detecting an existing italic shear in a `Tm` matrix.
/// A shear within `OBLIQUE_SHEAR ± OBLIQUE_SHEAR_TOL` is treated as synthetic italic.
pub const OBLIQUE_SHEAR_TOL: f64 = 0.04;

/// Stroke line width for synthetic bold, as a fraction of the font size.
pub const SYNTHETIC_BOLD_STROKE_FRAC: f64 = 0.03;

/// Compute the shear component of a text matrix relative to its x-basis.
///
/// Returns `(a·c + b·d) / (a² + b²)`, i.e. the projection of the y-column onto
/// the x-column divided by the x-column's squared length. For an identity-scaled
/// matrix `[fs 0 0 fs x y]` this is 0; for a synthetic-italic matrix the
/// c-component carries `OBLIQUE_SHEAR · a`, so the result ≈ `OBLIQUE_SHEAR`.
/// Returns 0.0 when the x-basis is degenerate (near-zero).
pub fn matrix_shear(a: f64, b: f64, c: f64, d: f64) -> f64 {
    let n = a * a + b * b;
    if n <= 1e-9 {
        0.0
    } else {
        (a * c + b * d) / n
    }
}

/// A maximal run of equal-styled characters, as a `[start, end)` range into the
/// engine's character buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct StyleRun {
    /// Inclusive start char index.
    pub start: usize,
    /// Exclusive end char index.
    pub end: usize,
    /// The style shared by every char in the run.
    pub style: CharStyle,
}

/// Resolved style of the current selection for the formatting panel.
///
/// Each `Option` is `Some(v)` when the value is uniform across the selection and
/// `None` when the selection spans multiple values ("mixed" → the panel shows an
/// indeterminate control). `align` is always concrete (block-level).
#[derive(Debug, Clone, Default)]
pub struct ActiveStyle {
    /// Uniform fill colour, or `None` if mixed.
    pub color: Option<[f64; 3]>,
    /// Uniform font, or `None` if mixed.
    pub font: Option<FontChoice>,
    /// Uniform font size, or `None` if mixed.
    pub font_size: Option<f64>,
    /// Uniform bold flag, or `None` if mixed.
    pub bold: Option<bool>,
    /// Uniform italic flag, or `None` if mixed.
    pub italic: Option<bool>,
    /// Uniform underline flag, or `None` if mixed.
    pub underline: Option<bool>,
    /// Uniform strikethrough flag, or `None` if mixed.
    pub strike: Option<bool>,
    /// Block alignment (always concrete).
    pub align: Align,
}

// ── Decoration geometry ────────────────────────────────────────────────────────

/// Line thickness for underline/strikethrough at `font_size` (points).
pub fn decoration_thickness(font_size: f64) -> f64 {
    (font_size * 0.05).max(0.5)
}

/// Distance the underline sits *below* the baseline (points).
pub fn underline_offset(font_size: f64) -> f64 {
    font_size * 0.12
}

/// Distance the strikethrough sits *above* the baseline (points).
pub fn strike_offset(font_size: f64) -> f64 {
    font_size * 0.30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_block_is_black_original_plain() {
        let s = CharStyle::from_block(12.0);
        assert_eq!(s.color, [0.0, 0.0, 0.0]);
        assert_eq!(s.font, FontChoice::Original);
        assert_eq!(s.font_size, 12.0);
        assert!(!s.bold && !s.italic && !s.underline && !s.strike);
        assert!(!s.needs_embedded_font());
    }

    #[test]
    fn needs_embedded_font_for_variants_and_family() {
        let mut s = CharStyle::from_block(10.0);
        s.bold = true;
        assert!(s.needs_embedded_font());
        let mut s = CharStyle::from_block(10.0);
        s.font = FontChoice::Family("Times".into());
        assert!(s.needs_embedded_font());
    }

    #[test]
    fn align_round_trips_through_str() {
        for a in [Align::Left, Align::Center, Align::Right] {
            assert_eq!(Align::parse(a.as_str()), a);
        }
        assert_eq!(Align::parse("nonsense"), Align::Left);
    }

    #[test]
    fn matrix_shear_identity_is_zero() {
        assert!((matrix_shear(1.0, 0.0, 0.0, 1.0)).abs() < 1e-9);
    }

    #[test]
    fn matrix_shear_oblique_detected() {
        // Simulates [fs 0 OBLIQUE_SHEAR*fs fs x y] → shear ≈ OBLIQUE_SHEAR.
        let fs = 12.0_f64;
        let shear = matrix_shear(fs, 0.0, OBLIQUE_SHEAR * fs, fs);
        assert!((shear - OBLIQUE_SHEAR).abs() < 1e-6, "shear={shear}");
    }

    #[test]
    fn matrix_shear_degenerate_is_zero() {
        assert_eq!(matrix_shear(0.0, 0.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn decoration_geometry_scales_with_size() {
        assert!((decoration_thickness(20.0) - 1.0).abs() < 1e-9);
        assert!(decoration_thickness(1.0) >= 0.5); // floor
        assert!(underline_offset(10.0) > 0.0);
        assert!(strike_offset(10.0) > underline_offset(10.0));
    }

    #[test]
    fn synthetic_style_bold_on_regular_and_off_on_bold() {
        // Bold requested on a regular original font → fake bold.
        let mut s = CharStyle::from_block(12.0);
        s.bold = true;
        let syn = run_synthetic_style(&s, false, false);
        assert!(syn.bold && !syn.italic && syn.any());

        // Bold turned OFF on an intrinsically bold font → keep original glyphs,
        // no synthetic styling (can't thin an embedded bold face).
        let s = CharStyle::from_block(12.0); // bold = false
        let syn = run_synthetic_style(&s, /* orig_bold */ true, false);
        assert!(!syn.bold && !syn.italic && !syn.any());
    }

    #[test]
    fn synthetic_style_italic_on_upright_only() {
        let mut s = CharStyle::from_block(12.0);
        s.italic = true;
        assert_eq!(
            run_synthetic_style(&s, false, false),
            SyntheticStyle {
                bold: false,
                italic: true
            }
        );
        // Italic already intrinsic → nothing synthetic.
        assert_eq!(
            run_synthetic_style(&s, false, /* orig_italic */ true),
            SyntheticStyle::default()
        );
    }

    #[test]
    fn synthetic_style_family_uses_real_face_not_synthetic() {
        let mut s = CharStyle::from_block(12.0);
        s.font = FontChoice::Family("Helvetica".into());
        s.bold = true;
        s.italic = true;
        // A chosen family resolves to a real bundled face → never synthetic.
        assert_eq!(
            run_synthetic_style(&s, false, false),
            SyntheticStyle::default()
        );
    }
}
