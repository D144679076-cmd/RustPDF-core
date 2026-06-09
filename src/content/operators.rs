//! PDF content stream operator parsing.
//!
//! Tokenizes a content stream into a sequence of (operands, operator) pairs.
//! Each operator is preceded by zero or more operand values on the implicit stack.
//! (ISO 32000-1 §7.8.2)

use crate::error::{PdfError, Result};
use crate::parser::lexer::{Keyword, Lexer, Token};
use crate::parser::objects::PdfObject;

/// A single parsed operation: an operator name with its preceding operands.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// Operand values preceding the operator.
    pub operands: Vec<PdfObject>,
    /// The operator name (e.g. "BT", "Tj", "cm", "q").
    pub operator: String,
}

/// Parse a content stream byte buffer into a sequence of operations.
///
/// Content streams differ from structural PDF in that:
/// - There are no indirect references (no `R` keyword)
/// - Operators are bare identifiers that consume preceding operands
/// - Inline images (BI/ID/EI) require special handling
pub fn parse_content_stream(data: &[u8]) -> Result<Vec<Operation>> {
    let mut lexer = Lexer::new(data);
    let mut operations = Vec::new();
    let mut operand_stack: Vec<PdfObject> = Vec::new();

    loop {
        if lexer.is_eof() {
            break;
        }

        let token = match lexer.next_token() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("content stream lexer error: {}, skipping", e);
                break;
            }
        };

        match token {
            Token::Eof => break,

            // Numeric values → push as operands
            Token::Integer(n) => operand_stack.push(PdfObject::Integer(n)),
            Token::Real(r) => operand_stack.push(PdfObject::Real(r)),

            // Strings → push as operands
            Token::LiteralString(s) => operand_stack.push(PdfObject::String(s)),
            Token::HexString(s) => operand_stack.push(PdfObject::String(s)),

            // Names → push as operands
            Token::Name(n) => operand_stack.push(PdfObject::Name(n)),

            // Booleans
            Token::Boolean(b) => operand_stack.push(PdfObject::Boolean(b)),
            Token::Null => operand_stack.push(PdfObject::Null),

            // Array
            Token::ArrayStart => {
                let arr = parse_array_operand(&mut lexer)?;
                operand_stack.push(PdfObject::Array(arr));
            }

            // Dictionary (rare in content streams, but used by BI inline images)
            Token::DictStart => {
                let dict = parse_dict_operand(&mut lexer)?;
                operand_stack.push(PdfObject::Dictionary(dict));
            }

            // Operators and keywords
            Token::Operator(op) => {
                if op == "BI" {
                    let inline_op = parse_inline_image(&mut lexer, &mut operand_stack)?;
                    operations.push(inline_op);
                } else {
                    operations.push(Operation {
                        operands: std::mem::take(&mut operand_stack),
                        operator: op,
                    });
                }
            }

            Token::Keyword(kw) => {
                let op_name = match kw {
                    Keyword::Other(s) => s,
                    // Some keywords can appear as operators in content streams
                    Keyword::R => "R".to_string(),
                    _ => {
                        log::warn!("unexpected keyword in content stream: {:?}", kw);
                        continue;
                    }
                };
                if op_name == "BI" {
                    let inline_op = parse_inline_image(&mut lexer, &mut operand_stack)?;
                    operations.push(inline_op);
                } else {
                    operations.push(Operation {
                        operands: std::mem::take(&mut operand_stack),
                        operator: op_name,
                    });
                }
            }

            // Delimiters that shouldn't appear bare
            Token::ArrayEnd | Token::DictEnd => {
                log::warn!("unexpected delimiter in content stream");
            }
        }
    }

    Ok(operations)
}

/// Parse an array operand within a content stream.
fn parse_array_operand(lexer: &mut Lexer) -> Result<Vec<PdfObject>> {
    let mut arr = Vec::new();
    loop {
        let token = lexer.next_token()?;
        match token {
            Token::ArrayEnd => break,
            Token::Eof => {
                return Err(PdfError::eof(
                    lexer.position(),
                    "unterminated array in content stream",
                ))
            }
            Token::Integer(n) => arr.push(PdfObject::Integer(n)),
            Token::Real(r) => arr.push(PdfObject::Real(r)),
            Token::LiteralString(s) => arr.push(PdfObject::String(s)),
            Token::HexString(s) => arr.push(PdfObject::String(s)),
            Token::Name(n) => arr.push(PdfObject::Name(n)),
            Token::Boolean(b) => arr.push(PdfObject::Boolean(b)),
            Token::Null => arr.push(PdfObject::Null),
            Token::ArrayStart => {
                let nested = parse_array_operand(lexer)?;
                arr.push(PdfObject::Array(nested));
            }
            _ => {
                log::warn!("unexpected token in content stream array: {:?}", token);
            }
        }
    }
    Ok(arr)
}

