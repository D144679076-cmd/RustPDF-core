//! WASM bindings for Word-style text editing (caret, selection, keys).
//!
//! Adds a stateful interactive layer on top of [`WasmEditor`]: `text_edit_enter`
//! lists editable blocks, `text_edit_open` starts a caret session for one block,
//! and the per-key methods mutate a [`TextEditEngine`].  The on-screen bitmap is
//! still produced by `WasmDocument::render_edit_block`; commits reuse the existing
//! `enter_edit_mode` / `commit_text_edit` path.

use wasm_bindgen::prelude::*;

use super::editor::WasmEditor;
use super::json_str;
use crate::editor::{
    build_text_model, font_metrics_for, text_width, Align, CharStyle, Dir, Measurer,
    PdfFontMetrics, TextEditEngine,
};

/// State for the block currently open for caret/selection editing.
pub(crate) struct ActiveTextEdit {
    /// Id of the open block within `WasmEditor::text_edit_blocks`.
    pub block_id: usize,
    /// The live single-line edit engine.
    pub engine: TextEditEngine,
    /// Glyph metrics for caret geometry, at the block's font + size.
    pub metrics: PdfFontMetrics,
    /// The raw `font_metrics_for` result (None when the font can't be resolved),
    /// cached so per-keystroke `text_edit_render_block` reuses it instead of
    /// re-inverting the ToUnicode CMap on every keystroke.
    #[allow(dead_code)]
    pub render_metrics: Option<PdfFontMetrics>,
    /// Block origin x (PDF user-space) — caret x is added to this.
    pub x: f64,
    /// Block baseline y (PDF user-space).
    pub y: f64,
    /// Font size in points.
    pub font_size: f64,
    /// Text→page horizontal scale at the block (from `tm·ctm`). Caret/selection
    /// x and width are measured in text space, so they're multiplied by this to
    /// report page-space values consistent with the block's `x`/`width`.
    pub scale_x: f64,
    /// Source frame ids (for the host's commit/render calls).
    pub frame_ids: Vec<usize>,
    /// Substitute fonts embedded for the live preview, keyed by
    /// `(base_family, bold, italic)` → (page `/EdN` key, embedded program). Lets a
    /// bold/italic/family run render in its real face *live*: embedded once on the
    /// format action (then the preview doc is rebuilt so it resolves), reused by
    /// every subsequent render. Stays empty without the `render` feature.
    pub preview_fonts: std::collections::HashMap<
        (String, bool, bool),
        (String, crate::writer::font_subset::EmbeddedCidFont),
    >,
    /// Font family name of the block's original font — used to resolve
    /// `FontChoice::Original` bold/italic runs when looking up `preview_metrics`.
    pub block_font_name: String,
    /// Human-facing font name for the picker (subset prefix stripped).
    pub display_font: String,
    /// The block font's *intrinsic* bold/italic (from the PDF FontDescriptor). A
    /// run keeps the original embedded glyphs when its requested bold/italic equals
    /// these; only a differing style triggers a bundled-face substitution.
    pub orig_bold: bool,
    pub orig_italic: bool,
    /// Per-`(family,bold,italic)` glyph metrics built from the embedded TTF advance
    /// widths, populated alongside `preview_fonts`. Used by `styled_offsets` and
    /// `run_metrics` so caret x, selection x, live width, and tile crop all reflect
    /// the actual bold/italic glyph widths rather than the regular font's widths.
    pub preview_metrics: std::collections::HashMap<(String, bool, bool), PdfFontMetrics>,
}

#[wasm_bindgen]
impl WasmEditor {
    /// Enter Word-style text editing for `page_index`.
    ///
    /// Builds the editable block model and returns a JSON array of blocks:
    /// `[{id,text,x,y,width,font_size,font_name,font_key,frame_ids:[..]}, ...]`.
    /// Coordinates are PDF user-space (origin bottom-left). Returns `"[]"` for
    /// scanned/image-only pages.
    pub fn text_edit_enter(&mut self, page_index: usize) -> Result<String, JsError> {
        log::debug!("[pdf-core] text_edit_enter page={}", page_index);

        // Editing a signed PDF invalidates its signature on save. Surface this
        // (the host can also gate on `is_signed()`); we warn rather than refuse
        // so the host keeps control of the UX.
        if self.editor.doc.is_signed() {
            log::warn!(
                "[pdf-core] text_edit_enter on a SIGNED document (page={}): committing \
                 edits will invalidate the digital signature",
                page_index
            );
        }

        // Reuse the already-built model when re-entering the same page with no new
        // edits since (e.g. opening several blocks on one page, or the per-open
        // `reenterForPage`). Rebuilding decodes the page + inverts every font's
        // CMap, which scales with page content — costly on heavier later pages.
        let generation = self.editor.writer.generation();
        if page_index == self.text_edit_page
            && self.text_edit_model_generation == generation
            && self.text_edit_model.is_some()
        {
            // Do NOT clear active_text_edit — text_edit_open resets it when the host
            // opens a specific block; clearing here races with fire-and-forget commits.
            // Return the live block list (kept up-to-date by text_edit_commit) so
            // the FE sees committed text/deletions without a full model rebuild.
            log::warn!(
                "[text_edit_enter] FAST-PATH page={} gen={} blocks={} texts=[{}]",
                page_index,
                generation,
                self.text_edit_blocks.len(),
                self.text_edit_blocks
                    .iter()
                    .map(|b| format!("{}:{:?}", b.id, b.text))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(blocks_to_json(&self.text_edit_blocks));
        }
        log::warn!(
            "[text_edit_enter] FULL-REBUILD page={} gen={} prev_page={} prev_gen={}",
            page_index,
            generation,
            self.text_edit_page,
            self.text_edit_model_generation
        );

        // Build the editable model from the *current* document state. After a
        // surgical commit the edit lives in the editor's writer pool (CoW), not in
        // the pristine `editor.doc` — so reading `doc` directly would resurrect the
        // original text. When edits are pending, serialize + reparse once per
        // writer-pool change (cached) so the model reflects exactly the bytes the
        // renderer sees.
        if self.editor.writer.is_empty() {
            self.edit_model_doc = None;
        } else {
            let fresh = !matches!(&self.edit_model_doc, Some((n, _)) if *n == generation);
            if fresh {
                // Build the committed doc view via clone + permanent overrides —
                // avoids save_append serialisation and PdfDocument::parse re-parsing.
                let doc = self.editor.doc.clone();
                let overrides: std::collections::HashMap<u32, crate::parser::objects::PdfObject> =
                    self.editor
                        .writer
                        .all_ids()
                        .into_iter()
                        .filter_map(|id| self.editor.writer.get_object(id).map(|o| (id, o.clone())))
                        .collect();
                doc.set_overrides(overrides);
                // Re-insert uncompressed bytes so CMap/text-model builds skip decompress.
                for (id, bytes) in &self.committed_bytes {
                    doc.preload_stream(*id, bytes);
                }
                log::debug!(
                    "[pdf-core] text_edit_enter rebuilt edit-model doc via clone (generation={})",
                    generation
                );
                self.edit_model_doc = Some((generation, doc));
            }
        }

        let model = build_text_model(self.text_edit_doc(), page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let json = blocks_to_json(&model.blocks);
        self.text_edit_blocks = model.blocks.clone();
        // Block ids are renumbered by the rebuild and the decoration stream is now
        // merged into the model's stream0, so the per-block style cache (keyed by
        // the OLD ids) is both stale and unnecessary — drop it.
        self.committed_style_runs.clear();
        self.text_edit_page = page_index;
        self.active_text_edit = None;
        // Retain the full model (blocks + parsed streams) for real-renderer previews.
        self.text_edit_model = Some(model);
        self.text_edit_model_generation = generation;
        Ok(json)
    }

    /// The document the text-edit model is read from: the reparsed post-edit doc
    /// when edits are pending (so committed text is visible), else the pristine
    /// `editor.doc`. Keeps the block model, metrics, encoding and preview render
    /// all reading the same document.
    pub(crate) fn text_edit_doc(&self) -> &crate::parser::objects::PdfDocument {
        match &self.edit_model_doc {
            Some((_, doc)) => doc,
            None => &self.editor.doc,
        }
    }

    /// Open block `block_id` for caret editing. Returns `false` if not found.
    pub fn text_edit_open(&mut self, block_id: usize) -> bool {
        let Some(block) = self.text_edit_blocks.iter().find(|b| b.id == block_id) else {
            log::warn!(
                "[text_edit_open] block_id={} NOT FOUND in text_edit_blocks (len={})",
                block_id,
                self.text_edit_blocks.len()
            );
            return false;
        };
        log::warn!(
            "[text_edit_open] block_id={} found text={:?} font_key={}",
            block_id,
            block.text,
            block.font_key
        );
        let render_metrics = font_metrics_for(
            self.text_edit_doc(),
            self.text_edit_page,
            &block.font_key,
            block.font_size,
        )
        .ok()
        .flatten();
        // Caret geometry needs a concrete metric; fall back to a proportional
        // estimate when the font can't be resolved.
        let metrics = render_metrics
            .clone()
            .unwrap_or_else(|| PdfFontMetrics::fallback(block.font_size));
        // Seed with effective bold/italic (intrinsic OR detected synthetic) so the
        // panel reflects the committed style on re-open. orig_bold/italic stay
        // intrinsic-only so run_synthetic_style knows what is already in the glyphs.
        let effective_bold = block.bold || block.synthetic_bold;
        let effective_italic = block.italic || block.synthetic_italic;
        let mut seed =
            CharStyle::from_block_styled(block.font_size, effective_bold, effective_italic);
        // Seed underline/strike from the decoration-rect model (survives cross-session).
        seed.underline = block.underline;
        seed.strike = block.strike;
        log::debug!(
            "[text_edit_open] id={} display_font={:?} size={:.3} scale_x={:.3} seed_bold={} seed_italic={} syn_bold={} syn_italic={} ul={} strike={}",
            block_id,
            block.display_font,
            block.font_size,
            block.scale_x,
            effective_bold,
            effective_italic,
            block.synthetic_bold,
            block.synthetic_italic,
            seed.underline,
            seed.strike,
        );
        self.active_text_edit = Some(ActiveTextEdit {
            block_id,
            engine: TextEditEngine::new_styled(&block.text, seed),
            metrics,
            render_metrics,
            x: block.x,
            y: block.y,
            font_size: block.font_size,
            scale_x: block.scale_x,
            frame_ids: block.frame_ids.clone(),
            preview_fonts: std::collections::HashMap::new(),
            block_font_name: block.font_name.clone(),
            display_font: block.display_font.clone(),
            orig_bold: block.bold,
            orig_italic: block.italic,
            preview_metrics: std::collections::HashMap::new(),
        });
        // Restore committed formatting (underline/strike) captured at the last
        // commit of this block, so a same-session reopen previews the decoration
        // live. Only applies when char count matches (the runs were captured for
        // the committed text); a mismatch means the text changed underneath, so
        // we fall back to the seed style rather than misapply runs.
        if let Some(runs) = self.committed_style_runs.get(&block_id).cloned() {
            if let Some(a) = self.active_text_edit.as_mut() {
                let buf_len = a.engine.text().chars().count();
                let runs_cover = runs.last().map(|r| r.end).unwrap_or(0);
                if runs_cover == buf_len {
                    a.engine.apply_style_runs(&runs);
                } else {
                    log::warn!(
                        "[text_edit_open] skip style restore block_id={} runs_cover={} buf_len={}",
                        block_id,
                        runs_cover,
                        buf_len
                    );
                }
            }
        }
        true
    }

    /// Insert text at the caret (also used for IME composition results).
    pub fn text_edit_insert(&mut self, s: &str) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.insert(s);
        }
    }

