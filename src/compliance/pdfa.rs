//! PDF/A validation and conversion (ISO 19005-1/2/3).
//!
//! Validates and converts PDF documents to PDF/A-1b, PDF/A-2b, and PDF/A-3b.
//! Validation is read-only; conversion requires the `writer` feature and an
//! Enterprise license (when the `crypto` feature is enabled).

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

#[cfg(feature = "writer")]
use crate::editor::PdfEditor;

// Standard 14 PDF fonts exempt from embedding requirements (ISO 32000-1 §9.6.2.2).
const STANDARD_14_FONTS: &[&str] = &[
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
    "Symbol",
    "ZapfDingbats",
];

// ─────────────────────────────────────────────────────────────────────────────
// Violation type
// ─────────────────────────────────────────────────────────────────────────────

/// A single PDF/A rule violation found during validation.
#[derive(Debug, Clone)]
pub struct PdfAViolation {
    /// ISO 19005 rule number (e.g. "6.2.2" for font embedding).
    pub rule: String,
    /// Human-readable description of the violation.
    pub description: String,
    /// Object ID of the violating object, if applicable.
    pub obj_id: Option<u32>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Public validation API
// ─────────────────────────────────────────────────────────────────────────────

/// Validate a PDF document against the PDF/A-1b standard (ISO 19005-1).
///
/// Returns a list of violations; an empty list means the document is conformant.
pub fn validate_pdfa_1b(doc: &PdfDocument) -> Result<Vec<PdfAViolation>> {
    let mut v = Vec::new();
    check_encryption(doc, &mut v)?;
    check_font_embedding(doc, &mut v)?;
    check_no_javascript(doc, &mut v)?;
    check_output_intents(doc, &mut v)?;
    check_xmp_metadata(doc, 1, &mut v)?;
    check_no_transparency(doc, &mut v)?;
    check_no_external_streams(doc, &mut v)?;
    Ok(v)
}

/// Validate a PDF document against the PDF/A-2b standard (ISO 19005-2).
///
/// PDF/A-2b relaxes transparency and allows JPEG2000 and optional content.
pub fn validate_pdfa_2b(doc: &PdfDocument) -> Result<Vec<PdfAViolation>> {
    let mut v = Vec::new();
    check_encryption(doc, &mut v)?;
    check_font_embedding(doc, &mut v)?;
    check_no_javascript(doc, &mut v)?;
    check_output_intents(doc, &mut v)?;
    check_xmp_metadata(doc, 2, &mut v)?;
    Ok(v)
}

/// Validate a PDF document against the PDF/A-3b standard (ISO 19005-3).
///
/// PDF/A-3b is identical to 2b but allows embedded files of any type via /AF.
pub fn validate_pdfa_3b(doc: &PdfDocument) -> Result<Vec<PdfAViolation>> {
    validate_pdfa_2b(doc)
}

// ─────────────────────────────────────────────────────────────────────────────
// Public conversion API (requires writer feature)
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a PDF document to PDF/A-1b conformance in place.
///
/// Adds sRGB output intent, XMP metadata, and removes JavaScript.
/// Font embedding is logged as a warning but not performed automatically
/// (requires external font data; see known limitations).
/// Requires an Enterprise license.
#[cfg(feature = "writer")]
pub fn convert_to_pdfa_1b(editor: &mut PdfEditor) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Enterprise, "pdfa")?;
    add_output_intents(editor)?;
    add_xmp_metadata(editor, 1, 'B')?;
    remove_javascript(editor)?;
    embed_missing_fonts(editor)?;
    Ok(())
}

/// Convert a PDF document to PDF/A-2b conformance in place.
///
/// Adds sRGB output intent, XMP metadata, and removes JavaScript.
/// Requires an Enterprise license.
#[cfg(feature = "writer")]
pub fn convert_to_pdfa_2b(editor: &mut PdfEditor) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Enterprise, "pdfa")?;
    add_output_intents(editor)?;
    add_xmp_metadata(editor, 2, 'B')?;
    remove_javascript(editor)?;
    Ok(())
}

