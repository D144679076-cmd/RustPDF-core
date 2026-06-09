# Phase 1 — Subscription / Licensing Layer

**Status:** Complete — 2026-06-06
**Effort:** ~5 days

## Context

Required before any public release. Offline HMAC-SHA256 validation — no network dependency in WASM. Three tiers: Free (view only, trial watermark on save), Pro (all editing features), Enterprise (signatures, PDF/A, REST API). Feature gates are added at the top of each restricted function.

## Step 1 — New module `src/license/mod.rs`

```rust
use std::sync::OnceLock;
use crate::error::{PdfError, Result};

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord)]
pub enum Tier { Free = 0, Pro = 1, Enterprise = 2 }

#[derive(Debug, Clone)]
pub struct License {
    pub tier: Tier,
    pub expiry: Option<u64>,   // Unix timestamp; None = perpetual
    pub licensee: String,
}

// IMPORTANT: The HMAC secret is embedded at compile time and must NOT be
// included in WASM output. Use a build.rs or env var: std::env!("PDF_CORE_LICENSE_SECRET")
// For now use a placeholder — replace before production.
const LICENSE_SECRET: &[u8] = b"REPLACE_BEFORE_PRODUCTION_DO_NOT_SHIP_THIS";

static ACTIVE_LICENSE: OnceLock<License> = OnceLock::new();

/// Validate a license key string and return the decoded License.
///
/// Key format (base64url, no padding):
///   byte 0:    tier (0=Free, 1=Pro, 2=Enterprise)
///   bytes 1-8: expiry u64 big-endian (0 = perpetual)
///   bytes 9-72: licensee UTF-8 zero-padded to 64 bytes
///   bytes 73-104: HMAC-SHA256(bytes 0..73, LICENSE_SECRET)
pub fn validate_license_key(key: &str) -> Result<License> {
    let bytes = base64url_decode(key.trim())
        .map_err(|_| PdfError::invalid_structure("invalid license key format"))?;
    if bytes.len() != 105 {
        return Err(PdfError::invalid_structure("license key wrong length"));
    }
    // Verify HMAC
    let expected_hmac = &bytes[73..105];
    let computed_hmac = hmac_sha256(&bytes[0..73], LICENSE_SECRET);
    if !constant_time_eq(expected_hmac, &computed_hmac) {
        return Err(PdfError::invalid_structure("license key signature invalid"));
    }
    let tier = match bytes[0] {
        0 => Tier::Free,
        1 => Tier::Pro,
        2 => Tier::Enterprise,
        _ => return Err(PdfError::invalid_structure("unknown tier")),
    };
    let expiry_raw = u64::from_be_bytes(bytes[1..9].try_into().unwrap());
    let expiry = if expiry_raw == 0 { None } else { Some(expiry_raw) };
    let licensee_bytes = &bytes[9..73];
    let end = licensee_bytes.iter().position(|&b| b == 0).unwrap_or(64);
    let licensee = String::from_utf8_lossy(&licensee_bytes[..end]).to_string();

    // Check expiry (WASM has no real clock; skip expiry check in wasm32)
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(exp) = expiry {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now > exp {
            return Err(PdfError::invalid_structure("license key expired"));
        }
    }

    Ok(License { tier, expiry, licensee })
}

/// Activate a license for this process. Can only be called once.
pub fn activate(license: License) -> Result<()> {
    ACTIVE_LICENSE.set(license)
        .map_err(|_| PdfError::invalid_structure("license already activated"))
}

/// Return the currently active tier (Free if no license activated).
pub fn current_tier() -> Tier {
    ACTIVE_LICENSE.get().map(|l| l.tier.clone()).unwrap_or(Tier::Free)
}

/// Return Ok if current tier >= min_tier, else Err with feature name.
pub fn require(min_tier: Tier, feature: &'static str) -> Result<()> {
    if current_tier() >= min_tier {
        Ok(())
    } else {
        Err(PdfError::LicenseRequired { feature })
    }
}

// ── Crypto helpers (pure Rust, no external deps) ──────────────────────────

fn hmac_sha256(data: &[u8], key: &[u8]) -> Vec<u8> {
    // HMAC-SHA256 implemented with sha2 crate (already in Cargo.toml under crypto feature)
    // If crypto feature not enabled, use a fallback compile-error
    #[cfg(feature = "crypto")]
    {
        use sha2::{Sha256, Digest};
        use hmac::{Hmac, Mac};
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key error");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }
    #[cfg(not(feature = "crypto"))]
    {
        // Fallback: if crypto feature disabled, license validation not available
        compile_error!("License validation requires the 'crypto' feature");
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn base64url_decode(s: &str) -> core::result::Result<Vec<u8>, ()> {
    // Use the `base64` crate if available, else hand-roll
    // base64 crate is WASM-safe — add to Cargo.toml: base64 = "0.21"
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    URL_SAFE_NO_PAD.decode(s).map_err(|_| ())
}
```

