//! PDF Cross-Reference (XRef) Table and Stream Parser.
//!
//! Exposes functional support for parsing traditional `xref` tables and
//! PDF 1.5+ XRef Streams (including compressed payloads using `/FlateDecode`).
//! Outputs a unified `HashMap<u32, u64>` mapping object IDs to absolute byte offsets.

use crate::error::{PdfError, Result};
use crate::parser::lexer::{Keyword, Lexer, Token};
use flate2::read::ZlibDecoder;
use nom::bytes::complete::{tag, take};
use nom::character::complete::{digit1, line_ending, space1};
use nom::IResult;
use std::collections::HashMap;
use std::io::Read;

/// Nom result type for parsing one XRef subsection: (start_id, entries).
type XRefSubsectionResult<'a> = IResult<&'a [u8], (u32, Vec<(u64, u32, bool)>)>;

/// Representation of basic PDF objects parsed from XRef stream dictionaries.
#[derive(Debug, Clone, PartialEq)]
enum PdfObject {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(Vec<u8>),
    Name(String),
    Array(Vec<PdfObject>),
    Dictionary(HashMap<String, PdfObject>),
}

/// Unified entry point to parse XRef data starting at the specified offset.
/// Supports both traditional XRef tables and PDF 1.5+ XRef Streams.
pub fn parse_xref(data: &[u8], offset: usize) -> Result<HashMap<u32, u64>> {
    let file_size = data.len() as u64;
    if offset >= data.len() {
        return Err(PdfError::invalid_token(
            offset,
            "XRef start offset is out of file bounds",
        ));
    }

    let slice = &data[offset..];
    let skipped = skip_whitespace_and_comments(slice);

    if skipped.starts_with(b"xref") {
        parse_traditional_xref_bytes(skipped, file_size)
    } else {
        // PDF 1.5+ XRef Stream format
        // An XRef Stream is a standard indirect stream object.
        let absolute_offset = data.len() - skipped.len();
        let (dict, stream_data) = parse_indirect_stream(data, absolute_offset)?;

        // Verify type is /XRef
        match dict.get("Type") {
            Some(PdfObject::Name(t)) if t == "XRef" => {}
            _ => {
                return Err(PdfError::invalid_token(
                    absolute_offset,
                    "expected stream of /Type /XRef",
                ))
            }
        }

        parse_xref_stream_bytes(&dict, &stream_data, file_size)
    }
}

// -----------------------------------------------------------------------------
// Helper Decompression Functions
// -----------------------------------------------------------------------------

fn decompress_flate(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    Ok(decompressed)
}

// -----------------------------------------------------------------------------
// Traditional XRef Parsing Logic
// -----------------------------------------------------------------------------

fn parse_u32(bytes: &[u8]) -> std::result::Result<u32, nom::Err<nom::error::Error<&[u8]>>> {
    let s = std::str::from_utf8(bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(bytes, nom::error::ErrorKind::Digit))
    })?;
    let val = s.parse::<u32>().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(bytes, nom::error::ErrorKind::Digit))
    })?;
    Ok(val)
}

fn parse_u64(bytes: &[u8]) -> std::result::Result<u64, nom::Err<nom::error::Error<&[u8]>>> {
    let s = std::str::from_utf8(bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(bytes, nom::error::ErrorKind::Digit))
    })?;
    let val = s.parse::<u64>().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(bytes, nom::error::ErrorKind::Digit))
    })?;
    Ok(val)
}

fn parse_xref_entry(input: &[u8]) -> IResult<&[u8], (u64, u32, bool)> {
    let (input, offset_bytes) = take(10usize)(input)?;
    let offset = parse_u64(offset_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = tag(b" ")(input)?;

    let (input, gen_bytes) = take(5usize)(input)?;
    let gen = parse_u32(gen_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = tag(b" ")(input)?;

    let (input, type_byte) = take(1usize)(input)?;
    let is_in_use = match type_byte[0] {
        b'n' => true,
        b'f' => false,
        _ => {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Char,
            )))
        }
    };

    let (input, _) = nom::branch::alt((
        tag(b" \r\n"),
        tag(b"\r\n"),
        tag(b" \n"),
        tag(b" \r"),
        tag(b"\n"),
        tag(b"\r"),
    ))(input)?;

    Ok((input, (offset, gen, is_in_use)))
}