/// Parse a dictionary operand within a content stream.
fn parse_dict_operand(lexer: &mut Lexer) -> Result<crate::parser::objects::PdfDict> {
    let mut dict = crate::parser::objects::PdfDict::new();
    loop {
        let token = lexer.next_token()?;
        match token {
            Token::DictEnd => break,
            Token::Eof => {
                return Err(PdfError::eof(
                    lexer.position(),
                    "unterminated dictionary in content stream",
                ))
            }
            Token::Name(key) => {
                let val_token = lexer.next_token()?;
                let val = token_to_object(val_token, lexer)?;
                dict.insert(key, val);
            }
            _ => {
                log::warn!("expected name key in content stream dict, got {:?}", token);
            }
        }
    }
    Ok(dict)
}

/// Convert a token to a PdfObject (for dict values).
fn token_to_object(token: Token, lexer: &mut Lexer) -> Result<PdfObject> {
    match token {
        Token::Integer(n) => Ok(PdfObject::Integer(n)),
        Token::Real(r) => Ok(PdfObject::Real(r)),
        Token::LiteralString(s) => Ok(PdfObject::String(s)),
        Token::HexString(s) => Ok(PdfObject::String(s)),
        Token::Name(n) => Ok(PdfObject::Name(n)),
        Token::Boolean(b) => Ok(PdfObject::Boolean(b)),
        Token::Null => Ok(PdfObject::Null),
        Token::ArrayStart => {
            let arr = parse_array_operand(lexer)?;
            Ok(PdfObject::Array(arr))
        }
        Token::DictStart => {
            let dict = parse_dict_operand(lexer)?;
            Ok(PdfObject::Dictionary(dict))
        }
        _ => Ok(PdfObject::Null),
    }
}

/// Parse an inline image (BI ... ID <data> EI).
///
/// Inline images have a special format:
/// ```text
/// BI
///   /W 100 /H 50 /BPC 8 /CS /RGB
/// ID
///   <raw image bytes>
/// EI
/// ```
fn parse_inline_image(lexer: &mut Lexer, _operand_stack: &mut Vec<PdfObject>) -> Result<Operation> {
    // Parse the image dictionary (key-value pairs until ID)
    let mut dict = crate::parser::objects::PdfDict::new();

    loop {
        let token = lexer.next_token()?;
        match token {
            Token::Operator(ref op) if op == "ID" => break,
            Token::Keyword(Keyword::Other(ref s)) if s == "ID" => break,
            Token::Name(key) => {
                let val_token = lexer.next_token()?;
                let val = token_to_object(val_token, lexer)?;
                // Expand abbreviated key names
                let full_key = expand_inline_image_key(&key);
                dict.insert(full_key, val);
            }
            Token::Eof => {
                return Err(PdfError::eof(
                    lexer.position(),
                    "unterminated inline image (missing ID)",
                ))
            }
            _ => {
                log::warn!("unexpected token in inline image header: {:?}", token);
            }
        }
    }

    // One whitespace byte after `ID` is part of the delimiter, not image data.
    let remaining = lexer.remaining();
    let data_start =
        if !remaining.is_empty() && matches!(remaining[0], b' ' | b'\n' | b'\r' | b'\t') {
            1
        } else {
            0
        };
    let search = &remaining[data_start..];

    // Prefer a deterministic data length (explicit `/L`, JPEG EOI, or an
    // unfiltered raster computed from W·H·components·bits). Only when the length
    // cannot be computed do we fall back to scanning for the `EI` marker — the
    // old approach, which can stop early if raw image bytes happen to spell
    // whitespace-`EI` (common in DCT/Flate streams).
    let (image_data, skip_total) = match inline_image_data_len(&dict, search) {
        Some(len) => {
            let len = len.min(search.len());
            let data = search[..len].to_vec();
            (data, data_start + len + consume_trailing_ei(&search[len..]))
        }
        None => match scan_for_ei(search) {
            Some(pos) => (search[..pos].to_vec(), data_start + pos + 3),
            None => {
                log::warn!(
                    "inline image EI marker not found at offset {}, consuming rest",
                    lexer.position()
                );
                let len = search.len();
                (search.to_vec(), data_start + len)
            }
        },
    };

    // Advance lexer past the inline image data + EI
    let new_pos = lexer.position() + skip_total;
    lexer.set_position(new_pos);

    let operands = vec![PdfObject::Dictionary(dict), PdfObject::String(image_data)];

    Ok(Operation {
        operands,
        operator: "BI".to_string(),
    })
}

