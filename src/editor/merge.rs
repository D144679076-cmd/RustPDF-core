//! PDF document merge — combine multiple PDFs into one.
//!
//! # Usage
//!
//! ```ignore
//! let merged = MergeBuilder::new()
//!     .add_source(pdf_a_bytes)?
//!     .add_source(pdf_b_bytes)?
//!     .merge()?;
//! ```
//!
//! Pages appear in the order sources were added. Bookmark/outline trees from
//! each source are chained together under a new shared outline root.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};
use crate::writer::document::PdfWriter;

use super::remap::remap_object;

// ─────────────────────────────────────────────────────────────────────────────

/// Builder for merging multiple PDF documents into one.
pub struct MergeBuilder {
    sources: Vec<PdfDocument>,
}

impl MergeBuilder {
    /// Create an empty merge builder.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Add a source PDF (raw bytes). Returns an error if the bytes cannot be parsed.
    pub fn add_source(mut self, data: Vec<u8>) -> Result<Self> {
        let doc = PdfDocument::parse(data)?;
        self.sources.push(doc);
        Ok(self)
    }

    /// Merge all added sources and return the combined PDF bytes.
    ///
    /// Pages appear in source order. Outline trees are chained together.
    /// Returns an error if no sources were added.
    pub fn merge(self) -> Result<Vec<u8>> {
        #[cfg(feature = "crypto")]
        crate::license::require(crate::license::Tier::Pro, "merge")?;
        if self.sources.is_empty() {
            return Err(PdfError::invalid_structure(
                "merge requires at least one source",
            ));
        }

        let mut writer = PdfWriter::new();

        // Pre-allocate the unified Pages node so pages can reference it as /Parent.
        let pages_id = writer.reserve_id(); // → ID 1

        // next_available tracks the lowest unused ID in our writer pool.
        // After reserve_id() the writer's internal next_id is 2.
        let mut next_available: u32 = 2;

        let mut all_page_ids: Vec<u32> = Vec::new();

        // Outline chain: (first_item_id, last_item_id, count) per source, remapped.
        let mut outline_chains: Vec<(u32, u32, i64)> = Vec::new();

        for doc in &self.sources {
            let src_max = doc.max_object_id();
            if src_max == 0 {
                continue; // empty / malformed source — skip
            }

            // offset maps source IDs 1..=src_max → next_available..=next_available+src_max-1
            let offset = next_available - 1;

            // ── Collect page IDs (before remapping) ──────────────────────────
            let src_pages_id = catalog_pages_id(doc)?;
            let src_page_ids = collect_page_ids(doc, src_pages_id)?;

            // ── Collect outline info (before remapping) ───────────────────────
            let outline_info = get_outline_chain(doc);

            // ── Copy all objects with ID remapping ───────────────────────────
            for src_id in 1..=src_max {
                let obj = match doc.get_object(src_id) {
                    Ok(PdfObject::Null) => continue, // free / missing
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let remapped = remap_object(&obj, offset);
                writer.set_object(src_id + offset, remapped);
            }

            // ── Fix page /Parent references ───────────────────────────────────
            for &src_page_id in &src_page_ids {
                let new_page_id = src_page_id + offset;
                if let Some(obj) = writer.get_object(new_page_id).cloned() {
                    let mut page_dict = match obj {
                        PdfObject::Dictionary(d) => d,
                        _ => continue,
                    };
                    // Flatten any inherited MediaBox so it survives reparenting.
                    flatten_inherited_mediabox(&mut page_dict, doc, src_pages_id);
                    // Flatten any inherited Resources dict.
                    flatten_inherited_resources(&mut page_dict, doc, src_pages_id);
                    // Update /Parent to point at the new unified Pages node.
                    page_dict.insert("Parent".to_owned(), PdfObject::Reference(pages_id, 0));
                    writer.set_object(new_page_id, PdfObject::Dictionary(page_dict));
                }
                all_page_ids.push(new_page_id);
            }

            // ── Record outline chain (remapped IDs) ──────────────────────────
            if let Some((first, last, count)) = outline_info {
                outline_chains.push((first + offset, last + offset, count));
            }

            next_available += src_max;
        }

        // ── Build unified Pages node ──────────────────────────────────────────
        let page_count = all_page_ids.len() as i64;
        let kids: Vec<PdfObject> = all_page_ids
            .iter()
            .map(|&id| PdfObject::Reference(id, 0))
            .collect();
        let mut pages_dict: PdfDict = PdfDict::new();
        pages_dict.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages_dict.insert("Kids".to_owned(), PdfObject::Array(kids));
        pages_dict.insert("Count".to_owned(), PdfObject::Integer(page_count));
        writer.set_object(pages_id, PdfObject::Dictionary(pages_dict));

        // ── Merge outline trees ───────────────────────────────────────────────
        let outline_root_id = if !outline_chains.is_empty() {
            Some(build_merged_outlines(&mut writer, &outline_chains)?)
        } else {
            None
        };

        // ── Build Catalog ─────────────────────────────────────────────────────
        let mut catalog: PdfDict = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        if let Some(oid) = outline_root_id {
            catalog.insert("Outlines".to_owned(), PdfObject::Reference(oid, 0));
        }
        let catalog_id = writer.add_object(PdfObject::Dictionary(catalog));

        writer.serialize_all(catalog_id, None, None)
    }
}

impl Default for MergeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the /Pages object ID from a document's Catalog.
fn catalog_pages_id(doc: &PdfDocument) -> Result<u32> {
    let root_ref = doc
        .trailer
        .get("Root")
        .ok_or_else(|| PdfError::invalid_structure("trailer missing /Root"))?
        .clone();
    let catalog = doc.resolve(&root_ref)?;
    let pages_ref = catalog
        .as_dict()
        .and_then(|d| d.get("Pages"))
        .ok_or_else(|| PdfError::invalid_structure("catalog missing /Pages"))?
        .clone();
    match pages_ref {
        PdfObject::Reference(id, _) => Ok(id),
        _ => Err(PdfError::invalid_structure("/Pages is not a reference")),
    }
}

/// Collect all leaf page object IDs by recursively walking the page tree.
fn collect_page_ids(doc: &PdfDocument, node_id: u32) -> Result<Vec<u32>> {
    let node = doc.get_object(node_id)?;
    let dict = node
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("page tree node is not a dict"))?;

