//! Font object writing — embed standard-14 and TrueType fonts into a document.

use crate::error::{PdfError, Result};
use crate::fonts::standard::StandardFont;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::document::PdfWriter;
use crate::writer::streams::make_raw_stream;

// ── Standard (Type1) fonts ────────────────────────────────────────────────────

/// Write a standard Type1 font dictionary (one of the 14 built-in fonts).
///
/// Returns the object ID of the font dictionary.
///
/// # Errors
///
/// Returns `InvalidStructure` if `name` is not one of the 14 standard PDF fonts.
pub fn write_standard_font(name: &str, writer: &mut PdfWriter) -> Result<u32> {
    if StandardFont::from_name(name).is_none() {
        return Err(PdfError::invalid_structure(format!(
            "'{name}' is not one of the 14 standard PDF fonts"
        )));
    }
    let mut dict = PdfDict::new();
    dict.insert("Type".to_owned(), PdfObject::Name("Font".to_owned()));
    dict.insert("Subtype".to_owned(), PdfObject::Name("Type1".to_owned()));
    dict.insert("BaseFont".to_owned(), PdfObject::Name(name.to_owned()));
    // WinAnsiEncoding is the most portable default for Latin scripts.
    dict.insert(
        "Encoding".to_owned(),
        PdfObject::Name("WinAnsiEncoding".to_owned()),
    );
    Ok(writer.add_object(PdfObject::Dictionary(dict)))
}

// ── TrueType fonts ────────────────────────────────────────────────────────────

/// Write an embedded TrueType font object.
///
/// Embeds the full TTF binary as `/FontFile2`. For simplicity, this covers the
/// Latin-1 range (first-char=32, last-char=255) with placeholder widths of 600
/// unless a proper width array is supplied.
///
/// Returns the object ID of the font dictionary.
pub fn write_truetype_font(
    font_data: &[u8],
    base_font_name: &str,
    widths: Option<&[i32; 224]>, // widths[0] = char 32, …, widths[223] = char 255
    writer: &mut PdfWriter,
) -> Result<u32> {
    // 1. Embed font binary as a stream.
    let mut stream_dict = PdfDict::new();
    stream_dict.insert("Subtype".to_owned(), PdfObject::Name("TrueType".to_owned()));
    let font_stream = make_raw_stream(font_data.to_vec(), stream_dict);
    let font_file_id = writer.add_object(PdfObject::Stream(Box::new(font_stream)));

    // 2. FontDescriptor
    let mut desc = PdfDict::new();
    desc.insert(
        "Type".to_owned(),
        PdfObject::Name("FontDescriptor".to_owned()),
    );
    desc.insert(
        "FontName".to_owned(),
        PdfObject::Name(base_font_name.to_owned()),
    );
    // Placeholder metrics — a full implementation would parse TTF head/hhea/OS2 tables.
    desc.insert("Flags".to_owned(), PdfObject::Integer(32));
    desc.insert("ItalicAngle".to_owned(), PdfObject::Integer(0));
    desc.insert("Ascent".to_owned(), PdfObject::Integer(800));
    desc.insert("Descent".to_owned(), PdfObject::Integer(-200));
    desc.insert("CapHeight".to_owned(), PdfObject::Integer(700));
    desc.insert(
        "FontBBox".to_owned(),
        PdfObject::Array(vec![
            PdfObject::Integer(-100),
            PdfObject::Integer(-200),
            PdfObject::Integer(1000),
            PdfObject::Integer(900),
        ]),
    );
    desc.insert("StemV".to_owned(), PdfObject::Integer(80));
    desc.insert(
        "FontFile2".to_owned(),
        PdfObject::Reference(font_file_id, 0),
    );
    let desc_id = writer.add_object(PdfObject::Dictionary(desc));

    // 3. Widths array (chars 32–255 = 224 entries)
    let width_array: Vec<PdfObject> = if let Some(w) = widths {
        w.iter().map(|&n| PdfObject::Integer(n as i64)).collect()
    } else {
        // Default: 600 units for every glyph (monospace-style placeholder)
        vec![PdfObject::Integer(600); 224]
    };

    // 4. Font dictionary
    let mut font = PdfDict::new();
    font.insert("Type".to_owned(), PdfObject::Name("Font".to_owned()));
    font.insert("Subtype".to_owned(), PdfObject::Name("TrueType".to_owned()));
    font.insert(
        "BaseFont".to_owned(),
        PdfObject::Name(base_font_name.to_owned()),
    );
    font.insert("FirstChar".to_owned(), PdfObject::Integer(32));
    font.insert("LastChar".to_owned(), PdfObject::Integer(255));
    font.insert("Widths".to_owned(), PdfObject::Array(width_array));
    font.insert(
        "FontDescriptor".to_owned(),
        PdfObject::Reference(desc_id, 0),
    );
    font.insert(
        "Encoding".to_owned(),
        PdfObject::Name("WinAnsiEncoding".to_owned()),
    );

    Ok(writer.add_object(PdfObject::Dictionary(font)))
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_font_dict_has_required_keys() {
        let mut writer = PdfWriter::new();
        let id = write_standard_font("Helvetica", &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("Type"), Some(&PdfObject::Name("Font".to_owned())));
            assert_eq!(d.get("Subtype"), Some(&PdfObject::Name("Type1".to_owned())));
            assert_eq!(
                d.get("BaseFont"),
                Some(&PdfObject::Name("Helvetica".to_owned()))
            );
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn unknown_standard_font_returns_error() {
        let mut writer = PdfWriter::new();
        let err = write_standard_font("NotAFont", &mut writer).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn truetype_font_has_font_descriptor() {
        let dummy_ttf = vec![0u8; 64]; // not a real font, just placeholder bytes
        let mut writer = PdfWriter::new();
        let font_id = write_truetype_font(&dummy_ttf, "TestFont", None, &mut writer).unwrap();
        let obj = writer.get_object(font_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert!(d.contains_key("FontDescriptor"));
            assert_eq!(
                d.get("Subtype"),
                Some(&PdfObject::Name("TrueType".to_owned()))
            );
            assert_eq!(d.get("FirstChar"), Some(&PdfObject::Integer(32)));
            assert_eq!(d.get("LastChar"), Some(&PdfObject::Integer(255)));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn all_14_standard_fonts_accepted() {
        let fonts = [
            "Times-Roman",
            "Times-Bold",
            "Times-Italic",
            "Times-BoldItalic",
            "Courier",
            "Courier-Bold",
            "Courier-Oblique",
            "Courier-BoldOblique",
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
            "Symbol",
            "ZapfDingbats",
        ];
        let mut writer = PdfWriter::new();
        for name in &fonts {
            write_standard_font(name, &mut writer).unwrap();
        }
    }
}
