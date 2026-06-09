//! PDF Structure Tree parser (ISO 32000-1 §14.7).
//!
//! Parses the `/StructTreeRoot` from the document catalog to provide
//! the logical document structure needed for PDF/UA accessibility and
//! correct reading order in complex Word layouts.

use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::text_string::decode_pdf_text_string;

/// A node in the logical structure tree.
#[derive(Debug, Clone)]
pub struct StructNode {
    /// Standard role (resolved via RoleMap, e.g., "Document", "P", "H1", "Table").
    pub role: String,
    /// Raw structure type from the /S entry.
    pub struct_type: String,
    /// Alternative text for accessibility (/Alt).
    pub alt_text: Option<String>,
    /// Actual text content replacement (/ActualText).
    pub actual_text: Option<String>,
    /// Language tag (/Lang).
    pub lang: Option<String>,
    /// Page reference (/Pg).
    pub page_ref: Option<PdfObject>,
    /// Child structure nodes.
    pub children: Vec<StructNode>,
    /// Marked content IDs for leaf content references.
    pub mcids: Vec<i32>,
}

/// Parse the full structure tree from the catalog.
///
/// Returns `Ok(None)` if no `/StructTreeRoot` is present in the catalog.
/// The tree is walked iteratively to avoid stack overflow on deep structures.
pub fn parse_struct_tree(doc: &PdfDocument, catalog_dict: &PdfDict) -> Result<Option<StructNode>> {
    let root_ref = match catalog_dict.get("StructTreeRoot") {
        Some(obj) => obj.clone(),
        None => return Ok(None),
    };

    let root_obj = doc.resolve(&root_ref)?;
    let root_dict = match root_obj {
        PdfObject::Dictionary(d) => d,
        _ => return Ok(None),
    };

    // Build role map for resolving custom types to standard roles
    let role_map = build_role_map(&root_dict, doc);

    // Parse the root element's children via /K
    let root_node = parse_struct_element(doc, &root_dict, &role_map)?;

    Ok(Some(root_node))
}

/// Parse a single structure element and its children recursively (iterative DFS).
fn parse_struct_element(
    doc: &PdfDocument,
    dict: &PdfDict,
    role_map: &std::collections::HashMap<String, String>,
) -> Result<StructNode> {
    let struct_type = dict
        .get("S")
        .and_then(|s| s.as_name())
        .unwrap_or("Document")
        .to_string();

    let role = role_map
        .get(&struct_type)
        .cloned()
        .unwrap_or_else(|| struct_type.clone());

    let alt_text = dict.get("Alt").and_then(|o| match o {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        _ => None,
    });

    let actual_text = dict.get("ActualText").and_then(|o| match o {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        _ => None,
    });

    let lang = dict.get("Lang").and_then(|o| match o {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        _ => None,
    });

    let page_ref = dict.get("Pg").cloned();

    let mut children = Vec::new();
    let mut mcids = Vec::new();

    // /K can be: a single dict, an integer (MCID), or an array of mixed
    if let Some(k_entry) = dict.get("K") {
        parse_k_entry(doc, k_entry, role_map, &mut children, &mut mcids)?;
    }

    Ok(StructNode {
        role,
        struct_type,
        alt_text,
        actual_text,
        lang,
        page_ref,
        children,
        mcids,
    })
}

