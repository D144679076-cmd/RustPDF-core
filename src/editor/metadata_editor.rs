//! Document metadata editor — update the `/Info` dictionary.

use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};

use super::document_editor::PdfEditor;

/// Metadata fields to write into a document's `/Info` dictionary.
///
/// Each field is `Option<&str>`: `Some(value)` sets it, `None` leaves it unchanged.
/// `mod_date` must be a PDF date string (e.g. `"D:20260523120000"`) or empty to skip.
pub struct MetadataFields<'a> {
    pub title: Option<&'a str>,
    pub author: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub keywords: Option<&'a str>,
    pub creator: Option<&'a str>,
    pub producer: Option<&'a str>,
    pub mod_date: &'a str,
}

/// Update document metadata fields in the `/Info` dictionary.
///
/// Fields set to `None` in `fields` are left unchanged. Always replaces (or creates)
/// the `/Info` object as part of the incremental update.
pub fn set_metadata(editor: &mut PdfEditor, fields: &MetadataFields<'_>) -> Result<()> {
    let title = fields.title;
    let author = fields.author;
    let subject = fields.subject;
    let keywords = fields.keywords;
    let creator = fields.creator;
    let producer = fields.producer;
    let mod_date = fields.mod_date;
    // Load existing /Info if present, otherwise start fresh.
    let mut info: PdfDict = if let Some(id) = editor.info_id {
        match editor.get_object(id)? {
            PdfObject::Dictionary(d) => d,
            _ => PdfDict::new(),
        }
    } else {
        PdfDict::new()
    };

    // Helper closure to set a string field.
    let mut set_field = |key: &str, val: Option<&str>| {
        if let Some(v) = val {
            info.insert(key.to_owned(), PdfObject::String(v.as_bytes().to_vec()));
        }
    };

    set_field("Title", title);
    set_field("Author", author);
    set_field("Subject", subject);
    set_field("Keywords", keywords);
    set_field("Creator", creator);
    set_field("Producer", producer);

    if !mod_date.is_empty() {
        info.insert(
            "ModDate".to_owned(),
            PdfObject::String(mod_date.as_bytes().to_vec()),
        );
    }

    // Write/replace the Info dict.
    if let Some(id) = editor.info_id {
        editor.replace_object(id, PdfObject::Dictionary(info));
    } else {
        let new_id = editor.add_object(PdfObject::Dictionary(info));
        editor.info_id = Some(new_id);
    }

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
    fn set_metadata_creates_info_dict() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        set_metadata(
            &mut editor,
            &MetadataFields {
                title: Some("My Title"),
                author: Some("Alice"),
                subject: None,
                keywords: None,
                creator: None,
                producer: None,
                mod_date: "",
            },
        )
        .unwrap();
        assert!(editor.info_id.is_some());
    }

    #[test]
    fn set_metadata_updates_existing_fields() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        set_metadata(
            &mut editor,
            &MetadataFields {
                title: Some("Title A"),
                author: None,
                subject: None,
                keywords: None,
                creator: None,
                producer: None,
                mod_date: "",
            },
        )
        .unwrap();
        set_metadata(
            &mut editor,
            &MetadataFields {
                title: Some("Title B"),
                author: None,
                subject: None,
                keywords: None,
                creator: None,
                producer: None,
                mod_date: "",
            },
        )
        .unwrap();

        let id = editor.info_id.unwrap();
        match editor.get_object(id).unwrap() {
            PdfObject::Dictionary(d) => match d.get("Title").unwrap() {
                PdfObject::String(bytes) => {
                    assert_eq!(bytes, b"Title B");
                }
                _ => panic!("expected string"),
            },
            _ => panic!("expected dict"),
        }
    }

    #[test]
    fn set_metadata_save_append_parseable() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        set_metadata(
            &mut editor,
            &MetadataFields {
                title: Some("Test"),
                author: Some("Bob"),
                subject: None,
                keywords: None,
                creator: None,
                producer: None,
                mod_date: "D:20260523",
            },
        )
        .unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn none_fields_do_not_overwrite_existing() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        // Set author first
        set_metadata(
            &mut editor,
            &MetadataFields {
                title: None,
                author: Some("Alice"),
                subject: None,
                keywords: None,
                creator: None,
                producer: None,
                mod_date: "",
            },
        )
        .unwrap();
        // Now set title only — author should still be Alice
        set_metadata(
            &mut editor,
            &MetadataFields {
                title: Some("Doc"),
                author: None,
                subject: None,
                keywords: None,
                creator: None,
                producer: None,
                mod_date: "",
            },
        )
        .unwrap();

        let id = editor.info_id.unwrap();
        match editor.get_object(id).unwrap() {
            PdfObject::Dictionary(d) => {
                let author = d.get("Author");
                assert!(
                    matches!(author, Some(PdfObject::String(_))),
                    "Author should be preserved"
                );
            }
            _ => panic!("expected dict"),
        }
    }
}
