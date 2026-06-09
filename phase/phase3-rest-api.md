# Phase 3 — REST Server API

**Status:** Not started
**Effort:** ~2–3 months
**Tier gate:** Enterprise (server-side license validation)
**Platform:** Native only (`#[cfg(not(target_arch = "wasm32"))]`)

## Context

A REST API server lets any language (Python, Java, .NET, Ruby, etc.) use the PDF toolkit without native bindings. Also enables server-side processing for large files where WASM would be too slow. Uses `axum` as the HTTP framework (Tokio-based, excellent ergonomics).

## New Binary `src/bin/pdf_server.rs`

```toml
# Add to Cargo.toml
[[bin]]
name = "pdf-server"
path = "src/bin/pdf_server.rs"
required-features = ["server"]

[features]
server = ["dep:axum", "dep:tokio", "dep:tower-http", "dep:serde_json"]
```

```toml
[dependencies]
axum = { version = "0.7", optional = true }
tokio = { version = "1", optional = true, features = ["full"] }
tower-http = { version = "0.5", optional = true, features = ["cors", "limit"] }
serde_json = { version = "1", optional = true }
```

## API Routes

| Method | Path | Body | Response |
|--------|------|------|----------|
| POST | `/api/v1/render` | PDF bytes | PNG bytes |
| POST | `/api/v1/extract-text` | PDF bytes | JSON `{pages: [{page: N, text: "..."}]}` |
| POST | `/api/v1/search` | PDF bytes + query params | JSON `{results: [{page, text, bounds}]}` |
| POST | `/api/v1/merge` | multipart(files[]) | PDF bytes |
| POST | `/api/v1/split` | PDF bytes + query `?start=0&end=3` | PDF bytes |
| POST | `/api/v1/optimize` | PDF bytes + JSON options | PDF bytes |
| POST | `/api/v1/redact` | PDF bytes + JSON zones | PDF bytes |
| POST | `/api/v1/form/export-fdf` | PDF bytes | FDF bytes |
| POST | `/api/v1/form/import-fdf` | multipart(pdf, fdf) | PDF bytes |
| POST | `/api/v1/annotate/flatten` | PDF bytes | PDF bytes |
| POST | `/api/v1/watermark` | PDF bytes + JSON watermark | PDF bytes |
| POST | `/api/v1/validate-pdfa` | PDF bytes + query `?level=1b` | JSON violations |
| POST | `/api/v1/convert-pdfa` | PDF bytes + query `?level=1b` | PDF bytes |
| GET  | `/api/v1/health` | — | JSON `{status: "ok", version: "0.1.0"}` |

## Implementation `src/server/`

```
src/server/
  mod.rs
  routes.rs     — axum Router setup
  handlers.rs   — one handler per route
  auth.rs       — Bearer token license validation
  multipart.rs  — multipart body parsing helper
```

### `src/server/routes.rs`

```rust
pub fn build_router() -> axum::Router {
    use axum::routing::post;
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
        .route("/api/v1/annotate/flatten", post(handlers::flatten_annotations))
        .route("/api/v1/watermark", post(handlers::watermark))
        .route("/api/v1/validate-pdfa", post(handlers::validate_pdfa))
        .route("/api/v1/convert-pdfa", post(handlers::convert_pdfa))
        .route("/api/v1/health", axum::routing::get(handlers::health))
        .layer(axum::middleware::from_fn(auth::verify_license_middleware))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(100 * 1024 * 1024)) // 100MB limit
        .layer(tower_http::cors::CorsLayer::permissive())
}
```

### `src/server/handlers.rs` — Example Handler

