# Phase 1 — Page Split / Extract

**Status:** Complete — 2026-06-06
**Effort:** ~4–5 days
**Tier gate:** Pro

## Context

`src/editor/merge.rs` already has all the building blocks:
- `collect_page_ids()` — recursively walk page tree and collect leaf page object IDs
- `flatten_inherited_mediabox()` — copy `/MediaBox` from parent to page if absent
- `flatten_inherited_resources()` — copy `/Resources` from parent to page if absent
- `remap.rs::remap_object()` — shift all `Reference(id, gen)` by an offset for ID collision avoidance

Page split is the inverse of merge: copy a subset of pages from one source into a new `PdfWriter` with remapped IDs, then `serialize_all()`.

## New Function in `src/editor/merge.rs`

```rust
/// Extract pages in `page_range` from `source_data` into a new PDF document.
/// `page_range` is 0-based, exclusive end: `0..3` extracts pages 0, 1, 2.
pub fn extract_pages(
    source_data: Vec<u8>,
    page_range: std::ops::Range<usize>,
) -> Result<Vec<u8>> {
    crate::license::require(crate::license::Tier::Pro, "extract_pages")?;

    let editor = crate::editor::PdfEditor::open(source_data)?;
    let total = editor.page_count()?;
    if page_range.start >= total || page_range.end > total || page_range.is_empty() {
        return Err(crate::error::PdfError::invalid_structure("page_range out of bounds"));
    }

    // Step 1: Collect page dicts for requested range
    let mut page_dicts: Vec<(u32, crate::parser::PdfDict)> = Vec::new();
    for i in page_range.clone() {
        let (page_id, mut page_dict) = editor.get_page_dict(i)?;
        flatten_inherited_mediabox(&editor, &mut page_dict)?;
        flatten_inherited_resources(&editor, &mut page_dict)?;
        // Remove parent reference; will be set to new Pages node
        page_dict.remove("Parent");
        page_dicts.push((page_id, page_dict));
    }

    // Step 2: Set up new writer
    // Use offset 0 — we will remap all IDs starting from 1
    let mut new_writer = crate::writer::PdfWriter::new();

    // Step 3: Deep-copy all reachable objects from each page
    // Build a map: old_id → new_id
    let mut id_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

    let mut work_queue: Vec<u32> = page_dicts.iter().map(|(id, _)| *id).collect();
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();

    // Collect all referenced object IDs transitively
    while let Some(old_id) = work_queue.pop() {
        if !visited.insert(old_id) { continue; }
        let obj = editor.get_object(old_id)?;
        collect_refs(&obj, &mut work_queue);
    }

    // Assign new IDs to all visited objects (except the page dicts themselves which we handle separately)
    for &old_id in &visited {
        if !id_map.contains_key(&old_id) {
            let new_id = new_writer.reserve_id();
            id_map.insert(old_id, new_id);
        }
    }

    // Write all copied objects with remapped refs
    for &old_id in &visited {
        let obj = editor.get_object(old_id)?;
        let remapped = remap_object_with_map(&obj, &id_map);
        let new_id = id_map[&old_id];
        new_writer.set_object(new_id, remapped);
    }

    // Step 4: Build new /Pages node
    let pages_id = new_writer.reserve_id();
    let root_id = new_writer.reserve_id();

    let mut kids: Vec<crate::parser::PdfObject> = Vec::new();
    for (old_page_id, mut page_dict) in page_dicts {
        let new_page_id = id_map[&old_page_id];
        // Set /Parent to new pages node
        page_dict.insert("Parent".to_owned(), crate::parser::PdfObject::Reference(pages_id, 0));
        // Remap all refs within page dict
        let remapped_page = remap_dict_with_map(&page_dict, &id_map);
        new_writer.set_object(new_page_id, crate::parser::PdfObject::Dictionary(remapped_page));
        kids.push(crate::parser::PdfObject::Reference(new_page_id, 0));
    }

    let page_count = kids.len() as i64;
    let mut pages_dict = crate::parser::PdfDict::new();
    pages_dict.insert("Type".to_owned(), crate::parser::PdfObject::Name("Pages".to_owned()));
    pages_dict.insert("Kids".to_owned(), crate::parser::PdfObject::Array(kids));
    pages_dict.insert("Count".to_owned(), crate::parser::PdfObject::Integer(page_count));
    new_writer.set_object(pages_id, crate::parser::PdfObject::Dictionary(pages_dict));

    let mut catalog_dict = crate::parser::PdfDict::new();
    catalog_dict.insert("Type".to_owned(), crate::parser::PdfObject::Name("Catalog".to_owned()));
    catalog_dict.insert("Pages".to_owned(), crate::parser::PdfObject::Reference(pages_id, 0));
    new_writer.set_object(root_id, crate::parser::PdfObject::Dictionary(catalog_dict));

    // Step 5: Serialize
    new_writer.serialize_all(root_id, None, None)
}

/// Recursively collect all Reference IDs from an object.
fn collect_refs(obj: &crate::parser::PdfObject, queue: &mut Vec<u32>) {
    use crate::parser::PdfObject;
    match obj {
        PdfObject::Reference(n, _) => queue.push(*n),
        PdfObject::Array(a) => a.iter().for_each(|x| collect_refs(x, queue)),
        PdfObject::Dictionary(d) => d.values().for_each(|x| collect_refs(x, queue)),
        PdfObject::Stream(s) => s.dict.values().for_each(|x| collect_refs(x, queue)),
        _ => {}
    }
}

/// Remap all Reference IDs in an object using id_map.
fn remap_object_with_map(
    obj: &crate::parser::PdfObject,
    id_map: &std::collections::HashMap<u32, u32>,
) -> crate::parser::PdfObject {
    use crate::parser::PdfObject;
    match obj {
        PdfObject::Reference(n, g) => {
            let new_id = id_map.get(n).copied().unwrap_or(*n);
            PdfObject::Reference(new_id, *g)
        }
        PdfObject::Array(a) => PdfObject::Array(a.iter().map(|x| remap_object_with_map(x, id_map)).collect()),
        PdfObject::Dictionary(d) => PdfObject::Dictionary(remap_dict_with_map(d, id_map)),
        PdfObject::Stream(s) => {
            let mut ns = *s.clone();
            ns.dict = remap_dict_with_map(&s.dict, id_map);
            PdfObject::Stream(Box::new(ns))
        }
        other => other.clone(),
    }
}

fn remap_dict_with_map(
    dict: &crate::parser::PdfDict,
    id_map: &std::collections::HashMap<u32, u32>,
) -> crate::parser::PdfDict {
    dict.iter().map(|(k, v)| (k.clone(), remap_object_with_map(v, id_map))).collect()
}
```