    let node_type = dict.get("Type").and_then(|o| o.as_name()).unwrap_or("");

    if node_type == "Page" {
        return Ok(vec![node_id]);
    }

    // Pages node — recurse into /Kids
    let kids = dict
        .get("Kids")
        .ok_or_else(|| PdfError::invalid_structure("Pages node missing /Kids"))?
        .clone();
    let kids_arr = match kids {
        PdfObject::Array(arr) => arr,
        _ => return Err(PdfError::invalid_structure("/Kids is not an array")),
    };

    let mut ids = Vec::new();
    for kid in &kids_arr {
        let kid_id = match kid {
            PdfObject::Reference(id, _) => *id,
            _ => continue,
        };
        ids.extend(collect_page_ids(doc, kid_id)?);
    }
    Ok(ids)
}

/// If the page dict has no /MediaBox, copy it from the parent Pages node.
fn flatten_inherited_mediabox(page_dict: &mut PdfDict, doc: &PdfDocument, pages_id: u32) {
    if page_dict.contains_key("MediaBox") {
        return;
    }
    if let Ok(pages_obj) = doc.get_object(pages_id) {
        if let Some(mb) = pages_obj.as_dict().and_then(|d| d.get("MediaBox")) {
            page_dict.insert("MediaBox".to_owned(), mb.clone());
        }
    }
}

/// If the page dict has no /Resources, copy it from the parent Pages node.
fn flatten_inherited_resources(page_dict: &mut PdfDict, doc: &PdfDocument, pages_id: u32) {
    if page_dict.contains_key("Resources") {
        return;
    }
    if let Ok(pages_obj) = doc.get_object(pages_id) {
        if let Some(res) = pages_obj.as_dict().and_then(|d| d.get("Resources")) {
            page_dict.insert("Resources".to_owned(), res.clone());
        }
    }
}

