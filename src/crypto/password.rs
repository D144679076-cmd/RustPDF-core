//! Password normalisation for PDF 2.0 encryption (ISO 32000-2 §7.6.4.3.2).
//!
//! PDF 2.0 makes the encryption password a **text string**: the string is
//! decoded to Unicode (UTF-16BE BOM / UTF-8 BOM / bare UTF-8 / PDFDocEncoding),
//! then lightly SASLprep-normalised (RFC 3454 mapping step), and re-encoded to
//! UTF-8 before Algorithm 2.B consumes it.
//!
//! Compatibility strategy (see `.doc/pdf2-support-plan-2026-11-07.md`,
//! dual-version policy): R6 verification tries the *normalised* password
//! first, then the *raw bytes* — legacy writers (pre-2.0 Acrobat extensions)
//! hashed raw bytes, PDF 2.0 writers hash normalised text strings. ASCII
//! passwords are identical under both paths, so nothing changes for them.

/// Decode an encryption password text string into Unicode characters.
///
/// Order matches ISO 32000-2 §7.9.2.2 text-string conventions:
/// 1. UTF-16BE BOM (`FE FF`) → UTF-16BE
/// 2. UTF-8 BOM (`EF BB BF`) → UTF-8 (BOM excluded, per spec the BOM is part
///    of the string but marks encoding)
/// 3. Bare valid UTF-8 → taken literally (PDF 2.0 password convention)
/// 4. Otherwise → PDFDocEncoding (via the shared text-string decoder; same
///    Latin-1-approximation the codebase already documents)
fn decode_to_string(raw: &[u8]) -> String {
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        // UTF-16BE
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
        // UTF-8 BOM (PDF 2.0)
        let bytes = &raw[3..];
        std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| crate::document::text_string::decode_pdf_text_string(bytes))
    } else if let Ok(s) = std::str::from_utf8(raw) {
        // Bare UTF-8 (valid even without a BOM — Password step treats it as text)
        s.to_string()
    } else {
        crate::document::text_string::decode_pdf_text_string(raw)
    }
}

/// Light SASLprep (RFC 3454 mapping step, RFC 4013 profile).
///
/// Applies only the mapping step — the commonly-implemented subset:
/// - Non-ASCII space characters map to `U+0020`.
/// - "Commonly mapped to nothing" codepoints are removed (soft hyphen,
///   zero-width joiners, variation selectors, BOM…).
///
/// Not implemented (documented limitation): the NFKC normalisation step and
/// the full prohibited/output tables. Byte-compatible for ASCII and all
/// passwords without decomposition-equal forms — the practical subset Adobe
/// itself relies on for verification tolerance.
fn saslprep_mapping(s: &str) -> String {
    s.chars()
        .filter(|c| {
            // Commonly mapped to nothing (RFC 3454 table B.1, main members).
            !matches!(
                *c,
                '\u{00AD}' // soft hyphen
                | '\u{034F}' // combining grapheme joiner
                | '\u{1806}' // Mongolian todo soft hyphen
                | '\u{180B}' | '\u{180C}' | '\u{180D}' | '\u{180E}' // shape selectors
                | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{200E}' | '\u{200F}'
                | '\u{2060}' | '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{2064}'
                | '\u{FE00}' | '\u{FE01}' | '\u{FE02}' | '\u{FE03}'
                | '\u{FE04}' | '\u{FE05}' | '\u{FE06}' | '\u{FE07}'
                | '\u{FE08}' | '\u{FE09}' | '\u{FE0A}' | '\u{FE0B}'
                | '\u{FE0C}' | '\u{FE0D}' | '\u{FE0E}' | '\u{FE0F}'
                | '\u{FEFF}' // zero-width no-break space / BOM
            )
        })
        .map(|c| {
            // Non-ASCII space characters (RFC 3454 table C.1.2, main members).
            if matches!(
                c,
                '\u{00A0}'
                    | '\u{1680}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
            ) || matches!(c, '\u{2000}'..='\u{200A}')
            {
                '\u{0020}'
            } else {
                c
            }
        })
        .collect()
}