```rust
use axum::{extract::Query, response::IntoResponse, body::Bytes};
use std::collections::HashMap;

/// POST /api/v1/extract-text?page=0
/// Body: raw PDF bytes
/// Response: JSON {pages: [{page: N, text: "..."}]}
pub async fn extract_text(
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    let result: Result<String, _> = tokio::task::spawn_blocking(move || {
        let doc = crate::parser::PdfDocument::parse(body.to_vec())?;
        let page_count = doc.page_count()?;
        let target_pages: Vec<usize> = if let Some(p) = params.get("page") {
            vec![p.parse().unwrap_or(0)]
        } else {
            (0..page_count).collect()
        };
        let mut pages = Vec::new();
        for page_index in target_pages {
            let catalog = crate::document::Catalog::from_document(&doc)?;
            let page_dict = catalog.get_page_dict(&doc, page_index)?;
            let page = crate::document::Page::from_dict(&doc, &page_dict)?;
            let extractor = crate::text::TextExtractor::extract_from_page(&doc, &page)?;
            pages.push(serde_json::json!({ "page": page_index, "text": extractor.plain_text() }));
        }
        Ok::<_, crate::error::PdfError>(serde_json::json!({ "pages": pages }).to_string())
    }).await.unwrap();

    match result {
        Ok(json) => (axum::http::StatusCode::OK, [("content-type", "application/json")], json).into_response(),
        Err(e) => (axum::http::StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response(),
    }
}

/// POST /api/v1/merge (multipart: files[])
pub async fn merge(body: axum::extract::Multipart) -> impl IntoResponse {
    let files = collect_multipart_files(body).await;
    let result: Result<Vec<u8>, _> = tokio::task::spawn_blocking(move || {
        let mut builder = crate::editor::MergeBuilder::new();
        for file_bytes in files? {
            builder = builder.add_source(file_bytes)?;
        }
        builder.merge()
    }).await.unwrap();
    match result {
        Ok(pdf) => (axum::http::StatusCode::OK, [("content-type", "application/pdf")], pdf).into_response(),
        Err(e) => (axum::http::StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response(),
    }
}
```

### `src/server/auth.rs`

```rust
pub async fn verify_license_middleware(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> impl axum::response::IntoResponse {
    // Check Authorization: Bearer <license_key> header
    // Or check X-API-Key header
    // Validate using crate::license::validate_license_key()
    let auth = req.headers().get("Authorization")
        .or_else(|| req.headers().get("X-API-Key"));
    match auth {
        Some(v) => {
            let key = v.to_str().unwrap_or("").trim_start_matches("Bearer ");
            if crate::license::current_tier() >= crate::license::Tier::Enterprise
                || crate::license::validate_license_key(key).is_ok() {
                next.run(req).await.into_response()
            } else {
                (axum::http::StatusCode::UNAUTHORIZED, "invalid license key").into_response()
            }
        }
        None => (axum::http::StatusCode::UNAUTHORIZED, "missing Authorization header").into_response(),
    }
}
```

### `src/bin/pdf_server.rs`

```rust
#[tokio::main]
async fn main() {
    env_logger::init();
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_owned());
    let addr = format!("0.0.0.0:{}", port);
    let router = pdf_core::server::build_router();
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    log::info!("pdf-server listening on {}", addr);
    axum::serve(listener, router).await.unwrap();
}
```

## Client Libraries (thin HTTP wrappers)

After the server is running, generate client SDKs:

**Python (`pdf_core_client`):**
```python
import httpx, typing

class PdfCoreClient:
    def __init__(self, base_url: str, api_key: str):
        self.client = httpx.Client(base_url=base_url, headers={"X-API-Key": api_key})

    def extract_text(self, pdf_bytes: bytes, page: int | None = None) -> dict:
        params = {"page": page} if page is not None else {}
        return self.client.post("/api/v1/extract-text", content=pdf_bytes, params=params).json()

    def merge(self, pdf_files: list[bytes]) -> bytes:
        files = [("files", (f"doc{i}.pdf", f, "application/pdf")) for i, f in enumerate(pdf_files)]
        return self.client.post("/api/v1/merge", files=files).content

    def split(self, pdf_bytes: bytes, start: int, end: int) -> bytes:
        return self.client.post("/api/v1/split", content=pdf_bytes, params={"start": start, "end": end}).content
```

**.NET NuGet / Java Maven:** Same pattern using `HttpClient`/`OkHttp` wrapping the REST endpoints.

## Tests

```rust
// Integration tests using the running server (start server in test setup)
#[cfg(feature = "server")]
#[tokio::test]
async fn server_health_check() {
    let app = build_router();
    let resp = axum_test::TestServer::new(app).unwrap()
        .get("/api/v1/health").await;
    resp.assert_status_ok();
}

#[cfg(feature = "server")]
#[tokio::test]
async fn server_extract_text_from_multipage() {
    let app = build_router();
    let pdf = include_bytes!("../../tests/fixtures/multipage.pdf");
    let resp = axum_test::TestServer::new(app).unwrap()
        .post("/api/v1/extract-text")
        .bytes(pdf.as_slice().into())
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["pages"].as_array().unwrap().len(), 3);
}
```

## Verification

```bash
cargo build --features server --bin pdf-server
cargo test --features server -- server_
# Run server and test with curl:
PDF_CORE_LICENSE=<pro_key> cargo run --features server --bin pdf-server &
curl -X POST http://localhost:3000/api/v1/extract-text \
  -H "X-API-Key: $LICENSE_KEY" \
  --data-binary @tests/fixtures/multipage.pdf
```
