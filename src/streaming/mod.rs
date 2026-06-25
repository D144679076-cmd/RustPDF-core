//! Incremental / streaming PDF loading for large remote documents.
//!
//! # Usage
//! 1. Fetch the last 4 KB of the file → [`StreamingDocument::from_tail`]
//! 2. Call [`StreamingDocument::needed_ranges_for_page`] → get byte ranges to fetch
//! 3. Feed each fetched range with [`StreamingDocument::feed`]
//! 4. Repeat until [`StreamingDocument::page_ready`] returns `true`
//! 5. Call [`StreamingDocument::build_page_document`] to obtain a renderable `PdfDocument`

use std::collections::{HashMap, HashSet};

use crate::error::{PdfError, Result};
use crate::parser::lexer::Lexer;
use crate::parser::objects::{parse_indirect_object, parse_object_from_lexer, PdfDict, PdfObject};
use crate::parser::xref::find_startxref;

// ---------------------------------------------------------------------------
// ByteCache
// ---------------------------------------------------------------------------

/// Tracks which byte ranges of a remote file have been fetched.
///
/// Stores received chunks sorted by offset and merges overlapping regions so
/// that [`has_range`] / [`get`] work in a single linear scan.
pub struct ByteCache {
    total_len: u64,
    /// Sorted, non-overlapping chunks: `(start_offset, bytes)`.
    chunks: Vec<(u64, Vec<u8>)>,
}

impl ByteCache {
    /// Create an empty cache for a file of `total_len` bytes.
    pub fn new(total_len: u64) -> Self {
        Self {
            total_len,
            chunks: vec![],
        }
    }

    /// Insert a fetched byte range at `offset` into the cache.
    ///
    /// Overlapping or adjacent chunks are merged automatically.
    pub fn feed(&mut self, offset: u64, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        self.chunks.push((offset, data));
        self.chunks.sort_by_key(|(o, _)| *o);

        let mut merged: Vec<(u64, Vec<u8>)> = vec![];
        for (start, data) in std::mem::take(&mut self.chunks) {
            if let Some(last) = merged.last_mut() {
                let last_end = last.0 + last.1.len() as u64;
                if start <= last_end {
                    let overlap = (last_end.saturating_sub(start)) as usize;
                    if overlap < data.len() {
                        last.1.extend_from_slice(&data[overlap..]);
                    }
                    continue;
                }
            }
            merged.push((start, data));
        }
        self.chunks = merged;
    }

    /// Return a byte slice covering `[offset, offset+len)` if fully cached, else `None`.
    pub fn get(&self, offset: u64, len: usize) -> Option<&[u8]> {
        for (start, data) in &self.chunks {
            let end = start + data.len() as u64;
            if *start <= offset && offset + len as u64 <= end {
                let off = (offset - start) as usize;
                return Some(&data[off..off + len]);
            }
        }
        None
    }

    /// Returns `true` if the range `[offset, offset+len)` is fully cached.
    pub fn has_range(&self, offset: u64, len: usize) -> bool {
        self.get(offset, len).is_some()
    }

    /// Estimated byte length of the object whose header starts at `file_offset`.
    ///
    /// Uses the gap to the next known object offset in `xref_offsets` as an
    /// upper bound, capped at `MAX_OBJ_LEN`. Falls back to `MAX_OBJ_LEN` when
    /// no next offset exists.
    pub fn estimate_object_len(
        &self,
        file_offset: u64,
        sorted_offsets: &[u64],
        max_len: u64,
    ) -> u64 {
        let idx = sorted_offsets.partition_point(|&o| o <= file_offset);
        if idx < sorted_offsets.len() {
            (sorted_offsets[idx] - file_offset).min(max_len)
        } else {
            max_len.min(self.total_len.saturating_sub(file_offset))
        }
    }

    /// Total file length this cache was created for.
    pub fn total_len(&self) -> u64 {
        self.total_len
    }

