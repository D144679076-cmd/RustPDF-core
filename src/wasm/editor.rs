//! WasmEditor — edit an existing PDF incrementally + WasmPdfWriter.

use wasm_bindgen::prelude::*;

use super::bbox_from_quad_points;

// ---------------------------------------------------------------------------
// WasmEditor — edit an existing PDF incrementally
// ---------------------------------------------------------------------------

/// An open PDF editor using the copy-on-write incremental update model.
///
/// Open with [`WasmEditor::open`], apply changes, then call [`WasmEditor::save`]
/// to get the updated PDF bytes.  Each call to `save` updates the stored bytes
/// so multiple rounds of editing and saving are supported.
#[wasm_bindgen]
pub struct WasmEditor {
    pub(crate) editor: crate::editor::PdfEditor,
    /// Original PDF bytes needed by `save_append` to produce the update section.
    pub(crate) original_bytes: Vec<u8>,
    /// Redact zones queued by `add_redact`; consumed by `apply_redactions`.
    pending_redact_zones: Vec<crate::editor::RedactZone>,
    /// Active edit sessions keyed by page index.  Populated by `enter_edit_mode`,
    /// consumed (and written back) by `exit_edit_mode`.
    edit_sessions: std::collections::HashMap<usize, crate::editor::EditSession>,
    /// Word-style text-edit blocks for the page entered via `text_edit_enter`.
    pub(crate) text_edit_blocks: Vec<crate::editor::EditBlock>,
    /// Page index the current `text_edit_blocks` belong to.
    pub(crate) text_edit_page: usize,
    /// The block currently open for caret/selection editing (Phase 1).
    pub(crate) active_text_edit: Option<super::text_edit::ActiveTextEdit>,
    /// Full text model (blocks + parsed content streams) retained so
    /// `text_edit_render_block` can re-render an edited block through the real
    /// renderer. Only read under the `render` feature.
    #[cfg_attr(not(feature = "render"), allow(dead_code))]
    pub(crate) text_edit_model: Option<crate::editor::TextModel>,
    /// Document reparsed from pending edits (writer pool applied), tagged with the
    /// writer **generation** it reflects. `text_edit_enter` rebuilds the editable
    /// model from this when edits are pending, so re-entering edit mode shows
    /// committed text — not the pristine `editor.doc`, which would resurrect the
    /// original. The generation key is correct across in-place `set_object`
    /// replacements and undo/redo, unlike the old pool-length key.
    pub(crate) edit_model_doc: Option<(u64, crate::parser::objects::PdfDocument)>,
    /// Writer generation at which the current `text_edit_model` was built. Lets
    /// `text_edit_enter` reuse the model when re-entering the same page with no
    /// new edits (e.g. opening several blocks on one page), instead of rebuilding
    /// the edit session + per-font metrics every time.
    pub(crate) text_edit_model_generation: u64,
    /// Uncompressed serialised content-stream bytes for each object ID written to
    /// the writer pool by a text commit.  Keyed by the new stream object ID.
    ///
    /// Used to skip the flate-decompress step inside `render_page` and
    /// `render_committed_block_tile`: after `set_overrides` clears the
    /// `decoded_stream_cache` entry, we re-insert the raw bytes here so the
    /// renderer never has to re-inflate the committed content stream.
    pub(crate) committed_bytes: std::collections::HashMap<u32, Vec<u8>>,
    /// Optional QuickJS engine for PDF JavaScript action execution.
    ///
    /// Populated lazily by [`WasmEditor::enable_javascript`].
    #[cfg(feature = "js-actions")]
    js_engine: Option<crate::js::JsEngine>,
    /// Whether the trial watermark has already been applied to the writer pool.
    /// The watermark is appended once; subsequent `save()` calls must NOT re-apply
    /// it (doing so multiplied watermark streams across every page on every save,
    /// bloating the file and bumping the generation on each commit).
    pub(crate) watermarked: bool,
    /// Underline/strikethrough rects queued by `commit_block_runs_impl` and drawn
    /// in `flush_and_cache` AFTER `commit_edit_session` rewrites `/Contents` to a
    /// single reference. Tuple: `(committed_block_id, rects_for_that_block)`.
    /// The block id is used to update only the edited block's entry in the page-level
    /// decoration rebuild while preserving all other blocks' decorations.
    pub(crate) pending_decorations: Option<(usize, Vec<crate::editor::DecoRect>)>,
    /// Committed per-char style runs, keyed by block id. Captured by
    /// `commit_block_runs_impl` so `text_edit_open` can restore decoration state
    /// (underline/strike) when a block is reopened **within the same session**
    /// (before any FULL-REBUILD merges the decoration stream into the model).
    /// Cleared on FULL-REBUILD (block ids renumber + deco is then in the model)
    /// and on `text_edit_exit`.
    pub(crate) committed_style_runs: std::collections::HashMap<usize, Vec<crate::editor::StyleRun>>,
}

#[wasm_bindgen]
impl WasmEditor {
    /// Open an existing PDF for editing.
    pub fn open(bytes: &[u8]) -> Result<WasmEditor, JsError> {
        log::info!("[pdf-core] WasmEditor::open — {} bytes", bytes.len());
        let original_bytes = bytes.to_vec();
        let editor = crate::editor::PdfEditor::open(bytes.to_vec()).map_err(|e| {
            log::error!("[pdf-core] WasmEditor::open failed: {}", e);
            JsError::new(&e.to_string())
        })?;
        log::info!("[pdf-core] WasmEditor::open — ok");
        Ok(WasmEditor {
            editor,
            original_bytes,
            pending_redact_zones: Vec::new(),
            edit_sessions: std::collections::HashMap::new(),
            text_edit_blocks: Vec::new(),
            text_edit_page: 0,
            active_text_edit: None,
            text_edit_model: None,
            edit_model_doc: None,
            text_edit_model_generation: u64::MAX,
            committed_bytes: std::collections::HashMap::new(),
            watermarked: false,
            pending_decorations: None,
            committed_style_runs: std::collections::HashMap::new(),
            #[cfg(feature = "js-actions")]
            js_engine: None,
        })
    }

    /// Open a password-protected PDF for editing.
    ///
    /// `password` is the UTF-8 user or owner password string.
    /// Returns a JS error wrapping `PdfError::Encrypted` if the password is wrong.
    #[cfg(feature = "crypto")]
    pub fn open_with_password(bytes: &[u8], password: &str) -> Result<WasmEditor, JsError> {
        log::info!(
            "[pdf-core] WasmEditor::open_with_password — {} bytes",
            bytes.len()
        );
        let original_bytes = bytes.to_vec();
        let editor =
            crate::editor::PdfEditor::open_with_password(bytes.to_vec(), password.as_bytes())
                .map_err(|e| {
                    log::error!("[pdf-core] WasmEditor::open_with_password failed: {}", e);
                    JsError::new(&e.to_string())
                })?;
        log::info!("[pdf-core] WasmEditor::open_with_password — ok");
        Ok(WasmEditor {
            editor,
            original_bytes,
            pending_redact_zones: Vec::new(),
            edit_sessions: std::collections::HashMap::new(),
            text_edit_blocks: Vec::new(),
            text_edit_page: 0,
            active_text_edit: None,
            text_edit_model: None,
            edit_model_doc: None,
            text_edit_model_generation: u64::MAX,
            committed_bytes: std::collections::HashMap::new(),
            watermarked: false,
            pending_decorations: None,
            committed_style_runs: std::collections::HashMap::new(),
            #[cfg(feature = "js-actions")]
            js_engine: None,
        })
    }

