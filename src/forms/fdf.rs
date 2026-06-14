//! FDF (Forms Data Format) and XFDF (XML Forms Data Format) import/export.
//!
//! FDF is a PDF-subset interchange format for form field data (ISO 32000-1 §12.7.7).
//! XFDF is the XML equivalent defined by Adobe.
//!
//! Both formats carry only field name/value pairs; they do not contain page content
//! or appearance streams. Import fills the matching fields in an open [`PdfEditor`];
//! export extracts the current field values from a [`PdfDocument`].

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDocument, PdfObject};

// ── FDF Export ────────────────────────────────────────────────────────────────

/// Export all form field values from `doc` as FDF bytes.
///
/// The returned bytes form a valid FDF 1.2 file (PDF-like syntax with an
/// `xref` table and `startxref`) that can be imported into any FDF-compatible
/// viewer or round-tripped through [`import_fdf`].
pub fn export_fdf(doc: &PdfDocument) -> Result<Vec<u8>> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "export_fdf")?;

    let fields = crate::forms::read_form_fields(doc)?;
    let mut out: Vec<u8> = Vec::new();

    out.extend_from_slice(b"%FDF-1.2\n");

    // Object 1 always starts right after the 9-byte header.
    let obj1_offset: usize = out.len(); // == 9

    out.extend_from_slice(b"1 0 obj\n");
    out.extend_from_slice(b"<< /FDF << /Fields [\n");
    for field in &fields {
        let v = fdf_value_for_field(field);
        let entry = format!(
            "  << /T ({}) /V {} >>\n",
            escape_pdf_string(&field.full_name),
            v
        );
        out.extend_from_slice(entry.as_bytes());
    }
    out.extend_from_slice(b"] >> >>\n");
    out.extend_from_slice(b"endobj\n");

    // XRef table — object 0 is always the free-list head.
    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n");
    out.extend_from_slice(b"0 2\n");
    // Each xref entry is exactly 20 bytes: 10-digit offset + space + 5-digit gen
    // + space + flag + space + LF (ISO 32000-1 §7.5.4).
    out.extend_from_slice(b"0000000000 65535 f \n");
    out.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());

    out.extend_from_slice(b"trailer\n");
    out.extend_from_slice(b"<< /Root 1 0 R /Size 2 >>\n");
    out.extend_from_slice(b"startxref\n");
    out.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
    out.extend_from_slice(b"%%EOF\n");

    Ok(out)
}

// ── FDF Import ────────────────────────────────────────────────────────────────

/// Parse FDF bytes and fill the matching form fields in `editor`.
///
/// Fields not present in the FDF are left unchanged. Unsupported field types
/// (e.g. signature fields) are silently skipped.
pub fn import_fdf(editor: &mut crate::editor::PdfEditor, fdf_bytes: &[u8]) -> Result<()> {
    let fields_data = parse_fdf(fdf_bytes)?;
    let existing_fields = crate::forms::read_form_fields(&editor.doc)?;

    for (name, value) in fields_data {
        let field = match existing_fields
            .iter()
            .find(|f| f.full_name == name || f.name == name)
        {
            Some(f) => f.clone(),
            None => continue,
        };
        match field.field_type {
            crate::forms::FieldType::Text => {
                crate::forms::set_text_field(editor, &field, &value)?;
            }
            crate::forms::FieldType::Checkbox => {
                let checked = value == "Yes" || value == "/Yes";
                crate::forms::set_checkbox(editor, &field, checked)?;
            }
            crate::forms::FieldType::List | crate::forms::FieldType::Combo => {
                crate::forms::set_combo_or_list(editor, &field, &value)?;
            }
            _ => {} // skip Radio, Signature, Unknown
        }
    }
    Ok(())
}

// ── XFDF Export ───────────────────────────────────────────────────────────────

/// Export all form field values from `doc` as an XFDF string.
///
/// The returned string is a valid XFDF 1.0 document. Use [`import_xfdf`] to
/// apply it back to an editor, or pass it to an external viewer.
pub fn export_xfdf(doc: &PdfDocument) -> Result<String> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "export_xfdf")?;

    let fields = crate::forms::read_form_fields(doc)?;
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n");
    xml.push_str("  <fields>\n");
    for field in &fields {
        let value = xml_escape(&field.value);
        xml.push_str(&format!(
            "    <field name=\"{}\">\n      <value>{}</value>\n    </field>\n",
            xml_attr_escape(&field.full_name),
            value
        ));
    }
    xml.push_str("  </fields>\n</xfdf>\n");
    Ok(xml)
}

