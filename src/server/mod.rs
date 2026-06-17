//! REST API server — Enterprise tier, native only.
//!
//! Exposes the pdf-core toolkit over HTTP so any language can use it without
//! native bindings. Gated on the `server` Cargo feature and excluded from
//! `wasm32` targets.
//!
//! Entry point: [`build_router`] returns an `axum::Router` ready to serve.

pub mod auth;
pub mod handlers;
pub mod multipart;
pub mod routes;

pub use routes::build_router;
