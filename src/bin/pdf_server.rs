//! `pdf-server` — REST API server for the pdf-core library.
//!
//! Listens on `0.0.0.0:<PORT>` (default 3000) and exposes all PDF operations
//! over HTTP. Requires an Enterprise license (passed via `PDF_CORE_LICENSE`
//! env var or per-request `Authorization: Bearer` / `X-API-Key` header).
//!
//! Usage:
//! ```bash
//! PDF_CORE_LICENSE=<enterprise_key> cargo run --features server --bin pdf-server
//! ```
//!
//! Dump OpenAPI spec and exit:
//! ```bash
//! cargo run --features server --bin pdf-server -- --dump-openapi > openapi.yaml
//! ```

#[tokio::main]
async fn main() {
    // --dump-openapi: print the OpenAPI JSON spec to stdout and exit.
    if std::env::args().any(|a| a == "--dump-openapi") {
        print!("{}", pdf_core::server::openapi_json());
        return;
    }

    env_logger::init();

    // Pre-activate license from environment variable if provided.
    if let Ok(key) = std::env::var("PDF_CORE_LICENSE") {
        match pdf_core::license::validate_license_key(key.trim()) {
            Ok(lic) => {
                log::info!(
                    "License activated: tier={:?}, licensee={}",
                    lic.tier,
                    lic.licensee
                );
                let _ = pdf_core::license::activate(lic);
            }
            Err(e) => {
                log::warn!("PDF_CORE_LICENSE is set but invalid: {e}");
            }
        }
    }

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_owned());
    let addr = format!("0.0.0.0:{port}");

    let router = pdf_core::server::build_router();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            log::error!("Failed to bind to {addr}: {e}");
            std::process::exit(1);
        });

    log::info!("pdf-server listening on {addr}");
    axum::serve(listener, router).await.unwrap_or_else(|e| {
        log::error!("Server error: {e}");
        std::process::exit(1);
    });
}