fn parse_xref_subsection(input: &[u8]) -> XRefSubsectionResult<'_> {
    let (input, start_id_bytes) = digit1(input)?;
    let start_id = parse_u32(start_id_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = space1(input)?;

    let (input, count_bytes) = digit1(input)?;
    let count = parse_u32(count_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = line_ending(input)?;

    let mut rem = input;
    let mut entries = Vec::new();
    for _ in 0..count {
        let (next_rem, entry) = parse_xref_entry(rem)?;
        entries.push(entry);
        rem = next_rem;
    }

    Ok((rem, (start_id, entries)))
}

fn parse_traditional_xref_bytes(mut input: &[u8], file_size: u64) -> Result<HashMap<u32, u64>> {
    input = skip_whitespace_and_comments(input);

    if !input.starts_with(b"xref") {
        return Err(PdfError::invalid_token(0, "expected 'xref' keyword"));
    }
    input = &input[4..];
    input = skip_whitespace_and_comments(input);

    let mut map = HashMap::new();

    while !input.is_empty() {
        if input.starts_with(b"trailer") {
            break;
        }

        let (rem, (start_id, entries)) = parse_xref_subsection(input)
            .map_err(|e| PdfError::lexer(0, format!("failed to parse XRef subsection: {:?}", e)))?;

        for (i, (offset, _gen, is_in_use)) in entries.into_iter().enumerate() {
            let obj_id = start_id + (i as u32);
            if is_in_use {
                if offset >= file_size {
                    return Err(PdfError::invalid_token(
                        0,
                        format!(
                            "object {} offset {} is out of file bounds {}",
                            obj_id, offset, file_size
                        ),
                    ));
                }
                map.insert(obj_id, offset);
            }
        }

        input = skip_whitespace_and_comments(rem);
    }

    Ok(map)
}

// -----------------------------------------------------------------------------
// XRef Stream Parsing Logic (PDF 1.5+)
// -----------------------------------------------------------------------------

fn parse_object(lexer: &mut Lexer) -> Result<PdfObject> {
    let token = lexer.next_token()?;
    match token {
        Token::Null => Ok(PdfObject::Null),
        Token::Boolean(b) => Ok(PdfObject::Boolean(b)),
        Token::Integer(i) => Ok(PdfObject::Integer(i)),
        Token::Real(r) => Ok(PdfObject::Real(r)),
        Token::LiteralString(s) => Ok(PdfObject::String(s)),
        Token::HexString(s) => Ok(PdfObject::String(s)),
        Token::Name(n) => Ok(PdfObject::Name(n)),
        Token::ArrayStart => {
            let mut arr = Vec::new();
            loop {
                if lexer.peek_token()? == Token::ArrayEnd {
                    lexer.next_token()?;
                    break;
                }
                let obj = parse_object(lexer)?;
                arr.push(obj);
            }
            Ok(PdfObject::Array(arr))
        }
        Token::DictStart => {
            let mut dict = HashMap::new();
            loop {
                let peek = lexer.peek_token()?;
                if peek == Token::DictEnd {
                    lexer.next_token()?;
                    break;
                }
                let key_token = lexer.next_token()?;
                let key = match key_token {
                    Token::Name(name) => name,
                    t => {
                        return Err(PdfError::invalid_token(
                            lexer.position(),
                            format!("expected dict key name, found {:?}", t),
                        ))
                    }
                };
                let val = parse_object(lexer)?;
                dict.insert(key, val);
            }
            Ok(PdfObject::Dictionary(dict))
        }
        Token::Eof => Err(PdfError::eof(lexer.position(), "unexpected EOF")),
        t => Err(PdfError::invalid_token(
            lexer.position(),
            format!("unexpected token {:?}", t),
        )),
    }
}

fn parse_indirect_stream(
    data: &[u8],
    offset: usize,
) -> Result<(HashMap<String, PdfObject>, Vec<u8>)> {
    let slice = &data[offset..];
    let mut lexer = Lexer::new(slice);

    let _obj_id = match lexer.peek_token()? {
        Token::Integer(i) => {
            lexer.next_token()?;
            i
        }
        t => {
            return Err(PdfError::invalid_token(
                offset + lexer.position(),
                format!("expected object ID, found {:?}", t),
            ))
        }
    };

    let _gen_num = match lexer.next_token()? {
        Token::Integer(i) => i,
        t => {
            return Err(PdfError::invalid_token(
                offset + lexer.position(),
                format!("expected generation ID, found {:?}", t),
            ))
        }
    };

    match lexer.next_token()? {
        Token::Keyword(Keyword::Obj) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset + lexer.position(),
                format!("expected 'obj', found {:?}", t),
            ))
        }
    };

    let dict = match parse_object(&mut lexer)? {
        PdfObject::Dictionary(d) => d,
        t => {
            return Err(PdfError::invalid_token(
                offset + lexer.position(),
                format!("expected dictionary, found {:?}", t),
            ))
        }
    };

    let next = lexer.next_token()?;
    if next == Token::Keyword(Keyword::Stream) {
        let mut start_pos = lexer.position();
        if start_pos < slice.len() && slice[start_pos] == b' ' {
            start_pos += 1;
        }
        if start_pos < slice.len() && slice[start_pos] == b'\r' {
            start_pos += 1;
        }
        if start_pos < slice.len() && slice[start_pos] == b'\n' {
            start_pos += 1;
        }

        let length = match dict.get("Length") {
            Some(PdfObject::Integer(l)) => *l as usize,
            _ => {
                return Err(PdfError::invalid_token(
                    offset,
                    "missing or invalid /Length in stream dictionary",
                ))
            }
        };

        if start_pos + length > slice.len() {
            return Err(PdfError::eof(
                offset + start_pos,
                "stream data goes out of bounds",
            ));
        }

        let stream_data = slice[start_pos..start_pos + length].to_vec();
        Ok((dict, stream_data))
    } else {
        Ok((dict, Vec::new()))
    }
}

