//! AcroForm (interactive form) field builders (ISO 32000-1 §12.7).

use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::document::PdfWriter;
use crate::writer::streams::make_flate_stream;

use super::appearance::checkbox_appearance;

// ── AcroForm container ────────────────────────────────────────────────────────

/// Builder for the top-level AcroForm dictionary.
pub struct AcroFormBuilder {
    /// Field object IDs collected via [`add_field`](Self::add_field).
    field_ids: Vec<u32>,
    /// If `true`, the viewer should regenerate appearances on open.
    need_appearances: bool,
}

impl AcroFormBuilder {
    /// Create an empty AcroForm builder.
    pub fn new() -> Self {
        Self {
            field_ids: Vec::new(),
            need_appearances: true,
        }
    }

    /// Register a field object ID with the form.
    pub fn add_field(&mut self, field_id: u32) -> &mut Self {
        self.field_ids.push(field_id);
        self
    }

    /// Write the AcroForm dictionary object and return its ID.
    pub fn build(self, writer: &mut PdfWriter) -> u32 {
        let mut dict = PdfDict::new();
        dict.insert(
            "Fields".to_owned(),
            PdfObject::Array(
                self.field_ids
                    .iter()
                    .map(|&id| PdfObject::Reference(id, 0))
                    .collect(),
            ),
        );
        dict.insert(
            "NeedAppearances".to_owned(),
            PdfObject::Boolean(self.need_appearances),
        );
        writer.add_object(PdfObject::Dictionary(dict))
    }
}

impl Default for AcroFormBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ── Text field ────────────────────────────────────────────────────────────────

/// Write a single-line or multi-line text input field.
///
/// `name` is the fully qualified field name (`/T`).
/// `rect` is `[x1, y1, x2, y2]` in default user space.
///
/// Returns the field widget annotation object ID.
pub fn build_text_field(
    name: &str,
    rect: [f64; 4],
    default_value: &str,
    multiline: bool,
    writer: &mut PdfWriter,
) -> Result<u32> {
    let mut d = PdfDict::new();

    // Combined Widget + Field entry (the common terminal-field approach).
    d.insert("Type".to_owned(), PdfObject::Name("Annot".to_owned()));
    d.insert("Subtype".to_owned(), PdfObject::Name("Widget".to_owned()));
    d.insert("FT".to_owned(), PdfObject::Name("Tx".to_owned()));
    d.insert("T".to_owned(), PdfObject::String(name.as_bytes().to_vec()));
    d.insert(
        "Rect".to_owned(),
        PdfObject::Array(rect.iter().map(|&v| PdfObject::Real(v)).collect()),
    );

    // Flags: bit 13 (Multiline) = 0x1000
    let flags: i64 = if multiline { 0x1000 } else { 0 };
    d.insert("Ff".to_owned(), PdfObject::Integer(flags));

    if !default_value.is_empty() {
        d.insert(
            "V".to_owned(),
            PdfObject::String(default_value.as_bytes().to_vec()),
        );
        d.insert(
            "DV".to_owned(),
            PdfObject::String(default_value.as_bytes().to_vec()),
        );
    }

    // Default appearance string.
    d.insert(
        "DA".to_owned(),
        PdfObject::String(b"/Helvetica 12 Tf 0 g".to_vec()),
    );

    // Annot flags: Print (4)
    d.insert("Flags".to_owned(), PdfObject::Integer(4));

    Ok(writer.add_object(PdfObject::Dictionary(d)))
}

// ── Checkbox ──────────────────────────────────────────────────────────────────

