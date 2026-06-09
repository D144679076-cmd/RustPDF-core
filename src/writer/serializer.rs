//! Serialization of `PdfObject` values to PDF byte syntax (ISO 32000-1 §7.3).

use crate::parser::objects::{PdfDict, PdfObject, PdfStream};

// ── Number formatting ─────────────────────────────────────────────────────────

/// Serialize a real number with minimal digits — strips trailing zeros.
pub fn format_real(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    let s = format!("{:.6}", f);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_owned()
}

// ── Name serialization ────────────────────────────────────────────────────────

/// Bytes that must be escaped as `#XX` inside a PDF name.
fn name_needs_escape(b: u8) -> bool {
    matches!(b, 0..=32 | b'#' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | 127..=255)
}

/// Write a PDF name token: `/Name` with `#XX` hex escaping.
pub fn serialize_name(name: &str, out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in name.as_bytes() {
        if name_needs_escape(b) {
            out.push(b'#');
            let hi = b >> 4;
            let lo = b & 0x0F;
            out.push(if hi < 10 { b'0' + hi } else { b'A' + hi - 10 });
            out.push(if lo < 10 { b'0' + lo } else { b'A' + lo - 10 });
        } else {
            out.push(b);
        }
    }
}

// ── String serialization ──────────────────────────────────────────────────────

/// Decide whether bytes can be safely encoded as a literal `(...)` string.
/// We prefer literal for printable ASCII with balanced parentheses.
fn use_literal_string(bytes: &[u8]) -> bool {
    let mut depth: i32 = 0;
    for &b in bytes {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            0..=8 | 11 | 12 | 14..=31 | 127..=255 => return false,
            _ => {}
        }
    }
    depth == 0
}

/// Write a literal PDF string: `(content)` with minimal escaping.
pub fn serialize_literal_string(bytes: &[u8], out: &mut Vec<u8>) {
    out.push(b'(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            b'\n' => {
                out.extend_from_slice(b"\\n");
            }
            b'\r' => {
                out.extend_from_slice(b"\\r");
            }
            b'\t' => {
                out.extend_from_slice(b"\\t");
            }
            _ => out.push(b),
        }
    }
    out.push(b')');
}

/// Write a hex PDF string: `<DEADBEEF>`.
pub fn serialize_hex_string(bytes: &[u8], out: &mut Vec<u8>) {
    out.push(b'<');
    for &b in bytes {
        let hi = b >> 4;
        let lo = b & 0x0F;
        out.push(if hi < 10 { b'0' + hi } else { b'A' + hi - 10 });
        out.push(if lo < 10 { b'0' + lo } else { b'A' + lo - 10 });
    }
    out.push(b'>');
}

/// Write a PDF string choosing literal vs hex encoding automatically.
pub fn serialize_string(bytes: &[u8], out: &mut Vec<u8>) {
    if use_literal_string(bytes) {
        serialize_literal_string(bytes, out);
    } else {
        serialize_hex_string(bytes, out);
    }
}

// ── Dictionary ────────────────────────────────────────────────────────────────

/// Write a PDF dictionary: `<< /Key value ... >>`.
///
/// Entries are emitted in the dictionary's insertion order (`PdfDict` is an
/// `IndexMap`), so a parsed dict round-trips with its original key order — this
/// keeps `/Filter`↔`/DecodeParms` pairing and signed-dict byte layout stable.
/// Output is still deterministic because insertion order is deterministic.
pub fn serialize_dict(dict: &PdfDict, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (key, value) in dict {
        out.push(b'\n');
        serialize_name(key, out);
        out.push(b' ');
        serialize_object(value, out);
    }
    out.extend_from_slice(b"\n>>");
}

// ── Stream ────────────────────────────────────────────────────────────────────

