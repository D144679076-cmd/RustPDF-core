//! Page CRUD and content-layer append.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::content_builder::ContentBuilder;
use crate::writer::streams::make_flate_stream;

use super::document_editor::PdfEditor;

// ── Add page ──────────────────────────────────────────────────────────────────

/// Insert a blank page at `index` (0-based).
///
/// Existing pages shift right. If `index` is ≥ the current page count the
/// new page is appended at the end.
///
/// Only flat page trees (a single `/Pages` node whose `/Kids` holds
/// individual `/Page` dicts) are currently supported. Multi-level trees are
/// silently treated as flat at the root level.
pub fn add_blank_page(editor: &mut PdfEditor, index: usize, width: f64, height: f64) -> Result<()> {
    let pages_id = editor.pages_id;

    // Read current pages node.
    let pages_obj = editor.get_object(pages_id)?;
    let pages_dict = pages_obj
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("pages node is not a dict"))?
        .clone();

    let mut kids: Vec<PdfObject> = match pages_dict.get("Kids") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Err(PdfError::invalid_structure("pages /Kids is not an array")),
    };

    let current_count = kids.len();
    let insert_pos = index.min(current_count);

    // Build new blank page dict.
    let mut page = PdfDict::new();
    page.insert("Type".to_owned(), PdfObject::Name("Page".to_owned()));
    page.insert("Parent".to_owned(), PdfObject::Reference(pages_id, 0));
    page.insert(
        "MediaBox".to_owned(),
        PdfObject::Array(vec![
            PdfObject::Real(0.0),
            PdfObject::Real(0.0),
            PdfObject::Real(width),
            PdfObject::Real(height),
        ]),
    );
    page.insert(
        "Resources".to_owned(),
        PdfObject::Dictionary(PdfDict::new()),
    );

    let new_page_id = editor.add_object(PdfObject::Dictionary(page));

    // Insert reference into kids.
    kids.insert(insert_pos, PdfObject::Reference(new_page_id, 0));

    // Update pages node.
    let new_count = kids.len() as i64;
    let mut updated_pages = pages_dict.clone();
    updated_pages.insert("Kids".to_owned(), PdfObject::Array(kids));
    updated_pages.insert("Count".to_owned(), PdfObject::Integer(new_count));
    editor.replace_object(pages_id, PdfObject::Dictionary(updated_pages));

    Ok(())
}

// ── Delete page ───────────────────────────────────────────────────────────────

/// Remove the page at `index` (0-based) from the document.
///
/// The page object itself becomes unreachable but is not physically erased
/// (this is correct per the incremental update model — the old object simply
/// loses all references).
pub fn delete_page(editor: &mut PdfEditor, index: usize) -> Result<()> {
    let pages_id = editor.pages_id;

    let pages_obj = editor.get_object(pages_id)?;
    let pages_dict = pages_obj
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("pages node is not a dict"))?
        .clone();

    let mut kids: Vec<PdfObject> = match pages_dict.get("Kids") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Err(PdfError::invalid_structure("pages /Kids is not an array")),
    };

    if index >= kids.len() {
        return Err(PdfError::invalid_structure(format!(
            "page index {} out of range (count={})",
            index,
            kids.len()
        )));
    }

    kids.remove(index);

    let new_count = kids.len() as i64;
    let mut updated_pages = pages_dict.clone();
    updated_pages.insert("Kids".to_owned(), PdfObject::Array(kids));
    updated_pages.insert("Count".to_owned(), PdfObject::Integer(new_count));
    editor.replace_object(pages_id, PdfObject::Dictionary(updated_pages));

    Ok(())
}

// ── Content layer ─────────────────────────────────────────────────────────────

/// An open content stream being built for a specific page.
///
/// Created by [`begin_edit_page`] and finalised by [`ContentLayer::commit`].
/// New drawing operators paint on top of existing content because the new
/// stream is appended at the end of the page's `/Contents` array.
pub struct ContentLayer {
    /// 0-based page index this layer targets.
    pub page_index: usize,
    /// Object ID of the page dictionary.
    pub page_id: u32,
    /// Existing `/Contents` references from the original page.
    existing_contents: Vec<PdfObject>,
    /// Builder for new content stream operators.
    pub builder: ContentBuilder,
}

