//! AES-256 encryption support for PDF Revision 5 and 6.
//!
//! Implements key derivation and password verification per ISO 32000-2 §7.6.4.3.
//! R5 uses straightforward SHA-256; R6 uses the iterative "Algorithm 2.B".

use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::error::{PdfError, Result};

/// Verify the user password for Revision 5.
///
/// Computes `SHA-256(password[0..127] || U[32..40])` and compares with `U[0..32]`.
pub fn verify_user_password_r5(password: &[u8], u_entry: &[u8]) -> bool {
    if u_entry.len() < 48 {
        return false;
    }
    let truncated = truncate_password(password);
    let validation_salt = &u_entry[32..40];

    let mut hasher = Sha256::new();
    hasher.update(&truncated);
    hasher.update(validation_salt);
    let hash: [u8; 32] = hasher.finalize().into();

    hash == u_entry[..32]
}

/// Verify the user password for Revision 6.
///
/// Uses Algorithm 2.B (iterative hash) for validation.
///
/// Per ISO 32000-2 §7.6.4.3.2 the password is a text string: both the
/// SASLprep-normalised form and the raw bytes are tried (see
/// [`super::password::password_candidates_for_r6`]).
pub fn verify_user_password_r6(password: &[u8], u_entry: &[u8]) -> bool {
    let candidates = super::password::password_candidates_for_r6(password);
    if candidates.is_empty() || u_entry.len() < 48 {
        return false;
    }
    let validation_salt = &u_entry[32..40];

    candidates.iter().any(|candidate| {
        let truncated = truncate_password(candidate);
        let hash = compute_hash_r6(&truncated, validation_salt, &[]);
        hash == u_entry[..32]
    })
}

/// Derive the 32-byte file encryption key for Revision 5.
///
/// Steps:
/// 1. Verify password using validation salt `U[32..40]`
/// 2. Compute intermediate key: `SHA-256(password || U[40..48])`
/// 3. Decrypt `/UE` with AES-256-CBC (IV = zero) using intermediate key
pub fn derive_file_key_r5(password: &[u8], u_entry: &[u8], ue_entry: &[u8]) -> Result<Vec<u8>> {
    if !verify_user_password_r5(password, u_entry) {
        return Err(PdfError::Encrypted { offset: 0 });
    }
    if u_entry.len() < 48 || ue_entry.len() < 32 {
        return Err(PdfError::filter_error(0, "AES-256 R5: /U or /UE too short"));
    }

    let truncated = truncate_password(password);
    let key_salt = &u_entry[40..48];

    let mut hasher = Sha256::new();
    hasher.update(&truncated);
    hasher.update(key_salt);
    let intermediate_key: [u8; 32] = hasher.finalize().into();

    // Decrypt UE with AES-256-CBC, IV = 0
    let iv = [0u8; 16];
    aes256_cbc_decrypt_raw(&intermediate_key, &iv, ue_entry)
}

/// Derive the 32-byte file encryption key for Revision 6.
///
/// Uses Algorithm 2.B for both validation and key derivation. Both the
/// SASLprep-normalised password (PDF 2.0 §7.6.4.3.2 text-string path) and the
/// raw bytes are tried, so legacy extension-era R6 files still open.
pub fn derive_file_key_r6(password: &[u8], u_entry: &[u8], ue_entry: &[u8]) -> Result<Vec<u8>> {
    if u_entry.len() < 48 || ue_entry.len() < 32 {
        return Err(PdfError::filter_error(0, "AES-256 R6: /U or /UE too short"));
    }
    let validation_salt = &u_entry[32..40];
    let key_salt = &u_entry[40..48];
    let iv = [0u8; 16];

    for candidate in super::password::password_candidates_for_r6(password) {
        let truncated = truncate_password(&candidate);
        if compute_hash_r6(&truncated, validation_salt, &[]) != u_entry[..32] {
            continue;
        }
        let intermediate_key = compute_hash_r6(&truncated, key_salt, &[]);
        // Decrypt UE with AES-256-CBC, IV = 0
        return aes256_cbc_decrypt_raw(&intermediate_key, &iv, ue_entry);
    }
    // No candidate verified — wrong password (R7: offset carried).
    Err(PdfError::Encrypted { offset: 0 })
}

