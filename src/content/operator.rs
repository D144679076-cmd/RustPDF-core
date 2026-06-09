//! Typed content-stream operator vocabulary (ISO 32000-1 §A — Operator Summary).
//!
//! The interpreter dispatches on [`Operator`] rather than raw strings so the
//! compiler enforces exhaustiveness (a new operator can't be silently dropped)
//! and typos in operator names become impossible in the dispatch `match`. The
//! complete set of operators the engine understands is documented here in one
//! place.

/// A PDF content-stream operator.
///
/// `from_token` maps the on-the-wire token to a variant (returning `None` for
/// tokens outside the spec); `as_str` returns the canonical token. Operators
/// that share dispatch behaviour (e.g. `f`/`F`) still get distinct variants so
/// `as_str` round-trips exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    // ── Graphics state ──────────────────────────────────────────────────
    SaveState,       // q
    RestoreState,    // Q
    Concat,          // cm
    LineWidth,       // w
    LineCap,         // J
    LineJoin,        // j
    MiterLimit,      // M
    DashPattern,     // d
    RenderingIntent, // ri
    Flatness,        // i
    ExtGState,       // gs

    // ── Path construction ───────────────────────────────────────────────
    MoveTo,    // m
    LineTo,    // l
    CurveTo,   // c
    CurveToV,  // v
    CurveToY,  // y
    ClosePath, // h
    Rectangle, // re

    // ── Path painting ───────────────────────────────────────────────────
    Stroke,                 // S
    CloseStroke,            // s
    Fill,                   // f
    FillCompat,             // F (deprecated alias for f)
    FillEvenOdd,            // f*
    FillStroke,             // B
    FillStrokeEvenOdd,      // B*
    CloseFillStroke,        // b
    CloseFillStrokeEvenOdd, // b*
    EndPath,                // n

    // ── Clipping ────────────────────────────────────────────────────────
    Clip,        // W
    ClipEvenOdd, // W*

    // ── Colour ──────────────────────────────────────────────────────────
    StrokeColorSpace, // CS
    FillColorSpace,   // cs
    StrokeColor,      // SC
    StrokeColorN,     // SCN
    FillColor,        // sc
    FillColorN,       // scn
    StrokeGray,       // G
    FillGray,         // g
    StrokeRgb,        // RG
    FillRgb,          // rg
    StrokeCmyk,       // K
    FillCmyk,         // k

    // ── Text objects & state ────────────────────────────────────────────
    BeginText,   // BT
    EndText,     // ET
    CharSpacing, // Tc
    WordSpacing, // Tw
    HorizScale,  // Tz
    Leading,     // TL
    Font,        // Tf
    RenderMode,  // Tr
    Rise,        // Ts

    // ── Text positioning & showing ──────────────────────────────────────
    MoveText,                // Td
    MoveTextLeading,         // TD
    TextMatrix,              // Tm
    NextLine,                // T*
    ShowText,                // Tj
    NextLineShowText,        // '
    NextLineShowTextSpacing, // "
    ShowTextArray,           // TJ

    // ── XObjects & images ───────────────────────────────────────────────
    XObject,     // Do
    InlineImage, // BI (ID/EI consumed during parsing)

    // ── Shading ─────────────────────────────────────────────────────────
    Shading, // sh

    // ── Marked content (no-op for rendering) ────────────────────────────
    BeginMarkedContent,     // BMC
    BeginMarkedContentDict, // BDC
    EndMarkedContent,       // EMC
    MarkedPoint,            // MP
    MarkedPointDict,        // DP

    // ── Compatibility (no-op) ───────────────────────────────────────────
    BeginCompat, // BX
    EndCompat,   // EX

    // ── Type 3 font glyph metrics (no-op outside a Type 3 CharProc) ──────
    Type3Width,     // d0
    Type3WidthBBox, // d1
}

