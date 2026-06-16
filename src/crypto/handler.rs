//! PDF Standard Security Handler: key derivation and per-object decryption.
//!
//! Supports Revisions 2–4 (RC4 + MD5) per ISO 32000-1 §7.6.3.
//! Revision 5–6 (AES-256) detection returns `PdfError::Encrypted`.

use md5::{Digest, Md5};

use crate::error::{PdfError, Result};
use crate::parser::objects::PdfDict;

use super::rc4::Rc4;

/// The 32-byte password padding string from ISO 32000-1 §7.6.3.3.
const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Encryption algorithm variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptAlgorithm {
    /// RC4 with 40-bit key (V=1, R=2).
    Rc4_40,
    /// RC4 with 128-bit key (V=2, R=3/4).
    Rc4_128,
    /// AES-128 (V=4, R=4).
    Aes128,
    /// AES-256 (V=5, R=5/6) — not yet decrypted; parse returns Encrypted.
    Aes256,
}

/// PDF document operation permissions encoded in the `/P` integer (ISO 32000-1 §7.6.3.3 Table 22).
#[derive(Debug, Clone, Copy, Default)]
pub struct Permissions {
    /// Bit 3 (value 4): print at any quality level.
    pub can_print: bool,
    /// Bit 4 (value 8): modify document content other than annotations, forms, or signatures.
    pub can_modify: bool,
    /// Bit 5 (value 16): copy or extract text and graphics.
    pub can_copy_text: bool,
    /// Bit 6 (value 32): add or modify annotations, including form fields.
    pub can_annotate: bool,
    /// Bit 9 (value 256): fill in existing interactive form fields.
    pub can_fill_forms: bool,
    /// Bit 10 (value 512): extract text/graphics for accessibility purposes.
    pub can_extract_accessibility: bool,
    /// Bit 11 (value 1024): assemble the document (insert/rotate/delete pages, bookmarks, thumbnails).
    pub can_assemble: bool,
    /// Bit 12 (value 2048): print in high quality (faithful digital copy).
    pub can_print_high_quality: bool,
}

/// Decode the `/P` integer from the `/Encrypt` dictionary into a [`Permissions`] value.
///
/// Bits are numbered from 1 per ISO 32000-1 §7.6.3.3; bits 1–2 are reserved and
/// always 0, so the lowest meaningful bit is bit 3 (value 4).
pub fn parse_permissions(p: i32) -> Permissions {
    Permissions {
        can_print: p & 4 != 0,
        can_modify: p & 8 != 0,
        can_copy_text: p & 16 != 0,
        can_annotate: p & 32 != 0,
        can_fill_forms: p & 256 != 0,
        can_extract_accessibility: p & 512 != 0,
        can_assemble: p & 1024 != 0,
        can_print_high_quality: p & 2048 != 0,
    }
}

/// PDF Standard Security Handler — holds the derived file encryption key.
#[derive(Debug, Clone)]
pub struct EncryptionHandler {
    pub algorithm: EncryptAlgorithm,
    /// Derived file encryption key.
    pub file_key: Vec<u8>,
    pub revision: u8,
}

