//! Rotated-text handling for extraction.
//!
//! Spans carry the 2×2 part of their render matrix, which encodes the text
//! advance direction. Vertically-rotated runs (90°/180°/270° — rotated axis
//! labels, landscape tables inside portrait pages) cannot be merged into the
//! normal top-to-bottom, left-to-right line flow: their "lines" stack along
//! a different axis.
//!
//! This module classifies each span's orientation and groups spans into
//! orientation buckets. Within a bucket, spans are rotated into a canonical
//! horizontal frame so the normal line clustering applies.
//!
//! Orientations are bucketed to the nearest 90°; skewed text (not a multiple
//! of 90°) falls into the [`OTHER`] bucket and is emitted ungrouped.

use super::layout::TextLine;
use crate::content::text_state::TextSpan;

/// Orientation bucket for spans whose matrix is not a multiple of 90°.
pub const OTHER: u16 = u16::MAX;

/// Classify a span's text advance direction.
///
/// Returns `0`, `90`, `180`, `270` (degrees, from the advance vector of the
/// render matrix) or [`OTHER`] for skewed matrices.
pub fn orientation(span: &TextSpan) -> u16 {
    let [a, b, _, _] = span.render_matrix_2x2;
    if a == 0.0 && b == 0.0 {
        return 0;
    }
    let angle = b.atan2(a).to_degrees();
    // Round to the nearest multiple of 90, wrapping to [0, 360).
    let bucket = ((angle / 90.0).round() as i64).rem_euclid(4);
    // Reject matrices that are not close to a multiple of 90° (skew).
    let snapped = bucket as f64 * 90.0;
    let diff = (angle - snapped).abs();
    let diff = diff.min(360.0 - diff);
    if diff > 1.0 {
        return OTHER;
    }
    match bucket {
        0 => 0,
        1 => 90,
        2 => 180,
        _ => 270,
    }
}

/// Rotate a span's origin into the canonical frame where its text advances
/// along +X, returning `(x, y)` in that frame.
///
/// In the canonical frame, "lines" cluster along the new Y and words order
/// along the new X — so the ordinary line-building logic applies unchanged.
fn canonical_coords(span: &TextSpan, deg: u16) -> (f64, f64) {
    let (x, y) = (span.x, span.y);
    match deg {
        90 => (y, -x),
        180 => (-x, -y),
        270 => (-y, x),
        _ => (x, y),
    }
}

/// Group spans by orientation and build lines per bucket.
///
/// Returns a vector of `(orientation, lines)` pairs for every bucket that has
/// spans, ordered: `0`, `90`, `180`, `270`, then [`OTHER`] (whose spans are
/// each emitted as a single one-word line, ungrouped).
///
/// 0° uses the ordinary extraction path (`into_lines`-style clustering is
/// the caller's responsibility for the main flow); this function is meant
/// for the *secondary* buckets. For convenience the 0° bucket is also built
/// here so callers can use one entry point for non-primary orientations.
pub fn group_rotated(spans: &[TextSpan]) -> Vec<(u16, Vec<TextLine>)> {
    let mut out: Vec<(u16, Vec<TextLine>)> = Vec::new();
    for deg in [90u16, 180, 270] {
        let bucket: Vec<&TextSpan> = spans.iter().filter(|s| orientation(s) == deg).collect();
        if bucket.is_empty() {
            continue;
        }
        let lines = build_lines_rotated(&bucket, deg);
        out.push((deg, lines));
    }
    // Skewed spans: no reliable grouping — emit each as its own line.
    let other: Vec<&TextSpan> = spans.iter().filter(|s| orientation(s) == OTHER).collect();
    if !other.is_empty() {
        let lines = other
            .iter()
            .map(|s| single_span_line(s))
            .collect::<Vec<_>>();
        out.push((OTHER, lines));
    }
    out
}

/// Build lines for a rotated bucket by rotating coordinates into the
/// canonical frame and clustering along the new Y axis.
fn build_lines_rotated(bucket: &[&TextSpan], deg: u16) -> Vec<TextLine> {
    // (canonical_x, canonical_y, span)
    let mut items: Vec<(f64, f64, &TextSpan)> = bucket
        .iter()
        .map(|s| {
            let (cx, cy) = canonical_coords(s, deg);
            (cx, cy, *s)
        })
        .collect();
    // Cluster into lines by canonical Y (descending = reading order).
    items.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut line_groups: Vec<Vec<(f64, f64, &TextSpan)>> = Vec::new();
    for item in items {
        let threshold = item.2.font_size * 0.5;
        let placed = line_groups
            .iter_mut()
            .find(|g| (item.1 - g[0].1).abs() <= threshold);
        match placed {
            Some(group) => group.push(item),
            None => line_groups.push(vec![item]),
        }
    }
    line_groups
        .into_iter()
        .map(|group| build_line_canonical(&group))
        .collect()
}

