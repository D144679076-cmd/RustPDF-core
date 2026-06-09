//! `PdfWriter` — object pool and full document serialization.

use crate::error::Result;
use crate::parser::objects::PdfObject;
use crate::writer::serializer::write_indirect;
use crate::writer::xref::{build_trailer_dict, write_full_xref_and_trailer};

/// PDF file header written at the start of every new (non-incremental) document.
///
/// The binary comment `%` + four high bytes signals to file-transfer tools
/// that this is a binary file (ISO 32000-1 §7.5.2).
const PDF_HEADER: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";

/// An object held in the writer pool.
#[derive(Debug, Clone)]
struct PoolEntry {
    id: u32,
    gen: u32,
    obj: PdfObject,
}

/// An opaque snapshot of a [`PdfWriter`]'s mutable state.
///
/// Produced by [`PdfWriter::snapshot`] and consumed by
/// [`PdfWriter::restore`]; the editor's undo/redo stack stores these without
/// needing to know the pool's internal representation.
#[derive(Debug, Clone)]
pub struct PoolSnapshot {
    pool: Vec<PoolEntry>,
    next_id: u32,
}

/// PDF document writer.
///
/// Accumulates objects and serializes them into a valid PDF byte stream.
/// Supports both fresh documents and incremental updates.
#[derive(Debug)]
pub struct PdfWriter {
    pool: Vec<PoolEntry>,
    next_id: u32,
    /// Monotonic mutation counter, bumped on every `add_object` / `set_object`.
    ///
    /// Used as a robust cache-invalidation key by callers that derive state
    /// from the pool (e.g. the WASM text-edit model). Unlike `len()`, this is
    /// correct even when `set_object` replaces an entry without growing the
    /// pool, and even if a future undo restores the pool to a prior length.
    generation: u64,
}

impl PdfWriter {
    /// Create a writer for a brand-new document (object IDs start at 1).
    pub fn new() -> Self {
        Self {
            pool: Vec::new(),
            next_id: 1,
            generation: 0,
        }
    }

    /// Create a writer for an incremental update.
    ///
    /// Object IDs start at `max_existing_id + 1` so they never collide
    /// with objects already in the original file.
    pub fn new_from_max_id(max_existing_id: u32) -> Self {
        Self {
            pool: Vec::new(),
            next_id: max_existing_id + 1,
            generation: 0,
        }
    }

    /// Allocate an object ID without storing an object yet.
    pub fn reserve_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Store a new object in the pool and return its assigned ID.
    pub fn add_object(&mut self, obj: PdfObject) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.pool.push(PoolEntry { id, gen: 0, obj });
        self.generation += 1;
        id
    }

    /// Replace or insert an object with a specific ID (used for shadowing
    /// existing objects in incremental-update mode).
    pub fn set_object(&mut self, id: u32, obj: PdfObject) {
        self.generation += 1;
        // If already queued, replace it.
        if let Some(entry) = self.pool.iter_mut().find(|e| e.id == id) {
            entry.obj = obj;
            return;
        }
        self.pool.push(PoolEntry { id, gen: 0, obj });
        // Keep next_id beyond any explicit ID we store.
        if id >= self.next_id {
            self.next_id = id + 1;
        }
    }

    /// Retrieve an object from the pool by ID, if present.
    pub fn get_object(&self, id: u32) -> Option<&PdfObject> {
        self.pool.iter().find(|e| e.id == id).map(|e| &e.obj)
    }

    /// Number of objects currently in the pool.
    pub fn len(&self) -> usize {
        self.pool.len()
    }

    /// True if the pool has no objects.
    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    /// Return all object IDs currently in the pool, sorted ascending.
    pub fn all_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.pool.iter().map(|e| e.id).collect();
        ids.sort();
        ids
    }

    /// Monotonic mutation counter for cache invalidation.
    ///
    /// Increments on every `add_object` and `set_object`. A caller can cache
    /// derived state keyed by this value and rebuild only when it changes —
    /// correct across replacements and (future) undo, unlike pool length.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Capture an opaque snapshot of the current mutable state (pool +
    /// allocation cursor) for undo/redo. Does not affect the generation.
    pub fn snapshot(&self) -> PoolSnapshot {
        PoolSnapshot {
            pool: self.pool.clone(),
            next_id: self.next_id,
        }
    }

    /// Restore a previously captured [`PoolSnapshot`].
    ///
    /// Bumps the generation so any cache keyed on it (e.g. the WASM text-edit
    /// model) invalidates — necessary because a restored pool may have a
    /// length that was already observed before.
    pub fn restore(&mut self, snapshot: PoolSnapshot) {
        self.pool = snapshot.pool;
        self.next_id = snapshot.next_id;
        self.generation += 1;
    }

    /// Serialize all pooled objects and build a complete PDF (or incremental section).
    ///
    /// # Parameters
    ///
    /// - `root_id` — object number of the Catalog (`/Root`).
    /// - `info_id` — optional object number of the Info dictionary.
    /// - `prev_xref_offset` — if `Some(n)`, this is an incremental update;
    ///   the PDF header is **not** written and `/Prev n` is added to the trailer.
    ///   If `None`, a fresh PDF header is prepended.
    pub fn serialize_all(
        &mut self,
        root_id: u32,
        info_id: Option<u32>,
        prev_xref_offset: Option<u64>,
    ) -> Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();

        // Only new (non-incremental) documents get the PDF header.
        if prev_xref_offset.is_none() {
            out.extend_from_slice(PDF_HEADER);
        }

        let mut offsets: Vec<(u32, u64)> = Vec::new();

        // Serialize objects in ID order for reproducibility.
        let mut sorted: Vec<&PoolEntry> = self.pool.iter().collect();
        sorted.sort_by_key(|e| e.id);

        for entry in sorted {
            write_indirect(entry.id, entry.gen, &entry.obj, &mut out, &mut offsets);
        }

        // Determine /Size: max object ID across what we wrote + 1.
        let max_id = offsets.iter().map(|(id, _)| *id).max().unwrap_or(0);
        // For incremental updates the original file may have larger IDs;
        // callers are responsible for passing the correct value via the
        // editor layer. Here we just use what we know.
        let size = max_id + 1;

        let xref_start = out.len() as u64;
        let trailer = build_trailer_dict(size, root_id, info_id, prev_xref_offset);
        write_full_xref_and_trailer(&offsets, &trailer, xref_start, &mut out);

        Ok(out)
    }
}