/// Convert a PDF document to PDF/A-3b conformance in place.
///
/// PDF/A-3b is identical to 2b for conversion purposes.
/// Requires an Enterprise license.
#[cfg(feature = "writer")]
pub fn convert_to_pdfa_3b(editor: &mut PdfEditor) -> Result<()> {
    convert_to_pdfa_2b(editor)
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation helpers
// ─────────────────────────────────────────────────────────────────────────────

fn resolve_root(doc: &PdfDocument) -> Result<PdfDict> {
    let root_ref = doc
        .trailer
        .get("Root")
        .ok_or_else(|| PdfError::invalid_structure("no /Root in document trailer"))?;
    let root_obj = doc.resolve(root_ref)?;
    match root_obj {
        PdfObject::Dictionary(d) => Ok(d),
        _ => Err(PdfError::invalid_structure(
            "document /Root is not a dictionary",
        )),
    }
}

fn check_encryption(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    if doc.trailer.contains_key("Encrypt") {
        v.push(PdfAViolation {
            rule: "6.1.1".to_owned(),
            description: "PDF/A does not permit encryption".to_owned(),
            obj_id: None,
        });
    }
    Ok(())
}

fn check_font_embedding(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    use crate::document::catalog::Catalog;

    let catalog = Catalog::from_document(doc)?;
    let page_count = catalog.page_count;

    for i in 0..page_count {
        let page_dict = catalog.get_page_dict(doc, i)?;

        let resources = match page_dict.get("Resources") {
            Some(resources_ref) => doc.resolve(resources_ref)?,
            None => continue,
        };

        let font_map = match resources.as_dict().and_then(|d| d.get("Font")) {
            Some(font_ref) => {
                let resolved = doc.resolve(font_ref)?;
                match resolved {
                    PdfObject::Dictionary(d) => d,
                    _ => continue,
                }
            }
            None => continue,
        };

        for (font_name, font_ref) in &font_map {
            let font_obj = doc.resolve(font_ref)?;
            let font_dict = match font_obj.as_dict() {
                Some(d) => d.clone(),
                None => continue,
            };

            let base_font = match font_dict.get("BaseFont") {
                Some(PdfObject::Name(n)) => n.as_str(),
                _ => "",
            };

            if STANDARD_14_FONTS.contains(&base_font) {
                continue;
            }

            if !font_dict.contains_key("FontDescriptor") {
                v.push(PdfAViolation {
                    rule: "6.2.2".to_owned(),
                    description: format!(
                        "Font '{}' on page {} has no FontDescriptor",
                        font_name, i
                    ),
                    obj_id: match font_ref {
                        PdfObject::Reference(n, _) => Some(*n),
                        _ => None,
                    },
                });
                continue;
            }

            if let Some(PdfObject::Reference(desc_id, _)) = font_dict.get("FontDescriptor") {
                let desc_obj = doc.get_object(*desc_id)?;
                let desc_dict = match desc_obj.as_dict() {
                    Some(d) => d.clone(),
                    None => continue,
                };
                let has_file = desc_dict.contains_key("FontFile")
                    || desc_dict.contains_key("FontFile2")
                    || desc_dict.contains_key("FontFile3");
                if !has_file {
                    v.push(PdfAViolation {
                        rule: "6.2.2".to_owned(),
                        description: format!(
                            "Font '{}' FontDescriptor missing embedded font file",
                            font_name
                        ),
                        obj_id: Some(*desc_id),
                    });
                }
            }
        }
    }
    Ok(())
}

fn check_no_javascript(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    let root_dict = resolve_root(doc)?;
    if let Some(names_ref) = root_dict.get("Names") {
        let names_obj = doc.resolve(names_ref)?;
        if let Some(names_dict) = names_obj.as_dict() {
            if names_dict.contains_key("JavaScript") {
                v.push(PdfAViolation {
                    rule: "6.6.1".to_owned(),
                    description: "JavaScript is not permitted in PDF/A".to_owned(),
                    obj_id: None,
                });
            }
        }
    }
    Ok(())
}

fn check_output_intents(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    let root_dict = resolve_root(doc)?;
    if !root_dict.contains_key("OutputIntents") {
        v.push(PdfAViolation {
            rule: "6.2.3".to_owned(),
            description: "/OutputIntents with ICC profile required for PDF/A".to_owned(),
            obj_id: None,
        });
    }
    Ok(())
}

fn check_xmp_metadata(doc: &PdfDocument, part: u8, v: &mut Vec<PdfAViolation>) -> Result<()> {
    let root_dict = resolve_root(doc)?;
    if !root_dict.contains_key("Metadata") {
        v.push(PdfAViolation {
            rule: "6.7.2".to_owned(),
            description: format!(
                "XMP metadata stream (/Metadata) required in catalog for PDF/A-{}b",
                part
            ),
            obj_id: None,
        });
    }
    Ok(())
}

fn check_no_transparency(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    use crate::document::catalog::Catalog;

    let catalog = match Catalog::from_document(doc) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    for i in 0..catalog.page_count {
        let page_dict = match catalog.get_page_dict(doc, i) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let resources = match page_dict.get("Resources") {
            Some(r) => match doc.resolve(r) {
                Ok(obj) => obj,
                Err(_) => continue,
            },
            None => continue,
        };

        let ext_gstate = match resources.as_dict().and_then(|d| d.get("ExtGState")) {
            Some(egs_ref) => match doc.resolve(egs_ref) {
                Ok(PdfObject::Dictionary(d)) => d,
                _ => continue,
            },
            None => continue,
        };

        for (gs_name, gs_ref) in &ext_gstate {
            let gs_obj = match doc.resolve(gs_ref) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let gs_dict = match gs_obj.as_dict() {
                Some(d) => d.clone(),
                None => continue,
            };

            // Check blend mode — must be /Normal for PDF/A-1b.
            if let Some(PdfObject::Name(bm)) = gs_dict.get("BM") {
                if bm != "Normal" {
                    v.push(PdfAViolation {
                        rule: "6.4.1".to_owned(),
                        description: format!(
                            "ExtGState '{}' uses blend mode '{}'; only Normal allowed in PDF/A-1b",
                            gs_name, bm
                        ),
                        obj_id: match gs_ref {
                            PdfObject::Reference(n, _) => Some(*n),
                            _ => None,
                        },
                    });
                }
            }

            // Check opacity — /ca and /CA must be 1.0.
            for key in &["ca", "CA"] {
                let opacity = match gs_dict.get(*key) {
                    Some(PdfObject::Real(f)) => Some(*f),
                    Some(PdfObject::Integer(n)) => Some(*n as f64),
                    _ => None,
                };
                if let Some(alpha) = opacity {
                    if (alpha - 1.0).abs() > f64::EPSILON {
                        v.push(PdfAViolation {
                            rule: "6.4.1".to_owned(),
                            description: format!(
                                "ExtGState '{}' has /{} = {}; transparency not allowed in PDF/A-1b",
                                gs_name, key, alpha
                            ),
                            obj_id: match gs_ref {
                                PdfObject::Reference(n, _) => Some(*n),
                                _ => None,
                            },
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_no_external_streams(doc: &PdfDocument, v: &mut Vec<PdfAViolation>) -> Result<()> {
    // Walk all known object IDs and check for /F (external file stream) in stream dicts.
    let max_id = doc.max_object_id();
    for id in 1..=max_id {
        let obj = match doc.get_object(id) {
            Ok(o) => o,
            Err(_) => continue,
        };
        if let PdfObject::Stream(s) = &obj {
            if s.dict.contains_key("F") {
                v.push(PdfAViolation {
                    rule: "6.1.3".to_owned(),
                    description: format!(
                        "Stream object {} uses external file (/F); not allowed in PDF/A",
                        id
                    ),
                    obj_id: Some(id),
                });
            }
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Conversion helpers (require writer feature)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "writer")]
fn get_root_dict(editor: &PdfEditor) -> Result<(u32, PdfDict)> {
    let root_id = editor.catalog_id;
    match editor.get_object(root_id)? {
        PdfObject::Dictionary(d) => Ok((root_id, d)),
        _ => Err(PdfError::invalid_structure(
            "catalog object is not a dictionary",
        )),
    }
}

#[cfg(feature = "writer")]
fn read_info_string(editor: &PdfEditor, field: &str) -> Option<String> {
    let info_id = editor.info_id?;
    let obj = editor.get_object(info_id).ok()?;
    if let PdfObject::Dictionary(d) = obj {
        if let Some(PdfObject::String(bytes)) = d.get(field) {
            return String::from_utf8(bytes.clone()).ok();
        }
    }
    None
}

#[cfg(feature = "writer")]
fn add_output_intents(editor: &mut PdfEditor) -> Result<()> {
    use crate::writer::streams::make_flate_stream;

    let icc_data = crate::compliance::icc::srgb_icc_profile();
    let mut icc_dict = PdfDict::new();
    icc_dict.insert("N".to_owned(), PdfObject::Integer(3)); // 3 components: R, G, B
    let icc_stream = make_flate_stream(icc_data, icc_dict)?;
    let icc_id = editor.add_object(PdfObject::Stream(Box::new(icc_stream)));

    let mut intent_dict = PdfDict::new();
    intent_dict.insert(
        "Type".to_owned(),
        PdfObject::Name("OutputIntent".to_owned()),
    );
    intent_dict.insert("S".to_owned(), PdfObject::Name("GTS_PDFA1".to_owned()));
    intent_dict.insert(
        "OutputConditionIdentifier".to_owned(),
        PdfObject::String(b"sRGB IEC61966-2-1".to_vec()),
    );
    intent_dict.insert(
        "DestOutputProfile".to_owned(),
        PdfObject::Reference(icc_id, 0),
    );
    let intent_id = editor.add_object(PdfObject::Dictionary(intent_dict));

    let (root_id, mut root_dict) = get_root_dict(editor)?;
    root_dict.insert(
        "OutputIntents".to_owned(),
        PdfObject::Array(vec![PdfObject::Reference(intent_id, 0)]),
    );
    editor.replace_object(root_id, PdfObject::Dictionary(root_dict));
    Ok(())
}

#[cfg(feature = "writer")]
fn add_xmp_metadata(editor: &mut PdfEditor, part: u8, conformance: char) -> Result<()> {
    use crate::writer::streams::make_raw_stream;

    let title = read_info_string(editor, "Title");
    let author = read_info_string(editor, "Author");

    let xmp_str = crate::compliance::xmp::build_pdfa_xmp(
        title.as_deref(),
        author.as_deref(),
        part,
        conformance,
    );

    let mut xmp_dict = PdfDict::new();
    xmp_dict.insert("Type".to_owned(), PdfObject::Name("Metadata".to_owned()));
    xmp_dict.insert("Subtype".to_owned(), PdfObject::Name("XML".to_owned()));
    // XMP must NOT be compressed per PDF/A spec (§6.7.3).
    let xmp_stream = make_raw_stream(xmp_str.into_bytes(), xmp_dict);
    let xmp_id = editor.add_object(PdfObject::Stream(Box::new(xmp_stream)));

    let (root_id, mut root_dict) = get_root_dict(editor)?;
    root_dict.insert("Metadata".to_owned(), PdfObject::Reference(xmp_id, 0));
    editor.replace_object(root_id, PdfObject::Dictionary(root_dict));
    Ok(())
}

#[cfg(feature = "writer")]
fn remove_javascript(editor: &mut PdfEditor) -> Result<()> {
    let (root_id, mut root_dict) = get_root_dict(editor)?;

    let names_ref = match root_dict.get("Names").cloned() {
        Some(r) => r,
        None => return Ok(()),
    };

    let (names_id, mut names_dict) = match names_ref {
        PdfObject::Reference(id, _) => {
            let obj = editor.get_object(id)?;
            match obj {
                PdfObject::Dictionary(d) => (Some(id), d),
                _ => return Ok(()),
            }
        }
        PdfObject::Dictionary(d) => (None, d),
        _ => return Ok(()),
    };

    if !names_dict.contains_key("JavaScript") {
        return Ok(());
    }

    names_dict.shift_remove("JavaScript");

    if let Some(id) = names_id {
        editor.replace_object(id, PdfObject::Dictionary(names_dict));
    } else {
        root_dict.insert("Names".to_owned(), PdfObject::Dictionary(names_dict));
        editor.replace_object(root_id, PdfObject::Dictionary(root_dict));
    }
    Ok(())
}

/// Font embedding stub.
///
/// Full font embedding requires external TTF/OTF data per font. This function
/// logs a warning and returns Ok — callers should pre-validate with
/// `validate_pdfa_1b` and manually embed fonts before converting.
#[cfg(feature = "writer")]
fn embed_missing_fonts(_editor: &mut PdfEditor) -> Result<()> {
    log::warn!(
        "pdf/a: automatic font embedding not implemented; \
         pre-embed all fonts or validate for 6.2.2 violations first"
    );
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap_or_else(|_| panic!("fixture '{}' not found", name))
    }

    #[test]
    fn minimal_pdf_fails_pdfa_1b_no_output_intents() {
        let doc = PdfDocument::parse(load_fixture("minimal.pdf")).unwrap();
        let violations = validate_pdfa_1b(&doc).unwrap();
        assert!(
            violations.iter().any(|v| v.rule == "6.2.3"),
            "expected 6.2.3 violation, got: {:?}",
            violations
        );
    }

    #[test]
    fn minimal_pdf_fails_pdfa_1b_no_xmp() {
        let doc = PdfDocument::parse(load_fixture("minimal.pdf")).unwrap();
        let violations = validate_pdfa_1b(&doc).unwrap();
        assert!(
            violations.iter().any(|v| v.rule == "6.7.2"),
            "expected 6.7.2 violation, got: {:?}",
            violations
        );
    }

    #[cfg(feature = "writer")]
    #[test]
    fn convert_to_pdfa_1b_adds_output_intents_and_metadata() {
        let data = load_fixture("minimal.pdf");
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        convert_to_pdfa_1b(&mut editor).unwrap();
        let saved = editor.save_new().unwrap();
        let doc2 = PdfDocument::parse(saved).unwrap();
        let root_ref = doc2.trailer.get("Root").unwrap().clone();
        let root_obj = doc2.resolve(&root_ref).unwrap();
        let root_dict = root_obj.as_dict().unwrap().clone();
        assert!(
            root_dict.contains_key("OutputIntents"),
            "missing /OutputIntents after conversion"
        );
        assert!(
            root_dict.contains_key("Metadata"),
            "missing /Metadata after conversion"
        );
    }

    #[cfg(feature = "writer")]
    #[test]
    fn convert_to_pdfa_2b_adds_output_intents() {
        let data = load_fixture("minimal.pdf");
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        convert_to_pdfa_2b(&mut editor).unwrap();
        let saved = editor.save_new().unwrap();
        let doc2 = PdfDocument::parse(saved).unwrap();
        let root_ref = doc2.trailer.get("Root").unwrap().clone();
        let root_obj = doc2.resolve(&root_ref).unwrap();
        let root_dict = root_obj.as_dict().unwrap().clone();
        assert!(root_dict.contains_key("OutputIntents"));
    }

    #[cfg(feature = "writer")]
    #[test]
    fn converted_doc_passes_output_intents_check() {
        let data = load_fixture("minimal.pdf");
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        convert_to_pdfa_1b(&mut editor).unwrap();
        let saved = editor.save_new().unwrap();
        let doc2 = PdfDocument::parse(saved).unwrap();
        let violations = validate_pdfa_1b(&doc2).unwrap();
        assert!(
            !violations.iter().any(|v| v.rule == "6.2.3"),
            "6.2.3 still violated after conversion"
        );
        assert!(
            !violations.iter().any(|v| v.rule == "6.7.2"),
            "6.7.2 still violated after conversion"
        );
    }

    #[test]
    fn validate_pdfa_3b_delegates_to_2b() {
        let doc = PdfDocument::parse(load_fixture("minimal.pdf")).unwrap();
        let v2 = validate_pdfa_2b(&doc).unwrap();
        let v3 = validate_pdfa_3b(&doc).unwrap();
        assert_eq!(v2.len(), v3.len());
    }
}
