//! Permanent content redaction.
//!
//! Redaction removes content from a PDF so it cannot be recovered, even by
//! inspecting raw bytes. The output of [`apply_redactions`] is produced by
//! [`PdfEditor::save_new`], which serialises the entire document from scratch —
//! no byte of the original file survives.
//!
//! # Two-step usage
//!
//! 1. Define the areas to remove as [`RedactZone`] values (page index + rect).
//! 2. Call [`apply_redactions`] to rewrite every affected content stream and
//!    return a self-contained PDF.

use std::collections::BTreeMap;

use crate::content::graphics_state::Matrix;
use crate::content::operators::{parse_content_stream, serialize_operations, Operation};
use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::streams::make_flate_stream;

use super::document_editor::PdfEditor;

// ── Public types ───────────────────────────────────────────────────────────────

/// A rectangular region on a page scheduled for permanent redaction.
#[derive(Debug, Clone)]
pub struct RedactZone {
    /// 0-based index of the page that contains this zone.
    pub page_index: usize,
    /// Bounding rectangle `[x1, y1, x2, y2]` in user-space coordinates
    /// (same coordinate system as `MediaBox`).
    pub rect: [f64; 4],
    /// RGB fill color of the overlay rectangle drawn after content is removed,
    /// each component in `[0.0, 1.0]`. Default is black `[0.0, 0.0, 0.0]`.
    pub overlay_color: [f64; 3],
}

impl RedactZone {
    /// Create a redact zone with a default black overlay.
    pub fn new(page_index: usize, rect: [f64; 4]) -> Self {
        Self {
            page_index,
            rect,
            overlay_color: [0.0, 0.0, 0.0],
        }
    }