/// Return `(first_item_id, last_item_id, count)` of the source's outline tree, if present.
///
/// These are source-local IDs (before remapping by the caller).
fn get_outline_chain(doc: &PdfDocument) -> Option<(u32, u32, i64)> {
    // Resolve Catalog → /Outlines
    let root_ref = doc.trailer.get("Root")?.clone();
    let catalog = doc.resolve(&root_ref).ok()?;
    let outlines_ref = catalog.as_dict()?.get("Outlines")?.clone();
    let outlines_id = match outlines_ref {
        PdfObject::Reference(id, _) => id,
        _ => return None,
    };

    let outlines = doc.get_object(outlines_id).ok()?;
    let d = outlines.as_dict()?;

    let first_id = match d.get("First")? {
        PdfObject::Reference(id, _) => *id,
        _ => return None,
    };
    let last_id = match d.get("Last")? {
        PdfObject::Reference(id, _) => *id,
        _ => return None,
    };
    let count = d.get("Count").and_then(|o| o.as_integer()).unwrap_or(0);

    Some((first_id, last_id, count))
}

/// Chain multiple outline linked-lists into a single new root Outlines object.
///
/// Updates `/Next` on each source's last item and `/Prev` on the next source's
/// first item so all items form one flat linked list under the new root.
fn build_merged_outlines(writer: &mut PdfWriter, chains: &[(u32, u32, i64)]) -> Result<u32> {
    // Reserve the new outline root ID up front so items can reference it via /Parent.
    let root_id = writer.reserve_id();
    let total_count: i64 = chains.iter().map(|(_, _, c)| c).sum();

    // Link adjacent chains: last[i].Next → first[i+1], first[i+1].Prev → last[i].
    for pair in chains.windows(2) {
        let (_, last_id, _) = pair[0];
        let (next_first_id, _, _) = pair[1];

        // Update last item of current chain
        if let Some(obj) = writer.get_object(last_id).cloned() {
            let mut d = match obj {
                PdfObject::Dictionary(d) => d,
                _ => continue,
            };
            d.insert("Next".to_owned(), PdfObject::Reference(next_first_id, 0));
            d.insert("Parent".to_owned(), PdfObject::Reference(root_id, 0));
            writer.set_object(last_id, PdfObject::Dictionary(d));
        }

        // Update first item of next chain
        if let Some(obj) = writer.get_object(next_first_id).cloned() {
            let mut d = match obj {
                PdfObject::Dictionary(d) => d,
                _ => continue,
            };
            d.insert("Prev".to_owned(), PdfObject::Reference(last_id, 0));
            d.insert("Parent".to_owned(), PdfObject::Reference(root_id, 0));
            writer.set_object(next_first_id, PdfObject::Dictionary(d));
        }
    }

    // Update /Parent on the first chain's first and last items.
    if let Some(&(first_id, last_id, _)) = chains.first() {
        for item_id in [first_id, last_id] {
            if let Some(PdfObject::Dictionary(mut d)) = writer.get_object(item_id).cloned() {
                d.insert("Parent".to_owned(), PdfObject::Reference(root_id, 0));
                writer.set_object(item_id, PdfObject::Dictionary(d));
            }
        }
    }
    // And the last chain's last item.
    if chains.len() > 1 {
        if let Some(&(_, last_id, _)) = chains.last() {
            if let Some(PdfObject::Dictionary(mut d)) = writer.get_object(last_id).cloned() {
                d.insert("Parent".to_owned(), PdfObject::Reference(root_id, 0));
                writer.set_object(last_id, PdfObject::Dictionary(d));
            }
        }
    }

    let overall_first = chains.first().map(|&(f, _, _)| f);
    let overall_last = chains.last().map(|&(_, l, _)| l);

    // Build the new outline root dictionary.
    let mut root: PdfDict = PdfDict::new();
    root.insert("Type".to_owned(), PdfObject::Name("Outlines".to_owned()));
    root.insert("Count".to_owned(), PdfObject::Integer(total_count));
    if let Some(fid) = overall_first {
        root.insert("First".to_owned(), PdfObject::Reference(fid, 0));
    }
    if let Some(lid) = overall_last {
        root.insert("Last".to_owned(), PdfObject::Reference(lid, 0));
    }
    writer.set_object(root_id, PdfObject::Dictionary(root));

    Ok(root_id)
}

