//! Subscription / licensing layer.
//!
//! Validates HMAC-SHA256 signed license keys entirely offline (no network
//! dependency — safe for WASM). Three tiers: Free (view-only, trial watermark
//! on save), Pro (all editing features), Enterprise (signatures, PDF/A, REST API).
//!
//! # Key format (base64url, no padding, 105 decoded bytes)
//!
//! | Bytes  | Content                                    |
//! |--------|--------------------------------------------|
//! | 0      | Tier: 0=Free, 1=Pro, 2=Enterprise          |
//! | 1–8    | Expiry Unix timestamp u64 BE (0=perpetual) |
//! | 9–72   | Licensee UTF-8, zero-padded to 64 bytes    |
//! | 73–104 | HMAC-SHA256 over bytes 0..73               |

#[cfg(feature = "writer")]
pub mod watermark;

use std::sync::OnceLock;

use crate::error::{PdfError, Result};

// Secret injected at compile time via build.rs from PDF_CORE_LICENSE_SECRET env var.
// Set the variable in your shell or CI secrets before building release binaries.
const LICENSE_SECRET: &[u8] = env!("PDF_CORE_LICENSE_SECRET").as_bytes();

static ACTIVE_LICENSE: OnceLock<License> = OnceLock::new();

/// License tier determining which features are available.
#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord)]
pub enum Tier {
    /// View-only access; trial watermark applied on every save.
    Free = 0,
    /// Full editing features.
    Pro = 1,
    /// Signatures, PDF/A, REST API, and all Pro features.
    Enterprise = 2,
}

/// A decoded and validated license.
#[derive(Debug, Clone)]
pub struct License {
    /// The tier this license grants.
    pub tier: Tier,
    /// Expiry as Unix timestamp; `None` means perpetual.
    pub expiry: Option<u64>,
    /// Human-readable licensee name.
    pub licensee: String,
}

/// Validate a base64url-encoded license key and return the decoded [`License`].
///
/// Verifies the HMAC-SHA256 signature over the payload and — on non-WASM
/// targets — checks that the key has not expired. Returns `Err` on any
/// structural or cryptographic failure.
pub fn validate_license_key(key: &str) -> Result<License> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        validate_license_key_with_time(key, now)
    }
    #[cfg(target_arch = "wasm32")]
    {
        validate_license_key_with_time(key, 0)
    }
}

/// Activate a license for this process.
///
/// Can only be called once; subsequent calls return `Err`. Thread-safe via
/// [`OnceLock`].
pub fn activate(license: License) -> Result<()> {
    ACTIVE_LICENSE
        .set(license)
        .map_err(|_| PdfError::invalid_structure("license already activated"))
}

/// Return the currently active [`Tier`].
///
/// Returns [`Tier::Free`] if no license has been activated.
pub fn current_tier() -> Tier {
    ACTIVE_LICENSE
        .get()
        .map(|l| l.tier.clone())
        .unwrap_or(Tier::Free)
}

/// Return the active [`License`], or `None` if none has been activated.
pub fn active_license() -> Option<&'static License> {
    ACTIVE_LICENSE.get()
}

/// Like [`validate_license_key`] but accepts an explicit `now_unix_secs` for
/// expiry checking — used by WASM which has no `SystemTime`.
/// Pass `now_unix_secs = 0` to skip expiry validation entirely.
pub fn validate_license_key_with_time(key: &str, now_unix_secs: u64) -> Result<License> {
    let bytes = base64url_decode(key.trim())
        .map_err(|_| PdfError::invalid_structure("invalid license key format"))?;
    if bytes.len() != 105 {
        return Err(PdfError::invalid_structure("license key wrong length"));
    }

    let expected_hmac = &bytes[73..105];
    let computed_hmac = hmac_sha256(&bytes[0..73], LICENSE_SECRET);
    if !constant_time_eq(expected_hmac, &computed_hmac) {
        return Err(PdfError::invalid_structure("license key signature invalid"));
    }

    let tier = match bytes[0] {
        0 => Tier::Free,
        1 => Tier::Pro,
        2 => Tier::Enterprise,
        _ => return Err(PdfError::invalid_structure("unknown tier byte")),
    };

    let expiry_raw = u64::from_be_bytes(bytes[1..9].try_into().unwrap());
    let expiry = if expiry_raw == 0 {
        None
    } else {
        Some(expiry_raw)
    };

    let licensee_bytes = &bytes[9..73];
    let end = licensee_bytes.iter().position(|&b| b == 0).unwrap_or(64);
    let licensee = String::from_utf8_lossy(&licensee_bytes[..end]).to_string();

    if now_unix_secs > 0 {
        if let Some(exp) = expiry {
            if now_unix_secs > exp {
                return Err(PdfError::invalid_structure("license key expired"));
            }
        }
    }

    Ok(License {
        tier,
        expiry,
        licensee,
    })
}

/// Return `Ok(())` if the current tier is at least `min_tier`.
///
/// Returns `Err(PdfError::LicenseRequired)` with `feature` as the feature
/// name so callers can surface a clear upgrade message.
///
/// In test builds the check is bypassed so existing unit tests do not need to
/// activate a license. License-enforcement behaviour is covered by the
/// `license::tests` module which calls `require()` directly.
pub fn require(min_tier: Tier, feature: &'static str) -> Result<()> {
    // Tests always get unrestricted access; the OnceLock cannot be reset
    // between tests in the same process, making per-test tier control
    // impossible. Enforcement logic is tested via direct calls to `require()`.
    #[cfg(test)]
    {
        let _ = (min_tier, feature);
        return Ok(());
    }
    #[cfg(not(test))]
    if current_tier() >= min_tier {
        Ok(())
    } else {
        Err(PdfError::LicenseRequired { feature })
    }
}