/// AES-256-CBC decrypt with explicit IV (first 16 bytes of data if not provided separately).
///
/// Used for per-object decryption where IV is prepended to ciphertext.
pub fn aes256_cbc_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 16 {
        return Err(PdfError::filter_error(
            0,
            "AES-256 encrypted data too short for IV",
        ));
    }
    let (iv, body) = data.split_at(16);
    if body.is_empty() {
        return Ok(Vec::new());
    }
    aes256_cbc_decrypt_padded(key, iv, body)
}

/// AES-256-CBC encrypt with a fresh random IV (inverse of [`aes256_cbc_decrypt`]).
///
/// Generates a 16-byte IV, PKCS7-pads `plaintext`, CBC-encrypts with `key` (the
/// file encryption key — V5/R6 uses it directly, no per-object key), and returns
/// `IV || ciphertext` as PDF readers expect (ISO 32000-1 §7.6.2).
pub fn aes256_cbc_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    use aes::Aes256;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    let iv = super::random_iv();
    // PKCS7 always appends 1..=16 bytes, so the buffer must hold one extra block.
    let padded_len = (plaintext.len() / 16 + 1) * 16;
    let mut buf = vec![0u8; padded_len];
    buf[..plaintext.len()].copy_from_slice(plaintext);

    let enc = cbc::Encryptor::<Aes256>::new_from_slices(key, &iv)
        .map_err(|_| PdfError::filter_error(0, "AES-256: invalid key or IV length"))?;
    let ct = enc
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .map_err(|_| PdfError::filter_error(0, "AES-256 encryption failed"))?;

    let mut out = Vec::with_capacity(16 + ct.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(ct);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Truncate password to 127 bytes per spec.
fn truncate_password(password: &[u8]) -> Vec<u8> {
    let len = password.len().min(127);
    password[..len].to_vec()
}

/// AES-256-CBC decrypt with PKCS7 padding removal.
fn aes256_cbc_decrypt_padded(key: &[u8], iv: &[u8], body: &[u8]) -> Result<Vec<u8>> {
    use aes::Aes256;
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

    let dec = cbc::Decryptor::<Aes256>::new_from_slices(key, iv)
        .map_err(|_| PdfError::filter_error(0, "AES-256: invalid key or IV length"))?;
    let mut buf = body.to_vec();
    let n = dec
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| PdfError::filter_error(0, "AES-256 decryption failed (bad padding?)"))?
        .len();
    buf.truncate(n);
    Ok(buf)
}

/// AES-256-CBC decrypt without padding removal (for key unwrapping).
fn aes256_cbc_decrypt_raw(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::Aes256;
    use cbc::cipher::{BlockDecryptMut, KeyIvInit};

    if data.is_empty() {
        return Ok(Vec::new());
    }

    let dec = cbc::Decryptor::<Aes256>::new_from_slices(key, iv)
        .map_err(|_| PdfError::filter_error(0, "AES-256: invalid key or IV length"))?;

    let mut buf = data.to_vec();
    // Pad to block boundary if needed (shouldn't be for spec-compliant files)
    let block_size = 16;
    let remainder = buf.len() % block_size;
    if remainder != 0 {
        buf.resize(buf.len() + block_size - remainder, 0);
    }

    dec.decrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|_| PdfError::filter_error(0, "AES-256 raw decryption failed"))?;

    buf.truncate(data.len().min(32));
    Ok(buf)
}