    /// Override the fill color of the overlay rectangle.
    pub fn with_color(mut self, color: [f64; 3]) -> Self {
        self.overlay_color = color;
        self
    }
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Apply redactions and return a new, self-contained PDF with no original bytes.
///
/// For each zone, content falling within the rectangle is removed from the
/// page's content stream and replaced with a filled overlay rectangle.
/// `/Redact` annotations are stripped from the output.
///
/// Uses [`PdfEditor::save_new`] internally — no bytes of the original file
/// survive in the output, satisfying forensic redaction requirements.
///
/// # Parameters
/// - `editor` — an open [`PdfEditor`] (must have the `writer` feature).
/// - `zones`  — list of rectangular areas to redact, each tied to a page index.
///
/// # Returns
/// Raw bytes of the redacted PDF ready to write to disk or return to a caller.
pub fn apply_redactions(editor: &mut PdfEditor, zones: &[RedactZone]) -> Result<Vec<u8>> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "apply_redactions")?;
    if zones.is_empty() {
        return editor.save_new();
    }

    // Group zones by page so we do one pass per page.
    let mut by_page: BTreeMap<usize, Vec<&RedactZone>> = BTreeMap::new();
    for z in zones {
        by_page.entry(z.page_index).or_default().push(z);
    }

    for (page_index, page_zones) in &by_page {
        let (page_id, mut page_dict) = editor.get_page_dict(*page_index)?;

        // Collect all content stream bytes (may be an array of streams).
        let content_bytes = collect_content_bytes(editor, &page_dict)?;

        // Build the list of zone rects for the rewriter.
        let rects: Vec<[f64; 4]> = page_zones.iter().map(|z| z.rect).collect();
        let new_bytes = rewrite_content_stream(&content_bytes, &rects)?;

        // Append per-zone overlay rectangles.
        let overlay = build_overlay(page_zones);
        let mut combined = new_bytes;
        combined.extend_from_slice(&overlay);

        // Compress and store the rewritten stream.
        let stream = make_flate_stream(&combined, PdfDict::new())?;
        let new_content_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

        // Update page to point at the new, single content stream.
        page_dict.insert(
            "Contents".to_owned(),
            PdfObject::Reference(new_content_id, 0),
        );

        // Remove any /Redact annotations from the page.
        strip_redact_annots(editor, &mut page_dict)?;

        editor.replace_object(page_id, PdfObject::Dictionary(page_dict));
    }

    editor.save_new()
}

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Collect and concatenate all content stream bytes for a page.
fn collect_content_bytes(editor: &PdfEditor, page_dict: &PdfDict) -> Result<Vec<u8>> {
    let contents = match page_dict.get("Contents") {
        Some(c) => c.clone(),
        None => return Ok(Vec::new()),
    };

    match contents {
        PdfObject::Reference(id, _) => editor.doc.get_stream_data(id),
        PdfObject::Array(refs) => {
            let mut out = Vec::new();
            for r in refs {
                let id = match r {
                    PdfObject::Reference(id, _) => id,
                    _ => continue,
                };
                if !out.is_empty() {
                    out.push(b'\n');
                }
                out.extend_from_slice(&editor.doc.get_stream_data(id)?);
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// Rewrite a content stream, dropping operations that paint within any zone.
///
/// Returns the filtered content stream bytes (without overlay rectangles — those
/// are appended separately by [`build_overlay`]).
fn rewrite_content_stream(data: &[u8], zones: &[[f64; 4]]) -> Result<Vec<u8>> {
    if data.is_empty() || zones.is_empty() {
        return Ok(data.to_vec());
    }

    let ops = parse_content_stream(data)?;
    let filtered = filter_operations(ops, zones);
    Ok(serialize_operations(&filtered))
}

/// State machine that tracks graphics / text state while filtering operations.
struct RewriterState {
    ctm_stack: Vec<Matrix>,
    path_buf: Vec<Operation>,
    in_text: bool,
    text_matrix: Matrix,
    text_line_matrix: Matrix,
    font_size: f64,
    text_leading: f64,
}

impl RewriterState {
    fn new() -> Self {
        Self {
            ctm_stack: vec![Matrix::identity()],
            path_buf: Vec::new(),
            in_text: false,
            text_matrix: Matrix::identity(),
            text_line_matrix: Matrix::identity(),
            font_size: 0.0,
            text_leading: 0.0,
        }
    }

    fn ctm(&self) -> Matrix {
        self.ctm_stack
            .last()
            .copied()
            .unwrap_or_else(Matrix::identity)
    }
}

/// Filter a list of parsed operations, dropping those that paint inside a zone.
fn filter_operations(ops: Vec<Operation>, zones: &[[f64; 4]]) -> Vec<Operation> {
    let mut state = RewriterState::new();
    let mut output: Vec<Operation> = Vec::with_capacity(ops.len());

    for op in ops {
        match op.operator.as_str() {
            // ── Graphics state ────────────────────────────────────────────────
            "q" => {
                let ctm = state.ctm();
                state.ctm_stack.push(ctm);
                output.push(op);
            }
            "Q" => {
                if state.ctm_stack.len() > 1 {
                    state.ctm_stack.pop();
                } else {
                    log::warn!("redact rewriter: Q with empty CTM stack, ignoring");
                }
                output.push(op);
            }
            "cm" => {
                if let Some(m) = parse_matrix(&op.operands) {
                    let new_ctm = state.ctm().concat(&m);
                    if let Some(top) = state.ctm_stack.last_mut() {
                        *top = new_ctm;
                    }
                }
                output.push(op);
            }

            // ── Path construction — buffer until a paint op arrives ────────────
            "m" | "l" | "c" | "v" | "y" | "h" | "re" => {
                state.path_buf.push(op);
            }

            // ── Path painting ─────────────────────────────────────────────────
            "f" | "F" | "f*" | "S" | "s" | "B" | "B*" | "b" | "b*" | "n" => {
                let ctm = state.ctm();
                let bbox = path_bbox(&state.path_buf, &ctm);
                if let Some(bbox) = bbox {
                    if intersects_any(bbox, zones) {
                        log::debug!(
                            "redact: dropping path op '{}' (bbox intersects zone)",
                            op.operator
                        );
                        state.path_buf.clear();
                        // Drop the paint op — don't emit.
                        continue;
                    }
                }
                // Flush buffered path construction ops, then the paint op.
                output.append(&mut state.path_buf);
                output.push(op);
            }

            // Clipping — flush path buffer but never drop (corrupts gfx state).
            "W" | "W*" => {
                output.append(&mut state.path_buf);
                output.push(op);
            }

            // ── Text state ────────────────────────────────────────────────────
            "BT" => {
                state.in_text = true;
                state.text_matrix = Matrix::identity();
                state.text_line_matrix = Matrix::identity();
                output.push(op);
            }
            "ET" => {
                state.in_text = false;
                output.push(op);
            }
            "Tf" => {
                if let Some(size) = op.operands.get(1).and_then(|o| o.as_real_or_int()) {
                    state.font_size = size;
                }
                output.push(op);
            }
            "Td" => {
                let tx = op
                    .operands
                    .first()
                    .and_then(|o| o.as_real_or_int())
                    .unwrap_or(0.0);
                let ty = op
                    .operands
                    .get(1)
                    .and_then(|o| o.as_real_or_int())
                    .unwrap_or(0.0);
                state.text_line_matrix = translate_matrix(&state.text_line_matrix, tx, ty);
                state.text_matrix = state.text_line_matrix;
                output.push(op);
            }
            "TD" => {
                let tx = op
                    .operands
                    .first()
                    .and_then(|o| o.as_real_or_int())
                    .unwrap_or(0.0);
                let ty = op
                    .operands
                    .get(1)
                    .and_then(|o| o.as_real_or_int())
                    .unwrap_or(0.0);
                state.text_leading = -ty;
                state.text_line_matrix = translate_matrix(&state.text_line_matrix, tx, ty);
                state.text_matrix = state.text_line_matrix;
                output.push(op);
            }
            "T*" => {
                state.text_line_matrix =
                    translate_matrix(&state.text_line_matrix, 0.0, -state.text_leading);
                state.text_matrix = state.text_line_matrix;
                output.push(op);
            }
            "Tm" => {
                if let Some(m) = parse_matrix(&op.operands) {
                    state.text_matrix = m;
                    state.text_line_matrix = m;
                }
                output.push(op);
            }
            "TL" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_real_or_int()) {
                    state.text_leading = v;
                }
                output.push(op);
            }

            // ── Text showing ──────────────────────────────────────────────────
            "Tj" | "TJ" | "'" | "\"" => {
                let n_chars = estimate_char_count(&op);
                let ctm = state.ctm();
                let bbox = text_bbox(&state.text_matrix, &ctm, state.font_size, n_chars);
                if intersects_any(bbox, zones) {
                    log::debug!(
                        "redact: dropping text op '{}' (position inside zone)",
                        op.operator
                    );
                    // Drop — don't emit.
                    continue;
                }
                output.push(op);
            }

            // ── XObjects / inline images ──────────────────────────────────────
            "Do" | "BI" => {
                // Conservative: drop if the CTM-mapped unit square intersects a zone.
                let ctm = state.ctm();
                let bbox = ctm_unit_bbox(&ctm);
                if intersects_any(bbox, zones) {
                    log::debug!(
                        "redact: dropping '{}' op (CTM unit bbox intersects zone)",
                        op.operator
                    );
                    continue;
                }
                output.push(op);
            }

            // ── Everything else — safe to pass through ────────────────────────
            _ => {
                // Flush any accumulated path ops so graphics state stays intact.
                if !state.path_buf.is_empty() {
                    output.append(&mut state.path_buf);
                }
                output.push(op);
            }
        }
    }

    output
}

/// Build overlay rectangles for all zones (emitted after filtered content).
fn build_overlay(zones: &[&RedactZone]) -> Vec<u8> {
    let mut ops: Vec<Operation> = Vec::new();
    for z in zones {
        let [x1, y1, x2, y2] = z.rect;
        let [r, g, b] = z.overlay_color;
        let w = x2 - x1;
        let h = y2 - y1;

        ops.push(op0("q"));
        ops.push(op_nums("rg", &[r, g, b]));
        ops.push(op_nums("re", &[x1, y1, w, h]));
        ops.push(op0("f"));
        ops.push(op0("Q"));
    }
    serialize_operations(&ops)
}

/// Remove `/Redact` annotations from a page's `/Annots` array.
fn strip_redact_annots(editor: &mut PdfEditor, page_dict: &mut PdfDict) -> Result<()> {
    let annots = match page_dict.get("Annots") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Ok(()),
    };

    let mut kept: Vec<PdfObject> = Vec::new();
    for entry in annots {
        let id = match &entry {
            PdfObject::Reference(id, _) => *id,
            _ => {
                kept.push(entry);
                continue;
            }
        };
        let subtype = editor
            .get_object(id)
            .ok()
            .and_then(|o| o.into_dict())
            .and_then(|d| d.get("Subtype").cloned())
            .and_then(|s| match s {
                PdfObject::Name(n) => Some(n),
                _ => None,
            });
        if subtype.as_deref() == Some("Redact") {
            log::debug!("redact: stripping /Redact annotation object {}", id);
        } else {
            kept.push(entry);
        }
    }
    page_dict.insert("Annots".to_owned(), PdfObject::Array(kept));
    Ok(())
}

// ── Geometry helpers ───────────────────────────────────────────────────────────

/// Compute an axis-aligned bounding box for the accumulated path ops under CTM.
fn path_bbox(path_buf: &[Operation], ctm: &Matrix) -> Option<[f64; 4]> {
    let mut points: Vec<(f64, f64)> = Vec::new();

    for op in path_buf {
        match op.operator.as_str() {
            "m" | "l" => {
                let x = op.operands.first().and_then(|o| o.as_real_or_int())?;
                let y = op.operands.get(1).and_then(|o| o.as_real_or_int())?;
                points.push(ctm.transform_point(x, y));
            }
            "c" => {
                // Cubic bezier — use start and end points as conservative approximation.
                let x3 = op.operands.get(4).and_then(|o| o.as_real_or_int())?;
                let y3 = op.operands.get(5).and_then(|o| o.as_real_or_int())?;
                points.push(ctm.transform_point(x3, y3));
            }
            "v" | "y" => {
                let x2 = op.operands.get(2).and_then(|o| o.as_real_or_int())?;
                let y2 = op.operands.get(3).and_then(|o| o.as_real_or_int())?;
                points.push(ctm.transform_point(x2, y2));
            }
            "re" => {
                let x = op.operands.first().and_then(|o| o.as_real_or_int())?;
                let y = op.operands.get(1).and_then(|o| o.as_real_or_int())?;
                let w = op.operands.get(2).and_then(|o| o.as_real_or_int())?;
                let h = op.operands.get(3).and_then(|o| o.as_real_or_int())?;
                points.push(ctm.transform_point(x, y));
                points.push(ctm.transform_point(x + w, y + h));
            }
            _ => {}
        }
    }

    if points.is_empty() {
        return None;
    }

    let (x0, y0) = points[0];
    let mut min_x = x0;
    let mut min_y = y0;
    let mut max_x = x0;
    let mut max_y = y0;
    for (px, py) in points.iter().skip(1) {
        if *px < min_x {
            min_x = *px;
        }
        if *py < min_y {
            min_y = *py;
        }
        if *px > max_x {
            max_x = *px;
        }
        if *py > max_y {
            max_y = *py;
        }
    }
    Some([min_x, min_y, max_x, max_y])
}

/// Estimate a text span bounding box from the current text/CTM matrices.
///
/// Uses a conservative width estimate (`font_size * 0.6 * char_count`) and a
/// height of `font_size`. Err toward over-redacting rather than under-redacting.
fn text_bbox(text_matrix: &Matrix, ctm: &Matrix, font_size: f64, n_chars: usize) -> [f64; 4] {
    let combined = text_matrix.concat(ctm);
    let (tx, ty) = (combined.e, combined.f);
    let height = font_size.abs();
    let width = font_size.abs() * 0.6 * (n_chars.max(1) as f64);
    [tx, ty, tx + width, ty + height]
}

/// Return the bounding box of the unit square [0,0,1,1] under CTM.
fn ctm_unit_bbox(ctm: &Matrix) -> [f64; 4] {
    let corners = [
        ctm.transform_point(0.0, 0.0),
        ctm.transform_point(1.0, 0.0),
        ctm.transform_point(1.0, 1.0),
        ctm.transform_point(0.0, 1.0),
    ];
    let mut min_x = corners[0].0;
    let mut min_y = corners[0].1;
    let mut max_x = corners[0].0;
    let mut max_y = corners[0].1;
    for (px, py) in corners.iter().skip(1) {
        if *px < min_x {
            min_x = *px;
        }
        if *py < min_y {
            min_y = *py;
        }
        if *px > max_x {
            max_x = *px;
        }
        if *py > max_y {
            max_y = *py;
        }
    }
    [min_x, min_y, max_x, max_y]
}

/// Test whether a bounding box intersects any of the provided zones.
fn intersects_any(bbox: [f64; 4], zones: &[[f64; 4]]) -> bool {
    zones.iter().any(|z| rects_intersect(bbox, *z))
}

/// Axis-aligned rectangle intersection test (open interval on each side).
fn rects_intersect(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}

// ── Parsing / construction helpers ─────────────────────────────────────────────

/// Parse six numeric operands into a [`Matrix`].
fn parse_matrix(operands: &[PdfObject]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }
    Some(Matrix {
        a: operands[0].as_real_or_int()?,
        b: operands[1].as_real_or_int()?,
        c: operands[2].as_real_or_int()?,
        d: operands[3].as_real_or_int()?,
        e: operands[4].as_real_or_int()?,
        f: operands[5].as_real_or_int()?,
    })
}