impl EncryptionHandler {
    /// Build from the `/Encrypt` dictionary found in the trailer.
    ///
    /// Returns `Ok(None)` if the trailer has no `/Encrypt` entry.
    /// Returns `Err(PdfError::Encrypted)` if the password is wrong.
    pub fn from_trailer(trailer: &PdfDict, doc_id: &[u8], password: &[u8]) -> Result<Option<Self>> {
        use crate::parser::objects::PdfObject;

        let encrypt_obj = match trailer.get("Encrypt") {
            Some(o) => o,
            None => return Ok(None),
        };

        let dict = match encrypt_obj {
            PdfObject::Dictionary(d) => d,
            _ => return Err(PdfError::invalid_token(0, "/Encrypt is not a dictionary")),
        };

        // Require Standard security handler.
        match dict.get("Filter") {
            Some(PdfObject::Name(n)) if n == "Standard" => {}
            _ => {
                return Err(PdfError::Encrypted { offset: 0 });
            }
        }

        let v = dict_int(dict, "V", 0)? as u8;
        let r = dict_int(dict, "R", 0)? as u8;

        if r >= 5 || v >= 5 {
            // AES-256 (Revision 5 or 6)
            let u_entry = dict_bytes(dict, "U")?;
            let ue_entry = dict_bytes(dict, "UE")?;

            let file_key = if r == 5 {
                super::aes256::derive_file_key_r5(password, &u_entry, &ue_entry)?
            } else {
                super::aes256::derive_file_key_r6(password, &u_entry, &ue_entry)?
            };

            return Ok(Some(EncryptionHandler {
                algorithm: EncryptAlgorithm::Aes256,
                file_key,
                revision: r,
            }));
        }

        let key_length = match v {
            1 => 40usize,
            _ => dict_int(dict, "Length", 128)? as usize,
        };
        let key_bytes = key_length / 8;

        let o_entry = dict_bytes(dict, "O")?;
        let u_entry = dict_bytes(dict, "U")?;
        let p_flags = dict_int(dict, "P", 0)? as i32;
        let encrypt_meta = dict_bool(dict, "EncryptMetadata", true);

        let algorithm = match v {
            1 => EncryptAlgorithm::Rc4_40,
            4 => EncryptAlgorithm::Aes128,
            _ => EncryptAlgorithm::Rc4_128,
        };

        // Derive the file key and verify the user password.
        let file_key = derive_file_key(
            password,
            r,
            key_bytes,
            &o_entry,
            p_flags,
            doc_id,
            encrypt_meta,
        );

        // Verify password using Algorithm 5 (R=2) or Algorithm 6 (R>=3).
        let valid = if r == 2 {
            verify_user_password_r2(&file_key, &u_entry)
        } else {
            verify_user_password_r3(&file_key, r, &u_entry, doc_id)
        };

        if !valid {
            return Err(PdfError::Encrypted { offset: 0 });
        }

        Ok(Some(EncryptionHandler {
            algorithm,
            file_key,
            revision: r,
        }))
    }

    /// Decrypt a string value (bytes modified in-place).
    ///
    /// `obj_num` and `gen` come from the indirect object containing the string.
    pub fn decrypt_string(&self, obj_num: u32, gen: u16, data: &mut Vec<u8>) -> Result<()> {
        match self.algorithm {
            EncryptAlgorithm::Aes256 => {
                *data = super::aes256::aes256_cbc_decrypt(&self.file_key, data)?;
            }
            EncryptAlgorithm::Aes128 => {
                let key = object_key_aes(&self.file_key, obj_num, gen);
                *data = aes128_cbc_decrypt(&key, data)?;
            }
            _ => {
                let key = object_key_rc4(&self.file_key, obj_num, gen);
                let mut rc4 = Rc4::new(&key).ok_or_else(|| {
                    PdfError::filter_error(0, "RC4 key derivation produced empty key")
                })?;
                rc4.apply_keystream(data);
            }
        }
        Ok(())
    }

    /// Decrypt a stream (returns a new decrypted byte buffer).
    ///
    /// Call this *before* applying stream filters.
    pub fn decrypt_stream(&self, obj_num: u32, gen: u16, data: &[u8]) -> Result<Vec<u8>> {
        match self.algorithm {
            EncryptAlgorithm::Aes256 => super::aes256::aes256_cbc_decrypt(&self.file_key, data),
            EncryptAlgorithm::Aes128 => {
                let key = object_key_aes(&self.file_key, obj_num, gen);
                aes128_cbc_decrypt(&key, data)
            }
            _ => {
                let key = object_key_rc4(&self.file_key, obj_num, gen);
                let mut out = data.to_vec();
                let mut rc4 = Rc4::new(&key).ok_or_else(|| {
                    PdfError::filter_error(0, "RC4 key derivation produced empty key")
                })?;
                rc4.apply_keystream(&mut out);
                Ok(out)
            }
        }
    }

