//! `PdfWriter` — object pool and full document serialization.

use crate::document::associated_files::AfRelationship;
use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::serializer::write_indirect;
use crate::writer::xref::{build_trailer_dict, write_full_xref_and_trailer};

/// PDF file header written at the start of every new (non-incremental) document.
///
/// The binary comment `%` + four high bytes signals to file-transfer tools
/// that this is a binary file (ISO 32000-1 §7.5.2).
const PDF_HEADER: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";

/// PDF 2.0 header (ISO 32000-2). Same binary comment convention.
const PDF_HEADER_2_0: &[u8] = b"%PDF-2.0\n%\xE2\xE3\xCF\xD3\n";

/// Which PDF version the writer emits.
///
/// `V1_7` (default) keeps byte-identical output for every existing user of
/// [`PdfWriter`]. `V2_0` opts into the PDF 2.0 emission policy (Phase E):
///
/// - header `%PDF-2.0`
/// - catalog object gets `/Version 2.0` when the caller hasn't set a version
/// - every `/Type /Filespec` object gets its required `/UF` (Unicode file
///   name, §7.11.3 Table 42) autofilled from `/F` when missing
/// - RC4-shaped `/Encrypt` dictionaries in the pool are rejected outright —
///   ISO 32000-2 removed RC4; a PDF 2.0 file must never ship one
/// - unchanged: object layout, compression, e.g. everything else emits as 1.7
///
/// Setting `/Version` on the *catalog* (vs the header) follows the spec: the
/// header is the reader-facing visual, the catalog keys the feature set for
/// conforming readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitVersion {
    /// PDF 1.7 header + 1.7-flavoured output (back-compat default).
    #[default]
    V1_7,
    /// PDF 2.0 emission policy (ISO 32000-2). See the struct list above.
    V2_0,
}

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
    /// Which PDF version header to emit. Defaults to 1.7 (every existing
    /// caller/test/golden expectation); `V2_0` opts into the ISO 32000-2
    /// emission policy — see [`EmitVersion`].
    emit_version: EmitVersion,
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
            emit_version: EmitVersion::default(),
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
            emit_version: EmitVersion::default(),
            generation: 0,
        }
    }

    /// Opt this writer into the PDF 2.0 emission policy (ISO 32000-2).
    ///
    /// Default output stays PDF 1.7 — see [`EmitVersion`] for everything the
    /// opt-in changes. Incremental updates don't get a header (one is present
    /// in the base file), but the pool-level normalization (`/Version`,
    /// `/UF` autofill, RC4 reject) still applies at `serialize_all` time.
    pub fn set_emit_version(&mut self, version: EmitVersion) {
        self.emit_version = version;
    }

    /// The writer's current emission version.
    pub fn emit_version(&self) -> EmitVersion {
        self.emit_version
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

    /// Attach a data payload as a PDF 2.0 Associated File (§7.11.3/§7.11.4).
    ///
    /// Creates an `/EmbeddedFile` stream (FlateDecode-compressed) plus a
    /// `/Type /Filespec` dict with the required `/UF` Unicode name, an ASCII
    /// `/F` fallback, the given `/AFRelationship`, and `/EF → F → stream`.
    /// Returns the file-spec object ID — call it before creating the Catalog
    /// and reference the returned ID in the catalog's `/AF` array:
    ///
    /// ```ignore
    /// let spec_id = writer.add_associated_file(AfRelationship::Data, "source.csv", &bytes)?;
    /// // catalog dict: /AF [spec_id 0 R]
    /// ```
    ///
    /// `/Params` carries `/Size` (uncompressed size). `/ModDate`/`/CreationDate`
    /// are intentionally omitted for deterministic output (writer round-trips
    /// and golden tests) — the editor layer adds dates when the product needs them.
    pub fn add_associated_file(
        &mut self,
        relationship: AfRelationship,
        name: &str,
        data: &[u8],
    ) -> Result<u32> {
        // EmbeddedFile stream: FlateDecode keeps the pool small for
        // multi-MB payloads; /Size reflects the uncompressed length (Table 44).
        let mut params: PdfDict = PdfDict::new();
        params.insert(
            "Size".to_owned(),
            PdfObject::Integer(i64::try_from(data.len()).map_err(|_| {
                crate::error::PdfError::write_error("embedded file larger than i64::MAX")
            })?),
        );
        let mut ef_dict_extra: PdfDict = PdfDict::new();
        ef_dict_extra.insert(
            "Type".to_owned(),
            PdfObject::Name("EmbeddedFile".to_owned()),
        );
        ef_dict_extra.insert("Params".to_owned(), PdfObject::Dictionary(params));
        let stream = super::streams::make_flate_stream(data, ef_dict_extra)?;
        let stream_id = self.add_object(PdfObject::Stream(Box::new(stream)));

        // File names are text strings (§7.11.3 Table 42): /UF carries Unicode
        // (UTF-16BE + BOM when non-ASCII), /F keeps an ASCII-safe fallback so
        // 1.x-flavored readers/extractors still see a stable name.
        let mut ef_ref: PdfDict = PdfDict::new();
        ef_ref.insert("F".to_owned(), PdfObject::Reference(stream_id, 0));
        let mut spec: PdfDict = PdfDict::new();
        spec.insert("Type".to_owned(), PdfObject::Name("Filespec".to_owned()));
        spec.insert(
            "F".to_owned(),
            PdfObject::String(ascii_fallback(name).into_bytes()),
        );
        spec.insert(
            "UF".to_owned(),
            PdfObject::String(unicode_text_string(name)),
        );
        spec.insert(
            "AFRelationship".to_owned(),
            PdfObject::Name(relationship.as_name().to_owned()),
        );
        spec.insert("EF".to_owned(), PdfObject::Dictionary(ef_ref));

        Ok(self.add_object(PdfObject::Dictionary(spec)))
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
        self.serialize_all_impl(root_id, info_id, prev_xref_offset, None, None)
    }

    /// Serialize a fully rewritten *encrypted* document.
    ///
    /// Same as [`serialize_all`] but the trailer gains `/Encrypt` (pointing
    /// at `encrypt_ref`, an already-pooled `/Encrypt` dictionary object) and
    /// `/ID` (`file_id`, required for key derivation). The pool must already
    /// be encrypted — see [`crate::writer::encrypt`]. `/ID` must also match
    /// what was used to build the `/Encrypt` entries.
    pub fn serialize_all_encrypted(
        &mut self,
        root_id: u32,
        info_id: Option<u32>,
        encrypt_ref: u32,
        file_id: &[u8; 16],
    ) -> Result<Vec<u8>> {
        self.serialize_all_impl(
            root_id,
            info_id,
            None,
            Some(encrypt_ref),
            Some(file_id.to_vec()),
        )
    }

    fn serialize_all_impl(
        &mut self,
        root_id: u32,
        info_id: Option<u32>,
        prev_xref_offset: Option<u64>,
        encrypt_ref: Option<u32>,
        file_id: Option<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        // PDF 2.0 emission policy pre-pass (Phase E, plan doc §Phase E):
        // header choice + catalog /Version + /UF autofill + RC4 reject all
        // happen here so the *whole* document is consistent, not just the
        // header bytes.
        if self.emit_version == EmitVersion::V2_0 {
            self.apply_pdf2_policy(root_id)?;
        }

        let mut out: Vec<u8> = Vec::new();

        // Only new (non-incremental) documents get the PDF header.
        if prev_xref_offset.is_none() {
            let header = match self.emit_version {
                EmitVersion::V2_0 => PDF_HEADER_2_0,
                EmitVersion::V1_7 => PDF_HEADER,
            };
            out.extend_from_slice(header);
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
        let mut trailer = build_trailer_dict(size, root_id, info_id, prev_xref_offset);
        if let Some(enc_id) = encrypt_ref {
            trailer.insert("Encrypt".to_owned(), PdfObject::Reference(enc_id, 0));
        }
        if let Some(id) = file_id {
            trailer.insert(
                "ID".to_owned(),
                PdfObject::Array(vec![PdfObject::String(id.clone()), PdfObject::String(id)]),
            );
        }
        write_full_xref_and_trailer(&offsets, &trailer, xref_start, &mut out);

        Ok(out)
    }

    /// PDF 2.0 pool normalization (runs before serialization when the writer
    /// emits 2.0): catalog `/Version`, required `/UF` on every FileSpec, RC4
    /// rejection. Fails closed — a policy violation is a `WriteError`, not a
    /// silent downgrade to half-2.0 output.
    fn apply_pdf2_policy(&mut self, root_id: u32) -> Result<()> {
        // RC4 must never ship inside a PDF 2.0 file (ISO 32000-2 removed it;
        // see the reader-side policy gate in `crypto/handler.rs`).
        for entry in &self.pool {
            if let Some(dict) = obj_dict_of(&entry.obj) {
                if dict_is_rc4_encrypt(dict) {
                    return Err(PdfError::write_error(format!(
                        "PDF 2.0 writer forbids RC4 encryption dictionaries \
                         (object {} uses V<=3 Standard security handler)",
                        entry.id
                    )));
                }
            }
        }

        for entry in self.pool.iter_mut() {
            let Some(dict) = obj_dict_of_mut(&mut entry.obj) else {
                continue;
            };

            if entry.id == root_id {
                // Catalog: default the /Version key when the caller hasn't
                // declared one; warn (don't fight) on a caller-set mismatch.
                match dict.get("Version") {
                    None => {
                        dict.insert("Version".to_owned(), PdfObject::Name("2.0".to_owned()));
                    }
                    Some(PdfObject::Name(v)) if v == "2.0" => {}
                    Some(other) => {
                        log::warn!(
                            "PDF 2.0 writer: catalog /Version is {:?} — left as set",
                            other
                        );
                    }
                }
                continue;
            }

            // Filespec (/Type /Filespec): /UF is required in PDF 2.0 files.
            // `add_associated_file` always sets it; manually-built specs get
            // an ASCII-safe autofill from /F (Table 42: /UF is a text string).
            let is_filespec =
                matches!(dict.get("Type"), Some(PdfObject::Name(t)) if t == "Filespec");
            if is_filespec && !dict.contains_key("UF") {
                match dict.get("F") {
                    Some(PdfObject::String(f)) => {
                        log::warn!(
                            "PDF 2.0 writer: Filespec (object {}) missing /UF — \
                             autofilled from /F",
                            entry.id
                        );
                        dict.insert("UF".to_owned(), PdfObject::String(f.clone()));
                    }
                    _ => {
                        return Err(PdfError::write_error(format!(
                            "PDF 2.0 writer: Filespec (object {}) has neither /UF nor /F",
                            entry.id
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Default for PdfWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Borrow the dict inside a pool object (dictionaries and stream dicts).
fn obj_dict_of(obj: &PdfObject) -> Option<&PdfDict> {
    match obj {
        PdfObject::Dictionary(d) => Some(d),
        PdfObject::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

/// Mutable variant of [`obj_dict_of`].
fn obj_dict_of_mut(obj: &mut PdfObject) -> Option<&mut PdfDict> {
    match obj {
        PdfObject::Dictionary(d) => Some(d),
        PdfObject::Stream(s) => Some(&mut s.dict),
        _ => None,
    }
}

/// True when a dictionary *is* the Standard security handler with RC4 (V≤3).
///
/// Writer pools don't normally carry `/Encrypt` dicts (the trailer layer
/// owns that), but a caller that stuffs one in gets rejected under the
/// PDF 2.0 emission policy rather than silently written.
fn dict_is_rc4_encrypt(dict: &PdfDict) -> bool {
    let is_standard_filter =
        matches!(dict.get("Filter"), Some(PdfObject::Name(n)) if n == "Standard");
    if !is_standard_filter {
        return false;
    }
    match (
        dict.get("V").and_then(|v| v.as_integer()),
        dict.get("R").and_then(|r| r.as_integer()),
    ) {
        (Some(v), Some(r)) => v <= 3 && r < 5,
        _ => false,
    }
}

/// File-name in `/F` must be PDFDocEncoding-safe; drop non-ASCII bytes to `_`.
fn ascii_fallback(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// File-name in `/UF` is a text string: PDFDocEncoding bytes when ASCII
/// (nearly all ASCII = valid UTF-8, and 1.x readers read /UF as a file name
/// string), UTF-16BE + BOM otherwise.
fn unicode_text_string(name: &str) -> Vec<u8> {
    if name.is_ascii() {
        name.as_bytes().to_vec()
    } else {
        let mut out = Vec::with_capacity(2 + name.len() * 2);
        out.extend_from_slice(&[0xFE, 0xFF]);
        for unit in name.encode_utf16() {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        out
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

    // --- PDF 2.0 emission policy (Phase E) --------------------------------------

    #[test]
    fn pdf2_optin_emits_2_0_header_and_version() {
        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);

        let pages_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
            d.insert("Kids".to_owned(), PdfObject::Array(vec![]));
            d.insert("Count".to_owned(), PdfObject::Integer(0));
            d
        }));
        let cat_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d
        }));
        let bytes = writer.serialize_all(cat_id, None, None).unwrap();

        assert!(bytes.starts_with(b"%PDF-2.0"));
        // Round-trip: reader reports V2_0 from header, catalog /Version set.
        let doc = PdfDocument::parse(bytes).unwrap();
        assert_eq!(doc.version(), crate::parser::version::PdfVersion::V2_0);
        let cat = doc
            .resolve(&doc.trailer.get("Root").unwrap().clone())
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        assert_eq!(
            cat.get("Version"),
            Some(&PdfObject::Name("2.0".to_owned())),
            "writer must default catalog /Version to 2.0"
        );
    }

    #[test]
    fn default_writer_stays_1_7() {
        // No opt-in → byte-compatible 1.7 output, NO catalog /Version injection.
        let bytes = minimal_pdf_bytes();
        assert!(bytes.starts_with(b"%PDF-1.7"));
        let doc = PdfDocument::parse(bytes).unwrap();
        let cat = doc
            .resolve(&doc.trailer.get("Root").unwrap().clone())
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        assert!(cat.get("Version").is_none());
    }

    #[test]
    fn pdf2_filespec_gets_uf_autofilled() {
        // Manually-built Filespec (no /UF) — 2.0 policy autofills from /F.
        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);

        let pages_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
            d.insert("Kids".to_owned(), PdfObject::Array(vec![]));
            d.insert("Count".to_owned(), PdfObject::Integer(0));
            d
        }));
        let spec_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Filespec".to_owned()));
            d.insert("F".to_owned(), PdfObject::String(b"manual.csv".to_vec()));
            d
        }));
        let cat_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d.insert(
                "AF".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(spec_id, 0)]),
            );
            d
        }));
        let bytes = writer.serialize_all(cat_id, None, None).unwrap();

        let doc = PdfDocument::parse(bytes).unwrap();
        let cat = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let specs = cat.associated_files(&doc).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name_uf.as_deref(), Some("manual.csv"));
        assert_eq!(specs[0].display_name(), Some("manual.csv"));
    }

    #[test]
    fn pdf2_rc4_encrypt_object_rejected() {
        // An RC4 Standard handler dict stuffed into the pool trips the 2.0
        // policy (ISO 32000-2 removed RC4). AES-128 (V=4) passes the scan.
        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);

        let enc = |v: i64| {
            let mut d = PdfDict::new();
            d.insert("Filter".to_owned(), PdfObject::Name("Standard".to_owned()));
            d.insert("V".to_owned(), PdfObject::Integer(v));
            d.insert(
                "R".to_owned(),
                PdfObject::Integer(if v == 2 { 3 } else { 4 }),
            );
            d.insert("Length".to_owned(), PdfObject::Integer(128));
            d.insert("O".to_owned(), PdfObject::String(vec![0u8; 32]));
            d.insert("U".to_owned(), PdfObject::String(vec![0u8; 32]));
            d.insert("P".to_owned(), PdfObject::Integer(-4));
            PdfObject::Dictionary(d)
        };

        let pages_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
            d.insert("Kids".to_owned(), PdfObject::Array(vec![]));
            d.insert("Count".to_owned(), PdfObject::Integer(0));
            d
        }));
        let cat_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d
        }));
        // Simulate a caller inserting their own /Encrypt object.
        let enc_id = writer.reserve_id();
        writer.set_object(enc_id, enc(2));

        let err = writer.serialize_all(cat_id, None, None).unwrap_err();
        assert!(
            matches!(err, crate::error::PdfError::WriteError { .. }),
            "RC4 + PDF 2.0 must be refused: {:?}",
            err
        );

        // AES-128 (V=4/R=4) is legal in 2.0 → scan passes and file is emitted.
        let mut writer_aes = PdfWriter::new();
        writer_aes.set_emit_version(EmitVersion::V2_0);
        let pages2 = writer_aes.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
            d.insert("Kids".to_owned(), PdfObject::Array(vec![]));
            d.insert("Count".to_owned(), PdfObject::Integer(0));
            d
        }));
        let cat2 = writer_aes.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages2, 0));
            d
        }));
        let enc_id = writer_aes.reserve_id();
        writer_aes.set_object(enc_id, enc(4));
        let bytes = writer_aes.serialize_all(cat2, None, None).unwrap();
        assert!(bytes.starts_with(b"%PDF-2.0"));
    }

    #[test]
    fn pdf2_filespec_without_f_or_uf_is_write_error() {
        // Neither /F nor /UF — the policy cannot autofill, and PDF 2.0
        // requires /UF: fail closed instead of emitting a malformed spec.
        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);
        writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Filespec".to_owned()));
            d
        }));
        let cat_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Integer(1));
            d
        }));
        let err = writer.serialize_all(cat_id, None, None).unwrap_err();
        assert!(matches!(err, crate::error::PdfError::WriteError { .. }));
    }

    #[test]
    fn fresh_pdf_parseable() {
        let bytes = minimal_pdf_bytes();
        let doc = PdfDocument::parse(bytes).expect("should parse");
        assert_eq!(doc.page_count().unwrap(), 0);
    }

    // --- Associated Files writer round-trip (Phase C) --------------------------

    #[test]
    fn associated_file_roundtrip() {
        use crate::document::catalog::Catalog;

        let mut writer = PdfWriter::new();

        // Associated file added BEFORE the catalog (needs its object ID).
        let spec_id = writer
            .add_associated_file(
                crate::document::associated_files::AfRelationship::Data,
                "source.csv",
                b"col1,col2\n1,2\n",
            )
            .unwrap();

        let pages_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
            d.insert("Kids".to_owned(), PdfObject::Array(vec![]));
            d.insert("Count".to_owned(), PdfObject::Integer(0));
            d
        }));

        let cat_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d.insert(
                "AF".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(spec_id, 0)]),
            );
            d
        }));

        let bytes = writer.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).expect("round-trip parse");
        let cat = Catalog::from_document(&doc).unwrap();
        let specs = cat.associated_files(&doc).unwrap();

        assert_eq!(specs.len(), 1);
        let fs = &specs[0];
        assert_eq!(fs.name_f.as_deref(), Some("source.csv"));
        assert_eq!(fs.name_uf.as_deref(), Some("source.csv"));
        assert_eq!(fs.display_name(), Some("source.csv"));
        assert_eq!(
            fs.relationship,
            crate::document::associated_files::AfRelationship::Data
        );
        let ef = fs
            .embedded
            .as_ref()
            .expect("embedded present after round-trip");
        assert_eq!(ef.data, b"col1,col2\n1,2\n");
        assert_eq!(ef.params.size, Some(14));
    }

    #[test]
    fn associated_file_unicode_name_roundtrip() {
        let mut writer = PdfWriter::new();
        let spec_id = writer
            .add_associated_file(AfRelationship::Supplement, "résumé.txt", b"data")
            .unwrap();

        let pages_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
            d.insert("Kids".to_owned(), PdfObject::Array(vec![]));
            d.insert("Count".to_owned(), PdfObject::Integer(0));
            d
        }));
        let cat_id = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d.insert(
                "AF".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(spec_id, 0)]),
            );
            d
        }));
        let bytes = writer.serialize_all(cat_id, None, None).unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        let cat = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let specs = cat.associated_files(&doc).unwrap();
        let fs = &specs[0];
        // /UF decodes via UTF-16BE → Unicode name preserved.
        assert_eq!(fs.name_uf.as_deref(), Some("résumé.txt"));
        assert_eq!(fs.display_name(), Some("résumé.txt"));
        // /F is the ASCII-safe fallback (é → _).
        assert_eq!(fs.name_f.as_deref(), Some("r_sum_.txt"));
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
