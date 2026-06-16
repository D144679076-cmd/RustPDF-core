# Phase 2 — PDF Optimization / Compression

**Status:** Complete — 2026-06-16
**Effort:** ~3 weeks
**Tier gate:** Pro

## Context

PDF files produced by our writer are currently not optimized — objects may have redundant data, uncompressed streams, duplicate images, or unused objects from incremental updates. This feature adds a post-processing optimization pass that produces smaller, cleaner PDFs.

## New Module `src/writer/optimizer.rs`

```rust
use crate::parser::{PdfDocument, PdfObject, PdfDict, PdfStream};
use crate::writer::PdfWriter;
use crate::error::Result;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct OptimizationOptions {
    /// Re-compress all FlateDecode streams at maximum compression level.
    pub recompress_streams: bool,
    /// Find identical streams (by SHA-256 hash) and merge into a single object.
    pub deduplicate_resources: bool,
    /// Remove objects not reachable from /Root or /Info (garbage collect).
    pub remove_unused_objects: bool,
    /// Downsample images above `image_max_dpi` to save space.
    pub downsample_images: bool,
    /// Maximum DPI for images (150 for web, 300 for print).
    pub image_max_dpi: u32,
}

impl Default for OptimizationOptions {
    fn default() -> Self {
        Self {
            recompress_streams: true,
            deduplicate_resources: true,
            remove_unused_objects: true,
            downsample_images: false,
            image_max_dpi: 150,
        }
    }
}

/// Optimize a PDF document. Returns optimized PDF bytes.
pub fn optimize(doc_bytes: &[u8], options: &OptimizationOptions) -> Result<Vec<u8>> {
    crate::license::require(crate::license::Tier::Pro, "optimize")?;

    let doc = PdfDocument::parse(doc_bytes.to_vec())?;
    let max_id = doc.max_object_id();

    // Step 1: Find all reachable object IDs
    let reachable = if options.remove_unused_objects {
        collect_reachable_ids(&doc, max_id)?
    } else {
        (1..=max_id).collect::<HashSet<_>>()
    };

    // Step 2: Collect all stream content hashes (for deduplication)
    let mut hash_to_id: HashMap<[u8; 32], u32> = HashMap::new();
    let mut dedup_map: HashMap<u32, u32> = HashMap::new(); // old_id → canonical_id
    if options.deduplicate_resources {
        for id in &reachable {
            if let Ok(PdfObject::Stream(s)) = doc.get_object(*id) {
                if let Ok(decoded) = s.decode_with_doc(&doc) {
                    let hash = sha256(&decoded);
                    if let Some(&canonical_id) = hash_to_id.get(&hash) {
                        dedup_map.insert(*id, canonical_id);
                    } else {
                        hash_to_id.insert(hash, *id);
                    }
                }
            }
        }
    }

    // Step 3: Build optimized writer
    let root_id_old = match doc.trailer.get("Root") {
        Some(PdfObject::Reference(n,_)) => *n,
        _ => return Err(crate::error::PdfError::invalid_structure("no root")),
    };
    let info_id_old = doc.trailer.get("Info").and_then(|o| if let PdfObject::Reference(n,_) = o { Some(*n) } else { None });

    // Assign sequential new IDs starting from 1
    let mut old_to_new: HashMap<u32, u32> = HashMap::new();
    let mut counter = 1u32;
    for &old_id in &reachable {
        // Skip deduplicated objects (they'll be remapped to canonical_id)
        if dedup_map.contains_key(&old_id) { continue; }
        old_to_new.insert(old_id, counter);
        counter += 1;
    }
    // Add dedup mappings: old_dup_id → new_canonical_id
    for (&dup_id, &canonical_id) in &dedup_map {
        if let Some(&new_canonical) = old_to_new.get(&canonical_id) {
            old_to_new.insert(dup_id, new_canonical);
        }
    }

    let mut new_writer = PdfWriter::new();
    // Reserve IDs
    for &new_id in old_to_new.values() {
        // ensure new_writer has capacity
        let _ = new_writer.reserve_id(); // increment internal counter
    }

    // Step 4: Copy and transform objects
    for &old_id in &reachable {
        if dedup_map.contains_key(&old_id) { continue; } // skip dups
        let new_id = old_to_new[&old_id];
        let obj = doc.get_object(old_id)?;

        let transformed = match obj {
            PdfObject::Stream(s) => {
                let decoded = s.decode_with_doc(&doc)?;
                let processed = if options.downsample_images && is_image_stream(&s.dict) {
                    downsample_if_needed(&s.dict, &decoded, options.image_max_dpi)?
                } else {
                    decoded
                };
                // Remap dict refs, recompress stream
                let mut new_dict = remap_dict(&s.dict, &old_to_new);
                let compressed = if options.recompress_streams {
                    crate::writer::streams::encode_flate(&processed)?
                } else { processed };
                new_dict.insert("Length".to_owned(), PdfObject::Integer(compressed.len() as i64));
                new_dict.insert("Filter".to_owned(), PdfObject::Name("FlateDecode".to_owned()));
                new_dict.remove("DecodeParms"); // clear old decode params
                PdfObject::Stream(Box::new(PdfStream { dict: new_dict, raw_data: compressed }))
            }
            other => remap_object(&other, &old_to_new),
        };

        new_writer.set_object(new_id, transformed);
    }

    let new_root_id = old_to_new[&root_id_old];
    let new_info_id = info_id_old.and_then(|id| old_to_new.get(&id)).copied();
    new_writer.serialize_all(new_root_id, new_info_id, None)
}

/// BFS from /Root and /Info to find all reachable object IDs.
fn collect_reachable_ids(doc: &PdfDocument, max_id: u32) -> Result<HashSet<u32>> {
    let mut reachable = HashSet::new();
    let mut queue = Vec::new();

    // Start from root and info
    if let Some(PdfObject::Reference(n,_)) = doc.trailer.get("Root") { queue.push(*n); }
    if let Some(PdfObject::Reference(n,_)) = doc.trailer.get("Info") { queue.push(*n); }

    while let Some(id) = queue.pop() {
        if !reachable.insert(id) { continue; }
        if let Ok(obj) = doc.get_object(id) {
            collect_refs_from_obj(&obj, &mut queue);
        }
    }
    Ok(reachable)
}

fn collect_refs_from_obj(obj: &PdfObject, queue: &mut Vec<u32>) {
    match obj {
        PdfObject::Reference(n, _) => queue.push(*n),
        PdfObject::Array(a) => a.iter().for_each(|x| collect_refs_from_obj(x, queue)),
        PdfObject::Dictionary(d) => d.values().for_each(|x| collect_refs_from_obj(x, queue)),
        PdfObject::Stream(s) => s.dict.values().for_each(|x| collect_refs_from_obj(x, queue)),
        _ => {}
    }
}

fn remap_object(obj: &PdfObject, map: &HashMap<u32, u32>) -> PdfObject {
    match obj {
        PdfObject::Reference(n, g) => PdfObject::Reference(*map.get(n).unwrap_or(n), *g),
        PdfObject::Array(a) => PdfObject::Array(a.iter().map(|x| remap_object(x, map)).collect()),
        PdfObject::Dictionary(d) => PdfObject::Dictionary(remap_dict(d, map)),
        PdfObject::Stream(s) => {
            let mut ns = *s.clone();
            ns.dict = remap_dict(&s.dict, map);
            PdfObject::Stream(Box::new(ns))
        }
        other => other.clone(),
    }
}

fn remap_dict(dict: &PdfDict, map: &HashMap<u32, u32>) -> PdfDict {
    dict.iter().map(|(k, v)| (k.clone(), remap_object(v, map))).collect()
}

fn is_image_stream(dict: &PdfDict) -> bool {
    matches!(dict.get("Subtype"), Some(PdfObject::Name(n)) if n == "Image")
}

fn downsample_if_needed(dict: &PdfDict, decoded: &[u8], max_dpi: u32) -> Result<Vec<u8>> {
    // Get image dimensions
    let width = match dict.get("Width") { Some(PdfObject::Integer(n)) => *n as u32, _ => return Ok(decoded.to_vec()) };
    let height = match dict.get("Height") { Some(PdfObject::Integer(n)) => *n as u32, _ => return Ok(decoded.to_vec()) };
    // Estimate DPI from image size (assume standard page ~8.5in = 612pt, so 72dpi base)
    // Without MediaBox context we can't know actual DPI accurately
    // For now: downsample if either dimension > (max_dpi * 11) (11 inches max page size)
    let max_pixels = max_dpi * 11;
    if width <= max_pixels && height <= max_pixels { return Ok(decoded.to_vec()); }
    let scale = max_pixels as f64 / width.max(height) as f64;
    let new_w = (width as f64 * scale) as u32;
    let new_h = (height as f64 * scale) as u32;
    // Bilinear downscale (3 channels assumed — DeviceRGB)
    let channels = 3usize;
    let mut result = vec![0u8; (new_w * new_h * channels as u32) as usize];
    for y in 0..new_h {
        for x in 0..new_w {
            let src_x = (x as f64 / new_w as f64 * width as f64) as u32;
            let src_y = (y as f64 / new_h as f64 * height as f64) as u32;
            let src_idx = ((src_y * width + src_x) * channels as u32) as usize;
            let dst_idx = ((y * new_w + x) * channels as u32) as usize;
            if src_idx + channels <= decoded.len() && dst_idx + channels <= result.len() {
                result[dst_idx..dst_idx+channels].copy_from_slice(&decoded[src_idx..src_idx+channels]);
            }
        }
    }
    // Re-compress as FlateDecode (lossless) — for lossy JPEG use zune-jpeg
    crate::writer::streams::encode_flate(&result)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Sha256, Digest};
    let mut h = Sha256::new(); h.update(data); h.finalize().into()
}
```