    /// Returns the current page count.
    pub fn page_count(&self) -> Result<usize, JsError> {
        self.editor
            .doc
            .page_count()
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Clone the editor's already-parsed document into a fresh [`super::document::WasmDocument`].
    ///
    /// Avoids a second `WasmDocument::parse` round-trip after `WasmEditor::open`:
    /// cloning skips XRef parsing (the expensive phase) and only copies the raw
    /// bytes + index structures, making it significantly faster than re-parsing.
    ///
    /// The returned document is independent — it does not share state with the
    /// editor and is safe to use for read-only rendering while the editor continues
    /// to accumulate edits.
    pub fn borrow_doc(&self) -> super::document::WasmDocument {
        super::document::WasmDocument {
            doc: self.editor.doc.clone(),
        }
    }

    /// Produce a [`super::document::WasmDocument`] that reflects every committed
    /// edit without serialising or re-parsing the PDF.
    ///
    /// Clones the editor's base document, then installs the writer pool's CoW
    /// objects as permanent overrides so `get_object` returns the committed
    /// versions. [`PdfDocument::set_overrides`] also clears stale
    /// `decoded_stream_cache` entries for those IDs, guaranteeing that
    /// `decode_contents` reads the committed content stream on the next call.
    ///
    /// Cheaper than `save()` + `WasmDocument::parse()`: the clone copies raw
    /// bytes and the XRef index but skips all XRef-chain traversal and object
    /// parsing work.
    pub fn make_committed_doc(&self) -> super::document::WasmDocument {
        let doc = self.editor.doc.clone();
        if !self.editor.writer.is_empty() {
            let overrides: std::collections::HashMap<u32, crate::parser::objects::PdfObject> = self
                .editor
                .writer
                .all_ids()
                .into_iter()
                .filter_map(|id| self.editor.writer.get_object(id).map(|o| (id, o.clone())))
                .collect();
            doc.set_overrides(overrides);
        }
        super::document::WasmDocument { doc }
    }

    /// Extract pages `start..end` (0-based, exclusive end) into a new PDF document.
    ///
    /// Returns the bytes of the new PDF. Requires a Pro license.
    pub fn extract_pages(&self, start: usize, end: usize) -> Result<Vec<u8>, JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_assemble, "assemble")?;
        crate::editor::extract_pages(self.original_bytes.clone(), start..end)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── Page operations ──────────────────────────────────────────────────────

