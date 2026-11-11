//! Text extraction from PDF content streams.
//!
//! Builds on the [`ContentInterpreter`] / [`OutputDevice`] infrastructure to
//! group raw [`TextSpan`] events into words, lines, and plain text output.
//!
//! [`ContentInterpreter`]: crate::content::interpreter::ContentInterpreter
//! [`OutputDevice`]: crate::content::interpreter::OutputDevice
//! [`TextSpan`]: crate::content::text_state::TextSpan

pub mod bidi;
pub mod columns;
pub mod dehyphenate;
pub mod extractor;
pub mod layout;
pub mod rotated;
pub mod search;

pub use bidi::{contains_rtl, reorder_visual_to_logical};
pub use columns::split_columns;
pub use dehyphenate::dehyphenate;
pub use extractor::TextExtractor;
pub use layout::{TextBlock, TextLine, TextWord};
pub use rotated::{group_rotated, orientation, OTHER};
pub use search::{search_document, search_page, SearchResult};
#[cfg(feature = "search")]
pub use search::{search_document_regex, search_page_regex};