impl Operator {
    /// Map a content-stream token to its operator, or `None` if unrecognised.
    pub fn from_token(token: &str) -> Option<Operator> {
        use Operator::*;
        Some(match token {
            "q" => SaveState,
            "Q" => RestoreState,
            "cm" => Concat,
            "w" => LineWidth,
            "J" => LineCap,
            "j" => LineJoin,
            "M" => MiterLimit,
            "d" => DashPattern,
            "ri" => RenderingIntent,
            "i" => Flatness,
            "gs" => ExtGState,
            "m" => MoveTo,
            "l" => LineTo,
            "c" => CurveTo,
            "v" => CurveToV,
            "y" => CurveToY,
            "h" => ClosePath,
            "re" => Rectangle,
            "S" => Stroke,
            "s" => CloseStroke,
            "f" => Fill,
            "F" => FillCompat,
            "f*" => FillEvenOdd,
            "B" => FillStroke,
            "B*" => FillStrokeEvenOdd,
            "b" => CloseFillStroke,
            "b*" => CloseFillStrokeEvenOdd,
            "n" => EndPath,
            "W" => Clip,
            "W*" => ClipEvenOdd,
            "CS" => StrokeColorSpace,
            "cs" => FillColorSpace,
            "SC" => StrokeColor,
            "SCN" => StrokeColorN,
            "sc" => FillColor,
            "scn" => FillColorN,
            "G" => StrokeGray,
            "g" => FillGray,
            "RG" => StrokeRgb,
            "rg" => FillRgb,
            "K" => StrokeCmyk,
            "k" => FillCmyk,
            "BT" => BeginText,
            "ET" => EndText,
            "Tc" => CharSpacing,
            "Tw" => WordSpacing,
            "Tz" => HorizScale,
            "TL" => Leading,
            "Tf" => Font,
            "Tr" => RenderMode,
            "Ts" => Rise,
            "Td" => MoveText,
            "TD" => MoveTextLeading,
            "Tm" => TextMatrix,
            "T*" => NextLine,
            "Tj" => ShowText,
            "'" => NextLineShowText,
            "\"" => NextLineShowTextSpacing,
            "TJ" => ShowTextArray,
            "Do" => XObject,
            "BI" => InlineImage,
            "sh" => Shading,
            "BMC" => BeginMarkedContent,
            "BDC" => BeginMarkedContentDict,
            "EMC" => EndMarkedContent,
            "MP" => MarkedPoint,
            "DP" => MarkedPointDict,
            "BX" => BeginCompat,
            "EX" => EndCompat,
            "d0" => Type3Width,
            "d1" => Type3WidthBBox,
            _ => return None,
        })
    }