impl Default for PdfWriter {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut writer = PdfWriter::new();

        // Pages node
        let mut pages = PdfDict::new();
        pages.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        pages.insert("Kids".to_owned(), PdfObject::Array(vec![]));
        pages.insert("Count".to_owned(), PdfObject::Integer(0));
        let pages_id = writer.add_object(PdfObject::Dictionary(pages));

        // Catalog
        let mut catalog = PdfDict::new();
        catalog.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        catalog.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        let cat_id = writer.add_object(PdfObject::Dictionary(catalog));

        writer.serialize_all(cat_id, None, None).unwrap()
    }

    #[test]
    fn fresh_pdf_has_header() {
        let bytes = minimal_pdf_bytes();
        assert!(bytes.starts_with(b"%PDF-1.7"));
    }

    #[test]
    fn fresh_pdf_parseable() {
        let bytes = minimal_pdf_bytes();
        let doc = PdfDocument::parse(bytes).expect("should parse");
        assert_eq!(doc.page_count().unwrap(), 0);
    }

    #[test]
    fn incremental_section_has_no_header() {
        let mut writer = PdfWriter::new_from_max_id(10);
        let mut dict = PdfDict::new();
        dict.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        dict.insert("Pages".to_owned(), PdfObject::Reference(2, 0));
        writer.set_object(1, PdfObject::Dictionary(dict));
        let bytes = writer.serialize_all(1, None, Some(500)).unwrap();
        assert!(!bytes.starts_with(b"%PDF"));
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("/Prev 500"));
    }

    #[test]
    fn set_object_shadows_previous() {
        let mut writer = PdfWriter::new();
        writer.add_object(PdfObject::Integer(1)); // id 1
        writer.set_object(1, PdfObject::Integer(99));
        assert_eq!(writer.get_object(1), Some(&PdfObject::Integer(99)));
        assert_eq!(writer.len(), 1);
    }

    #[test]
    fn reserve_id_increments() {
        let mut writer = PdfWriter::new();
        let a = writer.reserve_id();
        let b = writer.reserve_id();
        assert_eq!(b, a + 1);
    }

    #[test]
    fn generation_bumps_on_add_and_set() {
        let mut writer = PdfWriter::new();
        assert_eq!(writer.generation(), 0);

        // add_object bumps.
        writer.add_object(PdfObject::Integer(1)); // id 1
        let g1 = writer.generation();
        assert_eq!(g1, 1);

        // set_object that *replaces* an existing entry bumps even though the
        // pool length does not change — this is the case the old pool-length
        // cache key missed.
        let len_before = writer.len();
        writer.set_object(1, PdfObject::Integer(99));
        assert_eq!(writer.len(), len_before, "replacement must not grow pool");
        assert!(
            writer.generation() > g1,
            "generation must advance on in-place set_object"
        );

        // set_object that inserts a new id also bumps.
        let g2 = writer.generation();
        writer.set_object(5, PdfObject::Integer(7));
        assert!(writer.generation() > g2);
    }

    #[test]
    fn snapshot_and_restore_roundtrip() {
        let mut writer = PdfWriter::new();
        writer.add_object(PdfObject::Integer(1)); // id 1
        writer.add_object(PdfObject::Integer(2)); // id 2
        let snap = writer.snapshot();
        let gen_at_snapshot = writer.generation();

        // Mutate past the snapshot.
        writer.add_object(PdfObject::Integer(3)); // id 3
        assert_eq!(writer.len(), 3);

        // Restore: pool length returns to 2, generation advances (so caches
        // keyed on generation rebuild rather than wrongly reusing stale state).
        writer.restore(snap);
        assert_eq!(writer.len(), 2);
        assert_eq!(writer.get_object(3), None);
        assert!(writer.generation() > gen_at_snapshot);

        // The allocation cursor is restored too: the next add reuses id 3.
        let id = writer.add_object(PdfObject::Integer(4));
        assert_eq!(id, 3);
    }

    #[test]
    fn offsets_in_xref_match_actual_positions() {
        let bytes = minimal_pdf_bytes();
        // Locate "1 0 obj" in the output
        let marker = b"1 0 obj";
        let pos = bytes
            .windows(marker.len())
            .position(|w| w == marker)
            .expect("object 1 not found");
        // The xref should contain the correct offset for object 1
        let s = String::from_utf8_lossy(&bytes);
        let offset_str = format!("{:010}", pos);
        assert!(
            s.contains(&offset_str),
            "xref offset {} not found in output",
            pos
        );
    }
}
