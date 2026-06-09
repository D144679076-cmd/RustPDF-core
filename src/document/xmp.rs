//! XMP metadata stream parser.
//!
//! Parses the `/Metadata` stream from the document catalog (ISO 32000-1 §14.3.2).
//! Word always writes XMP with dc:title, xmp:CreatorTool, pdf:Producer, etc.
//! Uses byte-level scanning — no XML crate needed for the predictable Word XMP format.

use crate::error::Result;

/// Structured XMP metadata extracted from a PDF's /Metadata stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XmpMetadata {
    /// Document title (dc:title).
    pub title: Option<String>,
    /// Document creator/author (dc:creator).
    pub creator: Option<String>,
    /// Application that created the content (xmp:CreatorTool).
    pub creator_tool: Option<String>,
    /// Creation date in ISO 8601 format (xmp:CreateDate).
    pub create_date: Option<String>,
    /// Last modification date in ISO 8601 format (xmp:ModifyDate).
    pub modify_date: Option<String>,
    /// PDF producer application (pdf:Producer).
    pub producer: Option<String>,
    /// Keywords (pdf:Keywords).
    pub keywords: Option<String>,
    /// Document description (dc:description).
    pub description: Option<String>,
}

/// Parse XMP metadata from raw XML bytes.
///
/// Extracts standard Dublin Core, XMP, and PDF namespace fields.
/// Returns a default (all-None) struct on malformed or empty input
/// rather than propagating errors — XMP is advisory metadata.
pub fn parse_xmp_stream(xml_bytes: &[u8]) -> Result<XmpMetadata> {
    let text = match std::str::from_utf8(xml_bytes) {
        Ok(s) => s,
        Err(_) => return Ok(XmpMetadata::default()),
    };

    Ok(XmpMetadata {
        title: extract_rdf_li_value(text, "dc:title"),
        creator: extract_rdf_li_value(text, "dc:creator"),
        creator_tool: extract_simple_tag_value(text, "xmp:CreatorTool"),
        create_date: extract_simple_tag_value(text, "xmp:CreateDate"),
        modify_date: extract_simple_tag_value(text, "xmp:ModifyDate"),
        producer: extract_simple_tag_value(text, "pdf:Producer"),
        keywords: extract_simple_tag_value(text, "pdf:Keywords"),
        description: extract_rdf_li_value(text, "dc:description"),
    })
}

/// Extract a simple tag value: `<tag>value</tag>`.
fn extract_simple_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);

    let start_pos = xml.find(&open)?;
    let after_open = &xml[start_pos + open.len()..];

    // Skip attributes and find the closing '>'
    let content_start = after_open.find('>')?;

    // Handle self-closing tag
    if after_open[..content_start].ends_with('/') {
        return None;
    }

    let content = &after_open[content_start + 1..];
    let end_pos = content.find(&close)?;
    let value = content[..end_pos].trim();

    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Extract a value wrapped in rdf:Alt or rdf:Seq → rdf:li structure.
///
/// Pattern: `<tag><rdf:Alt><rdf:li ...>value</rdf:li></rdf:Alt></tag>`
/// Also handles `<rdf:Seq>` (used for dc:creator).
fn extract_rdf_li_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);

    let start_pos = xml.find(&open)?;
    let after_open = &xml[start_pos..];
    let end_pos = after_open.find(&close)?;
    let block = &after_open[..end_pos];

    // Find the first <rdf:li within this block
    let li_start = block.find("<rdf:li")?;
    let after_li = &block[li_start..];

    // Skip past the opening tag (may have attributes like xml:lang="x-default")
    let content_start = after_li.find('>')?;
    let content = &after_li[content_start + 1..];

    let li_end = content.find("</rdf:li>")?;
    let value = content[..li_end].trim();

    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
  <dc:title>
    <rdf:Alt>
      <rdf:li xml:lang="x-default">Test Document Title</rdf:li>
    </rdf:Alt>
  </dc:title>
  <dc:creator>
    <rdf:Seq>
      <rdf:li>John Doe</rdf:li>
    </rdf:Seq>
  </dc:creator>
  <dc:description>
    <rdf:Alt>
      <rdf:li xml:lang="x-default">A test PDF document</rdf:li>
    </rdf:Alt>
  </dc:description>
  <xmp:CreatorTool>Microsoft Word 2019</xmp:CreatorTool>
  <xmp:CreateDate>2023-04-15T12:00:00+05:30</xmp:CreateDate>
  <xmp:ModifyDate>2023-04-16T09:30:00Z</xmp:ModifyDate>
  <pdf:Producer>Microsoft Word 2019</pdf:Producer>
  <pdf:Keywords>test, pdf, word</pdf:Keywords>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    #[test]
    fn test_parse_full_xmp() {
        let meta = parse_xmp_stream(SAMPLE_XMP.as_bytes()).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Test Document Title"));
        assert_eq!(meta.creator.as_deref(), Some("John Doe"));
        assert_eq!(meta.creator_tool.as_deref(), Some("Microsoft Word 2019"));
        assert_eq!(
            meta.create_date.as_deref(),
            Some("2023-04-15T12:00:00+05:30")
        );
        assert_eq!(meta.modify_date.as_deref(), Some("2023-04-16T09:30:00Z"));
        assert_eq!(meta.producer.as_deref(), Some("Microsoft Word 2019"));
        assert_eq!(meta.keywords.as_deref(), Some("test, pdf, word"));
        assert_eq!(meta.description.as_deref(), Some("A test PDF document"));
    }

    #[test]
    fn test_parse_empty_xmp() {
        let meta = parse_xmp_stream(b"").unwrap();
        assert_eq!(meta, XmpMetadata::default());
    }

    #[test]
    fn test_parse_invalid_utf8() {
        let meta = parse_xmp_stream(&[0xFF, 0xFE, 0x00]).unwrap();
        assert_eq!(meta, XmpMetadata::default());
    }

    #[test]
    fn test_parse_partial_xmp() {
        let partial = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about=""
    xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
  <pdf:Producer>LibreOffice 7.4</pdf:Producer>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>"#;

        let meta = parse_xmp_stream(partial.as_bytes()).unwrap();
        assert_eq!(meta.producer.as_deref(), Some("LibreOffice 7.4"));
        assert!(meta.title.is_none());
        assert!(meta.creator.is_none());
    }

    #[test]
    fn test_extract_simple_tag() {
        let xml = "<xmp:CreatorTool>Word</xmp:CreatorTool>";
        assert_eq!(
            extract_simple_tag_value(xml, "xmp:CreatorTool"),
            Some("Word".to_string())
        );
    }

    #[test]
    fn test_extract_simple_tag_self_closing() {
        let xml = "<xmp:CreatorTool/>";
        assert_eq!(extract_simple_tag_value(xml, "xmp:CreatorTool"), None);
    }

    #[test]
    fn test_extract_rdf_li() {
        let xml = r#"<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Hello</rdf:li></rdf:Alt></dc:title>"#;
        assert_eq!(
            extract_rdf_li_value(xml, "dc:title"),
            Some("Hello".to_string())
        );
    }

    #[test]
    fn test_extract_missing_tag() {
        let xml = "<pdf:Producer>Test</pdf:Producer>";
        assert_eq!(extract_simple_tag_value(xml, "xmp:CreatorTool"), None);
    }
}
