//! In-place text replacement in PDF content streams.
//!
//! Unlike the cover-and-redraw approach (white rect + draw_text), this module
//! modifies the original `BT...ET` block in the page content stream so the
//! replacement text uses the same font, size, and position as the original.

use crate::content::operators::{parse_content_stream, Operation};
use crate::document::catalog::Catalog;
use crate::document::page::Page;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::streams::make_flate_stream;

use super::document_editor::PdfEditor;

// ── Public types ──────────────────────────────────────────────────────────────

/// Identifies a text span to replace, matched by position and content.
///
/// Coordinates are in PDF user-space (origin bottom-left, y increases upward).
pub struct TextEditTarget {
    /// X coordinate of the span's left edge (PDF user-space points).
    pub x: f64,
    /// Y coordinate of the span's baseline (PDF user-space points).
    pub y: f64,
    /// Width of the span in points (used for tolerance matching).
    pub width: f64,
    /// Font size in points (used for tolerance matching).
    pub font_size: f64,
    /// The original text content to find.
    pub old_text: String,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Replace a text span in a page's content stream in-place.
///
/// Walks the page's content stream, finds the `Tj` or `TJ` operator whose
/// decoded text matches `target.old_text` at approximately `(target.x, target.y)`,
/// replaces the string operand with `new_text`, and rewrites the page content
/// stream.
///
/// Returns `Ok(true)` if a replacement was made, `Ok(false)` if no match found.
pub fn replace_text_in_page(
    editor: &mut PdfEditor,
    page_index: usize,
    target: &TextEditTarget,
    new_text: &str,
) -> Result<bool> {
    let (page_id, page_dict) = editor.get_page_dict(page_index)?;

    // Collect and decode all content streams for this page.
    let content_bytes = decode_page_contents(&editor.doc, &page_dict)?;
    if content_bytes.is_empty() {
        return Ok(false);
    }

    let mut ops = parse_content_stream(&content_bytes)?;

    let replaced = patch_operations(&mut ops, target, new_text);
    if !replaced {
        return Ok(false);
    }

    // Serialize the modified operations back to bytes.
    let new_bytes = serialize_operations(&ops);

    // Write the new content stream as a single compressed stream object,
    // replacing the page's /Contents entirely.
    let stream = make_flate_stream(&new_bytes, PdfDict::new())?;
    let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

    // Update the page dict to point at the new single stream.
    let mut updated_page = page_dict.clone();
    updated_page.insert("Contents".to_owned(), PdfObject::Reference(stream_id, 0));
    editor.replace_object(page_id, PdfObject::Dictionary(updated_page));

    Ok(true)
}

// ── Content stream helpers ────────────────────────────────────────────────────

/// Decode and concatenate all content streams for a page into a single byte buffer.
///
/// Uses `doc.get_stream_data` for reference streams so encrypted PDFs are
/// decrypted before decompression (mirrors `page::Page::decode_contents`).
fn decode_page_contents(
    doc: &crate::parser::objects::PdfDocument,
    page_dict: &crate::parser::objects::PdfDict,
) -> Result<Vec<u8>> {
    match page_dict.get("Contents") {
        None => Ok(Vec::new()),

        Some(PdfObject::Reference(id, _)) => doc.get_stream_data(*id).map_err(|e| {
            PdfError::invalid_structure(format!("content stream decode failed: {}", e))
        }),

        Some(PdfObject::Array(refs)) => {
            let mut buf = Vec::new();
            for r in refs {
                let decoded = match r {
                    PdfObject::Reference(id, _) => doc.get_stream_data(*id).map_err(|e| {
                        PdfError::invalid_structure(format!("content stream decode failed: {}", e))
                    })?,
                    // Rare: inline stream in array — no object ID, not document-encrypted.
                    _ => match doc.resolve(r)? {
                        PdfObject::Stream(s) => s.decode_with_doc(doc).map_err(|e| {
                            PdfError::invalid_structure(format!(
                                "content stream decode failed: {}",
                                e
                            ))
                        })?,
                        _ => continue,
                    },
                };
                if !decoded.is_empty() {
                    if !buf.is_empty() {
                        buf.push(b'\n');
                    }
                    buf.extend_from_slice(&decoded);
                }
            }
            Ok(buf)
        }

        // Already-resolved inline stream (very rare, no object ID).
        Some(other) => match doc.resolve(other)? {
            PdfObject::Stream(s) => s.decode_with_doc(doc).map_err(|e| {
                PdfError::invalid_structure(format!("content stream decode failed: {}", e))
            }),
            _ => Ok(Vec::new()),
        },
    }
}

/// Walk `ops` and replace the first matching Tj/TJ operand.
///
/// Tracks the current text position using `Tm`, `Td`, `TD`, and `T*` operators.
/// Returns `true` if a replacement was made.
fn patch_operations(ops: &mut [Operation], target: &TextEditTarget, new_text: &str) -> bool {
    // Position tolerance: 1 pt for x, half a line height for y.
    let x_tol = 2.0_f64;
    let y_tol = target.font_size * 0.6;

    // Text position state (PDF user-space, updated by Tm/Td/TD/T*).
    let mut cur_x = 0.0_f64;
    let mut cur_y = 0.0_f64;
    // Line start (set by Tm, updated by T*).
    let mut line_x = 0.0_f64;
    let mut line_y = 0.0_f64;
    // Leading (set by TL, used by T*).
    let mut leading = 0.0_f64;
    let mut in_text = false;

    for op in ops.iter_mut() {
        match op.operator.as_str() {
            "BT" => {
                in_text = true;
                cur_x = 0.0;
                cur_y = 0.0;
                line_x = 0.0;
                line_y = 0.0;
            }
            "ET" => {
                in_text = false;
            }
            "Tm" if in_text && op.operands.len() == 6 => {
                // Tm a b c d e f — sets text matrix; e=x, f=y
                cur_x = op_f64(&op.operands[4]);
                cur_y = op_f64(&op.operands[5]);
                line_x = cur_x;
                line_y = cur_y;
            }
            "Td" | "TD" if in_text && op.operands.len() == 2 => {
                let dx = op_f64(&op.operands[0]);
                let dy = op_f64(&op.operands[1]);
                cur_x = line_x + dx;
                cur_y = line_y + dy;
                line_x = cur_x;
                line_y = cur_y;
                if op.operator == "TD" {
                    leading = -dy;
                }
            }
            "T*" if in_text => {
                cur_x = line_x;
                cur_y = line_y - leading;
                line_x = cur_x;
                line_y = cur_y;
            }
            "TL" if op.operands.len() == 1 => {
                leading = op_f64(&op.operands[0]);
            }
            "Tj" if in_text
                && op.operands.len() == 1
                && position_matches(cur_x, cur_y, target, x_tol, y_tol) =>
            {
                if let Some(decoded) = decode_pdf_string(&op.operands[0]) {
                    if text_matches(&decoded, &target.old_text) {
                        op.operands[0] = PdfObject::String(encode_pdf_string(new_text));
                        return true;
                    }
                }
            }
            "TJ" if in_text
                && op.operands.len() == 1
                && position_matches(cur_x, cur_y, target, x_tol, y_tol) =>
            {
                if let PdfObject::Array(ref arr) = op.operands[0].clone() {
                    let combined: String = arr.iter().filter_map(decode_pdf_string).collect();
                    if text_matches(&combined, &target.old_text) {
                        // Replace the TJ array with a single Tj string.
                        op.operator = "Tj".to_owned();
                        op.operands = vec![PdfObject::String(encode_pdf_string(new_text))];
                        return true;
                    }
                }
            }
            _ => {}
        }
    }

    false
}

fn position_matches(
    cur_x: f64,
    cur_y: f64,
    target: &TextEditTarget,
    x_tol: f64,
    y_tol: f64,
) -> bool {
    (cur_x - target.x).abs() <= x_tol && (cur_y - target.y).abs() <= y_tol
}

fn text_matches(decoded: &str, target: &str) -> bool {
    // Exact match first; fall back to trimmed comparison.
    decoded == target || decoded.trim() == target.trim()
}

// ── Serializer ────────────────────────────────────────────────────────────────

/// Serialize a slice of `Operation`s back to PDF content stream bytes.
///
/// Each operation is written as: `<operand> ... <operand> <operator>\n`.
pub fn serialize_operations(ops: &[Operation]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(ops.len() * 16);
    for op in ops {
        for (i, operand) in op.operands.iter().enumerate() {
            if i > 0 {
                buf.push(b' ');
            }
            serialize_object(operand, &mut buf);
        }
        if !op.operands.is_empty() {
            buf.push(b' ');
        }
        buf.extend_from_slice(op.operator.as_bytes());
        buf.push(b'\n');
    }
    buf
}

fn serialize_object(obj: &PdfObject, buf: &mut Vec<u8>) {
    match obj {
        PdfObject::Integer(n) => buf.extend_from_slice(n.to_string().as_bytes()),
        PdfObject::Real(f) => {
            // Use up to 6 decimal places, strip trailing zeros.
            let s = format!("{:.6}", f);
            let s = s.trim_end_matches('0');
            let s = s.trim_end_matches('.');
            buf.extend_from_slice(s.as_bytes());
        }
        PdfObject::Boolean(b) => buf.extend_from_slice(if *b { b"true" } else { b"false" }),
        PdfObject::Name(n) => {
            buf.push(b'/');
            buf.extend_from_slice(n.as_bytes());
        }
        PdfObject::String(s) => {
            // Write as PDF literal string with escaping.
            buf.push(b'(');
            for &byte in s {
                match byte {
                    b'(' => buf.extend_from_slice(b"\\("),
                    b')' => buf.extend_from_slice(b"\\)"),
                    b'\\' => buf.extend_from_slice(b"\\\\"),
                    b'\r' => buf.extend_from_slice(b"\\r"),
                    b'\n' => buf.extend_from_slice(b"\\n"),
                    b'\t' => buf.extend_from_slice(b"\\t"),
                    _ => buf.push(byte),
                }
            }
            buf.push(b')');
        }
        PdfObject::Array(arr) => {
            buf.push(b'[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    buf.push(b' ');
                }
                serialize_object(item, buf);
            }
            buf.push(b']');
        }
        PdfObject::Dictionary(dict) => {
            buf.extend_from_slice(b"<<");
            for (k, v) in dict {
                buf.push(b'/');
                buf.extend_from_slice(k.as_bytes());
                buf.push(b' ');
                serialize_object(v, buf);
                buf.push(b' ');
            }
            buf.extend_from_slice(b">>");
        }
        PdfObject::Null => buf.extend_from_slice(b"null"),
        // References and streams don't appear in content streams.
        _ => buf.extend_from_slice(b"null"),
    }
}

