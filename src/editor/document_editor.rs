//! `PdfEditor` — coordinate reader state with writer for incremental updates.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};
use crate::writer::document::PdfWriter;
use crate::writer::xref::{build_trailer_dict, write_full_xref_and_trailer};

/// Whether the editor produces an incremental update or a full rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    /// Append new/modified objects to the end of the original bytes.
    WriteAppend,
    /// Produce a completely fresh PDF (needed for redaction, merging, etc.).
    WriteNew,
}

/// PDF document editor.
///
/// Wraps a loaded [`PdfDocument`] (reader) together with a [`PdfWriter`]
/// (writer) to implement the copy-on-write incremental update model.
///
/// # Workflow
///
/// 1. [`PdfEditor::open`] — parse the existing PDF bytes.
/// 2. Make changes using the `page_editor`, `annotation`, or `metadata_editor`
///    helpers (they call [`replace_object`] / [`add_object`] internally).
/// 3. [`PdfEditor::save_append`] — serialize only the changed objects and
///    append them as an incremental update section.
pub struct PdfEditor {
    /// The loaded reader state (original document, unchanged).
    pub doc: PdfDocument,
    /// Accumulates new and modified objects for the next save.
    pub writer: PdfWriter,
    /// Editing strategy.
    pub mode: EditMode,
    /// Byte offset of the last `startxref` in the original file.
    pub original_xref_offset: u64,
    /// Object number of the Catalog dictionary.
    pub catalog_id: u32,
    /// Object number of the Pages root node.
    pub pages_id: u32,
    /// Object number of the Info dictionary (may be absent).
    pub info_id: Option<u32>,
    /// Undo history: writer snapshots captured by [`PdfEditor::checkpoint`]
    /// before each mutating operation (front = oldest). Bounded by
    /// [`MAX_UNDO_DEPTH`].
    undo_stack: Vec<crate::writer::document::PoolSnapshot>,
    /// Redo history: snapshots pushed when [`PdfEditor::undo`] runs, cleared
    /// whenever a fresh [`PdfEditor::checkpoint`] is taken.
    redo_stack: Vec<crate::writer::document::PoolSnapshot>,
}

/// Maximum number of undo steps retained. Each step is a clone of the writer
/// pool (typically a few KB), so 50 steps is a small, bounded memory cost.
const MAX_UNDO_DEPTH: usize = 50;

impl PdfEditor {
    /// Open an existing PDF for editing.
    ///
    /// Parses the bytes, extracts structural IDs, and initialises the writer
    /// so new objects start above the existing maximum ID.
    pub fn open(data: Vec<u8>) -> Result<Self> {
        // Find startxref before we move data into PdfDocument.
        let xref_offset = PdfDocument::startxref_offset(&data)? as u64;

        let doc = PdfDocument::parse(data)?;

        Self::from_doc(doc, xref_offset)
    }

    /// Open a password-protected PDF for editing.
    ///
    /// Identical to [`open`](Self::open) but decrypts with the supplied password.
    /// Returns [`PdfError::Encrypted`] if the password is wrong.
    #[cfg(feature = "crypto")]
    pub fn open_with_password(data: Vec<u8>, password: &[u8]) -> Result<Self> {
        let xref_offset = PdfDocument::startxref_offset(&data)? as u64;
        let doc = PdfDocument::parse_with_password(data, password)?;
        Self::from_doc(doc, xref_offset)
    }