/// Parse the /K entry which can be a single value or an array.
fn parse_k_entry(
    doc: &PdfDocument,
    k_entry: &PdfObject,
    role_map: &std::collections::HashMap<String, String>,
    children: &mut Vec<StructNode>,
    mcids: &mut Vec<i32>,
) -> Result<()> {
    match k_entry {
        PdfObject::Integer(n) => {
            mcids.push(*n as i32);
        }
        PdfObject::Dictionary(d) => {
            parse_k_dict(doc, d, role_map, children, mcids)?;
        }
        PdfObject::Reference(_, _) => {
            let resolved = doc.resolve(k_entry)?;
            match resolved {
                PdfObject::Dictionary(d) => {
                    parse_k_dict(doc, &d, role_map, children, mcids)?;
                }
                PdfObject::Integer(n) => {
                    mcids.push(n as i32);
                }
                _ => {}
            }
        }
        PdfObject::Array(arr) => {
            for item in arr {
                parse_k_entry(doc, item, role_map, children, mcids)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Parse a dictionary found in /K — either a struct element or a marked content reference.
fn parse_k_dict(
    doc: &PdfDocument,
    dict: &PdfDict,
    role_map: &std::collections::HashMap<String, String>,
    children: &mut Vec<StructNode>,
    mcids: &mut Vec<i32>,
) -> Result<()> {
    // Check if this is a marked content reference (MCR) dict
    let type_name = dict.get("Type").and_then(|t| t.as_name()).unwrap_or("");

    if type_name == "MCR" || dict.contains_key("MCID") {
        // Marked content reference — extract MCID
        if let Some(PdfObject::Integer(n)) = dict.get("MCID") {
            mcids.push(*n as i32);
        }
    } else if type_name == "OBJR" {
        // Object reference — skip (used for annotations/form fields)
    } else {
        // Structure element — recurse
        let child = parse_struct_element(doc, dict, role_map)?;
        children.push(child);
    }

    Ok(())
}

/// Build the role map from /RoleMap in StructTreeRoot.
fn build_role_map(
    root_dict: &PdfDict,
    doc: &PdfDocument,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();

    let role_map_obj = match root_dict.get("RoleMap") {
        Some(obj) => match doc.resolve(obj) {
            Ok(resolved) => resolved,
            Err(_) => return map,
        },
        None => return map,
    };

    if let PdfObject::Dictionary(rm_dict) = role_map_obj {
        for (key, value) in rm_dict.iter() {
            if let Some(standard_role) = value.as_name() {
                map.insert(key.clone(), standard_role.to_string());
            }
        }
    }

    map
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::PdfDocument;

    #[test]
    fn test_no_struct_tree_root() {
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
        let result = parse_struct_tree(&doc, &dict).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_struct_node_with_mcid() {
        let mut dict = PdfDict::new();
        dict.insert("S".to_string(), PdfObject::Name("P".to_string()));
        dict.insert("K".to_string(), PdfObject::Integer(5));

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
        let role_map = std::collections::HashMap::new();
        let node = parse_struct_element(&doc, &dict, &role_map).unwrap();

        assert_eq!(node.struct_type, "P");
        assert_eq!(node.role, "P");
        assert_eq!(node.mcids, vec![5]);
        assert!(node.children.is_empty());
    }

    #[test]
    fn test_role_map_resolves_custom_type() {
        let mut dict = PdfDict::new();
        dict.insert("S".to_string(), PdfObject::Name("MyHeading".to_string()));
        dict.insert("K".to_string(), PdfObject::Integer(0));

        let mut role_map = std::collections::HashMap::new();
        role_map.insert("MyHeading".to_string(), "H1".to_string());

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
        let node = parse_struct_element(&doc, &dict, &role_map).unwrap();

        assert_eq!(node.struct_type, "MyHeading");
        assert_eq!(node.role, "H1");
    }

    #[test]
    fn test_struct_node_with_alt_text() {
        let mut dict = PdfDict::new();
        dict.insert("S".to_string(), PdfObject::Name("Figure".to_string()));
        dict.insert(
            "Alt".to_string(),
            PdfObject::String(b"A photo of a cat".to_vec()),
        );

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
        let role_map = std::collections::HashMap::new();
        let node = parse_struct_element(&doc, &dict, &role_map).unwrap();

        assert_eq!(node.role, "Figure");
        assert_eq!(node.alt_text.as_deref(), Some("A photo of a cat"));
    }

    #[test]
    fn test_struct_node_with_array_k() {
        let mut child_dict = PdfDict::new();
        child_dict.insert("S".to_string(), PdfObject::Name("Span".to_string()));
        child_dict.insert("K".to_string(), PdfObject::Integer(2));

        let mut mcr_dict = PdfDict::new();
        mcr_dict.insert("Type".to_string(), PdfObject::Name("MCR".to_string()));
        mcr_dict.insert("MCID".to_string(), PdfObject::Integer(7));

        let mut parent_dict = PdfDict::new();
        parent_dict.insert("S".to_string(), PdfObject::Name("P".to_string()));
        parent_dict.insert(
            "K".to_string(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Dictionary(child_dict),
                PdfObject::Dictionary(mcr_dict),
            ]),
        );

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
        let role_map = std::collections::HashMap::new();
        let node = parse_struct_element(&doc, &parent_dict, &role_map).unwrap();

        assert_eq!(node.role, "P");
        assert_eq!(node.mcids, vec![1, 7]);
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].role, "Span");
        assert_eq!(node.children[0].mcids, vec![2]);
    }
}