// ── String helpers ────────────────────────────────────────────────────────────

/// Encode a UTF-8 string as raw PDF string bytes (Latin-1 where possible,
/// otherwise UTF-16BE with BOM).
///
/// The returned `Vec<u8>` is the raw bytes to store in a `PdfObject::String`.
/// The caller is responsible for wrapping in `(...)` when serializing.
pub fn encode_pdf_string(s: &str) -> Vec<u8> {
    // If all chars fit in Latin-1, use that (most common for standard fonts).
    if s.chars().all(|c| (c as u32) <= 0xFF) {
        return s.chars().map(|c| c as u8).collect();
    }
    // Otherwise UTF-16BE with BOM.
    let mut out = vec![0xFE, 0xFF];
    for c in s.encode_utf16() {
        out.push((c >> 8) as u8);
        out.push((c & 0xFF) as u8);
    }
    out
}

/// Decode a `PdfObject::String` to a Rust `String`.
///
/// Handles UTF-16BE (BOM `\xFE\xFF`) and Latin-1 byte strings.
fn decode_pdf_string(obj: &PdfObject) -> Option<String> {
    let bytes = match obj {
        PdfObject::String(b) => b,
        _ => return None,
    };
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16BE
        let words: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&words).ok()
    } else {
        // Latin-1 → UTF-8
        Some(bytes.iter().map(|&b| b as char).collect())
    }
}