    /// Iterate all cached chunks as `(start_offset, &[u8])`.
    pub fn chunks(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.chunks.iter().map(|(s, d)| (*s, d.as_slice()))
    }
}

// ---------------------------------------------------------------------------
// XRefData — parsed xref map + trailer
// ---------------------------------------------------------------------------

/// Holds the parsed cross-reference table and final trailer dictionary.
pub struct XRefData {
    /// Object ID → absolute file offset (type-1 / uncompressed objects only).
    pub offsets: HashMap<u32, u64>,
    /// The last trailer dictionary (contains `/Root`, `/Size`, etc.).
    pub trailer: PdfDict,
}

impl XRefData {
    /// Sort all known file offsets into a `Vec` for binary search.
    pub fn sorted_offsets(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.offsets.values().copied().collect();
        v.sort_unstable();
        v
    }
}

// ---------------------------------------------------------------------------
// StreamingDocument
// ---------------------------------------------------------------------------

/// Holds state for incremental streaming of a remote PDF.
///
/// Callers repeatedly call [`needed_ranges_for_page`] → fetch the returned
/// ranges → [`feed`] the data → check [`page_ready`].  Once ready, call
/// [`build_page_document`] to get a renderable [`PdfDocument`].
pub struct StreamingDocument {
    pub(crate) cache: ByteCache,
    /// The `startxref` value extracted from the file tail.
    pub(crate) xref_offset: u64,
    /// Parsed once the xref table bytes are available.
    pub(crate) xref: Option<XRefData>,
    pub(crate) total_len: u64,
}

/// Maximum bytes to fetch for an unknown-size object.
const MAX_OBJ_LEN: u64 = 65_536;
/// Initial fetch size for the xref table region.
const XREF_FETCH_LEN: u64 = 32_768;

impl StreamingDocument {
    /// Initialise from the last chunk of the remote file (typically 4096 bytes).
    ///
    /// `tail` must be the final `tail.len()` bytes of the file; `total_len`
    /// is the full file size in bytes (from `Content-Length`).
    /// Parses the `startxref` offset from the `%%EOF` trailer block.
    pub fn from_tail(tail: &[u8], total_len: u64) -> Result<Self> {
        let xref_offset = find_startxref(tail)
            .ok_or_else(|| PdfError::invalid_token(0, "startxref not found in file tail"))?;
        let tail_offset = total_len.saturating_sub(tail.len() as u64);
        let mut cache = ByteCache::new(total_len);
        cache.feed(tail_offset, tail.to_vec());

        let mut doc = Self {
            cache,
            xref_offset,
            xref: None,
            total_len,
        };
        // If the xref table happens to be in the tail, parse it now.
        doc.try_parse_xref();
        Ok(doc)
    }

    /// Feed a fetched byte range at `offset` into the cache.
    pub fn feed(&mut self, offset: u64, data: Vec<u8>) {
        self.cache.feed(offset, data);
        if self.xref.is_none() {
            self.try_parse_xref();
        }
    }

    /// Returns `true` when all bytes needed to render `page_index` are cached.
    pub fn page_ready(&self, page_index: usize) -> bool {
        self.needed_ranges_for_page(page_index).is_empty()
    }

