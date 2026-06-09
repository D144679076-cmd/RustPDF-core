//! PDF document structure navigation.
//!
//! - [`catalog`] — document catalog and page tree traversal
//! - [`page`] — individual page representation with inherited attributes
//! - [`metadata`] — document info dictionary and XMP metadata
//! - [`outline`] — bookmark/outline tree

pub mod catalog;
pub mod metadata;
pub mod name_tree;
pub mod outline;
pub mod page;
pub mod page_labels;
pub mod structure;
pub(crate) mod text_string;
pub mod xmp;