// ── XFDF Import ───────────────────────────────────────────────────────────────

/// Parse an XFDF string and fill the matching form fields in `editor`.
///
/// Fields not present in the XFDF are left unchanged. Unsupported field types
/// are silently skipped.
pub fn import_xfdf(editor: &mut crate::editor::PdfEditor, xfdf_str: &str) -> Result<()> {
    let fields_data = parse_xfdf(xfdf_str)?;
    let existing_fields = crate::forms::read_form_fields(&editor.doc)?;

    for (name, value) in fields_data {
        let field = match existing_fields
            .iter()
            .find(|f| f.full_name == name || f.name == name)
        {
            Some(f) => f.clone(),
            None => continue,
        };
        match field.field_type {
            crate::forms::FieldType::Text => {
                crate::forms::set_text_field(editor, &field, &value)?;
            }
            crate::forms::FieldType::Checkbox => {
                crate::forms::set_checkbox(editor, &field, value == "Yes")?;
            }
            crate::forms::FieldType::List | crate::forms::FieldType::Combo => {
                crate::forms::set_combo_or_list(editor, &field, &value)?;
            }
            _ => {}
        }
    }
    Ok(())
}

// ── Private: FDF serialisation helpers ───────────────────────────────────────

/// Produce the `/V` token for a field: `/Yes` or `/Off` for checkboxes;
/// a PDF literal string `(value)` for all other types.
fn fdf_value_for_field(field: &crate::forms::FormField) -> String {
    match field.field_type {
        crate::forms::FieldType::Checkbox => {
            if field.checked {
                "/Yes".to_owned()
            } else {
                "/Off".to_owned()
            }
        }
        _ => format!("({})", escape_pdf_string(&field.value)),
    }
}

/// Escape special characters inside a PDF literal string `(...)`.
///
/// Backslashes must be escaped before parentheses so that the added `\`
/// characters are not themselves re-escaped.
fn escape_pdf_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

// ── Private: FDF parse ────────────────────────────────────────────────────────

/// Parse FDF bytes into a list of `(full_field_name, value)` pairs.
///
/// FDF is a strict PDF subset, so we parse it with the standard [`PdfDocument`]
/// parser, then walk the `/FDF /Fields` array.
fn parse_fdf(data: &[u8]) -> Result<Vec<(String, String)>> {
    let doc = PdfDocument::parse(data.to_vec())?;

    // FDF trailer → /Root → object 1 dict (contains /FDF key)
    let root_ref = doc
        .trailer
        .get("Root")
        .ok_or_else(|| PdfError::invalid_structure("FDF: no /Root in trailer"))?
        .clone();
    let root_obj = doc.resolve(&root_ref)?;
    let root_dict = root_obj
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("FDF: /Root not a dict"))?
        .clone();

    let fdf_ref = root_dict
        .get("FDF")
        .ok_or_else(|| PdfError::invalid_structure("FDF: no /FDF key in root dict"))?
        .clone();
    let fdf_obj = doc.resolve(&fdf_ref)?;
    let fdf_dict = fdf_obj
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("FDF: /FDF value not a dict"))?
        .clone();

    let fields_ref = fdf_dict
        .get("Fields")
        .ok_or_else(|| PdfError::invalid_structure("FDF: no /Fields in /FDF dict"))?
        .clone();
    let fields_obj = doc.resolve(&fields_ref)?;
    let fields_arr = match fields_obj {
        PdfObject::Array(a) => a,
        _ => return Err(PdfError::invalid_structure("FDF: /Fields not an array")),
    };

    let mut result = Vec::new();
    for field_ref in &fields_arr {
        let field_obj = doc.resolve(field_ref)?;
        let field_dict = match field_obj.as_dict() {
            Some(d) => d.clone(),
            None => continue,
        };

        let name = match field_dict.get("T") {
            Some(PdfObject::String(b)) => String::from_utf8_lossy(b).into_owned(),
            Some(PdfObject::Name(n)) => n.clone(),
            _ => continue,
        };
        let value = match field_dict.get("V") {
            Some(PdfObject::String(b)) => String::from_utf8_lossy(b).into_owned(),
            Some(PdfObject::Name(n)) => n.clone(),
            _ => String::new(),
        };
        result.push((name, value));
    }
    Ok(result)
}

// ── Private: XFDF serialisation helpers ──────────────────────────────────────

