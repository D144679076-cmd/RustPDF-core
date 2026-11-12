//! Writer-side encryption (plan Phase P4: encrypt on write).
//!
//! Builds a fresh `/Encrypt` dictionary from passwords + permissions and
//! encrypts every string / stream body of a document pool before
//! serialisation. The per-object crypt primitives come from the reader side
//! ([`crate::crypto::EncryptionHandler::encrypt_string` /
//! `encrypt_stream`]) — they are exact inverses of the decryption path, so
//! round-trip correctness holds by construction.
//!
//! Three cipher suites (mirroring qpdf's defaults):
//!
//! | Variant | `/V` | `/R` | Cipher |
//! |---|---|---|---|
//! | [`WriteAlgorithm::Rc4_128`] | 2 | 3 | RC4-128 |
//! | [`WriteAlgorithm::Aes128`] | 4 | 4 | AES-128 (CF `/StdCF` `/AESV2`) |
//! | [`WriteAlgorithm::Aes256R6`] | 5 | 6 | AES-256 (`/AESV3`, ISO 32000-2) |
//!
//! RC4 and AES-128 use the legacy R2-R4 key schedule (compute helpers in
//! [`crate::crypto::handler`]); AES-256 uses the R6 pipeline from
//! [`crate::crypto::aes256::compute_entries_r6`].

use crate::crypto::handler::{parse_permissions, Permissions};
use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};

/// Cipher suite selection for encrypt-on-write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAlgorithm {
    /// RC4-128 — V=2, R=3. Legacy; PDF 2.0 files must not use it.
    Rc4_128,
    /// AES-128 — V=4, R=4.
    Aes128,
    /// AES-256 — V=5, R=6 (ISO 32000-2 pipeline).
    Aes256R6,
}

impl WriteAlgorithm {
    /// Parse a `rc4` / `aes128` / `aes256` string (API surfaces).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "rc4" => Some(Self::Rc4_128),
            "aes128" => Some(Self::Aes128),
            "aes256" => Some(Self::Aes256R6),
            _ => None,
        }
    }
}

/// Everything needed to produce an encrypted document.
#[derive(Debug, Clone)]
pub struct EncryptOptions {
    /// User (open) password. Empty string = open without a password prompt.
    pub user_password: String,
    /// Owner password. Defaults to the user password when absent.
    pub owner_password: Option<String>,
    /// Document permissions (`/P`).
    pub permissions: Permissions,
    /// Cipher suite.
    pub algorithm: WriteAlgorithm,
}

impl Default for EncryptOptions {
    fn default() -> Self {
        Self {
            user_password: String::new(),
            owner_password: None,
            permissions: parse_permissions(-1),
            algorithm: WriteAlgorithm::Aes256R6,
        }
    }
}

/// Result of building a fresh `/Encrypt` dictionary.
pub(crate) struct BuiltEncryption {
    /// The `/Encrypt` dictionary object (to be written *unencrypted*).
    pub dict: PdfDict,
    /// Handler carrying the derived file key — encrypt pool objects with it.
    pub handler: crate::crypto::EncryptionHandler,
    /// 16-byte file `/ID` (trailer `/ID` array).
    pub file_id: [u8; 16],
}

/// Cryptographically random bytes with the same no-panic fallback as the
/// crypto module's IV helper (R1).
fn random_bytes(buf: &mut [u8]) {
    if getrandom::getrandom(buf).is_err() {
        let seed = (buf.as_ptr() as usize as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let bytes = seed.to_le_bytes();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = bytes[i % 8].wrapping_add(i as u8);
        }
    }
}

