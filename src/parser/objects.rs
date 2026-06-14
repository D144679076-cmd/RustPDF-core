//! PDF object model and document loader.
//!
//! Provides the full PDF object type hierarchy (ISO 32000-1 §7.3), indirect
//! reference resolution, stream decoding, and document-level XRef parsing
//! including both traditional XRef tables and PDF 1.5+ XRef streams.
//!
//! Entry point: [`PdfDocument::parse`].

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::error::{PdfError, Result};
use crate::parser::filters;
use crate::parser::lexer::{Keyword, Lexer, Token};

// ---------------------------------------------------------------------------
// Public object types
// ---------------------------------------------------------------------------

/// An insertion-order-preserving key-value mapping for PDF dictionaries.
///
/// Backed by [`IndexMap`] so the order keys were parsed (or inserted) is
/// retained through a parse→serialize round-trip. This matters for
/// positionally-paired entries like `/Filter` ↔ `/DecodeParms` and for keeping
/// the byte layout of signed dictionaries stable. Lookups remain O(1).
pub type PdfDict = IndexMap<String, PdfObject>;

/// Any PDF value (ISO 32000-1 §7.3).
#[derive(Debug, Clone, PartialEq)]
pub enum PdfObject {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    /// Byte string — may be PDFDocEncoding, UTF-16BE, or raw bytes.
    String(Vec<u8>),
    /// `/Name` token (without the leading slash).
    Name(String),
    Array(Vec<PdfObject>),
    Dictionary(PdfDict),
    Stream(Box<PdfStream>),
    /// Indirect reference `obj_num gen_num R`.
    Reference(u32, u16),
}

impl PdfObject {
    /// Return the integer value if this is `Integer`, else `None`.
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            PdfObject::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// Return the name string if this is `Name`, else `None`.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            PdfObject::Name(s) => Some(s),
            _ => None,
        }
    }

    /// Return the dict if this is `Dictionary` or `Stream`, else `None`.
    pub fn as_dict(&self) -> Option<&PdfDict> {
        match self {
            PdfObject::Dictionary(d) => Some(d),
            PdfObject::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }
}

/// A PDF stream object: a dictionary plus raw (unfiltered) byte data.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfStream {
    /// Stream dictionary (contains /Filter, /Length, /DecodeParms, …).
    pub dict: PdfDict,
    /// Raw bytes between `stream` and `endstream`.
    pub raw_data: Vec<u8>,
}

impl PdfStream {
    /// Decode all filters and return the uncompressed content.
    ///
    /// Only works for `/DecodeParms` stored as a direct dictionary or array in
    /// the stream dict.  Use [`decode_with_doc`] when document access is
    /// available — it also resolves indirect references.
    pub fn decode(&self) -> Result<Vec<u8>> {
        let filter_names = self.filter_names();
        let names: Vec<&str> = filter_names.iter().map(|s| s.as_str()).collect();
        let data = filters::apply_pipeline(&names, &self.raw_data)?;
        if let Some(parms) = self.decode_parms() {
            apply_predictor(data, parms)
        } else {
            Ok(data)
        }
    }

    /// Decode all filters and apply the predictor, resolving `/DecodeParms`
    /// through indirect references via `doc`.
    ///
    /// Prefer this over `decode()` whenever a `PdfDocument` is available.
    pub fn decode_with_doc(&self, doc: &PdfDocument) -> Result<Vec<u8>> {
        let filter_names = self.filter_names();
        let names: Vec<&str> = filter_names.iter().map(|s| s.as_str()).collect();
        let data = filters::apply_pipeline(&names, &self.raw_data)?;

        // /DecodeParms may be a direct dict, an array, or an indirect reference.
        let resolved = match self.dict.get("DecodeParms") {
            Some(r @ PdfObject::Reference(..)) => doc.resolve(r).ok(),
            Some(other) => Some(other.clone()),
            None => None,
        };
        let parms: Option<PdfDict> = match resolved {
            Some(PdfObject::Dictionary(d)) => Some(d),
            Some(PdfObject::Array(arr)) => arr.into_iter().rev().find_map(|o| {
                // Array elements may themselves be indirect references — resolve them.
                let elem = match &o {
                    PdfObject::Reference(..) => doc.resolve(&o).ok()?,
                    _ => o,
                };
                match elem {
                    PdfObject::Dictionary(d) => Some(d),
                    _ => None,
                }
            }),
            _ => None,
        };

        if parms.is_none() {
            if let Some(dp) = self.dict.get("DecodeParms") {
                if !matches!(dp, PdfObject::Null) {
                    log::warn!(
                        "[decode] DecodeParms present but unresolved (variant={:?})",
                        std::mem::discriminant(dp)
                    );
                }
            }
        }

        if let Some(ref p) = parms {
            apply_predictor(data, p)
        } else {
            Ok(data)
        }
    }

    /// Extract `/DecodeParms` as a direct dict reference (no ref resolution).
    fn decode_parms(&self) -> Option<&PdfDict> {
        match self.dict.get("DecodeParms") {
            Some(PdfObject::Dictionary(d)) => Some(d),
            Some(PdfObject::Array(arr)) => arr.iter().rev().find_map(|o| match o {
                PdfObject::Dictionary(d) => Some(d),
                _ => None,
            }),
            _ => None,
        }
    }

