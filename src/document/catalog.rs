//! PDF Document Catalog and Page Tree traversal.
//!
//! The catalog is the root of the document's object hierarchy (ISO 32000-1 §7.7.2).
//! The page tree is walked iteratively to avoid stack overflow on deeply nested trees.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::name_tree;
use super::page_labels::PageLabelTree;
use super::structure::StructNode;

/// The document catalog — entry point for all document-level structures.
pub struct Catalog {
    /// The catalog dictionary itself.
    pub dict: PdfDict,
    /// Reference to the root of the page tree (/Pages).
    pub pages_ref: PdfObject,
    /// Total page count from the root /Pages node.
    pub page_count: usize,
}

impl Catalog {
    /// Build a Catalog from the document's trailer dictionary.
    ///
    /// Resolves /Root → catalog dict → /Pages and reads /Count.
    pub fn from_document(doc: &PdfDocument) -> Result<Self> {
        let root_ref = doc
            .trailer
            .get("Root")
            .ok_or_else(|| PdfError::invalid_token(0, "trailer missing /Root"))?
            .clone();

        let catalog_obj = doc.resolve(&root_ref)?;
        let catalog_dict = match catalog_obj {
            PdfObject::Dictionary(d) => d,
            _ => {
                return Err(PdfError::invalid_token(
                    0,
                    "trailer /Root does not resolve to a dictionary",
                ))
            }
        };

        let pages_ref = catalog_dict
            .get("Pages")
            .ok_or_else(|| PdfError::invalid_token(0, "catalog missing /Pages"))?
            .clone();

        let pages_obj = doc.resolve(&pages_ref)?;
        let pages_dict = pages_obj
            .as_dict()
            .ok_or_else(|| PdfError::invalid_token(0, "/Pages does not resolve to a dictionary"))?;

        let count = pages_dict
            .get("Count")
            .and_then(|c| match c {
                PdfObject::Integer(n) => Some(*n as usize),
                _ => None,
            })
            .ok_or_else(|| PdfError::invalid_token(0, "/Pages missing or invalid /Count"))?;

        Ok(Catalog {
            dict: catalog_dict,
            pages_ref,
            page_count: count,
        })
    }

    /// Retrieve the page dictionary for page at `index` (0-based).
    ///
    /// Uses an iterative (non-recursive) traversal of the page tree to avoid
    /// stack overflow on adversarial inputs with deeply nested /Pages nodes.
    pub fn get_page_dict(&self, doc: &PdfDocument, index: usize) -> Result<PdfDict> {
        if index >= self.page_count {
            return Err(PdfError::invalid_token(
                0,
                format!(
                    "page index {} out of range (document has {} pages)",
                    index, self.page_count
                ),
            ));
        }

        // O(1) fast path: flatten the page tree into an index → page-reference
        // table once (cached on the document), then resolve directly. This
        // replaces the per-lookup O(N) tree walk below — the dominant cost when
        // the same page is rendered repeatedly (e.g. per keystroke while editing
        // a later page). References (not resolved dicts) are cached, so the
        // override layer is still honored on resolve.
        if !doc.has_page_table() {
            let refs = collect_page_refs_iterative(doc, &self.pages_ref, self.page_count)?;
            doc.set_page_table(refs);
        }
        if let Some(page_ref) = doc.cached_page_ref(index) {
            if let PdfObject::Dictionary(d) = doc.resolve(&page_ref)? {
                return Ok(d);
            }
            // Cached ref didn't resolve to a page dict (malformed/unusual tree)
            // → fall through to the authoritative tree walk below.
        }

        // BFS/DFS iterative traversal: stack of (node_dict, accumulated_index)
        let mut stack: Vec<(PdfDict, usize)> = Vec::new();

        let root_obj = doc.resolve(&self.pages_ref)?;
        let root_dict = match root_obj {
            PdfObject::Dictionary(d) => d,
            _ => {
                return Err(PdfError::invalid_token(
                    0,
                    "/Pages root is not a dictionary",
                ))
            }
        };
        stack.push((root_dict, 0));

        // Depth limit to prevent infinite loops in malformed files.
        let mut iterations = 0u32;
        const MAX_ITERATIONS: u32 = 100_000;

        while let Some((node, base_idx)) = stack.pop() {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                return Err(PdfError::invalid_token(
                    0,
                    "page tree traversal exceeded iteration limit (possible cycle)",
                ));
            }

            let node_type = node.get("Type").and_then(|t| t.as_name()).unwrap_or("");

            if node_type == "Page" {
                if base_idx == index {
                    return Ok(node);
                }
                continue;
            }

            // It's a /Pages node — iterate its /Kids
            let kids = match node.get("Kids") {
                Some(PdfObject::Array(arr)) => arr.clone(),
                _ => {
                    return Err(PdfError::invalid_token(
                        0,
                        "/Pages node missing /Kids array",
                    ))
                }
            };

            // Walk kids in order, tracking cumulative page index.
            // We push onto the stack in reverse so that the first kid is processed first.
            let mut cumulative = base_idx;
            let mut children_to_push: Vec<(PdfDict, usize)> = Vec::new();

            for kid_ref in &kids {
                let kid_obj = doc.resolve(kid_ref)?;
                let kid_dict = match kid_obj {
                    PdfObject::Dictionary(d) => d,
                    _ => {
                        return Err(PdfError::invalid_token(
                            0,
                            "/Kids element does not resolve to a dictionary",
                        ))
                    }
                };

                let kid_type = kid_dict.get("Type").and_then(|t| t.as_name()).unwrap_or("");

                if kid_type == "Page" {
                    if cumulative == index {
                        return Ok(kid_dict);
                    }
                    cumulative += 1;
                } else {
                    // /Pages intermediate node
                    let kid_count = kid_dict
                        .get("Count")
                        .and_then(|c| match c {
                            PdfObject::Integer(n) => Some(*n as usize),
                            _ => None,
                        })
                        .unwrap_or(0);

                    if index < cumulative + kid_count {
                        // Target page is within this subtree
                        children_to_push.push((kid_dict, cumulative));
                        break;
                    }
                    cumulative += kid_count;
                }
            }

            // Push children in reverse order for correct DFS ordering
            for child in children_to_push.into_iter().rev() {
                stack.push(child);
            }
        }

