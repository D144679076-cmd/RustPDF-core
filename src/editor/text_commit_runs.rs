//! Multi-run write-back of a styled (rich-text) edited block.
//!
//! Where [`crate::editor::commit_block`] rewrites a block as a single `Tj`, a
//! formatted block is a sequence of [`StyleRun`](crate::editor::StyleRun)s, each
//! with its own colour/font/size. This module emits, in document order, one
//! operator group per run — `rg` (fill colour) / `Tf` (font) / `Tj` (show bytes),
//! coalescing redundant `rg`/`Tf` — plus an optional leading `Td` for alignment.
//!
//! Underline/strikethrough rects are drawn by the caller (`flush_and_cache` in
//! `wasm/text_edit.rs`) AFTER `commit_edit_session` rewrites `/Contents` to a
//! single reference, so the decoration layer survives the flush.
//!
//! Each run's bytes and final font key are **resolved by the caller** (the WASM
//! layer): encoding original-font runs needs the immutable document, while
//! embedding a substitute font for a changed family/bold/italic run needs
//! `&mut PdfEditor` — the two can't borrow simultaneously, so resolution happens
//! first and the ready-made [`ResolvedRun`]s flow in here.
//!
//! **Two-phase design:** like [`crate::editor::commit_block`], the content-stream
//! patch is deferred — [`commit_block_runs`] only mutates the in-memory model.
//! Call [`crate::editor::edit_session::commit_edit_session`] to flush.

use std::collections::HashSet;

use crate::content::operators::Operation;
use crate::editor::document_editor::PdfEditor;
use crate::editor::text_model::TextModel;
use crate::editor::text_style::{SyntheticStyle, OBLIQUE_SHEAR, SYNTHETIC_BOLD_STROKE_FRAC};
use crate::error::{PdfError, Result};
use crate::parser::objects::PdfObject;

/// A run whose bytes and PDF font key the caller has already resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRun {
    /// PDF `/Resources/Font` key to select for this run (original or `/EdN`).
    pub font_key: String,
    /// Font size in points.
    pub font_size: f64,
    /// Fill colour `[r, g, b]` (0.0–1.0).
    pub color: [f64; 3],
    /// Show-string bytes in the run's font (1-byte simple or 2-byte CID).
    pub bytes: Vec<u8>,
    /// Synthetic styling faked on the run's (original) glyphs — bold via a stroked
    /// outline, italic via a text-matrix shear. Empty for real bundled faces.
    pub synthetic: SyntheticStyle,
}

/// Per-run absolute-positioning data for the `Tm` emission path.
///
/// Auto-advance (PDF stepping the text cursor between `Tj`s) can't carry a
/// per-run synthetic-italic shear, so when any run is synthetic-italic the run
/// sequence positions each run with its own `Tm`. This carries the block's
/// primary text matrix and each run's starting text-space x so those `Tm`s can be
/// computed in a way that composes correctly with the in-content CTM.
#[derive(Debug, Clone, PartialEq)]
pub struct RunLayout {
    /// Block primary show op's text matrix (pre-CTM) as `[a, b, c, d, e, f]`.
    pub tm: [f64; 6],
    /// Starting text-space x advance of each run (sum of prior runs' text-space
    /// widths). Length must equal the run count.
    pub run_x_text: Vec<f64>,
    /// When `true`, always use per-run absolute `Tm` positioning (even if no
    /// synthetic-italic run is present). Needed when the base `tm` has had a stale
    /// shear stripped — without forcing the positioned path the old shear would
    /// survive in the stream unchanged.
    pub force_positioned: bool,
    /// When `true`, emit a leading `0 Tr` + `0 w` before the run loop so any
    /// stale synthetic-bold render-mode from a previous commit is cleared in the
    /// stream even when the current commit has no bold runs.
    pub reset_stroke: bool,
}

