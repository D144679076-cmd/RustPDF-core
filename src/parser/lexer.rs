//! PDF Lexer (Tokenizer) implementation.
//!
//! Provides a binary tokenization framework for PDF documents using the `nom` parser combinator library.
//! The lexer tracks byte offsets and handles edge cases, such as hex escape characters in names,
//! nested parentheses in literal strings, and whitespaces within hex strings.

use crate::error::{PdfError, Result};
use nom::{error::ParseError, IResult};

/// A PDF token representing all basic syntax elements.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Primitives
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),

    // Strings
    LiteralString(Vec<u8>), // (Hello World)
    HexString(Vec<u8>),     // <48656C6C6F>

    // Names
    Name(String), // /Type, /Font#20Name

    // Structural keywords
    Keyword(Keyword),

    // Delimiters
    ArrayStart, // [
    ArrayEnd,   // ]
    DictStart,  // <<
    DictEnd,    // >>

    // Content stream operators
    Operator(String), // BT, ET, Tj, Tf, cm, etc.

    // End of input
    Eof,
}

/// Structural PDF keywords.
#[derive(Debug, Clone, PartialEq)]
pub enum Keyword {
    Obj,
    EndObj,
    Stream,
    EndStream,
    Xref,
    Trailer,
    StartXref,
    R,             // indirect reference marker
    Other(String), // fallback for unknown keywords
}

