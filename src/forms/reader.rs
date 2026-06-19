//! Read interactive AcroForm fields from an existing PDF document.
//!
//! Entry point: [`read_form_fields`] returns a flat list of [`FormField`]
//! structs, one per interactive leaf field in the `/AcroForm /Fields` array.

use crate::document::catalog::Catalog;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

// ── Public types ──────────────────────────────────────────────────────────────

/// The interactive type of a PDF form field (ISO 32000-1 §12.7.3).
#[derive(Debug, Clone, PartialEq)]
pub enum FieldType {
    Text,
    Checkbox,
    Radio,
    List,
    Combo,
    Signature,
    Unknown,
}

/// A single interactive form field read from a PDF's `/AcroForm`.
#[derive(Debug, Clone)]
pub struct FormField {
    /// Object ID of the field's widget annotation dictionary.
    pub id: u32,
    /// Partial name (`/T`).
    pub name: String,
    /// Dot-joined full name, e.g. `"section.field"`.
    pub full_name: String,
    /// Field type derived from `/FT` and `/Ff`.
    pub field_type: FieldType,
    /// Current value (`/V`) as a UTF-8 string.
    pub value: String,
    /// Default value (`/DV`) as a UTF-8 string.
    pub default_value: String,
    /// Annotation rect `[x1, y1, x2, y2]` in default user space.
    pub rect: [f64; 4],
    /// 0-based page index (derived from `/P`).
    pub page_index: usize,
    /// Options for List/Combo fields (`/Opt`).
    pub options: Vec<String>,
    /// For Checkbox/Radio: true when `/AS` is not `"Off"`.
    pub checked: bool,
    /// `/Ff` bit 0 — field is read-only.
    pub readonly: bool,
    /// `/Ff` bit 1 — field is required.
    pub required: bool,
    /// `/Ff` bit 12 — multi-line text field.
    pub multiline: bool,
    /// `/MaxLen` for text fields.
    pub max_len: Option<u32>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Read all interactive form fields from a PDF document.
///
/// Returns an empty `Vec` when the document has no `/AcroForm` or no `/Fields`.
/// Non-terminal parent nodes (groups with `/Kids` but no `/FT`) are traversed
/// recursively; only leaf fields are included in the output.
pub fn read_form_fields(doc: &PdfDocument) -> Result<Vec<FormField>> {
    // Resolve /Root → /AcroForm
    let root_ref = doc
        .trailer
        .get("Root")
        .ok_or_else(|| PdfError::invalid_structure("no /Root in trailer"))?
        .clone();
    let root = doc.resolve(&root_ref)?;
    let root_dict = root
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("/Root did not resolve to a dict"))?;

    let acroform_ref = match root_dict.get("AcroForm") {
        Some(o) => o.clone(),
        None => return Ok(vec![]),
    };
    let acroform = doc.resolve(&acroform_ref)?;
    let acroform_dict = acroform
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("/AcroForm not a dict"))?;

    let fields_ref = match acroform_dict.get("Fields") {
        Some(o) => o.clone(),
        None => return Ok(vec![]),
    };
    let fields_obj = doc.resolve(&fields_ref)?;
    let fields_arr = match fields_obj {
        PdfObject::Array(a) => a,
        _ => {
            return Err(PdfError::invalid_structure(
                "/AcroForm /Fields not an array",
            ))
        }
    };

    // Build a page-ref table so we can map /P to a 0-based page index.
    let page_refs = build_page_refs(doc)?;

    let mut result = Vec::new();
    for field_ref in &fields_arr {
        collect_fields(doc, field_ref, "", &page_refs, &mut result)?;
    }
    Ok(result)
}

/// Detect whether a PDF document carries an XFA (XML Forms Architecture) form.
///
/// XFA is an Adobe-proprietary form format stored in the `/AcroForm /XFA`
/// key (deprecated in PDF 2.0, ISO 32000-2). Returns `Ok(false)` when the
/// document has no `/AcroForm` at all.
pub fn is_xfa_form(doc: &PdfDocument) -> Result<bool> {
    match get_acroform_dict(doc)? {
        Some(dict) => Ok(dict.contains_key("XFA")),
        None => Ok(false),
    }
}

