//! PDF Name Tree and Named Destination resolution.
//!
//! Name trees (ISO 32000-1 §7.9.6) map string keys to PDF objects.
//! The primary use case is resolving named destinations from the catalog's
//! `/Names /Dests` tree, used by Word TOC hyperlinks.

use std::collections::HashMap;

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::text_string::decode_pdf_text_string;

const MAX_ITERATIONS: u32 = 50_000;

/// Walk a PDF name tree node and collect all (key, value) pairs.
///
/// Name trees use `/Names` arrays at leaf nodes and `/Kids` arrays at
/// intermediate nodes. This function walks iteratively to avoid stack overflow.
pub fn walk_name_tree(doc: &PdfDocument, node: &PdfObject) -> Result<HashMap<String, PdfObject>> {
    let mut result = HashMap::new();
    let mut stack: Vec<PdfObject> = vec![node.clone()];
    let mut iterations = 0u32;

    while let Some(current_ref) = stack.pop() {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            return Err(PdfError::invalid_token(
                0,
                "name tree traversal exceeded iteration limit",
            ));
        }

        let current_obj = doc.resolve(&current_ref)?;
        let current_dict = match current_obj {
            PdfObject::Dictionary(d) => d,
            _ => continue,
        };

        // Leaf node: has /Names array of (key, value) pairs
        if let Some(PdfObject::Array(names)) = current_dict.get("Names") {
            let mut i = 0;
            while i + 1 < names.len() {
                let key = match &names[i] {
                    PdfObject::String(bytes) => decode_pdf_text_string(bytes),
                    _ => {
                        i += 2;
                        continue;
                    }
                };
                let value = names[i + 1].clone();
                result.insert(key, value);
                i += 2;
            }
        }

        // Intermediate node: has /Kids array of child tree nodes
        if let Some(PdfObject::Array(kids)) = current_dict.get("Kids") {
            for kid in kids.iter().rev() {
                stack.push(kid.clone());
            }
        }
    }

    Ok(result)
}

/// Resolve a named destination string to its destination object.
///
/// Checks the catalog's `/Names /Dests` name tree first, then falls back
/// to the catalog's direct `/Dests` dictionary (PDF 1.1 style).
/// Returns `Ok(None)` if the name is not found in either location.
pub fn resolve_named_dest(
    doc: &PdfDocument,
    catalog_dict: &PdfDict,
    name: &str,
) -> Result<Option<PdfObject>> {
    // Try /Names /Dests name tree first (PDF 1.2+)
    if let Some(names_ref) = catalog_dict.get("Names") {
        let names_obj = doc.resolve(names_ref)?;
        if let PdfObject::Dictionary(names_dict) = names_obj {
            if let Some(dests_ref) = names_dict.get("Dests") {
                let tree = walk_name_tree(doc, dests_ref)?;
                if let Some(dest_obj) = tree.get(name) {
                    let resolved = doc.resolve(dest_obj)?;
                    // Dest value may be a dict with /D key or a direct array
                    return Ok(Some(unwrap_dest(resolved)));
                }
            }
        }
    }

    // Fallback: direct /Dests dictionary (PDF 1.1 style)
    if let Some(dests_ref) = catalog_dict.get("Dests") {
        let dests_obj = doc.resolve(dests_ref)?;
        if let PdfObject::Dictionary(dests_dict) = dests_obj {
            if let Some(dest_obj) = dests_dict.get(name) {
                let resolved = doc.resolve(dest_obj)?;
                return Ok(Some(unwrap_dest(resolved)));
            }
        }
    }

    Ok(None)
}

/// Unwrap a destination value that may be wrapped in a dictionary with /D key.
fn unwrap_dest(obj: PdfObject) -> PdfObject {
    match obj {
        PdfObject::Dictionary(ref d) => {
            if let Some(d_val) = d.get("D") {
                d_val.clone()
            } else {
                obj
            }
        }
        _ => obj,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfDocument;

    #[test]
    fn test_resolve_named_dest_from_names_tree() {
        let pdf_bytes = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R /Names 4 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n\
4 0 obj << /Dests 5 0 R >> endobj\n\
5 0 obj << /Names [(section1) [3 0 R /XYZ 0 792 0]] >> endobj\n\
xref\n\
0 6\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000071 00000 n \n\
0000000128 00000 n \n\
0000000199 00000 n \n\
0000000233 00000 n \n\
trailer\n\
<< /Size 6 /Root 1 0 R >>\n\
startxref\n\
295\n\
%%EOF\n";

        let doc = PdfDocument::parse(pdf_bytes.to_vec()).unwrap();
        let catalog = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let result = resolve_named_dest(&doc, &catalog.dict, "section1").unwrap();
        assert!(result.is_some());
        match result.unwrap() {
            PdfObject::Array(arr) => {
                assert_eq!(arr[0], PdfObject::Reference(3, 0));
            }
            _ => panic!("expected array destination"),
        }
    }

    #[test]
    fn test_resolve_named_dest_not_found() {
        let pdf_bytes = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R /Names 4 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n\
4 0 obj << /Dests 5 0 R >> endobj\n\
5 0 obj << /Names [(section1) [3 0 R /XYZ 0 792 0]] >> endobj\n\
xref\n\
0 6\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000071 00000 n \n\
0000000128 00000 n \n\
0000000199 00000 n \n\
0000000233 00000 n \n\
trailer\n\
<< /Size 6 /Root 1 0 R >>\n\
startxref\n\
295\n\
%%EOF\n";

        let doc = PdfDocument::parse(pdf_bytes.to_vec()).unwrap();
        let catalog = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let result = resolve_named_dest(&doc, &catalog.dict, "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_named_dest_from_direct_dests_dict() {
        let pdf_bytes = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R /Dests 4 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n\
4 0 obj << /page1 [3 0 R /Fit] >> endobj\n\
xref\n\
0 5\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000071 00000 n \n\
0000000128 00000 n \n\
0000000199 00000 n \n\
trailer\n\
<< /Size 5 /Root 1 0 R >>\n\
startxref\n\
240\n\
%%EOF\n";

        let doc = PdfDocument::parse(pdf_bytes.to_vec()).unwrap();
        let catalog = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let result = resolve_named_dest(&doc, &catalog.dict, "page1").unwrap();
        assert!(result.is_some());
        match result.unwrap() {
            PdfObject::Array(arr) => {
                assert_eq!(arr[0], PdfObject::Reference(3, 0));
                assert_eq!(arr[1], PdfObject::Name("Fit".into()));
            }
            _ => panic!("expected array destination"),
        }
    }

    #[test]
    fn test_walk_empty_name_tree() {
        let pdf_bytes = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n\
4 0 obj << /Names [] >> endobj\n\
xref\n\
0 5\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000115 00000 n \n\
0000000186 00000 n \n\
trailer\n\
<< /Size 5 /Root 1 0 R >>\n\
startxref\n\
217\n\
%%EOF\n";

        let doc = PdfDocument::parse(pdf_bytes.to_vec()).unwrap();
        let node = PdfObject::Reference(4, 0);
        let tree = walk_name_tree(&doc, &node).unwrap();
        assert!(tree.is_empty());
    }
}
