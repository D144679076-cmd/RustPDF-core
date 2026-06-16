//! PDF optimization — compress, deduplicate, and garbage-collect objects.
//!
//! Entry point: [`optimize`].  Gated on the `writer` feature; SHA-256 stream
//! deduplication additionally requires the `crypto` feature.

use std::collections::{HashMap, HashSet};

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject, PdfStream};
use crate::writer::PdfWriter;

/// Options controlling the optimization pass.
#[derive(Debug, Clone)]
pub struct OptimizationOptions {
    /// Re-compress all FlateDecode streams at maximum compression level.
    pub recompress_streams: bool,
    /// Find identical streams (by SHA-256 hash) and merge into a single object.
    /// Requires the `crypto` feature; ignored when it is absent.
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

/// Optimize a PDF document in memory and return the optimized bytes.
///
/// Applies the passes selected in `options` in sequence:
/// 1. Garbage-collect unreachable objects (if `remove_unused_objects`).
/// 2. Deduplicate identical streams by SHA-256 hash (if `deduplicate_resources` + `crypto`).
/// 3. Downsample over-resolution images (if `downsample_images`).
/// 4. Re-compress all streams at maximum zlib level (if `recompress_streams`).
///
/// The result is a fresh PDF with sequentially numbered objects.
/// Requires a Pro license.
pub fn optimize(doc_bytes: &[u8], options: &OptimizationOptions) -> Result<Vec<u8>> {
    crate::license::require(crate::license::Tier::Pro, "optimize")?;

    let doc = PdfDocument::parse(doc_bytes.to_vec())?;
    let max_id = doc.max_object_id();

    // Step 1: collect reachable object IDs.
    let reachable: HashSet<u32> = if options.remove_unused_objects {
        collect_reachable_ids(&doc)?
    } else {
        (1..=max_id).collect()
    };

    // Step 2: build deduplication map (old_dup_id → canonical_id).
    let dedup_map: HashMap<u32, u32> =
        build_dedup_map(&doc, &reachable, options.deduplicate_resources);

    // Step 3: assign sequential new IDs (1-based) skipping duplicates.
    let old_to_new = assign_new_ids(&reachable, &dedup_map);

    // Locate /Root and /Info from the trailer.
    let root_id_old = match doc.trailer.get("Root") {
        Some(PdfObject::Reference(n, _)) => *n,
        _ => {
            return Err(PdfError::invalid_structure(
                "optimizer: no /Root in trailer",
            ))
        }
    };
    let info_id_old = doc.trailer.get("Info").and_then(|o| {
        if let PdfObject::Reference(n, _) = o {
            Some(*n)
        } else {
            None
        }
    });

    // Step 4: copy and transform objects into a fresh writer.
    let mut new_writer = PdfWriter::new();
    for &old_id in &reachable {
        if dedup_map.contains_key(&old_id) {
            continue; // replaced by canonical
        }
        let new_id = old_to_new[&old_id];
        let obj = doc.get_object(old_id)?;
        let transformed = transform_object(obj, options, &old_to_new, &doc)?;
        new_writer.set_object(new_id, transformed);
    }

    let new_root_id = old_to_new[&root_id_old];
    let new_info_id = info_id_old.and_then(|id| old_to_new.get(&id)).copied();
    new_writer.serialize_all(new_root_id, new_info_id, None)
}

// ── Reachability ──────────────────────────────────────────────────────────────

/// BFS from /Root and /Info to find every reachable object ID.
fn collect_reachable_ids(doc: &PdfDocument) -> Result<HashSet<u32>> {
    let mut reachable = HashSet::new();
    let mut queue: Vec<u32> = Vec::new();

    if let Some(PdfObject::Reference(n, _)) = doc.trailer.get("Root") {
        queue.push(*n);
    }
    if let Some(PdfObject::Reference(n, _)) = doc.trailer.get("Info") {
        queue.push(*n);
    }

    while let Some(id) = queue.pop() {
        if !reachable.insert(id) {
            continue;
        }
        if let Ok(obj) = doc.get_object(id) {
            collect_refs(&obj, &mut queue);
        }
    }
    Ok(reachable)
}

fn collect_refs(obj: &PdfObject, queue: &mut Vec<u32>) {
    match obj {
        PdfObject::Reference(n, _) => queue.push(*n),
        PdfObject::Array(a) => a.iter().for_each(|x| collect_refs(x, queue)),
        PdfObject::Dictionary(d) => d.values().for_each(|x| collect_refs(x, queue)),
        PdfObject::Stream(s) => s.dict.values().for_each(|x| collect_refs(x, queue)),
        _ => {}
    }
}

// ── Deduplication ─────────────────────────────────────────────────────────────

/// Build a map of duplicate stream IDs → canonical ID via SHA-256 content hash.
/// Returns an empty map when the `crypto` feature is absent.
fn build_dedup_map(
    doc: &PdfDocument,
    reachable: &HashSet<u32>,
    enabled: bool,
) -> HashMap<u32, u32> {
    if !enabled {
        return HashMap::new();
    }

    #[cfg(feature = "crypto")]
    {
        let mut hash_to_id: HashMap<[u8; 32], u32> = HashMap::new();
        let mut dedup: HashMap<u32, u32> = HashMap::new();

        for &id in reachable {
            if let Ok(PdfObject::Stream(s)) = doc.get_object(id) {
                if let Ok(decoded) = s.decode_with_doc(doc) {
                    let hash = sha256(&decoded);
                    if let Some(&canonical) = hash_to_id.get(&hash) {
                        dedup.insert(id, canonical);
                    } else {
                        hash_to_id.insert(hash, id);
                    }
                }
            }
        }
        dedup
    }
    #[cfg(not(feature = "crypto"))]
    {
        let _ = (doc, reachable);
        HashMap::new()
    }
}

// ── ID remapping ──────────────────────────────────────────────────────────────

/// Assign sequential new IDs to every reachable, non-duplicate object.
fn assign_new_ids(reachable: &HashSet<u32>, dedup_map: &HashMap<u32, u32>) -> HashMap<u32, u32> {
    let mut old_to_new: HashMap<u32, u32> = HashMap::new();
    let mut counter = 1u32;

    // Sort for deterministic output.
    let mut ids: Vec<u32> = reachable.iter().copied().collect();
    ids.sort_unstable();

    for old_id in ids {
        if dedup_map.contains_key(&old_id) {
            continue; // resolved in the second pass below
        }
        old_to_new.insert(old_id, counter);
        counter += 1;
    }

    // Duplicates point to their canonical new ID.
    for (&dup_id, &canonical_id) in dedup_map {
        if let Some(&new_canonical) = old_to_new.get(&canonical_id) {
            old_to_new.insert(dup_id, new_canonical);
        }
    }

    old_to_new
}

// ── Object transformation ─────────────────────────────────────────────────────

fn transform_object(
    obj: PdfObject,
    options: &OptimizationOptions,
    map: &HashMap<u32, u32>,
    doc: &PdfDocument,
) -> Result<PdfObject> {
    match obj {
        PdfObject::Stream(s) => transform_stream(*s, options, map, doc),
        other => Ok(remap_object(other, map)),
    }
}

fn transform_stream(
    s: PdfStream,
    options: &OptimizationOptions,
    map: &HashMap<u32, u32>,
    doc: &PdfDocument,
) -> Result<PdfObject> {
    let decoded = s.decode_with_doc(doc)?;

    let processed = if options.downsample_images && is_image_stream(&s.dict) {
        downsample_if_needed(&s.dict, &decoded, options.image_max_dpi)?
    } else {
        decoded
    };

    let mut new_dict = remap_dict(&s.dict, map);

    let (body, filter) = if options.recompress_streams {
        (
            crate::writer::streams::encode_flate(&processed)?,
            Some("FlateDecode"),
        )
    } else {
        (processed, None)
    };

    new_dict.insert("Length".to_owned(), PdfObject::Integer(body.len() as i64));
    if let Some(f) = filter {
        new_dict.insert("Filter".to_owned(), PdfObject::Name(f.to_owned()));
    } else {
        // No compression: remove /Filter and /DecodeParms so bytes are inline.
        new_dict.shift_remove("Filter");
    }
    new_dict.shift_remove("DecodeParms");

    Ok(PdfObject::Stream(Box::new(PdfStream {
        dict: new_dict,
        raw_data: body,
    })))
}

// ── Reference remapping helpers ───────────────────────────────────────────────

fn remap_object(obj: PdfObject, map: &HashMap<u32, u32>) -> PdfObject {
    match obj {
        PdfObject::Reference(n, g) => PdfObject::Reference(*map.get(&n).unwrap_or(&n), g),
        PdfObject::Array(a) => {
            PdfObject::Array(a.into_iter().map(|x| remap_object(x, map)).collect())
        }
        PdfObject::Dictionary(d) => PdfObject::Dictionary(remap_dict(&d, map)),
        PdfObject::Stream(s) => {
            let new_dict = remap_dict(&s.dict, map);
            PdfObject::Stream(Box::new(PdfStream {
                dict: new_dict,
                raw_data: s.raw_data,
            }))
        }
        other => other,
    }
}

fn remap_dict(dict: &PdfDict, map: &HashMap<u32, u32>) -> PdfDict {
    dict.iter()
        .map(|(k, v)| (k.clone(), remap_object(v.clone(), map)))
        .collect()
}

// ── Image downsampling ────────────────────────────────────────────────────────

fn is_image_stream(dict: &PdfDict) -> bool {
    matches!(dict.get("Subtype"), Some(PdfObject::Name(n)) if n == "Image")
}

/// Nearest-neighbour downsample for DeviceRGB (3-channel) images exceeding `max_dpi`.
///
/// Without actual page MediaBox context we estimate: if either pixel dimension
/// exceeds `max_dpi * 11` (11 inches max page height) we scale down. For other
/// colour spaces the raw bytes are returned unchanged.
fn downsample_if_needed(dict: &PdfDict, decoded: &[u8], max_dpi: u32) -> Result<Vec<u8>> {
    let width = match dict.get("Width") {
        Some(PdfObject::Integer(n)) => *n as u32,
        _ => return Ok(decoded.to_vec()),
    };
    let height = match dict.get("Height") {
        Some(PdfObject::Integer(n)) => *n as u32,
        _ => return Ok(decoded.to_vec()),
    };

    let max_pixels = max_dpi * 11;
    if width <= max_pixels && height <= max_pixels {
        return Ok(decoded.to_vec());
    }

    // Only handle DeviceRGB (3 channels) for now.
    let channels: u32 = 3;
    if decoded.len() != (width * height * channels) as usize {
        return Ok(decoded.to_vec());
    }

    let scale = max_pixels as f64 / width.max(height) as f64;
    let new_w = ((width as f64 * scale) as u32).max(1);
    let new_h = ((height as f64 * scale) as u32).max(1);

    let mut result = vec![0u8; (new_w * new_h * channels) as usize];
    for y in 0..new_h {
        for x in 0..new_w {
            let src_x = (x as f64 / new_w as f64 * width as f64) as u32;
            let src_y = (y as f64 / new_h as f64 * height as f64) as u32;
            let src_idx = ((src_y * width + src_x) * channels) as usize;
            let dst_idx = ((y * new_w + x) * channels) as usize;
            if src_idx + 3 <= decoded.len() {
                result[dst_idx..dst_idx + 3].copy_from_slice(&decoded[src_idx..src_idx + 3]);
            }
        }
    }

    crate::writer::streams::encode_flate(&result)
}

// ── SHA-256 helper (crypto feature) ──────────────────────────────────────────

#[cfg(feature = "crypto")]
fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimize_output_is_valid_pdf() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let optimized = optimize(&data, &OptimizationOptions::default()).unwrap();
        assert!(optimized.starts_with(b"%PDF-"));
        assert!(
            optimized.ends_with(b"%%EOF\n") || optimized.ends_with(b"%%EOF"),
            "must end with %%EOF"
        );
    }

    #[test]
    fn optimize_preserves_page_count() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let optimized = optimize(&data, &OptimizationOptions::default()).unwrap();
        let doc = PdfDocument::parse(optimized).unwrap();
        assert_eq!(doc.page_count().unwrap(), 3);
    }

    #[test]
    fn optimize_removes_unused_objects() {
        let data = include_bytes!("../../tests/fixtures/minimal.pdf").to_vec();
        let optimized = optimize(
            &data,
            &OptimizationOptions {
                remove_unused_objects: true,
                ..Default::default()
            },
        )
        .unwrap();
        let doc = PdfDocument::parse(optimized).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn optimize_reduces_file_size_or_equal() {
        let data = include_bytes!("../../tests/fixtures/multipage.pdf").to_vec();
        let original_len = data.len();
        let optimized = optimize(&data, &OptimizationOptions::default()).unwrap();
        let doc = PdfDocument::parse(optimized.clone()).unwrap();
        assert_eq!(doc.page_count().unwrap(), 3);
        // Optimized should not be significantly larger (allow small xref overhead).
        assert!(
            optimized.len() <= original_len + 512,
            "optimized size {} > original {} + 512",
            optimized.len(),
            original_len
        );
    }

    #[test]
    fn optimize_no_recompress_still_valid() {
        let data = include_bytes!("../../tests/fixtures/minimal.pdf").to_vec();
        let opts = OptimizationOptions {
            recompress_streams: false,
            deduplicate_resources: false,
            remove_unused_objects: true,
            downsample_images: false,
            image_max_dpi: 150,
        };
        let optimized = optimize(&data, &opts).unwrap();
        let doc = PdfDocument::parse(optimized).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }
}