    /// Delete the selection, or the character before the caret.
    pub fn text_edit_backspace(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.delete_back();
        }
    }

    /// Delete the selection, or the character after the caret.
    pub fn text_edit_delete_forward(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.delete_forward();
        }
    }

    /// Move the caret left (`left=true`) or right, extending the selection when
    /// `extend` is set (Shift+Arrow).
    pub fn text_edit_move(&mut self, left: bool, extend: bool) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine
                .move_caret(if left { Dir::Left } else { Dir::Right }, extend);
        }
    }

    /// Move the caret to the start of the line.
    pub fn text_edit_home(&mut self, extend: bool) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.home(extend);
        }
    }

    /// Move the caret to the end of the line.
    pub fn text_edit_end(&mut self, extend: bool) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.end(extend);
        }
    }

    /// Place the caret nearest to local x (page-space points from the block
    /// origin). Converted back to text space (÷ scale_x) for metric hit-testing.
    pub fn text_edit_click(&mut self, x_pts: f64, extend: bool) {
        if let Some(a) = self.active_text_edit.as_mut() {
            let text_x = if a.scale_x.abs() > 1e-6 {
                x_pts / a.scale_x
            } else {
                x_pts
            };
            a.engine.click(&a.metrics, text_x, extend);
        }
    }

    /// Select the entire block.
    pub fn text_edit_select_all(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.select_all();
        }
    }

    /// Delete all text in the open block atomically (no intermediate select state).
    pub fn text_edit_delete_all(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.delete_all();
        }
    }

    /// Select the word under page-space x `x_pts` (from the block origin) — the
    /// double-click gesture. Converts to text space (÷ scale_x), hit-tests to a
    /// character index, then selects the word run there.
    pub fn text_edit_select_word(&mut self, x_pts: f64) {
        if let Some(a) = self.active_text_edit.as_mut() {
            let text_x = if a.scale_x.abs() > 1e-6 {
                x_pts / a.scale_x
            } else {
                x_pts
            };
            let text = a.engine.text();
            let idx = crate::editor::hit_test(&a.metrics, &text, text_x);
            a.engine.select_word_at(idx);
        }
    }

    // ── Formatting: apply to the current selection (or pending typing style) ────

    /// Apply a fill colour (`r`/`g`/`b` in 0.0–1.0) to the selection.
    pub fn text_edit_apply_color(&mut self, r: f64, g: f64, b: f64) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.apply_color([r, g, b]);
        }
    }

    /// Set the font family (e.g. `"Helvetica"`) for the selection.
    pub fn text_edit_set_font(&mut self, family: &str) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.set_font(family);
        }
        self.prepare_preview_fonts();
    }

    /// Set the font size for the selection. `size` is a **visual** point size from
    /// the panel; convert it back to text space (÷ the vertical text→page scale,
    /// proxied by `scale_x`) so it round-trips with the visual size in the state.
    pub fn text_edit_set_size(&mut self, size: f64) {
        if let Some(a) = self.active_text_edit.as_mut() {
            let text_space = if a.scale_x.abs() > 1e-6 {
                size / a.scale_x
            } else {
                size
            };
            a.engine.set_size(text_space);
        }
    }

    /// Toggle bold for the selection (Word semantics).
    pub fn text_edit_toggle_bold(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.toggle_bold();
        }
        self.prepare_preview_fonts();
    }

    /// Toggle italic for the selection.
    pub fn text_edit_toggle_italic(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.toggle_italic();
        }
        self.prepare_preview_fonts();
    }

    /// Toggle underline for the selection.
    pub fn text_edit_toggle_underline(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.toggle_underline();
        }
        self.prepare_preview_fonts();
    }

    /// Toggle strikethrough for the selection.
    pub fn text_edit_toggle_strike(&mut self) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.toggle_strike();
        }
        self.prepare_preview_fonts();
    }

    /// Set block alignment (`"left"`/`"center"`/`"right"`).
    pub fn text_edit_set_align(&mut self, align: &str) {
        if let Some(a) = self.active_text_edit.as_mut() {
            a.engine.set_align(Align::parse(align));
        }
    }

    /// Current edit state as JSON:
    /// `{block_id,block_font_name,text,caret,caret_x,sel_start,sel_end,sel_start_x,sel_end_x,x,y,font_size,width,style}`.
    /// `caret_x`/`width` are in user-space points from the block origin;
    /// `sel_start`/`sel_end` are `null` when there is no selection.
    pub fn text_edit_state(&self) -> String {
        let Some(a) = self.active_text_edit.as_ref() else {
            return "null".to_owned();
        };
        let text = a.engine.text();
        // Advances are text-space; multiply by scale_x to report page-space x
        // consistent with the block's `x`/`width` (e.g. a 0.75 page `cm`). Guard a
        // degenerate scale (≈0, e.g. a malformed CTM): multiplying by ~0 would
        // collapse the caret + selection x to 0 and hide a real highlight, so fall
        // back to the raw text-space advance (1:1) instead.
        let sx = if a.scale_x.abs() > 1e-6 {
            a.scale_x
        } else {
            1.0
        };
        // Use per-run metrics so bold/italic runs are measured with their embedded
        // face's advance widths rather than the regular font's (fixes box flex +
        // caret/selection placement inside formatted runs).
        let offsets = styled_offsets(
            &a.engine,
            &a.metrics,
            &a.preview_metrics,
            &a.block_font_name,
            a.font_size,
            a.orig_bold,
            a.orig_italic,
        );
        let caret_x = offsets.get(a.engine.caret()).copied().unwrap_or(0.0) * sx;
        let width = offsets.last().copied().unwrap_or(0.0) * sx;
        let (sel_start, sel_end) = match a.engine.selection() {
            Some((s, e)) => (s.to_string(), e.to_string()),
            None => ("null".to_owned(), "null".to_owned()),
        };
        // Selection x-bounds (page-space points from block origin) so the host can
        // draw the highlight without needing font metrics — Rust owns geometry.
        let (sel_start_x, sel_end_x) = match a.engine.selection() {
            Some((s, e)) => ((offsets[s] * sx).to_string(), (offsets[e] * sx).to_string()),
            None => ("null".to_owned(), "null".to_owned()),
        };
        // Vertical extent for the host's flex-height box: sized from the LARGEST run
        // font (so a bigger run grows the box) and matching the render tile's
        // ascent/descent factors. `descent` (0.30·fs) already covers the underline
        // depth (underline_offset 0.12·fs + thickness 0.05·fs ≈ 0.17·fs). Sizes are
        // text-space; ×sx gives the visual page-space size (text matrices are ~uniform).
        let max_fs = a
            .engine
            .style_runs()
            .iter()
            .map(|r| r.style.font_size)
            .fold(a.font_size, f64::max);
        let ascent = max_fs * 0.85 * sx;
        let descent = max_fs * 0.30 * sx;
        // Visual point size for the panel: text-space `font_size` × the vertical
        // text→page scale (≈ sx for the uniform matrices text uses). Lets the picker
        // show e.g. 24 even when the PDF uses `1 Tf` under a 24× text matrix.
        let visual_size = a.font_size * sx;
        log::debug!(
            "[text_edit_state] id={} font_size(text)={:.3} sx={:.3} visual_size={:.3} ascent={:.2} descent={:.2} display_font={:?}",
            a.block_id,
            a.font_size,
            sx,
            visual_size,
            ascent,
            descent,
            a.display_font,
        );
        let style = style_to_json(&a.engine.active_style(), sx);
        format!(
            r#"{{"block_id":{},"block_font_name":{},"text":{},"caret":{},"caret_x":{},"sel_start":{},"sel_end":{},"sel_start_x":{},"sel_end_x":{},"x":{},"y":{},"font_size":{},"width":{},"ascent":{},"descent":{},"style":{}}}"#,
            a.block_id,
            json_str(&a.display_font),
            json_str(&text),
            a.engine.caret(),
            caret_x,
            sel_start,
            sel_end,
            sel_start_x,
            sel_end_x,
            a.x,
            a.y,
            visual_size,
            width,
            ascent,
            descent,
            style,
        )
    }

    /// Final text of the open block (for the host's commit call).
    pub fn text_edit_text(&self) -> String {
        self.active_text_edit
            .as_ref()
            .map(|a| a.engine.text())
            .unwrap_or_default()
    }

    /// Frame ids of the open block as a JSON array string (e.g. `"[3,4]"`).
    ///
    /// Returned as a JSON string for consistency with `text_edit_enter` /
    /// `text_edit_state` (the host already parses those); the caller uses these
    /// ids for commit / render calls.
    pub fn text_edit_frame_ids(&self) -> String {
        let ids = self
            .active_text_edit
            .as_ref()
            .map(|a| {
                a.frame_ids
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        format!("[{}]", ids)
    }

    /// Commit the open block's edited text into the page content stream.
    ///
    /// Re-encodes the engine's current text into the block's font codes and, if
    /// every character is encodable, surgically replaces the block's show
    /// operator (only that op changes). Returns JSON:
    /// `{"committed":bool,"missing":"…"}` — `committed:false` with the missing
    /// characters when a typed glyph isn't in the font (caller falls back / waits
    /// for the font-embed tiers). On success the host should call `save()`.
    pub fn text_edit_commit(&mut self, block_id: usize) -> Result<String, JsError> {
        use crate::document::catalog::Catalog;
        use crate::document::page::Page;

        log::debug!(
            "[text_edit_commit] block_id={} active_session={:?} page={}",
            block_id,
            self.active_text_edit.as_ref().map(|a| a.block_id),
            self.text_edit_page
        );

        // Pull what we need from the active session, then drop that borrow.
        let (text, font_key, font_name, font_size) = {
            let a = match self.active_text_edit.as_ref() {
                Some(a) if a.block_id == block_id => a,
                Some(a) => {
                    log::warn!(
                        "[text_edit_commit] active session block_id={} ≠ requested block_id={} \
                         → committed:false",
                        a.block_id,
                        block_id
                    );
                    return Ok(r#"{"committed":false,"missing":""}"#.to_owned());
                }
                None => {
                    log::warn!(
                        "[text_edit_commit] NO active session for block_id={} \
                         → committed:false (text_edit_open was not called?)",
                        block_id
                    );
                    return Ok(r#"{"committed":false,"missing":""}"#.to_owned());
                }
            };
            let block = self
                .text_edit_blocks
                .iter()
                .find(|b| b.id == block_id)
                .ok_or_else(|| JsError::new("commit: block not found"))?;
            (
                a.engine.text(),
                block.font_key.clone(),
                block.font_name.clone(),
                block.font_size,
            )
        };

        let page_index = self.text_edit_page;

        // Record an undo checkpoint before any write-back path mutates the
        // writer pool. One checkpoint per commit covers every sub-path below
        // (multi-run, surgical, Tier-3 embed); `undo()` rolls all of them back.
        self.editor.checkpoint();

        // Rich-text path: when the block carries any formatting — more than one
        // style run, a non-Left alignment, or a single run whose style differs
        // from the block default — write it back through the multi-run commit.
        // On `Ok(false)` (a run's font/glyph couldn't be resolved) fall through
        // to the plain/Tier-3 path below, which reports the missing characters.
        let is_formatted = self
            .active_text_edit
            .as_ref()
            .filter(|a| a.block_id == block_id)
            .map(|a| {
                let runs = a.engine.style_runs();
                let plain = a.engine.align() == Align::Left
                    && runs.len() <= 1
                    && runs
                        .first()
                        .map(|r| r.style == CharStyle::from_block(font_size))
                        .unwrap_or(true);
                !plain
            })
            .unwrap_or(false);
        log::warn!(
            "[text_edit_commit] block_id={} text={:?} is_formatted={}",
            block_id,
            text,
            is_formatted
        );
        if is_formatted {
            match self.commit_block_runs_impl(page_index, block_id, font_size) {
                Ok(true) => {
                    self.flush_and_cache(page_index)?;
                    // Refresh the preview doc so immediate renders read the
                    // committed text + decoration (with the now-cached deco bytes
                    // preloaded). Do NOT advance text_edit_model_generation: a
                    // multi-run commit changes op counts, so the next
                    // text_edit_enter must FULL-REBUILD (renumber blocks).
                    self.rebuild_edit_model_doc()?;
                    if let Some(b) = self.text_edit_blocks.iter_mut().find(|b| b.id == block_id) {
                        b.text = text.clone();
                    }
                    log::warn!(
                        "[text_edit_commit] multi-run OK block_id={} text={:?}",
                        block_id,
                        text
                    );
                    return Ok(r#"{"committed":true,"missing":""}"#.to_owned());
                }
                Ok(false) => {
                    log::warn!(
                        "[text_edit_commit] multi-run could not resolve run → plain fallback block_id={}",
                        block_id
                    );
                }
                Err(e) => return Err(e),
            }
        }

        let doc = self.text_edit_doc();
        let catalog = Catalog::from_document(doc).map_err(|e| JsError::new(&e.to_string()))?;
        let page_dict = catalog
            .get_page_dict(doc, page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let page = Page::from_dict(doc, &page_dict).map_err(|e| JsError::new(&e.to_string()))?;

        log::debug!(
            "[text_edit_commit] block_id={} text={:?} font_key={} font_size={}",
            block_id,
            text,
            font_key,
            font_size
        );

        let enc = crate::editor::encode_in_font(doc, &page, &font_key, font_size, &text);
        log::debug!(
            "[text_edit_commit] encode_in_font complete={} missing={:?}",
            enc.is_complete(),
            enc.missing.iter().collect::<String>()
        );

        // Happy path: every char encodes in the block's own font → surgical commit.
        if enc.is_complete() {
            {
                let model = self
                    .text_edit_model
                    .as_mut()
                    .ok_or_else(|| JsError::new("commit: no active model"))?;
                crate::editor::commit_block(
                    &mut self.editor,
                    model,
                    page_index,
                    block_id,
                    &enc.bytes,
                )
                .map_err(|e| JsError::new(&e.to_string()))?;
            }
            // Flush to the writer pool now (collapses array /Contents → one stream)
            // then cache, so the edit persists and survives the next rebuild.
            self.flush_and_cache(page_index)?;
            if let Some(b) = self.text_edit_blocks.iter_mut().find(|b| b.id == block_id) {
                b.text = text.clone();
            }
            // Tier-1 patches ops IN PLACE (op count unchanged), so the retained
            // session's block/op indices stay valid. Keep the same model and mark
            // it current at the post-flush generation so the next `text_edit_enter`
            // FAST-PATHs (returns the kept-updated `text_edit_blocks` with STABLE
            // ids) instead of rebuilding + renumbering — which is what desynced the
            // FE's `selectedRustId` and silently dropped subsequent commits.
            self.keep_model_current(page_index)?;
            log::warn!(
                "[text_edit_commit] Tier-1 OK block_id={} text={:?} enc_bytes={} (model kept, gen={})",
                block_id,
                text,
                enc.bytes.len(),
                self.text_edit_model_generation
            );
            return Ok(r#"{"committed":true,"missing":""}"#.to_owned());
        }

        // Tier 3: a typed glyph isn't in the block's font. Embed a bundled font
        // (matched on the block's BaseFont) that covers the WHOLE block text and
        // retarget the block to it. Done only under the `render` feature, which is
        // where the bundled font resolver lives.
        log::warn!(
            "[text_edit_commit] Tier-1 incomplete → Tier-3 fallback block_id={} font_name={}",
            block_id,
            font_name
        );
        match self.commit_block_embed_fallback(page_index, block_id, &font_name, &text) {
            Ok(true) => {
                self.flush_and_cache(page_index)?;
                if let Some(b) = self.text_edit_blocks.iter_mut().find(|b| b.id == block_id) {
                    b.text = text.clone();
                }
                log::warn!(
                    "[text_edit_commit] Tier-3 OK block_id={} text={:?}",
                    block_id,
                    text
                );
                Ok(r#"{"committed":true,"missing":""}"#.to_owned())
            }
            _ => {
                let missing: String = enc.missing.iter().collect();
                log::debug!(
                    "[text_edit_commit] tier3 embed failed block_id={} missing={:?} → committed:false",
                    block_id,
                    missing
                );
                Ok(format!(
                    r#"{{"committed":false,"missing":{}}}"#,
                    json_str(&missing)
                ))
            }
        }
    }

    /// Tier-3 embed fallback: resolve a bundled font matching the block's
    /// `base_font`, verify it covers the *whole* block `text`, embed it as a
    /// Type0/Identity-H font, and retarget the block to it. Returns `Ok(true)` on
    /// success, `Ok(false)` when no bundled font covers every character (caller
    /// reports the missing chars). Only meaningful with the `render` feature
    /// (bundled resolver); a no-op `Ok(false)` otherwise.
    #[cfg(feature = "render")]
    fn commit_block_embed_fallback(
        &mut self,
        page_index: usize,
        block_id: usize,
        base_font: &str,
        text: &str,
    ) -> Result<bool, JsError> {
        use crate::render::font_resolver::{
            normalize_font_name, EmbeddedFontResolver, FontResolver,
        };
        use crate::writer::font_subset::embed_cidfont_for_chars;

        let (_family, bold, italic) = normalize_font_name(base_font);
        let Some(font_bytes) = EmbeddedFontResolver.resolve(base_font, bold, italic) else {
            return Ok(false);
        };
        let chars: Vec<char> = text.chars().collect();

        let embedded = embed_cidfont_for_chars(&mut self.editor, &font_bytes, base_font, &chars)
            .map_err(|e| JsError::new(&e.to_string()))?;
        // Require the bundled font to cover everything; otherwise don't half-embed.
        if !chars
            .iter()
            .all(|&c| c.is_whitespace() || embedded.can_encode(c))
        {
            return Ok(false);
        }

        let model = self
            .text_edit_model
            .as_mut()
            .ok_or_else(|| JsError::new("commit: no active model"))?;
        crate::editor::commit_block_with_font(
            &mut self.editor,
            model,
            page_index,
            block_id,
            &embedded,
            text,
        )
        .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(true)
    }

    /// No-op embed fallback when the `render` feature (bundled fonts) is absent.
    #[cfg(not(feature = "render"))]
    fn commit_block_embed_fallback(
        &mut self,
        _page_index: usize,
        _block_id: usize,
        _base_font: &str,
        _text: &str,
    ) -> Result<bool, JsError> {
        Ok(false)
    }

    /// Multi-run write-back for a formatted block.
    ///
    /// Resolves each style run to a PDF font key + show bytes (original font via
    /// [`encode_in_font`](crate::editor::encode_in_font); a chosen family or
    /// bold/italic variant via a bundled embedded font), computes underline/
    /// strikethrough rectangles and the alignment origin shift, then commits
    /// through [`commit_block_runs`](crate::editor::commit_block_runs). Returns
    /// `Ok(false)` when a run's font/glyph can't be resolved (caller falls back).
    fn commit_block_runs_impl(
        &mut self,
        page_index: usize,
        block_id: usize,
        block_font_size: f64,
    ) -> Result<bool, JsError> {
        use crate::document::catalog::Catalog;
        use crate::document::page::Page;
        use crate::editor::{
            build_run_ops, commit_block_runs, decoration_thickness, encode_in_font,
            register_page_font, run_synthetic_style, strike_offset, underline_offset, DecoRect,
            FontChoice, ResolvedRun, RunLayout, SyntheticStyle,
        };
        use crate::writer::font_subset::{embed_cidfont_for_chars, EmbeddedCidFont};

        // 1. Snapshot the runs + geometry from the live session (immutable reads).
        struct RunInfo {
            text: String,
            size: f64,
            color: [f64; 3],
            underline: bool,
            strike: bool,
            font: FontChoice,
            bold: bool,
            italic: bool,
            /// Synthetic styling to fake on the original glyphs (bold/italic the
            /// embedded font lacks). Empty for `Family` runs (real bundled face).
            synthetic: SyntheticStyle,
        }
        let (
            run_infos,
            align,
            scale_x,
            block_x,
            block_y,
            block_width,
            block_font_key,
            block_font_name,
            block_tm,
            metrics,
            preview_metrics,
            orig_bold,
            orig_italic,
            block_synthetic_italic,
            block_synthetic_bold,
        ) = {
            let a = self
                .active_text_edit
                .as_ref()
                .filter(|a| a.block_id == block_id)
                .ok_or_else(|| JsError::new("commit_runs: no live session"))?;
            let block = self
                .text_edit_blocks
                .iter()
                .find(|b| b.id == block_id)
                .ok_or_else(|| JsError::new("commit_runs: block not found"))?;
            let chars: Vec<char> = a.engine.text().chars().collect();
            let infos: Vec<RunInfo> = a
                .engine
                .style_runs()
                .iter()
                .map(|r| RunInfo {
                    text: chars[r.start..r.end].iter().collect(),
                    size: r.style.font_size,
                    color: r.style.color,
                    underline: r.style.underline,
                    strike: r.style.strike,
                    font: r.style.font.clone(),
                    bold: r.style.bold,
                    italic: r.style.italic,
                    synthetic: run_synthetic_style(&r.style, a.orig_bold, a.orig_italic),
                })
                .collect();
            (
                infos,
                a.engine.align(),
                a.scale_x,
                a.x,
                a.y,
                block.width,
                block.font_key.clone(),
                block.font_name.clone(),
                block.tm,
                a.metrics.clone(),
                a.preview_metrics.clone(),
                a.orig_bold,
                a.orig_italic,
                block.synthetic_italic,
                block.synthetic_bold,
            )
        };

        // 2. Phase A — encode original-font runs against the immutable document.
        //    Runs that need a substitute font (chosen family / bold / italic) or
        //    whose glyphs are missing get queued for embedding.
        let mut resolved: Vec<Option<ResolvedRun>> = vec![None; run_infos.len()];
        let mut needs_embed: Vec<usize> = Vec::new();
        {
            let doc = self.text_edit_doc();
            let catalog = Catalog::from_document(doc).map_err(|e| JsError::new(&e.to_string()))?;
            let page_dict = catalog
                .get_page_dict(doc, page_index)
                .map_err(|e| JsError::new(&e.to_string()))?;
            let page =
                Page::from_dict(doc, &page_dict).map_err(|e| JsError::new(&e.to_string()))?;
            for (i, ri) in run_infos.iter().enumerate() {
                // An `Original`-font run ALWAYS keeps its embedded glyphs — a
                // bold/italic difference from the font's intrinsic style is faked
                // synthetically (stroke / shear), never swapped to a bundled face
                // (which would lose CJK/diacritics and "break" the font). Only a
                // chosen `Family` run needs a bundled substitute. A missing glyph
                // in the original font still falls through to embedding.
                let keep_original = matches!(ri.font, FontChoice::Original);
                log::debug!(
                    "[commit-run] i={} keep_original={} run(bold={},italic={}) orig(bold={},italic={}) synthetic={:?} text={:?}",
                    i,
                    keep_original,
                    ri.bold,
                    ri.italic,
                    orig_bold,
                    orig_italic,
                    ri.synthetic,
                    ri.text.chars().take(16).collect::<String>(),
                );
                if keep_original {
                    let enc = encode_in_font(doc, &page, &block_font_key, ri.size, &ri.text);
                    if enc.is_complete() {
                        resolved[i] = Some(ResolvedRun {
                            font_key: block_font_key.clone(),
                            font_size: ri.size,
                            color: ri.color,
                            bytes: enc.bytes,
                            synthetic: ri.synthetic,
                        });
                        continue;
                    }
                }
                needs_embed.push(i);
            }
        }

        // 3. Phase B — embed a bundled font per (family,bold,italic) covering the
        //    union of its runs' chars, then encode those runs against it.
        if !needs_embed.is_empty() {
            let mut union: std::collections::HashMap<(String, bool, bool), Vec<char>> =
                std::collections::HashMap::new();
            for &i in &needs_embed {
                let ri = &run_infos[i];
                let base = match &ri.font {
                    FontChoice::Family(f) => f.clone(),
                    FontChoice::Original => block_font_name.clone(),
                };
                union
                    .entry((base, ri.bold, ri.italic))
                    .or_default()
                    .extend(ri.text.chars());
            }

            let mut fonts: std::collections::HashMap<
                (String, bool, bool),
                (String, EmbeddedCidFont),
            > = std::collections::HashMap::new();
            for (key, chars) in &union {
                let synthetic = synthetic_font_name(&key.0, key.1, key.2);
                let Some(bytes) = self.resolve_run_font_bytes(&synthetic) else {
                    return Ok(false); // no bundled face (or no `render` feature)
                };
                let embedded = embed_cidfont_for_chars(&mut self.editor, &bytes, &synthetic, chars)
                    .map_err(|e| JsError::new(&e.to_string()))?;
                if !chars
                    .iter()
                    .all(|&c| c.is_whitespace() || embedded.can_encode(c))
                {
                    return Ok(false); // bundled face doesn't cover every glyph
                }
                let fkey = register_page_font(&mut self.editor, page_index, embedded.font_id)
                    .map_err(|e| JsError::new(&e.to_string()))?;
                fonts.insert(key.clone(), (fkey, embedded));
            }

            for &i in &needs_embed {
                let ri = &run_infos[i];
                let base = match &ri.font {
                    FontChoice::Family(f) => f.clone(),
                    FontChoice::Original => block_font_name.clone(),
                };
                let (fkey, embedded) = fonts
                    .get(&(base, ri.bold, ri.italic))
                    .ok_or_else(|| JsError::new("commit_runs: embed cache miss"))?;
                resolved[i] = Some(ResolvedRun {
                    font_key: fkey.clone(),
                    font_size: ri.size,
                    color: ri.color,
                    bytes: embedded.encode(&ri.text),
                    // A real bundled face carries bold/italic in the glyphs — no
                    // synthetic styling needed.
                    synthetic: SyntheticStyle::default(),
                });
            }
        }

        let resolved: Vec<ResolvedRun> = resolved
            .into_iter()
            .map(|o| o.ok_or_else(|| JsError::new("commit_runs: unresolved run")))
            .collect::<Result<_, _>>()?;

        // 4. Widths (text space, scaled by per-run size), alignment, decorations.
        // Use per-run metrics so bold/italic runs are measured with their embedded
        // face's advance widths, giving correct alignment and decoration geometry.
        let bfs = if block_font_size > 0.0 {
            block_font_size
        } else {
            1.0
        };
        let run_w_text: Vec<f64> = run_infos
            .iter()
            .map(|ri| {
                let ri_style = crate::editor::CharStyle {
                    font: ri.font.clone(),
                    bold: ri.bold,
                    italic: ri.italic,
                    ..crate::editor::CharStyle::from_block(ri.size)
                };
                let m = run_metrics(
                    &ri_style,
                    &metrics,
                    &preview_metrics,
                    &block_font_name,
                    orig_bold,
                    orig_italic,
                );
                text_width(m, &ri.text) * (ri.size / bfs)
            })
            .collect();
        let total_w_page: f64 = run_w_text.iter().sum::<f64>() * scale_x;
        let mut align_dx_page = match align {
            Align::Left => 0.0,
            Align::Center => (block_width - total_w_page) / 2.0,
            Align::Right => block_width - total_w_page,
        };
        if block_x + align_dx_page < 0.0 {
            log::warn!("[commit_runs] alignment shift clamped (origin would go off-page)");
            align_dx_page = -block_x;
        }
        let align_dx_text = if scale_x.abs() > 1e-6 {
            align_dx_page / scale_x
        } else {
            0.0
        };

        let mut decorations: Vec<DecoRect> = Vec::new();
        // Per-run starting text-space x (for the synthetic-italic `Tm` path).
        let mut run_x_text: Vec<f64> = Vec::with_capacity(run_infos.len());
        let mut cursor_text = 0.0_f64;
        for (i, ri) in run_infos.iter().enumerate() {
            run_x_text.push(cursor_text);
            let x0 = block_x + align_dx_page + cursor_text * scale_x;
            let w = run_w_text[i] * scale_x;
            let thick = decoration_thickness(ri.size);
            if ri.underline {
                decorations.push(DecoRect {
                    x: x0,
                    y: block_y - underline_offset(ri.size),
                    width: w,
                    height: thick,
                    color: ri.color,
                });
            }
            if ri.strike {
                decorations.push(DecoRect {
                    x: x0,
                    y: block_y + strike_offset(ri.size),
                    width: w,
                    height: thick,
                    color: ri.color,
                });
            }
            cursor_text += run_w_text[i];
        }

        // 5. Build the operator sequence and commit (font Resources already set).
        // The layout lets `build_run_ops` place synthetic-italic runs with an
        // absolute `Tm` (carrying the shear) derived from the block's text matrix.
        let layout = RunLayout {
            tm: [
                block_tm.a, block_tm.b, block_tm.c, block_tm.d, block_tm.e, block_tm.f,
            ],
            run_x_text,
            force_positioned: block_synthetic_italic,
            reset_stroke: block_synthetic_bold,
        };
        let run_ops = build_run_ops(&resolved, align_dx_text, Some(&layout));
        let model = self
            .text_edit_model
            .as_mut()
            .ok_or_else(|| JsError::new("commit_runs: no active model"))?;
        commit_block_runs(&mut self.editor, model, page_index, block_id, &run_ops)
            .map_err(|e| JsError::new(&e.to_string()))?;
        // Stash decorations for flush_and_cache to draw AFTER commit_edit_session
        // rewrites /Contents to a single reference. Drawing them here would add
        // them to an appended layer that the single-ref rewrite then clobbers.
        self.pending_decorations = Some((block_id, decorations));
        // Capture the committed run styles so a same-session reopen of this block
        // restores its decoration state (the deco stream isn't in the model until
        // a FULL-REBUILD). Collect into an owned Vec first to avoid holding an
        // immutable borrow of `active_text_edit` across the mutable map insert.
        let committed_runs = self
            .active_text_edit
            .as_ref()
            .filter(|a| a.block_id == block_id)
            .map(|a| a.engine.style_runs());
        if let Some(runs) = committed_runs {
            self.committed_style_runs.insert(block_id, runs);
        }
        Ok(true)
    }

    /// Resolve bundled font bytes for a synthetic name (family + bold/italic
    /// encoded into the name, which the resolver re-parses). `None` when no
    /// bundled face matches — including when the `render` feature is off.
    #[cfg(feature = "render")]
    fn resolve_run_font_bytes(&self, synthetic_name: &str) -> Option<Vec<u8>> {
        use crate::render::font_resolver::{EmbeddedFontResolver, FontResolver};
        EmbeddedFontResolver.resolve(synthetic_name, false, false)
    }

    /// No bundled fonts without the `render` feature.
    #[cfg(not(feature = "render"))]
    fn resolve_run_font_bytes(&self, _synthetic_name: &str) -> Option<Vec<u8>> {
        None
    }

    /// After a font-affecting format change (bold/italic/family): embed any newly
    /// needed substitute faces and, if any were added, rebuild the preview doc so
    /// the live render resolves them. Best-effort — silently no-ops without the
    /// `render` feature or when no bundled face matches.
    fn prepare_preview_fonts(&mut self) {
        match self.ensure_preview_fonts() {
            Ok(true) => {
                if self.rebuild_edit_model_doc().is_err() {
                    log::warn!("[preview-fonts] edit-model rebuild failed after embed");
                }
            }
            Ok(false) => {}
            Err(_) => log::warn!("[preview-fonts] ensure_preview_fonts failed"),
        }
    }

    /// Embed substitute fonts for the open block's bold/italic/family runs that
    /// aren't cached yet. Returns whether any new font was embedded (→ the caller
    /// rebuilds the preview doc). Each `(family,bold,italic)` is embedded once.
    fn ensure_preview_fonts(&mut self) -> Result<bool, JsError> {
        use crate::editor::FontChoice;

        let page_index = self.text_edit_page;
        // Collect (family,bold,italic) → union of chars for substitute runs.
        let needs: Vec<((String, bool, bool), Vec<char>)> = {
            let Some(a) = self.active_text_edit.as_ref() else {
                return Ok(false);
            };
            let chars: Vec<char> = a.engine.text().chars().collect();
            let mut union: std::collections::HashMap<(String, bool, bool), Vec<char>> =
                std::collections::HashMap::new();
            for r in a.engine.style_runs() {
                // Only a chosen `Family` run embeds a bundled preview face. An
                // `Original`-font run keeps its embedded glyphs and fakes any
                // bold/italic synthetically at draw time, so it needs no substitute.
                let FontChoice::Family(fam) = &r.style.font else {
                    continue;
                };
                union
                    .entry((fam.clone(), r.style.bold, r.style.italic))
                    .or_default()
                    .extend(chars[r.start..r.end].iter().copied());
            }
            union.into_iter().collect()
        };

        let mut changed = false;
        for (key, chars) in needs {
            let cached = self
                .active_text_edit
                .as_ref()
                .map(|a| a.preview_fonts.contains_key(&key))
                .unwrap_or(false);
            if cached {
                continue;
            }
            if self.embed_preview_font(page_index, &key.0, key.1, key.2, &chars)? {
                changed = true;
            }
        }
        Ok(changed)
    }

    /// Resolve + embed a bundled face for `(base,bold,italic)` covering `chars`,
    /// register it on the page, and cache it on the active session. Returns
    /// `Ok(true)` when embedded, `Ok(false)` when no bundled face covers it.
    #[cfg(feature = "render")]
    fn embed_preview_font(
        &mut self,
        page_index: usize,
        base: &str,
        bold: bool,
        italic: bool,
        chars: &[char],
    ) -> Result<bool, JsError> {
        use crate::editor::register_page_font;
        use crate::writer::font_subset::embed_cidfont_for_chars;

        let synthetic = synthetic_font_name(base, bold, italic);
        let Some(bytes) = self.resolve_run_font_bytes(&synthetic) else {
            return Ok(false);
        };
        let embedded = embed_cidfont_for_chars(&mut self.editor, &bytes, &synthetic, chars)
            .map_err(|e| JsError::new(&e.to_string()))?;
        if !chars
            .iter()
            .all(|&c| c.is_whitespace() || embedded.can_encode(c))
        {
            return Ok(false);
        }
        let fkey = register_page_font(&mut self.editor, page_index, embedded.font_id)
            .map_err(|e| JsError::new(&e.to_string()))?;
        if let Some(a) = self.active_text_edit.as_mut() {
            // Build width metrics from the embedded face so caret/box measurements
            // use bold/italic glyph advances rather than the regular font's widths.
            let preview_m =
                PdfFontMetrics::from_ttf_iter(embedded.iter_char_advances_1000(), a.font_size);
            a.preview_metrics
                .insert((base.to_owned(), bold, italic), preview_m);
            a.preview_fonts
                .insert((base.to_owned(), bold, italic), (fkey, embedded));
        }
        Ok(true)
    }

    /// No embedding without the `render` feature (no bundled font resolver).
    #[cfg(not(feature = "render"))]
    fn embed_preview_font(
        &mut self,
        _page_index: usize,
        _base: &str,
        _bold: bool,
        _italic: bool,
        _chars: &[char],
    ) -> Result<bool, JsError> {
        Ok(false)
    }

    /// Rebuild the post-edit preview document (`edit_model_doc`) using the
    /// clone + permanent-overrides strategy, avoiding save_append + re-parse.
    fn rebuild_edit_model_doc(&mut self) -> Result<(), JsError> {
        if self.editor.writer.is_empty() {
            self.edit_model_doc = None;
            return Ok(());
        }
        let generation = self.editor.writer.generation();
        let doc = self.editor.doc.clone();
        let overrides: std::collections::HashMap<u32, crate::parser::objects::PdfObject> = self
            .editor
            .writer
            .all_ids()
            .into_iter()
            .filter_map(|id| self.editor.writer.get_object(id).map(|o| (id, o.clone())))
            .collect();
        doc.set_overrides(overrides);
        for (id, bytes) in &self.committed_bytes {
            doc.preload_stream(*id, bytes);
        }
        self.edit_model_doc = Some((generation, doc));
        Ok(())
    }

    /// Close the active block without committing.
    pub fn text_edit_cancel(&mut self) {
        self.active_text_edit = None;
    }

    /// Flush all in-memory text edits to the writer pool and exit edit mode.
    ///
    /// Call when the user leaves text-editing mode (tool switch, page change,
    /// save). Serialises every dirty content stream into the writer pool
    /// (one flate-compressed object per changed stream) so `save()` /
    /// `save_append()` include the edits. After this call the in-memory model
    /// is cleared; the next `text_edit_enter` on the same page rebuilds from
    /// the updated document (one `PdfDocument::clone()` per session, not one
    /// per block commit).
    ///
    /// Any open block is silently cancelled — call `text_edit_commit` first if
    /// that block should be saved.
    ///
    /// Returns `"{}"` on success or `{"error":"..."}` on failure.
    pub fn text_edit_exit(&mut self) -> String {
        self.active_text_edit = None;

        if let Some(model) = &self.text_edit_model {
            log::warn!(
                "[text_edit_exit] session dirty={} page={} streams={}",
                model.session.dirty,
                self.text_edit_page,
                model.session.streams.len()
            );
            if model.session.dirty {
                log::warn!("[text_edit_exit] flushing to writer pool...");
                if let Err(e) = crate::editor::edit_session::commit_edit_session(
                    &mut self.editor,
                    self.text_edit_page,
                    &model.session,
                ) {
                    log::warn!("[text_edit_exit] commit_edit_session FAILED: {}", e);
                    return format!(r#"{{"error":{}}}"#, super::json_str(&e.to_string()));
                }
                log::warn!(
                    "[text_edit_exit] flush OK — writer gen={}",
                    self.editor.writer.generation()
                );
                // Refresh committed_bytes to the new stream IDs that
                // commit_edit_session just wrote to the pool, so the next
                // rebuild_edit_model_doc can preload them and skip flate-decompress.
                self.cache_committed_streams(self.text_edit_page);
            } else {
                log::warn!("[text_edit_exit] session NOT dirty — nothing to flush");
            }
        } else {
            log::warn!("[text_edit_exit] no active model — nothing to flush");
        }

        self.text_edit_model = None;
        self.text_edit_blocks = Vec::new();
        self.text_edit_model_generation = 0;
        self.edit_model_doc = None;
        self.committed_style_runs.clear();
        // committed_bytes kept intentionally: rebuild_edit_model_doc uses it on
        // the next text_edit_enter to preload stream bytes into the cloned doc.

        "{}".to_owned()
    }

    /// Flush the just-patched edit model to the writer pool **immediately**, then
    /// refresh the committed-bytes cache.
    ///
    /// Called from `text_edit_commit` after each successful commit. Flushing on
    /// commit (rather than deferring to `text_edit_exit`) is required for
    /// correctness with two facts about this app:
    /// 1. The page's `/Contents` is often a **multi-stream array**, so
    ///    `cache_committed_streams` cannot preload the patched concatenation at a
    ///    single id — only `commit_edit_session` (which collapses the array to one
    ///    stream) makes the edit visible.
    /// 2. The frontend calls `editor.save()` on **every** commit (undo history),
    ///    which serialises the writer pool; if the edit isn't flushed there first
    ///    it is lost, and the trial-watermark bump forces a `text_edit_enter`
    ///    rebuild that reads the un-edited document (the block reappears).
    ///
    /// After the flush `/Contents` is a single reference, so the follow-up
    /// `cache_committed_streams` succeeds and the next rebuild reads the edit.
    fn flush_and_cache(&mut self, page_index: usize) -> Result<(), JsError> {
        let dirty = self
            .text_edit_model
            .as_ref()
            .map(|m| m.session.dirty)
            .unwrap_or(false);
        if dirty {
            // Disjoint field borrows: &self.text_edit_model + &mut self.editor.
            if let Some(model) = &self.text_edit_model {
                crate::editor::edit_session::commit_edit_session(
                    &mut self.editor,
                    page_index,
                    &model.session,
                )
                .map_err(|e| JsError::new(&e.to_string()))?;
            }
            // Mark clean so a later text_edit_exit doesn't re-flush the same edit.
            if let Some(model) = self.text_edit_model.as_mut() {
                model.session.dirty = false;
            }
            // Draw decoration rects (underline/strike) NOW — after commit_edit_session
            // has rewritten /Contents to a single reference. begin_edit_page reads
            // that ref and appends a new stream → /Contents becomes an array
            // [stream0, decorations], which render_page concatenates correctly.
            // Drawing before the flush would leave the layer in a stream that the
            // single-ref rewrite silently clobbers.
            // Cache the text stream NOW, while /Contents is still a single
            // reference (commit_edit_session just collapsed it). Drawing the
            // decoration layer below turns /Contents into an array, which
            // cache_committed_streams cannot preload at a single id.
            self.cache_committed_streams(page_index);

            if self.pending_decorations.is_some() {
                let stash = self.pending_decorations.take();
                if let Some((committed_block_id, current_decos)) = stash {
                    self.rebuild_page_decorations(page_index, committed_block_id, &current_decos)
                        .map_err(|e| JsError::new(&e.to_string()))?;
                }
            }
        } else {
            // Not dirty (no text change) but a render may still need the cache.
            self.cache_committed_streams(page_index);
        }
        Ok(())
    }

    /// Keep the current edit session valid across a Tier-1 commit so the next
    /// `text_edit_enter` takes the FAST-PATH (stable block ids) instead of a
    /// FULL-REBUILD that renumbers blocks and desyncs the host's selection.
    ///
    /// Safe only for in-place (op-count-preserving) commits: it does NOT rebuild
    /// `text_edit_model` (whose `op_indices` would otherwise go stale), it only
    /// refreshes the cached preview document and advances the tracked generation
    /// to the post-flush value so the generation check matches on re-entry.
    fn keep_model_current(&mut self, _page_index: usize) -> Result<(), JsError> {
        // Refresh the post-edit preview doc so live render / metrics reflect the
        // just-flushed content (clone + writer-pool overrides + committed bytes).
        self.rebuild_edit_model_doc()?;
        // Mark the retained model as current at the new generation → fast-path.
        self.text_edit_model_generation = self.editor.writer.generation();
        Ok(())
    }

    /// After a successful text commit, cache the uncompressed serialised ops for
    /// every content stream in the session so that `render_page` and
    /// `render_committed_block_tile` can skip the flate-decompress step.
    ///
    /// After `set_overrides` clears `decoded_stream_cache` entries for overridden
    /// stream objects, the render path re-inserts these raw bytes — turning every
    /// `get_stream_data` call for a committed stream into a cache hit.
    pub(crate) fn cache_committed_streams(&mut self, page_index: usize) {
        use crate::editor::edit_session::OpStreamSource;
        use crate::editor::text_editor::serialize_operations;
        use crate::parser::objects::PdfObject;

        // Collect (source, raw_bytes) from the model first, then drop the
        // borrow of self.text_edit_model before mutating self.editor /
        // self.committed_bytes (split-field borrows via a temporary Vec).
        let pairs: Vec<(OpStreamSource, Vec<u8>)> = match self.text_edit_model.as_ref() {
            Some(model) => model
                .session
                .streams
                .iter()
                .map(|s| {
                    // PageContent must be wrapped in q/Q identically to
                    // commit_edit_session — this preload shadows that stream object
                    // by id, so unwrapped bytes here would make committed renders
                    // read content without the CTM reset and mis-place decorations.
                    // FormXObjects get no appended decoration layer, so leave them as-is.
                    let raw = match s.source {
                        OpStreamSource::PageContent => {
                            crate::editor::edit_session::wrap_page_content_bytes(&s.ops)
                        }
                        OpStreamSource::FormXObject(_) => serialize_operations(&s.ops),
                    };
                    (s.source.clone(), raw)
                })
                .collect(),
            None => return,
        };

        for (source, raw) in pairs {
            let stream_id = match source {
                OpStreamSource::PageContent => {
                    let Ok((page_id, _)) = self.editor.get_page_dict(page_index) else {
                        log::warn!(
                            "[cache_committed_streams] get_page_dict failed page={}",
                            page_index
                        );
                        continue;
                    };
                    // CoW lookup: works before flush (page dict in original doc,
                    // resolves the original stream ID) and after flush (page dict
                    // in pool, resolves the new stream ID written by
                    // commit_edit_session).
                    match self.editor.get_object(page_id) {
                        Ok(PdfObject::Dictionary(dict)) => match dict.get("Contents") {
                            Some(PdfObject::Reference(sid, _)) => {
                                log::warn!(
                                    "[cache_committed_streams] page_id={} Contents=Reference({}) — single stream, can preload",
                                    page_id, sid
                                );
                                *sid
                            }
                            Some(PdfObject::Array(items)) => {
                                // ARRAY /Contents: the session's streams[0] is the
                                // decoded concatenation of ALL these streams, so there
                                // is no single id to preload the patched blob at — this
                                // is why the deferred preview can't reflect the edit.
                                let ids: Vec<String> = items
                                    .iter()
                                    .map(|it| match it {
                                        PdfObject::Reference(id, _) => format!("{id} 0 R"),
                                        other => format!("{other:?}"),
                                    })
                                    .collect();
                                log::warn!(
                                    "[cache_committed_streams] page_id={} Contents=ARRAY[{}] — {} streams, CANNOT preload patched concat at a single id (page={}); edit only becomes visible after commit_edit_session collapses it to one stream",
                                    page_id,
                                    ids.join(", "),
                                    items.len(),
                                    page_index
                                );
                                continue;
                            }
                            other => {
                                log::warn!(
                                    "[cache_committed_streams] page_id={} Contents is neither Reference nor Array: {:?} (page={})",
                                    page_id, other, page_index
                                );
                                continue;
                            }
                        },
                        other => {
                            log::warn!(
                                "[cache_committed_streams] get_object(page_id={}) not a Dictionary (ok={}) page={}",
                                page_id,
                                other.is_ok(),
                                page_index
                            );
                            continue;
                        }
                    }
                }
                OpStreamSource::FormXObject(obj_num) => obj_num,
            };
            log::warn!(
                "[cache_committed_streams] preload stream_id={} bytes={} page={}",
                stream_id,
                raw.len(),
                page_index
            );
            self.committed_bytes.insert(stream_id, raw.clone());
            self.editor.doc.preload_stream(stream_id, &raw);
        }
    }

    /// Rebuild the page's single decoration stream covering ALL blocks.
    ///
    /// Collects decorations from every block in `text_edit_blocks`:
    /// - for `committed_block_id`, uses `current_decos` (the just-committed rects),
    /// - for every other block, uses its `block.decorations` (detected on open).
    ///
    /// Strategy:
    /// 1. Scan `/Contents` for the existing decoration stream (a stream whose ops
    ///    contain `re` or `f` but no `BT`/`Tj`/`TJ`). If found, replace it in-place.
    /// 2. If no existing deco stream and all_decos is non-empty, append a new one.
    /// 3. If all_decos is empty and a deco stream exists, drop it from `/Contents`.
    fn rebuild_page_decorations(
        &mut self,
        page_index: usize,
        committed_block_id: usize,
        current_decos: &[crate::editor::DecoRect],
    ) -> crate::error::Result<()> {
        use crate::content::operators::Operation;
        use crate::editor::build_decoration_ops;
        use crate::editor::text_editor::serialize_operations;
        use crate::parser::objects::{PdfDict, PdfObject};
        use crate::writer::streams::make_flate_stream;

        // Collect all decorations: current block's new rects, others' stored rects.
        let all_decos: Vec<crate::editor::DecoRect> = {
            let mut v: Vec<crate::editor::DecoRect> = Vec::new();
            for block in &self.text_edit_blocks {
                if block.id == committed_block_id {
                    v.extend_from_slice(current_decos);
                } else {
                    v.extend_from_slice(&block.decorations);
                }
            }
            v
        };

        let new_ops: Vec<Operation> = build_decoration_ops(&all_decos);
        let new_bytes = serialize_operations(&new_ops);

        let (page_id, page_dict) = self.editor.get_page_dict(page_index)?;

        // Collect current /Contents refs.
        let mut contents: Vec<PdfObject> = match page_dict.get("Contents") {
            Some(PdfObject::Array(arr)) => arr.clone(),
            Some(r @ PdfObject::Reference(_, _)) => vec![r.clone()],
            None => vec![],
            _ => vec![],
        };

        // Find the existing decoration stream (text-only stream: has re/f, no BT/Tj/TJ).
        let deco_pos = contents.iter().position(|item| {
            if let PdfObject::Reference(id, _) = item {
                if let Ok(PdfObject::Stream(st)) = self.editor.get_object(*id) {
                    let raw = &st.raw_data;
                    // Quick heuristic: look for re/f in bytes, absence of BT.
                    let has_re = raw.windows(3).any(|w| w == b" re");
                    let has_fill = raw.windows(2).any(|w| w == b" f" || w == b" F");
                    let has_bt = raw.windows(3).any(|w| w == b" BT");
                    return (has_re || has_fill) && !has_bt;
                }
            }
            false
        });

        if all_decos.is_empty() {
            // No decorations at all: drop the deco stream from /Contents if present.
            if let Some(pos) = deco_pos {
                contents.remove(pos);
            }
        } else if let Some(pos) = deco_pos {
            // Replace the existing decoration stream in-place.
            if let PdfObject::Reference(deco_id, _) = contents[pos] {
                let stream = make_flate_stream(&new_bytes, PdfDict::new())?;
                self.editor
                    .replace_object(deco_id, PdfObject::Stream(Box::new(stream)));
                // Cache the new bytes for preload.
                self.committed_bytes.insert(deco_id, new_bytes.clone());
                self.editor.doc.preload_stream(deco_id, &new_bytes);
            }
        } else {
            // Append a new decoration stream.
            let stream = make_flate_stream(&new_bytes, PdfDict::new())?;
            let deco_id = self.editor.add_object(PdfObject::Stream(Box::new(stream)));
            contents.push(PdfObject::Reference(deco_id, 0));
            // Cache for preload.
            self.committed_bytes.insert(deco_id, new_bytes.clone());
            self.editor.doc.preload_stream(deco_id, &new_bytes);
        }

        // Write the updated /Contents back.
        let mut updated_page = page_dict;
        updated_page.insert(
            "Contents".to_owned(),
            if contents.len() == 1 {
                contents.remove(0)
            } else {
                PdfObject::Array(contents)
            },
        );
        self.editor
            .replace_object(page_id, PdfObject::Dictionary(updated_page));

        Ok(())
    }
}

/// Build a PostScript-style font name that encodes bold/italic, so the font
/// resolver (which re-parses style from the name) picks the right face.
fn synthetic_font_name(family: &str, bold: bool, italic: bool) -> String {
    let suffix = match (bold, italic) {
        (true, true) => "-BoldItalic",
        (true, false) => "-Bold",
        (false, true) => "-Italic",
        (false, false) => "",
    };
    format!("{family}{suffix}")
}

/// Whether a run needs a bundled substitute face instead of the block's original
/// embedded font.
///
/// Keeps the original glyphs when the run uses the block's own font AND its
/// requested bold/italic equals the font's *intrinsic* style (`orig_bold`/
/// `orig_italic`, read from the PDF FontDescriptor). Only a chosen family or a
/// bold/italic that *differs* from the intrinsic style needs substitution — this
/// is what makes an already-bold title render with its own glyphs (no swap) yet
/// still respond visibly when you actually toggle the weight/slant.
fn run_needs_substitute(
    style: &crate::editor::CharStyle,
    orig_bold: bool,
    orig_italic: bool,
) -> bool {
    !matches!(style.font, crate::editor::FontChoice::Original)
        || style.bold != orig_bold
        || style.italic != orig_italic
}

/// Pick the correct [`PdfFontMetrics`] for a style run.
///
/// Returns the cached variant metrics when the run needs (and has) an embedded
/// substitute face; otherwise the block's original `default` metrics. Lets caret
/// x, selection x, live width, and tile crop reflect the actual glyph advances.
fn run_metrics<'a>(
    style: &crate::editor::CharStyle,
    default: &'a PdfFontMetrics,
    preview: &'a std::collections::HashMap<(String, bool, bool), PdfFontMetrics>,
    block_font_name: &str,
    orig_bold: bool,
    orig_italic: bool,
) -> &'a PdfFontMetrics {
    if !run_needs_substitute(style, orig_bold, orig_italic) {
        return default;
    }
    let base = match &style.font {
        crate::editor::FontChoice::Family(f) => f.as_str(),
        crate::editor::FontChoice::Original => block_font_name,
    };
    preview
        .get(&(base.to_owned(), style.bold, style.italic))
        .unwrap_or(default)
}

/// Compute per-character x-offsets (length `chars + 1`) accounting for per-run metrics.
///
/// Iterates `engine.style_runs()` and uses `run_metrics` for each run so that a
/// bold run is measured with the bold face's advances, not the regular font's.
/// Falls back to `default` metrics for any run whose variant hasn't been embedded yet.
#[allow(clippy::too_many_arguments)]
fn styled_offsets(
    engine: &crate::editor::TextEditEngine,
    default: &PdfFontMetrics,
    preview: &std::collections::HashMap<(String, bool, bool), PdfFontMetrics>,
    block_font_name: &str,
    block_font_size: f64,
    orig_bold: bool,
    orig_italic: bool,
) -> Vec<f64> {
    let chars: Vec<char> = engine.text().chars().collect();
    let bfs = if block_font_size > 0.0 {
        block_font_size
    } else {
        1.0
    };
    let mut offsets = vec![0.0f64; chars.len() + 1];
    let mut acc = 0.0f64;
    for run in engine.style_runs() {
        let m = run_metrics(
            &run.style,
            default,
            preview,
            block_font_name,
            orig_bold,
            orig_italic,
        );
        let size_scale = run.style.font_size / bfs;
        for (ci, &ch) in chars[run.start..run.end].iter().enumerate() {
            offsets[run.start + ci] = acc;
            acc += m.advance(ch) * size_scale;
        }
    }
    if !chars.is_empty() {
        offsets[chars.len()] = acc;
    }
    offsets
}

/// Serialise an [`ActiveStyle`](crate::editor::ActiveStyle) to the `"style"`
/// object embedded in `text_edit_state`.
///
/// Each value is `null` when the selection spans multiple values ("mixed"). The
/// `font` field is `""` for the block's own (unchanged) font, or the chosen
/// family name. `align` is always concrete. `size_scale` converts the engine's
/// text-space `font_size` to the visual point size shown in the panel.
fn style_to_json(s: &crate::editor::ActiveStyle, size_scale: f64) -> String {
    use crate::editor::FontChoice;
    let color = match s.color {
        Some([r, g, b]) => format!("[{r},{g},{b}]"),
        None => "null".to_owned(),
    };
    let font = match &s.font {
        Some(FontChoice::Family(f)) => json_str(f),
        Some(FontChoice::Original) => json_str(""),
        None => "null".to_owned(),
    };
    let size = match s.font_size {
        Some(v) => (v * size_scale).to_string(),
        None => "null".to_owned(),
    };
    let optb = |o: Option<bool>| match o {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    };
    format!(
        r#"{{"color":{},"font":{},"size":{},"bold":{},"italic":{},"underline":{},"strike":{},"align":{}}}"#,
        color,
        font,
        size,
        optb(s.bold),
        optb(s.italic),
        optb(s.underline),
        optb(s.strike),
        json_str(s.align.as_str()),
    )
}

/// Serialise editable blocks to the JSON returned by `text_edit_enter`.
fn blocks_to_json(blocks: &[crate::editor::EditBlock]) -> String {
    let parts: Vec<String> = blocks
        .iter()
        .map(|b| {
            let ids: Vec<String> = b.frame_ids.iter().map(|i| i.to_string()).collect();
            format!(
                r#"{{"id":{},"text":{},"x":{},"y":{},"width":{},"font_size":{},"font_name":{},"display_font":{},"bold":{},"italic":{},"font_key":{},"composite":{},"frame_ids":[{}]}}"#,
                b.id,
                json_str(&b.text),
                b.x,
                b.y,
                b.width,
                b.font_size,
                json_str(&b.font_name),
                json_str(&b.display_font),
                b.bold,
                b.italic,
                json_str(&b.font_key),
                b.composite,
                ids.join(","),
            )
        })
        .collect();
    format!("[{}]", parts.join(","))
}

// ── Real-renderer block preview (Option A) ──────────────────────────────────────

/// Result of [`WasmEditor::text_edit_render_block`]: an RGBA crop of one edited
/// block plus its device-pixel top-left position on the page.
#[cfg(feature = "render")]
#[wasm_bindgen]
pub struct EditBlockRender {
    /// Tile top-left x in device pixels (at the requested scale).
    pub x: f64,
    /// Tile top-left y in device pixels.
    pub y: f64,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    data: Vec<u8>,
}

#[cfg(feature = "render")]
#[wasm_bindgen]
impl EditBlockRender {
    /// Straight (un-premultiplied) RGBA bytes, row-major, 4 bytes/pixel.
    pub fn rgba_bytes(&self) -> js_sys::Uint8Array {
        js_sys::Uint8Array::from(self.data.as_slice())
    }
}

#[cfg(feature = "render")]
#[wasm_bindgen]
impl WasmEditor {
    /// Render the editable block `block_id` through the real page renderer,
    /// cropped to the block's bounding box, at `scale` device px per PDF point.
    ///
    /// Output is pixel-identical to normal page rendering (same embedded fonts,
    /// sizes, colours). When a caret session is open on this block AND its font
    /// is a resolvable simple font, the edited text is rendered in place;
    /// composite/CID blocks render their original text (Phase 3 covers CID
    /// re-encoding).
    pub fn text_edit_render_block(
        &self,
        block_id: usize,
        scale: f64,
    ) -> Result<EditBlockRender, JsError> {
        use crate::document::catalog::Catalog;
        use crate::document::page::Page;
        use crate::parser::objects::PdfObject;
        use crate::render::{render_block_tile, TileRect};

        let model = self
            .text_edit_model
            .as_ref()
            .ok_or_else(|| JsError::new("no active text-edit model"))?;
        let block = model
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .ok_or_else(|| JsError::new("block not found"))?;

        let doc = self.text_edit_doc();
        let page_index = self.text_edit_page;
        let catalog = Catalog::from_document(doc).map_err(|e| JsError::new(&e.to_string()))?;
        let page_dict = catalog
            .get_page_dict(doc, page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let page = Page::from_dict(doc, &page_dict).map_err(|e| JsError::new(&e.to_string()))?;

        let fs = block.font_size;

        // Current edited text when a caret session is open on this very block.
        let edit_text = self
            .active_text_edit
            .as_ref()
            .filter(|a| a.block_id == block_id)
            .map(|a| a.engine.text());

        // Encode the edited text into the font's own codes so the renderer draws
        // it with the **embedded** font (correct face), not a substitute.
        //   - simple font  → reverse Encoding/AGL via simple-font metrics (1-byte).
        //   - composite/CID → invert the ToUnicode CMap (2-byte codes).
        // `None` means "can't encode this text in this font" (e.g. a typed glyph
        // not present in the embedded font) → keep the original render.
        // Reuse the metrics computed when the block was opened (the common,
        // per-keystroke case) instead of re-inverting the ToUnicode CMap each time.
        let metrics = if self.active_text_edit.as_ref().map(|a| a.block_id) == Some(block_id) {
            self.active_text_edit
                .as_ref()
                .and_then(|a| a.render_metrics.clone())
        } else {
            crate::editor::font_metrics_for(doc, page_index, &block.font_key, fs)
                .ok()
                .flatten()
        };
        let encoded: Option<Vec<u8>> = edit_text
            .as_ref()
            .and_then(|t| encode_for_block(doc, &page, &block.font_key, &metrics, t));

        // Per-run preview plan when the open block carries formatting (colour /
        // size / alignment render live; font swap + underline/strike on commit).
        let plan = self.preview_run_plan(block, doc, &page, &metrics, fs);

        // Bounding box in user space (generous vertical padding around the baseline).
        // A formatted block sizes from the largest run; an alignment shift moves
        // the tile so it still covers the (re-positioned) text.
        let box_fs = plan.as_ref().map(|p| p.max_size).unwrap_or(fs);
        let ascent = box_fs * 0.85;
        let descent = box_fs * 0.30;
        let pad = (box_fs * 0.15).max(1.0);
        let align_dx = plan.as_ref().map(|p| p.align_dx_page).unwrap_or(0.0);
        // Width must stop at the last inked glyph: measuring the full edited text
        // (incl. trailing spaces) would push the tile/white-cover past the visible
        // text and overrun the page. Trim trailing whitespace before measuring; do
        // NOT trim the encoded text — the rendered string keeps what was typed.
        let width_pts = match (&plan, &edit_text, &metrics) {
            (Some(p), _, _) => p.width_pts,
            // Advances are text-space; scale to page space (matches `block.width`).
            (None, Some(t), Some(m)) => {
                (crate::editor::text_width(m, t.trim_end()) * block.scale_x).max(0.0)
            }
            _ => block.width,
        };
        let tile = TileRect {
            x: block.x + align_dx - pad,
            y: block.y - descent,
            width: width_pts + 2.0 * pad,
            height: ascent + descent,
        };

        // Build the content to interpret: the page content with this block's
        // show-text operand replaced by the re-encoded bytes; otherwise the
        // original page content (when the text isn't encodable in this font).
        let can_override = block.stream_idx == 0 && (encoded.is_some() || plan.is_some());
        // Formatted block: splice the per-run op sequence in place of the block's
        // primary show op (drop the rest), keeping only the run's `Tj`s.
        let plan_content: Option<Vec<u8>> = match (&plan, model.session.streams.first()) {
            (Some(p), Some(stream0)) if block.stream_idx == 0 => {
                let block_op_idx: Vec<usize> = block
                    .frame_ids
                    .iter()
                    .filter_map(|&fid| model.session.frames.get(fid).map(|f| f.stream_op_index))
                    .collect();
                let primary = block_op_idx.iter().copied().min().unwrap_or(0);
                let others: std::collections::HashSet<usize> = block_op_idx
                    .iter()
                    .copied()
                    .filter(|&i| i != primary)
                    .collect();
                let mut new_ops: Vec<crate::content::operators::Operation> =
                    Vec::with_capacity(stream0.ops.len() + p.run_ops.len());
                let mut run_tj_idx: Vec<usize> = Vec::new();
                for (i, op) in stream0.ops.iter().enumerate() {
                    if i == primary {
                        for ro in &p.run_ops {
                            if ro.operator == "Tj" || ro.operator == "TJ" {
                                run_tj_idx.push(new_ops.len());
                            }
                            new_ops.push(ro.clone());
                        }
                    } else if !others.contains(&i) {
                        new_ops.push(op.clone());
                    }
                }
                let kept =
                    crate::editor::edit_session::edit_render_content_ops(new_ops, &run_tj_idx);
                // Wrap the text ops in a balanced q/Q so the decoration rects below
                // start from the initial identity CTM, matching the committed render.
                // Without this, a top-level transform the page content leaves active
                // double-applies to the rects and pushes them off the tile.
                let mut wrapped: Vec<crate::content::operators::Operation> =
                    Vec::with_capacity(kept.len() + 2 + p.decorations.len() * 5);
                wrapped.push(crate::content::operators::Operation {
                    operator: "q".to_owned(),
                    operands: vec![],
                });
                wrapped.extend(kept);
                wrapped.push(crate::content::operators::Operation {
                    operator: "Q".to_owned(),
                    operands: vec![],
                });
                // Append underline/strikethrough as inline `re`/`f` ops AFTER the
                // text (outside any BT…ET), in page user-space — so decorations show
                // live in the tile, matching the committed result.
                wrapped.extend(crate::editor::build_decoration_ops(&p.decorations));
                Some(crate::editor::text_editor::serialize_operations(&wrapped))
            }
            _ => None,
        };
        let content: Vec<u8> = match (plan_content, encoded, model.session.streams.first()) {
            (Some(c), _, _) => c,
            (None, Some(enc), Some(stream0)) if block.stream_idx == 0 => {
                let mut ops = stream0.ops.clone();
                // Op indices of this block's own show ops (kept; all other show ops
                // and image `Do` ops are dropped below).
                let block_op_idx: Vec<usize> = block
                    .frame_ids
                    .iter()
                    .filter_map(|&fid| model.session.frames.get(fid).map(|f| f.stream_op_index))
                    .collect();
                for (k, &fid) in block.frame_ids.iter().enumerate() {
                    if let Some(f) = model.session.frames.get(fid) {
                        if let Some(op) = ops.get_mut(f.stream_op_index) {
                            op.operator = "Tj".to_owned();
                            // First op carries the whole replacement; blank the rest.
                            op.operands = vec![PdfObject::String(if k == 0 {
                                enc.clone()
                            } else {
                                Vec::new()
                            })];
                        }
                    }
                }
                // Block-only render: drop every *other* text show op and all image
                // `Do` ops so the interpreter shapes just this block's run and
                // decodes no images — the dominant per-keystroke cost. State ops
                // (cm / q / Q / rg / Tf / Tm) are kept, so the CTM and the
                // full-page crop math are unchanged. Visually identical: the host's
                // `composeBlock` white-covers the block and overlays only its crop,
                // so other content was never visible in the preview anyway.
                let kept = crate::editor::edit_session::edit_render_content_ops(ops, &block_op_idx);
                crate::editor::text_editor::serialize_operations(&kept)
            }
            _ => page
                .decode_contents(doc)
                .map_err(|e| JsError::new(&e.to_string()))?,
        };

        // Render ONLY the block's tile into a block-sized buffer — not the whole
        // page. `render_block_tile` uses an origin-(0,0) canvas with a tile-relative
        // CTM, so glyphs AND vector fills both land block-local and render correctly
        // for any page (including flip-`cm` pages). Cost is O(block area),
        // independent of page size and page number. The returned origin is the
        // block's top-left in full-page device pixels (where the host blits the crop).
        let (origin, buf) = render_block_tile(doc, &page, scale as f32, tile, &content)
            .map_err(|e| JsError::new(&e.to_string()))?;

        log::debug!(
            "[edit-render] id={} text={:?} block=({:.1},{:.1}) w={:.1} fs={:.1} stream_idx={} override={} tile=({:.1},{:.1},{:.1}x{:.1}) buf={}x{}",
            block_id,
            block.text.chars().take(24).collect::<String>(),
            block.x,
            block.y,
            block.width,
            fs,
            block.stream_idx,
            can_override,
            tile.x,
            tile.y,
            tile.width,
            tile.height,
            buf.width,
            buf.height,
        );

        // Un-premultiply the block buffer for ImageData (straight RGBA).
        let src = buf.data();
        let mut data = vec![0u8; src.len()];
        for (px, out) in src.chunks_exact(4).zip(data.chunks_exact_mut(4)) {
            let a = px[3];
            if a == 0 || a == 255 {
                out[0] = px[0];
                out[1] = px[1];
                out[2] = px[2];
            } else {
                let inv = 255.0 / a as f32;
                out[0] = (px[0] as f32 * inv).min(255.0) as u8;
                out[1] = (px[1] as f32 * inv).min(255.0) as u8;
                out[2] = (px[2] as f32 * inv).min(255.0) as u8;
            }
            out[3] = a;
        }

        Ok(EditBlockRender {
            x: origin.0 as f64,
            y: origin.1 as f64,
            width: buf.width,
            height: buf.height,
            data,
        })
    }
}

/// Render only the committed block's tile from the writer-pool content stream.
///
/// After [`WasmEditor::text_edit_commit`] the new content stream lives in the
/// writer pool. This function applies the same CoW overrides as
/// [`WasmEditor::render_page`], then renders only the block's original bounding
/// box (width = `block.width`, height derived from `font_size`) — O(block area)
/// instead of O(full page). Use this in the commit-render fast path to composite
/// a small updated crop onto the existing full-page cached bitmap rather than
/// re-rasterising the entire page.
#[cfg(feature = "render")]
#[wasm_bindgen]
impl WasmEditor {
    /// Render the committed block `block_id` as a cropped tile at `scale` device
    /// px per PDF point.
    ///
    /// Applies writer-pool overrides so the tile reflects the just-committed content
    /// stream. The tile covers the block's **full original width**, so it correctly
    /// erases deleted or shortened text when composited over the page bitmap.
    ///
    /// Returns the same [`EditBlockRender`] struct used by `text_edit_render_block`
    /// — origin (`x`, `y`) is the tile's top-left in full-page device pixels.
    pub fn render_committed_block_tile(
        &self,
        block_id: usize,
        scale: f64,
    ) -> Result<EditBlockRender, JsError> {
        use crate::document::{catalog::Catalog, page::Page};
        use crate::render::{render_block_tile, TileRect};

        let block = self
            .text_edit_blocks
            .iter()
            .find(|b| b.id == block_id)
            .ok_or_else(|| JsError::new("render_committed_block_tile: block not found"))?;

        // Tile covers the FULL original block width so deleted/shortened text is
        // fully hidden when this crop is composited over the page bitmap.
        let fs = block.font_size;
        let ascent = fs * 0.85;
        let descent = fs * 0.30;
        let pad = (fs * 0.15).max(1.0);
        let tile = TileRect {
            x: block.x - pad,
            y: block.y - descent,
            width: block.width + 2.0 * pad,
            height: ascent + descent,
        };

        // Apply writer-pool overrides so the page dict reflects the committed state.
        let overrides: std::collections::HashMap<u32, crate::parser::objects::PdfObject> = self
            .editor
            .writer
            .all_ids()
            .into_iter()
            .filter_map(|id| self.editor.writer.get_object(id).map(|o| (id, o.clone())))
            .collect();
        self.editor.doc.set_overrides(overrides);
        // Re-insert uncompressed bytes BEFORE decode_contents so get_stream_data
        // is a cache hit and no flate-decompress is needed.
        for (id, bytes) in &self.committed_bytes {
            self.editor.doc.preload_stream(*id, bytes);
        }

        let result = (|| -> crate::error::Result<((u32, u32), crate::render::PixmapBuffer)> {
            let catalog = Catalog::from_document(&self.editor.doc)?;
            let page_dict = catalog.get_page_dict(&self.editor.doc, self.text_edit_page)?;
            let page = Page::from_dict(&self.editor.doc, &page_dict)?;
            // decode_contents now hits decoded_stream_cache (preloaded above) —
            // no flate-decompress needed.
            let content = page.decode_contents(&self.editor.doc)?;
            render_block_tile(&self.editor.doc, &page, scale as f32, tile, &content)
        })();

        self.editor.doc.clear_overrides();

        let (origin, buf) = result.map_err(|e| JsError::new(&e.to_string()))?;

        // Un-premultiply the block buffer for ImageData (straight RGBA).
        let src = buf.data();
        let mut data = vec![0u8; src.len()];
        for (px, out) in src.chunks_exact(4).zip(data.chunks_exact_mut(4)) {
            let a = px[3];
            if a == 0 || a == 255 {
                out[0] = px[0];
                out[1] = px[1];
                out[2] = px[2];
            } else {
                let inv = 255.0 / a as f32;
                out[0] = (px[0] as f32 * inv).min(255.0) as u8;
                out[1] = (px[1] as f32 * inv).min(255.0) as u8;
                out[2] = (px[2] as f32 * inv).min(255.0) as u8;
            }
            out[3] = a;
        }
        Ok(EditBlockRender {
            x: origin.0 as f64,
            y: origin.1 as f64,
            width: buf.width,
            height: buf.height,
            data,
        })
    }
}

/// Per-run live-preview plan for a formatted block (see [`WasmEditor::preview_run_plan`]).
#[cfg(feature = "render")]
struct PreviewPlan {
    /// Replacement text operators (`rg`/`Tf`/`Tj` per run, optional leading `Td`).
    run_ops: Vec<crate::content::operators::Operation>,
    /// Total advance width in page-space points (for the crop tile).
    width_pts: f64,
    /// Alignment origin shift in page-space points (for tile placement).
    align_dx_page: f64,
    /// Largest run font size (for the tile's ascent/descent).
    max_size: f64,
    /// Underline/strikethrough rectangles (page user-space) for the live preview,
    /// drawn as inline `re`/`f` ops after the text so decorations show before commit.
    decorations: Vec<crate::editor::DecoRect>,
}

#[cfg(feature = "render")]
impl WasmEditor {
    /// Build a per-run preview plan for the open formatted block, or `None` when
    /// the block is plain (single default-styled run, left-aligned) or a run
    /// can't be encoded.
    ///
    /// Per-run colour, size and alignment render live. A bold/italic/family run
    /// renders in its **real embedded face** when that face has been embedded for
    /// the preview (see [`prepare_preview_fonts`](Self::prepare_preview_fonts) /
    /// `ActiveTextEdit::preview_fonts`); until then it falls back to the original
    /// font. Underline/strikethrough apply on commit.
    fn preview_run_plan(
        &self,
        block: &crate::editor::EditBlock,
        doc: &crate::parser::objects::PdfDocument,
        page: &crate::document::page::Page,
        metrics: &Option<PdfFontMetrics>,
        fs: f64,
    ) -> Option<PreviewPlan> {
        use crate::editor::{
            build_run_ops, decoration_thickness, encode_in_font, run_synthetic_style,
            strike_offset, underline_offset, CharStyle, DecoRect, FontChoice, ResolvedRun,
            RunLayout, SyntheticStyle,
        };

        let a = self
            .active_text_edit
            .as_ref()
            .filter(|a| a.block_id == block.id)?;
        let runs = a.engine.style_runs();
        let align = a.engine.align();
        // "Plain" = a single run still at the block's *seed* style (the font's
        // intrinsic bold/italic), left-aligned → no per-run preview needed, the
        // normal render already shows it correctly.
        let seed = CharStyle::from_block_styled(fs, a.orig_bold, a.orig_italic);
        let plain = align == Align::Left
            && runs.len() <= 1
            && runs.first().map(|r| r.style == seed).unwrap_or(true);
        if plain {
            return None;
        }
        let m = metrics.as_ref()?;
        let text: Vec<char> = a.engine.text().chars().collect();
        let scale_x = a.scale_x;
        let bfs = if fs > 0.0 { fs } else { 1.0 };

        let mut resolved: Vec<ResolvedRun> = Vec::with_capacity(runs.len());
        let mut total_w_text = 0.0_f64;
        let mut max_size = fs;
        // Per-run text-space width (size-scaled), for decoration placement below.
        let mut run_w_text_each: Vec<f64> = Vec::with_capacity(runs.len());
        for r in &runs {
            let run_text: String = text[r.start..r.end].iter().collect();
            // An `Original`-font run always renders from its embedded glyphs (with
            // synthetic bold/italic faked at draw time), so its decorations show
            // even when no substitute face is available. Only a chosen `Family`
            // run uses the embedded preview face (falling back to the original
            // font until that face is embedded).
            let (font_key, bytes, synthetic) = match &r.style.font {
                FontChoice::Family(fam) => {
                    match a
                        .preview_fonts
                        .get(&(fam.clone(), r.style.bold, r.style.italic))
                    {
                        Some((fkey, embedded)) => (
                            fkey.clone(),
                            embedded.encode(&run_text),
                            SyntheticStyle::default(),
                        ),
                        None => {
                            let enc = encode_in_font(
                                doc,
                                page,
                                &block.font_key,
                                r.style.font_size,
                                &run_text,
                            );
                            if !enc.is_complete() {
                                return None; // can't preview this run in the original font
                            }
                            (block.font_key.clone(), enc.bytes, SyntheticStyle::default())
                        }
                    }
                }
                FontChoice::Original => {
                    let enc =
                        encode_in_font(doc, page, &block.font_key, r.style.font_size, &run_text);
                    if !enc.is_complete() {
                        return None; // can't preview this run in the original font
                    }
                    (
                        block.font_key.clone(),
                        enc.bytes,
                        run_synthetic_style(&r.style, a.orig_bold, a.orig_italic),
                    )
                }
            };
            resolved.push(ResolvedRun {
                font_key,
                font_size: r.style.font_size,
                color: r.style.color,
                bytes,
                synthetic,
            });
            // Use per-run metrics so bold/italic tiles are cropped to the real width.
            let run_m = run_metrics(
                &r.style,
                m,
                &a.preview_metrics,
                &block.font_name,
                a.orig_bold,
                a.orig_italic,
            );
            let w_text = crate::editor::text_width(run_m, &run_text) * (r.style.font_size / bfs);
            run_w_text_each.push(w_text);
            total_w_text += w_text;
            if r.style.font_size > max_size {
                max_size = r.style.font_size;
            }
        }
        let width_pts = (total_w_text * scale_x).max(0.0);
        let align_dx_page = match align {
            Align::Left => 0.0,
            Align::Center => (block.width - width_pts) / 2.0,
            Align::Right => block.width - width_pts,
        };
        let align_dx_text = if scale_x.abs() > 1e-6 {
            align_dx_page / scale_x
        } else {
            0.0
        };

        // Underline/strikethrough rects, same geometry as `commit_block_runs_impl`
        // so the live preview matches the saved result. Page user-space.
        let mut decorations: Vec<DecoRect> = Vec::new();
        // Per-run starting text-space x (for the synthetic-italic `Tm` path).
        let mut run_x_text: Vec<f64> = Vec::with_capacity(runs.len());
        let mut cursor_text = 0.0_f64;
        for (i, r) in runs.iter().enumerate() {
            run_x_text.push(cursor_text);
            let x0 = block.x + align_dx_page + cursor_text * scale_x;
            let w = run_w_text_each[i] * scale_x;
            let size = r.style.font_size;
            let thick = decoration_thickness(size);
            if r.style.underline {
                decorations.push(DecoRect {
                    x: x0,
                    y: block.y - underline_offset(size),
                    width: w,
                    height: thick,
                    color: r.style.color,
                });
            }
            if r.style.strike {
                decorations.push(DecoRect {
                    x: x0,
                    y: block.y + strike_offset(size),
                    width: w,
                    height: thick,
                    color: r.style.color,
                });
            }
            cursor_text += run_w_text_each[i];
        }

        let layout = RunLayout {
            tm: [
                block.tm.a, block.tm.b, block.tm.c, block.tm.d, block.tm.e, block.tm.f,
            ],
            run_x_text,
            force_positioned: block.synthetic_italic,
            reset_stroke: false,
        };
        Some(PreviewPlan {
            run_ops: build_run_ops(&resolved, align_dx_text, Some(&layout)),
            width_pts,
            align_dx_page,
            max_size,
            decorations,
        })
    }
}

/// Encode `text` into the show-string bytes for the font under `font_key`, so the
/// renderer draws it with the **embedded** font.
///
/// - **Composite (Type0/CID):** invert the font's ToUnicode CMap to map each
///   Unicode char → its 2-byte code (big-endian), the inverse of how the renderer
///   decodes composite show-strings.
/// - **Simple:** reverse the font's Encoding/AGL via the resolved metrics' char→
///   code table (1 byte per char).
///
/// Returns `None` if any character can't be encoded in this font (e.g. a typed
/// glyph not present in the embedded subset) — the caller then keeps the original
/// render rather than show wrong glyphs. Pixel-exact handling of brand-new glyphs
/// (subsetting) is Phase B.
#[cfg(feature = "render")]
fn encode_for_block(
    doc: &crate::parser::objects::PdfDocument,
    page: &crate::document::page::Page,
    font_key: &str,
    metrics: &Option<crate::editor::PdfFontMetrics>,
    text: &str,
) -> Option<Vec<u8>> {
    use crate::content::interpreter::resolve_font_info;

    let (cmap, is_composite, _widths) =
        resolve_font_info(font_key, Some(doc), Some(&page.resources.raw));

    if is_composite {
        // Invert ToUnicode (code → unicode) to unicode → 2-byte code.
        let cmap = cmap?;
        let rev = cmap.unicode_to_code();
        let mut out = Vec::with_capacity(text.chars().count() * 2);
        for ch in text.chars() {
            // CMap values are full strings; a single typed char keys a 1-char string.
            let code = rev.get(&ch.to_string()).copied()?;
            // Composite show-strings are byte sequences of fixed code width (2 here
            // for Identity-H / typical CID fonts). Codes > u16 are not supported in
            // this preview path → fall back to original render.
            if code > 0xFFFF {
                return None;
            }
            out.push((code >> 8) as u8);
            out.push((code & 0xFF) as u8);
        }
        Some(out)
    } else {
        // Simple font: map each char → 1-byte code via the metrics reverse table.
        let m = metrics.as_ref()?;
        let mut out = Vec::with_capacity(text.chars().count());
        for ch in text.chars() {
            let code = m.code_for_char(ch)?;
            out.push(code);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::run_needs_substitute;
    use crate::editor::{CharStyle, FontChoice};

    /// After a FAST-PATH re-entry (same page/gen), `active_text_edit` must NOT be
    /// cleared — clearing it races with fire-and-forget commits on the JS side.
    #[test]
    #[cfg(feature = "wasm")]
    fn text_edit_enter_fast_path_preserves_active_session() {
        use crate::wasm::editor::WasmEditor;
        use std::path::PathBuf;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Group-3.pdf");
        let bytes = std::fs::read(&path).expect("Group-3.pdf fixture");
        let mut editor = WasmEditor::open(&bytes).expect("open");

        // FULL-REBUILD: builds the block model for page 0.
        editor.text_edit_enter(0).expect("initial text_edit_enter");
        let block_id = editor
            .text_edit_blocks
            .first()
            .expect("Group-3.pdf must have at least one text block on page 0")
            .id;

        // Open the block — stores the session in active_text_edit.
        assert!(
            editor.text_edit_open(block_id),
            "text_edit_open should succeed"
        );
        assert_eq!(
            editor.active_text_edit.as_ref().map(|a| a.block_id),
            Some(block_id),
        );

        // Re-enter the same page/gen → FAST-PATH branch.
        editor
            .text_edit_enter(0)
            .expect("fast-path text_edit_enter");

        // The session must still be alive.
        assert_eq!(
            editor.active_text_edit.as_ref().map(|a| a.block_id),
            Some(block_id),
            "FAST-PATH must not clear active_text_edit",
        );
    }

    #[test]
    #[cfg(feature = "wasm")]
    fn commit_italic_underline_keeps_original_font_and_commits() {
        use crate::wasm::editor::WasmEditor;
        use std::path::PathBuf;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Group-3.pdf");
        let bytes = std::fs::read(&path).expect("Group-3.pdf fixture");
        let mut editor = WasmEditor::open(&bytes).expect("open");
        editor.text_edit_enter(0).expect("enter");

        let (block_id, font_key) = {
            let b = editor
                .text_edit_blocks
                .first()
                .expect("a text block on page 0");
            (b.id, b.font_key.clone())
        };

        assert!(editor.text_edit_open(block_id), "open block");
        editor.text_edit_select_all();
        // Synthetic italic (Tm-shear path) + underline (decoration) on the
        // ORIGINAL embedded font — must commit, not fall back / swap the font.
        editor.text_edit_toggle_italic();
        editor.text_edit_toggle_underline();
        let res = editor.text_edit_commit(block_id).expect("commit");
        assert!(
            res.contains("\"committed\":true"),
            "rich-text commit should succeed, got {res}"
        );

        // The saved page still references the block's ORIGINAL font key — the
        // original-font run was kept (no DejaVu/Type0 substitution).
        let out = editor.save_bytes().expect("save");
        let doc = crate::parser::objects::PdfDocument::parse(out).expect("reparse");
        let catalog = crate::document::catalog::Catalog::from_document(&doc).expect("catalog");
        let page_dict = catalog.get_page_dict(&doc, 0).expect("page dict");
        let page = crate::document::page::Page::from_dict(&doc, &page_dict).expect("page");
        let content = page.decode_contents(&doc).expect("decode");
        let text = String::from_utf8_lossy(&content);
        assert!(
            text.contains(&format!("/{font_key}")),
            "original font key /{font_key} must still be selected after commit"
        );
    }

    #[test]
    fn keeps_original_when_style_matches_intrinsic() {
        // Original font, requested bold/italic == intrinsic → no substitution.
        let s = CharStyle::from_block_styled(12.0, true, false);
        assert!(!run_needs_substitute(&s, true, false));
        let s = CharStyle::from_block_styled(12.0, false, false);
        assert!(!run_needs_substitute(&s, false, false));
    }

    #[test]
    fn substitutes_when_weight_or_slant_differs() {
        // Original font but requested style differs from intrinsic → substitute.
        let s = CharStyle::from_block_styled(12.0, false, false);
        assert!(run_needs_substitute(&s, true, false), "un-bold a bold font");
        let s = CharStyle::from_block_styled(12.0, true, true);
        assert!(run_needs_substitute(&s, true, false), "add italic");
    }

    #[test]
    fn substitutes_for_chosen_family_even_if_flags_match() {
        let mut s = CharStyle::from_block_styled(12.0, false, false);
        s.font = FontChoice::Family("Times-Roman".into());
        assert!(run_needs_substitute(&s, false, false));
    }
}
