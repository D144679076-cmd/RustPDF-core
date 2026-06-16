//! XMP metadata builder for PDF/A conformance declarations.

/// Build a minimal XMP metadata XML string for PDF/A conformance.
///
/// `title` and `author` are optional document metadata fields.
/// `part` is the PDF/A part number (1, 2, or 3).
/// `conformance` is the conformance level ('B' for basic, 'U' for unicode).
pub fn build_pdfa_xmp(
    title: Option<&str>,
    author: Option<&str>,
    part: u8,
    conformance: char,
) -> String {
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
        xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
      <dc:title><rdf:Alt><rdf:li xml:lang="x-default">{}</rdf:li></rdf:Alt></dc:title>
      <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>
      <pdfaid:part>{}</pdfaid:part>
      <pdfaid:conformance>{}</pdfaid:conformance>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        title.unwrap_or(""),
        author.unwrap_or(""),
        part,
        conformance
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_contains_pdfa_tags() {
        let xmp = build_pdfa_xmp(Some("Test"), Some("Author"), 1, 'B');
        assert!(xmp.contains("<pdfaid:part>1</pdfaid:part>"));
        assert!(xmp.contains("<pdfaid:conformance>B</pdfaid:conformance>"));
        assert!(xmp.contains("Test"));
        assert!(xmp.contains("Author"));
    }

    #[test]
    fn xmp_handles_none_fields() {
        let xmp = build_pdfa_xmp(None, None, 2, 'B');
        assert!(xmp.contains("<pdfaid:part>2</pdfaid:part>"));
        assert!(xmp.contains("x-default\">"));
    }
}