/// Pack [`Permissions`] back into the `/P` integer (inverse of
/// [`parse_permissions`]); reserved bits 1-2 stay clear and every
/// unspecified high bit is set per Table 22.
pub fn permissions_to_p(permissions: &Permissions) -> i32 {
    let mut p: i32 = -1;
    let clear = |p: &mut i32, bit: i32, allowed: bool| {
        if !allowed {
            *p &= !bit;
        }
    };
    clear(&mut p, 4, permissions.can_print);
    clear(&mut p, 8, permissions.can_modify);
    clear(&mut p, 16, permissions.can_copy_text);
    clear(&mut p, 32, permissions.can_annotate);
    clear(&mut p, 256, permissions.can_fill_forms);
    clear(&mut p, 512, permissions.can_extract_accessibility);
    clear(&mut p, 1024, permissions.can_assemble);
    clear(&mut p, 2048, permissions.can_print_high_quality);
    p
}

/// Build a fresh `/Encrypt` dictionary + handler from [`EncryptOptions`].
///
/// Returns the dictionary (strings stay plaintext — the `/Encrypt` dict is
/// exempt from per-object encryption), the handler with the derived file key
/// for encrypting pool objects, and the 16-byte `/ID` to place in the
/// trailer.
pub(crate) fn build_encryption(options: &EncryptOptions) -> Result<BuiltEncryption> {
    let user_pw = options.user_password.as_bytes();
    let owner_pw = options
        .owner_password
        .as_deref()
        .unwrap_or(&options.user_password)
        .as_bytes();
    let p = permissions_to_p(&options.permissions);

    let mut file_id = [0u8; 16];
    random_bytes(&mut file_id);

    let (dict, handler) = match options.algorithm {
        WriteAlgorithm::Rc4_128 => {
            // V=2 / R=3, RC4-128: legacy key schedule, no crypt filters.
            let entries = crate::crypto::handler::compute_legacy_entries(
                user_pw,
                Some(owner_pw),
                3,
                16,
                p,
                &file_id,
            )?;
            let mut d = PdfDict::new();
            d.insert("Filter".to_owned(), PdfObject::Name("Standard".to_owned()));
            d.insert("V".to_owned(), PdfObject::Integer(2));
            d.insert("R".to_owned(), PdfObject::Integer(3));
            d.insert("Length".to_owned(), PdfObject::Integer(128));
            d.insert("O".to_owned(), PdfObject::String(entries.o_entry));
            d.insert("U".to_owned(), PdfObject::String(entries.u_entry));
            d.insert("P".to_owned(), PdfObject::Integer(p as i64));
            let handler = crate::crypto::EncryptionHandler {
                algorithm: crate::crypto::handler::EncryptAlgorithm::Rc4_128,
                file_key: entries.file_key,
                revision: 3,
            };
            (d, handler)
        }
        WriteAlgorithm::Aes128 => {
            // V=4 / R=4, AES-128 via the StdCF crypt filter.
            let entries = crate::crypto::handler::compute_legacy_entries(
                user_pw,
                Some(owner_pw),
                4,
                16,
                p,
                &file_id,
            )?;
            let mut stdcf = PdfDict::new();
            stdcf.insert("CFM".to_owned(), PdfObject::Name("AESV2".to_owned()));
            stdcf.insert("Length".to_owned(), PdfObject::Integer(16));
            let mut cf = PdfDict::new();
            cf.insert("StdCF".to_owned(), PdfObject::Dictionary(stdcf));
            let mut d = PdfDict::new();
            d.insert("Filter".to_owned(), PdfObject::Name("Standard".to_owned()));
            d.insert("V".to_owned(), PdfObject::Integer(4));
            d.insert("R".to_owned(), PdfObject::Integer(4));
            d.insert("Length".to_owned(), PdfObject::Integer(128));
            d.insert("CF".to_owned(), PdfObject::Dictionary(cf));
            d.insert("StmF".to_owned(), PdfObject::Name("StdCF".to_owned()));
            d.insert("StrF".to_owned(), PdfObject::Name("StdCF".to_owned()));
            d.insert("O".to_owned(), PdfObject::String(entries.o_entry));
            d.insert("U".to_owned(), PdfObject::String(entries.u_entry));
            d.insert("P".to_owned(), PdfObject::Integer(p as i64));
            let handler = crate::crypto::EncryptionHandler {
                algorithm: crate::crypto::handler::EncryptAlgorithm::Aes128,
                file_key: entries.file_key,
                revision: 4,
            };
            (d, handler)
        }
        WriteAlgorithm::Aes256R6 => {
            // V=5 / R=6, AES-256 (ISO 32000-2 pipeline). V5 uses the file key
            // directly — no per-object key derivation.
            let entries = crate::crypto::aes256::compute_entries_r6(user_pw, owner_pw, p)?;
            let mut stdcf = PdfDict::new();
            stdcf.insert("CFM".to_owned(), PdfObject::Name("AESV3".to_owned()));
            stdcf.insert("Length".to_owned(), PdfObject::Integer(32));
            let mut cf = PdfDict::new();
            cf.insert("StdCF".to_owned(), PdfObject::Dictionary(stdcf));
            let mut d = PdfDict::new();
            d.insert("Filter".to_owned(), PdfObject::Name("Standard".to_owned()));
            d.insert("V".to_owned(), PdfObject::Integer(5));
            d.insert("R".to_owned(), PdfObject::Integer(6));
            d.insert("Length".to_owned(), PdfObject::Integer(256));
            d.insert("CF".to_owned(), PdfObject::Dictionary(cf));
            d.insert("StmF".to_owned(), PdfObject::Name("StdCF".to_owned()));
            d.insert("StrF".to_owned(), PdfObject::Name("StdCF".to_owned()));
            d.insert("O".to_owned(), PdfObject::String(entries.o_entry.to_vec()));
            d.insert("U".to_owned(), PdfObject::String(entries.u_entry.to_vec()));
            d.insert(
                "OE".to_owned(),
                PdfObject::String(entries.oe_entry.to_vec()),
            );
            d.insert(
                "UE".to_owned(),
                PdfObject::String(entries.ue_entry.to_vec()),
            );
            d.insert(
                "Perms".to_owned(),
                PdfObject::String(entries.perms_entry.to_vec()),
            );
            d.insert("P".to_owned(), PdfObject::Integer(p as i64));
            let handler = crate::crypto::EncryptionHandler {
                algorithm: crate::crypto::handler::EncryptAlgorithm::Aes256,
                file_key: entries.file_key.to_vec(),
                revision: 6,
            };
            (d, handler)
        }
    };

    Ok(BuiltEncryption {
        dict,
        handler,
        file_id,
    })
}