    /// Return the list of filter names from the stream dictionary.
    pub fn filter_names(&self) -> Vec<String> {
        match self.dict.get("Filter") {
            Some(PdfObject::Name(n)) => vec![n.clone()],
            Some(PdfObject::Array(arr)) => arr
                .iter()
                .filter_map(|o| match o {
                    PdfObject::Name(n) => Some(n.clone()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Internal XRef entry type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum XRefEntry {
    /// Uncompressed object at a direct file byte offset.
    InUse {
        offset: u64,
        #[allow(dead_code)]
        generation: u16,
    },
    /// Object lives inside an object stream (PDF 1.5+).
    Compressed {
        stream_obj_num: u32,
        index: u32,
    },
    Free,
}

// ---------------------------------------------------------------------------
// PdfDocument
// ---------------------------------------------------------------------------

/// A loaded PDF document.
///
/// Owns the raw file bytes and the cross-reference index. Provides methods to
/// retrieve and resolve any object by its object number.
pub struct PdfDocument {
    data: Vec<u8>,
    xref: HashMap<u32, XRefEntry>,
    /// The most-recent trailer dictionary (combines all incremental-update trailers).
    pub trailer: PdfDict,
    // Cache of decoded object streams: stream_obj_num → list of parsed objects.
    obj_stream_cache: RwLock<HashMap<u32, Vec<PdfObject>>>,
    // Cache of decoded stream bytes: object_id → decoded bytes.
    // Avoids re-applying FlateDecode when the same stream is read multiple times
    // (e.g. a content stream decoded once per render tile).
    decoded_stream_cache: RwLock<HashMap<u32, Vec<u8>>>,
    // Temporary in-memory object overrides: object_id → object. Checked before the
    // xref/bytes in `get_object`, so a caller (the editor) can render the document's
    // *current* (edited) state by overlaying its writer-pool objects on the pristine
    // parse — no byte reparse. Normally empty; set/cleared around a render.
    overrides: RwLock<HashMap<u32, PdfObject>>,
    // Cached page table: page index → that page's indirect reference (or inline
    // dict). Built once by flattening the page tree on the first page lookup, so
    // every subsequent `Catalog::get_page_dict` is O(1) instead of an O(N)
    // tree walk. Stores *references* (not resolved dicts) so resolution stays
    // lazy and honors `overrides`. Safe to cache for the document's lifetime:
    // the parse is immutable, and structural page edits produce a freshly
    // reparsed document (which builds its own table).
    page_refs: RwLock<Option<Vec<PdfObject>>>,
    #[cfg(feature = "crypto")]
    enc: Option<crate::crypto::EncryptionHandler>,
}

// `RwLock` is not `Clone` (unlike the `RefCell` it replaced), so we clone by
// reading each lock and wrapping a fresh copy. This preserves the previous
// `#[derive(Clone)]` semantics — a clone carries the current cache contents —
// while making `PdfDocument: Sync` so it can be shared across render threads.
impl Clone for PdfDocument {
    fn clone(&self) -> Self {
        PdfDocument {
            data: self.data.clone(),
            xref: self.xref.clone(),
            trailer: self.trailer.clone(),
            obj_stream_cache: RwLock::new(self.obj_stream_cache.read().clone()),
            decoded_stream_cache: RwLock::new(self.decoded_stream_cache.read().clone()),
            overrides: RwLock::new(self.overrides.read().clone()),
            page_refs: RwLock::new(self.page_refs.read().clone()),
            #[cfg(feature = "crypto")]
            enc: self.enc.clone(),
        }
    }
}

impl PdfDocument {
    /// Parse a PDF from its raw bytes.
    ///
    /// Locates `startxref`, loads all XRef sections following `/Prev` chains,
    /// and builds the unified object index.
    /// For encrypted PDFs without the `crypto` feature, returns [`PdfError::Encrypted`].
    /// With the `crypto` feature, tries an empty user password first.
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        #[cfg(feature = "crypto")]
        return Self::parse_with_password(data, b"");

        #[cfg(not(feature = "crypto"))]
        {
            let start = find_startxref_offset(&data)?;
            let (xref, trailer) = build_xref(&data, start)?;
            if trailer.contains_key("Encrypt") {
                return Err(PdfError::Encrypted { offset: 0 });
            }
            Ok(PdfDocument {
                data,
                xref,
                trailer,
                obj_stream_cache: RwLock::new(HashMap::new()),
                decoded_stream_cache: RwLock::new(HashMap::new()),
                overrides: RwLock::new(HashMap::new()),
                page_refs: RwLock::new(None),
            })
        }
    }

    /// Parse an encrypted PDF using the supplied password.
    ///
    /// Tries `password` as both the user and owner password.
    /// Returns [`PdfError::Encrypted`] if the password is wrong.
    #[cfg(feature = "crypto")]
    pub fn parse_with_password(data: Vec<u8>, password: &[u8]) -> Result<Self> {
        let start = find_startxref_offset(&data)?;
        let (xref, mut trailer) = build_xref(&data, start)?;

        // /Encrypt is often stored as an indirect reference (e.g. `5 0 R`) rather
        // than an inline dict. Resolve it now so from_trailer always sees a dict.
        if let Some(PdfObject::Reference(id, _)) = trailer.get("Encrypt").cloned() {
            if let Some(XRefEntry::InUse { offset, .. }) = xref.get(&id) {
                match parse_object_at_offset(&data, *offset as usize) {
                    Ok(resolved) => {
                        trailer.insert("Encrypt".to_string(), resolved);
                    }
                    Err(e) => log::warn!("Failed to resolve /Encrypt ref {}: {}", id, e),
                }
            }
        }

        let doc_id = extract_file_id(&trailer);
        let enc = crate::crypto::EncryptionHandler::from_trailer(&trailer, &doc_id, password)?;

        Ok(PdfDocument {
            data,
            xref,
            trailer,
            obj_stream_cache: RwLock::new(HashMap::new()),
            decoded_stream_cache: RwLock::new(HashMap::new()),
            overrides: RwLock::new(HashMap::new()),
            page_refs: RwLock::new(None),
            enc,
        })
    }

    /// The document's encryption handler, when the file is encrypted and was
    /// opened with a valid password. Used by the writer to re-encrypt newly
    /// written objects on save (the handler carries the derived file key).
    #[cfg(feature = "crypto")]
    pub fn encryption_handler(&self) -> Option<&crate::crypto::EncryptionHandler> {
        self.enc.as_ref()
    }

    /// Whether the document declares encryption (`/Encrypt` in the trailer).
    ///
    /// With the `crypto` feature this reflects a successfully-built handler; without
    /// it, it simply reports the presence of an `/Encrypt` entry.
    pub fn is_encrypted(&self) -> bool {
        #[cfg(feature = "crypto")]
        {
            self.enc.is_some()
        }
        #[cfg(not(feature = "crypto"))]
        {
            self.trailer.contains_key("Encrypt")
        }
    }

    /// Install a temporary object-override map (see `overrides`). Pass the editor's
    /// writer-pool objects to render the document's current edited state without a
    /// byte reparse. Clears any cached decoded bytes for the overridden ids so a
    /// reused stream id can't return stale content.
    pub fn set_overrides(&self, map: HashMap<u32, PdfObject>) {
        {
            let mut dc = self.decoded_stream_cache.write();
            for id in map.keys() {
                dc.remove(id);
            }
        }
        *self.overrides.write() = map;
    }

    /// Pre-populate the decoded-stream cache with caller-supplied uncompressed bytes.
    ///
    /// After [`set_overrides`] clears a stream's cache entry, calling this
    /// method re-inserts the already-decompressed bytes so that the next
    /// `get_stream_data(id)` call is a cache hit — skipping the flate-decompress
    /// of the override object.
    pub fn preload_stream(&self, id: u32, bytes: &[u8]) {
        self.decoded_stream_cache.write().insert(id, bytes.to_vec());
    }

    /// Remove all object overrides (restore the pristine parsed view).
    pub fn clear_overrides(&self) {
        let cleared: Vec<u32> = self.overrides.read().keys().copied().collect();
        let mut dc = self.decoded_stream_cache.write();
        for id in cleared {
            dc.remove(&id);
        }
        self.overrides.write().clear();
    }

    /// Whether the cached page table (index → page reference) has been built.
    pub fn has_page_table(&self) -> bool {
        self.page_refs.read().is_some()
    }

    /// Install the flattened page table mapping page index → page reference.
    /// Called once (on the first page lookup) so subsequent lookups are O(1).
    pub fn set_page_table(&self, refs: Vec<PdfObject>) {
        *self.page_refs.write() = Some(refs);
    }

    /// Return page `index`'s cached reference (or inline dict), or `None` when
    /// the table isn't built yet or `index` is out of range. O(1).
    pub fn cached_page_ref(&self, index: usize) -> Option<PdfObject> {
        self.page_refs
            .read()
            .as_ref()
            .and_then(|v| v.get(index).cloned())
    }

    /// Drop the cached page table so the next page lookup rebuilds it. Rarely
    /// needed (the parse is immutable); useful when the page tree was mutated in
    /// place, or to benchmark the build cost.
    pub fn clear_page_table(&self) {
        *self.page_refs.write() = None;
    }

    /// Retrieve any object by its object number.
    ///
    /// Returns `PdfObject::Null` for free or unknown objects.
    pub fn get_object(&self, id: u32) -> Result<PdfObject> {
        if let Some(obj) = self.overrides.read().get(&id) {
            return Ok(obj.clone());
        }
        let (obj, gen) = match self.xref.get(&id) {
            None | Some(XRefEntry::Free) => return Ok(PdfObject::Null),
            Some(XRefEntry::InUse { offset, generation }) => {
                (self.parse_object_at(*offset as usize)?, *generation)
            }
            Some(XRefEntry::Compressed {
                stream_obj_num,
                index,
            }) => (self.get_from_obj_stream(*stream_obj_num, *index)?, 0u16),
        };

        #[cfg(feature = "crypto")]
        if let Some(enc) = &self.enc {
            return decrypt_object_strings(obj, id, gen, enc);
        }

        let _ = gen; // suppress unused warning without crypto
        Ok(obj)
    }

    /// Follow indirect references recursively until a non-Reference is reached.
    ///
    /// Handles chains of the form `A → B → C → value`.
    pub fn resolve(&self, obj: &PdfObject) -> Result<PdfObject> {
        let mut current = obj.clone();
        // Limit chain depth to avoid infinite loops in malformed files.
        for _ in 0..64 {
            match current {
                PdfObject::Reference(id, _gen) => {
                    current = self.get_object(id)?;
                }
                other => return Ok(other),
            }
        }
        Err(PdfError::invalid_token(
            0,
            "indirect reference chain exceeds depth limit (possible cycle)",
        ))
    }

    /// Return a reference to the raw PDF bytes backing this document.
    ///
    /// Required by the signature verifier to hash the signed byte ranges
    /// (ISO 32000-1 §12.8.1 — the `/ByteRange` covers specific regions of the
    /// original byte stream, not a re-serialized form).
    pub fn raw_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Whether the document carries at least one digital signature.
    ///
    /// Detected via the catalog's `/AcroForm /SigFlags`, bit 1
    /// (`SignaturesExist`, ISO 32000-1 §12.7.2). Editing a signed PDF and
    /// re-serializing changed content invalidates the signature, so callers
    /// (e.g. the text editor) should warn the user before proceeding.
    pub fn is_signed(&self) -> bool {
        let Some(root_ref) = self.trailer.get("Root") else {
            return false;
        };
        let Ok(root) = self.resolve(root_ref) else {
            return false;
        };
        let Some(acro_ref) = root.as_dict().and_then(|c| c.get("AcroForm")) else {
            return false;
        };
        let Ok(acroform) = self.resolve(acro_ref) else {
            return false;
        };
        acroform
            .as_dict()
            .and_then(|af| af.get("SigFlags"))
            .and_then(|f| f.as_integer())
            .is_some_and(|flags| flags & 1 != 0)
    }

    /// Decode the content of a stream object identified by `id`.
    ///
    /// Results are cached by object ID so the same stream (e.g. a page content
    /// stream decoded for every render tile) is only decompressed once per
    /// document load.
    pub fn get_stream_data(&self, id: u32) -> Result<Vec<u8>> {
        // Fast path: return a clone of the already-decoded bytes.
        if let Some(cached) = self.decoded_stream_cache.read().get(&id) {
            return Ok(cached.clone());
        }
        // Decode without holding the lock (decompression is the expensive part,
        // and parallel render threads may all miss at once).
        let decoded = self.decode_stream_uncached(id)?;
        // Double-checked insert: re-check under the write lock so concurrent
        // threads that decoded the same stream don't clobber each other and we
        // hand back a single shared copy.
        let mut cache = self.decoded_stream_cache.write();
        Ok(cache.entry(id).or_insert(decoded).clone())
    }

    /// Decode a stream without consulting or populating the stream cache.
    fn decode_stream_uncached(&self, id: u32) -> Result<Vec<u8>> {
        #[cfg_attr(not(feature = "crypto"), allow(unused_variables))]
        let gen = match self.xref.get(&id) {
            Some(XRefEntry::InUse { generation, .. }) => *generation,
            _ => 0u16,
        };
        match self.get_object(id)? {
            PdfObject::Stream(s) => {
                #[cfg(feature = "crypto")]
                if let Some(enc) = &self.enc {
                    let decrypted = enc.decrypt_stream(id, gen, &s.raw_data)?;
                    return filters::apply_pipeline(
                        &s.filter_names()
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>(),
                        &decrypted,
                    );
                }
                s.decode_with_doc(self)
            }
            other => Err(PdfError::invalid_token(
                0,
                format!("object {} is not a stream (found {:?})", id, other),
            )),
        }
    }

    /// Total number of pages in the document.
    pub fn page_count(&self) -> Result<usize> {
        let root = self
            .trailer
            .get("Root")
            .ok_or_else(|| PdfError::invalid_token(0, "trailer missing /Root"))?
            .clone();
        let catalog = self.resolve(&root)?;
        let pages_ref = catalog
            .as_dict()
            .and_then(|d| d.get("Pages"))
            .ok_or_else(|| PdfError::invalid_token(0, "/Catalog missing /Pages"))?
            .clone();
        let pages = self.resolve(&pages_ref)?;
        let count_obj = pages
            .as_dict()
            .and_then(|d| d.get("Count"))
            .ok_or_else(|| PdfError::invalid_token(0, "page tree /Count missing"))?
            .clone();
        let count = self.resolve(&count_obj)?;
        match count {
            PdfObject::Integer(n) => Ok(n as usize),
            other => Err(PdfError::invalid_token(
                0,
                format!("page tree /Count is not an integer: {:?}", other),
            )),
        }
    }

    /// Return the highest object number present in this document's XRef.
    ///
    /// Used by the editor to allocate new object IDs that don't collide with
    /// existing ones.
    pub fn max_object_id(&self) -> u32 {
        self.xref.keys().copied().max().unwrap_or(0)
    }

    /// Returns all object IDs present in the cross-reference table, excluding
    /// the free-object head (ID 0). Used by `save_new()` to enumerate original
    /// objects for a full document rewrite.
    pub fn all_object_ids(&self) -> Vec<u32> {
        self.xref.keys().copied().filter(|&id| id != 0).collect()
    }

    /// Return the byte offset of the last `startxref` value in the raw file.
    ///
    /// This is the `/Prev` pointer to write into the new trailer when
    /// appending an incremental update.
    pub fn startxref_offset(data: &[u8]) -> Result<usize> {
        find_startxref_offset(data)
    }

    /// Access the raw file bytes.
    pub fn raw_data(&self) -> &[u8] {
        &self.data
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Parse the indirect object whose header starts at `offset`.
    fn parse_object_at(&self, offset: usize) -> Result<PdfObject> {
        if offset >= self.data.len() {
            return Err(PdfError::eof(offset, "object offset out of file bounds"));
        }
        let slice = &self.data[offset..];
        let slice = skip_pdf_whitespace(slice);
        let mut lexer = Lexer::new(slice);

        // Consume "obj_num gen_num obj"
        match lexer.next_token()? {
            Token::Integer(_) => {}
            t => {
                return Err(PdfError::invalid_token(
                    offset,
                    format!("expected object number, found {:?}", t),
                ))
            }
        }
        match lexer.next_token()? {
            Token::Integer(_) => {}
            t => {
                return Err(PdfError::invalid_token(
                    offset,
                    format!("expected generation number, found {:?}", t),
                ))
            }
        }
        match lexer.next_token()? {
            Token::Keyword(Keyword::Obj) => {}
            t => {
                return Err(PdfError::invalid_token(
                    offset,
                    format!("expected 'obj' keyword, found {:?}", t),
                ))
            }
        }

        let value = parse_object_from_lexer(&mut lexer)?;

        // If the body is a dictionary, check whether a stream follows.
        let obj = match value {
            PdfObject::Dictionary(dict) => {
                match lexer.peek_token() {
                    Ok(Token::Keyword(Keyword::Stream)) => {
                        lexer.next_token()?; // consume "stream"

                        // After "stream" the PDF spec requires a single line ending
                        // (\n or \r\n). Skip it. — ISO 32000-1 §7.3.8.1
                        let stream_body = &slice[lexer.position()..];
                        let eol_skip = skip_stream_eol(stream_body);
                        let data_start = lexer.position() + eol_skip;

                        // Resolve /Length (may be an indirect reference).
                        let length = self.resolve_stream_length(&dict, offset)?;

                        let raw_data = if data_start + length > slice.len() {
                            let available = slice.len().saturating_sub(data_start);
                            log::warn!(
                                "stream /Length {} exceeds available bytes ({}), truncating",
                                length,
                                available
                            );
                            slice[data_start..data_start + available].to_vec()
                        } else {
                            slice[data_start..data_start + length].to_vec()
                        };
                        PdfObject::Stream(Box::new(PdfStream { dict, raw_data }))
                    }
                    _ => PdfObject::Dictionary(dict),
                }
            }
            other => other,
        };

        Ok(obj)
    }

    /// Resolve the /Length entry of a stream dictionary.
    /// /Length may itself be an indirect reference (e.g. `5 0 R`).
    fn resolve_stream_length(&self, dict: &PdfDict, ctx_offset: usize) -> Result<usize> {
        let len_obj = dict
            .get("Length")
            .ok_or_else(|| {
                PdfError::invalid_token(ctx_offset, "stream dictionary missing /Length")
            })?
            .clone();
        let resolved = self.resolve(&len_obj)?;
        match resolved {
            PdfObject::Integer(n) if n >= 0 => Ok(n as usize),
            other => Err(PdfError::invalid_token(
                ctx_offset,
                format!("stream /Length is not a non-negative integer: {:?}", other),
            )),
        }
    }

    /// Retrieve an object from an object stream (PDF 1.5+, XRef type-2 entries).
    fn get_from_obj_stream(&self, stream_obj_num: u32, index: u32) -> Result<PdfObject> {
        // Fast path: cache hit
        if let Some(objects) = self.obj_stream_cache.read().get(&stream_obj_num) {
            return Ok(objects
                .get(index as usize)
                .cloned()
                .unwrap_or(PdfObject::Null));
        }

        // Load and decompress the object stream.
        let stream = match self.get_object(stream_obj_num)? {
            PdfObject::Stream(s) => s,
            other => {
                return Err(PdfError::invalid_token(
                    0,
                    format!(
                        "object stream {} is not a stream: {:?}",
                        stream_obj_num, other
                    ),
                ))
            }
        };

        let n = match stream.dict.get("N") {
            Some(PdfObject::Integer(n)) if *n >= 0 => *n as usize,
            _ => {
                return Err(PdfError::invalid_token(
                    0,
                    format!("object stream {} missing /N", stream_obj_num),
                ))
            }
        };
        let first = match stream.dict.get("First") {
            Some(PdfObject::Integer(f)) if *f >= 0 => *f as usize,
            _ => {
                return Err(PdfError::invalid_token(
                    0,
                    format!("object stream {} missing /First", stream_obj_num),
                ))
            }
        };

        // Use get_stream_data (not stream.decode) so encrypted PDFs are decrypted first.
        let decoded = self.get_stream_data(stream_obj_num)?;

        // Parse the header section: N pairs of (obj_num, byte_offset_from_First).
        let mut lexer = Lexer::new(&decoded);
        let mut offsets: Vec<(u32, usize)> = Vec::with_capacity(n);
        for _ in 0..n {
            let obj_num = match lexer.next_token()? {
                Token::Integer(i) if i >= 0 => i as u32,
                t => {
                    return Err(PdfError::invalid_token(
                        0,
                        format!("bad object stream header token: {:?}", t),
                    ))
                }
            };
            let rel_offset = match lexer.next_token()? {
                Token::Integer(i) if i >= 0 => i as usize,
                t => {
                    return Err(PdfError::invalid_token(
                        0,
                        format!("bad object stream offset token: {:?}", t),
                    ))
                }
            };
            offsets.push((obj_num, first + rel_offset));
        }

        // Parse each embedded object.
        let mut objects: Vec<PdfObject> = Vec::with_capacity(n);
        for &(_, abs_offset) in &offsets {
            if abs_offset > decoded.len() {
                return Err(PdfError::eof(
                    abs_offset,
                    "object stream embedded object offset out of bounds",
                ));
            }
            let mut inner = Lexer::new(&decoded[abs_offset..]);
            let obj = parse_object_from_lexer(&mut inner)?;
            objects.push(obj);
        }

        let result = objects
            .get(index as usize)
            .cloned()
            .unwrap_or(PdfObject::Null);
        self.obj_stream_cache
            .write()
            .insert(stream_obj_num, objects);
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Object parser (structural context — not content streams)
// ---------------------------------------------------------------------------

/// Parse a single PDF value from `lexer`.
///
/// Handles all structural object types including indirect references
/// (`n g R`) using one token of speculative lookahead.
pub(crate) fn parse_object_from_lexer(lexer: &mut Lexer) -> Result<PdfObject> {
    let token = lexer.next_token()?;
    match token {
        Token::Null => Ok(PdfObject::Null),
        Token::Boolean(b) => Ok(PdfObject::Boolean(b)),
        Token::Real(r) => Ok(PdfObject::Real(r)),
        Token::Name(n) => Ok(PdfObject::Name(n)),
        Token::LiteralString(s) | Token::HexString(s) => Ok(PdfObject::String(s)),

        Token::Integer(n) => {
            // Speculative lookahead for "n gen R" indirect reference.
            let pos_after_n = lexer.position();
            if let Ok(Token::Integer(gen)) = lexer.peek_token() {
                if gen >= 0 {
                    lexer.next_token()?; // consume gen
                    if let Ok(Token::Keyword(Keyword::R)) = lexer.peek_token() {
                        lexer.next_token()?; // consume R
                        return Ok(PdfObject::Reference(n as u32, gen as u16));
                    }
                    // Not a reference — backtrack past gen.
                    lexer.set_position(pos_after_n);
                }
            }
            Ok(PdfObject::Integer(n))
        }

        Token::ArrayStart => {
            let mut arr = Vec::new();
            loop {
                match lexer.peek_token()? {
                    Token::ArrayEnd => {
                        lexer.next_token()?;
                        break;
                    }
                    Token::Eof => {
                        return Err(PdfError::eof(lexer.position(), "unterminated array"))
                    }
                    _ => arr.push(parse_object_from_lexer(lexer)?),
                }
            }
            Ok(PdfObject::Array(arr))
        }

        Token::DictStart => {
            let mut dict = PdfDict::new();
            loop {
                match lexer.peek_token()? {
                    Token::DictEnd => {
                        lexer.next_token()?;
                        break;
                    }
                    Token::Eof => {
                        return Err(PdfError::eof(lexer.position(), "unterminated dictionary"))
                    }
                    _ => {}
                }
                let key = match lexer.next_token()? {
                    Token::Name(n) => n,
                    t => {
                        return Err(PdfError::invalid_token(
                            lexer.position(),
                            format!("expected dictionary key name, found {:?}", t),
                        ))
                    }
                };
                let val = parse_object_from_lexer(lexer)?;
                dict.insert(key, val);
            }
            Ok(PdfObject::Dictionary(dict))
        }

        Token::Eof => Err(PdfError::eof(lexer.position(), "unexpected EOF in object")),

        // Streams are only valid as the body of an indirect object; the
        // parse_object_at method handles the "stream … endstream" wrapper.
        // Operators and unknown keywords are not valid in structural context.
        t => Err(PdfError::invalid_token(
            lexer.position(),
            format!("unexpected token in structural context: {:?}", t),
        )),
    }
}

// ---------------------------------------------------------------------------
// XRef loading: find startxref, chain all sections
// ---------------------------------------------------------------------------

/// Extract an integer value from a PdfObject (Integer or Real).
fn pdf_int_from_obj(obj: &PdfObject) -> Option<i64> {
    match obj {
        PdfObject::Integer(n) => Some(*n),
        PdfObject::Real(r) => Some(*r as i64),
        _ => None,
    }
}

/// Apply the predictor specified in a `/DecodeParms` dictionary to `data`.
fn apply_predictor(mut data: Vec<u8>, parms: &PdfDict) -> Result<Vec<u8>> {
    let predictor = parms
        .get("Predictor")
        .and_then(pdf_int_from_obj)
        .unwrap_or(1);
    if predictor >= 10 {
        let columns = parms.get("Columns").and_then(pdf_int_from_obj).unwrap_or(1) as usize;
        let colors = parms.get("Colors").and_then(pdf_int_from_obj).unwrap_or(1) as usize;
        let bpc = parms
            .get("BitsPerComponent")
            .and_then(pdf_int_from_obj)
            .unwrap_or(8) as usize;
        log::debug!(
            "[predictor] PNG pred={} cols={} colors={} bpc={} input={}B",
            predictor,
            columns,
            colors,
            bpc,
            data.len()
        );
        data = filters::apply_png_predictor(&data, columns, colors, bpc)?;
    } else if predictor == 2 {
        let columns = parms.get("Columns").and_then(pdf_int_from_obj).unwrap_or(1) as usize;
        let colors = parms.get("Colors").and_then(pdf_int_from_obj).unwrap_or(1) as usize;
        let bpc = parms
            .get("BitsPerComponent")
            .and_then(pdf_int_from_obj)
            .unwrap_or(8) as usize;
        data = filters::apply_tiff_predictor(&data, columns, colors, bpc)?;
    }
    Ok(data)
}

/// Cheaply detect if `data` is an encrypted PDF.
/// Only parses the XRef trailer — no objects are loaded or decrypted.
pub fn has_encryption_trailer(data: &[u8]) -> Result<bool> {
    let start = find_startxref_offset(data)?;
    let (_xref, trailer) = build_xref(data, start)?;
    Ok(trailer.contains_key("Encrypt"))
}

/// Search the last 1024 bytes for `startxref` and return the XRef offset.
fn find_startxref_offset(data: &[u8]) -> Result<usize> {
    let scan_from = data.len().saturating_sub(1024);
    let tail = &data[scan_from..];
    let keyword = b"startxref";

    // Find the last (rightmost) occurrence — incremental updates add more.
    let rel_pos = (0..=tail.len().saturating_sub(keyword.len()))
        .rev()
        .find(|&i| &tail[i..i + keyword.len()] == keyword)
        .ok_or_else(|| {
            PdfError::invalid_token(data.len(), "'startxref' not found in last 1024 bytes")
        })?;

    let after = skip_pdf_whitespace(&tail[rel_pos + keyword.len()..]);
    let num_end = after
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .unwrap_or(after.len());

    if num_end == 0 {
        return Err(PdfError::invalid_token(
            data.len(),
            "startxref offset is missing or zero-length",
        ));
    }

    let s = std::str::from_utf8(&after[..num_end])
        .map_err(|_| PdfError::invalid_token(0, "startxref offset is not ASCII"))?;
    s.parse::<usize>()
        .map_err(|_| PdfError::invalid_token(0, format!("invalid startxref offset: '{}'", s)))
}

/// Build the unified XRef map and final trailer by following the `/Prev` chain.
///
/// Sections are processed newest-first; entries from newer sections take
/// precedence (the first `insert` per object-id wins via `entry().or_insert`).
/// Parse the value of an indirect object at a raw byte offset.
///
/// Consumes `N G obj` then returns the parsed value, without stream handling.
/// Used to resolve indirect references before a `PdfDocument` is constructed.
#[cfg(feature = "crypto")]
fn parse_object_at_offset(data: &[u8], offset: usize) -> Result<PdfObject> {
    if offset >= data.len() {
        return Err(PdfError::eof(offset, "object offset out of file bounds"));
    }
    let slice = skip_pdf_whitespace(&data[offset..]);
    let mut lexer = Lexer::new(slice);
    match lexer.next_token()? {
        Token::Integer(_) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset,
                format!("expected object number, found {:?}", t),
            ))
        }
    }
    match lexer.next_token()? {
        Token::Integer(_) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset,
                format!("expected generation number, found {:?}", t),
            ))
        }
    }
    match lexer.next_token()? {
        Token::Keyword(Keyword::Obj) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset,
                format!("expected 'obj', found {:?}", t),
            ))
        }
    }
    parse_object_from_lexer(&mut lexer)
}

