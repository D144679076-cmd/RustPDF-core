//! Logical text model for Word-style PDF editing.
//!
//! Wraps [`build_edit_session`] and groups its per-show [`EditableFrame`]s into
//! baseline-clustered [`EditBlock`]s — the unit the text-edit engine operates on
//! (caret, selection, single-line reflow). Each block records the source
//! operator range so the Phase-2 writer can replace it surgically.

use std::collections::HashMap;

use crate::content::graphics_state::Matrix;
use crate::content::interpreter::strip_subset_prefix;
use crate::editor::edit_session::{build_edit_session, EditSession, EditableFrame, FilledRect};
use crate::editor::text_commit_runs::DecoRect;
use crate::editor::text_shape::{
    font_metrics_for, font_style_for, is_composite_font, text_width, PdfFontMetrics,
};
use crate::editor::text_style::{
    decoration_thickness, matrix_shear, strike_offset, underline_offset, OBLIQUE_SHEAR,
    OBLIQUE_SHEAR_TOL,
};
use crate::error::Result;
use crate::parser::objects::PdfDocument;

/// One editable text block: a run of show operators sharing a baseline and font.
#[derive(Debug, Clone)]
pub struct EditBlock {
    /// Sequential id used by the host to address this block.
    pub id: usize,
    /// Concatenated Unicode text of the block, in reading order.
    pub text: String,
    /// Left edge x of the block in PDF user-space (origin bottom-left).
    pub x: f64,
    /// Baseline y of the block in PDF user-space.
    pub y: f64,
    /// Total advance width of the block in user-space points.
    pub width: f64,
    /// Font size in points.
    pub font_size: f64,
    /// Raw PDF font resource key (e.g. `"F1"`).
    pub font_key: String,
    /// Resolved `/BaseFont` name (e.g. `"Helvetica-Bold"`, `"ABCDEF+Calibri"`).
    pub font_name: String,
    /// Human-facing font name for the picker: `font_name` with any subset prefix
    /// stripped (e.g. `"Calibri"`). Falls back to `font_name` when there's no tag.
    pub display_font: String,
    /// Whether the block's font is intrinsically bold (from the FontDescriptor).
    /// Seeds the editor's CharStyle so the panel reflects the PDF on open.
    pub bold: bool,
    /// Whether the block's font is intrinsically italic (from the FontDescriptor).
    pub italic: bool,
    /// Index into [`EditSession::streams`] that this block lives in.
    pub stream_idx: usize,
    /// Inclusive operator-index range in that stream that produced the block.
    pub op_range: (usize, usize),
    /// Frame ids (into [`EditSession::frames`]) composing the block, in order.
    pub frame_ids: Vec<usize>,
    /// Text→page horizontal scale at the block (from the show op's `tm·ctm`).
    /// Multiply a text-space width by this to get the on-page width.
    pub scale_x: f64,
    /// Whether the block's font is composite (Type0/CID). The host uses this to
    /// pick the preview path: CID blocks fall back to a Canvas2D preview of the
    /// decoded text until pixel-exact CID re-encoding (Phase B) lands.
    pub composite: bool,
    /// The text matrix (`Tm`, *pre*-CTM) of the block's primary show op.
    ///
    /// Lets the commit/preview path re-emit per-run `Tm` operators (for synthetic
    /// italic shear or multi-run positioning) that compose correctly with the
    /// in-content CTM, since `x`/`y` alone fold the CTM in and can't be inverted.
    /// When `synthetic_italic` is true, the shear component has already been
    /// stripped so re-commit applies exactly one shear.
    pub(crate) tm: Matrix,
    /// Whether the block carries synthetic italic (a `Tm` shear in the content
    /// stream) that is not intrinsic to the font (i.e. `italic == false`).
    pub synthetic_italic: bool,
    /// Whether the block carries synthetic bold (text render mode 2 `Tr` in the
    /// content stream) that is not intrinsic to the font.
    pub synthetic_bold: bool,
    /// Whether a matching underline rect was found in the decoration stream.
    pub underline: bool,
    /// Whether a matching strikethrough rect was found in the decoration stream.
    pub strike: bool,
    /// Decoration rects matched to this block (for regenerating the decoration
    /// layer on commit without clobbering other blocks' decorations).
    pub decorations: Vec<DecoRect>,
}

/// All editable blocks on a page plus the underlying edit session.
///
/// The retained `session` carries the parsed content streams needed to write
/// edits back (Phase 2 commit).
pub struct TextModel {
    /// Editable blocks in document order.
    pub blocks: Vec<EditBlock>,
    /// The session the blocks were derived from.
    pub session: EditSession,
}

