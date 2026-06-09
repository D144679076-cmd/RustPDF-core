//! Surgical write-back of an edited text block into the PDF content stream.
//!
//! **Two-phase design:**
//! - [`commit_block`] / [`commit_block_with_font`] patch the [`TextModel`]'s
//!   in-memory `OpStream` operators **only** — they do not touch the writer pool.
//! - Call [`crate::editor::edit_session::commit_edit_session`] once (typically
//!   from `text_edit_exit`) after all per-block edits are complete to flush the
//!   dirty streams to the writer pool and make them visible to `save_append`.
//!
//! This keeps the writer generation constant throughout an editing session so
//! `text_edit_enter` fast-paths on every re-entry without a full `PdfDocument`
//! clone per block commit.

use crate::content::operators::Operation;
use crate::editor::document_editor::PdfEditor;
use crate::editor::text_model::TextModel;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::font_subset::EmbeddedCidFont;

/// Patch block `block_id`'s show operator(s) with `bytes` (already encoded in
/// the block's font — 1-byte simple or 2-byte CID).
///
/// The block's leading show op is replaced with a single `Tj` carrying the full
/// replacement; any additional ops the block spanned are blanked (their text
/// consolidated into the first), matching the preview's substitution so on-page
/// layout is consistent. Positioning operators (`Tm`/`Td`/`Tf`) are untouched,
/// so the edited text keeps the block's original origin, font and size.
///
/// **This function only mutates the in-memory model — it does not flush to the
/// writer pool.** Call [`crate::editor::edit_session::commit_edit_session`] after
/// all block edits are complete to serialise and persist them.
///
/// Returns `InvalidStructure` if the block id is unknown.
pub fn commit_block(
    _editor: &mut PdfEditor,
    model: &mut TextModel,
    _page_index: usize,
    block_id: usize,
    bytes: &[u8],
) -> Result<()> {
    let block = model
        .blocks
        .iter()
        .find(|b| b.id == block_id)
        .ok_or_else(|| {
            PdfError::invalid_structure(format!("commit_block: unknown block id {block_id}"))
        })?;
    let stream_idx = block.stream_idx;
    let frame_ids = block.frame_ids.clone();

    // Resolve each frame's operator index within its stream up-front (immutable
    // borrow of `frames`) so the mutable stream borrow below stays clean.
    let op_indices: Vec<usize> = frame_ids
        .iter()
        .filter_map(|&fid| model.session.frames.get(fid).map(|f| f.stream_op_index))
        .collect();

    let stream =
        model.session.streams.get_mut(stream_idx).ok_or_else(|| {
            PdfError::invalid_structure("commit_block: stream index out of range")
        })?;

    let mut wrote_primary = false;
    for &op_idx in &op_indices {
        let Some(op) = stream.ops.get_mut(op_idx) else {
            continue;
        };
        // Only touch text-show ops; positioning/state ops are left as-is.
        if op.operator != "Tj" && op.operator != "TJ" {
            continue;
        }
        op.operator = "Tj".to_owned();
        op.operands = vec![PdfObject::String(if !wrote_primary {
            wrote_primary = true;
            bytes.to_vec()
        } else {
            Vec::new()
        })];
    }

    if !wrote_primary {
        return Err(PdfError::invalid_structure(
            "commit_block: no show operator found in block range",
        ));
    }

    // Mark the session dirty so the deferred flush (`commit_edit_session`, called
    // from `text_edit_exit`) actually fires. Without this the patched `ops` are
    // serialised into `committed_bytes` for live preview but never written to the
    // writer pool — so the edit is lost on save and reverts on the next re-enter.
    model.session.dirty = true;

    Ok(())
}