fn build_xref(data: &[u8], initial_offset: usize) -> Result<(HashMap<u32, XRefEntry>, PdfDict)> {
    let mut xref: HashMap<u32, XRefEntry> = HashMap::new();
    let mut final_trailer: PdfDict = PdfDict::new();
    let mut offset = initial_offset;
    let mut visited: HashSet<usize> = HashSet::new();

    loop {
        if !visited.insert(offset) {
            log::warn!("XRef chain cycle detected at offset {}, stopping", offset);
            break;
        }
        if offset >= data.len() {
            return Err(PdfError::eof(offset, "XRef offset beyond end of file"));
        }

        let slice = skip_pdf_whitespace(&data[offset..]);
        let (entries, trailer, prev) = if slice.starts_with(b"xref") {
            parse_traditional_xref(data, offset)?
        } else {
            parse_xref_stream(data, offset)?
        };

        // Newer entries win: use entry().or_insert so earlier (newer) inserts prevail.
        for (id, entry) in entries {
            xref.entry(id).or_insert(entry);
        }

        // Keep the newest trailer; merge older trailers for any keys we lack.
        if final_trailer.is_empty() {
            final_trailer = trailer;
        } else {
            for (k, v) in trailer {
                final_trailer.entry(k).or_insert(v);
            }
        }

        match prev {
            Some(p) if p < data.len() && p != offset => offset = p,
            _ => break,
        }
    }

    Ok((xref, final_trailer))
}