/// A lexical analyzer for a PDF byte buffer.
pub struct Lexer<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Lexer<'a> {
    /// Create a new Lexer for the given data slice.
    pub fn new(data: &'a [u8]) -> Self {
        Lexer { data, position: 0 }
    }

    /// Retrieve the current position (byte offset) in the buffer.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Retrieve the remaining (unconsumed) slice of the buffer.
    pub fn remaining(&self) -> &[u8] {
        &self.data[self.position..]
    }

    /// Rewind the lexer to an earlier byte offset.
    /// Used for lookahead when disambiguating integers from indirect references.
    pub fn set_position(&mut self, pos: usize) {
        self.position = pos.min(self.data.len());
    }

    /// Check if the lexer has reached the end of the input (ignoring whitespace and comments).
    pub fn is_eof(&self) -> bool {
        let unconsumed = &self.data[self.position..];
        let skipped = skip_whitespace_and_comments(unconsumed);
        skipped.is_empty()
    }

    /// Look ahead and parse the next token without advancing the lexer's position.
    pub fn peek_token(&self) -> Result<Token> {
        let unconsumed = &self.data[self.position..];
        let skipped = skip_whitespace_and_comments(unconsumed);
        if skipped.is_empty() {
            return Ok(Token::Eof);
        }

        match parse_token(skipped) {
            Ok((_, token)) => Ok(token),
            Err(nom::Err::Error(e)) | Err(nom::Err::Failure(e)) => {
                Err(self.map_internal_error(skipped, e))
            }
            Err(nom::Err::Incomplete(_)) => Err(PdfError::eof(self.data.len(), "more data")),
        }
    }

    /// Consume and return the next token from the stream.
    pub fn next_token(&mut self) -> Result<Token> {
        let unconsumed = &self.data[self.position..];
        let skipped = skip_whitespace_and_comments(unconsumed);
        self.position = self.data.len() - skipped.len();

        if skipped.is_empty() {
            return Ok(Token::Eof);
        }

        match parse_token(skipped) {
            Ok((rem, token)) => {
                self.position = self.data.len() - rem.len();
                Ok(token)
            }
            Err(nom::Err::Error(e)) | Err(nom::Err::Failure(e)) => {
                Err(self.map_internal_error(skipped, e))
            }
            Err(nom::Err::Incomplete(_)) => Err(PdfError::eof(self.data.len(), "more data")),
        }
    }

    /// Helper to tokenize all remaining data until EOF is reached.
    pub fn tokenize_all(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token()?;
            if token == Token::Eof {
                break;
            }
            tokens.push(token);
        }
        Ok(tokens)
    }

    /// Central mapping of custom internal error states to public `PdfError` instances.
    fn map_internal_error(&self, _start_input: &[u8], err: InternalError<'a>) -> PdfError {
        match err {
            InternalError::Nom(e) => {
                let offset = self.data.len() - e.input.len();
                PdfError::lexer(offset, format!("Nom error {:?}", e.code))
            }
            InternalError::UnterminatedLiteralString { start_input } => {
                let offset = self.data.len() - start_input.len();
                PdfError::lexer(
                    offset,
                    format!("Unterminated literal string starting at offset {}", offset),
                )
            }
            InternalError::UnterminatedHexString { start_input } => {
                let offset = self.data.len() - start_input.len();
                PdfError::lexer(
                    offset,
                    format!("Unterminated hex string starting at offset {}", offset),
                )
            }
            InternalError::InvalidHexDigitInName { err_input, digit } => {
                let offset = self.data.len() - err_input.len();
                PdfError::lexer(
                    offset,
                    format!(
                        "Invalid hex digit '{}' in name at offset {}",
                        digit as char, offset
                    ),
                )
            }
            InternalError::InvalidHexDigitInHex { err_input, digit } => {
                let offset = self.data.len() - err_input.len();
                PdfError::invalid_token(
                    offset,
                    format!(
                        "Invalid hex digit '{}' in hex string at offset {}",
                        digit as char, offset
                    ),
                )
            }
            InternalError::UnexpectedByte { err_input, byte } => {
                let offset = self.data.len() - err_input.len();
                PdfError::invalid_token(
                    offset,
                    format!("Unexpected byte 0x{:02X} at offset {}", byte, offset),
                )
            }
            InternalError::UnexpectedEof {
                err_input,
                expected,
            } => {
                let offset = self.data.len() - err_input.len();
                PdfError::eof(offset, expected)
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Internal Custom Error Handling
// -----------------------------------------------------------------------------

#[derive(Debug)]
enum InternalError<'a> {
    Nom(nom::error::Error<&'a [u8]>),
    UnterminatedLiteralString {
        start_input: &'a [u8],
    },
    UnterminatedHexString {
        start_input: &'a [u8],
    },
    InvalidHexDigitInName {
        err_input: &'a [u8],
        digit: u8,
    },
    InvalidHexDigitInHex {
        err_input: &'a [u8],
        digit: u8,
    },
    UnexpectedByte {
        err_input: &'a [u8],
        byte: u8,
    },
    UnexpectedEof {
        err_input: &'a [u8],
        expected: String,
    },
}

impl<'a> nom::error::ParseError<&'a [u8]> for InternalError<'a> {
    fn from_error_kind(input: &'a [u8], kind: nom::error::ErrorKind) -> Self {
        InternalError::Nom(nom::error::Error::new(input, kind))
    }

    fn append(_input: &'a [u8], _kind: nom::error::ErrorKind, other: Self) -> Self {
        other
    }
}

// -----------------------------------------------------------------------------
// Helper Character Checking Functions
// -----------------------------------------------------------------------------

#[inline]
fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

#[inline]
fn is_pdf_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Skip whitespaces and PDF comments (starting with `%` until EOL).
fn skip_whitespace_and_comments(mut input: &[u8]) -> &[u8] {
    loop {
        let start_len = input.len();
        // Consume whitespace
        while !input.is_empty() && is_pdf_whitespace(input[0]) {
            input = &input[1..];
        }
        // Consume comments
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
// nom sub-parsers
// -----------------------------------------------------------------------------

fn parse_name(input: &[u8]) -> IResult<&[u8], Token, InternalError<'_>> {
    if input.is_empty() || input[0] != b'/' {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Char,
        )));
    }

    let mut i = 1;
    let mut name_bytes = Vec::new();

    while i < input.len() {
        let b = input[i];
        if is_pdf_whitespace(b) || is_pdf_delimiter(b) {
            break;
        }

        if b == b'#' {
            if i + 2 >= input.len() {
                return Err(nom::Err::Failure(InternalError::UnexpectedEof {
                    err_input: &input[i..],
                    expected: "two hex digits for name escape".to_string(),
                }));
            }
            let h1 = input[i + 1];
            let h2 = input[i + 2];

            let val1 = match h1 {
                b'0'..=b'9' => h1 - b'0',
                b'a'..=b'f' => h1 - b'a' + 10,
                b'A'..=b'F' => h1 - b'A' + 10,
                _ => {
                    return Err(nom::Err::Failure(InternalError::InvalidHexDigitInName {
                        err_input: &input[i + 1..],
                        digit: h1,
                    }))
                }
            };

            let val2 = match h2 {
                b'0'..=b'9' => h2 - b'0',
                b'a'..=b'f' => h2 - b'a' + 10,
                b'A'..=b'F' => h2 - b'A' + 10,
                _ => {
                    return Err(nom::Err::Failure(InternalError::InvalidHexDigitInName {
                        err_input: &input[i + 2..],
                        digit: h2,
                    }))
                }
            };

            name_bytes.push((val1 << 4) | val2);
            i += 3;
        } else {
            name_bytes.push(b);
            i += 1;
        }
    }

    let name_str = String::from_utf8(name_bytes).map_err(|_| {
        nom::Err::Failure(InternalError::Nom(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )))
    })?;

    Ok((&input[i..], Token::Name(name_str)))
}