/// A decoration (underline / strikethrough) rectangle in page user-space.
#[derive(Debug, Clone, PartialEq)]
pub struct DecoRect {
    /// Left edge x (PDF points, origin bottom-left).
    pub x: f64,
    /// Bottom edge y (PDF points).
    pub y: f64,
    /// Width in points.
    pub width: f64,
    /// Height (line thickness) in points.
    pub height: f64,
    /// Fill colour `[r, g, b]` (0.0–1.0), matching the run's text colour.
    pub color: [f64; 3],
}

/// Build the replacement text operators for a block's style runs.
///
/// Emits per run `rg` (only when the colour changes) / `Tf` (only when the key or
/// size changes) / `Tj`, plus synthetic styling: a bold run is stroked
/// (`2 Tr` + line width, with `RG` matching the fill so the outline isn't black),
/// and an italic run is sheared.
///
/// **Positioning has two modes.** When no run is synthetic-italic, a leading
/// `Td align_dx 0` shifts the alignment origin and PDF auto-advances the text
/// cursor between runs (the block's surrounding `BT…ET` and origin `Tm` are left
/// in place by the caller). When at least one run is synthetic-italic, auto-advance
/// can't carry the per-run shear, so — given a [`RunLayout`] — each run is placed
/// with an absolute `Tm` derived from the block's text matrix and the run's
/// text-space x (with the shear folded into the matrix for italic runs, and
/// `align_dx` baked into the origin). `align_dx` is in text space.
pub fn build_run_ops(
    runs: &[ResolvedRun],
    align_dx: f64,
    layout: Option<&RunLayout>,
) -> Vec<Operation> {
    // Per-run absolute `Tm` is needed when:
    // - any run is synthetic-italic (shear must vary per run), OR
    // - force_positioned is set (stale shear stripped from base tm, must overwrite).
    let positioned = layout.filter(|l| {
        l.run_x_text.len() == runs.len()
            && (l.force_positioned || runs.iter().any(|r| r.synthetic.italic))
    });

    let mut ops: Vec<Operation> = Vec::with_capacity(runs.len() * 4 + 4);

    // Emit a leading stroke reset when the block previously had synthetic bold
    // (the old `2 Tr` + `w` may still be in the stream before the run position).
    if let Some(l) = layout {
        if l.reset_stroke {
            ops.push(Operation {
                operator: "Tr".to_owned(),
                operands: vec![PdfObject::Integer(0)],
            });
            ops.push(Operation {
                operator: "w".to_owned(),
                operands: vec![PdfObject::Real(0.0)],
            });
        }
    }

    // Alignment: a leading `Td` only in the auto-advance path; the positioned path
    // bakes `align_dx` into each run's `Tm` origin instead.
    if positioned.is_none() && align_dx.abs() > 1e-6 {
        ops.push(Operation {
            operator: "Td".to_owned(),
            operands: vec![PdfObject::Real(align_dx), PdfObject::Real(0.0)],
        });
    }

    let mut prev_color: Option<[f64; 3]> = None;
    let mut prev_font: Option<(String, f64)> = None;
    // Synthetic-bold state (stroke render mode + line width). `w` is graphics state
    // and persists past `ET`, so it must be reset after the run sequence.
    let mut stroke_active = false;
    let mut prev_stroke_w: Option<f64> = None;
    let mut any_stroke = false;

    for (i, r) in runs.iter().enumerate() {
        // Position: absolute `Tm` (with optional shear) in the positioned path.
        if let Some(l) = positioned {
            let [a, b, c, d, e, f] = l.tm;
            let cursor = align_dx + l.run_x_text[i];
            let e_i = e + cursor * a;
            let f_i = f + cursor * b;
            let (cc, dd) = if r.synthetic.italic {
                (c + OBLIQUE_SHEAR * a, d + OBLIQUE_SHEAR * b)
            } else {
                (c, d)
            };
            ops.push(Operation {
                operator: "Tm".to_owned(),
                operands: vec![
                    PdfObject::Real(a),
                    PdfObject::Real(b),
                    PdfObject::Real(cc),
                    PdfObject::Real(dd),
                    PdfObject::Real(e_i),
                    PdfObject::Real(f_i),
                ],
            });
        }

        if prev_color != Some(r.color) {
            ops.push(Operation {
                operator: "rg".to_owned(),
                operands: vec![
                    PdfObject::Real(r.color[0]),
                    PdfObject::Real(r.color[1]),
                    PdfObject::Real(r.color[2]),
                ],
            });
            prev_color = Some(r.color);
        }
        let font = (r.font_key.clone(), r.font_size);
        if prev_font.as_ref() != Some(&font) {
            ops.push(Operation {
                operator: "Tf".to_owned(),
                operands: vec![
                    PdfObject::Name(r.font_key.clone()),
                    PdfObject::Real(r.font_size),
                ],
            });
            prev_font = Some(font);
        }

        // Synthetic bold: stroke the glyph outline (text render mode 2). Match the
        // stroke colour to the fill (`RG`) and scale the line width to the font
        // size so the weight reads consistently.
        if r.synthetic.bold {
            let lw = r.font_size * SYNTHETIC_BOLD_STROKE_FRAC;
            if !stroke_active {
                ops.push(Operation {
                    operator: "RG".to_owned(),
                    operands: vec![
                        PdfObject::Real(r.color[0]),
                        PdfObject::Real(r.color[1]),
                        PdfObject::Real(r.color[2]),
                    ],
                });
                ops.push(Operation {
                    operator: "Tr".to_owned(),
                    operands: vec![PdfObject::Integer(2)],
                });
                stroke_active = true;
            }
            if prev_stroke_w != Some(lw) {
                ops.push(Operation {
                    operator: "w".to_owned(),
                    operands: vec![PdfObject::Real(lw)],
                });
                prev_stroke_w = Some(lw);
            }
            any_stroke = true;
        } else if stroke_active {
            // Back to fill-only for a non-bold run.
            ops.push(Operation {
                operator: "Tr".to_owned(),
                operands: vec![PdfObject::Integer(0)],
            });
            stroke_active = false;
        }

        ops.push(Operation {
            operator: "Tj".to_owned(),
            operands: vec![PdfObject::String(r.bytes.clone())],
        });
    }

    // Reset trailing synthetic-bold state: text render mode back to fill, and the
    // line width back to 0 (graphics state leaks past `ET` otherwise).
    if stroke_active {
        ops.push(Operation {
            operator: "Tr".to_owned(),
            operands: vec![PdfObject::Integer(0)],
        });
    }
    if any_stroke {
        ops.push(Operation {
            operator: "w".to_owned(),
            operands: vec![PdfObject::Real(0.0)],
        });
    }
    ops
}