/// Number of colour components implied by an inline image's `/ColorSpace`.
///
/// Returns `None` for indexed or named colour spaces that can only be resolved
/// against the page resource dictionary (not available here). `ImageMask`
/// images are always 1 component.
fn inline_cs_components(dict: &crate::parser::objects::PdfDict) -> Option<u32> {
    if matches!(dict.get("ImageMask"), Some(PdfObject::Boolean(true))) {
        return Some(1);
    }
    match dict.get("ColorSpace") {
        Some(PdfObject::Name(n)) => match n.as_str() {
            "G" | "DeviceGray" | "CalGray" => Some(1),
            "RGB" | "DeviceRGB" | "CalRGB" | "Lab" => Some(3),
            "CMYK" | "DeviceCMYK" => Some(4),
            _ => None, // indexed / ICC / named — needs resource resolution
        },
        _ => None,
    }
}

/// Compute the exact byte length of inline image data when it is determined by
/// the image parameters, avoiding the fragile `EI`-marker scan.
///
/// Resolves, in order: an explicit `/L`/`/Length`; the JPEG end-of-image marker
/// for `DCTDecode`; or, for unfiltered rasters, `ceil(W·components·bpc / 8)·H`
/// (rows are byte-aligned per ISO 32000-1 §8.9.5.2). Returns `None` when none
/// applies (e.g. Flate/LZW without an explicit length), leaving the caller to
/// scan for `EI`.
fn inline_image_data_len(dict: &crate::parser::objects::PdfDict, search: &[u8]) -> Option<usize> {
    // 1) Explicit length — unambiguous when present.
    if let Some(PdfObject::Integer(n)) = dict.get("Length").or_else(|| dict.get("L")) {
        if *n >= 0 {
            return Some(*n as usize);
        }
    }

    let filter = dict.get("Filter").or_else(|| dict.get("F"));

    // 2) JPEG: data ends at the EOI marker (FF D9), which cannot be confused
    //    with a whitespace-delimited `EI` token.
    if let Some(PdfObject::Name(f)) = filter {
        if f == "DCTDecode" || f == "DCT" {
            return find_jpeg_eoi(search);
        }
    }

    // 3) Unfiltered raster: length is fully determined by the geometry.
    if filter.is_none() {
        let w = dict.get("Width").and_then(|o| o.as_integer())?;
        let h = dict.get("Height").and_then(|o| o.as_integer())?;
        let is_mask = matches!(dict.get("ImageMask"), Some(PdfObject::Boolean(true)));
        let bpc = if is_mask {
            1
        } else {
            dict.get("BitsPerComponent").and_then(|o| o.as_integer())?
        };
        let comps = inline_cs_components(dict)? as i64;
        if w <= 0 || h <= 0 || bpc <= 0 {
            return None;
        }
        let bytes_per_row = (w * comps * bpc + 7) / 8;
        return Some((bytes_per_row * h) as usize);
    }

    None
}