fn parse_literal_string(input: &[u8]) -> IResult<&[u8], Token, InternalError<'_>> {
    if input.is_empty() || input[0] != b'(' {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Char,
        )));
    }

    let mut i = 1;
    let mut depth = 1;
    let mut string_bytes = Vec::new();

    while i < input.len() {
        let b = input[i];
        if b == b'\\' {
            if i + 1 >= input.len() {
                return Err(nom::Err::Failure(InternalError::UnexpectedEof {
                    err_input: &input[i..],
                    expected: "character after escape backslash".to_string(),
                }));
            }
            let next_b = input[i + 1];
            match next_b {
                b'n' => {
                    string_bytes.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    string_bytes.push(b'\r');
                    i += 2;
                }
                b't' => {
                    string_bytes.push(b'\t');
                    i += 2;
                }
                b'b' => {
                    string_bytes.push(0x08);
                    i += 2;
                }
                b'f' => {
                    string_bytes.push(0x0C);
                    i += 2;
                }
                b'(' => {
                    string_bytes.push(b'(');
                    i += 2;
                }
                b')' => {
                    string_bytes.push(b')');
                    i += 2;
                }
                b'\\' => {
                    string_bytes.push(b'\\');
                    i += 2;
                }
                b'\r' => {
                    if i + 2 < input.len() && input[i + 2] == b'\n' {
                        i += 3;
                    } else {
                        i += 2;
                    }
                }
                b'\n' => {
                    i += 2;
                }
                b'0'..=b'7' => {
                    let mut oct_val = (next_b - b'0') as u32;
                    let mut consumed = 2;

                    if i + 2 < input.len() && matches!(input[i + 2], b'0'..=b'7') {
                        oct_val = oct_val * 8 + (input[i + 2] - b'0') as u32;
                        consumed += 1;
                        if i + 3 < input.len() && matches!(input[i + 3], b'0'..=b'7') {
                            oct_val = oct_val * 8 + (input[i + 3] - b'0') as u32;
                            consumed += 1;
                        }
                    }
                    string_bytes.push((oct_val & 0xFF) as u8);
                    i += consumed;
                }
                _ => {
                    string_bytes.push(next_b);
                    i += 2;
                }
            }
        } else if b == b'(' {
            depth += 1;
            string_bytes.push(b'(');
            i += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                return Ok((&input[i + 1..], Token::LiteralString(string_bytes)));
            }
            string_bytes.push(b')');
            i += 1;
        } else {
            string_bytes.push(b);
            i += 1;
        }
    }

    Err(nom::Err::Failure(
        InternalError::UnterminatedLiteralString { start_input: input },
    ))
}

fn parse_hex_string(input: &[u8]) -> IResult<&[u8], Token, InternalError<'_>> {
    if input.is_empty() || input[0] != b'<' {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Char,
        )));
    }
    if input.len() >= 2 && input[1] == b'<' {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Char,
        )));
    }

    let mut i = 1;
    let mut hex_digits = Vec::new();
    let mut found_end = false;

    while i < input.len() {
        let b = input[i];
        if b == b'>' {
            found_end = true;
            i += 1;
            break;
        } else if is_pdf_whitespace(b) {
            i += 1;
        } else {
            let val = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => {
                    return Err(nom::Err::Failure(InternalError::InvalidHexDigitInHex {
                        err_input: &input[i..],
                        digit: b,
                    }));
                }
            };
            hex_digits.push(val);
            i += 1;
        }
    }

    if !found_end {
        return Err(nom::Err::Failure(InternalError::UnterminatedHexString {
            start_input: input,
        }));
    }

    let mut decoded = Vec::new();
    let mut chunks = hex_digits.chunks_exact(2);
    for chunk in &mut chunks {
        decoded.push((chunk[0] << 4) | chunk[1]);
    }

    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        decoded.push(remainder[0] << 4);
    }

    Ok((&input[i..], Token::HexString(decoded)))
}