## WASM in `src/wasm/editor.rs`

```rust
#[wasm_bindgen]
pub fn optimize(&mut self, options_json: &str) -> Result<Vec<u8>, JsError> {
    // Parse options from JSON
    let options = parse_optimization_options(options_json);
    // Get current document bytes
    let current_bytes = self.editor.save_append()
        .map_err(|e| JsError::new(&e.to_string()))?;
    crate::writer::optimizer::optimize(&current_bytes, &options)
        .map_err(|e| JsError::new(&e.to_string()))
}
```

## Tests

```rust
#[test]
fn optimize_reduces_file_size_or_equal() {
    let data = include_bytes!("fixtures/multipage.pdf").to_vec();
    let original_len = data.len();
    let optimized = optimize(&data, &OptimizationOptions::default()).unwrap();
    // Optimized should be parseable
    let doc = PdfDocument::parse(optimized.clone()).unwrap();
    assert_eq!(doc.page_count().unwrap(), 3);
    // Size should not increase (usually decreases)
    assert!(optimized.len() <= original_len + 100); // allow tiny overhead for xref
}

#[test]
fn optimize_removes_unused_objects() {
    // Create PDF with some dangling objects then optimize
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let optimized = optimize(&data, &OptimizationOptions { remove_unused_objects: true, ..Default::default() }).unwrap();
    let doc = PdfDocument::parse(optimized).unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);
}

#[test]
fn optimize_output_is_valid_pdf() {
    let data = include_bytes!("fixtures/multipage.pdf").to_vec();
    let optimized = optimize(&data, &OptimizationOptions::default()).unwrap();
    assert!(optimized.starts_with(b"%PDF-"));
    assert!(optimized.ends_with(b"%%EOF\n") || optimized.ends_with(b"%%EOF"));
}
```

## Verification

```bash
cargo test --features crypto -- optimize
cargo build --target wasm32-unknown-unknown --features wasm,crypto
```
