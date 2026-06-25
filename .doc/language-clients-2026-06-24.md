# Language Clients — Implementation Report

**Date:** 2026-06-24
**Scope:** Phase 4 — Python / Java / .NET REST Client Libraries

## What Was Implemented

### Rust / OpenAPI (pdf-editor-rust-core)
- `Cargo.toml`: added `utoipa = { version = "4", features = ["axum_extras"] }` as optional dep; added to `server` feature
- `src/server/handlers.rs`: added `#[utoipa::path(...)]` annotations to all 17 handlers covering query params, request body (both `application/pdf` and `multipart/form-data`), and response codes
- `src/server/routes.rs`: added `ApiDoc` struct with `#[derive(utoipa::OpenApi)]`, `/api/v1/openapi.json` GET endpoint, and `openapi_json()` function
- `src/server/mod.rs`: re-exported `openapi_json`
- `src/bin/pdf_server.rs`: added `--dump-openapi` flag (prints JSON spec and exits without starting HTTP server)

### Python client (`clients/python/`)
- `pdf_core_client/__init__.py` — package entrypoint
- `pdf_core_client/client.py` — `PdfCoreClient` sync class; all 17 endpoints; correct multipart for optimize/redact/watermark/merge/import-fdf
- `pdf_core_client/exceptions.py` — `PdfCoreError(status_code, message)`
- `tests/conftest.py` — `minimal_pdf` fixture
- `tests/test_client.py` — 16 mock-based unit tests (respx), one error-path test
- `pyproject.toml` — hatchling build, httpx>=0.27 dep, pytest/respx dev extras

### Java client (`clients/java/`)
- `PdfCoreClient.java` — full sync client (OkHttp4); all 17 endpoints; `AutoCloseable`
- `PdfCoreException.java` — checked exception carrying `statusCode`
- `PdfCoreClientTest.java` — 5 tests using OkHttp `MockWebServer`
- `pom.xml` — okhttp 4.12, org.json 20240303, junit-jupiter 5.10, mockwebserver

### .NET client (`clients/dotnet/`)
- `PdfCore.Client/PdfCoreClient.cs` — async client; all 17 endpoints; `System.Text.Json`; `IDisposable`
- `PdfCore.Client/PdfCoreException.cs` — typed exception with `StatusCode` property
- `PdfCore.Client/PdfCore.Client.csproj` — net8.0 target
- `PdfCore.Client.Tests/PdfCoreClientTests.cs` — shape/smoke tests; `MockHttpMessageHandler`
- `PdfCore.Client.Tests/PdfCore.Client.Tests.csproj` — xunit 2.8
- `PdfCore.sln` — solution file linking both projects

### GitHub Actions (`.github/workflows/clients.yml`)
- Triggers on changes to `pdf-editor-rust-core/src/server/**` or `openapi.yaml`
- Dumps fresh `openapi.yaml` via `--dump-openapi`
- Parallel jobs: test-python, test-java, test-dotnet, validate-spec (Spectral)
- Commits updated spec to `main` only when changed and all tests pass

## Design Decisions

- **utoipa 4 not utoipa-axum**: `utoipa-axum` provides tighter router-level integration but requires invasive changes to route wiring. Plain `#[utoipa::path]` proc-macro annotations on existing handlers are non-invasive.
- **`content = String` for multipart**: utoipa 4 requires `content = <Type>` when `content_type` is specified. `String` is used as a placeholder since multipart bodies have no single Rust type; `content_type = "multipart/form-data"` conveys the correct schema to spec consumers.
- **Python client uses httpx**: synchronous, zero transitive deps, idiomatic type hints. Async variant is a straightforward extension (swap `httpx.Client` → `httpx.AsyncClient`).
- **Java client uses OkHttp**: industry standard, minimal overhead, `AutoCloseable` for resource safety.
- **.NET client uses `System.Text.Json`**: built-in, no extra deps. `JsonNode` return type for JSON endpoints avoids coupling callers to generated DTO classes that would require schema versioning.
- **Multipart encoding in Python**: `httpx` `files=` dict naturally sets correct `Content-Type: multipart/form-data` with boundary; JSON option fields are sent as named file parts with `application/json` content-type to avoid conflicts with actual PDF bytes.

## Test Coverage

### Python (mock-based, respx)
- `test_health` — 200 JSON response parsed
- `test_extract_text` — pages array unwrapped
- `test_render_page` — PNG bytes returned verbatim
- `test_search` — results array unwrapped
- `test_merge` — multipart POST, PDF response
- `test_split` — query params forwarded
- `test_optimize` — multipart POST
- `test_redact` — multipart POST with zones JSON
- `test_watermark` — multipart POST with watermark JSON
- `test_flatten_annotations` — raw PDF body
- `test_export_fdf` — FDF bytes returned
- `test_import_fdf` — multipart POST with pdf + fdf
- `test_xfa_detect` — is_xfa bool unwrapped
- `test_validate_pdfa` — conformant bool checked
- `test_convert_pdfa` — PDF bytes returned
- `test_error_raises_pdf_core_error` — 422 raises `PdfCoreError` with correct status code

### Java (MockWebServer)
- `health_returns_status_ok`
- `extract_text_parses_response`
- `render_page_returns_bytes`
- `error_response_throws_pdf_core_exception`
- `merge_sends_multipart` — verifies Content-Type header

### .NET (unit)
- `Health_ReturnsStatusOk` — mock handler shape test
- `PdfCoreException_CarriesStatusCode` — exception type check

## Known Limitations / Follow-up

- **Generated clients not wired yet**: The GitHub Actions workflow dumps a fresh `openapi.yaml` but doesn't yet run `openapi-generator` to produce auto-generated stubs. The hand-written clients are the primary deliverable; generated stubs would supplement them.
- **Async Python client**: only sync variant implemented; `AsyncPdfCoreClient` using `httpx.AsyncClient` is a natural follow-on.
- **Java async**: OkHttp's `enqueue` API not exposed; add `CompletableFuture`-based methods if callers need non-blocking I/O.
- **.NET test coverage**: `MockHttpMessageHandler` approach requires injecting `HttpClient`; real integration requires refactoring `PdfCoreClient` to accept `IHttpClientFactory` for easier mocking.
- **openapi.yaml not committed**: the spec is generated at CI time; initial `openapi.yaml` must be committed manually on first run (`cargo run --features server --bin pdf-server -- --dump-openapi > openapi.yaml`).