/// Write a checkbox field.
///
/// The `checked` parameter sets the initial state.
///
/// Returns the field widget annotation object ID.
pub fn build_checkbox(
    name: &str,
    rect: [f64; 4],
    checked: bool,
    writer: &mut PdfWriter,
) -> Result<u32> {
    let state_name = if checked { "Yes" } else { "Off" };

    // Build appearance streams for On and Off states.
    let on_bytes = checkbox_appearance(rect, true);
    let off_bytes = checkbox_appearance(rect, false);

    let mut ap_stream_extras = PdfDict::new();
    ap_stream_extras.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
    ap_stream_extras.insert(
        "BBox".to_owned(),
        PdfObject::Array(vec![
            PdfObject::Real(0.0),
            PdfObject::Real(0.0),
            PdfObject::Real(rect[2] - rect[0]),
            PdfObject::Real(rect[3] - rect[1]),
        ]),
    );

    let on_stream = make_flate_stream(&on_bytes, ap_stream_extras.clone())?;
    let off_stream = make_flate_stream(&off_bytes, ap_stream_extras)?;

    let on_id = writer.add_object(PdfObject::Stream(Box::new(on_stream)));
    let off_id = writer.add_object(PdfObject::Stream(Box::new(off_stream)));

    // Appearance dict
    let mut ap_n = PdfDict::new();
    ap_n.insert("Yes".to_owned(), PdfObject::Reference(on_id, 0));
    ap_n.insert("Off".to_owned(), PdfObject::Reference(off_id, 0));
    let mut ap = PdfDict::new();
    ap.insert("N".to_owned(), PdfObject::Dictionary(ap_n));

    let mut d = PdfDict::new();
    d.insert("Type".to_owned(), PdfObject::Name("Annot".to_owned()));
    d.insert("Subtype".to_owned(), PdfObject::Name("Widget".to_owned()));
    d.insert("FT".to_owned(), PdfObject::Name("Btn".to_owned()));
    d.insert("T".to_owned(), PdfObject::String(name.as_bytes().to_vec()));
    d.insert(
        "Rect".to_owned(),
        PdfObject::Array(rect.iter().map(|&v| PdfObject::Real(v)).collect()),
    );
    d.insert("Ff".to_owned(), PdfObject::Integer(0));
    d.insert("V".to_owned(), PdfObject::Name(state_name.to_owned()));
    d.insert("AS".to_owned(), PdfObject::Name(state_name.to_owned()));
    d.insert("AP".to_owned(), PdfObject::Dictionary(ap));
    d.insert("Flags".to_owned(), PdfObject::Integer(4));

    Ok(writer.add_object(PdfObject::Dictionary(d)))
}

// ── Combo box (dropdown) ─────────────────────────────────────────────────────

/// Combo field flag: bit 18 (0x20000).
const FF_COMBO: i64 = 0x20000;

/// Write a combo box (dropdown) field.
///
/// `options` is a slice of `(export_value, display_string)` pairs.
/// `selected` is the 0-based index of the initially selected option, or `None`.
///
/// Returns the field widget annotation object ID.
pub fn build_combo_field(
    name: &str,
    rect: [f64; 4],
    options: &[(&str, &str)],
    selected: Option<usize>,
    writer: &mut PdfWriter,
) -> Result<u32> {
    build_choice_field(name, rect, options, selected, false, true, writer)
}

// ── List box ─────────────────────────────────────────────────────────────────

/// MultiSelect field flag: bit 22 (0x200000).
const FF_MULTI_SELECT: i64 = 0x200000;

/// Write a list box field.
///
/// Unlike a combo box, a list box displays all options simultaneously.
/// When `multi_select` is true, the user can select multiple items.
///
/// Returns the field widget annotation object ID.
pub fn build_list_field(
    name: &str,
    rect: [f64; 4],
    options: &[(&str, &str)],
    selected: Option<usize>,
    multi_select: bool,
    writer: &mut PdfWriter,
) -> Result<u32> {
    build_choice_field(name, rect, options, selected, multi_select, false, writer)
}

