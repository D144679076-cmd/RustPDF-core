//! PDF parsing subsystem.
//!
//! - [`lexer`] — byte-level tokenizer
//! - [`xref`] — XRef table/stream parser (standalone, returns offset map)
//! - [`filters`] — stream filter decoders (FlateDecode, ASCII85, etc.)
//! - [`objects`] — full object model, document loader, reference resolution
//! - [`version`] — `%PDF-m.n` header detection, [`PdfVersion`] model

pub mod ccitt;
pub mod ccitt_parms;
pub mod ccitt_tables;
pub mod filters;
pub mod jbig2;
pub mod jpx;
pub mod lexer;
pub mod objects;
pub mod version;
pub mod xref;
