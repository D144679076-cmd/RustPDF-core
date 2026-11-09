//! PDF file header / version detection (ISO 32000-1 & 32000-2 §7.2).
//!
//! Scans the first 1024 bytes of a file for the `%PDF-m.n` header, returning a
//! typed [`PdfVersion`]. Per spec, readers must not reject documents whose
//! version is newer than they know: unknown versions produce a warning and are
//! parsed anyway (`PdfVersion::Unknown`). Only malformed header *bytes* are an
//! error, carrying the byte offset via [`crate::error::PdfError::Header`].
//!
//! Font matter: real PDFs may start the header line at a nonzero offset
//! (e.g. after a binary-comment line), so the scan window — not just offset 0 —
//! is authoritative.

use crate::error::{PdfError, Result};

/// Number of leading bytes scanned for the `%PDF-m.n` header (spec §7.2).
const HEADER_SCAN_WINDOW: usize = 1024;

/// A PDF file version, as declared by the `%PDF-m.n` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PdfVersion {
    V1_0,
    V1_1,
    V1_2,
    V1_3,
    V1_4,
    V1_5,
    V1_6,
    V1_7,
    /// PDF 2.0 (ISO 32000-2). Header `%PDF-2.0`; encryption must be AES
    /// (RC4 and R5 removed) — see `.doc/pdf2-support-plan-2026-11-07.md`.
    V2_0,
    /// A version this build's table does not name. Spec §7.2 requires readers
    /// to tolerate it: parse continues after `log::warn!`. Retains the
    /// numbers so downstream code can compare against a known threshold.
    Unknown(u8, u8),
}

impl PdfVersion {
    /// Parse a `m.n` version pair into a [`PdfVersion`].
    ///
    /// Versions outside the known table become `PdfVersion::Unknown`, never an
    /// error — future-version tolerance is a conformance requirement.
    pub fn from_parts(major: u8, minor: u8) -> Self {
        match (major, minor) {
            (1, 0) => PdfVersion::V1_0,
            (1, 1) => PdfVersion::V1_1,
            (1, 2) => PdfVersion::V1_2,
            (1, 3) => PdfVersion::V1_3,
            (1, 4) => PdfVersion::V1_4,
            (1, 5) => PdfVersion::V1_5,
            (1, 6) => PdfVersion::V1_6,
            (1, 7) => PdfVersion::V1_7,
            (2, 0) => PdfVersion::V2_0,
            (m, n) => PdfVersion::Unknown(m, n),
        }
    }

    /// Major version number (e.g. `1` for `%PDF-1.7`).
    pub fn major(self) -> u8 {
        match self {
            PdfVersion::Unknown(m, _) => m,
            v => v.as_parts().0,
        }
    }

    /// Minor version number (e.g. `7` for `%PDF-1.7`).
    pub fn minor(self) -> u8 {
        match self {
            PdfVersion::Unknown(_, n) => n,
            v => v.as_parts().1,
        }
    }

    /// The `(major, minor)` pair for any non-`Unknown` variant.
    fn as_parts(self) -> (u8, u8) {
        match self {
            PdfVersion::V1_0 => (1, 0),
            PdfVersion::V1_1 => (1, 1),
            PdfVersion::V1_2 => (1, 2),
            PdfVersion::V1_3 => (1, 3),
            PdfVersion::V1_4 => (1, 4),
            PdfVersion::V1_5 => (1, 5),
            PdfVersion::V1_6 => (1, 6),
            PdfVersion::V1_7 => (1, 7),
            PdfVersion::V2_0 => (2, 0),
            PdfVersion::Unknown(m, n) => (m, n),
        }
    }

    /// True for versions newer than PDF 2.0 that this build does not name.
    pub fn is_unknown(self) -> bool {
        matches!(self, PdfVersion::Unknown(_, _))
    }

    /// True once the document is at least PDF 2.0 (ISO 32000-2).
    ///
    /// Used by encryption policy (RC4 forbidden in 2.0 files) and by the PDF 2.0
    /// string/`/AF` gates. `Unknown` versions `>= 2.0` count as PDF 2.0-or-later.
    pub fn is_pdf2_or_later(self) -> bool {
        match self {
            PdfVersion::Unknown(m, _) => m >= 2,
            v => v.as_parts().0 >= 2,
        }
    }