/// Translate a matrix by `(tx, ty)` (move-to variant used for text positioning).
fn translate_matrix(m: &Matrix, tx: f64, ty: f64) -> Matrix {
    Matrix {
        a: m.a,
        b: m.b,
        c: m.c,
        d: m.d,
        e: m.e + tx * m.a + ty * m.c,
        f: m.f + tx * m.b + ty * m.d,
    }
}

/// Estimate the number of characters in a text-showing operation.
fn estimate_char_count(op: &Operation) -> usize {
    match op.operator.as_str() {
        "Tj" | "'" | "\"" => op
            .operands
            .first()
            .and_then(|o| match o {
                PdfObject::String(s) => Some(s.len()),
                _ => None,
            })
            .unwrap_or(1),
        "TJ" => op
            .operands
            .first()
            .and_then(|o| match o {
                PdfObject::Array(arr) => Some(
                    arr.iter()
                        .filter_map(|e| match e {
                            PdfObject::String(s) => Some(s.len()),
                            _ => None,
                        })
                        .sum(),
                ),
                _ => None,
            })
            .unwrap_or(1),
        _ => 1,
    }
}

/// Build a no-operand operation.
fn op0(operator: &str) -> Operation {
    Operation {
        operands: vec![],
        operator: operator.to_owned(),
    }
}

