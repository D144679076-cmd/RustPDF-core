//! PDF parsing subsystem.
//!
//! - [`lexer`] — byte-level tokenizer
//! - [`xref`] — XRef table/stream parser (standalone, returns offset map)
//! - [`filters`] — stream filter decoders (FlateDecode, ASCII85, etc.)
//! - [`objects`] — full object model, document loader, reference resolution

pub mod filters;
pub mod lexer;
pub mod objects;
pub mod xref;
