//! Write-back helpers for interactive form fields (ISO 32000-1 §12.7).
//!
//! Each function mutates the corresponding field dictionary in the editor's
//! copy-on-write pool and regenerates the visual appearance stream.

use crate::editor::PdfEditor;
use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::streams::make_flate_stream;

use super::appearance;
use super::reader::FormField;

// ── Text field ────────────────────────────────────────────────────────────────

/// Update a text field's `/V` value and regenerate its appearance stream.
///
/// `field` must be a [`FieldType::Text`] leaf from [`super::read_form_fields`].
/// The new appearance is a single-line content stream using `/Helv` (Helvetica).
pub fn set_text_field(editor: &mut PdfEditor, field: &FormField, value: &str) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "set_text_field")?;
    let mut field_dict = get_field_dict(editor, field.id)?;

    field_dict.insert("V".to_owned(), PdfObject::String(value.as_bytes().to_vec()));

    let ap_bytes = appearance::text_field_appearance(value, field.rect, field.max_len);
    let ap_id = add_form_xobject(editor, &ap_bytes, field.rect)?;

    set_normal_appearance(&mut field_dict, PdfObject::Reference(ap_id, 0));
    editor.replace_object(field.id, PdfObject::Dictionary(field_dict));
    Ok(())
}

// ── Checkbox ──────────────────────────────────────────────────────────────────

/// Update a checkbox field's state and regenerate its appearance streams.
///
/// Sets `/V` and `/AS` to `"Yes"` (checked) or `"Off"` (unchecked).
/// Both the `Yes` and `Off` appearance streams are (re-)written so the field
/// displays correctly even when the viewer does not regenerate appearances.
pub fn set_checkbox(editor: &mut PdfEditor, field: &FormField, checked: bool) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "set_checkbox")?;
    let mut field_dict = get_field_dict(editor, field.id)?;

    let state = if checked { "Yes" } else { "Off" };
    field_dict.insert("V".to_owned(), PdfObject::Name(state.to_owned()));
    field_dict.insert("AS".to_owned(), PdfObject::Name(state.to_owned()));

    let yes_bytes = appearance::checkbox_appearance(field.rect, true);
    let off_bytes = appearance::checkbox_appearance(field.rect, false);
    let yes_id = add_form_xobject(editor, &yes_bytes, field.rect)?;
    let off_id = add_form_xobject(editor, &off_bytes, field.rect)?;

    let mut n_dict = PdfDict::new();
    n_dict.insert("Yes".to_owned(), PdfObject::Reference(yes_id, 0));
    n_dict.insert("Off".to_owned(), PdfObject::Reference(off_id, 0));
    let mut ap_dict = PdfDict::new();
    ap_dict.insert("N".to_owned(), PdfObject::Dictionary(n_dict));
    field_dict.insert("AP".to_owned(), PdfObject::Dictionary(ap_dict));

    editor.replace_object(field.id, PdfObject::Dictionary(field_dict));
    Ok(())
}

// ── Combo / List ──────────────────────────────────────────────────────────────

/// Update a combo box or list box field's selected value.
///
/// Sets `/V` to `selected_value` and, when the value is found in `field.options`,
/// also updates `/I` to the matching index so the viewer highlights the right row.
pub fn set_combo_or_list(
    editor: &mut PdfEditor,
    field: &FormField,
    selected_value: &str,
) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "set_combo_or_list")?;
    let mut field_dict = get_field_dict(editor, field.id)?;

    field_dict.insert(
        "V".to_owned(),
        PdfObject::String(selected_value.as_bytes().to_vec()),
    );
    if let Some(idx) = field.options.iter().position(|o| o == selected_value) {
        field_dict.insert(
            "I".to_owned(),
            PdfObject::Array(vec![PdfObject::Integer(idx as i64)]),
        );
    }

    editor.replace_object(field.id, PdfObject::Dictionary(field_dict));
    Ok(())
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Retrieve the field dictionary from the editor (writer pool first, then doc).
fn get_field_dict(editor: &PdfEditor, id: u32) -> Result<PdfDict> {
    match editor.get_object(id)? {
        PdfObject::Dictionary(d) => Ok(d),
        _ => Err(crate::error::PdfError::invalid_structure(
            "field object is not a dict",
        )),
    }
}

/// Compress `content_bytes` and add it as a Form XObject to the writer pool.
///
/// Returns the new object ID.  The BBox is derived from the field's `rect`
/// (translated to origin 0,0 so the stream draws in its own local space).
fn add_form_xobject(editor: &mut PdfEditor, content_bytes: &[u8], rect: [f64; 4]) -> Result<u32> {
    let w = rect[2] - rect[0];
    let h = rect[3] - rect[1];
    let mut extra = PdfDict::new();
    extra.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
    extra.insert(
        "BBox".to_owned(),
        PdfObject::Array(vec![
            PdfObject::Real(0.0),
            PdfObject::Real(0.0),
            PdfObject::Real(w),
            PdfObject::Real(h),
        ]),
    );
    let stream = make_flate_stream(content_bytes, extra)?;
    Ok(editor.add_object(PdfObject::Stream(Box::new(stream))))
}