/// Build an operation with numeric operands.
fn op_nums(operator: &str, nums: &[f64]) -> Operation {
    Operation {
        operands: nums.iter().map(|&v| PdfObject::Real(v)).collect(),
        operator: operator.to_owned(),
    }
}

// ── PdfObject extension ────────────────────────────────────────────────────────

trait PdfObjectExt {
    fn as_real_or_int(&self) -> Option<f64>;
    fn into_dict(self) -> Option<PdfDict>;
}

impl PdfObjectExt for PdfObject {
    fn as_real_or_int(&self) -> Option<f64> {
        match self {
            PdfObject::Real(v) => Some(*v),
            PdfObject::Integer(n) => Some(*n as f64),
            _ => None,
        }
    }

    fn into_dict(self) -> Option<PdfDict> {
        match self {
            PdfObject::Dictionary(d) => Some(d),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::operators::parse_content_stream;
    use crate::editor::document_editor::PdfEditor;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    // ── rewrite_content_stream ────────────────────────────────────────────────

    #[test]
    fn rewrite_removes_text_in_zone() {
        // Text at y=700, font size 12 → spans approx y=700–712.
        let src = b"BT /F1 12 Tf 0 700 Td (secret) Tj ET";
        let zones = [[0.0_f64, 695.0, 400.0, 715.0]];
        let out = rewrite_content_stream(src, &zones).unwrap();
        // The re-parsed output must not contain the Tj operator.
        let ops = parse_content_stream(&out).unwrap();
        assert!(
            !ops.iter().any(|o| o.operator == "Tj"),
            "Tj should have been dropped by rewriter"
        );
    }

    #[test]
    fn rewrite_keeps_text_outside_zone() {
        let src = b"BT /F1 12 Tf 0 700 Td (visible) Tj ET";
        let zones = [[0.0_f64, 0.0, 400.0, 100.0]]; // zone far below
        let out = rewrite_content_stream(src, &zones).unwrap();
        let ops = parse_content_stream(&out).unwrap();
        assert!(
            ops.iter().any(|o| o.operator == "Tj"),
            "Tj outside zone must survive"
        );
    }

    #[test]
    fn rewrite_removes_filled_rect_in_zone() {
        // Rectangle at (100, 200, 150, 230) — fully inside zone.
        let src = b"100 200 50 30 re f";
        let zones = [[50.0_f64, 150.0, 300.0, 300.0]];
        let out = rewrite_content_stream(src, &zones).unwrap();
        let ops = parse_content_stream(&out).unwrap();
        assert!(
            !ops.iter().any(|o| o.operator == "f"),
            "fill op inside zone must be dropped"
        );
    }

    #[test]
    fn rewrite_keeps_rect_outside_zone() {
        let src = b"100 200 50 30 re f";
        let zones = [[600.0_f64, 700.0, 700.0, 800.0]]; // zone far away
        let out = rewrite_content_stream(src, &zones).unwrap();
        let ops = parse_content_stream(&out).unwrap();
        assert!(
            ops.iter().any(|o| o.operator == "f"),
            "fill op outside zone must survive"
        );
    }

    #[test]
    fn rewrite_empty_stream_returns_empty() {
        let out = rewrite_content_stream(b"", &[[0.0, 0.0, 100.0, 100.0]]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn rewrite_malformed_stream_no_panic() {
        // Random bytes must not panic — graceful degradation (R1).
        let garbage = b"\x00\xFF\xFE junk bytes !@#$";
        let result = rewrite_content_stream(garbage, &[[0.0, 0.0, 100.0, 100.0]]);
        assert!(result.is_ok(), "malformed content stream must not error");
    }

    #[test]
    fn overlay_rect_appended_in_output() {
        let zones = [RedactZone::new(0, [10.0, 20.0, 110.0, 60.0])];
        let zone_refs: Vec<&RedactZone> = zones.iter().collect();
        let overlay = build_overlay(&zone_refs);
        let ops = parse_content_stream(&overlay).unwrap();
        // Must contain "re" and "f" operators.
        assert!(
            ops.iter().any(|o| o.operator == "re"),
            "overlay must have re"
        );
        assert!(ops.iter().any(|o| o.operator == "f"), "overlay must have f");
    }

    #[test]
    fn strip_redact_annots_removes_correct_entries() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();

        // Add a blank page so we can put annotations on it.
        crate::editor::page_editor::add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();

        // Add one Text annotation and one Redact annotation.
        let text_id = crate::editor::annotation::add_annotation(
            &mut editor,
            0,
            crate::editor::annotation::AnnotationBuilder::new(
                crate::editor::annotation::AnnotationType::Text {
                    contents: "keep me".to_owned(),
                    open: false,
                },
                [10.0, 10.0, 50.0, 50.0],
            ),
        )
        .unwrap();

        let redact_id = crate::editor::annotation::add_annotation(
            &mut editor,
            0,
            crate::editor::annotation::AnnotationBuilder::new(
                crate::editor::annotation::AnnotationType::Redact {
                    overlay_color: [0.0, 0.0, 0.0],
                },
                [100.0, 100.0, 300.0, 200.0],
            ),
        )
        .unwrap();

        let (_, mut page_dict) = editor.get_page_dict(0).unwrap();
        strip_redact_annots(&mut editor, &mut page_dict).unwrap();

        let annots = match page_dict.get("Annots") {
            Some(PdfObject::Array(a)) => a.clone(),
            _ => vec![],
        };

        let ids: Vec<u32> = annots
            .iter()
            .filter_map(|o| match o {
                PdfObject::Reference(id, _) => Some(*id),
                _ => None,
            })
            .collect();

        assert!(ids.contains(&text_id), "Text annotation must be kept");
        assert!(
            !ids.contains(&redact_id),
            "Redact annotation must be stripped"
        );
    }
}