/// Flattened frame data used purely for grouping (decoupled from PDF access so
/// the grouping rule can be unit-tested).
#[derive(Debug, Clone)]
pub(crate) struct FrameInput {
    pub stream_idx: usize,
    pub op_idx: usize,
    pub x: f64,
    pub y: f64,
    pub font_size: f64,
    pub width: f64,
    pub scale_x: f64,
    pub resource_key: String,
    pub text: String,
}

/// Build the editable text model for `page_index`.
///
/// Extracts frames via [`build_edit_session`], measures each frame's width with
/// the resolved font metrics, then groups frames into [`EditBlock`]s. Returns a
/// model with an empty `blocks` list for scanned/image-only pages.
pub fn build_text_model(doc: &PdfDocument, page_index: usize) -> Result<TextModel> {
    let session = build_edit_session(doc, page_index)?;

    // Cache metrics per (resource_key, font_size) — building them scans 256 codes.
    // Font size is keyed by its bit pattern (via the free fn) so identical sizes
    // share an entry without relying on f64 inherent methods.
    let mut cache: HashMap<(String, u64), Option<PdfFontMetrics>> = HashMap::new();
    let mut inputs: Vec<FrameInput> = Vec::with_capacity(session.frames.len());
    for f in &session.frames {
        let key = (f.resource_key.clone(), f64::to_bits(f.font_size));
        let metrics = cache.entry(key).or_insert_with(|| {
            font_metrics_for(doc, page_index, &f.resource_key, f.font_size)
                .ok()
                .flatten()
        });
        // Glyph advances are measured in text space; `f.x` is already page-space
        // (CTM-applied). Scale the width by the show op's text→page horizontal
        // scale so a page-level `cm [0.75 …]` doesn't leave the box oversized.
        let text_w = match metrics.as_ref() {
            Some(m) => text_width(m, &f.text),
            None => estimate_width(&f.text, f.font_size),
        };
        let width = text_w * f.scale_x;
        inputs.push(FrameInput {
            stream_idx: f.stream_idx,
            op_idx: f.stream_op_index,
            x: f.x,
            y: f.y,
            font_size: f.font_size,
            width,
            scale_x: f.scale_x,
            resource_key: f.resource_key.clone(),
            text: f.text.clone(),
        });
    }

    // Composite (Type0/CID) flag + intrinsic style per resource key, cached.
    let mut composite_cache: HashMap<String, bool> = HashMap::new();
    let mut style_cache: HashMap<String, (bool, bool, Option<String>)> = HashMap::new();

    let groups = group_blocks(&inputs);
    let mut blocks: Vec<EditBlock> = groups
        .into_iter()
        .enumerate()
        .map(|(id, members)| {
            let key = &inputs[members[0]].resource_key;
            let composite = *composite_cache
                .entry(key.clone())
                .or_insert_with(|| is_composite_font(doc, page_index, key));
            // Intrinsic bold/italic + base font from the FontDescriptor.
            let (bold, italic, base_font) = style_cache
                .entry(key.clone())
                .or_insert_with(|| match font_style_for(doc, page_index, key) {
                    Some(s) => (s.bold, s.italic, Some(s.base_font)),
                    None => (false, false, None),
                })
                .clone();
            build_block(
                id,
                &members,
                &inputs,
                &session.frames,
                composite,
                bold,
                italic,
                base_font,
            )
        })
        .collect();

    // Match filled rects from the session to blocks for underline/strike detection.
    match_decorations_to_blocks(&mut blocks, &session.rects);

    Ok(TextModel { blocks, session })
}

/// Width fallback when a font cannot be resolved (proportional estimate).
fn estimate_width(text: &str, font_size: f64) -> f64 {
    text.chars().count() as f64 * 0.5 * font_size
}