/// Write a PDF stream object: dict + `stream\n` + raw bytes + `\nendstream`.
pub fn serialize_stream(stream: &PdfStream, out: &mut Vec<u8>) {
    // Write dict with /Length reflecting raw_data length
    let mut dict = stream.dict.clone();
    dict.insert(
        "Length".to_owned(),
        PdfObject::Integer(stream.raw_data.len() as i64),
    );
    serialize_dict(&dict, out);
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&stream.raw_data);
    out.extend_from_slice(b"\nendstream");
}

// ── Main dispatcher ───────────────────────────────────────────────────────────

/// Serialize any `PdfObject` to PDF byte syntax.
pub fn serialize_object(obj: &PdfObject, out: &mut Vec<u8>) {
    match obj {
        PdfObject::Null => out.extend_from_slice(b"null"),
        PdfObject::Boolean(b) => {
            if *b {
                out.extend_from_slice(b"true");
            } else {
                out.extend_from_slice(b"false");
            }
        }
        PdfObject::Integer(n) => {
            let s = n.to_string();
            out.extend_from_slice(s.as_bytes());
        }
        PdfObject::Real(f) => {
            let s = format_real(*f);
            out.extend_from_slice(s.as_bytes());
        }
        PdfObject::String(bytes) => serialize_string(bytes, out),
        PdfObject::Name(name) => serialize_name(name, out),
        PdfObject::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                serialize_object(item, out);
            }
            out.push(b']');
        }
        PdfObject::Dictionary(dict) => serialize_dict(dict, out),
        PdfObject::Stream(stream) => serialize_stream(stream, out),
        PdfObject::Reference(num, gen) => {
            let s = format!("{} {} R", num, gen);
            out.extend_from_slice(s.as_bytes());
        }
    }
}

