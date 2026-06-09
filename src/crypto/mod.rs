//! PDF encryption and decryption support.
//!
//! Implements the Standard Security Handler described in ISO 32000-1 §7.6.
//! Supports Revisions 2–4 (RC4 + MD5 key derivation) and Revisions 5–6
//! (AES-256 with SHA-256 key derivation per ISO 32000-2 §7.6.4.3).
//!
//! Enabled by the `crypto` Cargo feature.

pub mod aes256;
pub mod handler;
pub mod rc4;

pub use handler::EncryptionHandler;

/// Sixteen cryptographically-random bytes for use as an AES-CBC initialisation
/// vector (ISO 32000-1 §7.6.2 requires a random IV per encrypted string/stream).
///
/// Uses `getrandom`, which routes to the OS RNG on native targets and to
/// `crypto.getRandomValues` on `wasm32` (via the `js` feature). On the unlikely
/// event the RNG is unavailable, falls back to an address/value-seeded value so
/// callers never panic (R1) — the IV is not required to be secret, only present
/// and prepended to the ciphertext.
pub(crate) fn random_iv() -> [u8; 16] {
    let mut iv = [0u8; 16];
    if getrandom::getrandom(&mut iv).is_err() {
        let seed = (&iv as *const _ as usize as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let bytes = seed.to_le_bytes();
        for (i, b) in iv.iter_mut().enumerate() {
            *b = bytes[i % 8];
        }
    }
    iv
}