/// Password candidates for R6 verification in precedence order.
///
/// 1. **Normalised** — decode to Unicode, SASLprep-map, re-encode UTF-8
///    (ISO 32000-2 §7.6.4.3.2 path).
/// 2. **Raw bytes** — legacy path (extension-era R6 writers hashed raw bytes);
///    skipped when identical to the normalised result (all ASCII passwords).
pub fn password_candidates_for_r6(password: &[u8]) -> Vec<Vec<u8>> {
    let normalized = saslprep_mapping(&decode_to_string(password)).into_bytes();
    if normalized == password {
        vec![normalized]
    } else {
        vec![normalized, password.to_vec()]
    }
}

/// Normalise a password for R6 key derivation (the first candidate).
///
/// Kept as a named public helper so writers (future Phase E) can hash a
/// password exactly once using the PDF 2.0 semantic.
pub fn normalize_password(password: &[u8]) -> Vec<u8> {
    saslprep_mapping(&decode_to_string(password)).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- happy paths --------------------------------------------------------

    #[test]
    fn test_ascii_passthrough() {
        // ASCII: normalised == raw, one candidate.
        let cands = password_candidates_for_r6(b"secret");
        assert_eq!(cands, vec![b"secret".to_vec()]);
        assert_eq!(normalize_password(b"secret"), b"secret");
    }

    #[test]
    fn test_empty_password() {
        assert_eq!(password_candidates_for_r6(b""), vec![Vec::<u8>::new()]);
    }

    #[test]
    fn test_utf8_bom_stripped() {
        // "héllo" with UTF-8 BOM — password text string per PDF 2.0.
        let mut raw = vec![0xEF, 0xBB, 0xBF];
        raw.extend_from_slice("héllo".as_bytes());
        assert_eq!(normalize_password(&raw), "héllo".as_bytes());
    }

    #[test]
    fn test_utf16be_bom_decoded() {
        // "héllo" as UTF-16BE with BOM.
        let raw = vec![
            0xFE, 0xFF, 0x00, 0x68, 0x00, 0xE9, 0x00, 0x6C, 0x00, 0x6C, 0x00, 0x6F,
        ];
        assert_eq!(normalize_password(&raw), "héllo".as_bytes());
    }

    #[test]
    fn test_bare_utf8_non_ascii() {
        // Bare UTF-8 (no BOM) non-ASCII password.
        assert_eq!(normalize_password("cöße".as_bytes()), "cöße".as_bytes());
    }

    #[test]
    fn test_soft_hyphen_removed_saslprep() {
        // U+00AD (soft hyphen) is commonly-mapped-to-nothing.
        let with_shy = "pass\u{00AD}word".as_bytes();
        assert_eq!(normalize_password(with_shy), b"password");
    }

    #[test]
    fn test_zero_width_and_variation_selectors_removed() {
        let messy = "pa\u{200B}ss\u{FE0F}word\u{FEFF}";
        assert_eq!(normalize_password(messy.as_bytes()), b"password");
    }

    #[test]
    fn test_non_ascii_space_mapped() {
        // U+00A0 NBSP and U+2003 em space map to U+0020.
        let s = "ab\u{00A0}cd\u{2003}ef";
        assert_eq!(normalize_password(s.as_bytes()), b"ab cd ef");
    }

    #[test]
    fn test_latin1_fallback_decoded() {
        // Invalid UTF-8 (raw byte 0xE9) → Latin-1 'é' via PDFDocEncoding path.
        assert_eq!(normalize_password(&[0xE9]), "é".as_bytes());
    }

    // --- candidate matrix ----------------------------------------------------

    #[test]
    fn test_non_ascii_gives_two_candidates() {
        // UTF-16BE "é" — normalised is UTF-8, raw is the BOM'd UTF-16; both kept.
        let raw = vec![0xFE, 0xFF, 0x00, 0xE9];
        let cands = password_candidates_for_r6(&raw);
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0], "é".as_bytes());
        assert_eq!(cands[1], raw);
    }

    #[test]
    fn test_ascii_gives_single_candidate() {
        let cands = password_candidates_for_r6(b"Secret42");
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0], b"Secret42");
    }

    #[test]
    fn test_no_panic_on_truncated_utf16() {
        // Unterminated UTF-16 surrogate + odd length must decode lossily, not panic.
        let raw = vec![0xFE, 0xFF, 0xD8, 0x3D, 0x00];
        let _ = normalize_password(&raw);
    }
}