        Err(PdfError::invalid_token(
            0,
            format!("page {} not found in page tree", index),
        ))
    }

    /// Collect all page dictionaries in document order.
    ///
    /// Returns a Vec of (page_dict, inherited_attributes) pairs.
    /// Useful for batch operations over all pages.
    pub fn all_page_dicts(&self, doc: &PdfDocument) -> Result<Vec<PdfDict>> {
        let mut pages = Vec::with_capacity(self.page_count);
        collect_pages_iterative(doc, &self.pages_ref, &mut pages)?;
        Ok(pages)
    }

    /// Get the /AcroForm dictionary reference if present.
    pub fn acroform(&self) -> Option<&PdfObject> {
        self.dict.get("AcroForm")
    }

    /// Get the /Outlines (bookmarks) reference if present.
    pub fn outlines(&self) -> Option<&PdfObject> {
        self.dict.get("Outlines")
    }

    /// Get the /Names dictionary reference if present.
    pub fn names(&self) -> Option<&PdfObject> {
        self.dict.get("Names")
    }

    /// Get the document's PDF version from the catalog /Version key.
    /// Falls back to None if not specified (version comes from header instead).
    pub fn version(&self) -> Option<&str> {
        self.dict.get("Version").and_then(|v| v.as_name())
    }

    /// Resolve a named destination string to its destination object.
    ///
    /// Checks `/Names /Dests` name tree first, then falls back to the
    /// catalog's direct `/Dests` dictionary (PDF 1.1 style).
    pub fn resolve_named_dest(&self, doc: &PdfDocument, name: &str) -> Result<Option<PdfObject>> {
        name_tree::resolve_named_dest(doc, &self.dict, name)
    }

    /// Parse the page label tree from the catalog's `/PageLabels` entry.
    ///
    /// Returns `Ok(None)` if no `/PageLabels` is present.
    pub fn page_labels(&self, doc: &PdfDocument) -> Result<Option<PageLabelTree>> {
        PageLabelTree::from_catalog(doc, &self.dict)
    }

    /// Parse the structure tree from the catalog's `/StructTreeRoot` entry.
    ///
    /// Returns `Ok(None)` if no `/StructTreeRoot` is present.
    pub fn struct_tree(&self, doc: &PdfDocument) -> Result<Option<StructNode>> {
        super::structure::parse_struct_tree(doc, &self.dict)
    }
}

/// Iteratively collect each leaf /Page's *reference* (or inline dict) in
/// document order — the building block for the document's O(1) page table.
///
/// Mirrors [`collect_pages_iterative`] but pushes the node reference rather than
/// the resolved dict, so resolution stays lazy (and honors `overrides`).
fn collect_page_refs_iterative(
    doc: &PdfDocument,
    pages_ref: &PdfObject,
    capacity: usize,
) -> Result<Vec<PdfObject>> {
    let mut refs = Vec::with_capacity(capacity);
    let mut stack: Vec<PdfObject> = vec![pages_ref.clone()];
    let mut iterations = 0u32;
    const MAX_ITERATIONS: u32 = 500_000;

    while let Some(node_ref) = stack.pop() {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            return Err(PdfError::invalid_token(
                0,
                "page tree collection exceeded iteration limit",
            ));
        }

        let node_dict = match doc.resolve(&node_ref)? {
            PdfObject::Dictionary(d) => d,
            _ => continue,
        };

        let node_type = node_dict
            .get("Type")
            .and_then(|t| t.as_name())
            .unwrap_or("");

        if node_type == "Page" {
            // Store the reference as found in /Kids (an inline dict resolves to
            // itself), keeping resolution lazy.
            refs.push(node_ref);
        } else if let Some(PdfObject::Array(kids)) = node_dict.get("Kids") {
            // /Pages node — push kids in reverse for correct document order.
            for kid in kids.iter().rev() {
                stack.push(kid.clone());
            }
        }
    }

    Ok(refs)
}