    /// The canonical on-the-wire token for this operator.
    pub fn as_str(self) -> &'static str {
        use Operator::*;
        match self {
            SaveState => "q",
            RestoreState => "Q",
            Concat => "cm",
            LineWidth => "w",
            LineCap => "J",
            LineJoin => "j",
            MiterLimit => "M",
            DashPattern => "d",
            RenderingIntent => "ri",
            Flatness => "i",
            ExtGState => "gs",
            MoveTo => "m",
            LineTo => "l",
            CurveTo => "c",
            CurveToV => "v",
            CurveToY => "y",
            ClosePath => "h",
            Rectangle => "re",
            Stroke => "S",
            CloseStroke => "s",
            Fill => "f",
            FillCompat => "F",
            FillEvenOdd => "f*",
            FillStroke => "B",
            FillStrokeEvenOdd => "B*",
            CloseFillStroke => "b",
            CloseFillStrokeEvenOdd => "b*",
            EndPath => "n",
            Clip => "W",
            ClipEvenOdd => "W*",
            StrokeColorSpace => "CS",
            FillColorSpace => "cs",
            StrokeColor => "SC",
            StrokeColorN => "SCN",
            FillColor => "sc",
            FillColorN => "scn",
            StrokeGray => "G",
            FillGray => "g",
            StrokeRgb => "RG",
            FillRgb => "rg",
            StrokeCmyk => "K",
            FillCmyk => "k",
            BeginText => "BT",
            EndText => "ET",
            CharSpacing => "Tc",
            WordSpacing => "Tw",
            HorizScale => "Tz",
            Leading => "TL",
            Font => "Tf",
            RenderMode => "Tr",
            Rise => "Ts",
            MoveText => "Td",
            MoveTextLeading => "TD",
            TextMatrix => "Tm",
            NextLine => "T*",
            ShowText => "Tj",
            NextLineShowText => "'",
            NextLineShowTextSpacing => "\"",
            ShowTextArray => "TJ",
            XObject => "Do",
            InlineImage => "BI",
            Shading => "sh",
            BeginMarkedContent => "BMC",
            BeginMarkedContentDict => "BDC",
            EndMarkedContent => "EMC",
            MarkedPoint => "MP",
            MarkedPointDict => "DP",
            BeginCompat => "BX",
            EndCompat => "EX",
            Type3Width => "d0",
            Type3WidthBBox => "d1",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_token_is_none() {
        assert_eq!(Operator::from_token("zz"), None);
        assert_eq!(Operator::from_token(""), None);
        assert_eq!(Operator::from_token("Tjj"), None);
    }

    #[test]
    fn known_tokens_map() {
        assert_eq!(Operator::from_token("Tj"), Some(Operator::ShowText));
        assert_eq!(Operator::from_token("TJ"), Some(Operator::ShowTextArray));
        assert_eq!(Operator::from_token("cm"), Some(Operator::Concat));
        assert_eq!(Operator::from_token("f*"), Some(Operator::FillEvenOdd));
    }

    #[test]
    fn as_str_round_trips_through_from_token() {
        // Every variant's canonical token must parse back to the same variant.
        // This catches table typos and keeps the two maps in sync. The list is
        // the authoritative operator set.
        let all = [
            Operator::SaveState,
            Operator::RestoreState,
            Operator::Concat,
            Operator::LineWidth,
            Operator::LineCap,
            Operator::LineJoin,
            Operator::MiterLimit,
            Operator::DashPattern,
            Operator::RenderingIntent,
            Operator::Flatness,
            Operator::ExtGState,
            Operator::MoveTo,
            Operator::LineTo,
            Operator::CurveTo,
            Operator::CurveToV,
            Operator::CurveToY,
            Operator::ClosePath,
            Operator::Rectangle,
            Operator::Stroke,
            Operator::CloseStroke,
            Operator::Fill,
            Operator::FillCompat,
            Operator::FillEvenOdd,
            Operator::FillStroke,
            Operator::FillStrokeEvenOdd,
            Operator::CloseFillStroke,
            Operator::CloseFillStrokeEvenOdd,
            Operator::EndPath,
            Operator::Clip,
            Operator::ClipEvenOdd,
            Operator::StrokeColorSpace,
            Operator::FillColorSpace,
            Operator::StrokeColor,
            Operator::StrokeColorN,
            Operator::FillColor,
            Operator::FillColorN,
            Operator::StrokeGray,
            Operator::FillGray,
            Operator::StrokeRgb,
            Operator::FillRgb,
            Operator::StrokeCmyk,
            Operator::FillCmyk,
            Operator::BeginText,
            Operator::EndText,
            Operator::CharSpacing,
            Operator::WordSpacing,
            Operator::HorizScale,
            Operator::Leading,
            Operator::Font,
            Operator::RenderMode,
            Operator::Rise,
            Operator::MoveText,
            Operator::MoveTextLeading,
            Operator::TextMatrix,
            Operator::NextLine,
            Operator::ShowText,
            Operator::NextLineShowText,
            Operator::NextLineShowTextSpacing,
            Operator::ShowTextArray,
            Operator::XObject,
            Operator::InlineImage,
            Operator::Shading,
            Operator::BeginMarkedContent,
            Operator::BeginMarkedContentDict,
            Operator::EndMarkedContent,
            Operator::MarkedPoint,
            Operator::MarkedPointDict,
            Operator::BeginCompat,
            Operator::EndCompat,
            Operator::Type3Width,
            Operator::Type3WidthBBox,
        ];
        for op in all {
            assert_eq!(
                Operator::from_token(op.as_str()),
                Some(op),
                "round-trip failed for {:?} ({})",
                op,
                op.as_str()
            );
        }
    }
}
