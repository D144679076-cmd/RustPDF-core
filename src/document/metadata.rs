//! PDF document metadata extraction.
//!
//! Parses the /Info dictionary (ISO 32000-1 §14.3.3) and provides structured
//! access to standard metadata fields like Title, Author, and dates.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::text_string::decode_pdf_text_string;

/// Standard PDF document metadata from the /Info dictionary.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// Document title.
    pub title: Option<String>,
    /// Person who created the document content.
    pub author: Option<String>,
    /// Subject of the document.
    pub subject: Option<String>,
    /// Keywords associated with the document.
    pub keywords: Option<String>,
    /// Application that created the original content.
    pub creator: Option<String>,
    /// Application that produced the PDF.
    pub producer: Option<String>,
    /// Date the document was created (raw PDF date string).
    pub creation_date: Option<String>,
    /// Date the document was last modified (raw PDF date string).
    pub mod_date: Option<String>,
    /// Trapping status.
    pub trapped: Option<String>,
}

impl Metadata {
    /// Extract metadata from the document's /Info dictionary.
    ///
    /// Returns default (all None) if no /Info dictionary is present.
    pub fn from_document(doc: &PdfDocument) -> Result<Self> {
        let info_ref = match doc.trailer.get("Info") {
            Some(obj) => obj.clone(),
            None => return Ok(Metadata::default()),
        };

        let info_obj = doc.resolve(&info_ref)?;
        let info_dict = match info_obj {
            PdfObject::Dictionary(d) => d,
            _ => return Ok(Metadata::default()),
        };

        Ok(Metadata {
            title: extract_text_string(&info_dict, "Title"),
            author: extract_text_string(&info_dict, "Author"),
            subject: extract_text_string(&info_dict, "Subject"),
            keywords: extract_text_string(&info_dict, "Keywords"),
            creator: extract_text_string(&info_dict, "Creator"),
            producer: extract_text_string(&info_dict, "Producer"),
            creation_date: extract_text_string(&info_dict, "CreationDate"),
            mod_date: extract_text_string(&info_dict, "ModDate"),
            trapped: extract_name_string(&info_dict, "Trapped"),
        })
    }
}

/// A parsed PDF date (ISO 32000-1 §7.9.4).
///
/// Format: `D:YYYYMMDDHHmmSSOHH'mm'`
#[derive(Debug, Clone, PartialEq)]
pub struct PdfDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// UTC offset: '+', '-', or 'Z'.
    pub tz_sign: char,
    /// UTC offset hours.
    pub tz_hour: u8,
    /// UTC offset minutes.
    pub tz_minute: u8,
}

impl PdfDate {
    /// Parse a PDF date string of the form `D:YYYYMMDDHHmmSSOHH'mm'`.
    ///
    /// All fields after YYYY are optional and default to sensible values.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.strip_prefix("D:").unwrap_or(s);

        if s.len() < 4 {
            return Err(PdfError::invalid_token(
                0,
                format!("PDF date too short: '{}'", s),
            ));
        }

        let year = parse_digits(s, 0, 4)? as u16;
        let month = if s.len() >= 6 {
            parse_digits(s, 4, 2)? as u8
        } else {
            1
        };
        let day = if s.len() >= 8 {
            parse_digits(s, 6, 2)? as u8
        } else {
            1
        };
        let hour = if s.len() >= 10 {
            parse_digits(s, 8, 2)? as u8
        } else {
            0
        };
        let minute = if s.len() >= 12 {
            parse_digits(s, 10, 2)? as u8
        } else {
            0
        };
        let second = if s.len() >= 14 {
            parse_digits(s, 12, 2)? as u8
        } else {
            0
        };

        let mut tz_sign = 'Z';
        let mut tz_hour = 0u8;
        let mut tz_minute = 0u8;

