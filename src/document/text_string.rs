//! Shared PDF text string decoding utility.
//!
//! PDF text strings (ISO 32000-1 §7.9.2.2) can be encoded as PDFDocEncoding,
//! UTF-16BE (with BOM 0xFE 0xFF), or UTF-8 (with BOM 0xEF 0xBB 0xBF in PDF 2.0).

/// Decode a PDF text string to a Rust String.
///
/// Handles UTF-16BE (BOM 0xFE 0xFF), UTF-8 (BOM 0xEF 0xBB 0xBF), and
/// PDFDocEncoding (Latin-1 superset) byte sequences.
pub(crate) fn decode_pdf_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16BE
        let u16_iter = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
        String::from_utf16_lossy(&u16_iter.collect::<Vec<u16>>())
    } else if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        // UTF-8 BOM (PDF 2.0)
        String::from_utf8_lossy(&bytes[3..]).into_owned()
    } else {
        // PDFDocEncoding — bytes 0x00–0x7F are ASCII, 0x80–0xFF map to Unicode.
        // For simplicity, treat as Latin-1 (correct for most real-world PDFs).
        bytes.iter().map(|&b| b as char).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_ascii() {
        assert_eq!(decode_pdf_text_string(b"Hello"), "Hello");
    }

    #[test]
    fn test_decode_utf16be() {
        let bytes = vec![0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        assert_eq!(decode_pdf_text_string(&bytes), "Hi");
    }

    #[test]
    fn test_decode_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("café".as_bytes());
        assert_eq!(decode_pdf_text_string(&bytes), "café");
    }

    #[test]
    fn test_decode_empty() {
        assert_eq!(decode_pdf_text_string(b""), "");
    }
}
