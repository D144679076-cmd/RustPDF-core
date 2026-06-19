//! Axum router wiring — maps every REST endpoint to its handler.

use axum::routing::{get, post};

use super::handlers;

/// Build the axum [`Router`](axum::Router) for the pdf-core REST API.
///
/// Applies license auth middleware, a 100 MB request-body limit, and
/// permissive CORS headers so browser clients can call the server directly.
pub fn build_router() -> axum::Router {
    axum::Router::new()
        .route("/api/v1/render", post(handlers::render))
        .route("/api/v1/extract-text", post(handlers::extract_text))
        .route("/api/v1/search", post(handlers::search))
        .route("/api/v1/merge", post(handlers::merge))
        .route("/api/v1/split", post(handlers::split))
        .route("/api/v1/optimize", post(handlers::optimize))
        .route("/api/v1/redact", post(handlers::redact))
        .route("/api/v1/form/export-fdf", post(handlers::export_fdf))
        .route("/api/v1/form/import-fdf", post(handlers::import_fdf))
        .route("/api/v1/form/xfa/detect", post(handlers::xfa_detect))
        .route("/api/v1/form/xfa/extract", post(handlers::xfa_extract))
        .route("/api/v1/form/xfa/flatten", post(handlers::xfa_flatten))
        .route(
            "/api/v1/annotate/flatten",
            post(handlers::flatten_annotations),
        )
        .route("/api/v1/watermark", post(handlers::watermark))
        .route("/api/v1/validate-pdfa", post(handlers::validate_pdfa))
        .route("/api/v1/convert-pdfa", post(handlers::convert_pdfa))
        .route("/api/v1/health", get(handlers::health))
        .layer(axum::middleware::from_fn(
            super::auth::verify_license_middleware,
        ))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            100 * 1024 * 1024,
        ))
        .layer(tower_http::cors::CorsLayer::permissive())
}