/// Algorithm 2.B (ISO 32000-2): iterative hash for R6 password verification.
///
/// Performs rounds of SHA-256/384/512 until a termination condition is met
/// (minimum 64 rounds, then check last byte of final hash).
fn compute_hash_r6(password: &[u8], salt: &[u8], u_entry: &[u8]) -> [u8; 32] {
    // Initial hash: SHA-256(password || salt || U)
    let mut hasher = Sha256::new();
    hasher.update(password);
    hasher.update(salt);
    hasher.update(u_entry);
    let mut k: Vec<u8> = hasher.finalize().to_vec();

    let mut round = 0u32;
    loop {
        // Build input: repeat (password || K || U) 64 times
        let mut input = Vec::with_capacity((password.len() + k.len() + u_entry.len()) * 64);
        for _ in 0..64 {
            input.extend_from_slice(password);
            input.extend_from_slice(&k);
            input.extend_from_slice(u_entry);
        }

        // AES-128-CBC encrypt with key=K[0..16], IV=K[16..32]
        let encrypted = aes128_cbc_encrypt_r6(&k[..16], &k[16..32], &input);

        // Determine which SHA to use based on first 16 bytes mod 3
        let sum: u32 = encrypted[..16].iter().map(|&b| b as u32).sum();
        k = match sum % 3 {
            0 => {
                let h: [u8; 32] = Sha256::digest(&encrypted).into();
                h.to_vec()
            }
            1 => {
                let h: [u8; 48] = Sha384::digest(&encrypted).into();
                h.to_vec()
            }
            _ => {
                let h: [u8; 64] = Sha512::digest(&encrypted).into();
                h.to_vec()
            }
        };

        round += 1;
        // Termination: at least 64 rounds, then check last byte of encrypted
        if round >= 64 {
            let last_byte = *encrypted.last().unwrap_or(&0);
            if last_byte as u32 + 32 <= round {
                break;
            }
        }
    }

    let mut result = [0u8; 32];
    result.copy_from_slice(&k[..32]);
    result
}

/// AES-128-CBC encrypt (used internally by Algorithm 2.B).
fn aes128_cbc_encrypt_r6(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    use aes::Aes128;
    use cbc::cipher::{block_padding::NoPadding, BlockEncryptMut, KeyIvInit};

    // Pad data to 16-byte boundary
    let block_size = 16;
    let padded_len = data.len().div_ceil(block_size) * block_size;
    let mut buf = vec![0u8; padded_len];
    buf[..data.len()].copy_from_slice(data);

    let enc = cbc::Encryptor::<Aes128>::new_from_slices(key, iv).unwrap();
    enc.encrypt_padded_mut::<NoPadding>(&mut buf, padded_len)
        .unwrap();
    buf
}

// ---------------------------------------------------------------------------
// Writer-side entry computation (Phase P4: encrypt on write)
// ---------------------------------------------------------------------------
// Computes fresh /O, /U, /UE, /OE and /Perms for a V=5 (R=5/6) /Encrypt
// dictionary, mirroring qpdf's `Encryption::compute_encryption_parameters_V5`.
// The reader-side verification functions above are the round-trip oracle.

/// Computed V5 encryption-dictionary entries plus the file encryption key.
#[derive(Debug, Clone)]
pub struct R5Entries {
    /// Computed `/U` value — 48 bytes: hash(32) || validation salt(8) || key salt(8).
    pub u_entry: [u8; 48],
    /// Computed `/O` value — 48 bytes, same layout, owner-password side.
    pub o_entry: [u8; 48],
    /// Computed `/UE` value — the 32-byte file key encrypted with the
    /// user-password intermediate key (AES-256-CBC, zero IV, no padding).
    pub ue_entry: [u8; 32],
    /// Computed `/OE` value — the file key encrypted with the
    /// owner-password intermediate key.
    pub oe_entry: [u8; 32],
    /// Computed `/Perms` value — AES-256-encrypted 16-byte permissions block.
    pub perms_entry: [u8; 16],
    /// The 32-byte file encryption key (kept in the writer, not in the file).
    pub file_key: [u8; 32],
}

/// Cryptographically random bytes for salts and file keys.
///
/// Mirrors [`super::random_iv`]'s no-panic fallback (R1): on the unlikely
/// RNG failure a deterministic fallback fills the buffer — values are
/// secret, but the file remains readable rather than the write failing.
fn random_bytes(buf: &mut [u8]) {
    if getrandom::getrandom(buf).is_err() {
        let seed = (buf.as_ptr() as usize as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let bytes = seed.to_le_bytes();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = bytes[i % 8].wrapping_add(i as u8);
        }
    }
}