/// Shared builder for combo and list choice fields (/FT /Ch).
fn build_choice_field(
    name: &str,
    rect: [f64; 4],
    options: &[(&str, &str)],
    selected: Option<usize>,
    multi_select: bool,
    is_combo: bool,
    writer: &mut PdfWriter,
) -> Result<u32> {
    let mut d = PdfDict::new();

    d.insert("Type".to_owned(), PdfObject::Name("Annot".to_owned()));
    d.insert("Subtype".to_owned(), PdfObject::Name("Widget".to_owned()));
    d.insert("FT".to_owned(), PdfObject::Name("Ch".to_owned()));
    d.insert("T".to_owned(), PdfObject::String(name.as_bytes().to_vec()));
    d.insert(
        "Rect".to_owned(),
        PdfObject::Array(rect.iter().map(|&v| PdfObject::Real(v)).collect()),
    );

    let mut flags: i64 = 0;
    if is_combo {
        flags |= FF_COMBO;
    }
    if multi_select {
        flags |= FF_MULTI_SELECT;
    }
    d.insert("Ff".to_owned(), PdfObject::Integer(flags));

    // /Opt array: each element is [export_value, display_string]
    let opt_array: Vec<PdfObject> = options
        .iter()
        .map(|(export, display)| {
            PdfObject::Array(vec![
                PdfObject::String(export.as_bytes().to_vec()),
                PdfObject::String(display.as_bytes().to_vec()),
            ])
        })
        .collect();
    d.insert("Opt".to_owned(), PdfObject::Array(opt_array));

    // Set initial value and selection index
    if let Some(idx) = selected {
        if idx < options.len() {
            d.insert(
                "V".to_owned(),
                PdfObject::String(options[idx].0.as_bytes().to_vec()),
            );
            d.insert(
                "I".to_owned(),
                PdfObject::Array(vec![PdfObject::Integer(idx as i64)]),
            );
        }
    }

    // Default appearance string
    d.insert(
        "DA".to_owned(),
        PdfObject::String(b"/Helvetica 10 Tf 0 g".to_vec()),
    );

    // Annot flags: Print (4)
    d.insert("Flags".to_owned(), PdfObject::Integer(4));

    Ok(writer.add_object(PdfObject::Dictionary(d)))
}

// ── Radio button group ───────────────────────────────────────────────────────

/// Radio field flag: bit 15 (0x8000).
const FF_RADIO: i64 = 0x8000;
/// NoToggleToOff field flag: bit 15 (0x4000).
const FF_NO_TOGGLE_TO_OFF: i64 = 0x4000;

/// Write a radio button group.
///
/// `buttons` is a slice of `(export_value, rect)` pairs — one per radio button.
/// `selected` is the 0-based index of the initially selected button, or `None`.
///
/// Returns `(parent_field_id, child_widget_ids)`. The parent must be registered
/// with [`AcroFormBuilder::add_field`]; the children are widget annotations
/// that should be added to their respective page `/Annots` arrays.
pub fn build_radio_group(
    group_name: &str,
    buttons: &[(&str, [f64; 4])],
    selected: Option<usize>,
    writer: &mut PdfWriter,
) -> Result<(u32, Vec<u32>)> {
    let selected_value = selected
        .and_then(|idx| buttons.get(idx))
        .map(|(val, _)| *val)
        .unwrap_or("Off");

    // Build child widget annotations
    let mut child_ids = Vec::with_capacity(buttons.len());
    for (i, (export_value, rect)) in buttons.iter().enumerate() {
        let is_selected = selected == Some(i);
        let child_id = build_radio_button_widget(export_value, *rect, is_selected, writer)?;
        child_ids.push(child_id);
    }

    // Build parent field dict
    let mut parent = PdfDict::new();
    parent.insert("FT".to_owned(), PdfObject::Name("Btn".to_owned()));
    parent.insert(
        "T".to_owned(),
        PdfObject::String(group_name.as_bytes().to_vec()),
    );
    parent.insert(
        "Ff".to_owned(),
        PdfObject::Integer(FF_RADIO | FF_NO_TOGGLE_TO_OFF),
    );
    parent.insert("V".to_owned(), PdfObject::Name(selected_value.to_owned()));
    parent.insert(
        "Kids".to_owned(),
        PdfObject::Array(
            child_ids
                .iter()
                .map(|&id| PdfObject::Reference(id, 0))
                .collect(),
        ),
    );

    let parent_id = writer.add_object(PdfObject::Dictionary(parent));
    Ok((parent_id, child_ids))
}

