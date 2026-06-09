//! PDF Outline (Bookmark) tree parser.
//!
//! Parses the /Outlines dictionary and its linked list of outline items
//! (ISO 32000-1 §12.3.3). The outline tree provides a table-of-contents
//! style navigation structure.

use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::text_string::decode_pdf_text_string;

/// A single outline (bookmark) entry.
#[derive(Debug, Clone)]
pub struct OutlineItem {
    /// Display title of this bookmark.
    pub title: String,
    /// Destination page index (0-based), if resolvable.
    pub dest_page: Option<usize>,
    /// Raw destination object (for advanced use).
    pub dest: Option<PdfObject>,
    /// Action associated with this item (/A entry).
    pub action: Option<PdfObject>,
    /// Whether this item is initially open (children visible).
    pub open: bool,
    /// Child outline items.
    pub children: Vec<OutlineItem>,
}

/// Parse the entire outline tree from the document catalog.
///
/// Returns an empty Vec if no /Outlines dictionary is present.
pub fn parse_outlines(doc: &PdfDocument, catalog_dict: &PdfDict) -> Result<Vec<OutlineItem>> {
    let outlines_ref = match catalog_dict.get("Outlines") {
        Some(obj) => obj.clone(),
        None => return Ok(Vec::new()),
    };

    let outlines_obj = doc.resolve(&outlines_ref)?;
    let outlines_dict = match outlines_obj {
        PdfObject::Dictionary(d) => d,
        _ => return Ok(Vec::new()),
    };

    let first_ref = match outlines_dict.get("First") {
        Some(obj) => obj.clone(),
        None => return Ok(Vec::new()),
    };

    parse_outline_level(doc, &first_ref)
}

/// Parse one level of the outline linked list starting from `first_ref`.
fn parse_outline_level(doc: &PdfDocument, first_ref: &PdfObject) -> Result<Vec<OutlineItem>> {
    let mut items = Vec::new();
    let mut current_ref = Some(first_ref.clone());
    let mut count = 0u32;
    const MAX_ITEMS_PER_LEVEL: u32 = 10_000;

    while let Some(ref node_ref) = current_ref {
        count += 1;
        if count > MAX_ITEMS_PER_LEVEL {
            log::warn!(
                "outline level exceeded {} items, stopping",
                MAX_ITEMS_PER_LEVEL
            );
            break;
        }

        let node_obj = doc.resolve(node_ref)?;
        let node_dict = match node_obj {
            PdfObject::Dictionary(d) => d,
            _ => break,
        };

        let title = extract_title(&node_dict);

        let dest = node_dict.get("Dest").cloned();
        let action = node_dict.get("A").cloned();

        // /Count < 0 means the item is closed
        let open = match node_dict.get("Count") {
            Some(PdfObject::Integer(n)) => *n > 0,
            _ => false,
        };

        // Parse children if /First is present
        let children = match node_dict.get("First") {
            Some(child_first) => parse_outline_level(doc, child_first)?,
            None => Vec::new(),
        };

        items.push(OutlineItem {
            title,
            dest_page: None, // Resolved later if needed
            dest,
            action,
            open,
            children,
        });

        // Move to /Next sibling
        current_ref = node_dict.get("Next").cloned();
    }

    Ok(items)
}

/// Extract the /Title string from an outline item dictionary.
fn extract_title(dict: &PdfDict) -> String {
    match dict.get("Title") {
        Some(PdfObject::String(bytes)) => decode_pdf_text_string(bytes),
        _ => String::new(),
    }
}

/// Resolve an outline destination to a page index.
///
/// Destinations can be:
/// - An array `[page_ref /XYZ left top zoom]`
/// - A name string referencing the /Dests or /Names tree
///
/// This function handles the direct array form. For named destinations
/// (string form), use [`resolve_dest_with_names`] which accesses the name tree.
pub fn resolve_dest_page_index(
    _doc: &PdfDocument,
    dest: &PdfObject,
    page_refs: &[PdfObject],
) -> Option<usize> {
    let arr = match dest {
        PdfObject::Array(a) => a,
        _ => return None,
    };

    if arr.is_empty() {
        return None;
    }

    let page_ref = &arr[0];

    // Match against known page references
    for (idx, known_ref) in page_refs.iter().enumerate() {
        if page_ref == known_ref {
            return Some(idx);
        }
    }

    // If it's a direct page number (some generators do this)
    if let PdfObject::Integer(n) = page_ref {
        let n = *n as usize;
        if n < page_refs.len() {
            return Some(n);
        }
    }

    None
}

/// Resolve an outline destination that may be a named destination string.
///
/// Handles both direct array destinations and string-keyed named destinations
/// by looking up the name in the catalog's `/Names /Dests` tree.
pub fn resolve_dest_with_names(
    doc: &PdfDocument,
    dest: &PdfObject,
    catalog_dict: &PdfDict,
    page_refs: &[PdfObject],
) -> Option<usize> {
    match dest {
        PdfObject::Array(_) => resolve_dest_page_index(doc, dest, page_refs),
        PdfObject::String(bytes) => {
            let name = decode_pdf_text_string(bytes);
            let resolved =
                super::name_tree::resolve_named_dest(doc, catalog_dict, &name).ok()??;
            resolve_dest_page_index(doc, &resolved, page_refs)
        }
        PdfObject::Name(name) => {
            let resolved = super::name_tree::resolve_named_dest(doc, catalog_dict, name).ok()??;
            resolve_dest_page_index(doc, &resolved, page_refs)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_outlines() {
        let dict = PdfDict::new();
        let pdf_bytes = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n\
xref\n\
0 4\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000115 00000 n \n\
trailer\n\
<< /Size 4 /Root 1 0 R >>\n\
startxref\n\
186\n\
%%EOF\n";
        let doc = PdfDocument::parse(pdf_bytes.to_vec()).unwrap();
        let outlines = parse_outlines(&doc, &dict).unwrap();
        assert!(outlines.is_empty());
    }

    #[test]
    fn test_decode_ascii_title() {
        let bytes = b"Chapter 1";
        assert_eq!(decode_pdf_text_string(bytes), "Chapter 1");
    }

    #[test]
    fn test_decode_utf16_title() {
        // BOM + "AB"
        let bytes = vec![0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42];
        assert_eq!(decode_pdf_text_string(&bytes), "AB");
    }

    #[test]
    fn test_resolve_dest_page_index_array() {
        let page_refs = vec![
            PdfObject::Reference(3, 0),
            PdfObject::Reference(5, 0),
            PdfObject::Reference(7, 0),
        ];
        let dest = PdfObject::Array(vec![
            PdfObject::Reference(5, 0),
            PdfObject::Name("XYZ".into()),
            PdfObject::Null,
            PdfObject::Null,
            PdfObject::Null,
        ]);

        let pdf_bytes = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n\
xref\n\
0 4\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000115 00000 n \n\
trailer\n\
<< /Size 4 /Root 1 0 R >>\n\
startxref\n\
186\n\
%%EOF\n";
        let doc = PdfDocument::parse(pdf_bytes.to_vec()).unwrap();
        assert_eq!(resolve_dest_page_index(&doc, &dest, &page_refs), Some(1));
    }
}