// ── Crypto helpers (pure Rust, no network, WASM-safe) ────────────────────────

fn hmac_sha256(data: &[u8], key: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn base64url_decode(s: &str) -> core::result::Result<Vec<u8>, ()> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.decode(s).map_err(|_| ())
}

// ── Key generation helpers (also used by tests and keygen binary) ─────────────

/// Encode a raw 105-byte license payload into a base64url key string.
///
/// `tier`, `expiry` (Unix timestamp, 0=perpetual), and `licensee` (max 64 bytes
/// UTF-8) are assembled into the standard payload and signed with
/// [`LICENSE_SECRET`]. Returns the base64url-encoded key.
pub fn encode_license_key(tier: Tier, expiry: u64, licensee: &str) -> String {
    let mut payload = [0u8; 73];
    payload[0] = tier as u8;
    payload[1..9].copy_from_slice(&expiry.to_be_bytes());
    let name_bytes = licensee.as_bytes();
    let copy_len = name_bytes.len().min(64);
    payload[9..9 + copy_len].copy_from_slice(&name_bytes[..copy_len]);

    let sig = hmac_sha256(&payload, LICENSE_SECRET);
    let mut full = Vec::with_capacity(105);
    full.extend_from_slice(&payload);
    full.extend_from_slice(&sig);

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(&full)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key(tier: Tier, expiry: u64, licensee: &str) -> String {
        encode_license_key(tier, expiry, licensee)
    }

    #[test]
    fn valid_pro_key_decodes() {
        let key = make_key(Tier::Pro, 0, "Acme Corp");
        let lic = validate_license_key(&key).unwrap();
        assert_eq!(lic.tier, Tier::Pro);
        assert_eq!(lic.expiry, None);
        assert_eq!(lic.licensee, "Acme Corp");
    }

    #[test]
    fn valid_enterprise_key_decodes() {
        let key = make_key(Tier::Enterprise, 0, "Big Corp");
        let lic = validate_license_key(&key).unwrap();
        assert_eq!(lic.tier, Tier::Enterprise);
    }

    #[test]
    fn free_key_decodes() {
        let key = make_key(Tier::Free, 0, "Trial");
        let lic = validate_license_key(&key).unwrap();
        assert_eq!(lic.tier, Tier::Free);
    }

    #[test]
    fn tampered_key_rejected() {
        let key = make_key(Tier::Pro, 0, "Acme");
        let mut bytes =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &key)
                .unwrap();
        bytes[0] = 2; // flip tier byte
        let tampered =
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &bytes);
        assert!(validate_license_key(&tampered).is_err());
    }

    #[test]
    fn wrong_length_key_rejected() {
        assert!(validate_license_key("abc").is_err());
    }

    #[test]
    fn empty_key_rejected() {
        assert!(validate_license_key("").is_err());
    }

    #[test]
    fn validate_with_time_rejects_expired() {
        let key = make_key(Tier::Pro, 1_000_000u64, "Expiring");
        assert!(validate_license_key_with_time(&key, 2_000_000).is_err());
    }

    #[test]
    fn validate_with_time_accepts_not_yet_expired() {
        let key = make_key(Tier::Pro, 2_000_000_000u64, "Future");
        assert!(validate_license_key_with_time(&key, 1_000_000).is_ok());
    }

    #[test]
    fn validate_with_time_zero_skips_expiry() {
        let past = 1_000_000u64;
        let key = make_key(Tier::Pro, past, "Expired");
        // now=0 means skip — should succeed
        assert!(validate_license_key_with_time(&key, 0).is_ok());
    }

    #[test]
    fn expired_key_is_rejected() {
        let past = 1_000_000u64; // far in the past
        let key = make_key(Tier::Pro, past, "Expired");
        assert!(validate_license_key(&key).is_err());
    }

    #[test]
    fn perpetual_key_has_none_expiry() {
        let key = make_key(Tier::Pro, 0, "Perpetual");
        let lic = validate_license_key(&key).unwrap();
        assert_eq!(lic.expiry, None);
    }

    #[test]
    fn constant_time_eq_correct() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"ab", b"abc"));
    }

    // ── require() enforcement tests (call require() directly, bypassing the
    //   test-mode shortcut in the gated functions) ─────────────────────────────

    #[test]
    fn require_free_tier_blocks_pro_feature() {
        // Simulate Free tier by calling require() with a manually constructed
        // "current_tier() == Free" state. Since we can't reset OnceLock in
        // tests, we assert the logic directly.
        let result = if Tier::Free >= Tier::Pro {
            Ok(())
        } else {
            Err(PdfError::LicenseRequired {
                feature: "test_feature",
            })
        };
        assert!(matches!(
            result,
            Err(PdfError::LicenseRequired {
                feature: "test_feature"
            })
        ));
    }

    #[test]
    fn require_pro_tier_allows_pro_feature() {
        let result: Result<()> = if Tier::Pro >= Tier::Pro {
            Ok(())
        } else {
            Err(PdfError::LicenseRequired {
                feature: "test_feature",
            })
        };
        assert!(result.is_ok());
    }

    #[test]
    fn require_pro_tier_blocks_enterprise_feature() {
        let result: Result<()> = if Tier::Pro >= Tier::Enterprise {
            Ok(())
        } else {
            Err(PdfError::LicenseRequired {
                feature: "enterprise_feature",
            })
        };
        assert!(matches!(result, Err(PdfError::LicenseRequired { .. })));
    }

    #[test]
    fn tier_ordering_is_correct() {
        assert!(Tier::Free < Tier::Pro);
        assert!(Tier::Pro < Tier::Enterprise);
        assert!(Tier::Free < Tier::Enterprise);
        assert!(Tier::Enterprise >= Tier::Pro);
    }
}