fn parse_number(input: &[u8]) -> IResult<&[u8], Token, InternalError<'_>> {
    if input.is_empty() {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Digit,
        )));
    }
    let mut i = 0;
    let sign_char = input[0];
    if sign_char == b'+' || sign_char == b'-' {
        i += 1;
    }

    let mut has_dot = false;
    let mut num_digits_before_dot = 0;
    let mut num_digits_after_dot = 0;

    while i < input.len() {
        let c = input[i];
        if c.is_ascii_digit() {
            if has_dot {
                num_digits_after_dot += 1;
            } else {
                num_digits_before_dot += 1;
            }
            i += 1;
        } else if c == b'.' {
            if has_dot {
                break;
            }
            has_dot = true;
            i += 1;
        } else {
            break;
        }
    }

    let total_digits = num_digits_before_dot + num_digits_after_dot;
    if total_digits == 0 {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Digit,
        )));
    }

    let num_bytes = &input[0..i];
    let num_str = std::str::from_utf8(num_bytes).map_err(|_| {
        nom::Err::Failure(InternalError::Nom(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )))
    })?;

    let token = if has_dot {
        let val: f64 = num_str.parse().map_err(|_| {
            nom::Err::Failure(InternalError::Nom(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Digit,
            )))
        })?;
        Token::Real(val)
    } else {
        let val: i64 = num_str.parse().map_err(|_| {
            nom::Err::Failure(InternalError::Nom(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Digit,
            )))
        })?;
        Token::Integer(val)
    };

    Ok((&input[i..], token))
}

fn parse_delimiter(input: &[u8]) -> IResult<&[u8], Token, InternalError<'_>> {
    if input.is_empty() {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Char,
        )));
    }
    if input.starts_with(b"<<") {
        Ok((&input[2..], Token::DictStart))
    } else if input.starts_with(b">>") {
        Ok((&input[2..], Token::DictEnd))
    } else if input[0] == b'[' {
        Ok((&input[1..], Token::ArrayStart))
    } else if input[0] == b']' {
        Ok((&input[1..], Token::ArrayEnd))
    } else {
        Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::Char,
        )))
    }
}

fn parse_identifier(input: &[u8]) -> IResult<&[u8], &[u8], InternalError<'_>> {
    if input.is_empty() {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::AlphaNumeric,
        )));
    }
    if is_pdf_whitespace(input[0]) || is_pdf_delimiter(input[0]) {
        return Err(nom::Err::Error(InternalError::from_error_kind(
            input,
            nom::error::ErrorKind::AlphaNumeric,
        )));
    }

    let mut i = 0;
    while i < input.len() && !is_pdf_whitespace(input[i]) && !is_pdf_delimiter(input[i]) {
        i += 1;
    }
    Ok((&input[i..], &input[0..i]))
}

fn is_operator(s: &str) -> bool {
    matches!(
        s,
        "b" | "B"
            | "b*"
            | "B*"
            | "BDC"
            | "BMC"
            | "BT"
            | "BX"
            | "c"
            | "cm"
            | "CS"
            | "cs"
            | "d"
            | "d0"
            | "d1"
            | "Do"
            | "DP"
            | "EI"
            | "EMC"
            | "ET"
            | "EX"
            | "f"
            | "F"
            | "f*"
            | "g"
            | "G"
            | "gs"
            | "h"
            | "i"
            | "ID"
            | "j"
            | "J"
            | "K"
            | "k"
            | "l"
            | "m"
            | "M"
            | "MP"
            | "n"
            | "q"
            | "Q"
            | "re"
            | "RG"
            | "rg"
            | "ri"
            | "s"
            | "S"
            | "sc"
            | "SC"
            | "scn"
            | "SCN"
            | "sh"
            | "T*"
            | "Tc"
            | "Td"
            | "TD"
            | "Tf"
            | "Tj"
            | "TJ"
            | "TL"
            | "Tm"
            | "Tr"
            | "Ts"
            | "Tw"
            | "Tz"
            | "v"
            | "w"
            | "W"
            | "W*"
            | "y"
            | "'"
            | "\""
    )
}