fn get_int(obj: &PdfObject) -> Option<i64> {
    match obj {
        PdfObject::Integer(i) => Some(*i),
        _ => None,
    }
}

fn read_uint(bytes: &[u8], offset: usize, size: usize) -> u64 {
    let mut val = 0;
    for i in 0..size {
        val = (val << 8) | (bytes[offset + i] as u64);
    }
    val
}

fn parse_xref_stream_bytes(
    dict: &HashMap<String, PdfObject>,
    stream_data: &[u8],
    file_size: u64,
) -> Result<HashMap<u32, u64>> {
    let size = match dict.get("Size") {
        Some(PdfObject::Integer(s)) => *s as u32,
        _ => return Err(PdfError::invalid_token(0, "missing /Size in XRef stream")),
    };

    let w_arr = match dict.get("W") {
        Some(PdfObject::Array(arr)) if arr.len() == 3 => arr,
        _ => {
            return Err(PdfError::invalid_token(
                0,
                "missing or invalid /W in XRef stream",
            ))
        }
    };

    let w0 = match get_int(&w_arr[0]) {
        Some(w) if w >= 0 => w as usize,
        _ => return Err(PdfError::invalid_token(0, "invalid field width W[0]")),
    };
    let w1 = match get_int(&w_arr[1]) {
        Some(w) if w >= 0 => w as usize,
        _ => return Err(PdfError::invalid_token(0, "invalid field width W[1]")),
    };
    let w2 = match get_int(&w_arr[2]) {
        Some(w) if w >= 0 => w as usize,
        _ => return Err(PdfError::invalid_token(0, "invalid field width W[2]")),
    };

    let entry_width = w0 + w1 + w2;
    if entry_width == 0 {
        return Err(PdfError::invalid_token(
            0,
            "XRef stream entry width is zero",
        ));
    }

    let mut index_pairs = Vec::new();
    if let Some(PdfObject::Array(arr)) = dict.get("Index") {
        if arr.len() % 2 != 0 {
            return Err(PdfError::invalid_token(
                0,
                "invalid /Index array: odd length",
            ));
        }
        for chunk in arr.chunks_exact(2) {
            let start = match get_int(&chunk[0]) {
                Some(s) if s >= 0 => s as u32,
                _ => return Err(PdfError::invalid_token(0, "invalid start ID in /Index")),
            };
            let count = match get_int(&chunk[1]) {
                Some(c) if c >= 0 => c as u32,
                _ => return Err(PdfError::invalid_token(0, "invalid count in /Index")),
            };
            index_pairs.push((start, count));
        }
    } else {
        index_pairs.push((0, size));
    }

    let decompressed_data = if let Some(PdfObject::Name(filter)) = dict.get("Filter") {
        if filter == "FlateDecode" {
            decompress_flate(stream_data).map_err(|e| {
                PdfError::lexer(0, format!("failed to decompress FlateDecode stream: {}", e))
            })?
        } else {
            return Err(PdfError::invalid_token(
                0,
                format!("unsupported filter /{}", filter),
            ));
        }
    } else if let Some(PdfObject::Array(filters)) = dict.get("Filter") {
        if filters.len() == 1 {
            if let PdfObject::Name(filter) = &filters[0] {
                if filter == "FlateDecode" {
                    decompress_flate(stream_data).map_err(|e| {
                        PdfError::lexer(
                            0,
                            format!("failed to decompress FlateDecode stream: {}", e),
                        )
                    })?
                } else {
                    return Err(PdfError::invalid_token(
                        0,
                        format!("unsupported filter /{}", filter),
                    ));
                }
            } else {
                return Err(PdfError::invalid_token(0, "invalid filter array element"));
            }
        } else {
            return Err(PdfError::invalid_token(
                0,
                "multi-filter streams are not supported",
            ));
        }
    } else {
        stream_data.to_vec()
    };

    let total_entries: u32 = index_pairs.iter().map(|(_, count)| count).sum();
    if decompressed_data.len() < (total_entries as usize) * entry_width {
        return Err(PdfError::invalid_token(
            0,
            format!(
                "XRef stream too short: expected {} bytes, found {}",
                (total_entries as usize) * entry_width,
                decompressed_data.len()
            ),
        ));
    }

    let mut map = HashMap::new();
    let mut data_ptr = 0;

    for &(start_id, count) in &index_pairs {
        for obj_idx in 0..count {
            let obj_id = start_id + obj_idx;
            let offset = data_ptr;
            data_ptr += entry_width;

            let entry_type = if w0 > 0 {
                read_uint(&decompressed_data, offset, w0)
            } else {
                1
            };

            let f2 = read_uint(&decompressed_data, offset + w0, w1);
            let _f3 = read_uint(&decompressed_data, offset + w0 + w1, w2);

            match entry_type {
                0 => {}
                1 => {
                    if f2 >= file_size {
                        return Err(PdfError::invalid_token(
                            0,
                            format!(
                                "object {} offset {} is out of file bounds {}",
                                obj_id, f2, file_size
                            ),
                        ));
                    }
                    map.insert(obj_id, f2);
                }
                2 => {
                    // Type 2 entries represent compressed objects inside object streams.
                    // Since they do not have direct file offsets, we skip resolving them to absolute offsets.
                }
                t => {
                    return Err(PdfError::invalid_token(
                        0,
                        format!("unsupported XRef stream entry type {}", t),
                    ));
                }
            }
        }
    }

    Ok(map)
}

