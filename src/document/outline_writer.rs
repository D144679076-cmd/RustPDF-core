//! PDF Outline (Bookmark) write API.
//!
//! Builds the linked-list structure of outline items (ISO 32000-1 §12.3.3)
//! and writes them into the document via the incremental update model.

use crate::editor::PdfEditor;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};

/// A single outline (bookmark) entry to write.
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    /// Display title shown in the viewer's bookmarks panel.
    pub title: String,
    /// 0-based page index the bookmark navigates to.
    pub page_index: usize,
    /// Vertical position on the target page in PDF user-space (points from
    /// the bottom). Pass `0.0` to let the viewer preserve the current position.
    pub y_position: f64,
    /// Whether this item's children are expanded by default.
    pub open: bool,
    /// Bold text style (/F bit 2).
    pub bold: bool,
    /// Italic text style (/F bit 1).
    pub italic: bool,
    /// Optional RGB colour for the title text (`[r, g, b]` in 0.0–1.0 range).
    pub color: Option<[f64; 3]>,
    /// Nested child entries.
    pub children: Vec<OutlineEntry>,
}

/// Replace the entire document outline (bookmarks) with `entries`.
///
/// Pass an empty slice to remove all existing bookmarks.
/// The root /Outlines dictionary and all item objects are written to the
/// editor's incremental update pool. The catalog's /Outlines pointer and
/// /PageMode are updated as well.
pub fn set_document_outline(editor: &mut PdfEditor, entries: &[OutlineEntry]) -> Result<()> {
    editor.checkpoint();

    if entries.is_empty() {
        return remove_outlines(editor);
    }

    // Reserve the root Outlines object ID before building children so that
    // child items can reference it as their /Parent.
    let outlines_id = editor.writer.reserve_id();

    let (first_id, last_id, count) = build_outline_items(editor, entries, outlines_id)?;

    let mut outlines_dict = PdfDict::new();
    outlines_dict.insert("Type".to_owned(), PdfObject::Name("Outlines".to_owned()));
    outlines_dict.insert("First".to_owned(), PdfObject::Reference(first_id, 0));
    outlines_dict.insert("Last".to_owned(), PdfObject::Reference(last_id, 0));
    outlines_dict.insert("Count".to_owned(), PdfObject::Integer(count as i64));
    editor
        .writer
        .set_object(outlines_id, PdfObject::Dictionary(outlines_dict));

    // Update the catalog to point at the new /Outlines and open the panel.
    let root_id = editor.catalog_id;
    let root_obj = editor.get_object(root_id)?;
    let mut root_dict = match root_obj {
        PdfObject::Dictionary(d) => d,
        _ => {
            return Err(PdfError::invalid_structure(
                "catalog object is not a dictionary",
            ))
        }
    };
    root_dict.insert("Outlines".to_owned(), PdfObject::Reference(outlines_id, 0));
    root_dict.insert(
        "PageMode".to_owned(),
        PdfObject::Name("UseOutlines".to_owned()),
    );
    editor.replace_object(root_id, PdfObject::Dictionary(root_dict));

    Ok(())
}