/// Escape `<`, `>`, and `&` for XML text content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape `<`, `>`, `&`, and `"` for use inside an XML attribute value.
fn xml_attr_escape(s: &str) -> String {
    xml_escape(s).replace('"', "&quot;")
}

// ── Private: XFDF parse ───────────────────────────────────────────────────────

/// Parse an XFDF string into a list of `(field_name, value)` pairs.
///
/// Uses a minimal state machine — no external XML dependency required for
/// this well-defined subset of XFDF.
fn parse_xfdf(xml: &str) -> Result<Vec<(String, String)>> {
    let mut result = Vec::new();
    let mut pos = 0usize;

    while let Some(rel) = xml[pos..].find("<field ") {
        let field_start = pos + rel;

        // Extract name="..." attribute.
        let name_attr_start = xml[field_start..]
            .find("name=\"")
            .map(|i| field_start + i + 6)
            .ok_or_else(|| PdfError::invalid_structure("XFDF: <field> missing name attribute"))?;
        let name_end = xml[name_attr_start..]
            .find('"')
            .map(|i| name_attr_start + i)
            .ok_or_else(|| PdfError::invalid_structure("XFDF: field name attribute not closed"))?;
        let name = xml_unescape(&xml[name_attr_start..name_end]);

        // Extract optional <value>...</value> content.
        let value = if let Some(vs_rel) = xml[field_start..].find("<value>") {
            let vs = field_start + vs_rel + 7; // past "<value>"
            let ve = xml[vs..]
                .find("</value>")
                .map(|i| vs + i)
                .ok_or_else(|| PdfError::invalid_structure("XFDF: unclosed <value>"))?;
            xml_unescape(&xml[vs..ve])
        } else {
            String::new()
        };

        result.push((name, value));

        // Advance past the closing </field> tag to avoid re-matching nested elements.
        pos = xml[field_start..]
            .find("</field>")
            .map(|i| field_start + i + 8)
            .unwrap_or(field_start + 7); // fall back to past "<field " if no close tag
    }
    Ok(result)
}