    /// Encrypt a string value in place — the exact inverse of [`decrypt_string`].
    ///
    /// `obj_num` and `gen` are the indirect object containing the string (used to
    /// derive the per-object key for RC4 / AES-128; ignored for AES-256, which
    /// uses the file key directly). Strings in the `/Encrypt` dict and the
    /// trailer `/ID` must NOT be passed here (they are exempt from encryption).
    ///
    /// [`decrypt_string`]: Self::decrypt_string
    pub fn encrypt_string(&self, obj_num: u32, gen: u16, data: &mut Vec<u8>) -> Result<()> {
        match self.algorithm {
            EncryptAlgorithm::Aes256 => {
                *data = super::aes256::aes256_cbc_encrypt(&self.file_key, data)?;
            }
            EncryptAlgorithm::Aes128 => {
                let key = object_key_aes(&self.file_key, obj_num, gen);
                *data = aes128_cbc_encrypt(&key, data)?;
            }
            _ => {
                // RC4 is symmetric: applying the keystream encrypts.
                let key = object_key_rc4(&self.file_key, obj_num, gen);
                let mut rc4 = Rc4::new(&key).ok_or_else(|| {
                    PdfError::filter_error(0, "RC4 key derivation produced empty key")
                })?;
                rc4.apply_keystream(data);
            }
        }
        Ok(())
    }

