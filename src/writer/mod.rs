//! PDF writer subsystem — serialize objects, build documents, emit content streams.
//!
//! Gated on the `writer` Cargo feature.

pub mod content_builder;
pub mod document;
pub mod font;
pub mod font_subset;
pub mod image;
pub mod page;
pub mod serializer;
pub mod streams;
pub mod xref;

pub use content_builder::{ContentBuilder, TjItem};
pub use document::PdfWriter;
pub use font::{write_standard_font, write_truetype_font};
pub use font_subset::{embed_cidfont_for_chars, EmbeddedCidFont};
pub use image::{write_image_xobject, write_jpeg_xobject, ImageColorSpace, ImageData};
pub use page::PageBuilder;
pub use serializer::{serialize_object, write_indirect};
pub use streams::{encode_flate, make_flate_stream, make_raw_stream};
pub use xref::{build_trailer_dict, write_full_xref_and_trailer};
