//! PDF document structure navigation.
//!
//! - [`catalog`] — document catalog and page tree traversal
//! - [`page`] — individual page representation with inherited attributes
//! - [`metadata`] — document info dictionary and XMP metadata
//! - [`outline`] — bookmark/outline tree (read)
//! - [`outline_writer`] — bookmark/outline tree (write, `writer` feature)

pub mod catalog;
pub mod metadata;
pub mod name_tree;
pub mod outline;
#[cfg(feature = "writer")]
pub mod outline_writer;
pub mod page;
pub mod page_labels;
pub mod structure;
pub(crate) mod text_string;
pub mod xmp;

#[cfg(feature = "writer")]
pub use outline_writer::{set_document_outline, OutlineEntry};
