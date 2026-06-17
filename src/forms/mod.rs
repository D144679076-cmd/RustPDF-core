//! PDF interactive forms — AcroForm fields and annotation appearance streams.
//!
//! Gated on the `forms` Cargo feature (which implies `writer`).

pub mod acroform;
pub mod appearance;
pub mod fdf;
pub mod filler;
pub mod reader;

pub use acroform::{
    build_checkbox, build_combo_field, build_list_field, build_radio_group, build_text_field,
    AcroFormBuilder,
};
pub use appearance::{
    caret_appearance, checkbox_appearance, file_attachment_appearance, freetext_appearance,
    highlight_appearance, highlight_appearance_quad, ink_appearance, polygon_appearance,
    polyline_appearance, radio_appearance, stamp_appearance, text_field_appearance,
    text_note_appearance,
};
pub use fdf::{export_fdf, export_xfdf, import_fdf, import_xfdf};
pub use filler::{
    flatten_all_form_fields, flatten_form_fields, set_checkbox, set_combo_or_list, set_text_field,
};
pub use reader::{read_form_fields, FieldType, FormField};
