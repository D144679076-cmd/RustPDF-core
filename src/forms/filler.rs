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

// ── Form flattening ───────────────────────────────────────────────────────────

/// Flatten all AcroForm fields on a single page into the content stream.
///
/// Widget annotations are removed from `/Annots`, their visual content is
/// embedded as either a Form XObject (`/AP/N` appearance) or synthesised
/// PDF operators (text / checkbox).  After this call the page fields are no
/// longer interactive.
pub fn flatten_form_fields(editor: &mut PdfEditor, page_index: usize) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "flatten_forms")?;

    let (page_id, page_dict) = editor.get_page_dict(page_index)?;

    // Resolve /Annots — may be a direct array or an indirect reference.
    let annots_obj = match page_dict.get("Annots") {
        Some(PdfObject::Reference(n, _)) => editor.get_object(*n)?,
        Some(other) => other.clone(),
        None => return Ok(()),
    };
    let annots = match annots_obj {
        PdfObject::Array(a) => a,
        _ => return Ok(()),
    };

    let mut widget_ids: Vec<u32> = Vec::new();
    let mut non_widget_annots: Vec<PdfObject> = Vec::new();

    for annot_ref in &annots {
        let annot_id = match annot_ref {
            PdfObject::Reference(n, _) => *n,
            _ => {
                non_widget_annots.push(annot_ref.clone());
                continue;
            }
        };
        let annot_obj = editor.get_object(annot_id)?;
        let annot_dict = match annot_obj {
            PdfObject::Dictionary(ref d) => d.clone(),
            _ => {
                non_widget_annots.push(annot_ref.clone());
                continue;
            }
        };
        match annot_dict.get("Subtype") {
            Some(PdfObject::Name(n)) if n == "Widget" => widget_ids.push(annot_id),
            _ => non_widget_annots.push(annot_ref.clone()),
        }
    }

    if widget_ids.is_empty() {
        return Ok(());
    }

    // Build content operators for each widget.
    let mut content_bytes = Vec::new();
    for &widget_id in &widget_ids {
        let widget_obj = editor.get_object(widget_id)?;
        let widget_dict = match widget_obj {
            PdfObject::Dictionary(d) => d,
            _ => continue,
        };

        let rect = extract_rect(&widget_dict);
        let x = rect[0];
        let y = rect[1];
        let w = rect[2] - rect[0];
        let h = rect[3] - rect[1];

        // Prefer /AP /N appearance stream — embed as Form XObject.
        let ap_n_ref = widget_dict.get("AP").and_then(|ap| {
            if let PdfObject::Dictionary(d) = ap {
                d.get("N").cloned()
            } else {
                None
            }
        });

        if let Some(PdfObject::Reference(ap_id, _)) = ap_n_ref {
            let ap_obj = editor.get_object(ap_id)?;
            if let PdfObject::Stream(_) = ap_obj {
                let xobj_name = format!("WFld{}", widget_id);
                let seg = format!("q {} 0 0 {} {} {} cm /{} Do Q\n", w, h, x, y, xobj_name);
                content_bytes.extend_from_slice(seg.as_bytes());
            }
        } else {
            // Synthesise from field value when no appearance stream exists.
            let field_type = widget_dict
                .get("FT")
                .and_then(|o| {
                    if let PdfObject::Name(n) = o {
                        Some(n.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("");
            let value = widget_dict
                .get("V")
                .and_then(|o| match o {
                    PdfObject::String(b) => Some(String::from_utf8_lossy(b).into_owned()),
                    PdfObject::Name(n) => Some(n.clone()),
                    _ => None,
                })
                .unwrap_or_default();

            let seg = match field_type {
                "Tx" => {
                    let font_size = (h * 0.7).clamp(6.0, 12.0);
                    format!(
                        "q BT /Helv {} Tf {} {} Td ({}) Tj ET Q\n",
                        font_size,
                        x + 2.0,
                        y + (h - font_size) / 2.0,
                        escape_pdf_string(&value),
                    )
                }
                "Btn" => {
                    let as_state = widget_dict
                        .get("AS")
                        .and_then(|o| {
                            if let PdfObject::Name(n) = o {
                                Some(n.clone())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    let checked = as_state != "Off" && !as_state.is_empty();
                    if checked {
                        let cx = x + w / 2.0;
                        let cy = y + h / 2.0;
                        format!(
                            "q 0 0 0 RG 1.5 w {} {} m {} {} l {} {} l S Q\n",
                            cx - w * 0.3,
                            cy,
                            cx - w * 0.1,
                            cy - h * 0.2,
                            cx + w * 0.3,
                            cy + h * 0.3,
                        )
                    } else {
                        String::new()
                    }
                }
                _ => String::new(),
            };
            content_bytes.extend_from_slice(seg.as_bytes());
        }
    }

    // Append synthesised content stream to page (even if empty we still remove
    // the widget annotations from /Annots and /AcroForm /Fields).
    let mut updated_page = page_dict.clone();

    if !content_bytes.is_empty() {
        let stream = make_flate_stream(&content_bytes, PdfDict::new())?;
        let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

        // Register AP stream objects as Form XObjects in page resources.
        for &widget_id in &widget_ids {
            if let Ok(PdfObject::Dictionary(wd)) = editor.get_object(widget_id) {
                if let Some(PdfObject::Dictionary(ap)) = wd.get("AP") {
                    if let Some(PdfObject::Reference(ap_id, _)) = ap.get("N") {
                        let xobj_name = format!("WFld{}", widget_id);
                        register_xobject_in_page(&mut updated_page, &xobj_name, *ap_id);
                    }
                }
            }
        }

        let new_contents = match updated_page.get("Contents") {
            Some(PdfObject::Array(arr)) => {
                let mut a = arr.clone();
                a.push(PdfObject::Reference(stream_id, 0));
                PdfObject::Array(a)
            }
            Some(single) => {
                PdfObject::Array(vec![single.clone(), PdfObject::Reference(stream_id, 0)])
            }
            None => PdfObject::Reference(stream_id, 0),
        };
        updated_page.insert("Contents".to_owned(), new_contents);
    }

    // Strip widget annotations from /Annots.
    if non_widget_annots.is_empty() {
        updated_page.shift_remove("Annots");
    } else {
        updated_page.insert("Annots".to_owned(), PdfObject::Array(non_widget_annots));
    }

    editor.replace_object(page_id, PdfObject::Dictionary(updated_page));

    // Remove flattened fields from AcroForm /Fields.
    remove_fields_from_acroform(editor, &widget_ids)?;

    Ok(())
}

/// Flatten all AcroForm fields across every page.
///
/// Calls [`flatten_form_fields`] for each page in order.  On completion the
/// document contains no interactive Widget annotations.
pub fn flatten_all_form_fields(editor: &mut PdfEditor) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "flatten_forms")?;

    let page_count = editor.page_count()?;
    for i in 0..page_count {
        flatten_form_fields(editor, i)?;
    }
    Ok(())
}

// ── Private helpers (flatten) ─────────────────────────────────────────────────

fn extract_rect(dict: &PdfDict) -> [f64; 4] {
    match dict.get("Rect") {
        Some(PdfObject::Array(a)) if a.len() == 4 => {
            let n: Vec<f64> = a
                .iter()
                .filter_map(|x| match x {
                    PdfObject::Real(r) => Some(*r),
                    PdfObject::Integer(i) => Some(*i as f64),
                    _ => None,
                })
                .collect();
            if n.len() == 4 {
                [n[0], n[1], n[2], n[3]]
            } else {
                [0.0; 4]
            }
        }
        _ => [0.0; 4],
    }
}

fn escape_pdf_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn register_xobject_in_page(page_dict: &mut PdfDict, name: &str, obj_id: u32) {
    let resources = page_dict
        .entry("Resources".to_owned())
        .or_insert(PdfObject::Dictionary(PdfDict::new()));
    if let PdfObject::Dictionary(res) = resources {
        let xobjs = res
            .entry("XObject".to_owned())
            .or_insert(PdfObject::Dictionary(PdfDict::new()));
        if let PdfObject::Dictionary(xobj_dict) = xobjs {
            xobj_dict.insert(name.to_owned(), PdfObject::Reference(obj_id, 0));
        }
    }
}

fn remove_fields_from_acroform(editor: &mut PdfEditor, widget_ids: &[u32]) -> Result<()> {
    let root_id = match editor.doc.trailer.get("Root") {
        Some(PdfObject::Reference(n, _)) => *n,
        _ => return Ok(()),
    };
    let root_obj = editor.get_object(root_id)?;
    let root_dict = match root_obj {
        PdfObject::Dictionary(d) => d,
        _ => return Ok(()),
    };
    let acroform_id = match root_dict.get("AcroForm") {
        Some(PdfObject::Reference(n, _)) => *n,
        _ => return Ok(()),
    };
    let acroform_obj = editor.get_object(acroform_id)?;
    let mut acroform_dict = match acroform_obj {
        PdfObject::Dictionary(d) => d,
        _ => return Ok(()),
    };
    let fields = match acroform_dict.get("Fields") {
        Some(PdfObject::Array(a)) => a.clone(),
        _ => return Ok(()),
    };
    let new_fields: Vec<PdfObject> = fields
        .into_iter()
        .filter(|f| match f {
            PdfObject::Reference(n, _) => !widget_ids.contains(n),
            _ => true,
        })
        .collect();
    acroform_dict.insert("Fields".to_owned(), PdfObject::Array(new_fields));
    editor.replace_object(acroform_id, PdfObject::Dictionary(acroform_dict));
    Ok(())
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

    // ── flatten helpers ───────────────────────────────────────────────────────

    /// Build a minimal single-page PDF that has one Widget annotation with a
    /// synthesised text-field value (no /AP stream) so flatten can be tested
    /// without a real form.pdf fixture.
    fn make_pdf_with_widget() -> Vec<u8> {
        use crate::writer::document::PdfWriter;

        let mut w = PdfWriter::new();

        // Widget dict — text field, no /AP stream.
        let mut wd = PdfDict::new();
        wd.insert("Type".to_owned(), PdfObject::Name("Annot".to_owned()));
        wd.insert("Subtype".to_owned(), PdfObject::Name("Widget".to_owned()));
        wd.insert("FT".to_owned(), PdfObject::Name("Tx".to_owned()));
        wd.insert("T".to_owned(), PdfObject::String(b"Name".to_vec()));
        wd.insert("V".to_owned(), PdfObject::String(b"Alice".to_vec()));
        wd.insert(
            "Rect".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Real(50.0),
                PdfObject::Real(700.0),
                PdfObject::Real(250.0),
                PdfObject::Real(720.0),
            ]),
        );
        let widget_id = w.add_object(PdfObject::Dictionary(wd));

        // AcroForm pointing at the widget.
        let mut acroform = PdfDict::new();
        acroform.insert(
            "Fields".to_owned(),
            PdfObject::Array(vec![PdfObject::Reference(widget_id, 0)]),
        );
        let acroform_id = w.add_object(PdfObject::Dictionary(acroform));

        // Content stream (empty page).
        let content_id = w.add_object(PdfObject::Stream(Box::new(
            crate::writer::streams::make_raw_stream(b"".to_vec(), PdfDict::new()),
        )));

        // Page dict referencing the widget in /Annots.
        let mut page = PdfDict::new();
        page.insert("Type".to_owned(), PdfObject::Name("Page".to_owned()));
        page.insert(
            "MediaBox".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(612),
                PdfObject::Integer(792),
            ]),
        );
        page.insert("Contents".to_owned(), PdfObject::Reference(content_id, 0));
        page.insert(
            "Annots".to_owned(),
            PdfObject::Array(vec![PdfObject::Reference(widget_id, 0)]),
        );
        let page_id = w.add_object(PdfObject::Dictionary(page));

        // Pages node.
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert(
            "Kids".to_owned(),
            PdfObject::Array(vec![PdfObject::Reference(page_id, 0)]),
        );
        pages.insert("Count".to_owned(), PdfObject::Integer(1));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));

        // Patch parent refs.
        if let Ok(PdfObject::Dictionary(mut pd)) = {
            // re-fetch page to patch /Parent
            let _ = page_id; // already added; mutate via replace
            Ok::<_, ()>(PdfObject::Dictionary(PdfDict::new()))
        } {
            pd.insert("Parent".to_owned(), PdfObject::Reference(pages_id, 0));
        }

        // Catalog.
        let mut cat = PdfDict::new();
        cat.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        cat.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        cat.insert("AcroForm".to_owned(), PdfObject::Reference(acroform_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(cat));

        w.serialize_all(cat_id, None, None).unwrap()
    }

    #[test]
    fn flatten_form_fields_removes_widget_annots() {
        let data = make_pdf_with_widget();
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        flatten_form_fields(&mut editor, 0).unwrap();

        // Page /Annots must be absent or contain no Widget entries.
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        if let Some(PdfObject::Array(annots)) = page_dict.get("Annots") {
            for a in annots {
                if let PdfObject::Reference(id, _) = a {
                    if let Ok(PdfObject::Dictionary(d)) = editor.get_object(*id) {
                        assert_ne!(
                            d.get("Subtype"),
                            Some(&PdfObject::Name("Widget".to_owned()))
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn flatten_form_fields_appends_content_stream() {
        let data = make_pdf_with_widget();
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        flatten_form_fields(&mut editor, 0).unwrap();

        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        // /Contents must be an array with ≥2 entries (original + flattened).
        match page_dict.get("Contents") {
            Some(PdfObject::Array(a)) => assert!(a.len() >= 2),
            _ => panic!("expected Contents array after flatten"),
        }
    }

    #[test]
    fn flatten_all_form_fields_produces_parseable_pdf() {
        let data = make_pdf_with_widget();
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        flatten_all_form_fields(&mut editor).unwrap();
        let saved = editor.save_new().unwrap();
        assert!(crate::parser::objects::PdfDocument::parse(saved).is_ok());
    }

    #[test]
    fn flatten_page_with_no_annots_is_noop() {
        // A PDF with no /Annots must not error.
        use crate::writer::document::PdfWriter;
        let mut w = PdfWriter::new();
        let content_id = w.add_object(PdfObject::Stream(Box::new(
            crate::writer::streams::make_raw_stream(b"".to_vec(), PdfDict::new()),
        )));
        let mut page = PdfDict::new();
        page.insert("Type".to_owned(), PdfObject::Name("Page".to_owned()));
        page.insert(
            "MediaBox".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(612),
                PdfObject::Integer(792),
            ]),
        );
        page.insert("Contents".to_owned(), PdfObject::Reference(content_id, 0));
        let page_id = w.add_object(PdfObject::Dictionary(page));
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert(
            "Kids".to_owned(),
            PdfObject::Array(vec![PdfObject::Reference(page_id, 0)]),
        );
        pages.insert("Count".to_owned(), PdfObject::Integer(1));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));
        let mut cat = PdfDict::new();
        cat.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        cat.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(cat));
        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let mut editor = crate::editor::PdfEditor::open(bytes).unwrap();
        assert!(flatten_form_fields(&mut editor, 0).is_ok());
    }
}