    /// True if the version predates PDF 2.0, i.e. is a 1.x / 2.0-boundary case.
    pub fn is_pdf1(self) -> bool {
        self.major() <= 1
    }
}

impl std::fmt::Display for PdfVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfVersion::Unknown(m, n) => write!(f, "{}.{}", m, n),
            v => write!(f, "{}.{}", v.as_parts().0, v.as_parts().1),
        }
    }
}

/// Locate and parse the `%PDF-m.n` header within the first 1024 bytes.
///
/// Returns the parsed [`PdfVersion`]. Behaviour matrix:
///
/// | Input | Result |
/// |-------|--------|
/// | `%PDF-1.4` at offset 0 | `Ok(V1_4)` |
/// | garbage line, then `%PDF-2.0` before byte 1024 | `Ok(V2_0)` — real files start the header at a nonzero offset |
/// | `%PDF-2.7` | `Ok(Unknown(2, 7))` + `log::warn!` — future-version tolerance (§7.2) |
/// | `%FDF-1.4` | `Ok(V1_4)` — FDF (Forms Data Format, §12.7.7) shares the PDF object grammar and parses through this entry point |
/// | `%PDF-` present but digits malformed | `Err(Header { offset })` — bytes recognizable as a header but corrupt |
/// | no `%PDF-` in scan window | `Err(Header { offset: 0 })` — not a PDF |
///
/// Byte offsets in every error allow callers to point at the offending bytes.
pub fn detect_version(data: &[u8]) -> Result<PdfVersion> {
    let window_end = data.len().min(HEADER_SCAN_WINDOW);
    let prefix = &data[..window_end];

    let pdf_marker = b"%PDF-";
    let fdf_marker = b"%FDF-";
    // Prefer %PDF-, fall back to %FDF- (FDF parses through the same object
    // pipeline; forms/fdf.rs round-trips depend on it).
    let marker_hit = prefix
        .windows(pdf_marker.len())
        .position(|w| w == pdf_marker)
        .map(|pos| (pos, pdf_marker))
        .or_else(|| {
            prefix
                .windows(fdf_marker.len())
                .position(|w| w == fdf_marker)
                .map(|pos| (pos, fdf_marker))
        })
        .ok_or(PdfError::Header { offset: 0 })?;
    let (marker_pos, marker) = marker_hit;

    // Digits after the marker: one digit, '.', one digit. Parsing stops after
    // the first mismatch so trailing garbage like "%PDF-3Binary" is caught.
    let after = &prefix[marker_pos + marker.len()..];
    if after.len() < 3
        || !after[0].is_ascii_digit()
        || after[1] != b'.'
        || !after[2].is_ascii_digit()
    {
        return Err(PdfError::Header { offset: marker_pos });
    }

    let (major, minor) = (after[0] - b'0', after[2] - b'0');
    let version = PdfVersion::from_parts(major, minor);
    if version.is_unknown() {
        // Future-version tolerance (ISO 32000-2 §7.2): warn, never reject.
        log::warn!(
            "PDF header '{}.{}' is newer than this build's version table; parsing anyway",
            major,
            minor
        );
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- happy paths (rule R5: one happy path per parse fn) ----------------

    #[test]
    fn test_detect_1_4() {
        assert_eq!(detect_version(b"%PDF-1.4\nrest").unwrap(), PdfVersion::V1_4);
    }

    #[test]
    fn test_detect_1_7() {
        assert_eq!(detect_version(b"%PDF-1.7\n%...").unwrap(), PdfVersion::V1_7);
    }

    #[test]
    fn test_detect_2_0() {
        assert_eq!(
            detect_version(b"%PDF-2.0\n%Some binary comment \xE2\xE3\xCF\xD3\n").unwrap(),
            PdfVersion::V2_0
        );
    }

    #[test]
    fn test_detect_after_garbage_prefix() {
        // Real files sometimes start with a garbage line; the scan window is
        // authoritative, not offset 0 (ISO 32000-2 §7.2).
        let data = b"binary\xE2\xE3\xCF\xD3 stuff\n%PDF-1.6\n";
        assert_eq!(detect_version(data).unwrap(), PdfVersion::V1_6);
    }

    #[test]
    fn test_future_version_warns_but_parses() {
        assert_eq!(
            detect_version(b"%PDF-2.7\n").unwrap(),
            PdfVersion::Unknown(2, 7)
        );
        assert_eq!(
            detect_version(b"%PDF-3.0\n").unwrap(),
            PdfVersion::Unknown(3, 0)
        );
    }

    #[test]
    fn test_fdf_header_accepted() {
        // FDF round-trips through the same parser (forms/fdf.rs).
        assert_eq!(detect_version(b"%FDF-1.4\nrest").unwrap(), PdfVersion::V1_4);
        assert_eq!(detect_version(b"%FDF-1.2\n").unwrap(), PdfVersion::V1_2);
        // FDF and PDF markers both present → the first %PDF- wins.
        assert_eq!(
            detect_version(b"%FDF-1.4\n%PDF-2.0\n").unwrap(),
            PdfVersion::V2_0
        );
    }

    #[test]
    fn test_max_scan_window() {
        // Header sitting exactly at the end of the 1024-byte window is found.
        let mut data = vec![b' '; 1024 - 8];
        data.extend_from_slice(b"%PDF-1.5\n");
        assert_eq!(detect_version(&data).unwrap(), PdfVersion::V1_5);

        // Header just past the window is not.
        let mut data = vec![b' '; 1017];
        data.extend_from_slice(b"%PDF-1.5\n");
        assert!(detect_version(&data).is_err());
    }

    // --- error paths (rule R5: one error path per parse fn) ----------------

    #[test]
    fn test_no_header_error() {
        let err = detect_version(b"not a pdf at all, no marker anywhere").unwrap_err();
        match err {
            PdfError::Header { offset } => assert_eq!(offset, 0),
            other => panic!("expected Header error, got {:?}", other),
        }
    }

    #[test]
    fn test_malformed_digits_error_offsets_are_absolute() {
        // `%PDF-` found but digits corrupt — error carries the marker offset.
        let data = b"%PDF-xY\n";
        let err = detect_version(data).unwrap_err();
        match err {
            PdfError::Header { offset } => assert_eq!(offset, 0),
            other => panic!("expected Header error, got {:?}", other),
        }

        // Marker after garbage: the offset must point at the marker, not 0.
        // "junk" = 4 bytes + 2 raw bytes + "\n" = 7 → marker at offset 7.
        let data2 = b"junk\xE2\xE3\n%PDF-AB\n";
        let err2 = detect_version(data2).unwrap_err();
        match err2 {
            PdfError::Header { offset } => assert_eq!(offset, 7),
            other => panic!("expected Header error, got {:?}", other),
        }
    }

    #[test]
    fn test_truncated_header_error() {
        assert!(detect_version(b"%PDF-1").is_err());
        assert!(detect_version(b"%PDF-1.").is_err());
    }

    #[test]
    fn test_header_too_late_error() {
        // Marker exists but past the scan window — treated as not a PDF.
        let mut data = vec![b' '; HEADER_SCAN_WINDOW];
        data.extend_from_slice(b"%PDF-1.4\n");
        assert!(detect_version(&data).is_err());
    }

    // --- PdfVersion model ---------------------------------------------------

    #[test]
    fn test_version_accessors() {
        assert_eq!(PdfVersion::V1_7.major(), 1);
        assert_eq!(PdfVersion::V1_7.minor(), 7);
        assert_eq!(PdfVersion::V2_0.to_string(), "2.0");
        assert_eq!(PdfVersion::Unknown(2, 9).to_string(), "2.9");
    }

    #[test]
    fn test_is_pdf2_or_later() {
        assert!(PdfVersion::V2_0.is_pdf2_or_later());
        assert!(!PdfVersion::V1_7.is_pdf2_or_later());
        // Future 2.x / 3.x counts as "2.0-or-later" for encryption policy.
        assert!(PdfVersion::Unknown(2, 7).is_pdf2_or_later());
        assert!(PdfVersion::Unknown(3, 0).is_pdf2_or_later());
        assert!(!PdfVersion::Unknown(1, 9).is_pdf2_or_later());
    }
}
