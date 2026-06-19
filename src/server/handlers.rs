//! One axum handler per REST route.
//!
//! All CPU-bound PDF work is offloaded via `tokio::task::spawn_blocking` so
//! the async executor is not starved. Raw PDF bytes are accepted as the
//! request body; multipart is used when extra JSON parameters are needed.

use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::{Multipart, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

// ── shared response helpers ───────────────────────────────────────────────────

fn pdf_response(bytes: Vec<u8>) -> Response {
    (StatusCode::OK, [("content-type", "application/pdf")], bytes).into_response()
}

fn json_response(body: String) -> Response {
    (StatusCode::OK, [("content-type", "application/json")], body).into_response()
}

#[cfg(feature = "render")]
fn png_response(bytes: Vec<u8>) -> Response {
    (StatusCode::OK, [("content-type", "image/png")], bytes).into_response()
}

fn unprocessable(msg: String) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response()
}

fn bad_request(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, msg.to_owned()).into_response()
}

// ── GET /api/v1/health ────────────────────────────────────────────────────────

/// Return a JSON health payload.
///
/// Does not require a license key — safe for monitoring probes.
pub async fn health() -> Response {
    let body = serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    })
    .to_string();
    json_response(body)
}

// ── POST /api/v1/render ───────────────────────────────────────────────────────

/// Render a PDF page to a PNG image.
///
/// Query parameters:
/// - `page` (default 0): 0-based page index to render.
/// - `scale` (default 1.0): render scale; 2.0 = 144 DPI.
///
/// Returns PNG bytes. Requires the `render` Cargo feature in addition to
/// `server`; returns 501 Not Implemented when render is not compiled in.
pub async fn render(Query(params): Query<HashMap<String, String>>, body: Bytes) -> Response {
    #[cfg(not(feature = "render"))]
    {
        let _ = (params, body);
        (
            StatusCode::NOT_IMPLEMENTED,
            "render feature not compiled — rebuild with --features server,render",
        )
            .into_response()
    }

    #[cfg(feature = "render")]
    {
        let page_index: usize = params.get("page").and_then(|s| s.parse().ok()).unwrap_or(0);
        let scale: f64 = params
            .get("scale")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);

        let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
            let doc = crate::parser::objects::PdfDocument::parse(body.to_vec())
                .map_err(|e| e.to_string())?;
            let (w, h, rgba) = crate::render::render_page_rgba(&doc, page_index, scale)
                .map_err(|e| e.to_string())?;
            encode_rgba_to_png(w, h, &rgba).map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

        match result {
            Ok(png) => png_response(png),
            Err(e) => unprocessable(e),
        }
    }
}

/// Encode raw unpremultiplied RGBA bytes into a PNG byte vector.
#[cfg(feature = "render")]
fn encode_rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(buf)
}

// ── POST /api/v1/extract-text ─────────────────────────────────────────────────

