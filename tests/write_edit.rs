//! Integration tests for the writer and editor subsystems.

/// Activate a Pro license once for the process so the gated annotation/forms
/// functions pass the license check. Idempotent — safe to call from every test.
#[cfg(feature = "crypto")]
fn ensure_pro_license() {
    use pdf_core::license::{activate, encode_license_key, validate_license_key, Tier};
    let key = encode_license_key(Tier::Pro, 0, "integration-test");
    let license = validate_license_key(&key).expect("test key must be valid");
    let _ = activate(license); // ignore error if already activated
}

#[cfg(not(feature = "crypto"))]
fn ensure_pro_license() {}

#[cfg(feature = "writer")]
mod writer_tests {
    use pdf_core::parser::objects::{PdfDict, PdfDocument, PdfObject};
    use pdf_core::writer::{write_standard_font, ContentBuilder, PageBuilder, PdfWriter, TjItem};

    fn build_minimal() -> Vec<u8> {
        let mut writer = PdfWriter::new();

        // Pages node (no pages yet)
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = writer.add_object(PdfObject::Dictionary(pages));

        // Catalog
        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = writer.add_object(PdfObject::Dictionary(catalog));

        writer.serialize_all(cat_id, None, None).unwrap()
    }

    #[test]
    fn create_empty_pdf_and_reopen() {
        let bytes = build_minimal();
        let doc = PdfDocument::parse(bytes).expect("should parse fresh empty PDF");
        assert_eq!(doc.page_count().unwrap(), 0);
    }

    #[test]
    fn create_pdf_with_one_page_and_text() {
        let mut writer = PdfWriter::new();

        // Font
        let font_id = write_standard_font("Helvetica", &mut writer).unwrap();

        // Content stream: "Hello PDF"
        let mut content = ContentBuilder::new();
        content
            .begin_text()
            .set_font("F1", 14.0)
            .move_text_pos(72.0, 720.0)
            .show_text_str("Hello PDF")
            .end_text();
        let content_bytes = content.build();

        // Page
        let pages_id = writer.reserve_id();
        let mut page_builder = PageBuilder::new(595.0, 842.0);
        page_builder.add_font("F1", font_id);
        page_builder.add_content(content_bytes);
        let page_id = page_builder.build(pages_id, &mut writer).unwrap();

        // Pages node
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert(
            "Kids".to_owned(),
            PdfObject::Array(vec![PdfObject::Reference(page_id, 0)]),
        );
        pages.insert("Count".to_owned(), PdfObject::Integer(1));
        writer.set_object(pages_id, PdfObject::Dictionary(pages));

        // Catalog
        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = writer.add_object(PdfObject::Dictionary(catalog));

        let bytes = writer.serialize_all(cat_id, None, None).unwrap();

        let doc = PdfDocument::parse(bytes).expect("should parse 1-page PDF");
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn content_builder_roundtrip_parseable() {
        use pdf_core::content::operators::parse_content_stream;

        let mut b = ContentBuilder::new();
        b.save()
            .set_fill_rgb(0.0, 0.5, 1.0)
            .rect(10.0, 10.0, 200.0, 50.0)
            .fill()
            .begin_text()
            .set_font("F1", 12.0)
            .move_text_pos(15.0, 25.0)
            .show_text_str("Test")
            .end_text()
            .restore();
        let bytes = b.build();
        let ops = parse_content_stream(&bytes).unwrap();
        // q rg re f BT Tf Td Tj ET Q → 10 operations
        assert_eq!(ops.len(), 10, "unexpected op count: {:?}", ops);
    }
}

#[cfg(feature = "writer")]
mod editor_tests {
    use pdf_core::editor::{
        add_annotation, add_blank_page, begin_edit_page, delete_page, flatten_all_annotations,
        flatten_annotations, set_metadata, AnnotationBuilder, AnnotationType, MetadataFields,
        PdfEditor,
    };
    use pdf_core::parser::objects::{PdfDocument, PdfObject};
    use std::fs;

    fn fixture(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    // ── Page operations ───────────────────────────────────────────────────────

    #[test]
    fn incremental_add_page_parseable() {
        let original = fixture("minimal.pdf");
        let before = PdfDocument::parse(original.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let result = editor.save_append(&original).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), before + 1);
    }

    #[test]
    fn incremental_add_two_pages() {
        let original = fixture("minimal.pdf");
        let before = PdfDocument::parse(original.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        add_blank_page(&mut editor, 1, 595.0, 842.0).unwrap();
        let result = editor.save_append(&original).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), before + 2);
    }