/// Iteratively collect all leaf /Page dicts from the page tree.
fn collect_pages_iterative(
    doc: &PdfDocument,
    pages_ref: &PdfObject,
    out: &mut Vec<PdfDict>,
) -> Result<()> {
    // Stack holds page tree nodes to process.
    let mut stack: Vec<PdfObject> = vec![pages_ref.clone()];
    let mut iterations = 0u32;
    const MAX_ITERATIONS: u32 = 500_000;

    while let Some(node_ref) = stack.pop() {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            return Err(PdfError::invalid_token(
                0,
                "page tree collection exceeded iteration limit",
            ));
        }

        let node_obj = doc.resolve(&node_ref)?;
        let node_dict = match node_obj {
            PdfObject::Dictionary(d) => d,
            _ => continue,
        };

        let node_type = node_dict
            .get("Type")
            .and_then(|t| t.as_name())
            .unwrap_or("");

        if node_type == "Page" {
            out.push(node_dict);
        } else {
            // /Pages node — push kids in reverse order for correct output order
            if let Some(PdfObject::Array(kids)) = node_dict.get("Kids") {
                for kid in kids.iter().rev() {
                    stack.push(kid.clone());
                }
            }
        }
    }

    Ok(())
}

/// Resolve inherited page attributes by walking up the /Parent chain.
///
/// PDF page trees allow /MediaBox, /CropBox, /Rotate, and /Resources to be
/// inherited from ancestor /Pages nodes (ISO 32000-1 §7.7.3.4).
pub fn resolve_inherited_attribute(
    doc: &PdfDocument,
    page_dict: &PdfDict,
    key: &str,
) -> Result<Option<PdfObject>> {
    // Check the page itself first
    if let Some(val) = page_dict.get(key) {
        return Ok(Some(doc.resolve(val)?));
    }

    // Walk up /Parent chain
    let mut current = page_dict.get("Parent").cloned();
    let mut depth = 0u32;
    const MAX_DEPTH: u32 = 100;

    while let Some(parent_ref) = current {
        depth += 1;
        if depth > MAX_DEPTH {
            return Err(PdfError::invalid_token(
                0,
                "page /Parent chain exceeds depth limit (possible cycle)",
            ));
        }

        let parent_obj = doc.resolve(&parent_ref)?;
        let parent_dict = match parent_obj {
            PdfObject::Dictionary(d) => d,
            _ => break,
        };

        if let Some(val) = parent_dict.get(key) {
            return Ok(Some(doc.resolve(val)?));
        }

        current = parent_dict.get("Parent").cloned();
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_from_minimal_pdf() {
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
        let catalog = Catalog::from_document(&doc).unwrap();
        assert_eq!(catalog.page_count, 1);
    }

    #[test]
    fn test_get_page_dict() {
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
        let catalog = Catalog::from_document(&doc).unwrap();
        let page = catalog.get_page_dict(&doc, 0).unwrap();

        assert_eq!(page.get("Type"), Some(&PdfObject::Name("Page".into())));
        assert!(page.get("MediaBox").is_some());
    }

    #[test]
    fn test_get_page_out_of_range() {
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
        let catalog = Catalog::from_document(&doc).unwrap();
        let result = catalog.get_page_dict(&doc, 5);
        assert!(result.is_err());
    }

    #[test]
    fn page_table_matches_walk_and_caches() {
        let bytes = std::fs::read(format!(
            "{}/tests/fixtures/multipage.pdf",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        let catalog = Catalog::from_document(&doc).unwrap();
        let n = catalog.page_count;
        assert!(n >= 2, "multipage fixture should have >= 2 pages, got {n}");

        // The O(1) page table is lazy — not built until the first lookup.
        assert!(!doc.has_page_table());

        // Every page via the (table-backed) get_page_dict must equal the page at
        // the same index from the independent document-order collector — proving
        // the flattened table preserves order/identity. The collector path does
        // not consult the table, so this is a genuine cross-check.
        let via_collector = catalog.all_page_dicts(&doc).unwrap();
        assert_eq!(via_collector.len(), n);
        for (i, expected) in via_collector.iter().enumerate() {
            assert_eq!(
                &catalog.get_page_dict(&doc, i).unwrap(),
                expected,
                "page {i}"
            );
        }

        // The first lookup built the table; subsequent lookups are O(1).
        assert!(doc.has_page_table());

        // Each cached entry resolves to that page's dict, and pages are distinct.
        for i in 0..n {
            let r = doc.cached_page_ref(i).expect("cached ref present");
            match doc.resolve(&r).unwrap() {
                PdfObject::Dictionary(d) => {
                    assert_eq!(d.get("Type"), Some(&PdfObject::Name("Page".into())))
                }
                other => panic!("page {i} ref did not resolve to a dict: {other:?}"),
            }
            for j in (i + 1)..n {
                assert_ne!(
                    doc.cached_page_ref(i),
                    doc.cached_page_ref(j),
                    "pages {i} and {j} share a reference"
                );
            }
        }
    }
}
