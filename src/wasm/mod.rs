//! WASM bridge — `#[wasm_bindgen]` wrappers for JavaScript integration.
//!
//! Enabled by the `wasm` Cargo feature.  `wasm-pack build --features wasm`
//! produces the `pkg/` directory with `pdf_core.js` and `pdf_core_bg.wasm`.
//!
//! Enable `wasm-render` instead to also include page rendering.
//! Enable `wasm-viewer` for a read-only build (no editor, writer, forms, or crypto).

pub mod document;
// Editor and writer bindings require the full wasm feature (writer + forms + crypto).
#[cfg(any(feature = "wasm", feature = "wasm-render"))]
pub mod editor;
#[cfg(any(feature = "wasm", feature = "wasm-render"))]
pub mod text_edit;

use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// Logging initialisation
// ---------------------------------------------------------------------------

/// Initialise the Rust logger for WASM.
///
/// Routes all `log::*` calls to `console.error/warn/log` in the browser and
/// installs a panic hook so Rust panics appear as readable `console.error`
/// messages instead of a generic "unreachable executed".
///
/// Call this **once** from JavaScript before any other pdf-core APIs:
/// ```js
/// import init, { init_logging } from './pkg/pdf_core.js';
/// await init();
/// init_logging();
/// ```
#[wasm_bindgen]
pub fn init_logging() {
    console_error_panic_hook::set_once();
    // Ignore if logger was already set (e.g. hot-reload calling this twice).
    let _ = console_log::init_with_level(log::Level::Debug);
    log::info!("[pdf-core] logging initialised (level=debug)");
}

// ---------------------------------------------------------------------------
// License activation
// ---------------------------------------------------------------------------

/// Validate and activate a license key for this WASM session.
///
/// `key` must be a base64url-encoded license key produced by the `keygen`
/// binary. `now_unix_secs` should be `Math.floor(Date.now() / 1000)` from
/// JavaScript — used to validate expiry. Pass `0` to skip expiry checking.
/// Returns a `JsError` if the key is malformed, the signature is invalid, or
/// a license has already been activated.
#[wasm_bindgen]
pub fn activate_license(key: &str, now_unix_secs: f64) -> Result<(), JsError> {
    let license = crate::license::validate_license_key_with_time(key, now_unix_secs as u64)
        .map_err(|e| JsError::new(&e.to_string()))?;
    crate::license::activate(license).map_err(|e| JsError::new(&e.to_string()))
}

/// Return the name of the currently active license tier.
///
/// Returns `"Free"` if no license has been activated, `"Pro"`, or
/// `"Enterprise"`.
#[wasm_bindgen]
pub fn current_license_tier() -> String {
    format!("{:?}", crate::license::current_tier())
}

/// Return a JSON object with the active license details.
///
/// Example: `{"tier":"Pro","licensee":"Acme Corp","expiry":1893456000}`
///
/// `expiry` is a Unix timestamp or `null` for perpetual licenses.
/// Returns `{"tier":"Free","licensee":"","expiry":null}` if no license is active.
#[wasm_bindgen]
pub fn current_license_info() -> String {
    match crate::license::active_license() {
        None => r#"{"tier":"Free","licensee":"","expiry":null}"#.to_string(),
        Some(lic) => {
            let tier = format!("{:?}", lic.tier);
            let licensee = lic.licensee.replace('\\', "\\\\").replace('"', "\\\"");
            let expiry = match lic.expiry {
                None => "null".to_string(),
                Some(ts) => ts.to_string(),
            };
            format!(r#"{{"tier":"{tier}","licensee":"{licensee}","expiry":{expiry}}}"#)
        }
    }
}

/// JSON array of font family names the editor can render, for the font picker.
///
/// With the `render` feature these are the embedded faces
/// ([`EMBEDDED_FONT_FAMILIES`](crate::render::font_resolver::EMBEDDED_FONT_FAMILIES));
/// without it, the three Standard-14 base families. The host populates the
/// font-family dropdown from this list so it only offers fonts that actually render.
#[wasm_bindgen]
pub fn available_fonts() -> String {
    #[cfg(feature = "render")]
    let families: &[&str] = crate::render::font_resolver::EMBEDDED_FONT_FAMILIES;
    #[cfg(not(feature = "render"))]
    let families: &[&str] = &["Helvetica", "Times-Roman", "Courier"];

    let items: Vec<String> = families.iter().map(|f| json_str(f)).collect();
    format!("[{}]", items.join(","))
}

/// Returns `true` if the PDF byte buffer has an `/Encrypt` entry in its trailer.
/// Fast: only parses the XRef trailer, not the full document.
#[wasm_bindgen]
pub fn is_pdf_encrypted(bytes: &[u8]) -> Result<bool, JsError> {
    crate::parser::objects::has_encryption_trailer(bytes).map_err(|e| JsError::new(&e.to_string()))
}

// ---------------------------------------------------------------------------
// Internal helpers (shared across submodules)
// ---------------------------------------------------------------------------

pub(crate) fn bbox_from_quad_points(qp: &[f64]) -> [f64; 4] {
    let xs: Vec<f64> = qp.iter().step_by(2).copied().collect();
    let ys: Vec<f64> = qp.iter().skip(1).step_by(2).copied().collect();
    let min_x = xs.iter().cloned().fold(f64::MAX, f64::min);
    let min_y = ys.iter().cloned().fold(f64::MAX, f64::min);
    let max_x = xs.iter().cloned().fold(f64::MIN, f64::max);
    let max_y = ys.iter().cloned().fold(f64::MIN, f64::max);
    [min_x, min_y, max_x, max_y]
}

pub(crate) fn json_opt_str(s: &Option<String>) -> String {
    match s {
        Some(v) => json_str(v),
        None => "null".to_string(),
    }
}

pub(crate) fn json_str(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{}\"", escaped)
}

pub(crate) fn outline_to_json(items: &[crate::document::outline::OutlineItem]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|item| {
            format!(
                r#"{{"title":{},"dest_page":{},"open":{},"children":{}}}"#,
                json_str(&item.title),
                item.dest_page.map_or("null".to_string(), |p| p.to_string()),
                item.open,
                outline_to_json(&item.children),
            )
        })
        .collect();
    format!("[{}]", parts.join(","))
}