fn classify_identifier<'a>(
    bytes: &[u8],
    original_input: &'a [u8],
) -> std::result::Result<Token, nom::Err<InternalError<'a>>> {
    let s = std::str::from_utf8(bytes).map_err(|_| {
        nom::Err::Failure(InternalError::UnexpectedByte {
            err_input: original_input,
            byte: bytes[0],
        })
    })?;

    let token = match s {
        "null" => Token::Null,
        "true" => Token::Boolean(true),
        "false" => Token::Boolean(false),
        "obj" => Token::Keyword(Keyword::Obj),
        "endobj" => Token::Keyword(Keyword::EndObj),
        "stream" => Token::Keyword(Keyword::Stream),
        "endstream" => Token::Keyword(Keyword::EndStream),
        "xref" => Token::Keyword(Keyword::Xref),
        "trailer" => Token::Keyword(Keyword::Trailer),
        "startxref" => Token::Keyword(Keyword::StartXref),
        "R" => Token::Keyword(Keyword::R),
        _ => {
            if is_operator(s) {
                Token::Operator(s.to_string())
            } else {
                Token::Keyword(Keyword::Other(s.to_string()))
            }
        }
    };
    Ok(token)
}

fn parse_token(input: &[u8]) -> IResult<&[u8], Token, InternalError<'_>> {
    if input.is_empty() {
        return Ok((input, Token::Eof));
    }

    let first = input[0];
    match first {
        b'/' => parse_name(input),
        b'(' => parse_literal_string(input),
        b'<' => {
            if input.starts_with(b"<<") {
                parse_delimiter(input)
            } else {
                parse_hex_string(input)
            }
        }
        b'>' | b'[' | b']' => parse_delimiter(input),
        b'+' | b'-' | b'.' | b'0'..=b'9' => match parse_number(input) {
            Ok(res) => Ok(res),
            Err(_) => {
                let (rem, id_bytes) = parse_identifier(input)?;
                let token = classify_identifier(id_bytes, input)?;
                Ok((rem, token))
            }
        },
        _ => match parse_identifier(input) {
            Ok((rem, id_bytes)) => {
                let token = classify_identifier(id_bytes, input)?;
                Ok((rem, token))
            }
            Err(_) => Err(nom::Err::Failure(InternalError::UnexpectedByte {
                err_input: input,
                byte: first,
            })),
        },
    }
}

