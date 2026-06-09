//! PDF interactive forms — AcroForm fields and annotation appearance streams.
//!
//! Gated on the `forms` Cargo feature (which implies `writer`).

pub mod acroform;
pub mod appearance;
pub mod filler;
pub mod reader;

pub use acroform::{
    build_checkbox, build_combo_field, build_list_field, build_radio_group, build_text_field,
    AcroFormBuilder,
};
pub use appearance::{
    checkbox_appearance, highlight_appearance, radio_appearance, text_field_appearance,
    text_note_appearance,
};
pub use filler::{set_checkbox, set_combo_or_list, set_text_field};
pub use reader::{read_form_fields, FieldType, FormField};