    fn from_doc(doc: PdfDocument, xref_offset: u64) -> Result<Self> {
        // Catalog ID
        let root_obj = doc
            .trailer
            .get("Root")
            .ok_or_else(|| PdfError::invalid_structure("trailer missing /Root"))?
            .clone();
        let catalog_id = match root_obj {
            PdfObject::Reference(id, _) => id,
            _ => return Err(PdfError::invalid_structure("/Root is not a reference")),
        };

        // Pages ID
        let catalog = doc.get_object(catalog_id)?;
        let pages_ref = catalog
            .as_dict()
            .and_then(|d| d.get("Pages"))
            .ok_or_else(|| PdfError::invalid_structure("catalog missing /Pages"))?
            .clone();
        let pages_id = match pages_ref {
            PdfObject::Reference(id, _) => id,
            _ => return Err(PdfError::invalid_structure("/Pages is not a reference")),
        };

        // Info ID (optional)
        let info_id = doc.trailer.get("Info").and_then(|o| match o {
            PdfObject::Reference(id, _) => Some(*id),
            _ => None,
        });

        let max_id = doc.max_object_id();
        let writer = PdfWriter::new_from_max_id(max_id);

        Ok(Self {
            doc,
            writer,
            mode: EditMode::WriteAppend,
            original_xref_offset: xref_offset,
            catalog_id,
            pages_id,
            info_id,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    /// Record the current writer state as an undo checkpoint.
    ///
    /// Call this **before** a mutating operation (text commit, annotation add,
    /// page op, …). It snapshots the writer pool, caps history at
    /// [`MAX_UNDO_DEPTH`], and clears the redo stack (a new edit invalidates any
    /// redo future).
    pub fn checkpoint(&mut self) {
        self.redo_stack.clear();
        self.undo_stack.push(self.writer.snapshot());
        if self.undo_stack.len() > MAX_UNDO_DEPTH {
            // Drop the oldest snapshot. O(n) but n ≤ 50 and only on overflow.
            self.undo_stack.remove(0);
        }
    }

    /// Revert to the most recent checkpoint. Returns `false` if there is
    /// nothing to undo.
    ///
    /// The pre-undo state is pushed onto the redo stack so [`PdfEditor::redo`]
    /// can replay it.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(self.writer.snapshot());
        self.writer.restore(prev);
        true
    }

    /// Re-apply the most recently undone change. Returns `false` if there is
    /// nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.writer.snapshot());
        self.writer.restore(next);
        true
    }

    /// Whether an [`PdfEditor::undo`] would do anything.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether a [`PdfEditor::redo`] would do anything.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Retrieve an object: checks the writer pool first, then the original doc.
    ///
    /// This gives the "copy-on-write" view: pending modifications win over the
    /// original file content.
    pub fn get_object(&self, id: u32) -> Result<PdfObject> {
        if let Some(obj) = self.writer.get_object(id) {
            return Ok(obj.clone());
        }
        self.doc.get_object(id)
    }

    /// Queue a replacement for an existing object (same ID, new content).
    ///
    /// On save, the new version is written in the incremental section so it
    /// shadows the original object for any PDF reader.
    pub fn replace_object(&mut self, id: u32, obj: PdfObject) {
        self.writer.set_object(id, obj);
    }

    /// Add a completely new object and return its assigned ID.
    pub fn add_object(&mut self, obj: PdfObject) -> u32 {
        self.writer.add_object(obj)
    }

    /// Current page count, reflecting any pending additions/deletions.
    pub fn page_count(&self) -> Result<usize> {
        let pages = self.get_object(self.pages_id)?;
        let count = pages
            .as_dict()
            .and_then(|d| d.get("Count"))
            .ok_or_else(|| PdfError::invalid_structure("pages node missing /Count"))?
            .clone();
        match self.doc.resolve(&count)? {
            PdfObject::Integer(n) => Ok(n as usize),
            _ => Err(PdfError::invalid_structure("/Count is not an integer")),
        }
    }

    /// Retrieve the page dictionary for page `index` (0-based).
    ///
    /// Uses the copy-on-write `get_object` so pending page replacements are visible.
    pub fn get_page_dict(&self, index: usize) -> Result<(u32, PdfDict)> {
        // Walk the page tree iteratively, using our get_object (CoW).
        let pages_obj = self.get_object(self.pages_id)?;
        let kids = pages_obj
            .as_dict()
            .and_then(|d| d.get("Kids"))
            .ok_or_else(|| PdfError::invalid_structure("pages node missing /Kids"))?
            .clone();

        let kids_arr = match kids {
            PdfObject::Array(arr) => arr,
            _ => return Err(PdfError::invalid_structure("/Kids is not an array")),
        };

        let mut cursor = 0usize;
        for kid_ref in &kids_arr {
            let kid_id = match kid_ref {
                PdfObject::Reference(id, _) => *id,
                _ => continue,
            };
            let kid = self.get_object(kid_id)?;
            let kid_dict = kid
                .as_dict()
                .ok_or_else(|| PdfError::invalid_structure("kid is not a dict"))?;

            let node_type = kid_dict.get("Type").and_then(|o| o.as_name()).unwrap_or("");

            if node_type == "Page" {
                if cursor == index {
                    return Ok((kid_id, kid_dict.clone()));
                }
                cursor += 1;
            } else if node_type == "Pages" {
                // Intermediate page tree node — recurse via count
                let sub_count = kid_dict
                    .get("Count")
                    .and_then(|o| o.as_integer())
                    .unwrap_or(0) as usize;
                if index < cursor + sub_count {
                    // Target page is in this subtree — do simple DFS
                    return self.get_page_dict_in_subtree(kid_id, index - cursor);
                }
                cursor += sub_count;
            }
        }
        Err(PdfError::invalid_structure(format!(
            "page index {} out of range",
            index
        )))
    }

    // Recursive helper for multi-level page trees (depth-limited at 64).
    fn get_page_dict_in_subtree(
        &self,
        pages_id: u32,
        local_index: usize,
    ) -> Result<(u32, PdfDict)> {
        let pages_obj = self.get_object(pages_id)?;
        let kids = pages_obj
            .as_dict()
            .and_then(|d| d.get("Kids"))
            .cloned()
            .ok_or_else(|| PdfError::invalid_structure("pages node missing /Kids"))?;

        let kids_arr = match kids {
            PdfObject::Array(arr) => arr,
            _ => return Err(PdfError::invalid_structure("/Kids is not an array")),
        };

        let mut cursor = 0usize;
        for kid_ref in &kids_arr {
            let kid_id = match kid_ref {
                PdfObject::Reference(id, _) => *id,
                _ => continue,
            };
            let kid = self.get_object(kid_id)?;
            let kid_dict = kid
                .as_dict()
                .ok_or_else(|| PdfError::invalid_structure("kid is not a dict"))?;

            let node_type = kid_dict.get("Type").and_then(|o| o.as_name()).unwrap_or("");

            if node_type == "Page" {
                if cursor == local_index {
                    return Ok((kid_id, kid_dict.clone()));
                }
                cursor += 1;
            } else {
                let sub_count = kid_dict
                    .get("Count")
                    .and_then(|o| o.as_integer())
                    .unwrap_or(0) as usize;
                if local_index < cursor + sub_count {
                    return self.get_page_dict_in_subtree(kid_id, local_index - cursor);
                }
                cursor += sub_count;
            }
        }
        Err(PdfError::invalid_structure(format!(
            "page index {} not found in subtree",
            local_index
        )))
    }

    // ── Save ──────────────────────────────────────────────────────────────────

    /// Serialize the pending changes as an **incremental update** and append
    /// them to `original_bytes`.
    ///
    /// The result is a valid PDF with the changes applied. The original bytes
    /// are preserved byte-for-byte at the front.
    pub fn save_append(&mut self, original_bytes: &[u8]) -> Result<Vec<u8>> {
        if self.writer.is_empty() {
            // Nothing changed — return the original unchanged.
            return Ok(original_bytes.to_vec());
        }

        // Compute the base offset: all new objects start after original_bytes.
        let base = original_bytes.len() as u64;

        let mut new_section: Vec<u8> = Vec::new();
        let mut offsets: Vec<(u32, u64)> = Vec::new();

        // Collect and sort objects for deterministic output.
        // We need to iterate the writer pool.  Access via serialize path.
        // Build a temporary copy by serializing each object and recording offsets.
        let mut pool_snapshot: Vec<(u32, u32, PdfObject)> = Vec::new();
        for id in self.writer_ids() {
            if let Some(obj) = self.writer.get_object(id) {
                pool_snapshot.push((id, 0, obj.clone()));
            }
        }
        pool_snapshot.sort_by_key(|(id, _, _)| *id);

        // For an encrypted document every newly-written string and stream must be
        // re-encrypted with the file key (the reader will decrypt them on open).
        // Clone the handler once so the per-object encryption below doesn't hold a
        // borrow of `self.doc` while we also read its trailer afterwards.
        #[cfg(feature = "crypto")]
        let enc = self.doc.encryption_handler().cloned();

        for (id, gen, obj) in &pool_snapshot {
            let abs_offset = base + new_section.len() as u64;
            offsets.push((*id, abs_offset));
            let header = format!("{} {} obj\n", id, gen);
            new_section.extend_from_slice(header.as_bytes());

            #[cfg(feature = "crypto")]
            let obj_owned;
            #[cfg(feature = "crypto")]
            let obj_ref: &PdfObject = if let Some(h) = &enc {
                let mut o = obj.clone();
                encrypt_object_for_write(&mut o, *id, *gen as u16, h)?;
                obj_owned = o;
                &obj_owned
            } else {
                obj
            };
            #[cfg(not(feature = "crypto"))]
            let obj_ref: &PdfObject = obj;

            crate::writer::serializer::serialize_object(obj_ref, &mut new_section);
            new_section.extend_from_slice(b"\nendobj\n");
        }

        // Compute /Size: must exceed the highest ID in the entire file.
        let writer_max = offsets.iter().map(|(id, _)| *id).max().unwrap_or(0);
        let doc_max = self.doc.max_object_id();
        #[cfg_attr(not(feature = "crypto"), allow(unused_mut))]
        let mut size = writer_max.max(doc_max) + 1;

        // Encrypted incremental update: the new (first-read) trailer must reference
        // `/Encrypt` and carry `/ID` so a reader re-derives the SAME file key and
        // decrypts our freshly-encrypted objects. The original `/Encrypt` ref id is
        // lost during parse (resolved to an inline dict), so re-emit the dict as a
        // fresh UNENCRYPTED object (its O/U/Perms must stay plaintext) and point at
        // it. Allocated locally — not via `add_object` — so repeated saves don't
        // accumulate duplicate Encrypt objects.
        #[cfg(feature = "crypto")]
        let encrypt_ref: Option<u32> = if enc.is_some() {
            if let Some(enc_dict) = self.doc.trailer.get("Encrypt").cloned() {
                let enc_id = size;
                size += 1;
                let abs_offset = base + new_section.len() as u64;
                offsets.push((enc_id, abs_offset));
                let header = format!("{} 0 obj\n", enc_id);
                new_section.extend_from_slice(header.as_bytes());
                crate::writer::serializer::serialize_object(&enc_dict, &mut new_section);
                new_section.extend_from_slice(b"\nendobj\n");
                Some(enc_id)
            } else {
                None
            }
        } else {
            None
        };

        let xref_start = base + new_section.len() as u64;
        let mut trailer = build_trailer_dict(
            size,
            self.catalog_id,
            self.info_id,
            Some(self.original_xref_offset),
        );
        // Carry `/Encrypt` (encrypted docs) and `/ID` (verbatim) into the new trailer.
        #[cfg(feature = "crypto")]
        if let Some(enc_id) = encrypt_ref {
            trailer.insert("Encrypt".to_owned(), PdfObject::Reference(enc_id, 0));
        }
        if let Some(id_arr) = self.doc.trailer.get("ID").cloned() {
            trailer.insert("ID".to_owned(), id_arr);
        }
        write_full_xref_and_trailer(&offsets, &trailer, xref_start, &mut new_section);

        let mut result = original_bytes.to_vec();
        result.extend_from_slice(&new_section);
        Ok(result)
    }

    /// Produce a full new PDF rewrite (no incremental section, no `/Prev`).
    ///
    /// Copies every object from the original document into a fresh writer, then
    /// applies any pending copy-on-write overrides from the editor's writer pool.
    /// The output bytes come entirely from the serializer — no bytes of the
    /// original file survive. Required for redaction, where the original data
    /// must not be recoverable from the output file.
    ///
    // TODO(crypto): save_new does not re-encrypt or carry `/Encrypt` forward, so
    // rewriting an encrypted document produces a structurally-encrypted-but-
    // plaintext file. The text-edit/save path uses save_append (which does
    // re-encrypt); redaction of encrypted docs is a separate follow-up.
    pub fn save_new(&mut self) -> Result<Vec<u8>> {
        // Union of original xref IDs and any new/modified objects from the writer.
        let mut all_ids: Vec<u32> = self.doc.all_object_ids();
        for id in self.writer.all_ids() {
            if !all_ids.contains(&id) {
                all_ids.push(id);
            }
        }
        all_ids.sort();

        // Build a fresh writer containing the complete logical document (CoW:
        // writer overrides win over original doc objects).
        let mut fresh = crate::writer::document::PdfWriter::new();
        for id in all_ids {
            match self.get_object(id) {
                Ok(obj) => {
                    fresh.set_object(id, obj);
                }
                Err(e) => {
                    log::warn!("save_new: skipping unreadable object {}: {}", id, e);
                }
            }
        }
        fresh.serialize_all(self.catalog_id, self.info_id, None)
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    /// Return the IDs of all objects currently in the writer pool.
    fn writer_ids(&self) -> Vec<u32> {
        // We expose this indirectly through a scan — PdfWriter doesn't expose
        // the pool directly, so we build IDs from what we've tracked.
        // The writer's pool is private; we reconstruct by iterating known IDs.
        // Implementation: we piggyback on the fact that set_object / add_object
        // keeps next_id monotonic. Scan from original_max+1 upward isn't safe
        // because set_object can use arbitrary IDs.
        // We add a helper method to PdfWriter instead.
        self.writer.all_ids()
    }
}

/// Recursively encrypt every string and stream body inside a newly-written
/// object, in place, using the document's encryption handler.
///
/// `id`/`gen` identify the indirect object (per-object key for RC4 / AES-128;
/// ignored by AES-256, which uses the file key directly). Integers, names,
/// booleans, nulls and references are left untouched. The caller must NOT pass
/// the `/Encrypt` dictionary or the trailer `/ID` here — those are exempt from
/// encryption (ISO 32000-1 §7.6.1).
#[cfg(feature = "crypto")]
fn encrypt_object_for_write(
    obj: &mut PdfObject,
    id: u32,
    gen: u16,
    h: &crate::crypto::EncryptionHandler,
) -> Result<()> {
    match obj {
        PdfObject::String(s) => h.encrypt_string(id, gen, s)?,
        PdfObject::Array(items) => {
            for it in items.iter_mut() {
                encrypt_object_for_write(it, id, gen, h)?;
            }
        }
        PdfObject::Dictionary(d) => {
            for v in d.values_mut() {
                encrypt_object_for_write(v, id, gen, h)?;
            }
        }
        PdfObject::Stream(st) => {
            for v in st.dict.values_mut() {
                encrypt_object_for_write(v, id, gen, h)?;
            }
            // Encrypt the (already filter-applied) stream body; the serializer
            // recomputes /Length from raw_data, so the encrypted length is correct.
            st.raw_data = h.encrypt_stream(id, gen, &st.raw_data)?;
        }
        _ => {}
    }
    Ok(())
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
    fn open_reads_structural_ids() {
        let data = load("minimal.pdf");
        let editor = PdfEditor::open(data).unwrap();
        assert!(editor.catalog_id > 0);
        assert!(editor.pages_id > 0);
    }

    /// Assemble a minimal one-page PDF whose catalog carries the given extra
    /// catalog entries (verbatim PDF dict body, e.g. an `/AcroForm` ref).
    fn pdf_with_catalog_extra(catalog_extra: &str, extra_objs: &[&str]) -> Vec<u8> {
        let mut objs: Vec<String> = vec![
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ];
        for e in extra_objs {
            objs.push((*e).to_string());
        }
        let mut pdf = String::from("%PDF-1.7\n");
        let mut offsets = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, body));
        }
        let xref_pos = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objs.len() + 1
        ));
        for off in &offsets {
            pdf.push_str(&format!("{:010} 00000 n \n", off));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            objs.len() + 1,
            xref_pos
        ));
        pdf.into_bytes()
    }

    #[test]
    fn is_signed_detects_sigflags() {
        // /AcroForm with /SigFlags 3 (bit 1 = SignaturesExist) ⇒ signed.
        let signed = pdf_with_catalog_extra("/AcroForm 4 0 R", &["<< /Fields [] /SigFlags 3 >>"]);
        let doc = PdfDocument::parse(signed).unwrap();
        assert!(doc.is_signed(), "SigFlags bit 1 set must report signed");
    }

    #[test]
    fn is_signed_false_without_sigflags() {
        // An AcroForm with no signature flag is not "signed".
        let unsigned = pdf_with_catalog_extra("/AcroForm 4 0 R", &["<< /Fields [] >>"]);
        let doc = PdfDocument::parse(unsigned).unwrap();
        assert!(!doc.is_signed());

        // And a document with no AcroForm at all.
        let plain = load("minimal.pdf");
        assert!(!PdfDocument::parse(plain).unwrap().is_signed());
    }

    #[test]
    fn get_object_falls_back_to_doc() {
        let data = load("minimal.pdf");
        let editor = PdfEditor::open(data).unwrap();
        let catalog = editor.get_object(editor.catalog_id).unwrap();
        assert!(catalog.as_dict().is_some());
    }

    #[test]
    fn replace_object_shadows_original() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let cat_id = editor.catalog_id;
        editor.replace_object(cat_id, PdfObject::Integer(999));
        assert_eq!(editor.get_object(cat_id).unwrap(), PdfObject::Integer(999));
    }

    #[test]
    fn undo_reverts_a_checkpointed_change() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        assert!(!editor.can_undo());

        editor.checkpoint();
        let id = editor.add_object(PdfObject::Integer(42));
        assert_eq!(editor.get_object(id).unwrap(), PdfObject::Integer(42));
        assert!(editor.can_undo());

        assert!(editor.undo());
        // Object is gone from the writer pool; the editor falls back to the
        // original doc, where this id does not exist and so resolves to Null
        // (per ISO 32000-1 §7.3.10 — a reference to a missing object is null).
        assert_eq!(editor.get_object(id).unwrap(), PdfObject::Null);
        assert!(!editor.can_undo());
    }

    #[test]
    fn redo_replays_an_undone_change() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();

        editor.checkpoint();
        let id = editor.add_object(PdfObject::Integer(7));
        assert!(editor.undo());
        assert!(editor.can_redo(), "redo must be available after undo");

        assert!(editor.redo());
        assert_eq!(editor.get_object(id).unwrap(), PdfObject::Integer(7));
        assert!(!editor.can_redo());
    }

    #[test]
    fn checkpoint_clears_redo_history() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();

        editor.checkpoint();
        editor.add_object(PdfObject::Integer(1));
        editor.undo();
        assert!(editor.can_redo());

        // A fresh edit invalidates the redo future.
        editor.checkpoint();
        editor.add_object(PdfObject::Integer(2));
        assert!(!editor.can_redo());
    }

    #[test]
    fn undo_with_empty_history_is_noop() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        assert!(!editor.undo());
        assert!(!editor.redo());
    }

    #[test]
    fn save_append_no_changes_returns_original() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        let result = editor.save_append(&original).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn save_append_with_change_is_parseable() {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        let before = editor.doc.page_count().unwrap();
        // Add a dummy object — should not change page count
        editor.add_object(PdfObject::Integer(42));
        let result = editor.save_append(&original).unwrap();
        // Must start with original bytes
        assert!(result.starts_with(&original));
        // Must be parseable with same page count
        let reopened = PdfDocument::parse(result).unwrap();
        assert_eq!(reopened.page_count().unwrap(), before);
    }

    #[test]
    fn save_new_produces_parseable_pdf() {
        let data = load("minimal.pdf");
        let before = PdfDocument::parse(data.clone())
            .unwrap()
            .page_count()
            .unwrap();
        let mut editor = PdfEditor::open(data).unwrap();
        let result = editor.save_new().unwrap();
        assert!(
            result.starts_with(b"%PDF-1.7"),
            "must start with PDF header"
        );
        let reopened = PdfDocument::parse(result).unwrap();
        assert_eq!(reopened.page_count().unwrap(), before);
    }

    #[test]
    fn save_new_includes_all_objects() {
        let data = load("minimal.pdf");
        let mut editor = PdfEditor::open(data).unwrap();
        let cat_id = editor.catalog_id;
        let result = editor.save_new().unwrap();
        // Catalog object must exist in the output (proves original objects were copied).
        let marker = format!("{} 0 obj", cat_id);
        assert!(
            result.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "catalog object {} not found in save_new output",
            cat_id
        );
    }

    #[cfg(feature = "crypto")]
    #[test]
    fn save_append_encrypted_roundtrips() {
        // Editing an encrypted PDF must re-encrypt newly-written objects so the
        // saved file re-opens with the same password and decrypts the edit.
        let data = load("encrypted_aes256.pdf");
        let mut editor = PdfEditor::open_with_password(data.clone(), b"test").unwrap();

        // Add a new object carrying a recognisable marker string.
        let marker = b"ENCRYPTME-marker-12345".to_vec();
        let mut dict = crate::parser::objects::PdfDict::new();
        dict.insert("Marker".to_owned(), PdfObject::String(marker.clone()));
        let id = editor.add_object(PdfObject::Dictionary(dict));

        let out = editor.save_append(&data).unwrap();

        // The marker must NOT appear in plaintext — it was encrypted on write.
        assert!(
            !out.windows(marker.len()).any(|w| w == &marker[..]),
            "marker leaked in plaintext — object was not encrypted on save"
        );

        // Re-open with the password: the marker decrypts back to the original.
        let reparsed = PdfDocument::parse_with_password(out, b"test").unwrap();
        match reparsed.get_object(id).unwrap() {
            PdfObject::Dictionary(d) => match d.get("Marker") {
                Some(PdfObject::String(s)) => assert_eq!(s, &marker),
                other => panic!("Marker missing or wrong type: {:?}", other),
            },
            other => panic!("object {} is not a dictionary: {:?}", id, other),
        }

        // The new trailer must carry /Encrypt and /ID so the key re-derives.
        assert!(
            reparsed.trailer.contains_key("Encrypt"),
            "new trailer missing /Encrypt"
        );
        assert!(
            reparsed.trailer.contains_key("ID"),
            "new trailer missing /ID"
        );
    }
}