        if s.len() > 14 {
            let tz_part = &s[14..];
            let first_char = tz_part.chars().next().unwrap_or('Z');
            if first_char == '+' || first_char == '-' || first_char == 'Z' {
                tz_sign = first_char;
                if first_char != 'Z' && tz_part.len() >= 4 {
                    tz_hour = parse_digits(tz_part, 1, 2).unwrap_or(0) as u8;
                    // Skip the apostrophe between hours and minutes
                    let min_offset = if tz_part.len() > 4 && tz_part.as_bytes()[3] == b'\'' {
                        4
                    } else {
                        3
                    };
                    if tz_part.len() >= min_offset + 2 {
                        tz_minute = parse_digits(tz_part, min_offset, 2).unwrap_or(0) as u8;
                    }
                }
            }
        }

        Ok(PdfDate {
            year,
            month,
            day,
            hour,
            minute,
            second,
            tz_sign,
            tz_hour,
            tz_minute,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a text string value from a dictionary, decoding PDFDocEncoding or UTF-16BE.
fn extract_text_string(dict: &PdfDict, key: &str) -> Option<String> {
    match dict.get(key) {
        Some(PdfObject::String(bytes)) => Some(decode_pdf_text_string(bytes)),
        _ => None,
    }
}

/// Extract a name value as a plain string.
fn extract_name_string(dict: &PdfDict, key: &str) -> Option<String> {
    match dict.get(key) {
        Some(PdfObject::Name(n)) => Some(n.clone()),
        _ => None,
    }
}

fn parse_digits(s: &str, offset: usize, count: usize) -> Result<u32> {
    if offset + count > s.len() {
        return Err(PdfError::invalid_token(
            0,
            format!("PDF date field too short at offset {}", offset),
        ));
    }
    let slice = &s[offset..offset + count];
    slice
        .parse::<u32>()
        .map_err(|_| PdfError::invalid_token(0, format!("invalid digits '{}' in PDF date", slice)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_date() {
        let date = PdfDate::parse("D:20230415120000+05'30'").unwrap();
        assert_eq!(date.year, 2023);
        assert_eq!(date.month, 4);
        assert_eq!(date.day, 15);
        assert_eq!(date.hour, 12);
        assert_eq!(date.minute, 0);
        assert_eq!(date.second, 0);
        assert_eq!(date.tz_sign, '+');
        assert_eq!(date.tz_hour, 5);
        assert_eq!(date.tz_minute, 30);
    }

    #[test]
    fn test_parse_date_year_only() {
        let date = PdfDate::parse("D:2020").unwrap();
        assert_eq!(date.year, 2020);
        assert_eq!(date.month, 1);
        assert_eq!(date.day, 1);
        assert_eq!(date.hour, 0);
    }

    #[test]
    fn test_parse_date_utc() {
        let date = PdfDate::parse("D:20210101000000Z").unwrap();
        assert_eq!(date.tz_sign, 'Z');
        assert_eq!(date.tz_hour, 0);
    }

    #[test]
    fn test_parse_date_no_prefix() {
        let date = PdfDate::parse("20230101120000").unwrap();
        assert_eq!(date.year, 2023);
        assert_eq!(date.hour, 12);
    }

    #[test]
    fn test_parse_date_too_short() {
        assert!(PdfDate::parse("D:20").is_err());
    }

    #[test]
    fn test_decode_ascii_string() {
        let bytes = b"Hello World";
        assert_eq!(decode_pdf_text_string(bytes), "Hello World");
    }

    #[test]
    fn test_decode_utf16be_string() {
        // BOM + "Hi" in UTF-16BE
        let bytes = vec![0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        assert_eq!(decode_pdf_text_string(&bytes), "Hi");
    }

    #[test]
    fn test_decode_utf8_bom_string() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("Héllo".as_bytes());
        assert_eq!(decode_pdf_text_string(&bytes), "Héllo");
    }

    #[test]
    fn test_metadata_default() {
        let meta = Metadata::default();
        assert!(meta.title.is_none());
        assert!(meta.author.is_none());
    }
}