    /// Returns byte ranges `(offset, length)` that must be fetched before
    /// `page_index` can be rendered.  Returns an empty `Vec` when ready.
    ///
    /// Callers should fetch each returned range and pass the data to [`feed`],
    /// then call this method again until the result is empty.
    pub fn needed_ranges_for_page(&self, page_index: usize) -> Vec<(u64, u64)> {
        // Stage 1: ensure the xref table bytes are cached.
        if self.xref.is_none() {
            let fetch_len = XREF_FETCH_LEN.min(self.total_len.saturating_sub(self.xref_offset));
            if !self.cache.has_range(self.xref_offset, fetch_len as usize) {
                return vec![(self.xref_offset, fetch_len)];
            }
            // Bytes are present but xref parse hasn't happened — caller
            // must call feed() to trigger the parse.
            return vec![];
        }

        // Stage 2+: xref is available, walk object dependency graph.
        let xref = self.xref.as_ref().unwrap();
        let sorted = xref.sorted_offsets();

        let needed_ids = match self.collect_page_object_ids(page_index) {
            Ok(ids) => ids,
            Err(_) => {
                // Cannot resolve page tree yet — return catalog range as seed.
                return self.catalog_seed_ranges(xref, &sorted);
            }
        };

        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for id in &needed_ids {
            let Some(&offset) = xref.offsets.get(id) else {
                continue;
            };
            let len = self.cache.estimate_object_len(offset, &sorted, MAX_OBJ_LEN);
            if !self.cache.has_range(offset, len as usize) {
                ranges.push((offset, len));
            }
        }
        ranges
    }