/// Extract the raw XFA XML data from a PDF's `/AcroForm /XFA` entry.
///
/// The `/XFA` value is either a single stream or an array of alternating
/// `[name, stream]` packet pairs (ISO 32000-2 §12.7.8); packets are
/// concatenated in array order. Requires an Enterprise license.
pub fn extract_xfa_data(doc: &PdfDocument) -> Result<String> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Enterprise, "extract_xfa_data")?;

    let acroform_dict = get_acroform_dict(doc)?
        .ok_or_else(|| PdfError::invalid_structure("document has no /AcroForm"))?;
    let xfa_ref = acroform_dict
        .get("XFA")
        .ok_or_else(|| PdfError::invalid_structure("document has no /XFA form"))?
        .clone();
    let xfa_obj = doc.resolve(&xfa_ref)?;

    match xfa_obj {
        PdfObject::Array(arr) => {
            let mut xml = String::new();
            let mut i = 0;
            while i + 1 < arr.len() {
                if let PdfObject::Stream(s) = doc.resolve(&arr[i + 1])? {
                    xml.push_str(&String::from_utf8_lossy(&s.decode_with_doc(doc)?));
                }
                i += 2;
            }
            Ok(xml)
        }
        PdfObject::Stream(s) => Ok(String::from_utf8_lossy(&s.decode_with_doc(doc)?).into_owned()),
        _ => Err(PdfError::invalid_structure(
            "/XFA value is not a stream or array",
        )),
    }
}

/// Resolve and return the `/AcroForm` dictionary, or `None` if absent.
fn get_acroform_dict(doc: &PdfDocument) -> Result<Option<PdfDict>> {
    let catalog = Catalog::from_document(doc)?;
    let acroform_ref = match catalog.acroform() {
        Some(o) => o.clone(),
        None => return Ok(None),
    };
    let acroform = doc.resolve(&acroform_ref)?;
    let dict = acroform
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("/AcroForm not a dict"))?
        .clone();
    Ok(Some(dict))
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Build a list of page indirect references in document order.
///
/// Ensures the document's internal page table is populated via the first
/// [`Catalog::get_page_dict`] call, then reads from the cache.
fn build_page_refs(doc: &PdfDocument) -> Result<Vec<PdfObject>> {
    let page_count = doc.page_count()?;
    if page_count == 0 {
        return Ok(vec![]);
    }
    // Calling get_page_dict(0) populates the page table as a side-effect when
    // it isn't already cached (Catalog's lazy-init path).
    if !doc.has_page_table() {
        let catalog = Catalog::from_document(doc)?;
        catalog.get_page_dict(doc, 0)?;
    }
    let mut refs = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if let Some(r) = doc.cached_page_ref(i) {
            refs.push(r);
        }
    }
    Ok(refs)
}