// ─────────────────────────────────────────────────────────────────────────────
// Page extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extract a contiguous range of pages from `source_data` into a new PDF document.
///
/// `page_range` is 0-based with exclusive end: `0..3` extracts pages 0, 1, 2.
/// Returns the bytes of the new single-document PDF. Requires `Tier::Pro`.
pub fn extract_pages(source_data: Vec<u8>, page_range: std::ops::Range<usize>) -> Result<Vec<u8>> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "extract_pages")?;

    let doc = PdfDocument::parse(source_data)?;

    let src_pages_id = catalog_pages_id(&doc)?;
    let all_page_ids = collect_page_ids(&doc, src_pages_id)?;
    let total = all_page_ids.len();

    if page_range.is_empty() || page_range.start >= total || page_range.end > total {
        return Err(PdfError::invalid_structure("page_range out of bounds"));
    }

    // Step 1 — collect and prepare page dicts (flatten inherited properties).
    let mut page_entries: Vec<(u32, PdfDict)> = Vec::new();
    let mut page_id_set: std::collections::HashSet<u32> = std::collections::HashSet::new();

    for i in page_range {
        let page_id = all_page_ids[i];
        let page_obj = doc.get_object(page_id)?;
        let mut page_dict = page_obj
            .as_dict()
            .ok_or_else(|| PdfError::invalid_structure("page object is not a dictionary"))?
            .clone();
        flatten_inherited_mediabox(&mut page_dict, &doc, src_pages_id);
        flatten_inherited_resources(&mut page_dict, &doc, src_pages_id);
        page_dict.shift_remove("Parent");
        page_entries.push((page_id, page_dict));
        page_id_set.insert(page_id);
    }

    // Step 2 — transitive closure of all objects referenced by the extracted pages.
    // Pre-mark page IDs as visited so we use the modified page dicts, not the originals.
    let mut visited: std::collections::HashSet<u32> = page_id_set.clone();
    let mut work_queue: Vec<u32> = Vec::new();

    for (_, page_dict) in &page_entries {
        collect_refs(&PdfObject::Dictionary(page_dict.clone()), &mut work_queue);
    }

    while let Some(old_id) = work_queue.pop() {
        if !visited.insert(old_id) {
            continue;
        }
        let obj = match doc.get_object(old_id) {
            Ok(PdfObject::Null) => continue,
            Ok(o) => o,
            Err(_) => continue,
        };
        collect_refs(&obj, &mut work_queue);
    }

    // Step 3 — assign fresh IDs to every visited object.
    let mut new_writer = PdfWriter::new();
    let mut id_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

    for &old_id in &visited {
        let new_id = new_writer.reserve_id();
        id_map.insert(old_id, new_id);
    }

    // Write non-page objects with remapped references.
    for &old_id in &visited {
        if page_id_set.contains(&old_id) {
            continue; // pages are written below using the modified dicts
        }
        match doc.get_object(old_id) {
            Ok(PdfObject::Null) | Err(_) => {}
            Ok(obj) => {
                let remapped = remap_object_with_map(&obj, &id_map);
                new_writer.set_object(id_map[&old_id], remapped);
            }
        }
    }

    // Step 4 — build new /Pages node and /Catalog.
    let pages_id = new_writer.reserve_id();
    let root_id = new_writer.reserve_id();

    let mut kids: Vec<PdfObject> = Vec::new();
    for (old_page_id, mut page_dict) in page_entries {
        let new_page_id = id_map[&old_page_id];
        page_dict.insert("Parent".to_owned(), PdfObject::Reference(pages_id, 0));
        let remapped = remap_dict_with_map(&page_dict, &id_map);
        new_writer.set_object(new_page_id, PdfObject::Dictionary(remapped));
        kids.push(PdfObject::Reference(new_page_id, 0));
    }

    let page_count = kids.len() as i64;
    let mut pages_dict = PdfDict::new();
    pages_dict.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
    pages_dict.insert("Kids".to_owned(), PdfObject::Array(kids));
    pages_dict.insert("Count".to_owned(), PdfObject::Integer(page_count));
    new_writer.set_object(pages_id, PdfObject::Dictionary(pages_dict));

    let mut catalog_dict = PdfDict::new();
    catalog_dict.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
    catalog_dict.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
    new_writer.set_object(root_id, PdfObject::Dictionary(catalog_dict));

    // Step 5 — serialize.
    new_writer.serialize_all(root_id, None, None)
}

