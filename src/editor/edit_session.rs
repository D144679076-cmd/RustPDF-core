//! ONLYOFFICE-style edit session for PDF text frames.
//!
//! Parse a page's content stream into indexed `EditableFrame`s once on
//! edit-mode entry.  Commits patch the stream at the pre-indexed operator
//! position — no position-matching scan needed on each edit.
//!
//! Recursively descends into Form XObjects so text that lives in `/Do`-invoked
//! sub-streams is also editable.

use std::collections::HashSet;

use crate::content::graphics_state::Matrix;
use crate::content::operators::{parse_content_stream, Operation};
use crate::document::catalog::{resolve_inherited_attribute, Catalog};
use crate::editor::document_editor::PdfEditor;
use crate::editor::text_editor::{encode_pdf_string, resolve_font_name, serialize_operations};
use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};
use crate::writer::streams::make_flate_stream;

const MAX_XOBJECT_DEPTH: usize = 10;

/// Raw extracted-frame tuple before resolution into an `EditableFrame`:
/// `(stream_idx, op_idx, text, x, y, font_size, scale_x, resource_key, tm, render_mode)`.
type RawFrame = (
    usize,
    usize,
    String,
    f64,
    f64,
    f64,
    f64,
    String,
    Matrix,
    i64,
);

/// A filled rectangle collected during the content-stream walk (candidate
/// underline or strikethrough decoration).
#[derive(Debug, Clone)]
pub struct FilledRect {
    /// Left edge x in PDF user-space (CTM-applied, same space as block coords).
    pub x: f64,
    /// Bottom edge y in PDF user-space.
    pub y: f64,
    /// Width of the rect.
    pub width: f64,
    /// Height (thickness) of the rect.
    pub height: f64,
    /// Fill colour at the time the rect was drawn, `[r, g, b]` (0.0–1.0).
    pub color: [f64; 3],
}

// ── Public types ──────────────────────────────────────────────────────────────

/// Identifies which stream a frame's operator lives in.
#[derive(Debug, Clone)]
pub(crate) enum OpStreamSource {
    /// The page's own `/Contents` stream.
    PageContent,
    /// A Form XObject with this object number.
    FormXObject(u32),
}

/// One parsed content stream (page content or a Form XObject).
pub(crate) struct OpStream {
    pub source: OpStreamSource,
    /// Parsed — and possibly patched — operators for this stream.
    pub ops: Vec<Operation>,
    /// Snapshot of `ops` as first parsed, used to detect whether this stream was
    /// actually edited. Streams that are unchanged are NOT rewritten on commit,
    /// so their original bytes (comments, number formatting, exact whitespace)
    /// survive a round-trip — a partial lossless-fidelity guarantee (TD-4).
    orig_ops: Vec<Operation>,
}

impl OpStream {
    /// Build a stream, snapshotting `ops` as the pristine baseline.
    fn new(source: OpStreamSource, ops: Vec<Operation>) -> Self {
        OpStream {
            source,
            orig_ops: ops.clone(),
            ops,
        }
    }

    /// Whether this stream's operators differ from the parsed baseline.
    fn changed(&self) -> bool {
        self.ops != self.orig_ops
    }
}

/// One editable text span extracted from a page's content streams.
///
/// Produced by [`build_edit_session`] for every `Tj`/`TJ` operator found in
/// the page content stream or any reachable Form XObject.
#[derive(Debug, Clone)]
pub struct EditableFrame {
    /// Sequential index used by JS to identify this frame.
    pub id: usize,
    /// Decoded Unicode text content.
    pub text: String,
    /// CTM-corrected x coordinate (PDF user-space, origin bottom-left).
    pub x: f64,
    /// CTM-corrected y coordinate (PDF user-space, origin bottom-left).
    pub y: f64,
    /// Font size in PDF points.
    pub font_size: f64,
    /// Resolved actual font name (e.g. `"Helvetica-Bold"`).
    pub font_name: String,
    /// Raw PDF resource key (e.g. `"F1"`).
    pub resource_key: String,
    /// Index into `EditSession::streams` — which stream this frame belongs to.
    pub(crate) stream_idx: usize,
    /// Index into `EditSession::streams[stream_idx].ops`.
    pub stream_op_index: usize,
    /// Horizontal scale of the text→page transform (`tm · ctm`) at this show op.
    ///
    /// `x`/`y` are page-space (CTM-applied), but glyph advances are measured in
    /// text space. Multiply a text-space width by this to get the on-page width
    /// (e.g. a page-level `cm [0.75 …]` makes this 0.75). Without it, widths on
    /// scaled pages are too large and the edit box overruns the page.
    pub scale_x: f64,
    /// The text matrix (`Tm`, *pre*-CTM) active at this show op.
    ///
    /// Unlike `x`/`y`/`scale_x` (which fold in the CTM), this is the raw text
    /// matrix so callers can re-emit a `Tm` operator that composes correctly with
    /// the in-content CTM — used to position per-run show ops and to apply a
    /// synthetic-italic shear in text space.
    pub(crate) tm: Matrix,
    /// Text rendering mode (`Tr` operator) active at this show op.
    /// 0 = fill (default), 2 = fill+stroke (used for synthetic bold).
    pub(crate) render_mode: i64,
}

/// Live edit session for a single page.
pub struct EditSession {
    /// Text frames extracted from all content streams.
    pub frames: Vec<EditableFrame>,
    /// All parsed content streams: `streams[0]` is always the page content;
    /// subsequent entries are Form XObjects encountered via `Do`.
    pub(crate) streams: Vec<OpStream>,
    /// `true` once `patch_frame` has actually changed an op. Guards the legacy
    /// write-back in `exit_edit_mode`: an unmodified session (the common case,
    /// since the session is built only to feed overlay metadata) must NOT be
    /// re-serialized — doing so would overwrite the page's `/Contents` with the
    /// pristine ops it was built from, clobbering a surgical `text_edit_commit`.
    pub(crate) dirty: bool,
    /// Filled rectangles collected during the content walk (decoration candidates).
    /// Coordinates are PDF user-space (CTM-applied), same space as frame x/y.
    pub rects: Vec<FilledRect>,
}