impl ContentLayer {
    /// Finalise this layer: compress the content stream, write it as a new
    /// stream object, and update the page's `/Contents` array.
    pub fn commit(self, editor: &mut PdfEditor) -> Result<()> {
        let bytes = self.builder.build();
        if bytes.is_empty() {
            return Ok(()); // nothing to commit
        }

        let stream = make_flate_stream(&bytes, PdfDict::new())?;
        let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

        // Rebuild /Contents: original refs + new stream ref.
        let mut contents = self.existing_contents;
        contents.push(PdfObject::Reference(stream_id, 0));

        // Read the current page dict (may already be in the writer pool).
        let page_obj = editor.get_object(self.page_id)?;
        let mut page_dict = page_obj
            .as_dict()
            .ok_or_else(|| PdfError::invalid_structure("page object is not a dict"))?
            .clone();

        page_dict.insert(
            "Contents".to_owned(),
            if contents.len() == 1 {
                contents.remove(0)
            } else {
                PdfObject::Array(contents)
            },
        );

        editor.replace_object(self.page_id, PdfObject::Dictionary(page_dict));
        Ok(())
    }
}

/// Open a content layer for page `index`, ready for drawing operations.
///
/// The returned [`ContentLayer`] holds a [`ContentBuilder`] where you emit
/// PDF operators. Call [`ContentLayer::commit`] when done.
pub fn begin_edit_page(editor: &PdfEditor, index: usize) -> Result<ContentLayer> {
    let (page_id, page_dict) = editor.get_page_dict(index)?;

    // Collect existing /Contents references.
    let existing_contents: Vec<PdfObject> = match page_dict.get("Contents") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        Some(r @ PdfObject::Reference(_, _)) => vec![r.clone()],
        None => vec![],
        _ => vec![],
    };

    Ok(ContentLayer {
        page_index: index,
        page_id,
        existing_contents,
        builder: ContentBuilder::new(),
    })
}

// ── Move page ─────────────────────────────────────────────────────────────────

/// Move the page at `from_index` to `to_index` (both 0-based).
///
/// Both indices must be within `0..page_count`. The page currently at
/// `from_index` is removed, then re-inserted at `to_index` (which is
/// evaluated after the removal, i.e. in the resulting shorter array).
pub fn move_page(editor: &mut PdfEditor, from_index: usize, to_index: usize) -> Result<()> {
    let pages_id = editor.pages_id;

    let pages_obj = editor.get_object(pages_id)?;
    let pages_dict = pages_obj
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("pages node is not a dict"))?
        .clone();

    let mut kids: Vec<PdfObject> = match pages_dict.get("Kids") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Err(PdfError::invalid_structure("pages /Kids is not an array")),
    };

    let n = kids.len();
    if from_index >= n {
        return Err(PdfError::invalid_structure(format!(
            "move_page: from_index {} out of range (count={})",
            from_index, n
        )));
    }
    if to_index >= n {
        return Err(PdfError::invalid_structure(format!(
            "move_page: to_index {} out of range (count={})",
            to_index, n
        )));
    }

    let item = kids.remove(from_index);
    kids.insert(to_index, item);

    let mut updated = pages_dict.clone();
    updated.insert("Kids".to_owned(), PdfObject::Array(kids));
    editor.replace_object(pages_id, PdfObject::Dictionary(updated));

    Ok(())
}

// ── Rotate page ───────────────────────────────────────────────────────────────

/// Set the rotation of page `index` (0-based) to `degrees` clockwise.
///
/// `degrees` must be a multiple of 90. Pass `0` to clear any existing rotation.
pub fn rotate_page(editor: &mut PdfEditor, index: usize, degrees: i32) -> Result<()> {
    let normalized = degrees.rem_euclid(360);
    if normalized % 90 != 0 {
        return Err(PdfError::invalid_structure(format!(
            "rotate_page: degrees must be a multiple of 90, got {}",
            degrees
        )));
    }

    let (page_id, mut page_dict) = editor.get_page_dict(index)?;
    page_dict.insert("Rotate".to_owned(), PdfObject::Integer(normalized as i64));
    editor.replace_object(page_id, PdfObject::Dictionary(page_dict));
    Ok(())
}

// ── Crop box ──────────────────────────────────────────────────────────────────