/// Inline content operators that draw `decorations` as filled rectangles.
///
/// Emits, per rect, an isolated graphics group `q / rg / re / f / Q` in page
/// user-space (the same space `DecoRect` is expressed in). Used by the live
/// preview to append underline/strikethrough to the rendered tile; commit uses a
/// separate appended layer instead. Returns an empty vec when there are no rects.
pub fn build_decoration_ops(decorations: &[DecoRect]) -> Vec<Operation> {
    let mut ops: Vec<Operation> = Vec::with_capacity(decorations.len() * 5);
    for d in decorations {
        ops.push(Operation {
            operator: "q".to_owned(),
            operands: vec![],
        });
        ops.push(Operation {
            operator: "rg".to_owned(),
            operands: vec![
                PdfObject::Real(d.color[0]),
                PdfObject::Real(d.color[1]),
                PdfObject::Real(d.color[2]),
            ],
        });
        ops.push(Operation {
            operator: "re".to_owned(),
            operands: vec![
                PdfObject::Real(d.x),
                PdfObject::Real(d.y),
                PdfObject::Real(d.width),
                PdfObject::Real(d.height),
            ],
        });
        ops.push(Operation {
            operator: "f".to_owned(),
            operands: vec![],
        });
        ops.push(Operation {
            operator: "Q".to_owned(),
            operands: vec![],
        });
    }
    ops
}