/// Match filled rectangles to blocks as underline/strike decorations.
///
/// For each block computes expected underline and strike geometry and checks
/// every collected rect. Strict bounds (x/width overlap + y proximity + thickness)
/// guard against matching table rules or other page graphics.
fn match_decorations_to_blocks(blocks: &mut [EditBlock], rects: &[FilledRect]) {
    for block in blocks.iter_mut() {
        let fs = block.font_size;
        if fs <= 0.0 || rects.is_empty() {
            continue;
        }
        let thick = decoration_thickness(fs);
        // Tolerance: allow up to one line-thickness of position drift.
        let pos_tol = thick.max(1.0);
        // Expected underline/strike y-center for this block.
        let ul_y = block.y - underline_offset(fs);
        let st_y = block.y + strike_offset(fs);
        // Block x-extent (with a small slack for rounding).
        let bx0 = block.x - 2.0;
        let bx1 = block.x + block.width + 2.0;
        let min_cover = block.width * 0.6;

        for rect in rects {
            // Thickness guard: rect height must be within one tolerance of expected.
            if (rect.height - thick).abs() > pos_tol {
                continue;
            }
            // x-extent guard: rect must overlap the block x-span substantially.
            let rx0 = rect.x;
            let rx1 = rect.x + rect.width;
            let overlap_x = rx1.min(bx1) - rx0.max(bx0);
            if overlap_x < min_cover.min(rect.width * 0.9) {
                continue;
            }
            // Check underline y.
            if (rect.y - ul_y).abs() < pos_tol && !block.underline {
                block.underline = true;
                block.decorations.push(DecoRect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    color: rect.color,
                });
            }
            // Check strikethrough y.
            if (rect.y - st_y).abs() < pos_tol && !block.strike {
                block.strike = true;
                block.decorations.push(DecoRect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    color: rect.color,
                });
            }
        }
    }
}

/// Group frames (in document order) into blocks sharing a baseline and font.
///
/// Returns, for each block, the indices of its member frames. The rule mirrors
/// the web overlay's clustering: same stream + same font resource, vertical
/// drift within `0.4·font_size`, and a horizontal gap within
/// `[-0.1·font_size, 2.0·font_size]` of the previous frame's right edge.
pub(crate) fn group_blocks(frames: &[FrameInput]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let joins = match groups.last() {
            Some(group) => {
                let prev = &frames[*group.last().expect("group is non-empty")];
                let same_stream = prev.stream_idx == f.stream_idx;
                let same_font = prev.resource_key == f.resource_key;
                let same_baseline = (f.y - prev.y).abs() <= prev.font_size * 0.4;
                let gap = f.x - (prev.x + prev.width);
                let near = gap >= -(prev.font_size * 0.1) && gap <= prev.font_size * 2.0;
                same_stream && same_font && same_baseline && near
            }
            None => false,
        };
        if joins {
            groups.last_mut().expect("group exists").push(i);
        } else {
            groups.push(vec![i]);
        }
    }
    groups
}