## Step 2 — Update `src/error.rs`

Add to `PdfError` enum:
```rust
/// A Pro or Enterprise license is required to use this feature.
#[error("feature '{feature}' requires a Pro or Enterprise license")]
LicenseRequired { feature: &'static str },
```

## Step 3 — Update `src/lib.rs`

```rust
pub mod license;
```

## Step 4 — Add `hmac` to Cargo.toml

```toml
[dependencies]
hmac = { version = "0.12", optional = true }
base64 = { version = "0.21", optional = true }

[features]
crypto = ["dep:md-5", "dep:sha2", "dep:aes", "dep:cbc", "dep:hmac", "dep:base64"]
wasm = ["dep:wasm-bindgen", ..., "crypto"]  # wasm always includes crypto for license checks
```

## Step 5 — Trial Watermark `src/license/watermark.rs`

```rust
use crate::editor::PdfEditor;
use crate::writer::content_builder::ContentBuilder;
use crate::writer::streams::make_flate_stream;
use crate::parser::{PdfObject, PdfDict};
use crate::error::Result;

/// Burn a diagonal "UNLICENSED — pdf-core trial" text overlay onto every page.
pub fn apply_trial_watermark(editor: &mut PdfEditor) -> Result<()> {
    let page_count = editor.page_count()?;
    for i in 0..page_count {
        apply_to_page(editor, i)?;
    }
    Ok(())
}

fn apply_to_page(editor: &mut PdfEditor, page_index: usize) -> Result<()> {
    let (page_id, page_dict) = editor.get_page_dict(page_index)?;
    // Get page dimensions from MediaBox
    let (pw, ph) = match page_dict.get("MediaBox") {
        Some(PdfObject::Array(a)) if a.len() == 4 => {
            let w = to_f64(&a[2]) - to_f64(&a[0]);
            let h = to_f64(&a[3]) - to_f64(&a[1]);
            (w, h)
        }
        _ => (612.0, 792.0),
    };

    // Center of page, 45° rotation
    let cx = pw / 2.0;
    let cy = ph / 2.0;
    let font_size = (pw / 12.0).min(36.0).max(18.0);

    // Content stream: save state, apply rotation CTM, draw gray text, restore
    // CTM for 45° rotation around (cx, cy):
    //   1. translate to center: [1 0 0 1 cx cy]
    //   2. rotate 45°: [cos45 sin45 -sin45 cos45 0 0] ≈ [0.707 0.707 -0.707 0.707 0 0]
    //   3. translate text so it centers: move_to(-text_width/2, 0)
    let text = "UNLICENSED - pdf-core trial";
    let approx_text_width = font_size * 0.5 * text.len() as f64;

    let mut cb = ContentBuilder::new();
    cb.save()
      .concat_matrix(0.707, 0.707, -0.707, 0.707, cx, cy)
      .set_fill_gray(0.75)
      .begin_text()
      .set_text_font("Helv", font_size)
      .set_text_position(-approx_text_width / 2.0, -font_size / 2.0)
      .show_text(text.as_bytes())
      .end_text()
      .restore();

    let bytes = cb.build();
    let stream = make_flate_stream(&bytes, PdfDict::new())?;
    let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

    // Append to /Contents
    let mut updated_page = page_dict.clone();
    let new_contents = match updated_page.get("Contents") {
        Some(PdfObject::Array(arr)) => {
            let mut a = arr.clone(); a.push(PdfObject::Reference(stream_id, 0)); PdfObject::Array(a)
        }
        Some(single) => PdfObject::Array(vec![single.clone(), PdfObject::Reference(stream_id, 0)]),
        None => PdfObject::Reference(stream_id, 0),
    };
    updated_page.insert("Contents".to_owned(), new_contents);
    editor.replace_object(page_id, PdfObject::Dictionary(updated_page));
    Ok(())
}

fn to_f64(o: &PdfObject) -> f64 {
    match o { PdfObject::Real(r) => *r, PdfObject::Integer(i) => *i as f64, _ => 0.0 }
}
```