// ---------------------------------------------------------------------------
// Pool encryption
// ---------------------------------------------------------------------------

/// Recursively encrypt every string and stream body inside an object, in
/// place. `id`/`gen` identify the indirect object (per-object key for RC4 /
/// AES-128; ignored by AES-256, which uses the file key directly).
///
/// `encrypt_stream_bodies` controls stream *bodies* only (stream dictionary
/// strings are always encrypted):
///
/// - `true` — bodies are plaintext (editor-produced streams or decrypted
///   source bodies); encrypt them. This is the incremental-append path.
/// - `false` — bodies are ALREADY encrypted with the same handler (same-key
///   full rewrite of an encrypted document); leave them untouched.
///
/// The caller must NOT pass the `/Encrypt` dictionary or the trailer `/ID`
/// — those are exempt from encryption (ISO 32000-1 §7.6.1).
pub(crate) fn encrypt_object_for_write(
    obj: &mut PdfObject,
    id: u32,
    gen: u16,
    handler: &crate::crypto::EncryptionHandler,
    encrypt_stream_bodies: bool,
) -> Result<()> {
    match obj {
        PdfObject::String(s) => handler.encrypt_string(id, gen, s)?,
        PdfObject::Array(items) => {
            for it in items.iter_mut() {
                encrypt_object_for_write(it, id, gen, handler, encrypt_stream_bodies)?;
            }
        }
        PdfObject::Dictionary(d) => {
            for v in d.values_mut() {
                encrypt_object_for_write(v, id, gen, handler, encrypt_stream_bodies)?;
            }
        }
        PdfObject::Stream(st) => {
            for v in st.dict.values_mut() {
                encrypt_object_for_write(v, id, gen, handler, encrypt_stream_bodies)?;
            }
            if encrypt_stream_bodies {
                // Encrypt the (already filter-applied) stream body; the serializer
                // recomputes /Length from raw_data, so the encrypted length is correct.
                st.raw_data = handler.encrypt_stream(id, gen, &st.raw_data)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_to_p_matches_parse() {
        // All allowed (default -1) round-trips.
        let p = permissions_to_p(&parse_permissions(-1));
        assert_eq!(parse_permissions(p).can_print, true);
        assert_eq!(parse_permissions(p).can_copy_text, true);
        // Nothing allowed.
        let none = Permissions::default();
        let p2 = permissions_to_p(&none);
        assert!(!parse_permissions(p2).can_print);
        assert!(!parse_permissions(p2).can_copy_text);
        assert!(!parse_permissions(p2).can_fill_forms);
    }

    #[test]
    fn build_encryption_legacy_dict_shape() {
        let opts = EncryptOptions {
            user_password: "secret".to_owned(),
            algorithm: WriteAlgorithm::Rc4_128,
            ..Default::default()
        };
        let built = build_encryption(&opts).expect("build RC4");
        assert_eq!(built.dict.get("V").and_then(|v| v.as_integer()), Some(2));
        assert_eq!(built.dict.get("R").and_then(|v| v.as_integer()), Some(3));
        assert_eq!(built.handler.file_key.len(), 16);
        // The handler's key must verify against the dict the reader will see.
        let trailer = {
            let mut t = PdfDict::new();
            t.insert(
                "Encrypt".to_owned(),
                PdfObject::Dictionary(built.dict.clone()),
            );
            t
        };
        let h = crate::crypto::EncryptionHandler::from_trailer(
            &trailer,
            &built.file_id,
            b"secret",
            crate::parser::version::PdfVersion::V1_4,
        )
        .expect("reader accepts dict")
        .expect("has handler");
        assert_eq!(h.file_key, built.handler.file_key);
    }

    #[test]
    fn build_encryption_aes128_dict_shape() {
        let opts = EncryptOptions {
            user_password: "secret".to_owned(),
            algorithm: WriteAlgorithm::Aes128,
            ..Default::default()
        };
        let built = build_encryption(&opts).expect("build AES-128");
        assert_eq!(built.dict.get("V").and_then(|v| v.as_integer()), Some(4));
        let trailer = {
            let mut t = PdfDict::new();
            t.insert(
                "Encrypt".to_owned(),
                PdfObject::Dictionary(built.dict.clone()),
            );
            t
        };
        let h = crate::crypto::EncryptionHandler::from_trailer(
            &trailer,
            &built.file_id,
            b"secret",
            crate::parser::version::PdfVersion::V1_5,
        )
        .expect("reader accepts dict")
        .expect("has handler");
        assert_eq!(h.file_key, built.handler.file_key);
        assert_eq!(
            h.algorithm,
            crate::crypto::handler::EncryptAlgorithm::Aes128
        );
    }

    #[test]
    fn build_encryption_aes256r6_self_verifies() {
        let opts = EncryptOptions {
            user_password: "s3cret".to_owned(),
            algorithm: WriteAlgorithm::Aes256R6,
            ..Default::default()
        };
        let built = build_encryption(&opts).expect("build R6");
        assert_eq!(built.dict.get("R").and_then(|v| v.as_integer()), Some(6));
        // Reader-side: the exact same path a parse takes.
        let trailer = {
            let mut t = PdfDict::new();
            t.insert(
                "Encrypt".to_owned(),
                PdfObject::Dictionary(built.dict.clone()),
            );
            t
        };
        let h = crate::crypto::EncryptionHandler::from_trailer(
            &trailer,
            &built.file_id,
            b"s3cret",
            crate::parser::version::PdfVersion::V2_0,
        )
        .expect("reader accepts dict")
        .expect("has handler");
        assert_eq!(h.file_key, built.handler.file_key);
        assert_eq!(
            h.algorithm,
            crate::crypto::handler::EncryptAlgorithm::Aes256
        );
    }
}
