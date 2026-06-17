//! JavaScript Actions Engine (ISO 32000-1 §12.6).
//!
//! Gated on the `js-actions` Cargo feature.  Embeds QuickJS via `rquickjs`
//! (~1 MB overhead) and exposes a PDF JavaScript API surface compatible with
//! the Acrobat spec.
//!
//! - [`engine`]  — QuickJS runtime wrapper
//! - [`pdf_api`] — PDF global objects (`app`, `event`, `doc`, `field`)
//! - [`actions`] — action dispatch and PDF structure navigation

pub mod actions;
pub mod engine;
pub mod pdf_api;

pub use actions::dispatch_doc_event;
pub use engine::{JsActionResult, JsEngine, JsEvent};