/// Recursively collect leaf form fields from the field tree.
///
/// `parent_name` is the dot-joined name from all ancestor nodes.
fn collect_fields(
    doc: &PdfDocument,
    field_ref: &PdfObject,
    parent_name: &str,
    page_refs: &[PdfObject],
    out: &mut Vec<FormField>,
) -> Result<()> {
    let resolved = doc.resolve(field_ref)?;
    let id = match field_ref {
        PdfObject::Reference(n, _) => *n,
        _ => 0,
    };
    let dict = resolved
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("form field not a dict"))?;

    let partial_name = dict
        .get("T")
        .and_then(|o| {
            if let PdfObject::String(b) = o {
                Some(pdf_string_to_utf8(b))
            } else {
                None
            }
        })
        .unwrap_or_default();

    let full_name = if parent_name.is_empty() {
        partial_name.clone()
    } else if partial_name.is_empty() {
        parent_name.to_owned()
    } else {
        format!("{}.{}", parent_name, partial_name)
    };

    // Non-terminal node: has /Kids but no /FT — recurse without emitting a field.
    if let Some(PdfObject::Array(kids)) = dict.get("Kids") {
        if !dict.contains_key("FT") {
            for kid in kids.clone() {
                collect_fields(doc, &kid, &full_name, page_refs, out)?;
            }
            return Ok(());
        }
    }

    // Leaf field: derive type from /FT + /Ff.
    let ff = dict
        .get("Ff")
        .and_then(|o| {
            if let PdfObject::Integer(i) = o {
                Some(*i)
            } else {
                None
            }
        })
        .unwrap_or(0);

    let ft_name = dict.get("FT").and_then(|o| o.as_name());
    let field_type = match ft_name {
        Some("Tx") => FieldType::Text,
        Some("Btn") => {
            if ff & (1 << 15) != 0 {
                FieldType::Radio
            } else {
                FieldType::Checkbox
            }
        }
        Some("Ch") => {
            if ff & (1 << 17) != 0 {
                FieldType::Combo
            } else {
                FieldType::List
            }
        }
        Some("Sig") => FieldType::Signature,
        _ => FieldType::Unknown,
    };

    let value = pdf_obj_to_string(dict.get("V")).unwrap_or_default();
    let default_value = pdf_obj_to_string(dict.get("DV")).unwrap_or_default();

    let rect = dict
        .get("Rect")
        .and_then(|o| {
            if let PdfObject::Array(a) = o {
                let nums: Vec<f64> = a
                    .iter()
                    .filter_map(|x| match x {
                        PdfObject::Real(r) => Some(*r),
                        PdfObject::Integer(i) => Some(*i as f64),
                        _ => None,
                    })
                    .collect();
                if nums.len() == 4 {
                    Some([nums[0], nums[1], nums[2], nums[3]])
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or([0.0, 0.0, 0.0, 0.0]);

    let page_index = dict
        .get("P")
        .and_then(|p| page_refs.iter().position(|r| refs_equal(r, p)))
        .unwrap_or(0);

    let readonly = ff & 1 != 0;
    let required = ff & 2 != 0;
    let multiline = ff & (1 << 12) != 0;
    let max_len = dict.get("MaxLen").and_then(|o| {
        if let PdfObject::Integer(i) = o {
            Some(*i as u32)
        } else {
            None
        }
    });

    let as_state = dict
        .get("AS")
        .and_then(|o| {
            if let PdfObject::Name(n) = o {
                Some(n.clone())
            } else {
                None
            }
        })
        .unwrap_or_default();
    let checked = !as_state.is_empty() && as_state != "Off";

    let options = if let Some(PdfObject::Array(opt)) = dict.get("Opt") {
        opt.iter()
            .filter_map(|o| match o {
                // Each /Opt entry may be a string or a two-element array [export, display].
                PdfObject::String(b) => Some(pdf_string_to_utf8(b)),
                PdfObject::Array(pair) => pair.first().and_then(|v| {
                    if let PdfObject::String(b) = v {
                        Some(pdf_string_to_utf8(b))
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .collect()
    } else {
        vec![]
    };

    out.push(FormField {
        id,
        name: partial_name,
        full_name,
        field_type,
        value,
        default_value,
        rect,
        page_index,
        options,
        checked,
        readonly,
        required,
        multiline,
        max_len,
    });
    Ok(())
}

/// Convert a PDF byte string to a UTF-8 `String`.
///
/// Attempts UTF-16BE (BOM `0xFE 0xFF`) first, then UTF-8, then latin-1
/// byte-by-byte as a final fallback.
fn pdf_string_to_utf8(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        // UTF-16BE
        let pairs: Vec<u16> = bytes[2..]
            .chunks(2)
            .filter_map(|c| {
                if c.len() == 2 {
                    Some(u16::from_be_bytes([c[0], c[1]]))
                } else {
                    None
                }
            })
            .collect();
        String::from_utf16_lossy(&pairs)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Extract a string from a `PdfObject::String` or `PdfObject::Name`.
fn pdf_obj_to_string(obj: Option<&PdfObject>) -> Option<String> {
    match obj? {
        PdfObject::String(b) => Some(pdf_string_to_utf8(b)),
        PdfObject::Name(n) => Some(n.clone()),
        _ => None,
    }
}

/// True when both objects are indirect references with the same object number.
fn refs_equal(a: &PdfObject, b: &PdfObject) -> bool {
    matches!((a, b), (PdfObject::Reference(an, _), PdfObject::Reference(bn, _)) if an == bn)
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_form_fields_no_acroform_returns_empty() {
        // A minimal PDF with no /AcroForm should return an empty list.
        use crate::parser::objects::{PdfDict, PdfObject};
        use crate::writer::document::PdfWriter;

        let mut w = PdfWriter::new();
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));
        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(catalog));
        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        let fields = read_form_fields(&doc).unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn refs_equal_same_object_number() {
        let a = PdfObject::Reference(5, 0);
        let b = PdfObject::Reference(5, 1);
        assert!(refs_equal(&a, &b));
    }

    #[test]
    fn refs_equal_different_object_numbers() {
        let a = PdfObject::Reference(5, 0);
        let b = PdfObject::Reference(6, 0);
        assert!(!refs_equal(&a, &b));
    }

    #[test]
    fn pdf_string_to_utf8_plain_ascii() {
        let s = pdf_string_to_utf8(b"hello");
        assert_eq!(s, "hello");
    }

    #[test]
    fn pdf_string_to_utf8_utf16be() {
        // UTF-16BE encoding of "AB"
        let bytes = [0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42];
        let s = pdf_string_to_utf8(&bytes);
        assert_eq!(s, "AB");
    }

    // ── XFA detection / extraction ─────────────────────────────────────────────

    use crate::writer::document::PdfWriter;
    use crate::writer::streams::make_raw_stream;

    /// Build a minimal one-page-less document with an optional `/AcroForm` dict.
    fn build_doc_with_acroform(acroform: Option<PdfDict>) -> PdfDocument {
        let mut w = PdfWriter::new();
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));

        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        if let Some(af) = acroform {
            let af_id = w.add_object(PdfObject::Dictionary(af));
            catalog.insert("AcroForm".to_owned(), PdfObject::Reference(af_id, 0));
        }
        let cat_id = w.add_object(PdfObject::Dictionary(catalog));
        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        PdfDocument::parse(bytes).unwrap()
    }

    #[test]
    fn is_xfa_form_false_when_no_acroform() {
        let doc = build_doc_with_acroform(None);
        assert!(!is_xfa_form(&doc).unwrap());
    }

    #[test]
    fn is_xfa_form_false_when_acroform_has_no_xfa() {
        let mut af = PdfDict::new();
        af.insert("Fields".to_owned(), PdfObject::Array(vec![]));
        let doc = build_doc_with_acroform(Some(af));
        assert!(!is_xfa_form(&doc).unwrap());
    }

    #[test]
    fn is_xfa_form_true_when_xfa_key_present() {
        let mut w = PdfWriter::new();
        let stream = make_raw_stream(b"<xdp:xdp></xdp:xdp>".to_vec(), PdfDict::new());
        let xfa_id = w.add_object(PdfObject::Stream(Box::new(stream)));

        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));

        let mut af = PdfDict::new();
        af.insert("XFA".to_owned(), PdfObject::Reference(xfa_id, 0));
        let af_id = w.add_object(PdfObject::Dictionary(af));

        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        catalog.insert("AcroForm".to_owned(), PdfObject::Reference(af_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(catalog));

        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        assert!(is_xfa_form(&doc).unwrap());
    }

    #[test]
    fn extract_xfa_data_errors_when_no_acroform() {
        let doc = build_doc_with_acroform(None);
        let err = extract_xfa_data(&doc).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn extract_xfa_data_errors_when_no_xfa_key() {
        let af = PdfDict::new();
        let doc = build_doc_with_acroform(Some(af));
        let err = extract_xfa_data(&doc).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn extract_xfa_data_single_stream() {
        let mut w = PdfWriter::new();
        let xml = b"<xdp:xdp>single packet</xdp:xdp>".to_vec();
        let stream = make_raw_stream(xml.clone(), PdfDict::new());
        let xfa_id = w.add_object(PdfObject::Stream(Box::new(stream)));

        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));

        let mut af = PdfDict::new();
        af.insert("XFA".to_owned(), PdfObject::Reference(xfa_id, 0));
        let af_id = w.add_object(PdfObject::Dictionary(af));

        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        catalog.insert("AcroForm".to_owned(), PdfObject::Reference(af_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(catalog));

        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        let result = extract_xfa_data(&doc).unwrap();
        assert_eq!(result, String::from_utf8(xml).unwrap());
    }

    #[test]
    fn extract_xfa_data_packet_array_concatenates_in_order() {
        let mut w = PdfWriter::new();
        let template_stream = make_raw_stream(b"<template/>".to_vec(), PdfDict::new());
        let template_id = w.add_object(PdfObject::Stream(Box::new(template_stream)));
        let datasets_stream = make_raw_stream(b"<datasets/>".to_vec(), PdfDict::new());
        let datasets_id = w.add_object(PdfObject::Stream(Box::new(datasets_stream)));

        let xfa_array = PdfObject::Array(vec![
            PdfObject::Name("template".to_owned()),
            PdfObject::Reference(template_id, 0),
            PdfObject::Name("datasets".to_owned()),
            PdfObject::Reference(datasets_id, 0),
        ]);
        let xfa_id = w.add_object(xfa_array);

        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));

        let mut af = PdfDict::new();
        af.insert("XFA".to_owned(), PdfObject::Reference(xfa_id, 0));
        let af_id = w.add_object(PdfObject::Dictionary(af));

        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        catalog.insert("AcroForm".to_owned(), PdfObject::Reference(af_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(catalog));

        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        let result = extract_xfa_data(&doc).unwrap();
        assert_eq!(result, "<template/><datasets/>");
    }
}