## Step 6 — WASM init in `src/wasm/mod.rs`

```rust
#[wasm_bindgen]
pub fn activate_license(key: &str) -> Result<(), JsError> {
    let license = crate::license::validate_license_key(key)
        .map_err(|e| JsError::new(&e.to_string()))?;
    crate::license::activate(license)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn current_license_tier() -> String {
    format!("{:?}", crate::license::current_tier())
}
```

## Step 7 — Wire watermark into `WasmEditor::save()` in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn save(&mut self) -> Result<Vec<u8>, JsError> {
    if crate::license::current_tier() == crate::license::Tier::Free {
        crate::license::watermark::apply_trial_watermark(&mut self.editor)
            .map_err(|e| JsError::new(&e.to_string()))?;
    }
    // ... existing save logic unchanged
}
```

## Step 8 — Key generation binary `src/bin/keygen.rs`

```rust
//! License key generator. NOT included in WASM build.
//! Usage: cargo run --bin keygen -- --tier pro --licensee "Acme Corp" --expiry 2027-01-01
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Parse --tier, --licensee, --expiry args
    // Build 105-byte payload, compute HMAC-SHA256 with LICENSE_SECRET
    // Output base64url-encoded key string
    // ...
}
```

## Step 9 — Add feature gates to all restricted functions

At the top of each function, add one line:
```rust
crate::license::require(crate::license::Tier::Pro, "feature_name")?;
```

| Function | File | Gate |
|----------|------|------|
| `search_document` | `src/text/search.rs` | Pro |
| `search_page` | `src/text/search.rs` | Pro |
| `set_text_field` | `src/forms/filler.rs` | Pro |
| `set_checkbox` | `src/forms/filler.rs` | Pro |
| `set_combo_or_list` | `src/forms/filler.rs` | Pro |
| `flatten_annotations` | `src/editor/annotation.rs` | Pro |
| `flatten_all_annotations` | `src/editor/annotation.rs` | Pro |
| `extract_pages` | `src/editor/merge.rs` | Pro |
| `apply_redactions` | `src/editor/redact.rs` | Pro |
| `add_annotation` | `src/editor/annotation.rs` | Pro |
| `MergeBuilder::merge` | `src/editor/merge.rs` | Pro |

## Tests

```rust
#[test]
fn free_tier_cannot_search() {
    // No license activated → current_tier() == Free
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let doc = PdfDocument::parse(data).unwrap();
    let result = crate::text::search_document(&doc, "test", true);
    assert!(matches!(result, Err(PdfError::LicenseRequired { .. })));
}

#[test]
fn valid_pro_key_activates() {
    let key = generate_test_key(Tier::Pro, None, "Test");  // test helper
    crate::license::validate_license_key(&key).unwrap();
}
```

## Verification

```bash
cargo test --features crypto -- license
cargo build --target wasm32-unknown-unknown --features wasm
```