// ── Internal graphics state ───────────────────────────────────────────────────

struct GfxState {
    ctm: Matrix,
    ctm_stack: Vec<Matrix>,
    in_text: bool,
    tm: Matrix,
    line_m: Matrix,
    font_size: f64,
    resource_key: String,
    leading: f64,
    /// Text rendering mode (0=fill, 2=fill+stroke for synthetic bold).
    render_mode: i64,
    /// Current fill colour `[r, g, b]` (0.0–1.0), updated by `rg`/`g`.
    fill_color: [f64; 3],
    /// Pending rectangle coordinates (from `re` before a fill operator).
    pending_rect: Option<(f64, f64, f64, f64)>,
}

impl GfxState {
    fn identity() -> Self {
        Self {
            ctm: Matrix::identity(),
            ctm_stack: Vec::new(),
            in_text: false,
            tm: Matrix::identity(),
            line_m: Matrix::identity(),
            font_size: 0.0,
            resource_key: String::new(),
            leading: 0.0,
            render_mode: 0,
            fill_color: [0.0, 0.0, 0.0],
            pending_rect: None,
        }
    }

    fn with_ctm(ctm: Matrix) -> Self {
        Self {
            ctm,
            ctm_stack: Vec::new(),
            in_text: false,
            tm: Matrix::identity(),
            line_m: Matrix::identity(),
            font_size: 0.0,
            resource_key: String::new(),
            leading: 0.0,
            render_mode: 0,
            fill_color: [0.0, 0.0, 0.0],
            pending_rect: None,
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Parse a page's content stream into an `EditSession`.
///
/// Walks the content stream (and any reachable Form XObjects) once, tracking
/// the CTM and text state, creating an `EditableFrame` for every `Tj`/`TJ`
/// operator.  Text bytes are decoded through the font's ToUnicode CMap so real
/// PDFs with glyph-encoded text work correctly (mirrors ONLYOFFICE's xpdf
/// `drawChar` approach).
///
/// Returns an `EditSession` with an empty `frames` list if the page has no
/// extractable text (e.g. a scanned/image-only page).
pub fn build_edit_session(doc: &PdfDocument, page_index: usize) -> Result<EditSession> {
    let catalog = Catalog::from_document(doc)?;
    let page_dict = catalog.get_page_dict(doc, page_index)?;

    // Walk /Parent chain so pages that inherit /Resources from a /Pages node work.
    let resources: Option<PdfDict> =
        match resolve_inherited_attribute(doc, &page_dict, "Resources")? {
            Some(PdfObject::Dictionary(d)) => Some(d),
            _ => None,
        };

    let content_bytes = decode_page_contents(doc, &page_dict)?;
    if content_bytes.is_empty() {
        return Ok(EditSession {
            frames: Vec::new(),
            streams: Vec::new(),
            dirty: false,
            rects: Vec::new(),
        });
    }

    log::debug!(
        "[edit-session] page={} resources={}",
        page_index,
        resources.is_some()
    );

    let ops = parse_content_stream(&content_bytes)?;
    log::debug!("[edit-session] page={} ops={}", page_index, ops.len());

    // Count key operator types to aid debugging.
    let tj_count = ops.iter().filter(|o| o.operator == "Tj").count();
    let tj_arr_count = ops.iter().filter(|o| o.operator == "TJ").count();
    let do_count = ops.iter().filter(|o| o.operator == "Do").count();
    let tf_count = ops.iter().filter(|o| o.operator == "Tf").count();
    let bt_count = ops.iter().filter(|o| o.operator == "BT").count();
    log::debug!(
        "[edit-session] page={} BT={} Tf={} Tj={} TJ={} Do={}",
        page_index,
        bt_count,
        tf_count,
        tj_count,
        tj_arr_count,
        do_count
    );

    // Sample first 30 operators to reveal what this stream actually contains.
    {
        let sample: Vec<&str> = ops.iter().take(30).map(|o| o.operator.as_str()).collect();
        log::debug!("[edit-session] page={} first_ops={:?}", page_index, sample);
    }

    // Also log a frequency map of all distinct operators.
    {
        let mut freq: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for o in &ops {
            *freq.entry(o.operator.as_str()).or_insert(0) += 1;
        }
        let mut sorted: Vec<(&str, usize)> = freq.into_iter().collect();
        sorted.sort_by_key(|b| std::cmp::Reverse(b.1));
        sorted.truncate(15);
        log::debug!("[edit-session] page={} op_freq={:?}", page_index, sorted);
    }

    let mut streams: Vec<OpStream> = vec![OpStream::new(OpStreamSource::PageContent, ops)];

    // raw: (stream_idx, op_idx, text, x, y, font_size, scale_x, resource_key, tm, render_mode)
    let mut raw: Vec<RawFrame> = Vec::new();
    let mut rects: Vec<FilledRect> = Vec::new();
    let mut gfx = GfxState::identity();
    let mut seen: HashSet<u32> = HashSet::new();

    // Clone the page ops so we can mutably borrow `streams` during recursion.
    let page_ops = streams[0].ops.clone();
    extract_frames_recursive(
        &page_ops,
        0,
        doc,
        resources.as_ref(),
        &mut gfx,
        &mut streams,
        &mut raw,
        &mut rects,
        &mut seen,
        0,
    );

    log::debug!(
        "[edit-session] page={} raw_frames={}",
        page_index,
        raw.len()
    );

    let frames: Vec<EditableFrame> = raw
        .into_iter()
        .enumerate()
        .map(
            |(
                id,
                (stream_idx, op_idx, text, x, y, font_size, scale_x, resource_key, tm, render_mode),
            )| {
                let font_name = resolve_font_name(doc, page_index, &resource_key);
                EditableFrame {
                    id,
                    text,
                    x,
                    y,
                    font_size,
                    font_name,
                    resource_key,
                    stream_idx,
                    stream_op_index: op_idx,
                    scale_x,
                    tm,
                    render_mode,
                }
            },
        )
        .collect();

    Ok(EditSession {
        frames,
        streams,
        dirty: false,
        rects,
    })
}

/// Patch the text operand of the frame identified by `frame_id` in-place.
///
/// Replaces the `Tj`/`TJ` operand at the pre-indexed stream position.
/// `TJ` arrays are collapsed to a single `Tj` string on replace.
///
/// Returns `true` if the frame was found and patched; `false` if `frame_id` is
/// out of range or the op index no longer points at a text operator.
pub fn patch_frame(session: &mut EditSession, frame_id: usize, new_text: &str) -> bool {
    let (stream_idx, op_idx) = match session.frames.get(frame_id) {
        Some(f) => (f.stream_idx, f.stream_op_index),
        None => return false,
    };
    let op = match session
        .streams
        .get_mut(stream_idx)
        .and_then(|s| s.ops.get_mut(op_idx))
    {
        Some(o) => o,
        None => return false,
    };
    let encoded = encode_pdf_string(new_text);
    let patched = match op.operator.as_str() {
        "Tj" => {
            if let Some(operand) = op.operands.get_mut(0) {
                *operand = PdfObject::String(encoded);
                true
            } else {
                false
            }
        }
        "TJ" => {
            // Collapse the kerning array to a plain Tj.
            op.operator = "Tj".to_owned();
            op.operands = vec![PdfObject::String(encoded)];
            true
        }
        _ => false,
    };
    // Only a real change marks the session for write-back (see `EditSession::dirty`).
    if patched {
        session.dirty = true;
    }
    patched
}

/// Shrink a page op-stream to just the edited block's run for the live preview
/// render: drop every text show op (`Tj`/`TJ`) whose index is **not** in
/// `block_op_idx`, and drop all image `Do` ops. State ops (`cm`/`q`/`Q`/`rg`/
/// `Tf`/`Tm`/…) are preserved so positioning and the CTM are unchanged.
///
/// This makes the per-keystroke edit render cost O(the edited run) instead of
/// O(whole page incl. images); the preview is identical because the host only
/// shows the block's cropped region over a white cover.
#[allow(dead_code)]
pub(crate) fn edit_render_content_ops(
    ops: Vec<Operation>,
    block_op_idx: &[usize],
) -> Vec<Operation> {
    ops.into_iter()
        .enumerate()
        .filter(|(i, op)| {
            if op.operator == "Do" {
                return false;
            }
            let is_show = op.operator == "Tj" || op.operator == "TJ";
            !is_show || block_op_idx.contains(i)
        })
        .map(|(_, op)| op)
        .collect()
}

/// Serialize page-content ops wrapped in a balanced `q … Q`.
///
/// A page's content stream may leave a non-identity CTM active (e.g. a top-level
/// flip `cm` with no closing `Q`). Any content stream **appended** after it on the
/// same page — the underline/strike decoration layer, the trial watermark —
/// inherits that residual CTM and would be transformed by it, mis-placing the
/// decoration (a flipped page mirrors an underline to the page bottom).
///
/// Wrapping the page content in `q … Q` restores the initial identity CTM before
/// any appended layer runs, so decoration rects expressed in page user-space land
/// where their geometry says.
///
/// This MUST be the single source of committed page-content bytes: both
/// [`commit_edit_session`] (which writes the new `/Contents` stream object) and the
/// decode-cache preload in `WasmEditor::cache_committed_streams` (which shadows
/// that object by id) call it. If the two produced different bytes, the cache would
/// shadow the stream with unwrapped content and committed renders would read the
/// wrong bytes.
pub(crate) fn wrap_page_content_bytes(ops: &[Operation]) -> Vec<u8> {
    let mut wrapped: Vec<Operation> = Vec::with_capacity(ops.len() + 2);
    wrapped.push(Operation {
        operator: "q".to_owned(),
        operands: vec![],
    });
    wrapped.extend(ops.iter().cloned());
    wrapped.push(Operation {
        operator: "Q".to_owned(),
        operands: vec![],
    });
    serialize_operations(&wrapped)
}

/// Flush the session's dirty streams to the writer pool.
///
/// This is the **deferred flush step** in the two-phase text-edit design.
/// `commit_block` / `commit_block_runs` only patch the in-memory `OpStream`
/// operators; call this function once — typically from `text_edit_exit` — after
/// all per-block edits are complete to serialise changed streams and register
/// them in the writer pool so `save_append` / `save_new` include the edits.
///
/// Only streams where `OpStream::changed()` is true are rewritten; unchanged
/// streams are left alone to avoid reformatting numbers or dropping comments.
///
/// For each dirty stream:
/// - `PageContent`: flate-compresses the ops, adds a new stream object, and
///   updates the page's `/Contents` reference to point at it.
/// - `FormXObject(obj_num)`: replaces the XObject stream object in-place.
pub fn commit_edit_session(
    editor: &mut PdfEditor,
    page_index: usize,
    session: &EditSession,
) -> Result<()> {
    for (idx, stream) in session.streams.iter().enumerate() {
        // Only rewrite streams that were actually edited. Untouched streams keep
        // their original object/bytes (TD-4: avoids reformatting numbers, dropping
        // comments, or perturbing the byte layout of streams the user never
        // changed — which would otherwise enlarge the diff and break signatures).
        if !stream.changed() {
            log::warn!(
                "[commit_edit_session] stream[{}] {:?} UNCHANGED — skip",
                idx,
                stream.source
            );
            continue;
        }
        let new_bytes = serialize_operations(&stream.ops);
        match &stream.source {
            OpStreamSource::PageContent => {
                let (page_id, page_dict) = editor.get_page_dict(page_index)?;
                let old_contents = match page_dict.get("Contents") {
                    Some(PdfObject::Reference(id, _)) => format!("Reference({id})"),
                    Some(PdfObject::Array(a)) => format!("Array(len={})", a.len()),
                    other => format!("{other:?}"),
                };
                // Wrap the page content in a balanced q/Q (see `wrap_page_content_bytes`)
                // so any layer appended afterwards (decoration rects, watermark) starts
                // from the initial identity CTM. The SAME wrap MUST be applied wherever
                // committed page bytes are produced — notably the decode-cache preload in
                // `cache_committed_streams`, which shadows this object by id; a mismatch
                // there would make committed renders read unwrapped bytes and mis-place
                // the appended decoration under the page's residual flip CTM.
                let new_bytes = wrap_page_content_bytes(&stream.ops);
                let obj = make_flate_stream(&new_bytes, crate::parser::objects::PdfDict::new())?;
                let stream_id = editor.add_object(PdfObject::Stream(Box::new(obj)));
                let mut updated = page_dict;
                updated.insert("Contents".to_owned(), PdfObject::Reference(stream_id, 0));
                editor.replace_object(page_id, PdfObject::Dictionary(updated));
                log::warn!(
                    "[commit_edit_session] PageContent flushed: page_id={} old Contents={} → Reference({}) ({} bytes)",
                    page_id,
                    old_contents,
                    stream_id,
                    new_bytes.len()
                );
            }
            OpStreamSource::FormXObject(obj_num) => {
                let obj = make_flate_stream(&new_bytes, crate::parser::objects::PdfDict::new())?;
                editor.replace_object(*obj_num, PdfObject::Stream(Box::new(obj)));
                log::warn!(
                    "[commit_edit_session] FormXObject({}) flushed ({} bytes)",
                    obj_num,
                    new_bytes.len()
                );
            }
        }
    }
    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Walk `ops` collecting text frames; descend into Form XObjects via `Do`.
///
/// `stream_idx` is the index of the current stream in `streams`.
/// Raw output tuples: `(stream_idx, op_idx, text, x, y, font_size, scale_x, resource_key, tm, render_mode)`.
#[allow(clippy::too_many_arguments)]
fn extract_frames_recursive(
    ops: &[Operation],
    stream_idx: usize,
    doc: &PdfDocument,
    resources: Option<&PdfDict>,
    gfx: &mut GfxState,
    streams: &mut Vec<OpStream>,
    frames: &mut Vec<RawFrame>,
    rects: &mut Vec<FilledRect>,
    seen: &mut HashSet<u32>,
    depth: usize,
) {
    for (idx, op) in ops.iter().enumerate() {
        match op.operator.as_str() {
            // ── Graphics state ───────────────────────────────────────────────
            "q" => gfx.ctm_stack.push(gfx.ctm),
            "Q" => {
                if let Some(saved) = gfx.ctm_stack.pop() {
                    gfx.ctm = saved;
                }
                gfx.pending_rect = None;
            }
            "cm" if op.operands.len() == 6 => {
                gfx.ctm = gfx.ctm.concat(&matrix_from_ops(&op.operands));
            }

            // ── Text render mode ─────────────────────────────────────────────
            "Tr" if op.operands.len() == 1 => {
                gfx.render_mode = op_i64(&op.operands[0]);
            }

            // ── Fill colour (for decoration matching) ────────────────────────
            "rg" if op.operands.len() == 3 => {
                gfx.fill_color = [
                    op_f64(&op.operands[0]),
                    op_f64(&op.operands[1]),
                    op_f64(&op.operands[2]),
                ];
            }
            "g" if op.operands.len() == 1 => {
                let v = op_f64(&op.operands[0]);
                gfx.fill_color = [v, v, v];
            }

            // ── Rectangle path constructor ───────────────────────────────────
            "re" if op.operands.len() == 4 => {
                // Record (x, y, w, h) in path space; fill ops below flush it.
                gfx.pending_rect = Some((
                    op_f64(&op.operands[0]),
                    op_f64(&op.operands[1]),
                    op_f64(&op.operands[2]),
                    op_f64(&op.operands[3]),
                ));
            }

            // ── Fill operators: flush any pending rect ───────────────────────
            "f" | "F" | "b" | "B" | "b*" | "B*" => {
                if let Some((rx, ry, rw, rh)) = gfx.pending_rect.take() {
                    // Transform rect corners through CTM to get page-space coords.
                    let (x0, y0) = gfx.ctm.transform_point(rx, ry);
                    let (x1, y1) = gfx.ctm.transform_point(rx + rw, ry + rh);
                    let (lx, ly) = (x0.min(x1), y0.min(y1));
                    let (uw, uh) = ((x1 - x0).abs(), (y1 - y0).abs());
                    rects.push(FilledRect {
                        x: lx,
                        y: ly,
                        width: uw,
                        height: uh,
                        color: gfx.fill_color,
                    });
                }
            }

            // ── Text block ───────────────────────────────────────────────────
            "BT" => {
                gfx.in_text = true;
                gfx.tm = Matrix::identity();
                gfx.line_m = Matrix::identity();
            }
            "ET" => {
                gfx.in_text = false;
            }

            // Tf is valid both inside and outside BT/ET (ISO 32000-1 §9.3.1).
            "Tf" if op.operands.len() >= 2 => {
                gfx.resource_key = match &op.operands[0] {
                    PdfObject::Name(n) => n.clone(),
                    _ => String::new(),
                };
                gfx.font_size = op_f64(&op.operands[1]);
            }
            "TL" if op.operands.len() == 1 => {
                gfx.leading = op_f64(&op.operands[0]);
            }

            // ── Text positioning ─────────────────────────────────────────────
            "Tm" if gfx.in_text && op.operands.len() == 6 => {
                gfx.tm = matrix_from_ops(&op.operands);
                gfx.line_m = gfx.tm;
            }
            "Td" | "TD" if gfx.in_text && op.operands.len() == 2 => {
                let dx = op_f64(&op.operands[0]);
                let dy = op_f64(&op.operands[1]);
                let delta = Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: dx,
                    f: dy,
                };
                gfx.line_m = delta.concat(&gfx.line_m);
                gfx.tm = gfx.line_m;
                if op.operator == "TD" {
                    gfx.leading = -dy;
                }
            }
            "T*" if gfx.in_text => {
                let delta = Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: 0.0,
                    f: -gfx.leading,
                };
                gfx.line_m = delta.concat(&gfx.line_m);
                gfx.tm = gfx.line_m;
            }

            // ── Text showing ─────────────────────────────────────────────────
            "Tj" if gfx.in_text && op.operands.len() == 1 => {
                if let PdfObject::String(ref bytes) = op.operands[0] {
                    let text = decode_text_with_font(bytes, &gfx.resource_key, doc, resources);
                    if !text.is_empty() {
                        let (x, y) = text_origin(&gfx.tm, &gfx.ctm);
                        let sx = text_scale_x(&gfx.tm, &gfx.ctm);
                        frames.push((
                            stream_idx,
                            idx,
                            text,
                            x,
                            y,
                            gfx.font_size,
                            sx,
                            gfx.resource_key.clone(),
                            gfx.tm,
                            gfx.render_mode,
                        ));
                    }
                }
            }
            "TJ" if gfx.in_text && op.operands.len() == 1 => {
                if let PdfObject::Array(ref arr) = op.operands[0] {
                    let combined: String = arr
                        .iter()
                        .filter_map(|item| match item {
                            PdfObject::String(ref bytes) => {
                                let t =
                                    decode_text_with_font(bytes, &gfx.resource_key, doc, resources);
                                if t.is_empty() {
                                    None
                                } else {
                                    Some(t)
                                }
                            }
                            _ => None,
                        })
                        .collect();
                    if !combined.is_empty() {
                        let (x, y) = text_origin(&gfx.tm, &gfx.ctm);
                        let sx = text_scale_x(&gfx.tm, &gfx.ctm);
                        frames.push((
                            stream_idx,
                            idx,
                            combined,
                            x,
                            y,
                            gfx.font_size,
                            sx,
                            gfx.resource_key.clone(),
                            gfx.tm,
                            gfx.render_mode,
                        ));
                    }
                }
            }

            // ── Form XObject invocation ──────────────────────────────────────
            "Do" if depth < MAX_XOBJECT_DEPTH && op.operands.len() == 1 => {
                if let PdfObject::Name(ref name) = op.operands[0] {
                    handle_do_xobject(
                        name, doc, resources, gfx, streams, frames, rects, seen, depth,
                    );
                }
            }

            _ => {}
        }
    }
}

/// Resolve a Form XObject named `name` and recursively extract frames from it.
#[allow(clippy::too_many_arguments)]
fn handle_do_xobject(
    name: &str,
    doc: &PdfDocument,
    resources: Option<&PdfDict>,
    gfx: &mut GfxState,
    streams: &mut Vec<OpStream>,
    frames: &mut Vec<RawFrame>,
    rects: &mut Vec<FilledRect>,
    seen: &mut HashSet<u32>,
    depth: usize,
) {
    // Resolve /XObject/<name> in current resources.
    let xobj_ref = resources
        .and_then(|r| r.get("XObject"))
        .and_then(|xo| doc.resolve(xo).ok())
        .and_then(|xo| match xo {
            PdfObject::Dictionary(d) => d.get(name).cloned(),
            _ => None,
        });

    log::debug!(
        "[edit-session] Do /{} xobj_ref={:?}",
        name,
        xobj_ref.as_ref().map(std::mem::discriminant)
    );

    let (obj_num, resolved) = match xobj_ref {
        Some(PdfObject::Reference(num, gen)) => {
            match doc.resolve(&PdfObject::Reference(num, gen)) {
                Ok(o) => (num, o),
                Err(e) => {
                    log::debug!("[edit-session] Do /{} resolve err: {}", name, e);
                    return;
                }
            }
        }
        _ => {
            log::debug!("[edit-session] Do /{} not a Reference — skipped", name);
            return;
        }
    };

    // Cycle detection.
    if !seen.insert(obj_num) {
        log::debug!(
            "[edit-session] Do /{} obj={} cycle — skipped",
            name,
            obj_num
        );
        return;
    }

    let stream = match resolved {
        PdfObject::Stream(s) => s,
        _ => {
            log::debug!(
                "[edit-session] Do /{} obj={} not a Stream — skipped",
                name,
                obj_num
            );
            seen.remove(&obj_num);
            return;
        }
    };

    // Only handle Form XObjects.
    let is_form = matches!(stream.dict.get("Subtype"), Some(PdfObject::Name(n)) if n == "Form");
    if !is_form {
        log::debug!(
            "[edit-session] Do /{} obj={} Subtype={:?} — not Form, skipped",
            name,
            obj_num,
            stream.dict.get("Subtype")
        );
        seen.remove(&obj_num);
        return;
    }

    let xobj_bytes = match stream.decode() {
        Ok(b) => b,
        Err(e) => {
            log::debug!(
                "[edit-session] Do /{} obj={} decode err: {}",
                name,
                obj_num,
                e
            );
            seen.remove(&obj_num);
            return;
        }
    };

    let xobj_ops = match parse_content_stream(&xobj_bytes) {
        Ok(ops) => ops,
        Err(e) => {
            log::debug!(
                "[edit-session] Do /{} obj={} parse err: {}",
                name,
                obj_num,
                e
            );
            seen.remove(&obj_num);
            return;
        }
    };

    // Form XObjects may define their own /Resources.
    let xobj_resources: Option<PdfDict> = stream
        .dict
        .get("Resources")
        .and_then(|r| doc.resolve(r).ok())
        .and_then(|obj| match obj {
            PdfObject::Dictionary(d) => Some(d),
            _ => None,
        });
    let effective_resources = xobj_resources.as_ref().or(resources);

    // The XObject's optional /Matrix pre-multiplies the current CTM.
    let xobj_matrix: Matrix = stream
        .dict
        .get("Matrix")
        .and_then(|m| doc.resolve(m).ok())
        .and_then(|obj| match obj {
            PdfObject::Array(arr) if arr.len() == 6 => Some(matrix_from_ops(&arr)),
            _ => None,
        })
        .unwrap_or_else(Matrix::identity);

    let new_stream_idx = streams.len();
    let xobj_op_count = xobj_ops.len();
    streams.push(OpStream::new(
        OpStreamSource::FormXObject(obj_num),
        xobj_ops,
    ));

    log::debug!(
        "[edit-session] Do /{} obj={} stream_idx={} ops={} resources={}",
        name,
        obj_num,
        new_stream_idx,
        xobj_op_count,
        effective_resources.is_some()
    );

    let frames_before = frames.len();
    // Recurse with fresh text state but inherited (concatenated) CTM.
    let inherited_ctm = gfx.ctm.concat(&xobj_matrix);
    let mut xobj_gfx = GfxState::with_ctm(inherited_ctm);
    let ops_clone = streams[new_stream_idx].ops.clone();
    extract_frames_recursive(
        &ops_clone,
        new_stream_idx,
        doc,
        effective_resources,
        &mut xobj_gfx,
        streams,
        frames,
        rects,
        seen,
        depth + 1,
    );

    log::debug!(
        "[edit-session] Do /{} obj={} frames_found={}",
        name,
        obj_num,
        frames.len() - frames_before
    );
    seen.remove(&obj_num);
}

/// Compute the CTM-corrected text origin from the text matrix and current CTM.
fn text_origin(tm: &Matrix, ctm: &Matrix) -> (f64, f64) {
    ctm.transform_point(tm.e, tm.f)
}

/// Horizontal scale of the combined text→page transform `tm · ctm`.
///
/// A text-space advance multiplied by this yields the width on the page. Derived
/// from the length of the transformed x-basis vector (`a`,`b`) so it is correct
/// under rotation as well as pure scaling.
fn text_scale_x(tm: &Matrix, ctm: &Matrix) -> f64 {
    let m = tm.concat(ctm);
    (m.a * m.a + m.b * m.b).sqrt()
}

fn matrix_from_ops(ops: &[PdfObject]) -> Matrix {
    Matrix {
        a: op_f64(&ops[0]),
        b: op_f64(&ops[1]),
        c: op_f64(&ops[2]),
        d: op_f64(&ops[3]),
        e: op_f64(&ops[4]),
        f: op_f64(&ops[5]),
    }
}

fn op_f64(obj: &PdfObject) -> f64 {
    match obj {
        PdfObject::Real(f) => *f,
        PdfObject::Integer(i) => *i as f64,
        _ => 0.0,
    }
}

fn op_i64(obj: &PdfObject) -> i64 {
    match obj {
        PdfObject::Integer(i) => *i,
        PdfObject::Real(f) => *f as i64,
        _ => 0,
    }
}

/// Decode text bytes to Unicode using the font's ToUnicode CMap when available.
///
/// Mirrors `ContentInterpreter::show_text`: resolves the font's CMap from the
/// page resources and maps glyph codes to Unicode.  Falls back to UTF-16BE BOM
/// detection then Latin-1 when no CMap is available.
fn decode_text_with_font(
    bytes: &[u8],
    resource_key: &str,
    doc: &PdfDocument,
    resources: Option<&PdfDict>,
) -> String {
    use crate::content::interpreter::{decode_bytes_with_cmap, resolve_font_info};
    let (cmap, is_composite, _) = resolve_font_info(resource_key, Some(doc), resources);
    if let Some(ref cm) = cmap {
        decode_bytes_with_cmap(bytes, cm, is_composite)
    } else if is_composite {
        bytes
            .chunks(2)
            .filter_map(|pair| {
                let cp = if pair.len() == 2 {
                    ((pair[0] as u32) << 8) | (pair[1] as u32)
                } else {
                    pair[0] as u32
                };
                char::from_u32(cp)
            })
            .collect()
    } else if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let words: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&words).unwrap_or_else(|_| bytes.iter().map(|&b| b as char).collect())
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Decode and concatenate all content streams for a page.
///
/// Uses `doc.get_stream_data` for reference streams so encrypted PDFs are
/// decrypted before decompression (mirrors `page::Page::decode_contents`).
fn decode_page_contents(doc: &PdfDocument, page_dict: &PdfDict) -> Result<Vec<u8>> {
    use crate::error::PdfError;

    match page_dict.get("Contents") {
        None => Ok(Vec::new()),

        Some(PdfObject::Reference(id, _)) => doc.get_stream_data(*id).map_err(|e| {
            PdfError::invalid_structure(format!("content stream decode failed: {}", e))
        }),

        Some(PdfObject::Array(refs)) => {
            let mut buf = Vec::new();
            for r in refs {
                let decoded = match r {
                    PdfObject::Reference(id, _) => doc.get_stream_data(*id).map_err(|e| {
                        PdfError::invalid_structure(format!("content stream decode failed: {}", e))
                    })?,
                    // Rare: inline stream in array — no object ID, not document-encrypted.
                    _ => match doc.resolve(r)? {
                        PdfObject::Stream(s) => s.decode_with_doc(doc).map_err(|e| {
                            PdfError::invalid_structure(format!(
                                "content stream decode failed: {}",
                                e
                            ))
                        })?,
                        _ => continue,
                    },
                };
                if !decoded.is_empty() {
                    if !buf.is_empty() {
                        buf.push(b'\n');
                    }
                    buf.extend_from_slice(&decoded);
                }
            }
            Ok(buf)
        }

        // Already-resolved inline stream (very rare, no object ID).
        Some(other) => match doc.resolve(other)? {
            PdfObject::Stream(s) => s.decode_with_doc(doc).map_err(|e| {
                PdfError::invalid_structure(format!("content stream decode failed: {}", e))
            }),
            _ => Ok(Vec::new()),
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ops(bytes: &[u8]) -> Vec<Operation> {
        parse_content_stream(bytes).expect("parse failed")
    }

    /// Extract frames without a `PdfDocument` (Latin-1 fallback, no XObject recursion).
    /// Used by unit tests that craft raw content streams in memory.
    fn extract_raw_frames_no_doc(ops: &[Operation]) -> Vec<RawFrame> {
        let mut out: Vec<RawFrame> = Vec::new();
        let mut ctm = Matrix::identity();
        let mut ctm_stack: Vec<Matrix> = Vec::new();
        let mut in_text = false;
        let mut tm = Matrix::identity();
        let mut line_m = Matrix::identity();
        let mut font_size = 0.0_f64;
        let mut resource_key = String::new();
        let mut leading = 0.0_f64;
        let mut render_mode = 0_i64;

        for (idx, op) in ops.iter().enumerate() {
            match op.operator.as_str() {
                "q" => ctm_stack.push(ctm),
                "Q" => {
                    if let Some(s) = ctm_stack.pop() {
                        ctm = s;
                    }
                }
                "cm" if op.operands.len() == 6 => {
                    ctm = ctm.concat(&matrix_from_ops(&op.operands));
                }
                "Tr" if op.operands.len() == 1 => {
                    render_mode = op_i64(&op.operands[0]);
                }
                "BT" => {
                    in_text = true;
                    tm = Matrix::identity();
                    line_m = Matrix::identity();
                }
                "ET" => {
                    in_text = false;
                }
                "Tf" if op.operands.len() >= 2 => {
                    resource_key = match &op.operands[0] {
                        PdfObject::Name(n) => n.clone(),
                        _ => String::new(),
                    };
                    font_size = op_f64(&op.operands[1]);
                }
                "TL" if op.operands.len() == 1 => {
                    leading = op_f64(&op.operands[0]);
                }
                "Tm" if in_text && op.operands.len() == 6 => {
                    tm = matrix_from_ops(&op.operands);
                    line_m = tm;
                }
                "Td" | "TD" if in_text && op.operands.len() == 2 => {
                    let dx = op_f64(&op.operands[0]);
                    let dy = op_f64(&op.operands[1]);
                    let delta = Matrix {
                        a: 1.0,
                        b: 0.0,
                        c: 0.0,
                        d: 1.0,
                        e: dx,
                        f: dy,
                    };
                    line_m = delta.concat(&line_m);
                    tm = line_m;
                    if op.operator == "TD" {
                        leading = -dy;
                    }
                }
                "T*" if in_text => {
                    let delta = Matrix {
                        a: 1.0,
                        b: 0.0,
                        c: 0.0,
                        d: 1.0,
                        e: 0.0,
                        f: -leading,
                    };
                    line_m = delta.concat(&line_m);
                    tm = line_m;
                }
                "Tj" if in_text && op.operands.len() == 1 => {
                    if let PdfObject::String(ref bytes) = op.operands[0] {
                        let text: String = bytes.iter().map(|&b| b as char).collect();
                        if !text.is_empty() {
                            let (x, y) = text_origin(&tm, &ctm);
                            out.push((
                                0,
                                idx,
                                text,
                                x,
                                y,
                                font_size,
                                1.0,
                                resource_key.clone(),
                                tm,
                                render_mode,
                            ));
                        }
                    }
                }
                "TJ" if in_text && op.operands.len() == 1 => {
                    if let PdfObject::Array(ref arr) = op.operands[0] {
                        let combined: String = arr
                            .iter()
                            .filter_map(|item| match item {
                                PdfObject::String(ref bytes) => {
                                    let t: String = bytes.iter().map(|&b| b as char).collect();
                                    if t.is_empty() {
                                        None
                                    } else {
                                        Some(t)
                                    }
                                }
                                _ => None,
                            })
                            .collect();
                        if !combined.is_empty() {
                            let (x, y) = text_origin(&tm, &ctm);
                            out.push((
                                0,
                                idx,
                                combined,
                                x,
                                y,
                                font_size,
                                1.0,
                                resource_key.clone(),
                                tm,
                                render_mode,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    // ── extract_raw_frames_no_doc ────────────────────────────────────────────

    #[test]
    fn extracts_tj_at_tm_position() {
        let stream = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello) Tj ET";
        let ops = make_ops(stream);
        let frames = extract_raw_frames_no_doc(&ops);
        assert_eq!(frames.len(), 1);
        let (_, _, text, x, y, fs, _sx, key, _, _) = &frames[0];
        assert_eq!(text, "Hello");
        assert!((x - 72.0).abs() < 1e-6, "x={}", x);
        assert!((y - 700.0).abs() < 1e-6, "y={}", y);
        assert!((fs - 12.0).abs() < 1e-6);
        assert_eq!(key, "F1");
    }

    #[test]
    fn extracts_tj_through_ctm_translation() {
        let stream = b"1 0 0 1 100 200 cm BT /F1 10 Tf 1 0 0 1 10 20 Tm (Hi) Tj ET";
        let ops = make_ops(stream);
        let frames = extract_raw_frames_no_doc(&ops);
        assert_eq!(frames.len(), 1);
        let (_, _, _, x, y, _, _, _, _, _) = &frames[0];
        assert!((x - 110.0).abs() < 1e-6, "x={}", x);
        assert!((y - 220.0).abs() < 1e-6, "y={}", y);
    }

    #[test]
    fn extracts_ctm_restored_after_q_q() {
        let stream = b"q 1 0 0 1 99 99 cm Q BT /F1 10 Tf 1 0 0 1 5 5 Tm (X) Tj ET";
        let ops = make_ops(stream);
        let frames = extract_raw_frames_no_doc(&ops);
        assert_eq!(frames.len(), 1);
        let (_, _, _, x, y, _, _, _, _, _) = &frames[0];
        assert!((x - 5.0).abs() < 1e-6, "x={}", x);
        assert!((y - 5.0).abs() < 1e-6, "y={}", y);
    }

    #[test]
    fn extracts_tj_array() {
        let stream = b"BT /F1 12 Tf 1 0 0 1 50 600 Tm [(Hel) 0 (lo)] TJ ET";
        let ops = make_ops(stream);
        let frames = extract_raw_frames_no_doc(&ops);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].2, "Hello");
    }

    #[test]
    fn td_advances_position() {
        let stream = b"BT /F1 12 Tf 1 0 0 1 10 700 Tm 0 -20 Td (Next) Tj ET";
        let ops = make_ops(stream);
        let frames = extract_raw_frames_no_doc(&ops);
        assert_eq!(frames.len(), 1);
        let (_, _, _, x, y, _, _, _, _, _) = &frames[0];
        assert!((x - 10.0).abs() < 1e-6, "x={}", x);
        assert!((y - 680.0).abs() < 1e-6, "y={}", y);
    }

    #[test]
    fn t_star_advances_by_leading() {
        let stream = b"BT /F1 12 Tf 14 TL 1 0 0 1 10 700 Tm (Line1) Tj T* (Line2) Tj ET";
        let ops = make_ops(stream);
        let frames = extract_raw_frames_no_doc(&ops);
        assert_eq!(frames.len(), 2);
        let (_, _, _, _, y0, _, _, _, _, _) = &frames[0];
        let (_, _, _, _, y1, _, _, _, _, _) = &frames[1];
        assert!((y0 - 700.0).abs() < 1e-6, "y0={}", y0);
        assert!((y1 - 686.0).abs() < 1e-6, "y1={}", y1);
    }

    #[test]
    fn skips_empty_text() {
        let stream = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm () Tj ET";
        let ops = make_ops(stream);
        let frames = extract_raw_frames_no_doc(&ops);
        assert_eq!(frames.len(), 0);
    }

    // ── patch_frame ──────────────────────────────────────────────────────────

    fn make_session_with_tj(x: f64, y: f64, text: &str) -> EditSession {
        let stream = format!("BT /F1 12 Tf 1 0 0 1 {} {} Tm ({}) Tj ET", x, y, text);
        let ops = make_ops(stream.as_bytes());
        let raw = extract_raw_frames_no_doc(&ops);
        let frames = raw
            .into_iter()
            .enumerate()
            .map(
                |(id, (stream_idx, op_idx, t, px, py, fs, sx, rk, tm, rm))| EditableFrame {
                    id,
                    text: t,
                    x: px,
                    y: py,
                    font_size: fs,
                    font_name: "Helvetica".into(),
                    resource_key: rk,
                    stream_idx,
                    stream_op_index: op_idx,
                    scale_x: sx,
                    tm,
                    render_mode: rm,
                },
            )
            .collect();
        EditSession {
            frames,
            streams: vec![OpStream::new(OpStreamSource::PageContent, ops)],
            dirty: false,
            rects: Vec::new(),
        }
    }

    #[test]
    fn patch_frame_replaces_tj() {
        let mut session = make_session_with_tj(72.0, 700.0, "Hello");
        assert!(patch_frame(&mut session, 0, "World"));
        let frame = &session.frames[0];
        let op = &session.streams[frame.stream_idx].ops[frame.stream_op_index];
        if let PdfObject::String(ref s) = op.operands[0] {
            assert_eq!(s, b"World");
        } else {
            panic!("expected String operand after patch");
        }
    }

    #[test]
    fn patch_frame_invalid_id_returns_false() {
        let mut session = make_session_with_tj(10.0, 10.0, "Test");
        assert!(!patch_frame(&mut session, 99, "New"));
    }

    #[test]
    fn dirty_only_set_by_a_real_patch() {
        let mut session = make_session_with_tj(10.0, 10.0, "Test");
        // Fresh session must be clean so exit_edit_mode skips its write-back and
        // never clobbers a surgical text_edit_commit on the same page.
        assert!(!session.dirty, "fresh session must not be dirty");
        // A failed patch (bad id) must not mark it dirty.
        assert!(!patch_frame(&mut session, 99, "New"));
        assert!(!session.dirty, "failed patch must not set dirty");
        // A real patch marks it dirty.
        assert!(patch_frame(&mut session, 0, "New"));
        assert!(session.dirty, "successful patch must set dirty");
    }

    #[test]
    fn edit_render_keeps_block_run_drops_others_and_images() {
        let mk = |operator: &str, operands: Vec<PdfObject>| Operation {
            operator: operator.to_owned(),
            operands,
        };
        // cm | q rg BT Tf Tm <block Tj> ET Q | Do | BT <other Tj> ET
        let ops = vec![
            mk("cm", vec![]),                                     // 0 state
            mk("q", vec![]),                                      // 1 state
            mk("rg", vec![]),                                     // 2 state
            mk("BT", vec![]),                                     // 3 state
            mk("Tf", vec![]),                                     // 4 state
            mk("Tm", vec![]),                                     // 5 state
            mk("Tj", vec![PdfObject::String(b"block".to_vec())]), // 6 block show
            mk("ET", vec![]),                                     // 7 state
            mk("Q", vec![]),                                      // 8 state
            mk("Do", vec![PdfObject::Name("Im1".to_owned())]),    // 9 image
            mk("BT", vec![]),                                     // 10 state
            mk("Tj", vec![PdfObject::String(b"other".to_vec())]), // 11 other show
            mk("ET", vec![]),                                     // 12 state
        ];

        let kept = edit_render_content_ops(ops, &[6]);
        let seen: Vec<&str> = kept.iter().map(|o| o.operator.as_str()).collect();
        // Block Tj kept; image Do dropped; other Tj dropped; all state ops kept.
        assert_eq!(
            seen,
            vec!["cm", "q", "rg", "BT", "Tf", "Tm", "Tj", "ET", "Q", "BT", "ET"]
        );
        // The single surviving show op carries the block's text.
        let show: Vec<_> = kept
            .iter()
            .filter(|o| o.operator == "Tj" || o.operator == "TJ")
            .collect();
        assert_eq!(show.len(), 1);
        assert!(matches!(&show[0].operands[0], PdfObject::String(s) if s == b"block"));
    }

    #[test]
    fn patch_frame_tj_array_collapses_to_tj() {
        let stream = b"BT /F1 12 Tf 1 0 0 1 50 600 Tm [(Hel) 0 (lo)] TJ ET";
        let ops = make_ops(stream);
        let raw = extract_raw_frames_no_doc(&ops);
        let frames = raw
            .into_iter()
            .enumerate()
            .map(
                |(id, (stream_idx, op_idx, t, x, y, fs, sx, rk, tm, rm))| EditableFrame {
                    id,
                    text: t,
                    x,
                    y,
                    font_size: fs,
                    font_name: "Helvetica".into(),
                    resource_key: rk,
                    stream_idx,
                    stream_op_index: op_idx,
                    scale_x: sx,
                    tm,
                    render_mode: rm,
                },
            )
            .collect();
        let mut session = EditSession {
            frames,
            streams: vec![OpStream::new(OpStreamSource::PageContent, ops)],
            dirty: false,
            rects: Vec::new(),
        };
        assert!(patch_frame(&mut session, 0, "World"));
        let frame = &session.frames[0];
        let op = &session.streams[frame.stream_idx].ops[frame.stream_op_index];
        assert_eq!(op.operator, "Tj");
        if let PdfObject::String(ref s) = op.operands[0] {
            assert_eq!(s, b"World");
        } else {
            panic!("expected String operand after TJ→Tj collapse");
        }
    }
}