    /// Encrypt a stream body (returns a new buffer) — the inverse of
    /// [`decrypt_stream`]. Call this *after* applying stream filters, i.e. on the
    /// final bytes that will be written between `stream`/`endstream`.
    ///
    /// [`decrypt_stream`]: Self::decrypt_stream
    pub fn encrypt_stream(&self, obj_num: u32, gen: u16, data: &[u8]) -> Result<Vec<u8>> {
        match self.algorithm {
            EncryptAlgorithm::Aes256 => super::aes256::aes256_cbc_encrypt(&self.file_key, data),
            EncryptAlgorithm::Aes128 => {
                let key = object_key_aes(&self.file_key, obj_num, gen);
                aes128_cbc_encrypt(&key, data)
            }
            _ => {
                let key = object_key_rc4(&self.file_key, obj_num, gen);
                let mut out = data.to_vec();
                let mut rc4 = Rc4::new(&key).ok_or_else(|| {
                    PdfError::filter_error(0, "RC4 key derivation produced empty key")
                })?;
                rc4.apply_keystream(&mut out);
                Ok(out)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key derivation helpers
// ---------------------------------------------------------------------------

/// Pad or truncate `password` to 32 bytes using the standard PDF padding string.
fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let take = password.len().min(32);
    out[..take].copy_from_slice(&password[..take]);
    if take < 32 {
        out[take..].copy_from_slice(&PASSWORD_PADDING[..32 - take]);
    }
    out
}

/// Algorithm 2 (ISO 32000-1 §7.6.3.3): derive the file encryption key.
pub fn derive_file_key(
    password: &[u8],
    revision: u8,
    key_bytes: usize,
    o_entry: &[u8],
    p_flags: i32,
    file_id: &[u8],
    encrypt_meta: bool,
) -> Vec<u8> {
    let padded = pad_password(password);

    let mut hasher = Md5::new();
    hasher.update(padded);
    hasher.update(o_entry);
    hasher.update(p_flags.to_le_bytes());
    hasher.update(file_id);
    if revision >= 4 && !encrypt_meta {
        hasher.update([0xFF, 0xFF, 0xFF, 0xFF]);
    }
    let mut hash: [u8; 16] = hasher.finalize().into();

    if revision >= 3 {
        for _ in 0..50 {
            let iter_hash: [u8; 16] = Md5::digest(&hash[..key_bytes]).into();
            hash = iter_hash;
        }
    }

    hash[..key_bytes].to_vec()
}

/// Algorithm 1 (ISO 32000-1 §7.6.3.1): per-object key for RC4.
fn object_key_rc4(file_key: &[u8], obj_num: u32, gen: u16) -> Vec<u8> {
    let n = file_key.len();
    let mut input = Vec::with_capacity(n + 5);
    input.extend_from_slice(file_key);
    input.push(obj_num as u8);
    input.push((obj_num >> 8) as u8);
    input.push((obj_num >> 16) as u8);
    input.push(gen as u8);
    input.push((gen >> 8) as u8);
    let hash: [u8; 16] = Md5::digest(&input).into();
    hash[..((n + 5).min(16))].to_vec()
}

/// Algorithm 1 for AES (appends the "sAlT" suffix).
fn object_key_aes(file_key: &[u8], obj_num: u32, gen: u16) -> Vec<u8> {
    let n = file_key.len();
    let mut input = Vec::with_capacity(n + 9);
    input.extend_from_slice(file_key);
    input.push(obj_num as u8);
    input.push((obj_num >> 8) as u8);
    input.push((obj_num >> 16) as u8);
    input.push(gen as u8);
    input.push((gen >> 8) as u8);
    input.extend_from_slice(b"sAlT");
    let hash: [u8; 16] = Md5::digest(&input).into();
    hash[..((n + 5).min(16))].to_vec()
}

// ---------------------------------------------------------------------------
// Password verification helpers
// ---------------------------------------------------------------------------

/// Algorithm 4 (R=2): encrypt the padding string and compare with /U.
fn verify_user_password_r2(file_key: &[u8], u_entry: &[u8]) -> bool {
    let mut data = PASSWORD_PADDING;
    if let Some(mut rc4) = Rc4::new(file_key) {
        rc4.apply_keystream(&mut data);
        u_entry.len() >= 32 && data == u_entry[..32]
    } else {
        false
    }
}

/// Algorithm 5 (R>=3): MD5 + 20-round RC4 and compare first 16 bytes of /U.
fn verify_user_password_r3(file_key: &[u8], _revision: u8, u_entry: &[u8], file_id: &[u8]) -> bool {
    // MD5(padding || file_id[0])
    let mut hasher = Md5::new();
    hasher.update(PASSWORD_PADDING);
    hasher.update(file_id);
    let mut data: [u8; 16] = hasher.finalize().into();

    // 20-round RC4
    for k in 0u8..20 {
        let round_key: Vec<u8> = file_key.iter().map(|&b| b ^ k).collect();
        if let Some(mut rc4) = Rc4::new(&round_key) {
            rc4.apply_keystream(&mut data);
        }
    }

    u_entry.len() >= 16 && data == u_entry[..16]
}

// ---------------------------------------------------------------------------
// AES decryption
// ---------------------------------------------------------------------------

/// AES-128-CBC decrypt where the IV is the first 16 bytes of `data`.
fn aes128_cbc_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::Aes128;
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

    if data.len() < 16 {
        return Err(PdfError::filter_error(
            0,
            "AES-encrypted data too short for IV",
        ));
    }
    let (iv, body) = data.split_at(16);
    if body.is_empty() {
        return Ok(Vec::new());
    }

    let dec = cbc::Decryptor::<Aes128>::new_from_slices(key, iv)
        .map_err(|_| PdfError::filter_error(0, "AES-128: invalid key or IV length"))?;
    let mut buf = body.to_vec();
    let n = dec
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| PdfError::filter_error(0, "AES-128 decryption failed (bad padding?)"))?
        .len();
    buf.truncate(n);
    Ok(buf)
}

/// AES-128-CBC encrypt with a fresh random IV (inverse of [`aes128_cbc_decrypt`]).
/// Returns `IV || ciphertext`. `key` is the per-object key from [`object_key_aes`].
fn aes128_cbc_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    use aes::Aes128;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    let iv = super::random_iv();
    // PKCS7 always appends 1..=16 bytes, so reserve one extra block.
    let padded_len = (plaintext.len() / 16 + 1) * 16;
    let mut buf = vec![0u8; padded_len];
    buf[..plaintext.len()].copy_from_slice(plaintext);

