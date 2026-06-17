//! Text extraction from PDF content streams.
//!
//! Builds on the [`ContentInterpreter`] / [`OutputDevice`] infrastructure to
//! group raw [`TextSpan`] events into words, lines, and plain text output.
//!
//! [`ContentInterpreter`]: crate::content::interpreter::ContentInterpreter
//! [`OutputDevice`]: crate::content::interpreter::OutputDevice
//! [`TextSpan`]: crate::content::text_state::TextSpan

pub mod extractor;
pub mod layout;
pub mod search;

pub use extractor::TextExtractor;
pub use layout::{TextBlock, TextLine, TextWord};
pub use search::{search_document, search_page, SearchResult};
#[cfg(feature = "search")]
pub use search::{search_document_regex, search_page_regex};