/// Assemble an [`EditBlock`] from its member frame indices.
#[allow(clippy::too_many_arguments)]
fn build_block(
    id: usize,
    members: &[usize],
    inputs: &[FrameInput],
    frames: &[EditableFrame],
    composite: bool,
    bold: bool,
    italic: bool,
    base_font: Option<String>,
) -> EditBlock {
    let first = &inputs[members[0]];
    let text: String = members.iter().map(|&m| inputs[m].text.as_str()).collect();
    // Width must stop at the last *inked* glyph: a trailing-space frame would push
    // the right edge (and thus the white cover + render tile) far past the visible
    // text, making the edit box overrun the page. Use the right edge of the last
    // member whose text isn't all whitespace; fall back to the final member.
    let last_inked = members
        .iter()
        .rev()
        .map(|&m| &inputs[m])
        .find(|fi| !fi.text.trim().is_empty())
        .unwrap_or(&inputs[*members.last().expect("block has members")]);
    let width = (last_inked.x + last_inked.width) - first.x;
    let op_min = members.iter().map(|&m| inputs[m].op_idx).min().unwrap_or(0);
    let op_max = members.iter().map(|&m| inputs[m].op_idx).max().unwrap_or(0);
    // Prefer the descriptor's /BaseFont; fall back to the frame's resolved name.
    let frame_font_name = frames
        .get(members[0])
        .map(|f| f.font_name.clone())
        .unwrap_or_default();
    let font_name = match &base_font {
        Some(b) if !b.is_empty() => b.clone(),
        _ => frame_font_name,
    };
    let display_font = strip_subset_prefix(&font_name).to_owned();

    // Detect synthetic italic from the primary frame's text matrix shear.
    // Strip the shear from the stored base `tm` so re-commit applies exactly one.
    let primary_frame = frames.get(members[0]);
    let raw_tm = primary_frame.map(|f| f.tm).unwrap_or_else(Matrix::identity);
    let shear = matrix_shear(raw_tm.a, raw_tm.b, raw_tm.c, raw_tm.d);
    let synthetic_italic = !italic && (shear - OBLIQUE_SHEAR).abs() < OBLIQUE_SHEAR_TOL;
    let mut tm = raw_tm;
    if synthetic_italic {
        // Strip the shear so re-commit doesn't double-apply it.
        tm.c -= OBLIQUE_SHEAR * tm.a;
        tm.d -= OBLIQUE_SHEAR * tm.b;
    }

    // Detect synthetic bold from the primary frame's render mode.
    let primary_render_mode = primary_frame.map(|f| f.render_mode).unwrap_or(0);
    let synthetic_bold = !bold && matches!(primary_render_mode, 1 | 2);

    log::debug!(
        "[build-block] id={} key={} font_name={:?} display={:?} size={:.3} scale_x={:.3} bold={} italic={} syn_italic={} syn_bold={} composite={}",
        id,
        first.resource_key,
        font_name,
        display_font,
        first.font_size,
        first.scale_x,
        bold,
        italic,
        synthetic_italic,
        synthetic_bold,
        composite,
    );

    EditBlock {
        id,
        text,
        x: first.x,
        y: first.y,
        width,
        font_size: first.font_size,
        font_key: first.resource_key.clone(),
        font_name,
        display_font,
        bold,
        italic,
        stream_idx: first.stream_idx,
        op_range: (op_min, op_max),
        frame_ids: members.to_vec(),
        scale_x: first.scale_x,
        composite,
        tm,
        synthetic_italic,
        synthetic_bold,
        // underline/strike/decorations are set after block building via rect-matching.
        underline: false,
        strike: false,
        decorations: Vec::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fi(stream_idx: usize, op_idx: usize, x: f64, y: f64, key: &str, text: &str) -> FrameInput {
        FrameInput {
            stream_idx,
            op_idx,
            x,
            y,
            font_size: 10.0,
            width: 20.0,
            scale_x: 1.0,
            resource_key: key.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn adjacent_same_line_frames_group() {
        // f0 right edge = 10+20 = 30; f1 starts at 32 → gap 2 (< 20) → joins.
        let frames = vec![
            fi(0, 0, 10.0, 700.0, "F1", "Hel"),
            fi(0, 1, 32.0, 700.0, "F1", "lo"),
        ];
        let groups = group_blocks(&frames);
        assert_eq!(groups, vec![vec![0, 1]]);
    }

    #[test]
    fn different_baseline_splits() {
        let frames = vec![
            fi(0, 0, 10.0, 700.0, "F1", "A"),
            fi(0, 1, 32.0, 680.0, "F1", "B"),
        ];
        let groups = group_blocks(&frames);
        assert_eq!(groups, vec![vec![0], vec![1]]);
    }

    #[test]
    fn different_font_splits() {
        let frames = vec![
            fi(0, 0, 10.0, 700.0, "F1", "A"),
            fi(0, 1, 32.0, 700.0, "F2", "B"),
        ];
        let groups = group_blocks(&frames);
        assert_eq!(groups, vec![vec![0], vec![1]]);
    }

    #[test]
    fn large_gap_splits() {
        // f1 starts at 200 → gap 170 ≫ 2·font_size → new block.
        let frames = vec![
            fi(0, 0, 10.0, 700.0, "F1", "A"),
            fi(0, 1, 200.0, 700.0, "F1", "B"),
        ];
        let groups = group_blocks(&frames);
        assert_eq!(groups, vec![vec![0], vec![1]]);
    }

    #[test]
    fn build_block_concatenates_and_spans() {
        let inputs = vec![
            fi(0, 3, 10.0, 700.0, "F1", "Hel"),
            fi(0, 5, 32.0, 700.0, "F1", "lo"),
        ];
        let frames: Vec<EditableFrame> = inputs
            .iter()
            .map(|i| EditableFrame {
                id: 0,
                text: i.text.clone(),
                x: i.x,
                y: i.y,
                font_size: i.font_size,
                font_name: "Helvetica".to_owned(),
                resource_key: i.resource_key.clone(),
                stream_idx: i.stream_idx,
                stream_op_index: i.op_idx,
                scale_x: i.scale_x,
                tm: Matrix::identity(),
                render_mode: 0,
            })
            .collect();
        let block = build_block(0, &[0, 1], &inputs, &frames, false, false, false, None);
        assert_eq!(block.text, "Hello");
        assert_eq!(block.op_range, (3, 5));
        assert_eq!(block.x, 10.0);
        // width = (32 + 20) - 10 = 42
        assert!((block.width - 42.0).abs() < 1e-9);
        assert_eq!(block.frame_ids, vec![0, 1]);
    }

    #[test]
    fn empty_frames_no_blocks() {
        assert!(group_blocks(&[]).is_empty());
    }
}