/// Patch block `block_id`'s show-operator span with `run_ops`.
///
/// The block's leading show operator is replaced in place by `run_ops`; the
/// block's other show operators are dropped (their text moved into the run
/// sequence). Positioning/state operators (`Tm`/`Td`/the original `Tf`) are
/// untouched.
///
/// **Content stream flush is deferred** — only the in-memory model is mutated.
/// Call [`crate::editor::edit_session::commit_edit_session`] to persist.
/// Decoration rects are drawn by the caller AFTER the session flush so they
/// survive the `/Contents` single-ref rewrite (see `flush_and_cache`).
/// Returns `InvalidStructure` for an unknown block id or an empty show-op span.
pub fn commit_block_runs(
    _editor: &mut PdfEditor,
    model: &mut TextModel,
    page_index: usize,
    block_id: usize,
    run_ops: &[Operation],
) -> Result<()> {
    let block = model
        .blocks
        .iter()
        .find(|b| b.id == block_id)
        .ok_or_else(|| {
            PdfError::invalid_structure(format!("commit_block_runs: unknown block id {block_id}"))
        })?;
    let stream_idx = block.stream_idx;
    let frame_ids = block.frame_ids.clone();

    let op_indices: Vec<usize> = frame_ids
        .iter()
        .filter_map(|&fid| model.session.frames.get(fid).map(|f| f.stream_op_index))
        .collect();
    let primary = *op_indices.iter().min().ok_or_else(|| {
        PdfError::invalid_structure("commit_block_runs: block has no show operators")
    })?;
    let others: HashSet<usize> = op_indices
        .iter()
        .copied()
        .filter(|&i| i != primary)
        .collect();

    let stream = model.session.streams.get_mut(stream_idx).ok_or_else(|| {
        PdfError::invalid_structure("commit_block_runs: stream index out of range")
    })?;

    // Rebuild the op vector: the primary show op becomes the run sequence; the
    // block's other show ops are dropped; everything else is preserved verbatim.
    let old = std::mem::take(&mut stream.ops);
    let mut new_ops: Vec<Operation> = Vec::with_capacity(old.len() + run_ops.len());
    for (i, op) in old.into_iter().enumerate() {
        if i == primary {
            new_ops.extend(run_ops.iter().cloned());
        } else if others.contains(&i) {
            // dropped: text folded into the run sequence
        } else {
            new_ops.push(op);
        }
    }
    stream.ops = new_ops;

    // Mark dirty so the deferred flush persists the patched stream (see
    // commit_block). Decoration rects are drawn by the caller (flush_and_cache)
    // AFTER commit_edit_session rewrites /Contents, so they survive the flush.
    let _ = page_index; // page_index reserved for future use; decorations now caller-drawn
    model.session.dirty = true;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::build_text_model;
    use crate::editor::document_editor::PdfEditor;
    use crate::editor::edit_session::commit_edit_session;

    fn run(font_key: &str, size: f64, color: [f64; 3], bytes: &[u8]) -> ResolvedRun {
        ResolvedRun {
            font_key: font_key.to_owned(),
            font_size: size,
            color,
            bytes: bytes.to_vec(),
            synthetic: SyntheticStyle::default(),
        }
    }

    fn op_names(ops: &[Operation]) -> Vec<&str> {
        ops.iter().map(|o| o.operator.as_str()).collect()
    }

    #[test]
    fn build_decoration_ops_empty_is_empty() {
        assert!(build_decoration_ops(&[]).is_empty());
    }

    #[test]
    fn build_decoration_ops_emits_q_rg_re_f_q_per_rect() {
        let rects = [
            DecoRect {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 1.5,
                color: [1.0, 0.0, 0.0],
            },
            DecoRect {
                x: 5.0,
                y: 40.0,
                width: 12.0,
                height: 0.8,
                color: [0.0, 0.0, 0.0],
            },
        ];
        let ops = build_decoration_ops(&rects);
        // Each rect → q / rg / re / f / Q (5 ops), isolated graphics group.
        assert_eq!(
            op_names(&ops),
            vec!["q", "rg", "re", "f", "Q", "q", "rg", "re", "f", "Q"]
        );
        // First rect's `re` carries x,y,w,h; its `rg` carries the colour.
        assert_eq!(
            ops[2].operands,
            vec![
                PdfObject::Real(10.0),
                PdfObject::Real(20.0),
                PdfObject::Real(30.0),
                PdfObject::Real(1.5),
            ]
        );
        assert_eq!(
            ops[1].operands,
            vec![
                PdfObject::Real(1.0),
                PdfObject::Real(0.0),
                PdfObject::Real(0.0),
            ]
        );
    }

    /// Re-open a saved PDF and return page 0's decoded (decompressed) content,
    /// concatenating every stream in `/Contents`. The committed content is
    /// FlateDecode-compressed in the output, so assertions decode first.
    fn decoded_page_content(out: &[u8]) -> String {
        use crate::document::catalog::Catalog;
        use crate::document::page::Page;
        let doc = crate::parser::objects::PdfDocument::parse(out.to_vec()).expect("parse");
        let catalog = Catalog::from_document(&doc).expect("catalog");
        let page_dict = catalog.get_page_dict(&doc, 0).expect("page dict");
        let page = Page::from_dict(&doc, &page_dict).expect("page");
        let bytes = page.decode_contents(&doc).expect("decode");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn build_run_ops_single_run_emits_one_group() {
        let ops = build_run_ops(&[run("F1", 12.0, [0.0, 0.0, 0.0], b"Hello")], 0.0, None);
        assert_eq!(op_names(&ops), vec!["rg", "Tf", "Tj"]);
    }

    #[test]
    fn build_run_ops_two_runs_emit_color_change_between() {
        let ops = build_run_ops(
            &[
                run("F1", 12.0, [0.0, 0.0, 0.0], b"Hello"),
                run("F1", 12.0, [1.0, 0.0, 0.0], b"World"),
            ],
            0.0,
            None,
        );
        // Same font → Tf emitted once; colour changes → rg twice.
        assert_eq!(op_names(&ops), vec!["rg", "Tf", "Tj", "rg", "Tj"]);
    }

    #[test]
    fn build_run_ops_alignment_prepends_td() {
        let ops = build_run_ops(&[run("F1", 12.0, [0.0, 0.0, 0.0], b"Hi")], 25.0, None);
        assert_eq!(op_names(&ops), vec!["Td", "rg", "Tf", "Tj"]);
        match &ops[0].operands[0] {
            PdfObject::Real(v) => assert!((v - 25.0).abs() < 1e-9),
            other => panic!("expected Real, got {other:?}"),
        }
    }

    /// A synthetic run helper: like `run`, with the given synthetic styling.
    fn srun(font_key: &str, size: f64, bytes: &[u8], bold: bool, italic: bool) -> ResolvedRun {
        ResolvedRun {
            synthetic: SyntheticStyle { bold, italic },
            ..run(font_key, size, [0.0, 0.0, 0.0], bytes)
        }
    }

    #[test]
    fn build_run_ops_synthetic_italic_emits_skew_tm() {
        // Identity text matrix translated to (50, 700).
        let layout = RunLayout {
            tm: [1.0, 0.0, 0.0, 1.0, 50.0, 700.0],
            run_x_text: vec![0.0],
            force_positioned: false,
            reset_stroke: false,
        };
        let ops = build_run_ops(&[srun("F1", 12.0, b"Hi", false, true)], 0.0, Some(&layout));
        let tm = ops
            .iter()
            .find(|o| o.operator == "Tm")
            .expect("synthetic-italic run emits a Tm");
        // c term = c0 + shear·a = 0 + OBLIQUE_SHEAR·1.
        match (&tm.operands[2], &tm.operands[4], &tm.operands[5]) {
            (PdfObject::Real(c), PdfObject::Real(e), PdfObject::Real(f)) => {
                assert!((c - OBLIQUE_SHEAR).abs() < 1e-9, "shear in c: {c}");
                assert!((e - 50.0).abs() < 1e-9, "origin x preserved");
                assert!((f - 700.0).abs() < 1e-9, "origin y preserved");
            }
            other => panic!("unexpected Tm operands: {other:?}"),
        }
    }

    #[test]
    fn build_run_ops_synthetic_bold_emits_tr2_and_w_then_resets() {
        let ops = build_run_ops(&[srun("F1", 10.0, b"Hi", true, false)], 0.0, None);
        // rg, Tf, then bold setup (RG, Tr 2, w), Tj, then trailing reset (Tr 0, w 0).
        assert_eq!(
            op_names(&ops),
            vec!["rg", "Tf", "RG", "Tr", "w", "Tj", "Tr", "w"]
        );
        // First Tr is render mode 2, trailing Tr is 0.
        let trs: Vec<&Operation> = ops.iter().filter(|o| o.operator == "Tr").collect();
        assert_eq!(trs[0].operands, vec![PdfObject::Integer(2)]);
        assert_eq!(trs[1].operands, vec![PdfObject::Integer(0)]);
        // Stroke line width scales with the font size; trailing w resets to 0.
        let ws: Vec<&Operation> = ops.iter().filter(|o| o.operator == "w").collect();
        match &ws[0].operands[0] {
            PdfObject::Real(v) => assert!((v - 10.0 * 0.03).abs() < 1e-9, "lw: {v}"),
            other => panic!("expected Real, got {other:?}"),
        }
        assert_eq!(ws[1].operands, vec![PdfObject::Real(0.0)]);
    }

    #[test]
    fn build_run_ops_mixed_runs_isolate_state() {
        // [plain, bold, plain]: the third run must NOT inherit the stroke state.
        let runs = [
            run("F1", 10.0, [0.0, 0.0, 0.0], b"a"),
            srun("F1", 10.0, b"b", true, false),
            run("F1", 10.0, [0.0, 0.0, 0.0], b"c"),
        ];
        let ops = build_run_ops(&runs, 0.0, None);
        // Exactly one stroke entry, and a Tr 0 reset appears before the 3rd Tj.
        assert_eq!(ops.iter().filter(|o| o.operator == "RG").count(), 1);
        let tj_idx: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter(|(_, o)| o.operator == "Tj")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(tj_idx.len(), 3);
        // Between the 2nd and 3rd Tj there is a `Tr 0` (back to fill-only).
        let reset = ops[tj_idx[1]..tj_idx[2]]
            .iter()
            .any(|o| o.operator == "Tr" && o.operands == vec![PdfObject::Integer(0)]);
        assert!(
            reset,
            "third run resets render mode to fill: {:?}",
            op_names(&ops)
        );
    }

    #[test]
    fn build_run_ops_synthetic_keeps_original_font_key() {
        // A synthetic run shows with the block's own font key (no substitute swap).
        let ops = build_run_ops(&[srun("F1", 12.0, b"Hi", true, true)], 0.0, None);
        let tf = ops.iter().find(|o| o.operator == "Tf").expect("Tf present");
        assert_eq!(tf.operands[0], PdfObject::Name("F1".to_owned()));
    }

    // Minimal one-page PDF with a single Tj using WinAnsi Helvetica.
    fn simple_pdf() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 72 700 Td (Hello) Tj ET";
        let mut objs: Vec<String> = Vec::new();
        objs.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
        objs.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string());
        objs.push(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_string(),
        );
        objs.push(format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            std::str::from_utf8(content).unwrap()
        ));
        objs.push(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>"
                .to_string(),
        );

        let mut pdf = String::from("%PDF-1.7\n");
        let mut offsets = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, body));
        }
        let xref_pos = pdf.len();
        pdf.push_str(&format!("xref\n0 {}\n", objs.len() + 1));
        pdf.push_str("0000000000 65535 f \n");
        for off in &offsets {
            pdf.push_str(&format!("{:010} 00000 n \n", off));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            objs.len() + 1,
            xref_pos
        ));
        pdf.into_bytes()
    }

    #[test]
    fn commit_block_runs_two_run_block_saves_expected_sequence() {
        let bytes = simple_pdf();
        let mut editor = PdfEditor::open(bytes.clone()).expect("open");
        let mut model = build_text_model(&editor.doc, 0).expect("model");
        let block = model
            .blocks
            .iter()
            .find(|b| b.text.contains("Hello"))
            .expect("Hello block")
            .id;

        // Split "Hello" into "Hel" (black) + "lo" (red), WinAnsi == ASCII bytes.
        let run_ops = build_run_ops(
            &[
                run("F1", 24.0, [0.0, 0.0, 0.0], b"Hel"),
                run("F1", 24.0, [1.0, 0.0, 0.0], b"lo"),
            ],
            0.0,
            None,
        );
        commit_block_runs(&mut editor, &mut model, 0, block, &run_ops).expect("commit");
        commit_edit_session(&mut editor, 0, &model.session).expect("flush");

        let out = editor.save_append(&bytes).expect("save");
        let content = decoded_page_content(&out);
        // Expect, in order: black Hel then red lo.
        let hel = content.find("(Hel)").expect("(Hel) present");
        let lo = content.find("(lo)").expect("(lo) present");
        assert!(hel < lo, "Hel must precede lo");
        assert!(
            content.contains("1 0 0 rg"),
            "red rg for second run: {content}"
        );
    }

    #[test]
    fn commit_block_runs_unknown_id_errors() {
        let bytes = simple_pdf();
        let mut editor = PdfEditor::open(bytes).expect("open");
        let mut model = build_text_model(&editor.doc, 0).expect("model");
        let ops = build_run_ops(&[run("F1", 24.0, [0.0, 0.0, 0.0], b"x")], 0.0, None);
        let err = commit_block_runs(&mut editor, &mut model, 0, 9999, &ops).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    // Full embed + multi-run + save path (what the WASM orchestration drives):
    // a "Hel" run in the original font + a bold "lo" run embedded as a Type0
    // font, committed and saved — assert the PDF reparses and gains the bold
    // font key in a Tf for the second run.
    #[cfg(feature = "render")]
    #[test]
    fn commit_block_runs_embedded_bold_run_persists() {
        use crate::render::font_resolver::{EmbeddedFontResolver, FontResolver};
        use crate::writer::font_subset::embed_cidfont_for_chars;

        let bytes = simple_pdf();
        let mut editor = PdfEditor::open(bytes.clone()).expect("open");
        let mut model = build_text_model(&editor.doc, 0).expect("model");
        let block = model
            .blocks
            .iter()
            .find(|b| b.text.contains("Hello"))
            .expect("Hello block")
            .id;

        // Embed a bold Helvetica face covering the second run and register it.
        let ttf = EmbeddedFontResolver
            .resolve("Helvetica-Bold", true, false)
            .expect("bundled bold face");
        let chars: Vec<char> = "lo".chars().collect();
        let embedded =
            embed_cidfont_for_chars(&mut editor, &ttf, "Helvetica-Bold", &chars).expect("embed");
        let bold_key =
            crate::editor::register_page_font(&mut editor, 0, embedded.font_id).expect("register");

        let resolved = vec![
            run("F1", 24.0, [0.0, 0.0, 0.0], b"Hel"),
            ResolvedRun {
                font_key: bold_key.clone(),
                font_size: 24.0,
                color: [0.0, 0.0, 0.0],
                bytes: embedded.encode("lo"),
                synthetic: SyntheticStyle::default(),
            },
        ];
        let run_ops = build_run_ops(&resolved, 0.0, None);
        commit_block_runs(&mut editor, &mut model, 0, block, &run_ops).expect("commit");
        commit_edit_session(&mut editor, 0, &model.session).expect("flush");

        let out = editor.save_append(&bytes).expect("save");
        // Reparses cleanly (valid incremental update).
        let content = decoded_page_content(&out);
        assert!(
            content.contains("(Hel)"),
            "original-font run present: {content}"
        );
        assert!(
            content.contains(&format!("/{bold_key}")),
            "bold run switches to the embedded font key: {content}"
        );
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("/Type0"), "saved PDF gains a Type0 font");
    }
}