// ---------------------------------------------------------------------------
// Traditional XRef table parser
// ---------------------------------------------------------------------------

/// Parse one XRef entry by scanning fields character by character.
///
/// Returns `(offset, generation, is_in_use, bytes_consumed)`.
/// Handles all real-world EOL variants: `\r\n`, `\r`, `\n`, ` \r\n`, ` \r`, ` \n`.
fn parse_xref_entry_bytes(input: &[u8]) -> Option<(u64, u16, bool, usize)> {
    let mut pos = 0;

    // Read decimal digits for byte offset (up to 10).
    let offset_start = pos;
    while pos < input.len() && input[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos == offset_start {
        return None;
    }
    let obj_offset: u64 = std::str::from_utf8(&input[offset_start..pos])
        .ok()?
        .parse()
        .ok()?;

    // Skip one space separator.
    if pos >= input.len() || input[pos] != b' ' {
        return None;
    }
    pos += 1;

    // Read decimal digits for generation number (up to 5).
    let gen_start = pos;
    while pos < input.len() && input[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos == gen_start {
        return None;
    }
    let gen: u16 = std::str::from_utf8(&input[gen_start..pos])
        .ok()?
        .parse()
        .ok()?;

    // Skip one space separator.
    if pos >= input.len() || input[pos] != b' ' {
        return None;
    }
    pos += 1;

    // Read the type byte: 'n' (in-use) or 'f' (free).
    if pos >= input.len() {
        return None;
    }
    let is_in_use = match input[pos] {
        b'n' => true,
        b'f' => false,
        _ => return None,
    };
    pos += 1;

    // Consume all trailing EOL/whitespace (space, CR, LF) — xpdf style.
    // This handles: \r\n (20 bytes), ' '\r\n (21 bytes), ' '\r, ' '\n, \n, \r, etc.
    while pos < input.len() && (input[pos] == b' ' || input[pos] == b'\r' || input[pos] == b'\n') {
        pos += 1;
    }

    Some((obj_offset, gen, is_in_use, pos))
}

fn parse_traditional_xref(
    data: &[u8],
    offset: usize,
) -> Result<(HashMap<u32, XRefEntry>, PdfDict, Option<usize>)> {
    let slice = &data[offset..];
    let slice = skip_pdf_whitespace(slice);

    if !slice.starts_with(b"xref") {
        return Err(PdfError::invalid_token(offset, "expected 'xref' keyword"));
    }
    let mut cur = skip_pdf_whitespace(&slice[4..]);
    let mut entries: HashMap<u32, XRefEntry> = HashMap::new();

    // Parse subsections until "trailer"
    while !cur.is_empty() && !cur.starts_with(b"trailer") {
        let (start_id, count, rest) = parse_subsection_header(cur, offset)?;
        cur = rest;

        for i in 0..count {
            if cur.len() < 18 {
                return Err(PdfError::eof(offset, "XRef entry truncated"));
            }

            let (obj_offset, gen, is_in_use, consumed) = parse_xref_entry_bytes(cur)
                .ok_or_else(|| PdfError::invalid_token(offset, "malformed XRef entry"))?;

            let entry = if is_in_use {
                XRefEntry::InUse {
                    offset: obj_offset,
                    generation: gen,
                }
            } else {
                XRefEntry::Free
            };
            entries.insert(start_id + i, entry);

            cur = &cur[consumed.min(cur.len())..];
        }
        cur = skip_pdf_whitespace(cur);
    }

    // Parse "trailer << … >>"
    if !cur.starts_with(b"trailer") {
        return Err(PdfError::invalid_token(
            offset,
            "XRef table not followed by 'trailer'",
        ));
    }
    cur = skip_pdf_whitespace(&cur[7..]);
    let mut lexer = Lexer::new(cur);
    let trailer = match parse_object_from_lexer(&mut lexer)? {
        PdfObject::Dictionary(d) => d,
        other => {
            return Err(PdfError::invalid_token(
                offset,
                format!("trailer is not a dictionary: {:?}", other),
            ))
        }
    };

    let prev = match trailer.get("Prev") {
        Some(PdfObject::Integer(n)) if *n >= 0 => Some(*n as usize),
        _ => None,
    };

    Ok((entries, trailer, prev))
}

fn parse_subsection_header(input: &[u8], ctx: usize) -> Result<(u32, u32, &[u8])> {
    let space_pos = input
        .iter()
        .position(|&b| b == b' ')
        .ok_or_else(|| PdfError::invalid_token(ctx, "XRef subsection header missing space"))?;
    let start_id = parse_decimal_u32(&input[..space_pos], ctx)?;

    let rest = &input[space_pos + 1..];
    let nl_pos = rest
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .ok_or_else(|| PdfError::invalid_token(ctx, "XRef subsection header missing newline"))?;
    let count = parse_decimal_u32(&rest[..nl_pos], ctx)?;

    // Skip the newline (may be \r\n or \n)
    let after_nl = &rest[nl_pos..];
    let after_nl = if after_nl.starts_with(b"\r\n") {
        &after_nl[2..]
    } else {
        &after_nl[1..]
    };

    Ok((start_id, count, after_nl))
}

// ---------------------------------------------------------------------------
// XRef stream parser (PDF 1.5+, ISO 32000-1 §7.5.8)
// ---------------------------------------------------------------------------

fn parse_xref_stream(
    data: &[u8],
    offset: usize,
) -> Result<(HashMap<u32, XRefEntry>, PdfDict, Option<usize>)> {
    if offset >= data.len() {
        return Err(PdfError::eof(offset, "XRef stream offset out of bounds"));
    }
    let slice = skip_pdf_whitespace(&data[offset..]);
    let mut lexer = Lexer::new(slice);

    // Consume "obj_num gen_num obj"
    match lexer.next_token()? {
        Token::Integer(_) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset,
                format!("XRef stream: expected obj_num, found {:?}", t),
            ))
        }
    }
    match lexer.next_token()? {
        Token::Integer(_) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset,
                format!("XRef stream: expected gen_num, found {:?}", t),
            ))
        }
    }
    match lexer.next_token()? {
        Token::Keyword(Keyword::Obj) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset,
                format!("XRef stream: expected 'obj', found {:?}", t),
            ))
        }
    }

    let dict = match parse_object_from_lexer(&mut lexer)? {
        PdfObject::Dictionary(d) => d,
        other => {
            return Err(PdfError::invalid_token(
                offset,
                format!("XRef stream body is not a dictionary: {:?}", other),
            ))
        }
    };

    // Verify /Type /XRef
    match dict.get("Type") {
        Some(PdfObject::Name(t)) if t == "XRef" => {}
        _ => {
            return Err(PdfError::invalid_token(
                offset,
                "XRef stream /Type is not /XRef",
            ))
        }
    }

    // Consume "stream" keyword and mandatory EOL
    match lexer.next_token()? {
        Token::Keyword(Keyword::Stream) => {}
        t => {
            return Err(PdfError::invalid_token(
                offset,
                format!("expected 'stream', found {:?}", t),
            ))
        }
    }
    let stream_body = &slice[lexer.position()..];
    let eol_skip = skip_stream_eol(stream_body);
    let data_start = lexer.position() + eol_skip;

    // /Length must be an integer (no indirect ref resolution available here
    // since we're still building the XRef map).
    let length = match dict.get("Length") {
        Some(PdfObject::Integer(n)) if *n >= 0 => *n as usize,
        _ => {
            return Err(PdfError::invalid_token(
                offset,
                "XRef stream missing/invalid /Length",
            ))
        }
    };

    if data_start + length > slice.len() {
        return Err(PdfError::eof(
            offset + data_start,
            "XRef stream data exceeds available bytes",
        ));
    }

    let raw_data = &slice[data_start..data_start + length];

    // Decompress via filter pipeline.
    let filter_names = match dict.get("Filter") {
        Some(PdfObject::Name(n)) => vec![n.as_str()],
        Some(PdfObject::Array(arr)) => arr
            .iter()
            .filter_map(|o| match o {
                PdfObject::Name(n) => Some(n.as_str()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    };
    let decoded = filters::apply_pipeline(&filter_names, raw_data)?;

    // Apply predictor (e.g. PNG predictor 12 is common in XRef streams).
    let decoded = match dict.get("DecodeParms") {
        Some(PdfObject::Dictionary(parms)) => apply_predictor(decoded, parms)?,
        Some(PdfObject::Array(arr)) => {
            if let Some(parms) = arr.iter().find_map(|o| match o {
                PdfObject::Dictionary(d) => Some(d),
                _ => None,
            }) {
                apply_predictor(decoded, parms)?
            } else {
                decoded
            }
        }
        _ => decoded,
    };

    // Parse /W field widths
    let w = match dict.get("W") {
        Some(PdfObject::Array(arr)) if arr.len() == 3 => arr,
        _ => {
            return Err(PdfError::invalid_token(
                offset,
                "XRef stream missing/invalid /W",
            ))
        }
    };
    let w0 = xref_field_width(&w[0], offset)?;
    let w1 = xref_field_width(&w[1], offset)?;
    let w2 = xref_field_width(&w[2], offset)?;
    let entry_width = w0 + w1 + w2;
    if entry_width == 0 {
        return Err(PdfError::invalid_token(
            offset,
            "XRef stream /W entry width is zero",
        ));
    }

    // Parse /Index
    let size = match dict.get("Size") {
        Some(PdfObject::Integer(s)) if *s >= 0 => *s as u32,
        _ => {
            return Err(PdfError::invalid_token(
                offset,
                "XRef stream missing/invalid /Size",
            ))
        }
    };
    let index_pairs: Vec<(u32, u32)> = match dict.get("Index") {
        Some(PdfObject::Array(arr)) => {
            if arr.len() % 2 != 0 {
                return Err(PdfError::invalid_token(
                    offset,
                    "XRef stream /Index has odd length",
                ));
            }
            arr.chunks_exact(2)
                .map(|c| {
                    let s = match &c[0] {
                        PdfObject::Integer(n) if *n >= 0 => *n as u32,
                        _ => return Err(PdfError::invalid_token(offset, "invalid /Index start")),
                    };
                    let cnt = match &c[1] {
                        PdfObject::Integer(n) if *n >= 0 => *n as u32,
                        _ => return Err(PdfError::invalid_token(offset, "invalid /Index count")),
                    };
                    Ok((s, cnt))
                })
                .collect::<Result<Vec<_>>>()?
        }
        _ => vec![(0, size)],
    };

    let mut entries: HashMap<u32, XRefEntry> = HashMap::new();
    let mut ptr = 0usize;

    for (start_id, count) in index_pairs {
        for i in 0..count {
            let obj_id = start_id + i;
            if ptr + entry_width > decoded.len() {
                return Err(PdfError::eof(
                    offset,
                    format!("XRef stream too short for {} entries", count),
                ));
            }

            let field_type = if w0 > 0 {
                read_be_uint(&decoded[ptr..ptr + w0])
            } else {
                1 // default type is 1 (in-use)
            };
            let f2 = read_be_uint(&decoded[ptr + w0..ptr + w0 + w1]);
            let f3 = if w2 > 0 {
                read_be_uint(&decoded[ptr + w0 + w1..ptr + w0 + w1 + w2])
            } else {
                0
            };
            ptr += entry_width;

            let entry = match field_type {
                0 => XRefEntry::Free,
                1 => XRefEntry::InUse {
                    offset: f2,
                    generation: f3 as u16,
                },
                2 => XRefEntry::Compressed {
                    stream_obj_num: f2 as u32,
                    index: f3 as u32,
                },
                t => {
                    log::warn!("Unknown XRef stream entry type {} for obj {}", t, obj_id);
                    XRefEntry::Free
                }
            };
            entries.insert(obj_id, entry);
        }
    }

    let prev = match dict.get("Prev") {
        Some(PdfObject::Integer(n)) if *n >= 0 => Some(*n as usize),
        _ => None,
    };

    Ok((entries, dict, prev))
}

// ---------------------------------------------------------------------------
// Small byte-level helpers
// ---------------------------------------------------------------------------

/// Skip PDF whitespace bytes (ISO 32000-1 Table 1).
pub(crate) fn skip_pdf_whitespace(input: &[u8]) -> &[u8] {
    let mut s = input;
    loop {
        let start = s.len();
        while !s.is_empty() && matches!(s[0], 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20) {
            s = &s[1..];
        }
        // Skip comments
        if s.starts_with(b"%") {
            s = &s[1..];
            while !s.is_empty() && s[0] != b'\r' && s[0] != b'\n' {
                s = &s[1..];
            }
            if s.starts_with(b"\r\n") {
                s = &s[2..];
            } else if !s.is_empty() {
                s = &s[1..];
            }
        }
        if s.len() == start {
            break;
        }
    }
    s
}

/// Skip the mandatory line ending after the `stream` keyword.
/// Returns the number of bytes to skip (1 for \n or \r, 2 for \r\n).
fn skip_stream_eol(data: &[u8]) -> usize {
    match data.first() {
        Some(b'\n') => 1,
        Some(b'\r') => {
            if data.get(1) == Some(&b'\n') {
                2
            } else {
                1
            }
        }
        // Some malformed files have a space before the newline
        Some(b' ') => 1 + skip_stream_eol(&data[1..]),
        _ => 0,
    }
}

/// Read a big-endian unsigned integer from `bytes` (up to 8 bytes).
fn read_be_uint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64)
}