/// Write an indirect object wrapper: `n g obj\n...\nendobj\n`.
/// Records the byte offset (before the header) into `offsets`.
pub fn write_indirect(
    id: u32,
    gen: u32,
    obj: &PdfObject,
    out: &mut Vec<u8>,
    offsets: &mut Vec<(u32, u64)>,
) {
    offsets.push((id, out.len() as u64));
    let header = format!("{} {} obj\n", id, gen);
    out.extend_from_slice(header.as_bytes());
    serialize_object(obj, out);
    out.extend_from_slice(b"\nendobj\n");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a `PdfDict` from alternating key/value pairs (convenience).
pub fn build_dict(pairs: &[(&str, PdfObject)]) -> PdfDict {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::{PdfObject, PdfStream};

    fn ser(obj: &PdfObject) -> Vec<u8> {
        let mut out = Vec::new();
        serialize_object(obj, &mut out);
        out
    }

    #[test]
    fn null() {
        assert_eq!(ser(&PdfObject::Null), b"null");
    }

    #[test]
    fn booleans() {
        assert_eq!(ser(&PdfObject::Boolean(true)), b"true");
        assert_eq!(ser(&PdfObject::Boolean(false)), b"false");
    }

    #[test]
    fn integers() {
        assert_eq!(ser(&PdfObject::Integer(0)), b"0");
        assert_eq!(ser(&PdfObject::Integer(42)), b"42");
        assert_eq!(ser(&PdfObject::Integer(-17)), b"-17");
    }

    #[test]
    fn reals() {
        assert_eq!(format_real(1.0), "1");
        assert_eq!(format_real(-0.5), "-0.5");
        assert_eq!(format_real(3.14), "3.14");
        assert_eq!(format_real(0.0), "0");
        assert_eq!(format_real(100.0), "100");
    }

    #[test]
    fn name_simple() {
        let mut out = Vec::new();
        serialize_name("Type", &mut out);
        assert_eq!(out, b"/Type");
    }

    #[test]
    fn name_with_space_escape() {
        let mut out = Vec::new();
        serialize_name("A B", &mut out);
        assert_eq!(out, b"/A#20B");
    }

    #[test]
    fn name_with_hash() {
        let mut out = Vec::new();
        serialize_name("A#B", &mut out);
        assert_eq!(out, b"/A#23B");
    }

    #[test]
    fn literal_string_simple() {
        let mut out = Vec::new();
        serialize_literal_string(b"Hello", &mut out);
        assert_eq!(out, b"(Hello)");
    }

    #[test]
    fn literal_string_escapes() {
        let mut out = Vec::new();
        serialize_literal_string(b"a\nb\\c", &mut out);
        assert_eq!(out, b"(a\\nb\\\\c)");
    }

    #[test]
    fn hex_string() {
        let mut out = Vec::new();
        serialize_hex_string(b"Hi", &mut out);
        assert_eq!(out, b"<4869>");
    }

    #[test]
    fn binary_string_uses_hex() {
        let bytes: Vec<u8> = vec![0x00, 0xFF, 0x80];
        let mut out = Vec::new();
        serialize_string(&bytes, &mut out);
        assert!(out.starts_with(b"<"), "binary data should use hex encoding");
    }

    #[test]
    fn ascii_string_uses_literal() {
        let mut out = Vec::new();
        serialize_string(b"hello world", &mut out);
        assert!(out.starts_with(b"("));
    }

    #[test]
    fn array_empty() {
        assert_eq!(ser(&PdfObject::Array(vec![])), b"[]");
    }

    #[test]
    fn array_mixed() {
        let arr = PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Boolean(true),
            PdfObject::Name("X".to_owned()),
        ]);
        assert_eq!(ser(&arr), b"[1 true /X]");
    }

    #[test]
    fn dict_serialized() {
        let mut d = PdfDict::new();
        d.insert("Type".to_owned(), PdfObject::Name("Page".to_owned()));
        let obj = PdfObject::Dictionary(d);
        let out = ser(&obj);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("/Type /Page"));
    }

    #[test]
    fn reference() {
        assert_eq!(ser(&PdfObject::Reference(5, 0)), b"5 0 R");
    }

    #[test]
    fn dict_preserves_insertion_order_not_alphabetical() {
        // TD-1: keys are emitted in insertion order, not sorted. This keeps
        // positionally-paired entries (/Filter ↔ /DecodeParms) and signed-dict
        // byte layout stable across a round-trip.
        let mut d = PdfDict::new();
        d.insert(
            "Filter".to_owned(),
            PdfObject::Name("FlateDecode".to_owned()),
        );
        d.insert("Zebra".to_owned(), PdfObject::Integer(1));
        d.insert("Alpha".to_owned(), PdfObject::Integer(2));
        let s = String::from_utf8(ser(&PdfObject::Dictionary(d))).unwrap();
        let pf = s.find("/Filter").unwrap();
        let pz = s.find("/Zebra").unwrap();
        let pa = s.find("/Alpha").unwrap();
        // Insertion order Filter < Zebra < Alpha must hold; alphabetical would
        // have put /Alpha first.
        assert!(pf < pz && pz < pa, "expected insertion order, got: {s}");
    }

    #[test]
    fn stream_length_auto() {
        let mut dict = PdfDict::new();
        dict.insert(
            "Filter".to_owned(),
            PdfObject::Name("FlateDecode".to_owned()),
        );
        let stream = PdfStream {
            dict,
            raw_data: vec![1, 2, 3, 4, 5],
        };
        let mut out = Vec::new();
        serialize_stream(&stream, &mut out);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("/Length 5"));
        assert!(s.contains("stream\n"));
        assert!(s.contains("endstream"));
    }

    #[test]
    fn write_indirect_records_offset() {
        let mut out = Vec::new();
        let mut offsets: Vec<(u32, u64)> = Vec::new();
        out.extend_from_slice(b"XXXX"); // 4 bytes padding
        write_indirect(3, 0, &PdfObject::Integer(99), &mut out, &mut offsets);
        assert_eq!(offsets[0], (3, 4)); // offset = 4 (after padding)
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("3 0 obj"));
        assert!(s.contains("99"));
        assert!(s.contains("endobj"));
    }
}