    /// Insert a blank page at `index` (0-based).
    ///
    /// Pass `index == page_count()` to append at the end.
    pub fn add_blank_page(
        &mut self,
        index: usize,
        width_pt: f64,
        height_pt: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_assemble, "assemble")?;
        log::debug!(
            "[pdf-core] add_blank_page index={} {}×{}",
            index,
            width_pt,
            height_pt
        );
        crate::editor::add_blank_page(&mut self.editor, index, width_pt, height_pt)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Delete the page at `index` (0-based).
    pub fn delete_page(&mut self, index: usize) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_assemble, "assemble")?;
        log::debug!("[pdf-core] delete_page index={}", index);
        crate::editor::delete_page(&mut self.editor, index)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Move the page at `from_index` to `to_index` (both 0-based).
    pub fn move_page(&mut self, from_index: usize, to_index: usize) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_assemble, "assemble")?;
        log::debug!("[pdf-core] move_page from={} to={}", from_index, to_index);
        crate::editor::move_page(&mut self.editor, from_index, to_index)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Set the rotation of page `index` to `degrees` clockwise (must be multiple of 90).
    pub fn rotate_page(&mut self, index: usize, degrees: i32) -> Result<(), JsError> {
        log::debug!("[pdf-core] rotate_page index={} degrees={}", index, degrees);
        crate::editor::rotate_page(&mut self.editor, index, degrees)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Set or replace the crop box on page `index`.
    ///
    /// `x1, y1, x2, y2` define the visible region in PDF user-space points.
    pub fn set_crop_box(
        &mut self,
        index: usize,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    ) -> Result<(), JsError> {
        crate::editor::set_crop_box(&mut self.editor, index, [x1, y1, x2, y2])
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── Content drawing ──────────────────────────────────────────────────────

    /// Draw a single line of text on a page.
    ///
    /// `font_name` must be one of the 14 standard PDF fonts.
    /// `r`, `g`, `b` are fill color in 0.0–1.0.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        text: &str,
        font_name: &str,
        font_size: f64,
        r: f64,
        g: f64,
        b: f64,
    ) -> Result<(), JsError> {
        let style = crate::editor::TextStyle::new(font_name, font_size, [r, g, b]);
        crate::editor::draw_text(&mut self.editor, page_index, x, y, text, &style)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Draw a filled rectangle on a page.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_filled_rect(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        r: f64,
        g: f64,
        b: f64,
    ) -> Result<(), JsError> {
        let style = crate::editor::RectStyle::filled([r, g, b]);
        crate::editor::draw_rect(&mut self.editor, page_index, x, y, width, height, &style)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Draw a stroked rectangle on a page.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_stroked_rect(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        r: f64,
        g: f64,
        b: f64,
        line_width: f64,
    ) -> Result<(), JsError> {
        let style = crate::editor::RectStyle::stroked([r, g, b], line_width);
        crate::editor::draw_rect(&mut self.editor, page_index, x, y, width, height, &style)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Draw a straight line on a page.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line(
        &mut self,
        page_index: usize,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        r: f64,
        g: f64,
        b: f64,
        line_width: f64,
    ) -> Result<(), JsError> {
        crate::editor::draw_line(
            &mut self.editor,
            page_index,
            x1,
            y1,
            x2,
            y2,
            [r, g, b],
            line_width,
        )
        .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Draw an ellipse on a page.
    ///
    /// `cx`, `cy` is the center; `rx`, `ry` are the radii.
    /// `fill_r/g/b` is the fill color; pass negative to skip fill.
    /// `stroke_r/g/b` is the stroke color; pass negative to skip stroke.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_ellipse(
        &mut self,
        page_index: usize,
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        fill_r: f64,
        fill_g: f64,
        fill_b: f64,
        stroke_r: f64,
        stroke_g: f64,
        stroke_b: f64,
        line_width: f64,
    ) -> Result<(), JsError> {
        let fill = if fill_r >= 0.0 {
            Some([fill_r, fill_g, fill_b])
        } else {
            None
        };
        let stroke = if stroke_r >= 0.0 {
            Some([stroke_r, stroke_g, stroke_b])
        } else {
            None
        };
        let style = crate::editor::RectStyle {
            fill,
            stroke,
            line_width,
        };
        crate::editor::draw_ellipse(&mut self.editor, page_index, cx, cy, rx, ry, &style)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Place a JPEG image on a page.
    ///
    /// `x`, `y` is the bottom-left position; `display_w`, `display_h` is the
    /// rendered size in points. `pixel_width`, `pixel_height` are the JPEG
    /// dimensions in pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn place_jpeg(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        display_w: f64,
        display_h: f64,
        jpeg_data: &[u8],
        pixel_width: u32,
        pixel_height: u32,
    ) -> Result<(), JsError> {
        crate::editor::place_jpeg(
            &mut self.editor,
            page_index,
            x,
            y,
            display_w,
            display_h,
            jpeg_data,
            pixel_width,
            pixel_height,
        )
        .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Place a raw RGB image on a page.
    ///
    /// `pixels` is row-major RGB bytes (3 bytes per pixel).
    #[allow(clippy::too_many_arguments)]
    pub fn place_rgb_image(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        display_w: f64,
        display_h: f64,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), JsError> {
        crate::editor::place_image(
            &mut self.editor,
            page_index,
            x,
            y,
            display_w,
            display_h,
            pixels,
            width,
            height,
            3,
        )
        .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── Annotations ──────────────────────────────────────────────────────────

    /// Add a text (sticky note) annotation to a page.
    ///
    /// `x`, `y`, `width`, `height` are in PDF user-space points.
    pub fn add_text_annotation(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        contents: &str,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::Text {
                contents: contents.to_string(),
                open: false,
            },
            [x, y, x + width, y + height],
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add a styled free-text box annotation to a page.
    #[allow(clippy::too_many_arguments)]
    pub fn add_text_box(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        text: &str,
        font_name: &str,
        font_size: f64,
        r: f64,
        g: f64,
        b: f64,
        align: u8,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        let da = format!("/{} {} Tf {} {} {} rg", font_name, font_size, r, g, b);
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::FreeText {
                contents: text.to_string(),
                default_appearance: da,
                align: Some(align),
            },
            [x, y, x + width, y + height],
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add a highlight annotation to a page.
    ///
    /// `quad_points` is a flat array of 8·n values describing n quadrilaterals.
    pub fn add_highlight(
        &mut self,
        page_index: usize,
        quad_points: &[f64],
        r: f64,
        g: f64,
        b: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        if quad_points.len() < 8 {
            return Err(JsError::new("quad_points must have at least 8 values"));
        }
        let bbox = bbox_from_quad_points(quad_points);
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::Highlight {
                color: [r, g, b],
                quad_points: quad_points.to_vec(),
            },
            bbox,
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add a strikeout annotation to a page.
    pub fn add_strikeout(
        &mut self,
        page_index: usize,
        quad_points: &[f64],
        r: f64,
        g: f64,
        b: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        if quad_points.len() < 8 {
            return Err(JsError::new("quad_points must have at least 8 values"));
        }
        let bbox = bbox_from_quad_points(quad_points);
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::StrikeOut {
                color: [r, g, b],
                quad_points: quad_points.to_vec(),
            },
            bbox,
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add a link annotation to a page.
    pub fn add_link(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        uri: &str,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::Link {
                uri: uri.to_string(),
            },
            [x, y, x + width, y + height],
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add an underline annotation to a page.
    pub fn add_underline(
        &mut self,
        page_index: usize,
        quad_points: &[f64],
        r: f64,
        g: f64,
        b: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        if quad_points.len() < 8 {
            return Err(JsError::new("quad_points must have at least 8 values"));
        }
        let bbox = bbox_from_quad_points(quad_points);
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::Underline {
                color: [r, g, b],
                quad_points: quad_points.to_vec(),
            },
            bbox,
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Mark a rectangular area on a page for redaction.
    #[allow(clippy::too_many_arguments)]
    pub fn add_redact(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        r: f64,
        g: f64,
        b: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::Redact {
                overlay_color: [r, g, b],
            },
            [x, y, x + width, y + height],
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.pending_redact_zones.push(
            crate::editor::RedactZone::new(page_index, [x, y, x + width, y + height])
                .with_color([r, g, b]),
        );
        Ok(())
    }

    /// Permanently apply all pending redactions and rewrite the PDF.
    pub fn apply_redactions(&mut self) -> Result<(), JsError> {
        let zones = std::mem::take(&mut self.pending_redact_zones);
        let new_bytes = crate::editor::apply_redactions(&mut self.editor, &zones)
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.editor = crate::editor::PdfEditor::open(new_bytes.clone())
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.original_bytes = new_bytes;
        Ok(())
    }

    /// Add a stamp annotation to a page.
    ///
    /// `name` is the stamp label, e.g. "Approved", "Draft", "Confidential".
    /// `r`, `g`, `b` are the stamp colour components [0.0–1.0].
    #[allow(clippy::too_many_arguments)]
    pub fn add_stamp(
        &mut self,
        page_index: usize,
        name: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        r: f64,
        g: f64,
        b: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::Stamp {
                name: name.to_string(),
                color: [r, g, b],
            },
            [x, y, x + width, y + height],
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Embed a file in the PDF as a FileAttachment annotation on the given page.
    ///
    /// `file_bytes` is the raw file content; `filename` is the embedded file name
    /// shown in the attachment panel; `description` is the annotation tooltip.
    #[allow(clippy::too_many_arguments)]
    pub fn add_file_attachment(
        &mut self,
        page_index: usize,
        file_bytes: &[u8],
        filename: &str,
        description: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        let builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::FileAttachment {
                file_data: file_bytes.to_vec(),
                filename: filename.to_string(),
                description: description.to_string(),
                icon_name: "PushPin".to_string(),
            },
            [x, y, x + width, y + height],
        );
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Flatten all annotations on a single page into the content stream.
    ///
    /// After this call the page has no `/Annots` and annotation visuals are
    /// part of the page content, visible in all viewers.
    pub fn flatten_annotations(&mut self, page_index: usize) -> Result<(), JsError> {
        crate::editor::flatten_annotations(&mut self.editor, page_index)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Flatten annotations on every page.
    pub fn flatten_all_annotations(&mut self) -> Result<(), JsError> {
        crate::editor::flatten_all_annotations(&mut self.editor)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add a freehand ink annotation to a page.
    #[allow(clippy::too_many_arguments)]
    pub fn add_ink(
        &mut self,
        page_index: usize,
        points: &[f64],
        r: f64,
        g: f64,
        b: f64,
        line_width: f64,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_annotate, "annotate")?;
        if points.len() < 4 || !points.len().is_multiple_of(2) {
            return Err(JsError::new(
                "points must be an even-length array with at least 4 values",
            ));
        }
        let stroke: Vec<[f64; 2]> = points.chunks_exact(2).map(|c| [c[0], c[1]]).collect();
        let xs: Vec<f64> = stroke.iter().map(|p| p[0]).collect();
        let ys: Vec<f64> = stroke.iter().map(|p| p[1]).collect();
        let pad = line_width / 2.0;
        let bbox = [
            xs.iter().cloned().fold(f64::MAX, f64::min) - pad,
            ys.iter().cloned().fold(f64::MAX, f64::min) - pad,
            xs.iter().cloned().fold(f64::MIN, f64::max) + pad,
            ys.iter().cloned().fold(f64::MIN, f64::max) + pad,
        ];
        let mut builder = crate::editor::AnnotationBuilder::new(
            crate::editor::AnnotationType::Ink {
                ink_list: vec![stroke],
            },
            bbox,
        );
        builder = builder.subject(&format!("w={} r={} g={} b={}", line_width, r, g, b));
        crate::editor::add_annotation(&mut self.editor, page_index, builder)
            .map(|_| ())
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── In-place text editing ────────────────────────────────────────────────

    /// Replace a text span in a page's content stream in-place.
    ///
    /// `x`, `y`, `font_size` identify the span (from `extract_text_spans`).
    /// `old_text` is the original text to find; `new_text` is the replacement.
    /// Returns `true` if a replacement was made, `false` if no match found
    /// (e.g. scanned/image PDF with no extractable text).
    #[allow(clippy::too_many_arguments)]
    pub fn replace_text_in_stream(
        &mut self,
        page_index: usize,
        x: f64,
        y: f64,
        width: f64,
        font_size: f64,
        old_text: &str,
        new_text: &str,
    ) -> Result<bool, JsError> {
        let target = crate::editor::TextEditTarget {
            x,
            y,
            width,
            font_size,
            old_text: old_text.to_owned(),
        };
        crate::editor::replace_text_in_page(&mut self.editor, page_index, &target, new_text)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── Edit session (ONLYOFFICE-style enter / commit / exit) ────────────────

    /// Enter edit mode for a page: parse the content stream into indexed text
    /// frames and store the session internally.
    ///
    /// Returns a JSON array of frame objects:
    /// `[{id, text, x, y, font_size, font_name, resource_key}, ...]`
    ///
    /// `x`/`y` are CTM-corrected PDF user-space coordinates (origin
    /// bottom-left).  Returns `"[]"` for scanned/image-only pages.
    pub fn enter_edit_mode(&mut self, page_index: usize) -> Result<String, JsError> {
        log::debug!("[pdf-core] enter_edit_mode page={}", page_index);
        let session = crate::editor::build_edit_session(&self.editor.doc, page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let json = frames_to_json(&self.editor.doc, page_index, &session.frames);
        self.edit_sessions.insert(page_index, session);
        Ok(json)
    }

    /// Patch one text frame in the active edit session for `page_index`.
    ///
    /// `frame_id` is the `id` field from a frame returned by `enter_edit_mode`.
    /// Returns `true` if the frame was found and patched; `false` if the session
    /// does not exist or `frame_id` is out of range.
    pub fn commit_text_edit(&mut self, page_index: usize, frame_id: usize, new_text: &str) -> bool {
        log::debug!(
            "[pdf-core] commit_text_edit page={} frame={} text={:?}",
            page_index,
            frame_id,
            new_text
        );
        match self.edit_sessions.get_mut(&page_index) {
            Some(session) => crate::editor::patch_frame(session, frame_id, new_text),
            None => false,
        }
    }

    /// Exit edit mode: serialize all patched ops back to the page content
    /// stream and clear the session.
    ///
    /// Call `save()` afterwards to get the updated PDF bytes.
    ///
    /// Returns `true` if a modified session was actually written back to the
    /// page (so the host knows it must re-render), `false` if nothing changed.
    pub fn exit_edit_mode(&mut self, page_index: usize) -> Result<bool, JsError> {
        log::debug!("[pdf-core] exit_edit_mode page={}", page_index);
        if let Some(session) = self.edit_sessions.remove(&page_index) {
            // Only write back a session that was actually patched via
            // `commit_text_edit`. An unmodified session was built only to feed
            // overlay frame metadata; re-serializing its pristine ops would
            // overwrite the page and clobber any surgical `text_edit_commit`.
            if session.dirty {
                crate::editor::commit_edit_session(&mut self.editor, page_index, &session)
                    .map_err(|e| JsError::new(&e.to_string()))?;
                return Ok(true);
            }
            log::debug!(
                "[pdf-core] exit_edit_mode page={} — session unmodified, skipping write-back",
                page_index
            );
        }
        Ok(false)
    }

    // ── Metadata ─────────────────────────────────────────────────────────────

    /// Update document metadata.
    ///
    /// Pass an empty string to leave a field unchanged.
    pub fn set_metadata(
        &mut self,
        title: &str,
        author: &str,
        subject: &str,
        keywords: &str,
    ) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_modify, "modify")?;
        let fields = crate::editor::MetadataFields {
            title: if title.is_empty() { None } else { Some(title) },
            author: if author.is_empty() {
                None
            } else {
                Some(author)
            },
            subject: if subject.is_empty() {
                None
            } else {
                Some(subject)
            },
            keywords: if keywords.is_empty() {
                None
            } else {
                Some(keywords)
            },
            creator: None,
            producer: None,
            mod_date: "",
        };
        crate::editor::set_metadata(&mut self.editor, &fields)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── Undo / Redo ────────────────────────────────────────────────────────

    /// Undo the most recent committed edit. Returns `true` if a change was
    /// reverted, `false` if there was nothing to undo.
    ///
    /// Any open caret session and the cached text-edit model are discarded so
    /// a following `text_edit_enter` rebuilds against the restored state (the
    /// writer generation advanced, so the rebuild is forced regardless).
    pub fn undo(&mut self) -> bool {
        let did = self.editor.undo();
        if did {
            self.invalidate_edit_caches();
        }
        did
    }

    /// Redo the most recently undone edit. Returns `true` if a change was
    /// re-applied, `false` if there was nothing to redo.
    pub fn redo(&mut self) -> bool {
        let did = self.editor.redo();
        if did {
            self.invalidate_edit_caches();
        }
        did
    }

    /// Whether a subsequent `undo` would do anything.
    pub fn can_undo(&self) -> bool {
        self.editor.can_undo()
    }

    /// Whether a subsequent `redo` would do anything.
    pub fn can_redo(&self) -> bool {
        self.editor.can_redo()
    }

    /// Whether this PDF carries a digital signature (AcroForm `/SigFlags`).
    ///
    /// Editing and saving will invalidate the signature, so the host should
    /// warn the user (or block editing) when this returns `true`.
    pub fn is_signed(&self) -> bool {
        self.editor.doc.is_signed()
    }

    /// Return the document's operation permissions as a JSON object.
    ///
    /// For unencrypted documents all permissions are `true`.
    /// Keys: `can_print`, `can_modify`, `can_copy_text`, `can_annotate`,
    /// `can_fill_forms`, `can_assemble`.
    #[cfg(feature = "crypto")]
    pub fn get_permissions(&self) -> String {
        let perms = self
            .editor
            .doc
            .permissions()
            .unwrap_or(crate::crypto::handler::Permissions {
                can_print: true,
                can_modify: true,
                can_copy_text: true,
                can_annotate: true,
                can_fill_forms: true,
                can_extract_accessibility: true,
                can_assemble: true,
                can_print_high_quality: true,
            });
        format!(
            r#"{{"can_print":{},"can_modify":{},"can_copy_text":{},"can_annotate":{},"can_fill_forms":{},"can_assemble":{}}}"#,
            perms.can_print,
            perms.can_modify,
            perms.can_copy_text,
            perms.can_annotate,
            perms.can_fill_forms,
            perms.can_assemble,
        )
    }

    /// Drop derived edit caches after an undo/redo so they rebuild from the
    /// restored writer state on next use.
    fn invalidate_edit_caches(&mut self) {
        self.active_text_edit = None;
        self.text_edit_model = None;
        self.edit_model_doc = None;
        self.committed_bytes.clear();
        self.text_edit_model_generation = u64::MAX;
    }

    // ── Form filling ─────────────────────────────────────────────────────────

    /// Return all interactive form fields as a JSON array.
    ///
    /// Each element contains: `id`, `name`, `full_name`, `field_type`, `value`,
    /// `checked`, `readonly`, `required`, `rect` (array), and `options` (array).
    /// Returns `"[]"` when the document has no `/AcroForm`.
    pub fn get_form_fields(&self) -> Result<String, JsError> {
        let fields = crate::forms::read_form_fields(&self.editor.doc)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let mut json = String::from("[");
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            let opts = f
                .options
                .iter()
                .map(|o| super::json_str(o))
                .collect::<Vec<_>>()
                .join(",");
            json.push_str(&format!(
                r#"{{"id":{},"name":{},"full_name":{},"field_type":{},"value":{},"checked":{},"readonly":{},"required":{},"rect":[{},{},{},{}],"options":[{}]}}"#,
                f.id,
                super::json_str(&f.name),
                super::json_str(&f.full_name),
                super::json_str(&format!("{:?}", f.field_type)),
                super::json_str(&f.value),
                f.checked,
                f.readonly,
                f.required,
                f.rect[0], f.rect[1], f.rect[2], f.rect[3],
                opts,
            ));
        }
        json.push(']');
        Ok(json)
    }

    /// Set the value of a form field by name (full or partial).
    ///
    /// For text fields, `value` is the new string.
    /// For checkboxes, `value` should be `"true"`, `"Yes"` (checked) or anything else (unchecked).
    /// For combo/list fields, `value` is the export value to select.
    pub fn set_field_value(&mut self, field_name: &str, value: &str) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_fill_forms, "fill_forms")?;
        let fields = crate::forms::read_form_fields(&self.editor.doc)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let field = fields
            .iter()
            .find(|f| f.full_name == field_name || f.name == field_name)
            .ok_or_else(|| JsError::new(&format!("field '{}' not found", field_name)))?
            .clone();
        match field.field_type {
            crate::forms::FieldType::Text => {
                crate::forms::set_text_field(&mut self.editor, &field, value)
            }
            crate::forms::FieldType::Checkbox => {
                let checked = value == "true" || value == "Yes";
                crate::forms::set_checkbox(&mut self.editor, &field, checked)
            }
            crate::forms::FieldType::List | crate::forms::FieldType::Combo => {
                crate::forms::set_combo_or_list(&mut self.editor, &field, value)
            }
            _ => Err(crate::error::PdfError::invalid_structure(
                "set_field_value: unsupported field type",
            )),
        }
        .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Get the current value of a form field by name (full or partial).
    pub fn get_field_value(&self, field_name: &str) -> Result<String, JsError> {
        let fields = crate::forms::read_form_fields(&self.editor.doc)
            .map_err(|e| JsError::new(&e.to_string()))?;
        fields
            .iter()
            .find(|f| f.full_name == field_name || f.name == field_name)
            .map(|f| f.value.clone())
            .ok_or_else(|| JsError::new(&format!("field '{}' not found", field_name)))
    }

    // ── FDF / XFDF ───────────────────────────────────────────────────────────

    /// Export all form field values as FDF bytes.
    ///
    /// Returns a valid FDF 1.2 file that can be saved to disk or passed back
    /// to [`import_fdf`] for a round-trip. Requires a Pro license.
    pub fn export_fdf(&self) -> Result<js_sys::Uint8Array, JsError> {
        crate::forms::export_fdf(&self.editor.doc)
            .map(|v| js_sys::Uint8Array::from(v.as_slice()))
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Import FDF bytes and fill the matching form fields.
    ///
    /// Fields not present in the FDF are left unchanged. Requires a Pro license
    /// (enforced per-field by the underlying `set_*` helpers).
    pub fn import_fdf(&mut self, fdf_bytes: &[u8]) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_fill_forms, "fill_forms")?;
        crate::forms::import_fdf(&mut self.editor, fdf_bytes)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Export all form field values as an XFDF string.
    ///
    /// Returns a valid XFDF 1.0 XML document. Requires a Pro license.
    pub fn export_xfdf(&self) -> Result<String, JsError> {
        crate::forms::export_xfdf(&self.editor.doc).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Import an XFDF string and fill the matching form fields.
    ///
    /// Fields not present in the XFDF are left unchanged. Requires a Pro
    /// license (enforced per-field by the underlying `set_*` helpers).
    pub fn import_xfdf(&mut self, xfdf_str: &str) -> Result<(), JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.editor.doc, |p| p.can_fill_forms, "fill_forms")?;
        crate::forms::import_xfdf(&mut self.editor, xfdf_str)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── Form flattening ───────────────────────────────────────────────────────

    /// Flatten all interactive form fields on a single page into static content.
    ///
    /// Widget annotations are removed and their visual appearance is burned into
    /// the page content stream.  Requires a Pro license.
    pub fn flatten_form_fields(&mut self, page_index: usize) -> Result<(), JsError> {
        crate::forms::flatten_form_fields(&mut self.editor, page_index)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Flatten all interactive form fields across every page.
    ///
    /// After this call the document contains no Widget annotations.
    /// Requires a Pro license.
    pub fn flatten_all_form_fields(&mut self) -> Result<(), JsError> {
        crate::forms::flatten_all_form_fields(&mut self.editor)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── JavaScript actions ────────────────────────────────────────────────────

    /// Enable PDF JavaScript action execution for this document.
    ///
    /// Initialises the embedded QuickJS runtime (gated on the `js-actions`
    /// feature) and runs any document-open (`/OpenAction /JavaScript`) scripts.
    /// Field modifications produced by those scripts are applied immediately.
    ///
    /// Returns a `JsError` when the feature is not compiled in, or when the
    /// QuickJS runtime cannot be initialised.
    #[cfg(feature = "js-actions")]
    pub fn enable_javascript(&mut self) -> Result<(), JsError> {
        let engine = crate::js::JsEngine::new().map_err(|e| JsError::new(&e.to_string()))?;
        let result =
            crate::js::dispatch_doc_event(&self.editor.doc, &engine, crate::js::JsEvent::DocOpen)
                .map_err(|e| JsError::new(&e.to_string()))?;
        // Apply field modifications requested by the doc-open script.
        for (name, value) in result.modified_fields {
            let _ = self.set_field_value(&name, &value);
        }
        self.js_engine = Some(engine);
        Ok(())
    }

    /// Run a PDF JavaScript action script for a named field event.
    ///
    /// `event_type` must be one of: `"keystroke"`, `"validate"`, `"format"`,
    /// `"calculate"`, `"mouseup"`.  Returns a JSON object:
    /// `{"rc": bool, "value": string|null, "alerts": string[]}`.
    ///
    /// Returns `JsError` when JavaScript is not enabled (call
    /// [`enable_javascript`](WasmEditor::enable_javascript) first) or the
    /// feature is not compiled in.
    #[cfg(feature = "js-actions")]
    pub fn run_field_action(
        &mut self,
        field_name: &str,
        event_type: &str,
        value: &str,
        change: &str,
    ) -> Result<String, JsError> {
        let engine = self.js_engine.as_ref().ok_or_else(|| {
            JsError::new("JavaScript not enabled; call enable_javascript() first")
        })?;

        let event = match event_type {
            "keystroke" => crate::js::JsEvent::FieldKeystroke {
                field_name: field_name.to_owned(),
                value: value.to_owned(),
                change: change.to_owned(),
            },
            "validate" => crate::js::JsEvent::FieldValidate {
                field_name: field_name.to_owned(),
                value: value.to_owned(),
            },
            "format" => crate::js::JsEvent::FieldFormat {
                field_name: field_name.to_owned(),
                value: value.to_owned(),
            },
            "calculate" => crate::js::JsEvent::FieldCalculate {
                field_name: field_name.to_owned(),
            },
            "mouseup" => crate::js::JsEvent::ButtonMouseUp {
                field_name: field_name.to_owned(),
            },
            other => {
                return Err(JsError::new(&format!(
                    "unknown event_type '{}'; use keystroke/validate/format/calculate/mouseup",
                    other
                )))
            }
        };

        let result = crate::js::dispatch_doc_event(&self.editor.doc, engine, event)
            .map_err(|e| JsError::new(&e.to_string()))?;

        // Apply field modifications.
        for (name, val) in &result.modified_fields {
            let _ = self.set_field_value(name, val);
        }

        let value_json = match &result.value {
            Some(v) => super::json_str(v),
            None => "null".to_string(),
        };
        let alerts_json: Vec<String> = result.alerts.iter().map(|a| super::json_str(a)).collect();
        Ok(format!(
            r#"{{"rc":{},"value":{},"alerts":[{}]}}"#,
            result.rc,
            value_json,
            alerts_json.join(","),
        ))
    }

    // ── Bookmarks ────────────────────────────────────────────────────────────

    /// Replace the document outline (bookmarks) from a JSON array.
    ///
    /// Each element: `{ "title": string, "page_index": number, "y_position": number,
    /// "open": bool, "bold": bool, "italic": bool, "color": [r,g,b] | null,
    /// "children": [...] }`.
    ///
    /// Pass `"[]"` to remove all bookmarks.
    pub fn set_outline(&mut self, outline_json: &str) -> Result<(), JsError> {
        let entries = parse_outline_json(outline_json).map_err(|e| JsError::new(&e.to_string()))?;
        crate::document::set_document_outline(&mut self.editor, &entries)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    // ── Save ─────────────────────────────────────────────────────────────────

    /// Serialise the edited document as PDF bytes (incremental update).
    ///
    /// If no Pro/Enterprise license is active a trial watermark is burned onto
    /// every page before serialisation. Subsequent calls to `save` or edit
    /// methods work on the new bytes.
    pub fn save(&mut self) -> Result<js_sys::Uint8Array, JsError> {
        log::debug!("[pdf-core] WasmEditor::save — serialising incremental update");
        // Diagnostic: if a text-edit session has dirty (patched-but-not-flushed)
        // streams, those edits live only in the in-memory model — `save_append`
        // serialises the WRITER POOL, which does NOT contain them yet. So this save
        // will NOT include the pending edit unless `text_edit_exit` flushed first.
        if let Some(model) = &self.text_edit_model {
            if model.session.dirty {
                log::warn!(
                    "[save] ⚠ text_edit_model is DIRTY but NOT flushed — pending edits on page {} will be LOST from this save (call text_edit_exit before save). streams={}",
                    self.text_edit_page,
                    model.session.streams.len()
                );
            } else {
                log::warn!("[save] text_edit_model present, not dirty — nothing pending");
            }
        } else {
            log::warn!("[save] no text_edit_model — pure pool save");
        }
        // Apply the trial watermark ONCE. It then lives in the writer pool (and is
        // baked into the collapsed main content stream on the next text flush), so
        // re-applying on every save would multiply watermark streams across every
        // page (file bloat) and bump the generation on each commit (forcing a
        // FULL-REBUILD that renumbers blocks and desyncs the host's selection).
        if !self.watermarked && crate::license::current_tier() == crate::license::Tier::Free {
            crate::license::watermark::apply_trial_watermark(&mut self.editor)
                .map_err(|e| JsError::new(&e.to_string()))?;
            self.watermarked = true;
        }
        let new_bytes = self.editor.save_append(&self.original_bytes).map_err(|e| {
            log::error!("[pdf-core] WasmEditor::save failed: {}", e);
            JsError::new(&e.to_string())
        })?;
        log::info!(
            "[pdf-core] WasmEditor::save — {} bytes written",
            new_bytes.len()
        );
        self.original_bytes = new_bytes.clone();
        Ok(js_sys::Uint8Array::from(new_bytes.as_slice()))
    }

    /// Optimize the current document and return the optimized PDF bytes.
    ///
    /// `options_json` is a JSON object with optional boolean/number fields:
    /// `recompress_streams`, `deduplicate_resources`, `remove_unused_objects`,
    /// `downsample_images`, `image_max_dpi`. Missing fields use the defaults.
    /// Requires a Pro license.
    #[cfg(feature = "crypto")]
    pub fn optimize(&mut self, options_json: &str) -> Result<js_sys::Uint8Array, JsError> {
        let options = parse_optimization_options(options_json);
        let current_bytes = self
            .editor
            .save_append(&self.original_bytes)
            .map_err(|e| JsError::new(&e.to_string()))?;
        crate::writer::optimizer::optimize(&current_bytes, &options)
            .map(|v| js_sys::Uint8Array::from(v.as_slice()))
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Render `page_index` from the editor's CURRENT (edited) state **without** a
    /// byte reparse: overlay the writer-pool objects (the surgical commit's new
    /// page dict + content stream) on the pristine parsed doc, render, then remove
    /// the overlay. Lets the host paint a just-committed page immediately, skipping
    /// the whole-document `WasmDocument::parse` round-trip. `scale` is device px per
    /// PDF point.
    #[cfg(feature = "render")]
    pub fn render_page(
        &self,
        page_index: usize,
        scale: f64,
    ) -> Result<super::document::RenderResult, JsError> {
        let overrides: std::collections::HashMap<u32, crate::parser::objects::PdfObject> = self
            .editor
            .writer
            .all_ids()
            .into_iter()
            .filter_map(|id| self.editor.writer.get_object(id).map(|o| (id, o.clone())))
            .collect();
        self.editor.doc.set_overrides(overrides);
        // Re-insert uncompressed bytes for committed streams so render_page_rgba
        // can skip the flate-decompress step (set_overrides just cleared these).
        for (id, bytes) in &self.committed_bytes {
            self.editor.doc.preload_stream(*id, bytes);
        }
        let rendered = crate::render::render_page_rgba(&self.editor.doc, page_index, scale);
        self.editor.doc.clear_overrides();
        let (w, h, data) = rendered.map_err(|e| JsError::new(&e.to_string()))?;
        Ok(super::document::RenderResult::new(w, h, data))
    }
}

// ---------------------------------------------------------------------------
// Digital signature WASM bindings (requires `signatures` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "signatures")]
#[wasm_bindgen]
impl WasmEditor {
    /// Sign the PDF and return the signed bytes as a `Uint8Array`.
    ///
    /// - `private_key_der` — PKCS#8 RSA private key in DER.
    /// - `cert_der` — Signer X.509 certificate in DER.
    /// - `field_name` — PDF field name for the signature widget (`/T`).
    /// - `page_index` — 0-based page on which to place the widget.
    /// - `x1`, `y1`, `x2`, `y2` — Widget rectangle in PDF user space.
    ///
    /// Requires an Enterprise license.
    #[wasm_bindgen]
    #[allow(clippy::too_many_arguments)]
    pub fn sign_pdf(
        &self,
        private_key_der: &[u8],
        cert_der: &[u8],
        field_name: &str,
        page_index: usize,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    ) -> Result<js_sys::Uint8Array, JsError> {
        let options = crate::signatures::SignatureOptions {
            rect: [x1, y1, x2, y2],
            page_index,
            field_name: field_name.to_owned(),
            reason: None,
            location: None,
            contact_info: None,
        };
        let signed = crate::signatures::sign_document(
            &self.original_bytes,
            private_key_der,
            cert_der,
            &options,
        )
        .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(js_sys::Uint8Array::from(signed.as_slice()))
    }

    /// Verify all digital signatures in the currently loaded PDF.
    ///
    /// Returns a JSON array of objects:
    /// `[{"field_name":"Sig1","valid":true,"covers_whole_file":true,"signer_name":"Alice"}]`
    #[wasm_bindgen]
    pub fn verify_signatures(&self) -> Result<String, JsError> {
        let results = crate::signatures::verify_signatures(&self.original_bytes)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(sigs_to_json(&results))
    }
}

#[cfg(feature = "signatures")]
fn sigs_to_json(results: &[crate::signatures::SignatureVerification]) -> String {
    let items: Vec<String> = results
        .iter()
        .map(|r| {
            let signer = match &r.signer_name {
                Some(n) => format!(",\"signer_name\":{}", super::json_str(n)),
                None => String::new(),
            };
            let error = match &r.error {
                Some(e) => format!(",\"error\":{}", super::json_str(e)),
                None => String::new(),
            };
            format!(
                "{{\"field_name\":{},\"valid\":{},\"covers_whole_file\":{}{}{}}}",
                super::json_str(&r.field_name),
                r.signature_valid,
                r.covers_whole_file,
                signer,
                error,
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Parse a JSON object into `OptimizationOptions`, using defaults for missing keys.
#[cfg(feature = "crypto")]
fn parse_optimization_options(json: &str) -> crate::writer::optimizer::OptimizationOptions {
    use crate::writer::optimizer::OptimizationOptions;
    let mut opts = OptimizationOptions::default();

    if let Some(v) = json_opt_bool(json, "recompress_streams") {
        opts.recompress_streams = v;
    }
    if let Some(v) = json_opt_bool(json, "deduplicate_resources") {
        opts.deduplicate_resources = v;
    }
    if let Some(v) = json_opt_bool(json, "remove_unused_objects") {
        opts.remove_unused_objects = v;
    }
    if let Some(v) = json_opt_bool(json, "downsample_images") {
        opts.downsample_images = v;
    }
    if let Some(v) = json_opt_u32(json, "image_max_dpi") {
        opts.image_max_dpi = v;
    }
    opts
}

/// Extract an optional boolean field from a flat JSON object string.
#[cfg(feature = "crypto")]
fn json_opt_bool(json: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let rest = json[pos + needle.len()..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Extract an optional u32 field from a flat JSON object string.
#[cfg(feature = "crypto")]
fn json_opt_u32(json: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let rest = json[pos + needle.len()..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Parse a `TextWatermark` from a JSON object string, applying defaults for missing fields.
fn parse_text_watermark_json(json: &str) -> crate::error::Result<crate::editor::TextWatermark> {
    let mut wm = crate::editor::TextWatermark::default();
    if let Some(text) = json_str_field(json, "text")? {
        wm.text = text;
    }
    if let Some(v) = json_f64_field(json, "font_size")? {
        wm.font_size = v;
    }
    if let Some(c) = json_color_field(json, "color")? {
        wm.color = c;
    }
    if let Some(v) = json_f64_field(json, "opacity")? {
        wm.opacity = v;
    }
    if let Some(v) = json_f64_field(json, "angle_degrees")? {
        wm.angle_degrees = v;
    }
    if let Some(v) = json_bool_field(json, "repeat")? {
        wm.repeat = v;
    }
    if let Some(v) = json_f64_field(json, "tile_spacing")? {
        wm.tile_spacing = v;
    }
    Ok(wm)
}

fn frames_to_json(
    doc: &crate::parser::objects::PdfDocument,
    page_index: usize,
    frames: &[crate::editor::EditableFrame],
) -> String {
    use super::json_str;
    use std::collections::HashMap;

    // Cache metrics per (resource_key, font_size) so each distinct font+size is
    // resolved once; `width` is the real advance width so the overlay never has
    // to fall back to an undefined field (which produced NaN rects).
    let mut cache: HashMap<(String, u64), Option<crate::editor::PdfFontMetrics>> = HashMap::new();
    let parts: Vec<String> = frames
        .iter()
        .map(|f| {
            let key = (f.resource_key.clone(), f64::to_bits(f.font_size));
            let metrics = cache.entry(key).or_insert_with(|| {
                crate::editor::font_metrics_for(doc, page_index, &f.resource_key, f.font_size)
                    .ok()
                    .flatten()
            });
            let width = match metrics.as_ref() {
                Some(m) => crate::editor::text_width(m, &f.text),
                // Proportional estimate when the font can't be resolved (CID, etc.).
                None => f.text.chars().count() as f64 * 0.5 * f.font_size,
            };
            format!(
                r#"{{"id":{},"text":{},"x":{},"y":{},"width":{},"font_size":{},"font_name":{},"resource_key":{}}}"#,
                f.id,
                json_str(&f.text),
                f.x,
                f.y,
                width,
                f.font_size,
                json_str(&f.font_name),
                json_str(&f.resource_key),
            )
        })
        .collect();
    format!("[{}]", parts.join(","))
}

// ---------------------------------------------------------------------------
// Outline JSON parser (for set_outline)
// ---------------------------------------------------------------------------

/// Parse a JSON array of outline entries into `Vec<OutlineEntry>`.
///
/// Accepts the format described in [`WasmEditor::set_outline`].
fn parse_outline_json(json: &str) -> crate::error::Result<Vec<crate::document::OutlineEntry>> {
    parse_outline_array(json.trim())
}

fn parse_outline_array(s: &str) -> crate::error::Result<Vec<crate::document::OutlineEntry>> {
    let s = s.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return Err(crate::error::PdfError::invalid_structure(
            "outline JSON must be an array",
        ));
    }
    let inner = &s[1..s.len() - 1];
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut in_string = false;
    let mut escape_next = false;

    let bytes = inner.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape_next = true,
            b'"' => in_string = !in_string,
            b'{' | b'[' if !in_string => depth += 1,
            b'}' | b']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    let chunk = inner[start..=i].trim();
                    if !chunk.is_empty() {
                        entries.push(parse_outline_object(chunk)?);
                    }
                    start = i + 1;
                    // skip the comma after closing brace
                    if start < bytes.len() && bytes[start] == b',' {
                        start += 1;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(entries)
}

fn parse_outline_object(s: &str) -> crate::error::Result<crate::document::OutlineEntry> {
    let title = json_str_field(s, "title")?.unwrap_or_default();
    let page_index = json_usize_field(s, "page_index")?.unwrap_or(0);
    let y_position = json_f64_field(s, "y_position")?.unwrap_or(0.0);
    let open = json_bool_field(s, "open")?.unwrap_or(false);
    let bold = json_bool_field(s, "bold")?.unwrap_or(false);
    let italic = json_bool_field(s, "italic")?.unwrap_or(false);
    let color = json_color_field(s, "color")?;
    let children = json_children_field(s)?;

    Ok(crate::document::OutlineEntry {
        title,
        page_index,
        y_position,
        open,
        bold,
        italic,
        color,
        children,
    })
}

/// Extract a JSON string field value.
fn json_str_field(obj: &str, key: &str) -> crate::error::Result<Option<String>> {
    let needle = format!("\"{}\"", key);
    let Some(pos) = obj.find(&needle) else {
        return Ok(None);
    };
    let rest = obj[pos + needle.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .ok_or_else(|| crate::error::PdfError::invalid_structure("expected ':' after JSON key"))?;
    let rest = rest.trim_start();
    if rest.starts_with('"') {
        Ok(Some(parse_json_string(rest)?))
    } else {
        Ok(None)
    }
}

/// Extract a JSON number as usize.
fn json_usize_field(obj: &str, key: &str) -> crate::error::Result<Option<usize>> {
    let needle = format!("\"{}\"", key);
    let Some(pos) = obj.find(&needle) else {
        return Ok(None);
    };
    let rest = obj[pos + needle.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .ok_or_else(|| crate::error::PdfError::invalid_structure("expected ':' after JSON key"))?
        .trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let num_str = &rest[..end];
    if num_str.is_empty() {
        return Ok(None);
    }
    num_str
        .parse::<usize>()
        .map(Some)
        .map_err(|_| crate::error::PdfError::invalid_structure("invalid usize in JSON"))
}

/// Extract a JSON number as f64.
fn json_f64_field(obj: &str, key: &str) -> crate::error::Result<Option<f64>> {
    let needle = format!("\"{}\"", key);
    let Some(pos) = obj.find(&needle) else {
        return Ok(None);
    };
    let rest = obj[pos + needle.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .ok_or_else(|| crate::error::PdfError::invalid_structure("expected ':' after JSON key"))?
        .trim_start();
    let end = rest
        .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+' | 'e' | 'E'))
        .unwrap_or(rest.len());
    let num_str = &rest[..end];
    if num_str.is_empty() {
        return Ok(None);
    }
    num_str
        .parse::<f64>()
        .map(Some)
        .map_err(|_| crate::error::PdfError::invalid_structure("invalid f64 in JSON"))
}

/// Extract a JSON boolean field.
fn json_bool_field(obj: &str, key: &str) -> crate::error::Result<Option<bool>> {
    let needle = format!("\"{}\"", key);
    let Some(pos) = obj.find(&needle) else {
        return Ok(None);
    };
    let rest = obj[pos + needle.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .ok_or_else(|| crate::error::PdfError::invalid_structure("expected ':' after JSON key"))?
        .trim_start();
    if rest.starts_with("true") {
        Ok(Some(true))
    } else if rest.starts_with("false") {
        Ok(Some(false))
    } else {
        Ok(None)
    }
}

/// Extract `"color": [r, g, b]` or `null`.
fn json_color_field(obj: &str, key: &str) -> crate::error::Result<Option<[f64; 3]>> {
    let needle = format!("\"{}\"", key);
    let Some(pos) = obj.find(&needle) else {
        return Ok(None);
    };
    let rest = obj[pos + needle.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .ok_or_else(|| crate::error::PdfError::invalid_structure("expected ':' after JSON key"))?
        .trim_start();
    if rest.starts_with("null") {
        return Ok(None);
    }
    if !rest.starts_with('[') {
        return Ok(None);
    }
    let end = rest
        .find(']')
        .ok_or_else(|| crate::error::PdfError::invalid_structure("unclosed color array"))?;
    let inner = &rest[1..end];
    let parts: Vec<&str> = inner.split(',').collect();
    if parts.len() != 3 {
        return Ok(None);
    }
    let r = parts[0]
        .trim()
        .parse::<f64>()
        .map_err(|_| crate::error::PdfError::invalid_structure("invalid color component"))?;
    let g = parts[1]
        .trim()
        .parse::<f64>()
        .map_err(|_| crate::error::PdfError::invalid_structure("invalid color component"))?;
    let b = parts[2]
        .trim()
        .parse::<f64>()
        .map_err(|_| crate::error::PdfError::invalid_structure("invalid color component"))?;
    Ok(Some([r, g, b]))
}

/// Extract the `"children"` array (recursive).
fn json_children_field(obj: &str) -> crate::error::Result<Vec<crate::document::OutlineEntry>> {
    let needle = "\"children\"";
    let Some(pos) = obj.find(needle) else {
        return Ok(Vec::new());
    };
    let rest = obj[pos + needle.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .ok_or_else(|| crate::error::PdfError::invalid_structure("expected ':' after 'children'"))?
        .trim_start();
    if !rest.starts_with('[') {
        return Ok(Vec::new());
    }
    // Find the matching ']' respecting nesting.
    let mut depth = 0i32;
    let mut end = 0usize;
    let mut in_string = false;
    let mut escape_next = false;
    for (i, b) in rest.bytes().enumerate() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape_next = true,
            b'"' => in_string = !in_string,
            b'[' if !in_string => depth += 1,
            b']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    parse_outline_array(&rest[..=end])
}

/// Parse a quoted JSON string starting at `s` (which must start with `"`).
fn parse_json_string(s: &str) -> crate::error::Result<String> {
    let mut chars = s.chars().peekable();
    if chars.next() != Some('"') {
        return Err(crate::error::PdfError::invalid_structure(
            "expected opening quote",
        ));
    }
    let mut result = String::new();
    loop {
        match chars.next() {
            None => {
                return Err(crate::error::PdfError::invalid_structure(
                    "unterminated JSON string",
                ))
            }
            Some('"') => break,
            Some('\\') => match chars.next() {
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some('/') => result.push('/'),
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16).map_err(|_| {
                        crate::error::PdfError::invalid_structure("invalid \\u escape")
                    })?;
                    result.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                _ => {}
            },
            Some(c) => result.push(c),
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// WasmPdfWriter — create a new PDF from scratch
// ---------------------------------------------------------------------------

/// Builder for creating a new PDF document.
///
/// Internally starts from a minimal blank template and adds pages on top.
/// Call [`WasmPdfWriter::build`] to serialise the result.
#[wasm_bindgen]
pub struct WasmPdfWriter {
    editor: crate::editor::PdfEditor,
    current_bytes: Vec<u8>,
    cleared: bool,
}

const BLANK_PDF: &[u8] = include_bytes!("../../tests/fixtures/minimal.pdf");

#[wasm_bindgen]
impl WasmPdfWriter {
    /// Create a new empty PDF document.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmPdfWriter, JsError> {
        let bytes = BLANK_PDF.to_vec();
        let editor = crate::editor::PdfEditor::open(bytes.clone())
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(WasmPdfWriter {
            editor,
            current_bytes: bytes,
            cleared: false,
        })
    }

    /// Append a blank page of the given size (in PDF points, 1 pt = 1/72 inch).
    pub fn add_page(&mut self, width_pt: f64, height_pt: f64) -> Result<(), JsError> {
        if !self.cleared {
            crate::editor::delete_page(&mut self.editor, 0)
                .map_err(|e| JsError::new(&e.to_string()))?;
            self.cleared = true;
        }
        let n = self
            .editor
            .doc
            .page_count()
            .map_err(|e| JsError::new(&e.to_string()))?;
        crate::editor::add_blank_page(&mut self.editor, n, width_pt, height_pt)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Serialise the document to PDF bytes.
    pub fn build(&mut self) -> Result<js_sys::Uint8Array, JsError> {
        let bytes = self
            .editor
            .save_append(&self.current_bytes)
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.current_bytes = bytes.clone();
        Ok(js_sys::Uint8Array::from(bytes.as_slice()))
    }
}

// ---------------------------------------------------------------------------
// Native-accessible byte-returning variants (for Rust tests and non-JS use)
// ---------------------------------------------------------------------------

impl WasmEditor {
    /// Like [`save`](WasmEditor::save) but returns raw `Vec<u8>` bytes.
    pub fn save_bytes(&mut self) -> crate::error::Result<Vec<u8>> {
        let bytes = self.editor.save_append(&self.original_bytes)?;
        self.original_bytes = bytes.clone();
        Ok(bytes)
    }
}

impl WasmPdfWriter {
    /// Like [`build`](WasmPdfWriter::build) but returns raw `Vec<u8>` bytes.
    pub fn build_bytes(&mut self) -> crate::error::Result<Vec<u8>> {
        let bytes = self.editor.save_append(&self.current_bytes)?;
        self.current_bytes = bytes.clone();
        Ok(bytes)
    }
}

// ---------------------------------------------------------------------------
// Watermark
// ---------------------------------------------------------------------------

#[wasm_bindgen]
impl WasmEditor {
    /// Add a text watermark to a single page (0-based `page_index`).
    ///
    /// `options_json` is a JSON object with optional keys:
    /// `text`, `font_size`, `color` ([r,g,b] in 0–1), `opacity`,
    /// `angle_degrees`, `repeat`, `tile_spacing`. Missing keys use defaults.
    /// Requires a Pro license.
    pub fn add_text_watermark(
        &mut self,
        page_index: usize,
        options_json: &str,
    ) -> Result<(), JsError> {
        let wm =
            parse_text_watermark_json(options_json).map_err(|e| JsError::new(&e.to_string()))?;
        crate::editor::add_text_watermark(&mut self.editor, page_index, &wm)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add a text watermark to every page in the document.
    ///
    /// Same `options_json` schema as [`add_text_watermark`]. Requires a Pro license.
    pub fn add_watermark_all_pages(&mut self, options_json: &str) -> Result<(), JsError> {
        let wm =
            parse_text_watermark_json(options_json).map_err(|e| JsError::new(&e.to_string()))?;
        crate::editor::add_watermark_all_pages(&mut self.editor, &wm)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Add an image watermark to a single page (0-based `page_index`).
    ///
    /// `pixels` — row-major raw pixel bytes. `channels`: 1=gray, 3=RGB, 4=CMYK.
    /// `rect_x1/y1/x2/y2` — placement in PDF user-space points.
    /// `opacity` — 0.0 invisible, 1.0 opaque. Requires a Pro license.
    #[allow(clippy::too_many_arguments)]
    pub fn add_image_watermark(
        &mut self,
        page_index: usize,
        pixels: &[u8],
        width: u32,
        height: u32,
        channels: u8,
        rect_x1: f64,
        rect_y1: f64,
        rect_x2: f64,
        rect_y2: f64,
        opacity: f64,
    ) -> Result<(), JsError> {
        let wm = crate::editor::ImageWatermark {
            pixels: pixels.to_vec(),
            width,
            height,
            channels,
            rect: [rect_x1, rect_y1, rect_x2, rect_y2],
            opacity,
        };
        crate::editor::add_image_watermark(&mut self.editor, page_index, &wm)
            .map_err(|e| JsError::new(&e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// PDF/A compliance — validate and convert
// ---------------------------------------------------------------------------

#[wasm_bindgen]
impl WasmEditor {
    /// Validate the open document against a PDF/A conformance level.
    ///
    /// `level` must be one of `"1b"`, `"2b"`, or `"3b"`.
    /// Returns a JSON array of violation objects:
    /// `[{"rule":"6.2.3","description":"…","obj_id":null}, …]`
    /// An empty array `[]` means the document is conformant.
    pub fn validate_pdfa(&self, level: &str) -> Result<String, JsError> {
        let violations = match level {
            "1b" => crate::compliance::validate_pdfa_1b(&self.editor.doc),
            "2b" => crate::compliance::validate_pdfa_2b(&self.editor.doc),
            "3b" => crate::compliance::validate_pdfa_3b(&self.editor.doc),
            _ => return Err(JsError::new("unknown PDF/A level; use '1b', '2b', or '3b'")),
        }
        .map_err(|e| JsError::new(&e.to_string()))?;

        let items: Vec<String> = violations
            .iter()
            .map(|v| {
                let desc = v.description.replace('\\', "\\\\").replace('"', "\\\"");
                let obj_id = match v.obj_id {
                    Some(id) => id.to_string(),
                    None => "null".to_string(),
                };
                format!(
                    r#"{{"rule":"{}","description":"{}","obj_id":{}}}"#,
                    v.rule, desc, obj_id
                )
            })
            .collect();

        Ok(format!("[{}]", items.join(",")))
    }

    /// Convert the open document to a PDF/A conformance level in place.
    ///
    /// `level` must be one of `"1b"`, `"2b"`, or `"3b"`.
    /// Call [`save`](WasmEditor::save) afterwards to get the updated bytes.
    /// Requires an Enterprise license.
    pub fn convert_to_pdfa(&mut self, level: &str) -> Result<(), JsError> {
        match level {
            "1b" => crate::compliance::convert_to_pdfa_1b(&mut self.editor),
            "2b" => crate::compliance::convert_to_pdfa_2b(&mut self.editor),
            "3b" => crate::compliance::convert_to_pdfa_3b(&mut self.editor),
            _ => return Err(JsError::new("unknown PDF/A level; use '1b', '2b', or '3b'")),
        }
        .map_err(|e| JsError::new(&e.to_string()))
    }
}
