//! Stream encoding — compress raw bytes into PDF stream objects.

use std::io::Write as IoWrite;

use flate2::{write::ZlibEncoder, Compression};

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject, PdfStream};

/// Compress `data` with zlib (FlateDecode).
pub fn encode_flate(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)
        .map_err(|e| PdfError::write_error(format!("flate encode: {e}")))?;
    enc.finish()
        .map_err(|e| PdfError::write_error(format!("flate finish: {e}")))
}

/// Build a raw (uncompressed) `PdfStream`.
///
/// `dict_extras` is merged into the stream dict; `/Length` is always overwritten.
pub fn make_raw_stream(data: Vec<u8>, dict_extras: PdfDict) -> PdfStream {
    let mut dict = dict_extras;
    dict.insert("Length".to_owned(), PdfObject::Integer(data.len() as i64));
    PdfStream {
        dict,
        raw_data: data,
    }
}

/// Build a FlateDecode-compressed `PdfStream`.
///
/// `dict_extras` is merged into the stream dict alongside `/Filter` and `/Length`.
pub fn make_flate_stream(data: &[u8], dict_extras: PdfDict) -> Result<PdfStream> {
    let compressed = encode_flate(data)?;
    let mut dict = dict_extras;
    dict.insert(
        "Filter".to_owned(),
        PdfObject::Name("FlateDecode".to_owned()),
    );
    dict.insert(
        "Length".to_owned(),
        PdfObject::Integer(compressed.len() as i64),
    );
    Ok(PdfStream {
        dict,
        raw_data: compressed,
    })
}

/// Build an empty dict (convenience for `dict_extras` when no extras needed).
pub fn empty_dict() -> PdfDict {
    PdfDict::new()
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::filters::apply_filter;

    #[test]
    fn flate_round_trip() {
        let original = b"Hello, PDF world! ".repeat(20);
        let compressed = encode_flate(&original).unwrap();
        let decoded = apply_filter("FlateDecode", &compressed).unwrap();
        assert_eq!(decoded, original.to_vec());
    }

    #[test]
    fn make_raw_stream_length() {
        let data = b"abcde".to_vec();
        let stream = make_raw_stream(data, empty_dict());
        match stream.dict.get("Length").unwrap() {
            crate::parser::objects::PdfObject::Integer(n) => assert_eq!(*n, 5),
            _ => panic!("expected integer"),
        }
        assert_eq!(stream.raw_data, b"abcde");
    }

    #[test]
    fn make_flate_stream_has_filter_and_correct_length() {
        let data = b"some content to compress".repeat(10);
        let stream = make_flate_stream(&data, empty_dict()).unwrap();
        assert_eq!(
            stream.dict.get("Filter").unwrap(),
            &crate::parser::objects::PdfObject::Name("FlateDecode".to_owned())
        );
        match stream.dict.get("Length").unwrap() {
            crate::parser::objects::PdfObject::Integer(n) => {
                assert_eq!(*n as usize, stream.raw_data.len());
            }
            _ => panic!("expected integer"),
        }
        // Verify the compressed bytes are valid
        let decoded = apply_filter("FlateDecode", &stream.raw_data).unwrap();
        assert_eq!(decoded, data.to_vec());
    }

    #[test]
    fn dict_extras_preserved() {
        let mut extras = PdfDict::new();
        extras.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
        let stream = make_flate_stream(b"x", extras).unwrap();
        assert!(stream.dict.contains_key("Subtype"));
        assert!(stream.dict.contains_key("Filter"));
        assert!(stream.dict.contains_key("Length"));
    }
}