/// Like [`commit_block`], but the edited text is encoded against a newly
/// **embedded** font (write-back Tier 3): when the original font can't represent
/// a typed glyph, the caller embeds a bundled font via
/// [`crate::writer::font_subset::embed_cidfont_for_chars`] and passes it here.
///
/// Registers the embedded font in the page's `/Resources/Font` under a fresh key
/// (this part writes to the writer pool — unavoidable for Tier-3), switches the
/// block to it by prepending a `/<key> <size> Tf` operator before the block's
/// leading show op, then patches the Identity-H bytes. The original `Tf` for the
/// rest of the page is unaffected.
///
/// **Content stream flush is deferred** — like [`commit_block`], this function
/// only patches the in-memory model. Call
/// [`crate::editor::edit_session::commit_edit_session`] to persist it.
pub fn commit_block_with_font(
    editor: &mut PdfEditor,
    model: &mut TextModel,
    page_index: usize,
    block_id: usize,
    font: &EmbeddedCidFont,
    text: &str,
) -> Result<()> {
    let (stream_idx, frame_ids, font_size) = {
        let block = model
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .ok_or_else(|| {
                PdfError::invalid_structure(format!(
                    "commit_block_with_font: unknown id {block_id}"
                ))
            })?;
        // Only the page content stream can carry a new page-level font resource
        // here; XObject-local resources are out of scope for this fallback.
        if block.stream_idx != 0 {
            return Err(PdfError::invalid_structure(
                "commit_block_with_font: embedded-font fallback only supports page content",
            ));
        }
        (block.stream_idx, block.frame_ids.clone(), block.font_size)
    };

    // Register the embedded font under a fresh /Resources/Font key on the page.
    let key = register_page_font(editor, page_index, font.font_id)?;
    let bytes = font.encode(text);

    let op_indices: Vec<usize> = frame_ids
        .iter()
        .filter_map(|&fid| model.session.frames.get(fid).map(|f| f.stream_op_index))
        .collect();
    let primary_op = *op_indices
        .iter()
        .min()
        .ok_or_else(|| PdfError::invalid_structure("commit_block_with_font: empty block"))?;

    let stream = model.session.streams.get_mut(stream_idx).ok_or_else(|| {
        PdfError::invalid_structure("commit_block_with_font: stream index out of range")
    })?;

    // Replace the block's show ops (primary carries the new bytes; rest blanked).
    let mut wrote = false;
    for &op_idx in &op_indices {
        let Some(op) = stream.ops.get_mut(op_idx) else {
            continue;
        };
        if op.operator != "Tj" && op.operator != "TJ" {
            continue;
        }
        op.operator = "Tj".to_owned();
        op.operands = vec![PdfObject::String(if !wrote {
            wrote = true;
            bytes.clone()
        } else {
            Vec::new()
        })];
    }
    if !wrote {
        return Err(PdfError::invalid_structure(
            "commit_block_with_font: no show operator in block",
        ));
    }

    // Insert `/<key> <size> Tf` immediately before the primary show op so the
    // block renders with the embedded font; ops after it shift by one.
    let tf = Operation {
        operator: "Tf".to_owned(),
        operands: vec![PdfObject::Name(key), PdfObject::Real(font_size)],
    };
    stream.ops.insert(primary_op, tf);

    // Mark dirty so the deferred flush persists this stream (see commit_block).
    model.session.dirty = true;

    Ok(())
}