/// Set `field_dict["AP"]["N"]` to a single appearance stream reference.
///
/// This is the normal-appearance value used for single-state fields (text).
fn set_normal_appearance(field_dict: &mut PdfDict, ap_ref: PdfObject) {
    let mut n_dict = PdfDict::new();
    n_dict.insert("N".to_owned(), ap_ref);
    field_dict.insert("AP".to_owned(), PdfObject::Dictionary(n_dict));
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forms::reader::{FieldType, FormField};

    fn dummy_text_field(id: u32) -> FormField {
        FormField {
            id,
            name: "Name".to_owned(),
            full_name: "Name".to_owned(),
            field_type: FieldType::Text,
            value: String::new(),
            default_value: String::new(),
            rect: [10.0, 700.0, 200.0, 720.0],
            page_index: 0,
            options: vec![],
            checked: false,
            readonly: false,
            required: false,
            multiline: false,
            max_len: None,
        }
    }

    fn dummy_checkbox_field(id: u32) -> FormField {
        FormField {
            id,
            field_type: FieldType::Checkbox,
            rect: [10.0, 680.0, 22.0, 692.0],
            ..dummy_text_field(id)
        }
    }

    fn make_editor_with_field(field_id: u32) -> crate::editor::PdfEditor {
        use crate::parser::objects::PdfDict;
        use crate::writer::document::PdfWriter;

        let mut w = PdfWriter::new_from_max_id(field_id);

        // Add the field dict at the desired ID.
        let mut fd = PdfDict::new();
        fd.insert("FT".to_owned(), PdfObject::Name("Tx".to_owned()));
        fd.insert("T".to_owned(), PdfObject::String(b"Name".to_vec()));
        fd.insert(
            "Rect".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Real(10.0),
                PdfObject::Real(700.0),
                PdfObject::Real(200.0),
                PdfObject::Real(720.0),
            ]),
        );
        w.set_object(field_id, PdfObject::Dictionary(fd));

        // Build a tiny valid PDF around it.
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));
        let mut cat = PdfDict::new();
        cat.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        cat.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(cat));
        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let base_editor = crate::editor::PdfEditor::open(bytes).unwrap();

        // Restore the field dict into the new editor's writer pool.
        let mut fd2 = PdfDict::new();
        fd2.insert("FT".to_owned(), PdfObject::Name("Tx".to_owned()));
        fd2.insert("T".to_owned(), PdfObject::String(b"Name".to_vec()));
        fd2.insert(
            "Rect".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Real(10.0),
                PdfObject::Real(700.0),
                PdfObject::Real(200.0),
                PdfObject::Real(720.0),
            ]),
        );
        let mut editor = base_editor;
        editor.replace_object(field_id, PdfObject::Dictionary(fd2));
        editor
    }

    #[test]
    fn set_text_field_updates_v_and_adds_ap() {
        let field_id = 10;
        let mut editor = make_editor_with_field(field_id);
        let field = dummy_text_field(field_id);
        set_text_field(&mut editor, &field, "Hello World").unwrap();

        let updated = editor.get_object(field_id).unwrap();
        let d = updated.as_dict().unwrap();
        assert_eq!(
            d.get("V"),
            Some(&PdfObject::String(b"Hello World".to_vec()))
        );
        assert!(d.contains_key("AP"));
    }

    #[test]
    fn set_checkbox_checked_sets_yes_state() {
        let field_id = 11;
        let mut editor = make_editor_with_field(field_id);
        let field = dummy_checkbox_field(field_id);
        set_checkbox(&mut editor, &field, true).unwrap();

        let updated = editor.get_object(field_id).unwrap();
        let d = updated.as_dict().unwrap();
        assert_eq!(d.get("V"), Some(&PdfObject::Name("Yes".to_owned())));
        assert_eq!(d.get("AS"), Some(&PdfObject::Name("Yes".to_owned())));
    }

    #[test]
    fn set_checkbox_unchecked_sets_off_state() {
        let field_id = 12;
        let mut editor = make_editor_with_field(field_id);
        let field = dummy_checkbox_field(field_id);
        set_checkbox(&mut editor, &field, false).unwrap();

        let updated = editor.get_object(field_id).unwrap();
        let d = updated.as_dict().unwrap();
        assert_eq!(d.get("V"), Some(&PdfObject::Name("Off".to_owned())));
    }

    #[test]
    fn set_combo_or_list_updates_v_and_i() {
        let field_id = 13;
        let mut editor = make_editor_with_field(field_id);
        let mut field = dummy_text_field(field_id);
        field.field_type = FieldType::Combo;
        field.options = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        set_combo_or_list(&mut editor, &field, "b").unwrap();

        let updated = editor.get_object(field_id).unwrap();
        let d = updated.as_dict().unwrap();
        assert_eq!(d.get("V"), Some(&PdfObject::String(b"b".to_vec())));
        assert_eq!(
            d.get("I"),
            Some(&PdfObject::Array(vec![PdfObject::Integer(1)]))
        );
    }
}