// -----------------------------------------------------------------------------
// Lexer Whitespace Skip Mirror
// -----------------------------------------------------------------------------

fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

fn skip_whitespace_and_comments(mut input: &[u8]) -> &[u8] {
    loop {
        let start_len = input.len();
        while !input.is_empty() && is_pdf_whitespace(input[0]) {
            input = &input[1..];
        }
        if input.starts_with(b"%") {
            input = &input[1..];
            while !input.is_empty() && input[0] != b'\r' && input[0] != b'\n' {
                input = &input[1..];
            }
            if !input.is_empty() {
                if input.starts_with(b"\r\n") {
                    input = &input[2..];
                } else {
                    input = &input[1..];
                }
            }
        }
        if input.len() == start_len {
            break;
        }
    }
    input
}

// -----------------------------------------------------------------------------
// Unit Tests (5 targeted test cases)
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn compress_data(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn test_parse_traditional_xref() {
        let data = b"xref\n0 3\n0000000000 65535 f \r\n0000000010 00000 n \r\n0000000050 00000 n \r\ntrailer\n<< /Size 3 >>";
        let map = parse_xref(data, 0).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&1), Some(&10));
        assert_eq!(map.get(&2), Some(&50));
    }

    #[test]
    fn test_parse_traditional_xref_out_of_bounds() {
        let data = b"xref\n0 2\n0000000000 65535 f \r\n0000000100 00000 n \r\n";
        let res = parse_xref(data, 0);
        assert!(res.is_err());
        match res.err().unwrap() {
            PdfError::InvalidToken { detail, .. } => {
                assert!(detail.contains("out of file bounds"));
            }
            _ => panic!("Expected PdfError::InvalidToken due to out of bounds"),
        }
    }

    #[test]
    fn test_parse_xref_stream_uncompressed() {
        let data = b"1 0 obj << /Type /XRef /Size 3 /W [1 1 1] /Length 9 >> stream \x00\x00\x00\x01\x0A\x00\x01\x14\x00endstream\nendobj";
        let map = parse_xref(data, 0).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&1), Some(&10));
        assert_eq!(map.get(&2), Some(&20));
    }

    #[test]
    fn test_parse_xref_stream_compressed() {
        let raw_stream = vec![
            0x00, 0x00, 0x00, // free
            0x01, 0x0A, 0x00, // in-use (offset 10)
            0x01, 0x14, 0x00, // in-use (offset 20)
        ];
        let compressed = compress_data(&raw_stream);
        let header = format!(
            "2 0 obj << /Type /XRef /Size 3 /W [1 1 1] /Filter /FlateDecode /Length {} >> stream ",
            compressed.len()
        );
        let mut data = header.into_bytes();
        data.extend(compressed);
        data.extend(b"endstream\nendobj");

        let map = parse_xref(&data, 0).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&1), Some(&10));
        assert_eq!(map.get(&2), Some(&20));
    }

    #[test]
    fn test_parse_xref_stream_with_index() {
        let data = b"5 0 obj << /Type /XRef /Size 10 /Index [4 2] /W [1 1 1] /Length 6 >> stream \x01\x1E\x00\x01\x28\x00endstream\nendobj";
        let map = parse_xref(data, 0).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&4), Some(&30));
        assert_eq!(map.get(&5), Some(&40));
    }
}