/// Recursively build outline item dictionaries for `entries`.
///
/// Returns `(first_item_id, last_item_id, total_descendant_count)`.
fn build_outline_items(
    editor: &mut PdfEditor,
    entries: &[OutlineEntry],
    parent_id: u32,
) -> Result<(u32, u32, usize)> {
    // Reserve IDs for every sibling before writing any of them so that
    // /Prev and /Next links can be filled in on the first pass.
    let item_ids: Vec<u32> = (0..entries.len())
        .map(|_| editor.writer.reserve_id())
        .collect();

    let mut total_count = 0usize;

    for (i, entry) in entries.iter().enumerate() {
        let item_id = item_ids[i];
        let prev_id = if i > 0 { Some(item_ids[i - 1]) } else { None };
        let next_id = item_ids.get(i + 1).copied();

        let page_ref = resolve_page_ref(editor, entry.page_index)?;

        let mut item = PdfDict::new();
        item.insert(
            "Title".to_owned(),
            PdfObject::String(entry.title.as_bytes().to_vec()),
        );
        item.insert("Parent".to_owned(), PdfObject::Reference(parent_id, 0));
        if let Some(prev) = prev_id {
            item.insert("Prev".to_owned(), PdfObject::Reference(prev, 0));
        }
        if let Some(next) = next_id {
            item.insert("Next".to_owned(), PdfObject::Reference(next, 0));
        }

        // [page_ref /XYZ left top zoom] — null preserves viewer's current value.
        item.insert(
            "Dest".to_owned(),
            PdfObject::Array(vec![
                page_ref,
                PdfObject::Name("XYZ".to_owned()),
                PdfObject::Null,
                PdfObject::Real(entry.y_position),
                PdfObject::Null,
            ]),
        );

        if entry.bold || entry.italic {
            // ISO 32000-1 Table 153: bit 1 = italic, bit 2 = bold.
            let flags = (if entry.italic { 1i64 } else { 0 }) | (if entry.bold { 2 } else { 0 });
            item.insert("F".to_owned(), PdfObject::Integer(flags));
        }
        if let Some(c) = &entry.color {
            item.insert(
                "C".to_owned(),
                PdfObject::Array(vec![
                    PdfObject::Real(c[0]),
                    PdfObject::Real(c[1]),
                    PdfObject::Real(c[2]),
                ]),
            );
        }

        let child_count = if !entry.children.is_empty() {
            let (first_child, last_child, cc) =
                build_outline_items(editor, &entry.children, item_id)?;
            item.insert("First".to_owned(), PdfObject::Reference(first_child, 0));
            item.insert("Last".to_owned(), PdfObject::Reference(last_child, 0));
            // Positive /Count = open; negative = closed (children hidden).
            let count_val = if entry.open { cc as i64 } else { -(cc as i64) };
            item.insert("Count".to_owned(), PdfObject::Integer(count_val));
            cc
        } else {
            0
        };

        total_count += 1 + child_count;
        editor
            .writer
            .set_object(item_id, PdfObject::Dictionary(item));
    }

    Ok((item_ids[0], *item_ids.last().unwrap(), total_count))
}

/// Return the page reference object for `page_index`.
///
/// Uses the cached page table when available (O(1)), falling back to
/// `PdfEditor::get_page_dict` which also builds the table on first call.
fn resolve_page_ref(editor: &mut PdfEditor, page_index: usize) -> Result<PdfObject> {
    if let Some(page_ref) = editor.doc.cached_page_ref(page_index) {
        return Ok(page_ref);
    }
    // get_page_dict builds the page table as a side effect; we only need
    // the object ID from the returned (id, dict) pair.
    let (page_id, _) = editor.get_page_dict(page_index)?;
    Ok(PdfObject::Reference(page_id, 0))
}