/// Assemble a `TextLine` from canonically-sorted spans.
fn build_line_canonical(group: &[(f64, f64, &TextSpan)]) -> TextLine {
    let mut text = String::new();
    let (first_x, first_y, first) = group[0];
    let (last_cx, _, last) = group.last().expect("group is non-empty");
    // Line width in canonical X: from first span start to last span end.
    let width = (last_cx + last.width - first_x).abs();
    for (_, _, span) in group {
        text.push_str(&span.text);
    }
    TextLine {
        words: vec![super::layout::TextWord {
            text,
            x: first.x,
            y: first.y,
            width: width.abs(),
            font_size: first.font_size,
            font_name: first.font_name.clone(),
        }],
        baseline_y: first_y,
        x: first_x,
        width: width.abs(),
    }
}

fn single_span_line(span: &TextSpan) -> TextLine {
    TextLine {
        words: vec![super::layout::TextWord {
            text: span.text.clone(),
            x: span.x,
            y: span.y,
            width: span.width,
            font_size: span.font_size,
            font_name: span.font_name.clone(),
        }],
        baseline_y: span.y,
        x: span.x,
        width: span.width,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, x: f64, y: f64, matrix: [f64; 4]) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width: text.len() as f64 * 6.0,
            font_size: 10.0,
            font_size_px: 10.0,
            font_name: "F1".to_string(),
            char_advances: vec![],
            char_advances_y: vec![],
            char_cids: vec![],
            is_type3: false,
            render_matrix_2x2: matrix,
            stroke_text: false,
        }
    }

    #[test]
    fn horizontal_spans_classify_as_zero() {
        assert_eq!(orientation(&span("hi", 0.0, 0.0, [1.0, 0.0, 0.0, -1.0])), 0);
    }

    #[test]
    fn rotated_matrices_classify_correctly() {
        // 90° CCW label: advance direction (0, 1) in user space.
        assert_eq!(orientation(&span("up", 0.0, 0.0, [0.0, 1.0, 1.0, 0.0])), 90);
        assert_eq!(
            orientation(&span("dn", 0.0, 0.0, [0.0, -1.0, -1.0, 0.0])),
            270
        );
        assert_eq!(
            orientation(&span("back", 0.0, 0.0, [-1.0, 0.0, 0.0, 1.0])),
            180
        );
    }

    #[test]
    fn skewed_matrix_is_other() {
        assert_eq!(
            orientation(&span("sk", 0.0, 0.0, [1.0, 0.5, 0.0, -1.0])),
            OTHER
        );
    }

    #[test]
    fn vertical_runs_group_into_their_own_lines() {
        // Two 90° lines stacked along -X (canonical Y), each with two words
        // along +Y (canonical X).
        let s1 = span("AB", 100.0, 10.0, [0.0, 1.0, 1.0, 0.0]);
        let s2 = span("CD", 100.0, 40.0, [0.0, 1.0, 1.0, 0.0]);
        let s3 = span("EF", 70.0, 10.0, [0.0, 1.0, 1.0, 0.0]);
        let s4 = span("GH", 70.0, 40.0, [0.0, 1.0, 1.0, 0.0]);
        let groups = group_rotated(&[s1, s2, s3, s4]);
        assert_eq!(groups.len(), 1);
        let (deg, lines) = &groups[0];
        assert_eq!(*deg, 90);
        assert_eq!(lines.len(), 2);
        // Canonical frame: (y, −x); lines order by canonical Y descending, so
        // the line at user-space x = 70 (canonical Y = −70) comes first.
        assert_eq!(lines[0].words[0].text, "EFGH");
        assert_eq!(lines[1].words[0].text, "ABCD");
    }

    #[test]
    fn horizontal_spans_are_not_in_rotated_groups() {
        let s = span("plain", 10.0, 100.0, [1.0, 0.0, 0.0, -1.0]);
        assert!(group_rotated(&[s]).is_empty());
    }

    #[test]
    fn skewed_spans_emit_as_single_lines() {
        let s = span("skew", 10.0, 100.0, [1.0, 0.5, 0.0, -1.0]);
        let groups = group_rotated(&[s]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, OTHER);
        assert_eq!(groups[0].1.len(), 1);
        assert_eq!(groups[0].1[0].words[0].text, "skew");
    }
}
