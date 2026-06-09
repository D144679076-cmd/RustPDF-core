//! PDF editor subsystem — incremental update model.
//!
//! Gated on the `writer` Cargo feature.

pub mod annotation;
pub mod content_draw;
pub mod document_editor;
pub mod edit_session;
pub mod merge;
pub mod metadata_editor;
pub mod page_editor;
pub mod redact;
pub mod remap;
pub mod text_commit;
pub mod text_commit_runs;
pub mod text_edit_engine;
pub mod text_editor;
pub mod text_encode;
pub mod text_model;
pub mod text_shape;
pub mod text_style;

pub use annotation::{
    add_annotation, delete_annotation, flatten_all_annotations, flatten_annotations,
    AnnotationBuilder, AnnotationType,
};
pub use content_draw::{
    draw_ellipse, draw_line, draw_rect, draw_text, place_image, place_jpeg, RectStyle, TextStyle,
};
pub use document_editor::{EditMode, PdfEditor};
pub use edit_session::{
    build_edit_session, commit_edit_session, patch_frame, EditSession, EditableFrame,
};
pub use merge::{extract_pages, MergeBuilder};
pub use metadata_editor::{set_metadata, MetadataFields};
pub use page_editor::{
    add_blank_page, begin_edit_page, delete_page, move_page, rotate_page, set_crop_box,
    ContentLayer,
};
pub use redact::{apply_redactions, RedactZone};
pub use remap::{remap_dict, remap_object};
pub use text_commit::{commit_block, commit_block_with_font, register_page_font};
pub use text_commit_runs::{
    build_decoration_ops, build_run_ops, commit_block_runs, DecoRect, ResolvedRun, RunLayout,
};
pub use text_edit_engine::{Dir, TextEditEngine};
pub use text_editor::{encode_pdf_string, replace_text_in_page, resolve_font_name, TextEditTarget};
pub use text_encode::{encode_in_font, EncodeResult};
pub use text_model::{build_text_model, EditBlock, TextModel};
pub use text_shape::{
    caret_offsets, font_metrics_for, hit_test, text_width, wrap_lines, Measurer, PdfFontMetrics,
};
pub use text_style::{
    decoration_thickness, run_synthetic_style, strike_offset, underline_offset, ActiveStyle, Align,
    CharStyle, FontChoice, StyleRun, SyntheticStyle, OBLIQUE_SHEAR, SYNTHETIC_BOLD_STROKE_FRAC,
};