/// Extract an `f64` from a `PdfObject::Real` or `PdfObject::Integer`.
fn op_f64(obj: &PdfObject) -> f64 {
    match obj {
        PdfObject::Real(f) => *f,
        PdfObject::Integer(i) => *i as f64,
        _ => 0.0,
    }
}

// ── Font resolution ───────────────────────────────────────────────────────────

/// Resolve a PDF font resource key (e.g. `"F1"`) to the actual font name
/// (e.g. `"Helvetica-Bold"`) by walking the page's `/Resources/Font` dict.
///
/// Returns `"Helvetica"` as a safe fallback on any error.
pub fn resolve_font_name(
    doc: &crate::parser::objects::PdfDocument,
    page_index: usize,
    resource_key: &str,
) -> String {
    (|| -> Option<String> {
        let catalog = Catalog::from_document(doc).ok()?;
        let page_dict = catalog.get_page_dict(doc, page_index).ok()?;
        let page = Page::from_dict(doc, &page_dict).ok()?;

        let resources = &page.resources.raw;
        let font_dict = match resources.get("Font")? {
            PdfObject::Dictionary(d) => d,
            _ => return None,
        };
        let font_ref = font_dict.get(resource_key)?;
        let font_obj = doc.resolve(font_ref).ok()?;
        let font_dict = font_obj.as_dict()?;
        let base_font = font_dict.get("BaseFont")?;
        match base_font {
            PdfObject::Name(n) => Some(n.clone()),
            _ => None,
        }
    })()
    .unwrap_or_else(|| "Helvetica".to_owned())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfObject;

    #[test]
    fn encode_latin1_roundtrip() {
        let s = "Hello World";
        let encoded = encode_pdf_string(s);
        let obj = PdfObject::String(encoded);
        assert_eq!(decode_pdf_string(&obj).unwrap(), s);
    }

    #[test]
    fn encode_utf16_roundtrip() {
        // Use characters outside Latin-1 (> U+00FF) to force the UTF-16BE path.
        let s = "こんにちは";
        let encoded = encode_pdf_string(s);
        // UTF-16BE path: starts with BOM
        assert_eq!(encoded[0], 0xFE);
        assert_eq!(encoded[1], 0xFF);
        let obj = PdfObject::String(encoded);
        assert_eq!(decode_pdf_string(&obj).unwrap(), s);
    }

    #[test]
    fn encode_high_latin1_roundtrip() {
        // Chars in range U+00A0–U+00FF fit in Latin-1; no UTF-16 needed.
        let s = "Héllo Wörld";
        let encoded = encode_pdf_string(s);
        // Latin-1 path: no BOM
        assert!(encoded[0] != 0xFE || encoded[1] != 0xFF);
        let obj = PdfObject::String(encoded);
        assert_eq!(decode_pdf_string(&obj).unwrap(), s);
    }

    #[test]
    fn serialize_simple_ops() {
        let ops = vec![
            Operation {
                operands: vec![],
                operator: "BT".to_owned(),
            },
            Operation {
                operands: vec![
                    PdfObject::Real(1.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(1.0),
                    PdfObject::Real(72.0),
                    PdfObject::Real(720.0),
                ],
                operator: "Tm".to_owned(),
            },
            Operation {
                operands: vec![PdfObject::String(b"Hello".to_vec())],
                operator: "Tj".to_owned(),
            },
            Operation {
                operands: vec![],
                operator: "ET".to_owned(),
            },
        ];
        let bytes = serialize_operations(&ops);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("BT"));
        assert!(text.contains("Tm"));
        assert!(text.contains("(Hello) Tj"));
        assert!(text.contains("ET"));
    }

    #[test]
    fn patch_tj_replaces_matching_span() {
        let mut ops = vec![
            Operation {
                operands: vec![],
                operator: "BT".to_owned(),
            },
            Operation {
                operands: vec![
                    PdfObject::Real(1.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(1.0),
                    PdfObject::Real(72.0),
                    PdfObject::Real(700.0),
                ],
                operator: "Tm".to_owned(),
            },
            Operation {
                operands: vec![PdfObject::String(b"Hello".to_vec())],
                operator: "Tj".to_owned(),
            },
            Operation {
                operands: vec![],
                operator: "ET".to_owned(),
            },
        ];

        let target = TextEditTarget {
            x: 72.0,
            y: 700.0,
            width: 30.0,
            font_size: 12.0,
            old_text: "Hello".to_owned(),
        };
        let replaced = patch_operations(&mut ops, &target, "World");
        assert!(replaced);
        // The Tj operand should now be "World"
        if let PdfObject::String(ref s) = ops[2].operands[0] {
            assert_eq!(s, b"World");
        } else {
            panic!("expected String operand");
        }
    }

    #[test]
    fn patch_tj_no_match_returns_false() {
        let mut ops = vec![
            Operation {
                operands: vec![],
                operator: "BT".to_owned(),
            },
            Operation {
                operands: vec![
                    PdfObject::Real(1.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(1.0),
                    PdfObject::Real(72.0),
                    PdfObject::Real(700.0),
                ],
                operator: "Tm".to_owned(),
            },
            Operation {
                operands: vec![PdfObject::String(b"Hello".to_vec())],
                operator: "Tj".to_owned(),
            },
            Operation {
                operands: vec![],
                operator: "ET".to_owned(),
            },
        ];

        let target = TextEditTarget {
            x: 200.0,
            y: 500.0,
            width: 30.0,
            font_size: 12.0,
            old_text: "Hello".to_owned(),
        };
        let replaced = patch_operations(&mut ops, &target, "World");
        assert!(!replaced);
    }

    #[test]
    fn patch_tj_replaces_via_td() {
        let mut ops = vec![
            Operation {
                operands: vec![],
                operator: "BT".to_owned(),
            },
            Operation {
                operands: vec![
                    PdfObject::Real(1.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(0.0),
                    PdfObject::Real(1.0),
                    PdfObject::Real(50.0),
                    PdfObject::Real(700.0),
                ],
                operator: "Tm".to_owned(),
            },
            // Td moves to (50+22, 700+0) = (72, 700)
            Operation {
                operands: vec![PdfObject::Real(22.0), PdfObject::Real(0.0)],
                operator: "Td".to_owned(),
            },
            Operation {
                operands: vec![PdfObject::String(b"World".to_vec())],
                operator: "Tj".to_owned(),
            },
            Operation {
                operands: vec![],
                operator: "ET".to_owned(),
            },
        ];

        let target = TextEditTarget {
            x: 72.0,
            y: 700.0,
            width: 30.0,
            font_size: 12.0,
            old_text: "World".to_owned(),
        };
        let replaced = patch_operations(&mut ops, &target, "Rust");
        assert!(replaced);
    }
}