/// Remove the /Outlines entry from the catalog, if present.
fn remove_outlines(editor: &mut PdfEditor) -> Result<()> {
    let root_id = editor.catalog_id;
    let root_obj = editor.get_object(root_id)?;
    let mut root_dict = match root_obj {
        PdfObject::Dictionary(d) => d,
        _ => return Ok(()),
    };
    root_dict.shift_remove("Outlines");
    editor.replace_object(root_id, PdfObject::Dictionary(root_dict));
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::catalog::Catalog;
    use crate::document::outline::parse_outlines;
    use crate::editor::PdfEditor;
    use crate::parser::objects::PdfDocument;

    fn load(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read(path).unwrap()
    }

    fn multipage() -> Vec<u8> {
        load("multipage.pdf")
    }

    #[test]
    fn set_outline_creates_bookmarks() {
        let data = multipage();
        let mut editor = PdfEditor::open(data).unwrap();
        let entries = vec![
            OutlineEntry {
                title: "Chapter 1".to_owned(),
                page_index: 0,
                y_position: 0.0,
                open: true,
                bold: false,
                italic: false,
                color: None,
                children: vec![],
            },
            OutlineEntry {
                title: "Chapter 2".to_owned(),
                page_index: 1,
                y_position: 0.0,
                open: true,
                bold: false,
                italic: false,
                color: None,
                children: vec![OutlineEntry {
                    title: "Section 2.1".to_owned(),
                    page_index: 1,
                    y_position: 400.0,
                    open: false,
                    bold: false,
                    italic: false,
                    color: None,
                    children: vec![],
                }],
            },
        ];
        set_document_outline(&mut editor, &entries).unwrap();

        let original = multipage();
        let saved = editor.save_append(&original).unwrap();
        let doc2 = PdfDocument::parse(saved).unwrap();
        let catalog2 = Catalog::from_document(&doc2).unwrap();
        let outlines = parse_outlines(&doc2, &catalog2.dict).unwrap();

        assert_eq!(outlines.len(), 2);
        assert_eq!(outlines[0].title, "Chapter 1");
        assert_eq!(outlines[1].title, "Chapter 2");
        assert_eq!(outlines[1].children.len(), 1);
        assert_eq!(outlines[1].children[0].title, "Section 2.1");
    }

    #[test]
    fn set_outline_styling() {
        let data = multipage();
        let mut editor = PdfEditor::open(data).unwrap();
        let entries = vec![OutlineEntry {
            title: "Bold+Italic".to_owned(),
            page_index: 0,
            y_position: 100.0,
            open: true,
            bold: true,
            italic: true,
            color: Some([1.0, 0.0, 0.0]),
            children: vec![],
        }];
        set_document_outline(&mut editor, &entries).unwrap();

        let original = multipage();
        let saved = editor.save_append(&original).unwrap();
        let doc2 = PdfDocument::parse(saved).unwrap();

        // Confirm the document is still parseable and bookmark count is correct.
        let catalog2 = Catalog::from_document(&doc2).unwrap();
        let outlines = parse_outlines(&doc2, &catalog2.dict).unwrap();
        assert_eq!(outlines.len(), 1);
        assert_eq!(outlines[0].title, "Bold+Italic");
    }

    #[test]
    fn remove_outlines_works() {
        let data = multipage();
        let mut editor = PdfEditor::open(data).unwrap();

        // First add some bookmarks.
        let entries = vec![OutlineEntry {
            title: "Temp".to_owned(),
            page_index: 0,
            y_position: 0.0,
            open: true,
            bold: false,
            italic: false,
            color: None,
            children: vec![],
        }];
        set_document_outline(&mut editor, &entries).unwrap();

        // Then remove them.
        set_document_outline(&mut editor, &[]).unwrap();

        let original = multipage();
        let saved = editor.save_append(&original).unwrap();
        let doc2 = PdfDocument::parse(saved).unwrap();
        assert!(doc2.page_count().unwrap() > 0);

        let catalog2 = Catalog::from_document(&doc2).unwrap();
        let outlines = parse_outlines(&doc2, &catalog2.dict).unwrap();
        assert!(outlines.is_empty());
    }

    #[test]
    fn set_outline_idempotent() {
        let data = multipage();
        let mut editor = PdfEditor::open(data).unwrap();
        let entries = vec![OutlineEntry {
            title: "Only".to_owned(),
            page_index: 0,
            y_position: 0.0,
            open: true,
            bold: false,
            italic: false,
            color: None,
            children: vec![],
        }];

        // Call twice — second call should replace the first outline.
        set_document_outline(&mut editor, &entries).unwrap();
        set_document_outline(&mut editor, &entries).unwrap();

        let original = multipage();
        let saved = editor.save_append(&original).unwrap();
        let doc2 = PdfDocument::parse(saved).unwrap();
        let catalog2 = Catalog::from_document(&doc2).unwrap();
        let outlines = parse_outlines(&doc2, &catalog2.dict).unwrap();
        assert_eq!(outlines.len(), 1);
    }
}