    let enc = cbc::Encryptor::<Aes128>::new_from_slices(key, &iv)
        .map_err(|_| PdfError::filter_error(0, "AES-128: invalid key or IV length"))?;
    let ct = enc
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .map_err(|_| PdfError::filter_error(0, "AES-128 encryption failed"))?;

    let mut out = Vec::with_capacity(16 + ct.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(ct);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Dictionary helpers
// ---------------------------------------------------------------------------

fn dict_int(dict: &PdfDict, key: &str, default: i64) -> Result<i64> {
    use crate::parser::objects::PdfObject;
    match dict.get(key) {
        None => Ok(default),
        Some(PdfObject::Integer(n)) => Ok(*n),
        _ => Err(PdfError::invalid_token(
            0,
            format!("/Encrypt /{} is not an integer", key),
        )),
    }
}

fn dict_bytes(dict: &PdfDict, key: &str) -> Result<Vec<u8>> {
    use crate::parser::objects::PdfObject;
    match dict.get(key) {
        Some(PdfObject::String(b)) => Ok(b.clone()),
        _ => Err(PdfError::invalid_token(
            0,
            format!("/Encrypt /{} is missing or not a string", key),
        )),
    }
}

fn dict_bool(dict: &PdfDict, key: &str, default: bool) -> bool {
    use crate::parser::objects::PdfObject;
    match dict.get(key) {
        Some(PdfObject::Boolean(b)) => *b,
        _ => default,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfObject;

    fn make_encrypt_dict(v: i64, r: i64, key_bits: i64, o: Vec<u8>, u: Vec<u8>, p: i64) -> PdfDict {
        let mut d = PdfDict::new();
        d.insert(
            "Filter".to_string(),
            PdfObject::Name("Standard".to_string()),
        );
        d.insert("V".to_string(), PdfObject::Integer(v));
        d.insert("R".to_string(), PdfObject::Integer(r));
        d.insert("Length".to_string(), PdfObject::Integer(key_bits));
        d.insert("O".to_string(), PdfObject::String(o));
        d.insert("U".to_string(), PdfObject::String(u));
        d.insert("P".to_string(), PdfObject::Integer(p));
        d
    }

    #[test]
    fn test_no_encrypt_dict() {
        let trailer: PdfDict = PdfDict::new();
        let result = EncryptionHandler::from_trailer(&trailer, b"", b"");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_encrypt_detected_wrong_password() {
        // Build a minimal R=3 encrypt dict with a known file key derivation.
        // We compute a valid U entry for empty password, then test with wrong password.
        let file_id = b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F\x10";

        // O = 32 bytes of zeros (synthetic owner entry).
        let o_entry = vec![0u8; 32];
        let p_flags: i32 = -4;
        let key_bytes = 16;
        let revision = 3;

        // Derive the file key for empty password.
        let file_key = derive_file_key(b"", revision, key_bytes, &o_entry, p_flags, file_id, true);

        // Build valid U entry for empty password using Algorithm 5.
        let mut hasher = Md5::new();
        hasher.update(PASSWORD_PADDING);
        hasher.update(file_id);
        let mut u_data: [u8; 16] = hasher.finalize().into();
        for k in 0u8..20 {
            let round_key: Vec<u8> = file_key.iter().map(|&b| b ^ k).collect();
            Rc4::new(&round_key).unwrap().apply_keystream(&mut u_data);
        }
        let mut u_entry = u_data.to_vec();
        u_entry.extend_from_slice(&[0u8; 16]); // pad to 32 bytes

        // Build trailer with this encrypt dict.
        let encrypt_dict = make_encrypt_dict(2, 3, 128, o_entry.clone(), u_entry.clone(), -4);
        let mut trailer: PdfDict = PdfDict::new();
        trailer.insert("Encrypt".to_string(), PdfObject::Dictionary(encrypt_dict));

        // Empty password → Ok(Some).
        let result = EncryptionHandler::from_trailer(&trailer, file_id, b"");
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());

        // Wrong password → Err(Encrypted).
        let encrypt_dict2 = make_encrypt_dict(2, 3, 128, o_entry, u_entry, -4);
        let mut trailer2: PdfDict = PdfDict::new();
        trailer2.insert("Encrypt".to_string(), PdfObject::Dictionary(encrypt_dict2));
        let result2 = EncryptionHandler::from_trailer(&trailer2, file_id, b"wrong_password");
        assert!(matches!(result2, Err(PdfError::Encrypted { .. })));
    }

    #[test]
    fn test_key_derivation_r2_deterministic() {
        let file_id = b"\xAA\xBB\xCC\xDD\xEE\xFF\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99";
        let o = vec![0u8; 32];
        let k1 = derive_file_key(b"", 2, 5, &o, -4, file_id, true);
        let k2 = derive_file_key(b"", 2, 5, &o, -4, file_id, true);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 5);
    }

    #[test]
    fn test_key_derivation_r3_longer_key() {
        let file_id = b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F\x10";
        let o = vec![0u8; 32];
        let key = derive_file_key(b"", 3, 16, &o, -4, file_id, true);
        assert_eq!(key.len(), 16);
        // Different password → different key.
        let key2 = derive_file_key(b"secret", 3, 16, &o, -4, file_id, true);
        assert_ne!(key, key2);
    }

    #[test]
    fn test_decrypt_string_roundtrip() {
        // Build a handler with a known file key.
        let handler = EncryptionHandler {
            algorithm: EncryptAlgorithm::Rc4_128,
            file_key: b"0123456789abcdef".to_vec(),
            revision: 3,
        };

        let plaintext = b"Hello, encrypted PDF!".to_vec();
        let obj_num = 5u32;
        let gen = 0u16;

        // Encrypt.
        let mut ciphertext = plaintext.clone();
        handler
            .decrypt_string(obj_num, gen, &mut ciphertext)
            .unwrap();

        // Ciphertext should differ from plaintext.
        assert_ne!(ciphertext, plaintext);

        // Decrypt (RC4 is symmetric).
        handler
            .decrypt_string(obj_num, gen, &mut ciphertext)
            .unwrap();
        assert_eq!(ciphertext, plaintext);
    }

    #[test]
    fn test_decrypt_stream_roundtrip() {
        let handler = EncryptionHandler {
            algorithm: EncryptAlgorithm::Rc4_128,
            file_key: b"abcdef0123456789".to_vec(),
            revision: 3,
        };

        let original = b"Stream content data here.";
        let encrypted = handler.decrypt_stream(3, 0, original).unwrap();
        let decrypted = handler.decrypt_stream(3, 0, &encrypted).unwrap();
        assert_eq!(decrypted, original);
    }

    #[test]
    fn encrypt_string_rc4_roundtrip() {
        let handler = EncryptionHandler {
            algorithm: EncryptAlgorithm::Rc4_128,
            file_key: b"0123456789abcdef".to_vec(),
            revision: 3,
        };
        let plaintext = b"Hello, encrypted PDF!".to_vec();

        let mut buf = plaintext.clone();
        handler.encrypt_string(5, 0, &mut buf).unwrap();
        assert_ne!(buf, plaintext, "ciphertext must differ from plaintext");

        // decrypt_string is the inverse (RC4 symmetric).
        handler.decrypt_string(5, 0, &mut buf).unwrap();
        assert_eq!(buf, plaintext);
    }

    #[test]
    fn encrypt_stream_rc4_roundtrip() {
        let handler = EncryptionHandler {
            algorithm: EncryptAlgorithm::Rc4_128,
            file_key: b"abcdef0123456789".to_vec(),
            revision: 3,
        };
        let original = b"BT /F1 24 Tf 72 700 Td (World) Tj ET";
        let enc = handler.encrypt_stream(7, 0, original).unwrap();
        assert_ne!(&enc[..], &original[..]);
        let dec = handler.decrypt_stream(7, 0, &enc).unwrap();
        assert_eq!(dec, original);
    }

    #[test]
    fn encrypt_stream_aes128_roundtrip() {
        let handler = EncryptionHandler {
            algorithm: EncryptAlgorithm::Aes128,
            file_key: b"0123456789abcdef".to_vec(), // 16-byte file key
            revision: 4,
        };
        let original = b"BT /F1 12 Tf 72 700 Td (Edited via AES-128) Tj ET";

        let enc = handler.encrypt_stream(11, 0, original).unwrap();
        // IV(16) + block-aligned ciphertext, and not the plaintext.
        assert!(enc.len() >= 16 + 16);
        assert_eq!((enc.len() - 16) % 16, 0);
        assert_ne!(&enc[16..], &original[..]);

        let dec = handler.decrypt_stream(11, 0, &enc).unwrap();
        assert_eq!(dec, original);

        // The per-object key depends on obj_num → a different object can't decrypt it.
        let wrong = handler.decrypt_stream(12, 0, &enc);
        assert!(wrong.is_err() || wrong.unwrap() != original);
    }

    #[test]
    fn encrypt_string_aes128_roundtrip() {
        let handler = EncryptionHandler {
            algorithm: EncryptAlgorithm::Aes128,
            file_key: b"fedcba9876543210".to_vec(),
            revision: 4,
        };
        let plaintext = b"a string value".to_vec();
        let mut buf = plaintext.clone();
        handler.encrypt_string(3, 0, &mut buf).unwrap();
        assert_ne!(buf, plaintext);
        handler.decrypt_string(3, 0, &mut buf).unwrap();
        assert_eq!(buf, plaintext);
    }

    // ── parse_permissions ──────────────────────────────────────────────────

    #[test]
    fn parse_permissions_deny_all() {
        // P = 0: all permission bits clear.
        let p = parse_permissions(0);
        assert!(!p.can_print);
        assert!(!p.can_modify);
        assert!(!p.can_copy_text);
        assert!(!p.can_annotate);
        assert!(!p.can_fill_forms);
        assert!(!p.can_extract_accessibility);
        assert!(!p.can_assemble);
        assert!(!p.can_print_high_quality);
    }

    #[test]
    fn parse_permissions_allow_all() {
        // P = -1 (0xFFFFFFFF): all bits set.
        let p = parse_permissions(-1);
        assert!(p.can_print);
        assert!(p.can_modify);
        assert!(p.can_copy_text);
        assert!(p.can_annotate);
        assert!(p.can_fill_forms);
        assert!(p.can_extract_accessibility);
        assert!(p.can_assemble);
        assert!(p.can_print_high_quality);
    }

    #[test]
    fn parse_permissions_print_only() {
        // Bit 3 (value 4) set, all else clear.
        let p = parse_permissions(4);
        assert!(p.can_print);
        assert!(!p.can_modify);
        assert!(!p.can_copy_text);
        assert!(!p.can_annotate);
    }

    #[test]
    fn parse_permissions_p_minus_3904() {
        // P = -3904 (0xFFFFF0C0): all permission bits (3-12) are clear.
        // Used by qpdf and the gen_aes256_fixture.py script.
        let p = parse_permissions(-3904);
        assert!(!p.can_print);
        assert!(!p.can_modify);
        assert!(!p.can_copy_text);
        assert!(!p.can_annotate);
        assert!(!p.can_fill_forms);
        assert!(!p.can_extract_accessibility);
        assert!(!p.can_assemble);
        assert!(!p.can_print_high_quality);
    }

    #[test]
    fn parse_permissions_individual_bits() {
        assert!(parse_permissions(4).can_print);
        assert!(parse_permissions(8).can_modify);
        assert!(parse_permissions(16).can_copy_text);
        assert!(parse_permissions(32).can_annotate);
        assert!(parse_permissions(256).can_fill_forms);
        assert!(parse_permissions(512).can_extract_accessibility);
        assert!(parse_permissions(1024).can_assemble);
        assert!(parse_permissions(2048).can_print_high_quality);
    }
}