## WASM in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn extract_pages(&self, start: usize, end: usize) -> Result<Vec<u8>, JsError> {
    // Re-serialize original document bytes to pass to extract_pages
    let original_bytes = self.editor.doc.raw_bytes().to_vec(); // assumes PdfDocument stores raw bytes
    crate::editor::merge::extract_pages(original_bytes, start..end)
        .map_err(|e| JsError::new(&e.to_string()))
}
```

Note: if `PdfDocument` does not currently expose raw bytes, add `pub fn raw_bytes(&self) -> &[u8]` to `PdfDocument` (it already stores the original `data: Vec<u8>` internally).

## Tests in `tests/merge_redact.rs`

```rust
#[test]
fn extract_one_page_from_multipage() {
    let data = include_bytes!("fixtures/multipage.pdf").to_vec();
    let result = extract_pages(data, 0..1).unwrap();
    let doc = PdfDocument::parse(result).unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);
}

#[test]
fn extract_range_correct_count() {
    let data = include_bytes!("fixtures/multipage.pdf").to_vec();
    let result = extract_pages(data, 0..2).unwrap();
    let doc = PdfDocument::parse(result).unwrap();
    assert_eq!(doc.page_count().unwrap(), 2);
}

#[test]
fn extracted_pdf_starts_with_pdf_header() {
    let data = include_bytes!("fixtures/multipage.pdf").to_vec();
    let result = extract_pages(data, 1..2).unwrap();
    assert!(result.starts_with(b"%PDF-"));
}

#[test]
fn extract_out_of_bounds_errors() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let result = extract_pages(data, 0..5);
    assert!(result.is_err());
}
```

## Verification

```bash
cargo test -- extract_pages
cargo build --target wasm32-unknown-unknown --features wasm
```