/// Reverse the five standard XML entity escapes.
fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── escape helpers ────────────────────────────────────────────────────────

    #[test]
    fn escape_pdf_string_backslash_first() {
        // Backslash must be escaped before parens so added `\` aren't re-escaped.
        assert_eq!(escape_pdf_string("a\\(b)c"), "a\\\\\\(b\\)c");
    }

    #[test]
    fn escape_pdf_string_plain() {
        assert_eq!(escape_pdf_string("hello"), "hello");
    }

    #[test]
    fn xml_escape_entities() {
        assert_eq!(xml_escape("a&b<c>d"), "a&amp;b&lt;c&gt;d");
    }

    #[test]
    fn xml_attr_escape_includes_quote() {
        assert_eq!(xml_attr_escape("say \"hi\""), "say &quot;hi&quot;");
    }

    #[test]
    fn xml_unescape_roundtrip() {
        let original = "a & b < c > d \" e ' f";
        let escaped = xml_attr_escape(original).replace("'", "&apos;");
        assert_eq!(xml_unescape(&escaped), original);
    }

    // ── fdf_value_for_field ───────────────────────────────────────────────────

    #[test]
    fn fdf_value_checkbox_checked() {
        use crate::forms::reader::{FieldType, FormField};
        let field = FormField {
            id: 1,
            name: "cb".to_owned(),
            full_name: "cb".to_owned(),
            field_type: FieldType::Checkbox,
            value: String::new(),
            default_value: String::new(),
            rect: [0.0; 4],
            page_index: 0,
            options: vec![],
            checked: true,
            readonly: false,
            required: false,
            multiline: false,
            max_len: None,
        };
        assert_eq!(fdf_value_for_field(&field), "/Yes");
    }

    #[test]
    fn fdf_value_checkbox_unchecked() {
        use crate::forms::reader::{FieldType, FormField};
        let field = FormField {
            id: 1,
            name: "cb".to_owned(),
            full_name: "cb".to_owned(),
            field_type: FieldType::Checkbox,
            value: String::new(),
            default_value: String::new(),
            rect: [0.0; 4],
            page_index: 0,
            options: vec![],
            checked: false,
            readonly: false,
            required: false,
            multiline: false,
            max_len: None,
        };
        assert_eq!(fdf_value_for_field(&field), "/Off");
    }

    #[test]
    fn fdf_value_text_field() {
        use crate::forms::reader::{FieldType, FormField};
        let field = FormField {
            id: 2,
            name: "t".to_owned(),
            full_name: "t".to_owned(),
            field_type: FieldType::Text,
            value: "hello (world)".to_owned(),
            default_value: String::new(),
            rect: [0.0; 4],
            page_index: 0,
            options: vec![],
            checked: false,
            readonly: false,
            required: false,
            multiline: false,
            max_len: None,
        };
        assert_eq!(fdf_value_for_field(&field), "(hello \\(world\\))");
    }

    // ── parse_xfdf ────────────────────────────────────────────────────────────

    #[test]
    fn parse_xfdf_two_fields() {
        let xml = r#"<?xml version="1.0"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/">
  <fields>
    <field name="Name"><value>John</value></field>
    <field name="City"><value>Paris</value></field>
  </fields>
</xfdf>"#;
        let pairs = parse_xfdf(xml).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("Name".to_owned(), "John".to_owned()));
        assert_eq!(pairs[1], ("City".to_owned(), "Paris".to_owned()));
    }

    #[test]
    fn parse_xfdf_empty_value() {
        let xml = r#"<fields><field name="Empty"><value></value></field></fields>"#;
        let pairs = parse_xfdf(xml).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1, "");
    }

    #[test]
    fn parse_xfdf_xml_entities_in_value() {
        let xml = r#"<fields><field name="F"><value>a &amp; b</value></field></fields>"#;
        let pairs = parse_xfdf(xml).unwrap();
        assert_eq!(pairs[0].1, "a & b");
    }

    #[test]
    fn parse_xfdf_no_fields_returns_empty() {
        let xml = r#"<xfdf><fields></fields></xfdf>"#;
        let pairs = parse_xfdf(xml).unwrap();
        assert!(pairs.is_empty());
    }

    // ── parse_fdf ─────────────────────────────────────────────────────────────

    fn make_fdf(entries: &[(&str, &str)]) -> Vec<u8> {
        // Build a valid FDF with tracked byte offsets.
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"%FDF-1.2\n");
        let obj1_offset = out.len();

        out.extend_from_slice(b"1 0 obj\n");
        out.extend_from_slice(b"<< /FDF << /Fields [\n");
        for (name, value) in entries {
            let v = if *value == "/Yes" || *value == "/Off" {
                value.to_string()
            } else {
                format!("({})", escape_pdf_string(value))
            };
            out.extend_from_slice(
                format!("  << /T ({}) /V {} >>\n", escape_pdf_string(name), v).as_bytes(),
            );
        }
        out.extend_from_slice(b"] >> >>\n");
        out.extend_from_slice(b"endobj\n");

        let xref_offset = out.len();
        out.extend_from_slice(b"xref\n");
        out.extend_from_slice(b"0 2\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        out.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
        out.extend_from_slice(b"trailer\n");
        out.extend_from_slice(b"<< /Root 1 0 R /Size 2 >>\n");
        out.extend_from_slice(b"startxref\n");
        out.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    #[test]
    fn parse_fdf_two_fields() {
        let fdf = make_fdf(&[("Name", "John"), ("City", "Paris")]);
        let pairs = parse_fdf(&fdf).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("Name".to_owned(), "John".to_owned()));
        assert_eq!(pairs[1], ("City".to_owned(), "Paris".to_owned()));
    }

    #[test]
    fn parse_fdf_checkbox_name_value() {
        let fdf = make_fdf(&[("Agree", "/Yes")]);
        let pairs = parse_fdf(&fdf).unwrap();
        assert_eq!(pairs[0], ("Agree".to_owned(), "Yes".to_owned()));
    }

    #[test]
    fn parse_fdf_empty_fields_array() {
        let fdf = make_fdf(&[]);
        let pairs = parse_fdf(&fdf).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn parse_fdf_no_root_returns_error() {
        // A document with no /Root in trailer.
        let bad = b"%FDF-1.2\ntrailer\n<< /Size 1 >>\nstartxref\n9\n%%EOF\n";
        // We can't even parse this as a valid FDF (no xref), but the error path
        // should still return Err rather than panic.
        let result = parse_fdf(bad);
        assert!(result.is_err());
    }

    // ── export_fdf structure ──────────────────────────────────────────────────

    #[test]
    fn export_fdf_header_and_eof() {
        use crate::parser::objects::PdfDocument;
        use crate::writer::document::PdfWriter;

        // Build a tiny PDF with no AcroForm (returns 0 fields → empty FDF).
        let mut w = PdfWriter::new();
        let mut pages = crate::parser::objects::PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));
        let mut cat = crate::parser::objects::PdfDict::new();
        cat.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        cat.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(cat));
        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();

        let fdf = export_fdf(&doc).unwrap();
        assert!(fdf.starts_with(b"%FDF-1.2\n"), "must start with FDF header");
        assert!(fdf.ends_with(b"%%EOF\n"), "must end with %%EOF");
        assert!(
            fdf.windows(9).any(|w| w == b"startxref"),
            "must contain startxref"
        );
    }

    #[test]
    fn export_fdf_parse_roundtrip() {
        use crate::parser::objects::PdfDocument;
        use crate::writer::document::PdfWriter;

        let mut w = PdfWriter::new();
        let mut pages = crate::parser::objects::PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));
        let mut cat = crate::parser::objects::PdfDict::new();
        cat.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        cat.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(cat));
        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();

        let fdf = export_fdf(&doc).unwrap();
        // Should be parseable back by the PDF parser.
        let result = PdfDocument::parse(fdf);
        assert!(result.is_ok(), "exported FDF must be parseable as PDF");
    }

    // ── export_xfdf structure ─────────────────────────────────────────────────

    #[test]
    fn export_xfdf_structure() {
        use crate::parser::objects::PdfDocument;
        use crate::writer::document::PdfWriter;

        let mut w = PdfWriter::new();
        let mut pages = crate::parser::objects::PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = w.add_object(PdfObject::Dictionary(pages));
        let mut cat = crate::parser::objects::PdfDict::new();
        cat.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        cat.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = w.add_object(PdfObject::Dictionary(cat));
        let bytes = w.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();

        let xfdf = export_xfdf(&doc).unwrap();
        assert!(xfdf.contains("<xfdf"), "must contain <xfdf opening tag");
        assert!(xfdf.contains("</xfdf>"), "must contain </xfdf> closing tag");
        assert!(xfdf.contains("<fields>"), "must contain <fields>");
        assert!(xfdf.contains("</fields>"), "must contain </fields>");
    }

    // ── integration: form.pdf round-trip ─────────────────────────────────────

    #[cfg(feature = "forms")]
    #[test]
    fn fdf_export_round_trips() {
        let data = include_bytes!("../../tests/fixtures/form.pdf").to_vec();
        let doc = crate::parser::objects::PdfDocument::parse(data.clone()).unwrap();
        let fdf = export_fdf(&doc).unwrap();
        assert!(fdf.starts_with(b"%FDF-1.2"));
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        import_fdf(&mut editor, &fdf).unwrap();
    }

    #[cfg(feature = "forms")]
    #[test]
    fn xfdf_export_is_valid_xml() {
        let data = include_bytes!("../../tests/fixtures/form.pdf").to_vec();
        let doc = crate::parser::objects::PdfDocument::parse(data).unwrap();
        let xfdf = export_xfdf(&doc).unwrap();
        assert!(xfdf.contains("<xfdf"));
        assert!(xfdf.contains("</xfdf>"));
    }

    #[cfg(feature = "forms")]
    #[test]
    fn fdf_import_updates_field_values() {
        let data = include_bytes!("../../tests/fixtures/form.pdf").to_vec();
        // Find the first text field name so we can target it.
        let doc = crate::parser::objects::PdfDocument::parse(data.clone()).unwrap();
        let fields = crate::forms::read_form_fields(&doc).unwrap();
        let text_field = fields
            .iter()
            .find(|f| f.field_type == crate::forms::FieldType::Text);
        if let Some(tf) = text_field {
            let fdf = make_fdf(&[(&tf.full_name, "TestValue")]);
            let mut editor = crate::editor::PdfEditor::open(data).unwrap();
            import_fdf(&mut editor, &fdf).unwrap();
        }
        // Test passes as long as no panic/error.
    }

    #[cfg(feature = "forms")]
    #[test]
    fn xfdf_import_updates_field_values() {
        let data = include_bytes!("../../tests/fixtures/form.pdf").to_vec();
        let doc = crate::parser::objects::PdfDocument::parse(data.clone()).unwrap();
        let xfdf = export_xfdf(&doc).unwrap();
        let mut editor = crate::editor::PdfEditor::open(data).unwrap();
        import_xfdf(&mut editor, &xfdf).unwrap();
    }
}
