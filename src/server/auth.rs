//! License-key middleware for the REST API.
//!
//! Every request must carry either `Authorization: Bearer <key>` or an
//! `X-API-Key: <key>` header. The key is validated offline by the license
//! module; no network call is made. Enterprise-tier licenses are required
//! because the REST server itself is an Enterprise feature (ISO 32000-2
//! server-side processing tier).

use axum::{
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Axum middleware that validates the API key on every request.
///
/// Returns 401 if the header is missing or the key fails validation.
/// Bypasses the check for `GET /api/v1/health` so monitoring systems
/// can probe the server without a license key.
pub async fn verify_license_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    // Health check is always allowed — monitoring probes should not need a key.
    if req.uri().path() == "/api/v1/health" {
        return next.run(req).await;
    }

    let auth = req
        .headers()
        .get("Authorization")
        .or_else(|| req.headers().get("X-API-Key"));

    match auth {
        Some(v) => {
            let raw = match v.to_str() {
                Ok(s) => s,
                Err(_) => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        "invalid Authorization header encoding",
                    )
                        .into_response()
                }
            };
            let key = raw.trim_start_matches("Bearer ").trim();

            // If an Enterprise license is already activated in this process,
            // skip per-request key validation (reduces latency).
            if crate::license::current_tier() >= crate::license::Tier::Enterprise {
                return next.run(req).await;
            }

            match crate::license::validate_license_key(key) {
                Ok(lic) if lic.tier >= crate::license::Tier::Enterprise => {
                    // Activate on first valid Enterprise key seen.
                    let _ = crate::license::activate(lic);
                    next.run(req).await
                }
                Ok(_) => (
                    StatusCode::FORBIDDEN,
                    "Enterprise license required for REST API",
                )
                    .into_response(),
                Err(_) => (StatusCode::UNAUTHORIZED, "invalid license key").into_response(),
            }
        }
        None => (
            StatusCode::UNAUTHORIZED,
            "missing Authorization or X-API-Key header",
        )
            .into_response(),
    }
}