/// AES-256-CBC encrypt with a **zero IV and no padding** (32-byte input →
/// 32-byte output). This is the mode `/UE` and `/OE` use (qpdf
/// `process_with_aes(..., useZeroIV, disablePadding)`); the zero IV is
/// implicit — it is NOT written to the output.
fn aes256_cbc_encrypt_zero_iv_32(key: &[u8; 32], plaintext: &[u8; 32]) -> Result<[u8; 32]> {
    use aes::Aes256;
    use cbc::cipher::{block_padding::NoPadding, BlockEncryptMut, KeyIvInit};

    let mut buf = *plaintext;
    let iv = [0u8; 16];
    let enc = cbc::Encryptor::<Aes256>::new_from_slices(key, &iv)
        .map_err(|_| PdfError::filter_error(0, "AES-256: invalid key length"))?;
    enc.encrypt_padded_mut::<NoPadding>(&mut buf, 32)
        .map_err(|_| PdfError::filter_error(0, "AES-256 encryption failed"))?;
    Ok(buf)
}

/// AES-256 single-block encrypt (ECB-equivalent for one block) — used for
/// `/Perms` (qpdf: AES-256, zero IV, no padding, exactly 16 bytes in/out).
fn aes256_encrypt_block(key: &[u8; 32], block: &[u8; 16]) -> Result<[u8; 16]> {
    use aes::cipher::{BlockEncrypt, KeyInit};
    use aes::Aes256;

    let cipher = Aes256::new_from_slice(key)
        .map_err(|_| PdfError::filter_error(0, "AES-256: invalid key length"))?;
    let mut out = *block;
    cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
        &mut out,
    ));
    Ok(out)
}

/// Build the clear 16-byte `/Perms` block (qpdf
/// `compute_Perms_value_V5_clear`, ISO 32000-2 §7.6.4.3.3 Algorithm 13):
/// bytes 0-3 = /P little-endian, bytes 4-7 = FF FF FF FF, bytes 8-11 =
/// "Tadb" (byte 8 = 'F' when `/EncryptMetadata` is false — we always emit
/// `true`, so 'T'), bytes 12-15 = random (validators ignore them).
fn perms_clear_block(p_flags: i32) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[..4].copy_from_slice(&p_flags.to_le_bytes());
    block[4..8].copy_from_slice(&[0xFF; 4]);
    block[8..12].copy_from_slice(b"Tadb");
    random_bytes(&mut block[12..16]);
    block
}

/// Shared V5 compute core. `hash(password, salt, udata)` selects R5 (plain
/// SHA-256) or R6 (Algorithm 2.B); `password_bytes` maps the raw password to
/// the bytes the hash sees (R6: SASLprep-normalised candidate; R5: raw).
fn compute_v5_entries(
    user_password: &[u8],
    owner_password: &[u8],
    p_flags: i32,
    hash: impl Fn(&[u8], &[u8], &[u8]) -> [u8; 32],
) -> Result<R5Entries> {
    // The file key itself is 32 random bytes (ISO 32000-2 Algorithm 10 step 2).
    let mut file_key = [0u8; 32];
    random_bytes(&mut file_key);

    // Algorithm 11 (user side): U = hash(pw, uvs, "") || uvs || uks.
    let mut uvs = [0u8; 8];
    let mut uks = [0u8; 8];
    random_bytes(&mut uvs);
    random_bytes(&mut uks);
    let mut u_entry = [0u8; 48];
    u_entry[..32].copy_from_slice(&hash(user_password, &uvs, &[]));
    u_entry[32..40].copy_from_slice(&uvs);
    u_entry[40..48].copy_from_slice(&uks);

    // User intermediate key + /UE = AES-256-CBC(zero IV, ik, file_key).
    let user_ik = hash(user_password, &uks, &[]);
    let ue_entry = aes256_cbc_encrypt_zero_iv_32(&user_ik, &file_key)?;

    // Algorithm 8 (owner side): O = hash(owner pw, ovs, U[0..48]) || ovs || oks.
    let mut ovs = [0u8; 8];
    let mut oks = [0u8; 8];
    random_bytes(&mut ovs);
    random_bytes(&mut oks);
    let mut o_entry = [0u8; 48];
    o_entry[..32].copy_from_slice(&hash(owner_password, &ovs, &u_entry));
    o_entry[32..40].copy_from_slice(&ovs);
    o_entry[40..48].copy_from_slice(&oks);

    // Owner intermediate key + /OE.
    let owner_ik = hash(owner_password, &oks, &u_entry);
    let oe_entry = aes256_cbc_encrypt_zero_iv_32(&owner_ik, &file_key)?;

    // Algorithm 13: /Perms = AES-256(file_key, clear block).
    let perms_entry = aes256_encrypt_block(&file_key, &perms_clear_block(p_flags))?;

    Ok(R5Entries {
        u_entry,
        o_entry,
        ue_entry,
        oe_entry,
        perms_entry,
        file_key,
    })
}

