# server-rest-api — Implementation Report

**Date:** 2026-06-17
**Scope:** Phase 3 REST API server (`server` Cargo feature)

## What Was Implemented

### New files

| File | Purpose |
|------|---------|
| `src/server/mod.rs` | Module root; re-exports `build_router` |
| `src/server/routes.rs` | `build_router()` — axum Router with all 14 routes, auth middleware, 100 MB limit, CORS |
| `src/server/auth.rs` | `verify_license_middleware` — validates `Authorization: Bearer` / `X-API-Key` header; health check bypassed |
| `src/server/multipart.rs` | `collect_multipart_files`, `collect_named_parts` helpers |
| `src/server/handlers.rs` | One handler per route (see table below) |
| `src/bin/pdf_server.rs` | `pdf-server` binary; reads `PORT` and `PDF_CORE_LICENSE` env vars |

### Routes implemented

| Method | Path | Body | Response |
|--------|------|------|----------|
| GET | `/api/v1/health` | — | `{"status":"ok","version":"..."}` |
| POST | `/api/v1/render` | PDF bytes | PNG bytes (requires `render` feature) |
| POST | `/api/v1/extract-text` | PDF bytes | `{"pages":[{"page":N,"text":"..."}]}` |
| POST | `/api/v1/search` | PDF bytes | `{"results":[{"page","text","bounds"}]}` |
| POST | `/api/v1/merge` | multipart files[] | PDF bytes |
| POST | `/api/v1/split` | PDF bytes + `?start&end` | PDF bytes |
| POST | `/api/v1/optimize` | multipart(pdf, options) | PDF bytes |
| POST | `/api/v1/redact` | multipart(pdf, zones) | PDF bytes |
| POST | `/api/v1/form/export-fdf` | PDF bytes | FDF bytes |
| POST | `/api/v1/form/import-fdf` | multipart(pdf, fdf) | PDF bytes |
| POST | `/api/v1/annotate/flatten` | PDF bytes | PDF bytes |
| POST | `/api/v1/watermark` | multipart(pdf, watermark) | PDF bytes |
| POST | `/api/v1/validate-pdfa` | PDF bytes + `?level` | `{"conformant":bool,"violations":[...]}` |
| POST | `/api/v1/convert-pdfa` | PDF bytes + `?level` | PDF bytes |

### Cargo.toml additions

```toml
axum = { version = "0.7", features = ["multipart"], optional = true }
tokio = { version = "1", features = ["full"], optional = true }
tower-http = { version = "0.5", features = ["cors","limit"], optional = true }
serde_json = { version = "1", optional = true }
serde = { version = "1", features = ["derive"], optional = true }
png = { version = "0.17", optional = true }
env_logger = { version = "0.11", optional = true }

[features]
server = ["dep:axum", "dep:tokio", "dep:tower-http", "dep:serde_json",
          "dep:serde", "dep:png", "dep:env_logger",
          "writer", "forms", "crypto", "search"]
```

## Design Decisions

- **`render` excluded from `server` feature** — The existing `render` feature requires the `core-fonts/` directory via `include_bytes!` at compile time. That directory does not exist in the repo. The render endpoint is guarded with `#[cfg(feature = "render")]` and returns 501 when not compiled in. Users who have `core-fonts/` can build with `--features server,render`.

- **Multipart for handlers with extra params** — Handlers that need both PDF bytes and JSON configuration (optimize, redact, watermark, import-fdf) use multipart/form-data. Single-param handlers (split, extract-text, validate-pdfa, convert-pdfa) use query string. This is consistent with HTTP conventions and avoids custom Content-Type negotiation.

- **`spawn_blocking` for all PDF work** — All PDF processing runs in `tokio::task::spawn_blocking` so the async executor is never blocked by CPU-intensive operations.

- **License pre-activation from env** — The binary reads `PDF_CORE_LICENSE` at startup and calls `license::activate()`. If a license is already activated, the per-request middleware skips key re-validation to reduce latency.

- **Health check bypasses auth** — `GET /api/v1/health` always returns 200 without requiring a license key, enabling monitoring probes.

- **PNG encoding via `png` crate** — Uses `render_page_rgba` (returns unpremultiplied RGBA) + the `png` crate encoder. Avoids enabling `png-format` in tiny-skia which would require the font files at check time.

- **JSON request structs** — `OptimizeOptions`, `RedactZoneRequest`, `WatermarkRequest` are new `#[derive(Deserialize)]` structs defined in handlers.rs. The existing `OptimizationOptions`, `RedactZone` don't derive serde to avoid adding serde as a hard dep to the whole crate.

## Test Coverage

| Test | What it covers |
|------|---------------|
| `health_returns_200` | Health endpoint returns 200 without auth |
| `missing_license_returns_401` | Unauthenticated request to POST endpoint returns 401 |

The router is built from `build_router()` in both tests and the binary — same code path.

## Known Limitations / Follow-up

1. **render feature**: Production use requires building with `--features server,render` and providing the `core-fonts/` directory at compile time.
2. **Enterprise tier check**: Auth middleware requires Enterprise-tier license. There is no Pro-tier REST-API access path.
3. **Streaming**: Large PDF operations return the full response body in memory. Streaming could reduce peak memory usage for large files.
4. **No rate limiting**: The 100 MB body limit is the only guard against abuse. Adding rate limiting (tower-governor) is follow-up work.
5. **Client SDKs**: Python/Java/.NET thin HTTP wrappers described in the phase spec are not yet generated.