/// Index just past the JPEG end-of-image marker `FF D9`, or `None` if absent.
fn find_jpeg_eoi(data: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] == 0xFF && data[i + 1] == 0xD9 {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Bytes to skip past optional whitespace and the `EI` marker following
/// known-length inline image data. Best-effort: if `EI` is not where expected,
/// only the whitespace run is consumed.
fn consume_trailing_ei(after_data: &[u8]) -> usize {
    let mut i = 0;
    while i < after_data.len() && matches!(after_data[i], b' ' | b'\n' | b'\r' | b'\t') {
        i += 1;
    }
    if i + 1 < after_data.len() && after_data[i] == b'E' && after_data[i + 1] == b'I' {
        i += 2;
    }
    i
}

/// Scan for a whitespace-delimited `EI` token followed by a delimiter or EOF.
///
/// Returns the index (within `search`) of the whitespace byte immediately
/// before `EI`, i.e. the end of the image data. Fallback used only when the
/// data length cannot be computed deterministically.
fn scan_for_ei(search: &[u8]) -> Option<usize> {
    for i in 0..search.len().saturating_sub(1) {
        if matches!(search[i], b' ' | b'\n' | b'\r' | b'\t')
            && search[i + 1] == b'E'
            && i + 2 < search.len()
            && search[i + 2] == b'I'
        {
            let after_ei = i + 3;
            if after_ei >= search.len()
                || matches!(
                    search[after_ei],
                    b' ' | b'\n' | b'\r' | b'\t' | b'/' | b'<' | b'['
                )
            {
                return Some(i);
            }
        }
    }
    None
}

/// A lazy, streaming content stream parser.
///
/// Yields one `Operation` at a time, keeping only the current operand stack
/// (≤ ~6 items) in memory. This matches the one-pass behaviour of ONLYOFFICE's
/// `Gfx::display()` and avoids buffering the entire page into a `Vec<Operation>`.
pub struct ContentStreamIter<'a> {
    lexer: Lexer<'a>,
    operand_stack: Vec<PdfObject>,
    done: bool,
}

impl<'a> ContentStreamIter<'a> {
    /// Create an iterator over the given raw content stream bytes.
    pub fn new(data: &'a [u8]) -> Self {
        ContentStreamIter {
            lexer: Lexer::new(data),
            operand_stack: Vec::new(),
            done: false,
        }
    }
}

impl<'a> Iterator for ContentStreamIter<'a> {
    type Item = Result<Operation>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            if self.lexer.is_eof() {
                return None;
            }
            let token = match self.lexer.next_token() {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("content stream lexer error: {}, stopping", e);
                    self.done = true;
                    return None;
                }
            };
            match token {
                Token::Eof => return None,
                Token::Integer(n) => self.operand_stack.push(PdfObject::Integer(n)),
                Token::Real(r) => self.operand_stack.push(PdfObject::Real(r)),
                Token::LiteralString(s) => self.operand_stack.push(PdfObject::String(s)),
                Token::HexString(s) => self.operand_stack.push(PdfObject::String(s)),
                Token::Name(n) => self.operand_stack.push(PdfObject::Name(n)),
                Token::Boolean(b) => self.operand_stack.push(PdfObject::Boolean(b)),
                Token::Null => self.operand_stack.push(PdfObject::Null),
                Token::ArrayStart => match parse_array_operand(&mut self.lexer) {
                    Ok(arr) => self.operand_stack.push(PdfObject::Array(arr)),
                    Err(e) => return Some(Err(e)),
                },
                Token::DictStart => match parse_dict_operand(&mut self.lexer) {
                    Ok(dict) => self.operand_stack.push(PdfObject::Dictionary(dict)),
                    Err(e) => return Some(Err(e)),
                },
                Token::Operator(op) => {
                    if op == "BI" {
                        match parse_inline_image(&mut self.lexer, &mut self.operand_stack) {
                            Ok(inline_op) => return Some(Ok(inline_op)),
                            Err(e) => return Some(Err(e)),
                        }
                    } else {
                        return Some(Ok(Operation {
                            operands: std::mem::take(&mut self.operand_stack),
                            operator: op,
                        }));
                    }
                }
                Token::Keyword(kw) => {
                    let op_name = match kw {
                        Keyword::Other(s) => s,
                        Keyword::R => "R".to_string(),
                        _ => {
                            log::warn!("unexpected keyword in content stream: {:?}", kw);
                            continue;
                        }
                    };
                    if op_name == "BI" {
                        match parse_inline_image(&mut self.lexer, &mut self.operand_stack) {
                            Ok(inline_op) => return Some(Ok(inline_op)),
                            Err(e) => return Some(Err(e)),
                        }
                    } else {
                        return Some(Ok(Operation {
                            operands: std::mem::take(&mut self.operand_stack),
                            operator: op_name,
                        }));
                    }
                }
                Token::ArrayEnd | Token::DictEnd => {
                    log::warn!("unexpected delimiter in content stream");
                }
            }
        }
    }
}