/// Add `font_id` to the page's `/Resources/Font` under a fresh `/EdN` key and
/// return that key. Reuses an existing entry that already points at `font_id`.
///
/// Shared by the single-font ([`commit_block_with_font`]) and multi-run
/// ([`crate::editor::commit_block_runs`]) write-back paths.
pub fn register_page_font(
    editor: &mut PdfEditor,
    page_index: usize,
    font_id: u32,
) -> Result<String> {
    let (page_obj_id, page_dict) = editor.get_page_dict(page_index)?;
    let mut page = page_dict;

    // Resources / Font may be inline dicts or indirect references — resolve
    // through the CoW chain so existing fonts (F1, F2, …) are preserved when
    // a new embedded font is registered alongside them.
    let resources_val = page.get("Resources").cloned();
    let mut resources: PdfDict = match resources_val {
        Some(PdfObject::Dictionary(d)) => d,
        Some(PdfObject::Reference(id, _)) => match editor.get_object(id) {
            Ok(PdfObject::Dictionary(d)) => d,
            _ => PdfDict::new(),
        },
        _ => PdfDict::new(),
    };

    let fonts_val = resources.get("Font").cloned();
    let mut fonts: PdfDict = match fonts_val {
        Some(PdfObject::Dictionary(d)) => d,
        Some(PdfObject::Reference(id, _)) => match editor.get_object(id) {
            Ok(PdfObject::Dictionary(d)) => d,
            _ => PdfDict::new(),
        },
        _ => PdfDict::new(),
    };

    // Reuse if already present (idempotent across repeated edits of one block).
    for (k, v) in &fonts {
        if matches!(v, PdfObject::Reference(id, _) if *id == font_id) {
            return Ok(k.clone());
        }
    }

    // Pick a fresh /EdN key that doesn't collide with existing font keys.
    let mut n = 0u32;
    let key = loop {
        let cand = format!("Ed{n}");
        if !fonts.contains_key(&cand) {
            break cand;
        }
        n += 1;
    };
    fonts.insert(key.clone(), PdfObject::Reference(font_id, 0));
    resources.insert("Font".to_owned(), PdfObject::Dictionary(fonts));
    page.insert("Resources".to_owned(), PdfObject::Dictionary(resources));
    editor.replace_object(page_obj_id, PdfObject::Dictionary(page));
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::build_text_model;
    use crate::editor::document_editor::PdfEditor;

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
    fn commit_block_replaces_show_text_simple_font() {
        use crate::editor::edit_session::commit_edit_session;
        let bytes = simple_pdf();
        let mut editor = PdfEditor::open(bytes.clone()).expect("open");
        let mut model = build_text_model(&editor.doc, 0).expect("model");
        let block = model
            .blocks
            .iter()
            .find(|b| b.text.contains("Hello"))
            .expect("Hello block")
            .id;

        // "World" in WinAnsi == raw ASCII bytes.
        commit_block(&mut editor, &mut model, 0, block, b"World").expect("commit");
        // Flush the patched ops to the writer pool before saving.
        commit_edit_session(&mut editor, 0, &model.session).expect("flush");

        let out = editor.save_append(&bytes).expect("save");
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("(World)"), "expected (World) in saved stream");
    }

    #[test]
    fn commit_block_unknown_id_errors() {
        let bytes = simple_pdf();
        let mut editor = PdfEditor::open(bytes).expect("open");
        let mut model = build_text_model(&editor.doc, 0).expect("model");
        let err = commit_block(&mut editor, &mut model, 0, 9999, b"x").unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn commit_block_sets_session_dirty() {
        // Regression: commit_block must mark the session dirty so the deferred
        // flush in text_edit_exit actually fires. Without this the edit is lost
        // on save and reverts on the next re-enter.
        let bytes = simple_pdf();
        let mut editor = PdfEditor::open(bytes).expect("open");
        let mut model = build_text_model(&editor.doc, 0).expect("model");
        assert!(!model.session.dirty, "fresh model must not be dirty");
        let block = model
            .blocks
            .iter()
            .find(|b| b.text.contains("Hello"))
            .expect("Hello block")
            .id;
        commit_block(&mut editor, &mut model, 0, block, b"World").expect("commit");
        assert!(
            model.session.dirty,
            "commit_block must set session.dirty so the flush fires"
        );
    }

    #[test]
    fn commit_block_failed_patch_leaves_session_clean() {
        // A failed commit (unknown id) must NOT mark the session dirty.
        let bytes = simple_pdf();
        let mut editor = PdfEditor::open(bytes).expect("open");
        let mut model = build_text_model(&editor.doc, 0).expect("model");
        let _ = commit_block(&mut editor, &mut model, 0, 9999, b"x");
        assert!(!model.session.dirty, "failed commit must not set dirty");
    }

    // Build a minimal PDF where /Resources is an indirect object reference
    // rather than an inline dictionary, as many real-world PDFs produce.
    fn indirect_resources_pdf() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 72 700 Td (Hello) Tj ET";
        // Object layout:
        //  1 = Catalog  2 = Pages  3 = Page  4 = Content stream
        //  5 = Font F1  6 = Resources dict (indirect)
        let mut objs: Vec<String> = Vec::new();
        objs.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
        objs.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string());
        // Page references Resources as an indirect object (6 0 R).
        objs.push(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources 6 0 R /Contents 4 0 R >>"
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
        // Object 6: the Resources dict (indirect).
        objs.push("<< /Font << /F1 5 0 R >> >>".to_string());

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

    /// Registering an embedded font must preserve existing fonts (F1, F2, …)
    /// even when the page's /Resources entry is an indirect reference.
    #[cfg(feature = "render")]
    #[test]
    fn register_page_font_preserves_existing_fonts_with_indirect_resources() {
        use crate::render::font_resolver::{EmbeddedFontResolver, FontResolver};
        use crate::writer::font_subset::embed_cidfont_for_chars;

        let bytes = indirect_resources_pdf();
        let mut editor = PdfEditor::open(bytes.clone()).expect("open");

        let font_bytes = EmbeddedFontResolver
            .resolve("Helvetica", false, false)
            .expect("bundled font");
        let chars: Vec<char> = "Hi".chars().collect();
        let embedded =
            embed_cidfont_for_chars(&mut editor, &font_bytes, "Helvetica", &chars).expect("embed");

        let key = register_page_font(&mut editor, 0, embedded.font_id).expect("register");
        assert_eq!(key, "Ed0");

        // After registration the page must still contain the original F1 font.
        let (_, page_dict) = editor.get_page_dict(0).expect("page dict");
        let resources = match page_dict.get("Resources").cloned() {
            Some(PdfObject::Dictionary(d)) => d,
            Some(PdfObject::Reference(id, _)) => match editor.get_object(id).expect("resolve") {
                PdfObject::Dictionary(d) => d,
                _ => panic!("resources not a dict"),
            },
            other => panic!("unexpected Resources: {other:?}"),
        };
        let fonts = match resources.get("Font") {
            Some(PdfObject::Dictionary(d)) => d,
            _ => panic!("no Font dict"),
        };
        assert!(fonts.contains_key("F1"), "original F1 must be preserved");
        assert!(
            fonts.contains_key("Ed0"),
            "newly registered Ed0 must be present"
        );
    }

    // Tier-3 embed fallback: register a bundled font, retarget a block to it, and
    // verify the saved PDF gains a Type0/Identity-H font + a `/Ed0 … Tf` switch.
    #[cfg(feature = "render")]
    #[test]
    fn commit_block_with_font_embeds_and_retargets() {
        use crate::editor::edit_session::commit_edit_session;
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

        let font_bytes = EmbeddedFontResolver
            .resolve("Helvetica", false, false)
            .expect("bundled font");
        let chars: Vec<char> = "Hi".chars().collect();
        let embedded =
            embed_cidfont_for_chars(&mut editor, &font_bytes, "Helvetica", &chars).expect("embed");

        commit_block_with_font(&mut editor, &mut model, 0, block, &embedded, "Hi").expect("commit");
        // Flush patched ops to the writer pool before saving.
        commit_edit_session(&mut editor, 0, &model.session).expect("flush");

        let out = editor.save_append(&bytes).expect("save");
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("/Type0"),
            "saved PDF should contain a Type0 font"
        );
        assert!(
            text.contains("/Identity-H"),
            "Type0 font should use Identity-H"
        );
        assert!(
            text.contains("/Ed0"),
            "block should be retargeted to the /Ed0 font key"
        );
    }
}
