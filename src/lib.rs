//! # pdf-core
//!
//! A Rust-based PDF processing core library targeting WebAssembly.
//!
//! This crate provides low-level PDF parsing, rendering, writing, and editing
//! capabilities. It is designed to be compiled to both native targets and
//! WebAssembly via `wasm-pack`.
//!
//! ## Modules
//!
//! - [`parser`] — PDF tokenization, object parsing, and document loading.
//! - [`document`] — Document structure: catalog, pages, metadata, outlines.
//! - [`error`] — Shared error types used across the crate.

pub mod compliance;
pub mod content;
#[cfg(feature = "crypto")]
pub mod crypto;
pub mod display;
pub mod document;
#[cfg(feature = "writer")]
pub mod editor;
pub mod error;
#[cfg(all(feature = "ffi", not(target_arch = "wasm32")))]
pub mod ffi;
pub mod fonts;
#[cfg(feature = "forms")]
pub mod forms;
#[cfg(feature = "js-actions")]
pub mod js;
#[cfg(feature = "crypto")]
pub mod license;
pub mod parser;
#[cfg(feature = "render")]
pub mod render;
#[cfg(all(feature = "server", not(target_arch = "wasm32")))]
pub mod server;
#[cfg(feature = "signatures")]
pub mod signatures;
pub mod text;
#[cfg(feature = "wasm")]
pub mod wasm;
#[cfg(feature = "writer")]
pub mod writer;

// uniffi scaffolding initialisation — enables `uniffi-bindgen generate` to
// produce Swift/Kotlin wrappers from the compiled native library.
#[cfg(all(feature = "mobile", not(target_arch = "wasm32")))]
uniffi::setup_scaffolding!("pdf_core");

// Re-export the most commonly used public types at the crate root.
#[cfg(feature = "writer")]
pub use editor::MergeBuilder;
pub use error::{PdfError, Result};
pub use parser::objects::{PdfDict, PdfDocument, PdfObject, PdfStream};