/// Serialize a list of parsed operations back into raw content stream bytes.
///
/// Each operation is written as `operand1 operand2 … operator\n`.
/// Operand values are rendered using the standard PDF object serializer so the
/// result can be decoded again by `parse_content_stream`.
#[cfg(feature = "writer")]
pub fn serialize_operations(ops: &[Operation]) -> Vec<u8> {
    let mut out = Vec::new();
    for op in ops {
        for operand in &op.operands {
            crate::writer::serializer::serialize_object(operand, &mut out);
            out.push(b' ');
        }
        out.extend_from_slice(op.operator.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Expand abbreviated inline image dictionary keys to full names.
fn expand_inline_image_key(key: &str) -> String {
    match key {
        "BPC" => "BitsPerComponent".to_string(),
        "CS" => "ColorSpace".to_string(),
        "D" => "Decode".to_string(),
        "DP" => "DecodeParms".to_string(),
        "F" => "Filter".to_string(),
        "H" => "Height".to_string(),
        "IM" => "ImageMask".to_string(),
        "I" => "Interpolate".to_string(),
        "W" => "Width".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_operators() {
        let data = b"q 1 0 0 1 72 720 cm Q";
        let ops = parse_content_stream(data).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].operator, "q");
        assert_eq!(ops[0].operands.len(), 0);
        assert_eq!(ops[1].operator, "cm");
        assert_eq!(ops[1].operands.len(), 6);
        assert_eq!(ops[2].operator, "Q");
    }

    #[test]
    fn test_parse_text_operators() {
        let data = b"BT /F1 12 Tf 72 700 Td (Hello World) Tj ET";
        let ops = parse_content_stream(data).unwrap();
        assert_eq!(ops.len(), 5);
        assert_eq!(ops[0].operator, "BT");
        assert_eq!(ops[1].operator, "Tf");
        assert_eq!(ops[1].operands[0], PdfObject::Name("F1".into()));
        assert_eq!(ops[1].operands[1], PdfObject::Integer(12));
        assert_eq!(ops[2].operator, "Td");
        assert_eq!(ops[3].operator, "Tj");
        assert_eq!(
            ops[3].operands[0],
            PdfObject::String(b"Hello World".to_vec())
        );
        assert_eq!(ops[4].operator, "ET");
    }

    #[test]
    fn test_parse_tj_array() {
        let data = b"BT [(Hello) -120 (World)] TJ ET";
        let ops = parse_content_stream(data).unwrap();
        assert_eq!(ops[1].operator, "TJ");
        match &ops[1].operands[0] {
            PdfObject::Array(arr) => {
                assert_eq!(arr.len(), 3);
                assert_eq!(arr[0], PdfObject::String(b"Hello".to_vec()));
                assert_eq!(arr[1], PdfObject::Integer(-120));
                assert_eq!(arr[2], PdfObject::String(b"World".to_vec()));
            }
            _ => panic!("expected array operand"),
        }
    }

    #[test]
    fn test_parse_color_operators() {
        let data = b"0.5 0.2 0.8 rg 1 0 0 RG";
        let ops = parse_content_stream(data).unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].operator, "rg");
        assert_eq!(ops[0].operands.len(), 3);
        assert_eq!(ops[1].operator, "RG");
        assert_eq!(ops[1].operands.len(), 3);
    }

    #[test]
    fn test_parse_path_operators() {
        let data = b"100 200 m 300 400 l 100 200 300 400 500 600 c h S";
        let ops = parse_content_stream(data).unwrap();
        assert_eq!(ops.len(), 5);
        assert_eq!(ops[0].operator, "m");
        assert_eq!(ops[0].operands.len(), 2);
        assert_eq!(ops[1].operator, "l");
        assert_eq!(ops[2].operator, "c");
        assert_eq!(ops[2].operands.len(), 6);
        assert_eq!(ops[3].operator, "h");
        assert_eq!(ops[4].operator, "S");
    }

    #[test]
    fn test_parse_empty_stream() {
        let data = b"";
        let ops = parse_content_stream(data).unwrap();
        assert!(ops.is_empty());
    }

    #[cfg(feature = "writer")]
    #[test]
    fn serialize_empty_ops_is_empty() {
        let out = serialize_operations(&[]);
        assert!(out.is_empty());
    }

    #[cfg(feature = "writer")]
    #[test]
    fn serialize_round_trips_simple_ops() {
        let src = b"q 1 0 0 1 10 20 cm Q";
        let ops = parse_content_stream(src).unwrap();
        let bytes = serialize_operations(&ops);
        let ops2 = parse_content_stream(&bytes).unwrap();
        assert_eq!(ops.len(), ops2.len());
        for (a, b) in ops.iter().zip(ops2.iter()) {
            assert_eq!(a.operator, b.operator);
            assert_eq!(a.operands.len(), b.operands.len());
        }
    }

    #[test]
    fn test_parse_inline_image() {
        // Minimal 2×2 RGB 8-bpc inline image = 6 bytes/row × 2 = 12 data bytes.
        let data = b"BI /W 2 /H 2 /BPC 8 /CS /RGB ID \x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B EI Q";
        let ops = parse_content_stream(data).unwrap();
        assert_eq!(ops[0].operator, "BI");
        match &ops[0].operands[0] {
            PdfObject::Dictionary(d) => {
                assert_eq!(d.get("Width"), Some(&PdfObject::Integer(2)));
                assert_eq!(d.get("Height"), Some(&PdfObject::Integer(2)));
            }
            _ => panic!("expected dict operand for inline image"),
        }
        // The data operand must be exactly the 12 raster bytes.
        assert_eq!(ops[0].operands[1], PdfObject::String((0u8..12).collect()));
        // The operator after the image must be parsed correctly (proves the
        // lexer resumed exactly past `EI`).
        assert_eq!(ops[1].operator, "Q");
    }

    #[test]
    fn inline_image_unfiltered_data_may_contain_ei_bytes() {
        // Regression for TD-7: an unfiltered raster whose bytes spell a
        // whitespace-delimited "EI" must NOT terminate early. The exact length
        // (1×4 RGB 8-bpc = 12 bytes) is computed from the geometry.
        // Data deliberately contains ' ', 'E', 'I', ' ' at the front.
        let mut data = b"BI /W 4 /H 1 /BPC 8 /CS /RGB ID ".to_vec();
        let raster = [b' ', b'E', b'I', b' ', 1, 2, 3, 4, 5, 6, 7, 8]; // 12 bytes
        data.extend_from_slice(&raster);
        data.extend_from_slice(b" EI Q");
        let ops = parse_content_stream(&data).unwrap();
        assert_eq!(ops[0].operator, "BI");
        assert_eq!(ops[0].operands[1], PdfObject::String(raster.to_vec()));
        assert_eq!(ops[1].operator, "Q", "lexer must resume after the real EI");
    }

    #[test]
    fn inline_image_explicit_length_is_respected() {
        // /L gives the byte count directly; data contains an "EI" lookalike.
        let mut data = b"BI /W 1 /H 1 /L 5 /F /AHx ID ".to_vec();
        let payload = [b'A', b' ', b'E', b'I', b'Z']; // 5 bytes
        data.extend_from_slice(&payload);
        data.extend_from_slice(b" EI");
        let ops = parse_content_stream(&data).unwrap();
        assert_eq!(ops[0].operands[1], PdfObject::String(payload.to_vec()));
    }

    #[test]
    fn inline_image_filtered_without_length_falls_back_to_ei_scan() {
        // Flate with no explicit length: we cannot compute the length, so the
        // EI-scan fallback applies (data has no embedded EI here).
        let data = b"BI /W 1 /H 1 /F /Fl ID \xAB\xCD\xEF EI Q";
        let ops = parse_content_stream(data).unwrap();
        assert_eq!(ops[0].operator, "BI");
        assert_eq!(
            ops[0].operands[1],
            PdfObject::String(vec![0xAB, 0xCD, 0xEF])
        );
        assert_eq!(ops[1].operator, "Q");
    }

    #[test]
    fn find_jpeg_eoi_locates_marker() {
        assert_eq!(find_jpeg_eoi(&[0x01, 0xFF, 0xD9, 0x02]), Some(3));
        assert_eq!(find_jpeg_eoi(&[0x01, 0x02, 0x03]), None);
    }
}