    /// Build a renderable [`crate::parser::objects::PdfDocument`] for `page_index`.
    ///
    /// Returns `Err` if the page is not yet ready — call [`page_ready`] first.
    pub fn build_page_document(
        &self,
        page_index: usize,
    ) -> Result<crate::parser::objects::PdfDocument> {
        if !self.page_ready(page_index) {
            return Err(PdfError::invalid_token(0, "page not yet fully loaded"));
        }
        let xref = self.xref.as_ref().unwrap();
        let needed_ids = self.collect_page_object_ids(page_index)?;
        let sorted = xref.sorted_offsets();

        // Build a minimal synthetic PDF containing only the required objects.
        let mut buf: Vec<u8> = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut new_offsets: HashMap<u32, u64> = HashMap::new();

        for &id in &needed_ids {
            let Some(&orig_offset) = xref.offsets.get(&id) else {
                continue;
            };
            let est_len = self
                .cache
                .estimate_object_len(orig_offset, &sorted, MAX_OBJ_LEN)
                as usize;
            let Some(obj_bytes) = self.cache.get(orig_offset, est_len) else {
                continue;
            };
            // Trim to actual `endobj` boundary.
            let trimmed = trim_to_endobj(obj_bytes);
            new_offsets.insert(id, buf.len() as u64);
            buf.extend_from_slice(trimmed);
            if !trimmed.ends_with(b"\n") {
                buf.push(b'\n');
            }
        }

        let xref_start = buf.len() as u64;
        write_xref_table(&mut buf, &new_offsets);
        write_trailer(&mut buf, &xref.trailer, xref_start);

        crate::parser::objects::PdfDocument::parse(buf)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Attempt to parse the xref table from currently cached bytes.
    fn try_parse_xref(&mut self) {
        let fetch_len =
            XREF_FETCH_LEN.min(self.total_len.saturating_sub(self.xref_offset)) as usize;
        let Some(xref_bytes) = self.cache.get(self.xref_offset, fetch_len) else {
            return;
        };
        // parse_xref needs the absolute position within the full file, so we
        // build a minimal slice starting at xref_offset filled with the cached bytes.
        // Since parse_xref validates offsets against file_size, we pass total_len.
        // We cheat by creating a view: the slice IS at offset 0 from parse_xref's
        // perspective, so we adjust all offsets by xref_offset.
        //
        // A simpler route: copy xref_bytes into a temporary vec prefixed with
        // `xref_offset` zeros so absolute offsets in entries remain valid.
        // For large xref_offset values this is wasteful; use offset 0 remapping.
        //
        // Instead, call parse_xref with the original full-file offset semantics:
        // build a scratch buf where position 0..=xref_offset-1 is zeroed and
        // position xref_offset.. contains the cached xref bytes.
        let needed = self.xref_offset as usize + fetch_len;
        if needed > 512 * 1024 * 1024 {
            // Xref offset unreasonably large — malformed tail.
            log::warn!(
                "[streaming] xref_offset {} too large, skipping parse",
                self.xref_offset
            );
            return;
        }
        let mut scratch = vec![0u8; needed];
        scratch[self.xref_offset as usize..].copy_from_slice(xref_bytes);

        match crate::parser::xref::parse_xref(&scratch, self.xref_offset as usize) {
            Ok(offsets) => {
                // Re-parse the trailer from the scratch buf.
                let trailer = parse_trailer_from_bytes(xref_bytes).unwrap_or_default();
                log::debug!(
                    "[streaming] xref parsed: {} objects, trailer keys: {:?}",
                    offsets.len(),
                    trailer.keys().collect::<Vec<_>>()
                );
                self.xref = Some(XRefData { offsets, trailer });
            }
            Err(e) => {
                log::warn!("[streaming] xref parse failed: {}", e);
            }
        }
    }

    /// Return the ranges for the catalog seed (when we can't walk the page tree yet).
    fn catalog_seed_ranges(&self, xref: &XRefData, sorted: &[u64]) -> Vec<(u64, u64)> {
        let root_id = match xref.trailer.get("Root") {
            Some(PdfObject::Reference(id, _)) => *id,
            _ => return vec![],
        };
        let Some(&offset) = xref.offsets.get(&root_id) else {
            return vec![];
        };
        let len = self.cache.estimate_object_len(offset, sorted, MAX_OBJ_LEN);
        if self.cache.has_range(offset, len as usize) {
            vec![]
        } else {
            vec![(offset, len)]
        }
    }

    /// BFS from the PDF catalog to collect all object IDs needed for `page_index`.
    fn collect_page_object_ids(&self, page_index: usize) -> Result<HashSet<u32>> {
        let xref = self
            .xref
            .as_ref()
            .ok_or_else(|| PdfError::invalid_token(0, "xref not yet parsed"))?;
        let sorted = xref.sorted_offsets();

        let root_id = match xref.trailer.get("Root") {
            Some(PdfObject::Reference(id, _)) => *id,
            _ => {
                return Err(PdfError::invalid_token(
                    0,
                    "trailer missing /Root reference",
                ))
            }
        };

        let mut visited: HashSet<u32> = HashSet::new();
        let mut queue: Vec<u32> = vec![root_id];
        let mut page_counter = 0usize;
        let mut found_page = false;

        while let Some(id) = queue.first().copied() {
            queue.remove(0);
            if !visited.insert(id) {
                continue;
            }

            let Some(&offset) = xref.offsets.get(&id) else {
                continue;
            };
            let est_len = self.cache.estimate_object_len(offset, &sorted, MAX_OBJ_LEN) as usize;
            let Some(obj_bytes) = self.cache.get(offset, est_len) else {
                return Err(PdfError::eof(
                    offset as usize,
                    "object bytes not yet cached",
                ));
            };

            let obj = parse_indirect_object(obj_bytes, 0)?;
            let dict = match obj.as_dict() {
                Some(d) => d.clone(),
                None => continue,
            };

            // Walk the page tree, stop once we reach page_index.
            match dict.get("Type").and_then(|o| o.as_name()) {
                Some("Pages") => {
                    // Pages node: descend into /Kids
                    if let Some(PdfObject::Array(kids)) = dict.get("Kids") {
                        for kid in kids {
                            if let PdfObject::Reference(kid_id, _) = kid {
                                queue.push(*kid_id);
                            }
                        }
                    }
                }
                Some("Page") => {
                    if page_counter == page_index {
                        found_page = true;
                        // Collect all direct resource refs from this page.
                        collect_all_refs(
                            &PdfObject::Dictionary(dict.clone()),
                            &mut visited,
                            &mut queue,
                        );
                        // Stop enqueuing further pages.
                        queue.retain(|qid| visited.contains(qid) || xref.offsets.contains_key(qid));
                    } else {
                        page_counter += 1;
                    }
                    if found_page {
                        // Still need to drain the queue to gather resource objects.
                    }
                }
                _ => {
                    // Non-page object: collect any refs it contains (resources, fonts, etc.)
                    collect_all_refs(&PdfObject::Dictionary(dict), &mut visited, &mut queue);
                }
            }
        }

        if !found_page && page_index > 0 {
            return Err(PdfError::invalid_token(
                0,
                format!("page {} not found in page tree", page_index),
            ));
        }

        Ok(visited)
    }
}

// ---------------------------------------------------------------------------
// Helpers: object ref collection
// ---------------------------------------------------------------------------

/// Recursively enqueue all indirect object IDs found in `obj`.
fn collect_all_refs(obj: &PdfObject, visited: &mut HashSet<u32>, queue: &mut Vec<u32>) {
    match obj {
        PdfObject::Reference(id, _) if !visited.contains(id) => {
            queue.push(*id);
        }
        PdfObject::Reference(_, _) => {}
        PdfObject::Dictionary(d) => {
            for v in d.values() {
                collect_all_refs(v, visited, queue);
            }
        }
        PdfObject::Array(arr) => {
            for item in arr {
                collect_all_refs(item, visited, queue);
            }
        }
        PdfObject::Stream(s) => {
            for v in s.dict.values() {
                collect_all_refs(v, visited, queue);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Helpers: synthetic PDF assembly
// ---------------------------------------------------------------------------

/// Trim `data` to just past the first `endobj` keyword.
fn trim_to_endobj(data: &[u8]) -> &[u8] {
    let marker = b"endobj";
    if let Some(pos) = find_bytes(data, marker) {
        &data[..pos + marker.len()]
    } else {
        data
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Write a traditional xref table to `buf` from `offsets`.
fn write_xref_table(buf: &mut Vec<u8>, offsets: &HashMap<u32, u64>) {
    if offsets.is_empty() {
        buf.extend_from_slice(b"xref\n0 0\n");
        return;
    }

    let mut ids: Vec<u32> = offsets.keys().copied().collect();
    ids.sort_unstable();

    buf.extend_from_slice(b"xref\n");
    // Write as contiguous subsection(s).
    let mut subsec_start = ids[0];
    let mut subsec: Vec<u32> = vec![ids[0]];

    let flush_subsec = |buf: &mut Vec<u8>, start: u32, ids: &[u32], offsets: &HashMap<u32, u64>| {
        let line = format!("{} {}\n", start, ids.len());
        buf.extend_from_slice(line.as_bytes());
        for &id in ids {
            let offset = offsets.get(&id).copied().unwrap_or(0);
            let entry = format!("{:010} 00000 n \r\n", offset);
            buf.extend_from_slice(entry.as_bytes());
        }
    };

    for &id in &ids[1..] {
        if id == *subsec.last().unwrap() + 1 {
            subsec.push(id);
        } else {
            flush_subsec(buf, subsec_start, &subsec, offsets);
            subsec_start = id;
            subsec = vec![id];
        }
    }
    flush_subsec(buf, subsec_start, &subsec, offsets);
}

/// Write a minimal trailer to `buf`.
fn write_trailer(buf: &mut Vec<u8>, trailer: &PdfDict, xref_start: u64) {
    buf.extend_from_slice(b"trailer\n<< ");

    // Always include /Size and /Root.
    if let Some(size) = trailer.get("Size") {
        write_pdf_obj(buf, "Size", size);
    }
    if let Some(root) = trailer.get("Root") {
        write_pdf_obj(buf, "Root", root);
    }
    if let Some(info) = trailer.get("Info") {
        write_pdf_obj(buf, "Info", info);
    }

    buf.extend_from_slice(b">>\nstartxref\n");
    buf.extend_from_slice(xref_start.to_string().as_bytes());
    buf.extend_from_slice(b"\n%%EOF\n");
}

fn write_pdf_obj(buf: &mut Vec<u8>, key: &str, val: &PdfObject) {
    buf.push(b'/');
    buf.extend_from_slice(key.as_bytes());
    buf.push(b' ');
    match val {
        PdfObject::Reference(id, gen) => {
            let s = format!("{} {} R ", id, gen);
            buf.extend_from_slice(s.as_bytes());
        }
        PdfObject::Integer(n) => {
            buf.extend_from_slice(n.to_string().as_bytes());
            buf.push(b' ');
        }
        PdfObject::Name(n) => {
            buf.push(b'/');
            buf.extend_from_slice(n.as_bytes());
            buf.push(b' ');
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Helpers: trailer extraction
// ---------------------------------------------------------------------------

/// Extract the trailer dictionary from raw xref section bytes (best-effort).
fn parse_trailer_from_bytes(data: &[u8]) -> Option<PdfDict> {
    let marker = b"trailer";
    let pos = data.windows(marker.len()).position(|w| w == marker)?;
    let after = &data[pos + marker.len()..];
    let mut lexer = Lexer::new(after);
    match parse_object_from_lexer(&mut lexer) {
        Ok(PdfObject::Dictionary(d)) => Some(d),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cache(total: u64, offset: u64, data: &[u8]) -> ByteCache {
        let mut c = ByteCache::new(total);
        c.feed(offset, data.to_vec());
        c
    }

    #[test]
    fn byte_cache_basic_get() {
        let mut c = ByteCache::new(100);
        c.feed(10, vec![1, 2, 3, 4, 5]);
        assert_eq!(c.get(10, 5), Some(&[1u8, 2, 3, 4, 5][..]));
        assert_eq!(c.get(12, 2), Some(&[3u8, 4][..]));
        assert!(c.get(9, 1).is_none());
        assert!(c.get(15, 1).is_none());
    }

    #[test]
    fn byte_cache_merge_overlapping() {
        let mut c = ByteCache::new(100);
        c.feed(0, vec![0, 1, 2, 3]);
        c.feed(2, vec![2, 3, 4, 5]);
        // Should merge into one chunk [0,1,2,3,4,5]
        assert_eq!(c.chunks().count(), 1);
        assert_eq!(c.get(0, 6), Some(&[0u8, 1, 2, 3, 4, 5][..]));
    }

    #[test]
    fn byte_cache_adjacent_merge() {
        let mut c = ByteCache::new(100);
        c.feed(0, vec![0, 1]);
        c.feed(2, vec![2, 3]);
        // Adjacent feeds are merged into a contiguous chunk.
        assert_eq!(c.chunks().count(), 1);
        assert_eq!(c.get(0, 4), Some(&[0u8, 1, 2, 3][..]));
    }

    #[test]
    fn byte_cache_has_range() {
        let c = make_cache(100, 10, &[0u8; 20]);
        assert!(c.has_range(10, 20));
        assert!(c.has_range(15, 5));
        assert!(!c.has_range(9, 1));
        assert!(!c.has_range(29, 2));
    }

    #[test]
    fn estimate_object_len_uses_next_offset() {
        let c = ByteCache::new(10_000);
        let sorted = vec![100u64, 500, 900];
        assert_eq!(c.estimate_object_len(100, &sorted, 1000), 400);
        assert_eq!(c.estimate_object_len(500, &sorted, 1000), 400);
        assert_eq!(c.estimate_object_len(900, &sorted, 1000), 1000); // hits cap
    }

    #[test]
    fn find_bytes_finds_marker() {
        let data = b"hello endobj world";
        assert_eq!(find_bytes(data, b"endobj"), Some(6));
        assert!(find_bytes(data, b"notfound").is_none());
    }

    #[test]
    fn trim_to_endobj_keeps_marker() {
        let data = b"1 0 obj << /Type /Page >> endobj extra stuff";
        let trimmed = trim_to_endobj(data);
        assert!(trimmed.ends_with(b"endobj"));
        assert!(!trimmed.ends_with(b"extra stuff"));
    }
}