/// Extract text from all pages (or a single page) of a PDF.
///
/// Query parameters:
/// - `page` (optional): if present, extract only that 0-based page index.
///
/// Returns JSON: `{"pages": [{"page": N, "text": "..."}]}`.
pub async fn extract_text(Query(params): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let result: Result<String, String> = tokio::task::spawn_blocking(move || {
        let doc =
            crate::parser::objects::PdfDocument::parse(body.to_vec()).map_err(|e| e.to_string())?;
        let catalog =
            crate::document::catalog::Catalog::from_document(&doc).map_err(|e| e.to_string())?;
        let page_count = catalog.page_count;

        let target_pages: Vec<usize> = if let Some(p) = params.get("page") {
            let idx: usize = p.parse().map_err(|_| "invalid page parameter".to_owned())?;
            vec![idx]
        } else {
            (0..page_count).collect()
        };

        let mut pages = Vec::new();
        for page_index in target_pages {
            let page_dict = catalog
                .get_page_dict(&doc, page_index)
                .map_err(|e| e.to_string())?;
            let page = crate::document::page::Page::from_dict(&doc, &page_dict)
                .map_err(|e| e.to_string())?;
            let extractor = crate::text::TextExtractor::extract_from_page(&doc, &page)
                .map_err(|e| e.to_string())?;
            pages.push(serde_json::json!({
                "page": page_index,
                "text": extractor.plain_text(),
            }));
        }

        Ok(serde_json::json!({ "pages": pages }).to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(json) => json_response(json),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/search ───────────────────────────────────────────────────────

/// Search a PDF for occurrences of a query string.
///
/// Query parameters:
/// - `q`: search term (required).
/// - `case_sensitive` (default false): whether to use case-sensitive matching.
/// - `page` (optional): if present, search only that 0-based page index.
///
/// Returns JSON: `{"results": [{"page": N, "text": "...", "bounds": [x1,y1,x2,y2]}]}`.
pub async fn search(Query(params): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let query_str = match params.get("q") {
        Some(q) => q.clone(),
        None => return bad_request("missing query parameter 'q'"),
    };
    let case_sensitive = params
        .get("case_sensitive")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let page_filter: Option<usize> = params.get("page").and_then(|s| s.parse().ok());

    let result: Result<String, String> = tokio::task::spawn_blocking(move || {
        let doc =
            crate::parser::objects::PdfDocument::parse(body.to_vec()).map_err(|e| e.to_string())?;

        let raw_results = if let Some(page_idx) = page_filter {
            crate::text::search::search_page(&doc, page_idx, &query_str, case_sensitive)
                .map_err(|e| e.to_string())?
        } else {
            crate::text::search::search_document(&doc, &query_str, case_sensitive)
                .map_err(|e| e.to_string())?
        };

        let results: Vec<_> = raw_results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "page": r.page_index,
                    "text": r.text,
                    "bounds": r.bounds,
                })
            })
            .collect();

        Ok(serde_json::json!({ "results": results }).to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(json) => json_response(json),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/merge ────────────────────────────────────────────────────────

/// Merge multiple PDFs into one.
///
/// Body: multipart/form-data with one or more file fields named `files[]`
/// (or any name — all parts are collected in order).
///
/// Returns merged PDF bytes.
pub async fn merge(multipart: Multipart) -> Response {
    let files = match super::multipart::collect_multipart_files(multipart).await {
        Ok(f) => f,
        Err(e) => return bad_request(&e),
    };

    if files.is_empty() {
        return bad_request("no files provided");
    }

    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let mut builder = crate::editor::MergeBuilder::new();
        for file_bytes in files {
            builder = builder.add_source(file_bytes).map_err(|e| e.to_string())?;
        }
        builder.merge().map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/split ────────────────────────────────────────────────────────

/// Extract a page range from a PDF.
///
/// Query parameters:
/// - `start` (default 0): first page to include (0-based, inclusive).
/// - `end` (required): last page to include (0-based, inclusive).
///
/// Returns PDF bytes containing only the selected pages.
pub async fn split(Query(params): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let start: usize = params
        .get("start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let end: usize = match params.get("end").and_then(|s| s.parse().ok()) {
        Some(e) => e,
        None => return bad_request("missing required query parameter 'end'"),
    };

    if end < start {
        return bad_request("'end' must be >= 'start'");
    }

    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        // extract_pages uses exclusive end: pass end+1 so `end` is included.
        crate::editor::merge::extract_pages(body.to_vec(), start..end + 1)
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/optimize ─────────────────────────────────────────────────────

/// JSON body accepted by the `/optimize` endpoint.
#[derive(Deserialize)]
struct OptimizeOptions {
    #[serde(default = "default_true")]
    recompress_streams: bool,
    #[serde(default = "default_true")]
    deduplicate_resources: bool,
    #[serde(default = "default_true")]
    remove_unused_objects: bool,
    #[serde(default)]
    downsample_images: bool,
    #[serde(default = "default_dpi")]
    image_max_dpi: u32,
}

fn default_true() -> bool {
    true
}
fn default_dpi() -> u32 {
    150
}

/// Optimize a PDF.
///
/// Body: multipart with a `pdf` field (PDF bytes) and an optional `options`
/// field (JSON object). If `options` is omitted, defaults are used.
///
/// Returns optimized PDF bytes.
pub async fn optimize(multipart: Multipart) -> Response {
    let mut parts = match super::multipart::collect_named_parts(multipart).await {
        Ok(p) => p,
        Err(e) => return bad_request(&e),
    };

    let pdf_bytes = match parts.remove("pdf") {
        Some(b) => b,
        None => return bad_request("missing 'pdf' multipart field"),
    };

    let opts: OptimizeOptions = if let Some(json) = parts.remove("options") {
        match serde_json::from_slice(&json) {
            Ok(o) => o,
            Err(e) => return bad_request(&format!("invalid options JSON: {e}")),
        }
    } else {
        serde_json::from_str("{}").unwrap()
    };

    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let options = crate::writer::optimizer::OptimizationOptions {
            recompress_streams: opts.recompress_streams,
            deduplicate_resources: opts.deduplicate_resources,
            remove_unused_objects: opts.remove_unused_objects,
            downsample_images: opts.downsample_images,
            image_max_dpi: opts.image_max_dpi,
        };
        crate::writer::optimizer::optimize(&pdf_bytes, &options).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/redact ───────────────────────────────────────────────────────

/// A single redaction zone from the JSON request body.
#[derive(Deserialize)]
struct RedactZoneRequest {
    page_index: usize,
    rect: [f64; 4],
    #[serde(default)]
    overlay_color: Option<[f64; 3]>,
}

/// Redact rectangular zones in a PDF.
///
/// Body: multipart with a `pdf` field (PDF bytes) and a `zones` field
/// (JSON array of `{page_index, rect, overlay_color?}` objects).
///
/// Returns a new PDF with redacted content replaced by filled rectangles.
pub async fn redact(multipart: Multipart) -> Response {
    let mut parts = match super::multipart::collect_named_parts(multipart).await {
        Ok(p) => p,
        Err(e) => return bad_request(&e),
    };

    let pdf_bytes = match parts.remove("pdf") {
        Some(b) => b,
        None => return bad_request("missing 'pdf' multipart field"),
    };

    let zone_requests: Vec<RedactZoneRequest> = match parts.remove("zones") {
        Some(json) => match serde_json::from_slice(&json) {
            Ok(z) => z,
            Err(e) => return bad_request(&format!("invalid zones JSON: {e}")),
        },
        None => return bad_request("missing 'zones' multipart field"),
    };

    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let zones: Vec<crate::editor::redact::RedactZone> = zone_requests
            .iter()
            .map(|z| {
                let mut zone = crate::editor::redact::RedactZone::new(z.page_index, z.rect);
                if let Some(color) = z.overlay_color {
                    zone = zone.with_color(color);
                }
                zone
            })
            .collect();

        let mut editor = crate::editor::PdfEditor::open(pdf_bytes).map_err(|e| e.to_string())?;
        crate::editor::redact::apply_redactions(&mut editor, &zones).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/form/export-fdf ──────────────────────────────────────────────

/// Export form field values from a PDF as FDF bytes.
///
/// Body: raw PDF bytes.
/// Returns FDF bytes (application/vnd.fdf).
pub async fn export_fdf(body: Bytes) -> Response {
    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let doc =
            crate::parser::objects::PdfDocument::parse(body.to_vec()).map_err(|e| e.to_string())?;
        crate::forms::fdf::export_fdf(&doc).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(fdf) => (
            StatusCode::OK,
            [("content-type", "application/vnd.fdf")],
            fdf,
        )
            .into_response(),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/form/import-fdf ──────────────────────────────────────────────

/// Import FDF form-field values into a PDF.
///
/// Body: multipart with a `pdf` field (PDF bytes) and an `fdf` field (FDF bytes).
/// Returns an updated PDF with the form fields filled.
pub async fn import_fdf(multipart: Multipart) -> Response {
    let mut parts = match super::multipart::collect_named_parts(multipart).await {
        Ok(p) => p,
        Err(e) => return bad_request(&e),
    };

    let pdf_bytes = match parts.remove("pdf") {
        Some(b) => b,
        None => return bad_request("missing 'pdf' multipart field"),
    };
    let fdf_bytes = match parts.remove("fdf") {
        Some(b) => b,
        None => return bad_request("missing 'fdf' multipart field"),
    };

    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let mut editor = crate::editor::PdfEditor::open(pdf_bytes).map_err(|e| e.to_string())?;
        crate::forms::fdf::import_fdf(&mut editor, &fdf_bytes).map_err(|e| e.to_string())?;
        editor.save_new().map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/form/xfa/detect ──────────────────────────────────────────────

/// Detect whether a PDF carries an XFA (XML Forms Architecture) form.
///
/// Body: raw PDF bytes.
/// Returns JSON: `{"is_xfa": true|false}`.
pub async fn xfa_detect(body: Bytes) -> Response {
    let result: Result<bool, String> = tokio::task::spawn_blocking(move || {
        let doc =
            crate::parser::objects::PdfDocument::parse(body.to_vec()).map_err(|e| e.to_string())?;
        crate::forms::is_xfa_form(&doc).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(is_xfa) => json_response(serde_json::json!({ "is_xfa": is_xfa }).to_string()),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/form/xfa/extract ─────────────────────────────────────────────

/// Extract the raw XFA XML data from a PDF's `/AcroForm /XFA` entry.
///
/// Body: raw PDF bytes.
/// Returns the XFA XML as `text/xml`. 422 if the document has no XFA form.
pub async fn xfa_extract(body: Bytes) -> Response {
    let result: Result<String, String> = tokio::task::spawn_blocking(move || {
        let doc =
            crate::parser::objects::PdfDocument::parse(body.to_vec()).map_err(|e| e.to_string())?;
        crate::forms::extract_xfa_data(&doc).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(xml) => (StatusCode::OK, [("content-type", "text/xml")], xml).into_response(),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/form/xfa/flatten ─────────────────────────────────────────────

/// Flatten an XFA form into a static PDF by round-tripping it through
/// LibreOffice headless (`soffice --convert-to pdf`).
///
/// LibreOffice's PDF import/export does not preserve XFA interactivity, so
/// the resulting file is a flattened, non-interactive rendering. Requires
/// the `libreoffice` (or `soffice`) binary to be on `$PATH`.
///
/// Body: raw PDF bytes.
/// Returns flattened PDF bytes. 422 if the document is not an XFA form, the
/// binary is missing, or conversion fails.
pub async fn xfa_flatten(body: Bytes) -> Response {
    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let doc =
            crate::parser::objects::PdfDocument::parse(body.to_vec()).map_err(|e| e.to_string())?;
        if !crate::forms::is_xfa_form(&doc).map_err(|e| e.to_string())? {
            return Err("document is not an XFA form".to_owned());
        }
        flatten_xfa_via_libreoffice(&body)
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

/// Round-trip `pdf_bytes` through LibreOffice headless to flatten XFA content.
///
/// Writes to a uniquely-named file under the OS temp dir, converts in place
/// (LibreOffice writes its `--convert-to pdf` output back to the same path
/// since the input is already a `.pdf`), reads the result back, and removes
/// the temp file regardless of outcome.
fn flatten_xfa_via_libreoffice(pdf_bytes: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir();
    let path = dir.join(format!("pdf-core-xfa-{}-{nonce}.pdf", std::process::id()));

    std::fs::write(&path, pdf_bytes).map_err(|e| format!("failed to write temp file: {e}"))?;

    let convert_result = std::process::Command::new("libreoffice")
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(&dir)
        .arg(&path)
        .status();

    let status = match convert_result {
        Ok(s) => s,
        Err(_) => std::process::Command::new("soffice")
            .args(["--headless", "--convert-to", "pdf", "--outdir"])
            .arg(&dir)
            .arg(&path)
            .status()
            .map_err(|e| {
                let _ = std::fs::remove_file(&path);
                format!("failed to spawn libreoffice/soffice: {e}")
            })?,
    };

    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err("LibreOffice conversion failed".to_owned());
    }

    let flattened = std::fs::read(&path).map_err(|e| format!("failed to read output: {e}"));
    let _ = std::fs::remove_file(&path);
    flattened
}

// ── POST /api/v1/annotate/flatten ─────────────────────────────────────────────

/// Flatten all annotations in a PDF (burn them into page content).
///
/// Body: raw PDF bytes.
/// Returns PDF bytes with annotations removed and their appearance burned in.
pub async fn flatten_annotations(body: Bytes) -> Response {
    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let mut editor =
            crate::editor::PdfEditor::open(body.to_vec()).map_err(|e| e.to_string())?;
        crate::editor::annotation::flatten_all_annotations(&mut editor)
            .map_err(|e| e.to_string())?;
        editor.save_new().map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/watermark ────────────────────────────────────────────────────

/// JSON watermark definition for the `/watermark` endpoint.
#[derive(Deserialize)]
struct WatermarkRequest {
    /// Watermark text to render.
    text: String,
    /// X coordinate in PDF user-space points (origin bottom-left).
    x: f64,
    /// Y coordinate in PDF user-space points.
    y: f64,
    /// Font size in points.
    #[serde(default = "default_font_size")]
    font_size: f64,
    /// Fill color `[r, g, b]` in 0.0–1.0.
    #[serde(default = "default_gray")]
    color: [f64; 3],
    /// Standard PDF font name (e.g. `"Helvetica"`). Defaults to Helvetica.
    #[serde(default = "default_font")]
    font_name: String,
    /// 0-based page index to watermark; `null` applies to all pages.
    page: Option<usize>,
}

fn default_font_size() -> f64 {
    24.0
}
fn default_gray() -> [f64; 3] {
    [0.5, 0.5, 0.5]
}
fn default_font() -> String {
    "Helvetica".to_owned()
}

/// Add a text watermark to a PDF.
///
/// Body: multipart with a `pdf` field (PDF bytes) and a `watermark` field
/// (JSON `WatermarkRequest`). Applies the watermark to one or all pages.
///
/// Returns PDF bytes with the watermark added.
pub async fn watermark(multipart: Multipart) -> Response {
    let mut parts = match super::multipart::collect_named_parts(multipart).await {
        Ok(p) => p,
        Err(e) => return bad_request(&e),
    };

    let pdf_bytes = match parts.remove("pdf") {
        Some(b) => b,
        None => return bad_request("missing 'pdf' multipart field"),
    };
    let wm: WatermarkRequest = match parts.remove("watermark") {
        Some(json) => match serde_json::from_slice(&json) {
            Ok(w) => w,
            Err(e) => return bad_request(&format!("invalid watermark JSON: {e}")),
        },
        None => return bad_request("missing 'watermark' multipart field"),
    };

    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let mut editor = crate::editor::PdfEditor::open(pdf_bytes).map_err(|e| e.to_string())?;

        let page_count = editor.page_count().map_err(|e| e.to_string())?;
        let pages: Vec<usize> = match wm.page {
            Some(p) => vec![p],
            None => (0..page_count).collect(),
        };

        let font_name = wm.font_name.clone();
        let style = crate::editor::content_draw::TextStyle::new(&font_name, wm.font_size, wm.color);

        for page_idx in pages {
            crate::editor::content_draw::draw_text(
                &mut editor,
                page_idx,
                wm.x,
                wm.y,
                &wm.text,
                &style,
            )
            .map_err(|e| e.to_string())?;
        }

        editor.save_new().map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/validate-pdfa ────────────────────────────────────────────────

/// A single PDF/A violation serialised in the JSON response.
#[derive(Serialize)]
struct ViolationJson {
    rule: String,
    description: String,
    obj_id: Option<u32>,
}

/// Validate a PDF against a PDF/A conformance level.
///
/// Query parameters:
/// - `level` (default `1b`): one of `1b`, `2b`, `3b`.
///
/// Returns JSON: `{"level": "1b", "conformant": true, "violations": [...]}`.
pub async fn validate_pdfa(Query(params): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let level = params
        .get("level")
        .cloned()
        .unwrap_or_else(|| "1b".to_owned());

    let result: Result<String, String> = tokio::task::spawn_blocking(move || {
        let doc =
            crate::parser::objects::PdfDocument::parse(body.to_vec()).map_err(|e| e.to_string())?;

        let violations = match level.as_str() {
            "1b" => crate::compliance::pdfa::validate_pdfa_1b(&doc),
            "2b" => crate::compliance::pdfa::validate_pdfa_2b(&doc),
            "3b" => crate::compliance::pdfa::validate_pdfa_3b(&doc),
            other => {
                return Err(format!(
                    "unknown PDF/A level '{other}'; supported: 1b, 2b, 3b"
                ))
            }
        }
        .map_err(|e| e.to_string())?;

        let viols: Vec<ViolationJson> = violations
            .iter()
            .map(|v| ViolationJson {
                rule: v.rule.clone(),
                description: v.description.clone(),
                obj_id: v.obj_id,
            })
            .collect();

        Ok(serde_json::json!({
            "level": level,
            "conformant": viols.is_empty(),
            "violations": viols,
        })
        .to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(json) => json_response(json),
        Err(e) => unprocessable(e),
    }
}

// ── POST /api/v1/convert-pdfa ─────────────────────────────────────────────────

/// Convert a PDF to a PDF/A conformance level.
///
/// Query parameters:
/// - `level` (default `1b`): one of `1b`, `2b`, `3b`.
///
/// Body: raw PDF bytes.
/// Returns PDF/A-conformant PDF bytes.
pub async fn convert_pdfa(Query(params): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let level = params
        .get("level")
        .cloned()
        .unwrap_or_else(|| "1b".to_owned());

    let result: Result<Vec<u8>, String> = tokio::task::spawn_blocking(move || {
        let mut editor =
            crate::editor::PdfEditor::open(body.to_vec()).map_err(|e| e.to_string())?;

        match level.as_str() {
            "1b" => crate::compliance::pdfa::convert_to_pdfa_1b(&mut editor),
            "2b" => crate::compliance::pdfa::convert_to_pdfa_2b(&mut editor),
            "3b" => crate::compliance::pdfa::convert_to_pdfa_3b(&mut editor),
            other => {
                return Err(format!(
                    "unknown PDF/A level '{other}'; supported: 1b, 2b, 3b"
                ))
            }
        }
        .map_err(|e| e.to_string())?;

        editor.save_new().map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panic: {e}")));

    match result {
        Ok(pdf) => pdf_response(pdf),
        Err(e) => unprocessable(e),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::server::routes::build_router;

    #[tokio::test]
    async fn health_returns_200() {
        let app = build_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_license_returns_401() {
        let app = build_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/extract-text")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