fn xref_field_width(obj: &PdfObject, ctx: usize) -> Result<usize> {
    match obj {
        PdfObject::Integer(n) if *n >= 0 => Ok(*n as usize),
        _ => Err(PdfError::invalid_token(
            ctx,
            "XRef /W element is not a non-negative integer",
        )),
    }
}

fn parse_decimal_u32(bytes: &[u8], ctx: usize) -> Result<u32> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| PdfError::invalid_token(ctx, "XRef field is not ASCII"))?;
    s.trim()
        .parse::<u32>()
        .map_err(|_| PdfError::invalid_token(ctx, format!("invalid XRef field '{}'", s)))
}

// ---------------------------------------------------------------------------
// Encryption helpers
// ---------------------------------------------------------------------------

/// Extract the first element of the /ID array from the trailer as raw bytes.
#[cfg(feature = "crypto")]
fn extract_file_id(trailer: &PdfDict) -> Vec<u8> {
    match trailer.get("ID") {
        Some(PdfObject::Array(arr)) => match arr.first() {
            Some(PdfObject::String(b)) => b.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Recursively walk an object and decrypt all embedded string values.
#[cfg(feature = "crypto")]
fn decrypt_object_strings(
    obj: PdfObject,
    obj_num: u32,
    gen: u16,
    enc: &crate::crypto::EncryptionHandler,
) -> Result<PdfObject> {
    match obj {
        PdfObject::String(mut bytes) => {
            enc.decrypt_string(obj_num, gen, &mut bytes)?;
            Ok(PdfObject::String(bytes))
        }
        PdfObject::Array(arr) => {
            let decrypted: Result<Vec<_>> = arr
                .into_iter()
                .map(|o| decrypt_object_strings(o, obj_num, gen, enc))
                .collect();
            Ok(PdfObject::Array(decrypted?))
        }
        PdfObject::Dictionary(dict) => {
            let decrypted: Result<IndexMap<_, _>> = dict
                .into_iter()
                .map(|(k, v)| Ok((k, decrypt_object_strings(v, obj_num, gen, enc)?)))
                .collect();
            Ok(PdfObject::Dictionary(decrypted?))
        }
        PdfObject::Stream(mut stream) => {
            let dict =
                decrypt_object_strings(PdfObject::Dictionary(stream.dict), obj_num, gen, enc)?;
            stream.dict = match dict {
                PdfObject::Dictionary(d) => d,
                _ => unreachable!(),
            };
            // Raw stream data is decrypted separately in get_stream_data.
            Ok(PdfObject::Stream(stream))
        }
        other => Ok(other),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // Minimal single-page PDF with a traditional XRef table (no compression).
    // Generated by hand; validates: parse → page_count == 1.
    #[allow(dead_code)]
    const MINIMAL_PDF: &[u8] = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n\
xref\n\
0 4\n\
0000000000 65535 f \r\n\
0000000009 00000 n \r\n\
0000000058 00000 n \r\n\
0000000115 00000 n \r\n\
trailer\n\
<< /Size 4 /Root 1 0 R >>\n\
startxref\n\
190\n\
%%EOF\n";

    #[test]
    fn pdf_document_is_send_and_sync() {
        // TD-2: with parking_lot::RwLock caches, PdfDocument is Send + Sync so
        // it can be shared across render threads (rayon / WASM atomics). This is
        // a compile-time guarantee — it fails to build if a `!Sync` field (like
        // the old RefCell) is reintroduced.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PdfDocument>();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_reads_populate_caches_safely() {
        // Exercise the RwLock caches under contention: many threads share &doc
        // and hammer get_object / page lookups concurrently. Would deadlock or
        // race if the locking were wrong; double-checked decode keeps it correct.
        let bytes = std::fs::read(format!(
            "{}/tests/fixtures/minimal.pdf",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..200 {
                        let _ = doc.get_object(1);
                        let _ = doc.get_object(3);
                        let _ = doc.page_count();
                    }
                });
            }
        });
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn overrides_shadow_then_restore_objects() {
        let bytes = std::fs::read(format!(
            "{}/tests/fixtures/minimal.pdf",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let doc = PdfDocument::parse(bytes).unwrap();
        // A high id with no xref entry resolves to Null in the pristine parse.
        assert_eq!(doc.get_object(999_999).unwrap(), PdfObject::Null);

        // Overlay an object for that id (no byte reparse).
        let mut ov = HashMap::new();
        ov.insert(999_999u32, PdfObject::Integer(7));
        doc.set_overrides(ov);
        assert_eq!(doc.get_object(999_999).unwrap(), PdfObject::Integer(7));

        // Clearing restores the pristine view (the id is Null again).
        doc.clear_overrides();
        assert_eq!(doc.get_object(999_999).unwrap(), PdfObject::Null);
    }

    #[test]
    fn test_parse_object_null() {
        let mut lex = Lexer::new(b"null");
        assert_eq!(parse_object_from_lexer(&mut lex).unwrap(), PdfObject::Null);
    }

    #[test]
    fn test_parse_object_boolean() {
        let mut lex = Lexer::new(b"true");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Boolean(true)
        );
    }

    #[test]
    fn test_parse_object_integer() {
        let mut lex = Lexer::new(b"42");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Integer(42)
        );
    }

    #[test]
    fn test_parse_object_reference() {
        let mut lex = Lexer::new(b"5 0 R");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Reference(5, 0)
        );
    }

    #[test]
    fn test_parse_object_integer_not_reference() {
        // Two integers followed by a non-R should both be parsed as integers.
        let mut lex = Lexer::new(b"1 2");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Integer(1)
        );
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Integer(2)
        );
    }

    #[test]
    fn test_parse_object_name() {
        let mut lex = Lexer::new(b"/Type");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Name("Type".into())
        );
    }

    #[test]
    fn test_parse_object_string() {
        let mut lex = Lexer::new(b"(Hello)");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::String(b"Hello".to_vec())
        );
    }

    #[test]
    fn test_parse_object_array() {
        let mut lex = Lexer::new(b"[1 2 3]");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Integer(2),
                PdfObject::Integer(3),
            ])
        );
    }

    #[test]
    fn test_parse_object_array_with_refs() {
        let mut lex = Lexer::new(b"[1 0 R 2 0 R]");
        assert_eq!(
            parse_object_from_lexer(&mut lex).unwrap(),
            PdfObject::Array(vec![PdfObject::Reference(1, 0), PdfObject::Reference(2, 0),])
        );
    }

    #[test]
    fn test_parse_object_dict() {
        let mut lex = Lexer::new(b"<< /Type /Catalog /Pages 1 0 R >>");
        let obj = parse_object_from_lexer(&mut lex).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("Type"), Some(&PdfObject::Name("Catalog".into())));
            assert_eq!(d.get("Pages"), Some(&PdfObject::Reference(1, 0)));
        } else {
            panic!("expected dictionary");
        }
    }

    #[test]
    fn test_parse_object_nested_dict() {
        let src = b"<< /A << /B 42 >> >>";
        let mut lex = Lexer::new(src);
        let obj = parse_object_from_lexer(&mut lex).unwrap();
        if let PdfObject::Dictionary(outer) = obj {
            if let Some(PdfObject::Dictionary(inner)) = outer.get("A") {
                assert_eq!(inner.get("B"), Some(&PdfObject::Integer(42)));
            } else {
                panic!("expected inner dict");
            }
        } else {
            panic!("expected outer dict");
        }
    }

    #[test]
    fn test_find_startxref_offset() {
        let data = b"...content...\nstartxref\n1234\n%%EOF\n";
        let offset = find_startxref_offset(data).unwrap();
        assert_eq!(offset, 1234);
    }

    #[test]
    fn test_find_startxref_not_found() {
        let data = b"no xref here at all";
        let result = find_startxref_offset(data);
        assert!(result.is_err());
    }

    #[test]
    fn test_skip_pdf_whitespace() {
        assert_eq!(skip_pdf_whitespace(b"  \t\nhello"), b"hello");
        assert_eq!(skip_pdf_whitespace(b"% comment\nworld"), b"world");
        assert_eq!(skip_pdf_whitespace(b"% comment\r\nworld"), b"world");
    }

    #[test]
    fn test_skip_stream_eol_lf() {
        assert_eq!(skip_stream_eol(b"\nhello"), 1);
    }

    #[test]
    fn test_skip_stream_eol_crlf() {
        assert_eq!(skip_stream_eol(b"\r\nhello"), 2);
    }

    #[test]
    fn test_read_be_uint() {
        assert_eq!(read_be_uint(&[0x01, 0x00]), 256);
        assert_eq!(read_be_uint(&[0x00, 0x0A]), 10);
    }

    #[test]
    fn test_pdf_stream_decode_flate() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"Hello stream";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(original).unwrap();
        let compressed = enc.finish().unwrap();

        let mut dict = PdfDict::new();
        dict.insert("Filter".into(), PdfObject::Name("FlateDecode".into()));
        dict.insert("Length".into(), PdfObject::Integer(compressed.len() as i64));
        let stream = PdfStream {
            dict,
            raw_data: compressed,
        };
        let decoded = stream.decode().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn all_object_ids_excludes_zero() {
        let data = std::fs::read(format!(
            "{}/tests/fixtures/minimal.pdf",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let doc = PdfDocument::parse(data).unwrap();
        let ids = doc.all_object_ids();
        assert!(!ids.contains(&0), "ID 0 (free head) must not be returned");
    }

    #[test]
    fn all_object_ids_nonempty() {
        let data = std::fs::read(format!(
            "{}/tests/fixtures/minimal.pdf",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let doc = PdfDocument::parse(data).unwrap();
        assert!(!doc.all_object_ids().is_empty());
    }

    /// Verify that `decode_with_doc` resolves a `/DecodeParms [3 0 R]` entry where
    /// the array element is an indirect reference to the predictor dict (Bug 1 fix).
    #[test]
    fn test_decode_with_doc_decode_parms_array_of_refs() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // 1-row, 2-pixel, grayscale image.  Average predictor (type=3) encoding:
        //   p0: raw=10 - avg(0,0)=0 → 10
        //   p1: raw=20 - avg(recon[0]=10, prev[1]=0)=5 → 15
        // Encoded bytes before zlib: [3, 10, 15]
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&[3u8, 10, 15]).unwrap();
        let compressed = enc.finish().unwrap();
        let clen = compressed.len();

        // Build a minimal PDF where:
        //   obj 3 = << /Predictor 12 /Columns 2 /Colors 1 /BitsPerComponent 8 >>
        //   obj 4 = stream with /DecodeParms [3 0 R]  ← array element is a Reference
        let mut pdf = Vec::<u8>::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Predictor 12 /Columns 2 /Colors 1 /BitsPerComponent 8 >>\nendobj\n",
        );
        let off4 = pdf.len();
        let dict_line = format!(
            "4 0 obj\n<< /Filter /FlateDecode /DecodeParms [3 0 R] /Length {} >>\nstream\n",
            clen
        );
        pdf.extend_from_slice(dict_line.as_bytes());
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_off = pdf.len();
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \r\n\
             {:010} 00000 n \r\n\
             {:010} 00000 n \r\n\
             {:010} 00000 n \r\n\
             {:010} 00000 n \r\n\
             trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            off1, off2, off3, off4, xref_off
        );
        pdf.extend_from_slice(xref.as_bytes());

        let doc = PdfDocument::parse(pdf).unwrap();
        let s = match doc.get_object(4).unwrap() {
            PdfObject::Stream(s) => s,
            other => panic!("object 4 should be a stream, got {:?}", other),
        };

        let decoded = s.decode_with_doc(&doc).unwrap();
        // Average predictor: recon[0]=10, recon[1]=15+floor((10+0)/2)=15+5=20
        assert_eq!(decoded, vec![10u8, 20]);
    }
}