/// Set or replace the `/CropBox` on page `index` (0-based).
///
/// `rect` is `[x1, y1, x2, y2]` in PDF user-space points (origin bottom-left).
/// The crop box restricts the visible region but does not remove content.
pub fn set_crop_box(editor: &mut PdfEditor, index: usize, rect: [f64; 4]) -> Result<()> {
    let (page_id, mut page_dict) = editor.get_page_dict(index)?;
    page_dict.insert(
        "CropBox".to_owned(),
        PdfObject::Array(rect.iter().map(|&v| PdfObject::Real(v)).collect()),
    );
    editor.replace_object(page_id, PdfObject::Dictionary(page_dict));
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::document_editor::PdfEditor;
    use crate::parser::objects::PdfDocument;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    #[test]
    fn add_page_increments_count() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data.clone()).unwrap();
        let before = editor.page_count().unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        assert_eq!(editor.page_count().unwrap(), before + 1);
    }

    #[test]
    fn add_page_save_append_parseable() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let before = PdfDocument::parse(original.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(data).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let result = editor.save_append(&original).unwrap();
        let reopened = PdfDocument::parse(result).unwrap();
        assert_eq!(reopened.page_count().unwrap(), before + 1);
    }

    #[test]
    fn add_two_pages_correct_order() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let before = PdfDocument::parse(original.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(data).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap(); // prepend another
        let result = editor.save_append(&original).unwrap();
        let reopened = PdfDocument::parse(result).unwrap();
        assert_eq!(reopened.page_count().unwrap(), before + 2);
    }

    #[test]
    fn delete_page_out_of_range_errors() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        // Use an index beyond the actual page count.
        let err = delete_page(&mut editor, 999).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn delete_page_decrements_count() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        let before = editor.page_count().unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        assert_eq!(editor.page_count().unwrap(), before + 1);
        delete_page(&mut editor, 0).unwrap();
        assert_eq!(editor.page_count().unwrap(), before);
    }

    #[test]
    fn content_layer_commit_creates_content_stream() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let before = PdfDocument::parse(original.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(data).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();

        {
            let mut layer = begin_edit_page(&editor, 0).unwrap();
            layer
                .builder
                .save()
                .set_fill_rgb(1.0, 0.0, 0.0)
                .rect(10.0, 10.0, 100.0, 100.0)
                .fill()
                .restore();
            layer.commit(&mut editor).unwrap();
        }

        let result = editor.save_append(&original).unwrap();
        let reopened = PdfDocument::parse(result).unwrap();
        assert_eq!(reopened.page_count().unwrap(), before + 1);
    }

    #[test]
    fn empty_content_layer_commit_is_noop() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();

        // Commit with empty builder — should not add a content stream.
        let pool_before = editor.writer.len();
        let layer = begin_edit_page(&editor, 0).unwrap();
        layer.commit(&mut editor).unwrap();
        // pool should not grow (empty commit)
        assert_eq!(editor.writer.len(), pool_before);
    }

    #[test]
    fn move_page_swaps_order() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        // Delete the existing page so we start with a clean slate.
        delete_page(&mut editor, 0).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap(); // page 0
        add_blank_page(&mut editor, 1, 612.0, 792.0).unwrap(); // page 1
        assert_eq!(editor.page_count().unwrap(), 2);
        // Remember IDs before move.
        let (id0_before, _) = editor.get_page_dict(0).unwrap();
        let (id1_before, _) = editor.get_page_dict(1).unwrap();
        // Move page 0 to position 1 (swap).
        move_page(&mut editor, 0, 1).unwrap();
        let (id0_after, _) = editor.get_page_dict(0).unwrap();
        let (id1_after, _) = editor.get_page_dict(1).unwrap();
        assert_eq!(id0_after, id1_before, "former page 1 is now page 0");
        assert_eq!(id1_after, id0_before, "former page 0 is now page 1");
    }

    #[test]
    fn move_page_out_of_range_errors() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let err = move_page(&mut editor, 0, 999).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn rotate_page_sets_rotate_key() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        rotate_page(&mut editor, 0, 90).unwrap();
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        assert_eq!(page_dict.get("Rotate"), Some(&PdfObject::Integer(90)));
    }

    #[test]
    fn rotate_page_normalises_degrees() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        rotate_page(&mut editor, 0, 450).unwrap(); // 450 % 360 = 90
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        assert_eq!(page_dict.get("Rotate"), Some(&PdfObject::Integer(90)));
    }

    #[test]
    fn rotate_page_invalid_angle_errors() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let err = rotate_page(&mut editor, 0, 45).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn set_crop_box_stores_rect() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        set_crop_box(&mut editor, 0, [10.0, 10.0, 580.0, 830.0]).unwrap();
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        match page_dict.get("CropBox") {
            Some(PdfObject::Array(arr)) => {
                assert_eq!(arr.len(), 4);
                assert_eq!(arr[0], PdfObject::Real(10.0));
            }
            _ => panic!("expected /CropBox array"),
        }
    }
}