    #[test]
    fn incremental_delete_page() {
        let original = fixture("minimal.pdf");
        let before = PdfDocument::parse(original.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        add_blank_page(&mut editor, 1, 595.0, 842.0).unwrap();
        delete_page(&mut editor, 0).unwrap();
        let result = editor.save_append(&original).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), before + 1);
    }

    #[test]
    fn incremental_add_content_layer() {
        let original = fixture("minimal.pdf");
        let before = PdfDocument::parse(original.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        // Draw on existing page 0 (present in minimal.pdf)
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
        let doc = PdfDocument::parse(result).unwrap();
        // Page count must not change — we only added content, not a page
        assert_eq!(doc.page_count().unwrap(), before);
    }

    #[test]
    fn result_starts_with_original_bytes() {
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let result = editor.save_append(&original).unwrap();
        assert!(
            result.starts_with(&original),
            "original bytes must be preserved at start"
        );
    }

    // ── Annotation operations ─────────────────────────────────────────────────

    #[test]
    fn incremental_add_highlight_annotation() {
        super::ensure_pro_license();
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let annot = AnnotationBuilder::new(
            AnnotationType::Highlight {
                color: [1.0, 1.0, 0.0],
                quad_points: vec![10.0, 700.0, 200.0, 700.0, 10.0, 712.0, 200.0, 712.0],
            },
            [10.0, 700.0, 200.0, 712.0],
        );
        add_annotation(&mut editor, 0, annot).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn incremental_add_link_annotation() {
        super::ensure_pro_license();
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let annot = AnnotationBuilder::new(
            AnnotationType::Link {
                uri: "https://example.com".to_owned(),
            },
            [10.0, 10.0, 200.0, 30.0],
        );
        add_annotation(&mut editor, 0, annot).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn incremental_set_metadata() {
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        set_metadata(
            &mut editor,
            &MetadataFields {
                title: Some("Integration Test"),
                author: Some("Claude"),
                subject: None,
                keywords: None,
                creator: None,
                producer: Some("pdf-core"),
                mod_date: "D:20260523120000",
            },
        )
        .unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    // ── Annotation flatten ────────────────────────────────────────────────────

    #[test]
    fn flatten_highlight_removes_annots_key() {
        super::ensure_pro_license();
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let annot = AnnotationBuilder::new(
            AnnotationType::Highlight {
                color: [1.0, 1.0, 0.0],
                quad_points: vec![100.0, 700.0, 200.0, 700.0, 100.0, 720.0, 200.0, 720.0],
            },
            [100.0, 700.0, 200.0, 720.0],
        );
        add_annotation(&mut editor, 0, annot).unwrap();
        flatten_annotations(&mut editor, 0).unwrap();

        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        match page_dict.get("Annots") {
            None => {}
            Some(PdfObject::Array(a)) => assert!(a.is_empty(), "/Annots must be empty"),
            _ => panic!("unexpected /Annots value after flatten"),
        }
    }

    #[test]
    fn flatten_produces_parseable_pdf() {
        super::ensure_pro_license();
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let annot = AnnotationBuilder::new(
            AnnotationType::StrikeOut {
                color: [1.0, 0.0, 0.0],
                quad_points: vec![50.0, 710.0, 300.0, 710.0, 50.0, 724.0, 300.0, 724.0],
            },
            [50.0, 710.0, 300.0, 724.0],
        );
        add_annotation(&mut editor, 0, annot).unwrap();
        flatten_annotations(&mut editor, 0).unwrap();
        let saved = editor.save_append(&original).unwrap();
        assert!(PdfDocument::parse(saved).is_ok());
    }

    #[test]
    fn flatten_all_on_page_with_no_annots_is_noop() {
        super::ensure_pro_license();
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        // No annotations added — should succeed without error.
        flatten_all_annotations(&mut editor).unwrap();
        let saved = editor.save_append(&original).unwrap();
        let doc = PdfDocument::parse(saved).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn flatten_ink_annotation_produces_parseable_pdf() {
        super::ensure_pro_license();
        let original = fixture("minimal.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        let annot = AnnotationBuilder::new(
            AnnotationType::Ink {
                ink_list: vec![vec![[10.0, 10.0], [50.0, 80.0], [100.0, 30.0]]],
            },
            [10.0, 10.0, 100.0, 80.0],
        );
        add_annotation(&mut editor, 0, annot).unwrap();
        flatten_all_annotations(&mut editor).unwrap();
        let saved = editor.save_append(&original).unwrap();
        assert!(PdfDocument::parse(saved).is_ok());
    }

    // ── Multipage fixture ─────────────────────────────────────────────────────

    #[test]
    fn edit_multipage_preserves_page_count() {
        let original = fixture("multipage.pdf");
        let mut editor = PdfEditor::open(original.clone()).unwrap();
        let before = editor.page_count().unwrap();
        // Add a content layer to page 0 without adding pages.
        {
            let mut layer = begin_edit_page(&editor, 0).unwrap();
            layer
                .builder
                .save()
                .set_stroke_gray(0.0)
                .set_line_width(2.0)
                .rect(5.0, 5.0, 50.0, 50.0)
                .stroke()
                .restore();
            layer.commit(&mut editor).unwrap();
        }
        let result = editor.save_append(&original).unwrap();
        let doc = PdfDocument::parse(result).unwrap();
        assert_eq!(doc.page_count().unwrap(), before);
    }
}

#[cfg(feature = "forms")]
mod forms_tests {
    use pdf_core::forms::{build_checkbox, build_text_field, AcroFormBuilder};
    use pdf_core::parser::objects::PdfObject;
    use pdf_core::writer::PdfWriter;

    // ── Phase 1: reader / filler integration tests ────────────────────────────

    #[test]
    fn form_fields_read_from_fixture_pdf() {
        let data = include_bytes!("fixtures/form.pdf").to_vec();
        let doc = pdf_core::parser::objects::PdfDocument::parse(data).unwrap();
        let fields = pdf_core::forms::read_form_fields(&doc).unwrap();
        assert!(!fields.is_empty(), "fixture must have at least one field");
        let has_text = fields
            .iter()
            .any(|f| f.field_type == pdf_core::forms::FieldType::Text);
        assert!(has_text, "fixture must contain a text field");
    }

    #[test]
    fn form_set_text_field_round_trips() {
        super::ensure_pro_license();
        let data = include_bytes!("fixtures/form.pdf").to_vec();
        let mut editor = pdf_core::editor::PdfEditor::open(data).unwrap();
        let fields = pdf_core::forms::read_form_fields(&editor.doc).unwrap();
        let text_field = fields
            .iter()
            .find(|f| f.field_type == pdf_core::forms::FieldType::Text)
            .unwrap()
            .clone();
        pdf_core::forms::set_text_field(&mut editor, &text_field, "Hello World").unwrap();

        // Save and re-parse.
        let original = include_bytes!("fixtures/form.pdf").to_vec();
        let saved = editor.save_append(&original).unwrap();
        let doc2 = pdf_core::parser::objects::PdfDocument::parse(saved).unwrap();
        let fields2 = pdf_core::forms::read_form_fields(&doc2).unwrap();
        let updated = fields2
            .iter()
            .find(|f| f.full_name == text_field.full_name)
            .unwrap();
        assert_eq!(updated.value, "Hello World");
    }

    #[test]
    fn form_set_checkbox_round_trips() {
        super::ensure_pro_license();
        let data = include_bytes!("fixtures/form.pdf").to_vec();
        let mut editor = pdf_core::editor::PdfEditor::open(data).unwrap();
        let fields = pdf_core::forms::read_form_fields(&editor.doc).unwrap();
        let cb = fields
            .iter()
            .find(|f| f.field_type == pdf_core::forms::FieldType::Checkbox)
            .unwrap()
            .clone();
        assert!(!cb.checked, "fixture checkbox should start unchecked");
        pdf_core::forms::set_checkbox(&mut editor, &cb, true).unwrap();

        let original = include_bytes!("fixtures/form.pdf").to_vec();
        let saved = editor.save_append(&original).unwrap();
        let doc2 = pdf_core::parser::objects::PdfDocument::parse(saved).unwrap();
        let fields2 = pdf_core::forms::read_form_fields(&doc2).unwrap();
        let updated = fields2
            .iter()
            .find(|f| f.full_name == cb.full_name)
            .unwrap();
        assert!(updated.checked, "checkbox should now be checked");
    }

    // ── Original write tests ──────────────────────────────────────────────────

    #[test]
    fn acroform_with_text_and_checkbox() {
        let mut writer = PdfWriter::new();
        let text_id = build_text_field(
            "Name",
            [10.0, 700.0, 200.0, 720.0],
            "Default",
            false,
            &mut writer,
        )
        .unwrap();
        let cb_id =
            build_checkbox("Accept", [10.0, 670.0, 22.0, 682.0], false, &mut writer).unwrap();
        let mut form = AcroFormBuilder::new();
        form.add_field(text_id).add_field(cb_id);
        let form_id = form.build(&mut writer);
        let obj = writer.get_object(form_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            match d.get("Fields") {
                Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 2),
                _ => panic!("expected Fields array"),
            }
        }
    }
}
