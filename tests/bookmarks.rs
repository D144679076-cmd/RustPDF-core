//! Integration tests for the bookmarks write API (Phase 2).

#![cfg(feature = "writer")]

use pdf_core::document::catalog::Catalog;
use pdf_core::document::outline::parse_outlines;
use pdf_core::document::{set_document_outline, OutlineEntry};
use pdf_core::editor::PdfEditor;
use pdf_core::parser::objects::PdfDocument;

fn load(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

fn multipage() -> Vec<u8> {
    load("multipage.pdf")
}

fn roundtrip(mut editor: PdfEditor, original: &[u8]) -> Vec<PdfDocument> {
    let saved = editor.save_append(original).unwrap();
    vec![PdfDocument::parse(saved).unwrap()]
}

// ---------------------------------------------------------------------------

#[test]
fn set_outline_two_chapters_with_child() {
    let original = multipage();
    let mut editor = PdfEditor::open(original.clone()).unwrap();

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

    let docs = roundtrip(editor, &original);
    let catalog = Catalog::from_document(&docs[0]).unwrap();
    let outlines = parse_outlines(&docs[0], &catalog.dict).unwrap();

    assert_eq!(outlines.len(), 2);
    assert_eq!(outlines[0].title, "Chapter 1");
    assert_eq!(outlines[1].title, "Chapter 2");
    assert_eq!(outlines[1].children.len(), 1);
    assert_eq!(outlines[1].children[0].title, "Section 2.1");
    assert!(!outlines[1].children[0].open);
}

#[test]
fn set_outline_catalog_has_outlines_ref() {
    let original = multipage();
    let mut editor = PdfEditor::open(original.clone()).unwrap();
    let entries = vec![OutlineEntry {
        title: "A".to_owned(),
        page_index: 0,
        y_position: 0.0,
        open: true,
        bold: false,
        italic: false,
        color: None,
        children: vec![],
    }];
    set_document_outline(&mut editor, &entries).unwrap();

    let saved = editor.save_append(&original).unwrap();
    let doc = PdfDocument::parse(saved).unwrap();
    let catalog = Catalog::from_document(&doc).unwrap();

    assert!(
        catalog.dict.contains_key("Outlines"),
        "catalog must have /Outlines"
    );
}

#[test]
fn remove_outlines_clears_catalog_entry() {
    let original = multipage();
    let mut editor = PdfEditor::open(original.clone()).unwrap();

    // First add bookmarks.
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

    let saved = editor.save_append(&original).unwrap();
    let doc = PdfDocument::parse(saved).unwrap();
    assert!(doc.page_count().unwrap() > 0);

    let catalog = Catalog::from_document(&doc).unwrap();
    let outlines = parse_outlines(&doc, &catalog.dict).unwrap();
    assert!(
        outlines.is_empty(),
        "outlines should be empty after removal"
    );
}

#[test]
fn set_outline_deep_nesting() {
    let original = multipage();
    let mut editor = PdfEditor::open(original.clone()).unwrap();

    // Three levels deep.
    let entries = vec![OutlineEntry {
        title: "L1".to_owned(),
        page_index: 0,
        y_position: 0.0,
        open: true,
        bold: false,
        italic: false,
        color: None,
        children: vec![OutlineEntry {
            title: "L2".to_owned(),
            page_index: 0,
            y_position: 200.0,
            open: true,
            bold: false,
            italic: false,
            color: None,
            children: vec![OutlineEntry {
                title: "L3".to_owned(),
                page_index: 0,
                y_position: 100.0,
                open: false,
                bold: false,
                italic: false,
                color: None,
                children: vec![],
            }],
        }],
    }];
    set_document_outline(&mut editor, &entries).unwrap();

    let saved = editor.save_append(&original).unwrap();
    let doc = PdfDocument::parse(saved).unwrap();
    let catalog = Catalog::from_document(&doc).unwrap();
    let outlines = parse_outlines(&doc, &catalog.dict).unwrap();

    assert_eq!(outlines[0].title, "L1");
    assert_eq!(outlines[0].children[0].title, "L2");
    assert_eq!(outlines[0].children[0].children[0].title, "L3");
}

#[test]
fn set_outline_replace_existing() {
    let original = multipage();
    let mut editor = PdfEditor::open(original.clone()).unwrap();

    let first = vec![OutlineEntry {
        title: "First".to_owned(),
        page_index: 0,
        y_position: 0.0,
        open: true,
        bold: false,
        italic: false,
        color: None,
        children: vec![],
    }];
    set_document_outline(&mut editor, &first).unwrap();

    let second = vec![
        OutlineEntry {
            title: "A".to_owned(),
            page_index: 0,
            y_position: 0.0,
            open: true,
            bold: false,
            italic: false,
            color: None,
            children: vec![],
        },
        OutlineEntry {
            title: "B".to_owned(),
            page_index: 1,
            y_position: 0.0,
            open: true,
            bold: false,
            italic: false,
            color: None,
            children: vec![],
        },
    ];
    set_document_outline(&mut editor, &second).unwrap();

    let saved = editor.save_append(&original).unwrap();
    let doc = PdfDocument::parse(saved).unwrap();
    let catalog = Catalog::from_document(&doc).unwrap();
    let outlines = parse_outlines(&doc, &catalog.dict).unwrap();

    assert_eq!(outlines.len(), 2, "second call should replace first");
    assert_eq!(outlines[0].title, "A");
    assert_eq!(outlines[1].title, "B");
}