/// Recursively collect all indirect reference IDs reachable from `obj`.
fn collect_refs(obj: &PdfObject, queue: &mut Vec<u32>) {
    match obj {
        PdfObject::Reference(n, _) => queue.push(*n),
        PdfObject::Array(a) => a.iter().for_each(|x| collect_refs(x, queue)),
        PdfObject::Dictionary(d) => d.values().for_each(|x| collect_refs(x, queue)),
        PdfObject::Stream(s) => s.dict.values().for_each(|x| collect_refs(x, queue)),
        _ => {}
    }
}

/// Remap all indirect reference IDs in `obj` using `id_map`.
///
/// References with no entry in `id_map` are left unchanged (cross-doc refs are
/// unusual but should not cause a panic).
fn remap_object_with_map(
    obj: &PdfObject,
    id_map: &std::collections::HashMap<u32, u32>,
) -> PdfObject {
    match obj {
        PdfObject::Reference(n, g) => {
            PdfObject::Reference(id_map.get(n).copied().unwrap_or(*n), *g)
        }
        PdfObject::Array(a) => {
            PdfObject::Array(a.iter().map(|x| remap_object_with_map(x, id_map)).collect())
        }
        PdfObject::Dictionary(d) => PdfObject::Dictionary(remap_dict_with_map(d, id_map)),
        PdfObject::Stream(s) => {
            let mut ns = *s.clone();
            ns.dict = remap_dict_with_map(&s.dict, id_map);
            PdfObject::Stream(Box::new(ns))
        }
        other => other.clone(),
    }
}

fn remap_dict_with_map(dict: &PdfDict, id_map: &std::collections::HashMap<u32, u32>) -> PdfDict {
    dict.iter()
        .map(|(k, v)| (k.clone(), remap_object_with_map(v, id_map)))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    #[test]
    fn empty_merge_errors() {
        let err = MergeBuilder::new().merge().unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }

    #[test]
    fn merge_single_source_parseable() {
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let merged = MergeBuilder::new()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        let doc = PdfDocument::parse(merged).unwrap();
        assert_eq!(doc.page_count().unwrap(), before);
    }

    #[test]
    fn merge_two_copies_doubles_page_count() {
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let merged = MergeBuilder::new()
            .add_source(data.clone())
            .unwrap()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        let doc = PdfDocument::parse(merged).unwrap();
        assert_eq!(doc.page_count().unwrap(), before * 2);
    }

    #[test]
    fn merge_three_copies_correct_count() {
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let merged = MergeBuilder::new()
            .add_source(data.clone())
            .unwrap()
            .add_source(data.clone())
            .unwrap()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        let doc = PdfDocument::parse(merged).unwrap();
        assert_eq!(doc.page_count().unwrap(), before * 3);
    }

    #[test]
    fn merged_pdf_has_header() {
        let data = load("minimal.pdf");
        let merged = MergeBuilder::new()
            .add_source(data)
            .unwrap()
            .merge()
            .unwrap();
        assert!(merged.starts_with(b"%PDF-1.7"));
    }
}