/// Compute all V=5/R=5 encryption entries (plain SHA-256 hashing).
///
/// Passwords are truncated to 127 bytes per spec; no Unicode normalisation
/// applies at R5 (it is an R6 concept).
pub fn compute_entries_r5(
    user_password: &[u8],
    owner_password: &[u8],
    p_flags: i32,
) -> Result<R5Entries> {
    compute_v5_entries(
        truncate_password(user_password).as_slice(),
        truncate_password(owner_password).as_slice(),
        p_flags,
        |password, salt, udata| {
            let mut hasher = Sha256::new();
            hasher.update(password);
            hasher.update(salt);
            hasher.update(udata);
            hasher.finalize().into()
        },
    )
}

/// Compute all V=5/R=6 encryption entries (Algorithm 2.B iterative hash).
///
/// Both passwords go through the R6 candidate pipeline (SASLprep
/// normalisation, ISO 32000-2 §7.6.4.3.2) so the written dictionary
/// verifies against the same normalised bytes the reader will try first.
pub fn compute_entries_r6(
    user_password: &[u8],
    owner_password: &[u8],
    p_flags: i32,
) -> Result<R5Entries> {
    let user_candidates = super::password::password_candidates_for_r6(user_password);
    let owner_candidates = super::password::password_candidates_for_r6(owner_password);
    let user = user_candidates
        .first()
        .map(|c| truncate_password(c))
        .ok_or_else(|| PdfError::filter_error(0, "R6: empty user password candidates"))?;
    let owner = owner_candidates
        .first()
        .map(|c| truncate_password(c))
        .ok_or_else(|| PdfError::filter_error(0, "R6: empty owner password candidates"))?;
    compute_v5_entries(user.as_slice(), owner.as_slice(), p_flags, compute_hash_r6)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_password() {
        let short = b"hello";
        assert_eq!(truncate_password(short).len(), 5);

        let long = vec![b'x'; 200];
        assert_eq!(truncate_password(&long).len(), 127);
    }

    #[test]
    fn aes256_cbc_encrypt_decrypt_roundtrip() {
        let key = [0x11u8; 32];
        let plaintext = b"AES-256 round-trip: edit a block, save, reopen.".to_vec();

        let ct = aes256_cbc_encrypt(&key, &plaintext).expect("encrypt");
        // Output is IV(16) || ciphertext; ciphertext is block-aligned.
        assert!(ct.len() >= 16 + 16);
        assert_eq!((ct.len() - 16) % 16, 0);
        // Decrypting yields the original bytes.
        let pt = aes256_cbc_decrypt(&key, &ct).expect("decrypt");
        assert_eq!(pt, plaintext);
        // A fresh random IV each call → different ciphertext for the same input.
        let ct2 = aes256_cbc_encrypt(&key, &plaintext).expect("encrypt2");
        assert_ne!(ct, ct2, "IV should be random per call");
    }

    #[test]
    fn aes256_cbc_encrypt_empty_roundtrip() {
        let key = [0x22u8; 32];
        let ct = aes256_cbc_encrypt(&key, b"").expect("encrypt empty");
        // IV + one PKCS7 padding block.
        assert_eq!(ct.len(), 32);
        let pt = aes256_cbc_decrypt(&key, &ct).expect("decrypt empty");
        assert_eq!(pt, b"");
    }

    #[test]
    fn test_verify_r5_known_vectors() {
        // Construct synthetic U entry: SHA-256("" || validation_salt) as first 32 bytes
        let password = b"";
        let validation_salt = [0x01u8; 8];
        let key_salt = [0x02u8; 8];

        let mut hasher = Sha256::new();
        hasher.update(password);
        hasher.update(validation_salt);
        let hash: [u8; 32] = hasher.finalize().into();

        let mut u_entry = Vec::with_capacity(48);
        u_entry.extend_from_slice(&hash);
        u_entry.extend_from_slice(&validation_salt);
        u_entry.extend_from_slice(&key_salt);

        assert!(verify_user_password_r5(password, &u_entry));
        assert!(!verify_user_password_r5(b"wrong", &u_entry));
    }

    #[test]
    fn test_verify_r5_too_short_u() {
        assert!(!verify_user_password_r5(b"test", &[0u8; 10]));
    }

    #[test]
    fn test_aes256_cbc_decrypt_too_short() {
        let key = [0u8; 32];
        let result = aes256_cbc_decrypt(&key, &[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn test_aes256_cbc_decrypt_empty_body() {
        let key = [0u8; 32];
        let iv = [0u8; 16]; // just IV, no body
        let result = aes256_cbc_decrypt(&key, &iv);
        assert_eq!(result.unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn test_derive_file_key_r5_wrong_password() {
        let password = b"correct";
        let validation_salt = [0x01u8; 8];
        let key_salt = [0x02u8; 8];

        let mut hasher = Sha256::new();
        hasher.update(password);
        hasher.update(validation_salt);
        let hash: [u8; 32] = hasher.finalize().into();

        let mut u_entry = Vec::with_capacity(48);
        u_entry.extend_from_slice(&hash);
        u_entry.extend_from_slice(&validation_salt);
        u_entry.extend_from_slice(&key_salt);

        let ue_entry = [0u8; 32];
        let result = derive_file_key_r5(b"wrong", &u_entry, &ue_entry);
        assert!(result.is_err());
    }

    // --- writer-side entry computation (Phase P4) ----------------------------

    #[test]
    fn compute_r5_roundtrips_through_reader_verify() {
        let entries = compute_entries_r5(b"user-secret", b"owner-secret", -4).expect("compute R5");

        // Reader side: verify user password and recover the same file key.
        assert!(verify_user_password_r5(b"user-secret", &entries.u_entry));
        let key = derive_file_key_r5(b"user-secret", &entries.u_entry, &entries.ue_entry)
            .expect("derive from computed entries");
        assert_eq!(key, entries.file_key.to_vec());

        // Owner path: hash_V5(owner, O[32..40] (validation salt), U[0..48])
        // must equal O[0..32], and /OE must decrypt (with the key salt,
        // O[40..48]) to the same file key.
        let mut hasher = Sha256::new();
        hasher.update(truncate_password(b"owner-secret"));
        hasher.update(&entries.o_entry[32..40]);
        hasher.update(&entries.u_entry[..48]);
        let owner_hash: [u8; 32] = hasher.finalize().into();
        assert_eq!(owner_hash, entries.o_entry[..32]);

        let mut hasher = Sha256::new();
        hasher.update(truncate_password(b"owner-secret"));
        hasher.update(&entries.o_entry[40..48]);
        hasher.update(&entries.u_entry[..48]);
        let owner_ik: [u8; 32] = hasher.finalize().into();
        let unwrapped =
            aes256_cbc_decrypt_raw(&owner_ik, &[0u8; 16], &entries.oe_entry).expect("OE decrypt");
        assert_eq!(unwrapped.len(), 32);
        assert_eq!(unwrapped, entries.file_key.to_vec());
    }

    #[test]
    fn compute_r6_roundtrips_through_reader_verify() {
        let entries = compute_entries_r6(b"user-secret", b"owner-secret", -4).expect("compute R6");

        assert!(verify_user_password_r6(b"user-secret", &entries.u_entry));
        let key = derive_file_key_r6(b"user-secret", &entries.u_entry, &entries.ue_entry)
            .expect("derive R6 from computed entries");
        assert_eq!(key, entries.file_key.to_vec());
        // Wrong password must not verify.
        assert!(!verify_user_password_r6(b"wrong", &entries.u_entry));
    }

    #[test]
    fn perms_clear_block_layout() {
        let block = perms_clear_block(-4);
        assert_eq!(&block[..4], &(-4i32).to_le_bytes());
        assert_eq!(&block[4..8], &[0xFF; 4]);
        assert_eq!(&block[8..12], b"Tadb");
    }
}