// -----------------------------------------------------------------------------
// Unit Tests (30 Comprehensive Test Cases)
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // --- Numbers ---

    #[test]
    fn test_positive_integer() {
        let mut lexer = Lexer::new(b"42");
        assert_eq!(lexer.next_token().unwrap(), Token::Integer(42));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_negative_integer() {
        let mut lexer = Lexer::new(b"-17");
        assert_eq!(lexer.next_token().unwrap(), Token::Integer(-17));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_positive_real() {
        let mut lexer = Lexer::new(b"3.14");
        assert_eq!(lexer.next_token().unwrap(), Token::Real(3.14));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_negative_real() {
        let mut lexer = Lexer::new(b"-2.5");
        assert_eq!(lexer.next_token().unwrap(), Token::Real(-2.5));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_leading_dot_real() {
        let mut lexer = Lexer::new(b".5");
        assert_eq!(lexer.next_token().unwrap(), Token::Real(0.5));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_zero() {
        let mut lexer = Lexer::new(b"0");
        assert_eq!(lexer.next_token().unwrap(), Token::Integer(0));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    // --- Names ---

    #[test]
    fn test_simple_name() {
        let mut lexer = Lexer::new(b"/Type");
        assert_eq!(lexer.next_token().unwrap(), Token::Name("Type".to_string()));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_name_with_hex() {
        let mut lexer = Lexer::new(b"/A#20B");
        assert_eq!(lexer.next_token().unwrap(), Token::Name("A B".to_string()));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_empty_name() {
        let mut lexer = Lexer::new(b"/");
        assert_eq!(lexer.next_token().unwrap(), Token::Name("".to_string()));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_name_with_special() {
        let mut lexer = Lexer::new(b"/Lime#23Green");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::Name("Lime#Green".to_string())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    // --- Strings ---

    #[test]
    fn test_simple_literal_string() {
        let mut lexer = Lexer::new(b"(Hello)");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::LiteralString(b"Hello".to_vec())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_nested_parens() {
        let mut lexer = Lexer::new(b"(a(b)c)");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::LiteralString(b"a(b)c".to_vec())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_escape_sequences() {
        let mut lexer = Lexer::new(b"(a\\nb\\\\c)");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::LiteralString(b"a\nb\\c".to_vec())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_octal_escape() {
        let mut lexer = Lexer::new(b"(\\110\\145)");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::LiteralString(b"He".to_vec())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_empty_literal_string() {
        let mut lexer = Lexer::new(b"()");
        assert_eq!(lexer.next_token().unwrap(), Token::LiteralString(vec![]));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_simple_hex_string() {
        let mut lexer = Lexer::new(b"<48656C6C6F>");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::HexString(b"Hello".to_vec())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_hex_with_whitespace() {
        let mut lexer = Lexer::new(b"<48 65 6C>");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::HexString(b"Hel".to_vec())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_odd_length_hex() {
        let mut lexer = Lexer::new(b"<ABC>");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::HexString(vec![0xAB, 0xC0])
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_empty_hex_string() {
        let mut lexer = Lexer::new(b"<>");
        assert_eq!(lexer.next_token().unwrap(), Token::HexString(vec![]));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    // --- Keywords ---

    #[test]
    fn test_null() {
        let mut lexer = Lexer::new(b"null");
        assert_eq!(lexer.next_token().unwrap(), Token::Null);
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_booleans() {
        let mut lexer = Lexer::new(b"true false");
        assert_eq!(lexer.next_token().unwrap(), Token::Boolean(true));
        assert_eq!(lexer.next_token().unwrap(), Token::Boolean(false));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_obj_endobj() {
        let mut lexer = Lexer::new(b"obj endobj");
        assert_eq!(lexer.next_token().unwrap(), Token::Keyword(Keyword::Obj));
        assert_eq!(lexer.next_token().unwrap(), Token::Keyword(Keyword::EndObj));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_stream_endstream() {
        let mut lexer = Lexer::new(b"stream endstream");
        assert_eq!(lexer.next_token().unwrap(), Token::Keyword(Keyword::Stream));
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::Keyword(Keyword::EndStream)
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    // --- Delimiters ---

    #[test]
    fn test_array_delimiters() {
        let mut lexer = Lexer::new(b"[ ]");
        assert_eq!(lexer.next_token().unwrap(), Token::ArrayStart);
        assert_eq!(lexer.next_token().unwrap(), Token::ArrayEnd);
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn test_dict_delimiters() {
        let mut lexer = Lexer::new(b"<< >>");
        assert_eq!(lexer.next_token().unwrap(), Token::DictStart);
        assert_eq!(lexer.next_token().unwrap(), Token::DictEnd);
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    // --- Operators ---

    #[test]
    fn test_text_operators() {
        let mut lexer = Lexer::new(b"BT ET Tj TJ");
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::Operator("BT".to_string())
        );
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::Operator("ET".to_string())
        );
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::Operator("Tj".to_string())
        );
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::Operator("TJ".to_string())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    // --- Whitespace ---

    #[test]
    fn test_whitespace_comments() {
        let mut lexer = Lexer::new(b"% comment\n42");
        assert_eq!(lexer.next_token().unwrap(), Token::Integer(42));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    // --- Complex ---

    #[test]
    fn test_full_object_definition() {
        let mut lexer = Lexer::new(b"1 0 obj << /Type /Page >> endobj");
        let tokens = lexer.tokenize_all().unwrap();
        let expected = vec![
            Token::Integer(1),
            Token::Integer(0),
            Token::Keyword(Keyword::Obj),
            Token::DictStart,
            Token::Name("Type".to_string()),
            Token::Name("Page".to_string()),
            Token::DictEnd,
            Token::Keyword(Keyword::EndObj),
        ];
        assert_eq!(tokens, expected);
    }

    // --- Errors ---

    #[test]
    fn test_unterminated_literal_string() {
        let mut lexer = Lexer::new(b"(hello");
        let result = lexer.next_token();
        assert!(result.is_err());
        let err = result.err().unwrap();
        match err {
            PdfError::LexerError { offset, message } => {
                assert_eq!(offset, 0);
                assert!(message.contains("Unterminated literal string starting at offset 0"));
            }
            _ => panic!("Expected PdfError::LexerError"),
        }
    }

    #[test]
    fn test_unexpected_byte() {
        let mut lexer = Lexer::new(b"\xFF");
        let result = lexer.next_token();
        assert!(result.is_err());
        let err = result.err().unwrap();
        match err {
            PdfError::InvalidToken { offset, detail } => {
                assert_eq!(offset, 0);
                assert!(detail.contains("Unexpected byte 0xFF at offset 0"));
            }
            _ => panic!("Expected PdfError::InvalidToken"),
        }
    }
}