/// Build a single radio button widget annotation.
fn build_radio_button_widget(
    export_value: &str,
    rect: [f64; 4],
    is_selected: bool,
    writer: &mut PdfWriter,
) -> Result<u32> {
    let current_state = if is_selected { export_value } else { "Off" };

    // Appearance streams: selected (filled circle) and off (empty circle)
    let on_bytes = super::appearance::radio_appearance(rect, true);
    let off_bytes = super::appearance::radio_appearance(rect, false);

    let mut ap_stream_extras = PdfDict::new();
    ap_stream_extras.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
    ap_stream_extras.insert(
        "BBox".to_owned(),
        PdfObject::Array(vec![
            PdfObject::Real(0.0),
            PdfObject::Real(0.0),
            PdfObject::Real(rect[2] - rect[0]),
            PdfObject::Real(rect[3] - rect[1]),
        ]),
    );

    let on_stream = make_flate_stream(&on_bytes, ap_stream_extras.clone())?;
    let off_stream = make_flate_stream(&off_bytes, ap_stream_extras)?;

    let on_id = writer.add_object(PdfObject::Stream(Box::new(on_stream)));
    let off_id = writer.add_object(PdfObject::Stream(Box::new(off_stream)));

    let mut ap_n = PdfDict::new();
    ap_n.insert(export_value.to_owned(), PdfObject::Reference(on_id, 0));
    ap_n.insert("Off".to_owned(), PdfObject::Reference(off_id, 0));
    let mut ap = PdfDict::new();
    ap.insert("N".to_owned(), PdfObject::Dictionary(ap_n));

    let mut d = PdfDict::new();
    d.insert("Type".to_owned(), PdfObject::Name("Annot".to_owned()));
    d.insert("Subtype".to_owned(), PdfObject::Name("Widget".to_owned()));
    d.insert(
        "Rect".to_owned(),
        PdfObject::Array(rect.iter().map(|&v| PdfObject::Real(v)).collect()),
    );
    d.insert("AS".to_owned(), PdfObject::Name(current_state.to_owned()));
    d.insert("AP".to_owned(), PdfObject::Dictionary(ap));
    d.insert("Flags".to_owned(), PdfObject::Integer(4));

    Ok(writer.add_object(PdfObject::Dictionary(d)))
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_field_has_required_keys() {
        let mut writer = PdfWriter::new();
        let id =
            build_text_field("Name", [10.0, 700.0, 200.0, 720.0], "", false, &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("FT"), Some(&PdfObject::Name("Tx".to_owned())));
            assert_eq!(
                d.get("Subtype"),
                Some(&PdfObject::Name("Widget".to_owned()))
            );
            assert!(d.contains_key("T"));
            assert!(d.contains_key("Rect"));
            assert!(d.contains_key("DA"));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn multiline_flag_set() {
        let mut writer = PdfWriter::new();
        let id =
            build_text_field("Notes", [0.0, 0.0, 200.0, 100.0], "", true, &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("Ff"), Some(&PdfObject::Integer(0x1000)));
        }
    }

    #[test]
    fn checkbox_checked_has_yes_state() {
        let mut writer = PdfWriter::new();
        let id = build_checkbox("Accept", [10.0, 10.0, 22.0, 22.0], true, &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("V"), Some(&PdfObject::Name("Yes".to_owned())));
            assert_eq!(d.get("AS"), Some(&PdfObject::Name("Yes".to_owned())));
            assert_eq!(d.get("FT"), Some(&PdfObject::Name("Btn".to_owned())));
        }
    }

    #[test]
    fn checkbox_unchecked_has_off_state() {
        let mut writer = PdfWriter::new();
        let id = build_checkbox("Accept", [10.0, 10.0, 22.0, 22.0], false, &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("V"), Some(&PdfObject::Name("Off".to_owned())));
        }
    }

    #[test]
    fn acroform_builder_includes_fields() {
        let mut writer = PdfWriter::new();
        let f1 = build_text_field("F1", [0.0, 0.0, 100.0, 20.0], "", false, &mut writer).unwrap();
        let f2 = build_checkbox("CB", [0.0, 0.0, 12.0, 12.0], false, &mut writer).unwrap();
        let mut form = AcroFormBuilder::new();
        form.add_field(f1).add_field(f2);
        let form_id = form.build(&mut writer);
        let obj = writer.get_object(form_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            match d.get("Fields") {
                Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 2),
                _ => panic!("expected Fields array"),
            }
        }
    }

    #[test]
    fn combo_field_has_choice_type_and_combo_flag() {
        let mut writer = PdfWriter::new();
        let options = &[("a", "Option A"), ("b", "Option B"), ("c", "Option C")];
        let id = build_combo_field(
            "Dropdown",
            [10.0, 700.0, 200.0, 720.0],
            options,
            Some(1),
            &mut writer,
        )
        .unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("FT"), Some(&PdfObject::Name("Ch".to_owned())));
            // Combo flag (bit 18 = 0x20000)
            assert_eq!(d.get("Ff"), Some(&PdfObject::Integer(0x20000)));
            // Opt array has 3 entries
            match d.get("Opt") {
                Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 3),
                _ => panic!("expected Opt array"),
            }
            // Selected value is "b"
            assert_eq!(d.get("V"), Some(&PdfObject::String(b"b".to_vec())));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn list_field_multi_select_flag() {
        let mut writer = PdfWriter::new();
        let options = &[("x", "X"), ("y", "Y")];
        let id = build_list_field(
            "List",
            [0.0, 0.0, 100.0, 60.0],
            options,
            None,
            true,
            &mut writer,
        )
        .unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("FT"), Some(&PdfObject::Name("Ch".to_owned())));
            // MultiSelect flag (0x200000), no Combo flag
            assert_eq!(d.get("Ff"), Some(&PdfObject::Integer(0x200000)));
            assert!(d.get("V").is_none());
        }
    }

    #[test]
    fn radio_group_structure() {
        let mut writer = PdfWriter::new();
        let buttons = &[
            ("opt1", [10.0, 10.0, 22.0, 22.0]),
            ("opt2", [30.0, 10.0, 42.0, 22.0]),
            ("opt3", [50.0, 10.0, 62.0, 22.0]),
        ];
        let (parent_id, child_ids) =
            build_radio_group("RadioQ", buttons, Some(1), &mut writer).unwrap();

        // Parent has Radio + NoToggleToOff flags
        let parent = writer.get_object(parent_id).unwrap();
        if let PdfObject::Dictionary(d) = parent {
            assert_eq!(d.get("FT"), Some(&PdfObject::Name("Btn".to_owned())));
            assert_eq!(d.get("Ff"), Some(&PdfObject::Integer(0x8000 | 0x4000)));
            assert_eq!(d.get("V"), Some(&PdfObject::Name("opt2".to_owned())));
            match d.get("Kids") {
                Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 3),
                _ => panic!("expected Kids array"),
            }
        }

        // Selected child (index 1) has AS = "opt2"
        let child = writer.get_object(child_ids[1]).unwrap();
        if let PdfObject::Dictionary(d) = child {
            assert_eq!(d.get("AS"), Some(&PdfObject::Name("opt2".to_owned())));
        }

        // Unselected child has AS = "Off"
        let child0 = writer.get_object(child_ids[0]).unwrap();
        if let PdfObject::Dictionary(d) = child0 {
            assert_eq!(d.get("AS"), Some(&PdfObject::Name("Off".to_owned())));
        }
    }
}
