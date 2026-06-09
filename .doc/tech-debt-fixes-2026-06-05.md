# pdf-core — Tech Debt: Root Causes & Fix Suggestions

**Date:** 2026-06-05  
**Scope:** All 10 structural tech-debt items, prioritized by risk/effort

---

## Quick Priority Matrix

| # | Item | Risk if left | Effort | Priority |
|---|------|-------------|--------|----------|
| TD-1 | HashMap dict loses insertion order | Low–Medium | Small | Medium |
| TD-2 | RefCell panics under multi-thread | High (native) | Medium | High |
| TD-3 | String operator dispatch | Low | Medium | Low |
| TD-4 | Content stream round-trip not lossless | High (signatures) | Medium | High |
| TD-5 | pool_len as cache key is a heuristic | Medium | Small | Medium |
| TD-6 | No undo stack | High (UX) | Large | High |
| TD-7 | Inline image EI boundary heuristic | Medium | Small | Medium |
| TD-8 | Entire file in memory | High (large PDFs) | Large | High |
| TD-9 | Font metrics estimated when font missing | Medium | Medium | Medium |
| TD-10 | WASM blocks JS event loop | High (UX) | Large | High |

---

## TD-1 — `HashMap` Loses Dictionary Insertion Order

### Where in the code

[src/parser/objects.rs:22](src/parser/objects.rs#L22)

```rust
// current
pub type PdfDict = HashMap<String, PdfObject>;
```

The type alias is used everywhere: stream dicts, page dicts, resources dicts, font dicts. The comment even admits it: *"insertion order is not preserved (sufficient for reading)"*.

### Why this matters

PDF is not strictly order-sensitive for most dicts, but there are two real failure modes:

1. **`/Filter` + `/DecodeParms` must be paired positionally.** If the dict has `[ /FlateDecode /DCTDecode ]` in `/Filter` and `[ null << /Columns 800 >> ]` in `/DecodeParms`, the association is by index, not key. A HashMap won't corrupt this because both values are stored separately, but if you *serialize* a dict that was parsed from an incremental update and then modify it, you can output them in a different order than the original — which breaks format-sensitive downstream tools.

2. **Digital signature `/ByteRange` and `/Contents` position.** Signatures are verified by hashing a specific byte range of the file. If you ever touch the signed dict and serialize it with a different key order, the hash changes and the signature breaks. This is the High-risk scenario.

3. **Cosmetic:** Some authoring tools embed comments in dict ordering conventions (e.g., `/Type` first). Scrambled order makes the output harder to diff.

### Fix suggestion

Replace `HashMap` with `IndexMap` from the `indexmap` crate, which is WASM-compatible (MIT/Apache), has identical API, and preserves insertion order:

**Step 1 — Add dependency in `Cargo.toml`:**

```toml
[dependencies]
indexmap = "2"
```

**Step 2 — Change the alias in `src/parser/objects.rs`:**

```rust
// before
use std::collections::HashMap;
pub type PdfDict = HashMap<String, PdfObject>;

// after
use indexmap::IndexMap;
pub type PdfDict = IndexMap<String, PdfObject>;
```

**Step 3 — The rest of the codebase uses `PdfDict` through the type alias, so most sites compile unchanged.** The only places that need touching are the ones that construct a bare `HashMap::new()` for a dict:

```bash
grep -rn "HashMap::new\(\)" src/ | grep -v "xref\|cache\|overrides\|pool"
```

Each of those should be changed to `IndexMap::new()` or `PdfDict::new()`.

**Step 4 — Remove the stray `use std::collections::HashMap` in `src/writer/serializer.rs:3` where it creates a local dict.** Change to `IndexMap`.

**Why `indexmap` and not `Vec<(String, PdfObject)>`?** `IndexMap` gives O(1) lookup (same as `HashMap`) while preserving order. A `Vec` degrades lookups to O(N), which hurts the hot path in `get_object`.

**Test to add:** Round-trip a signed PDF through `serialize_all`, then verify the `/AcroForm`/`/Sig` dict keys appear in the same order as the original.

---

## TD-2 — `RefCell` in `PdfDocument` Panics Under Multi-Threading

### Where in the code

[src/parser/objects.rs:207–224](src/parser/objects.rs#L207)

```rust
obj_stream_cache: RefCell<HashMap<u32, Vec<PdfObject>>>,
decoded_stream_cache: RefCell<HashMap<u32, Vec<u8>>>,
overrides: RefCell<HashMap<u32, PdfObject>>,
page_refs: RefCell<Option<Vec<PdfObject>>>,
```

All four caches use `RefCell`, which gives interior mutability with a runtime borrow-check. `RefCell` is `!Sync`, so `PdfDocument` is not `Sync` — the Rust compiler currently prevents it from being sent across threads. But:

- The current WASM target is single-threaded, so `RefCell` is safe.
- If you ever enable parallel tile rendering on native (e.g., rayon), or if WASM threads land (`wasm32-unknown-unknown` with `--features atomics`), any concurrent `borrow_mut()` call will **panic** at runtime, not at compile time.

### The two paths forward

#### Path A — `RwLock<HashMap>` (recommended for native parallelism)

```rust
// replace all four RefCell fields:
obj_stream_cache: std::sync::RwLock<HashMap<u32, Vec<PdfObject>>>,
decoded_stream_cache: std::sync::RwLock<HashMap<u32, Vec<u8>>>,
overrides: std::sync::RwLock<HashMap<u32, PdfObject>>,
page_refs: std::sync::RwLock<Option<Vec<PdfObject>>>,
```

`RwLock` allows many concurrent readers and one exclusive writer. The caches are written once (on first miss) and read many times thereafter — a perfect RwLock workload. 

Change call sites:
```rust
// read
self.decoded_stream_cache.read().unwrap().get(&id)
// write
self.decoded_stream_cache.write().unwrap().insert(id, bytes)
```

**Downside:** `std::sync::RwLock` is not available in `wasm32-unknown-unknown` (no threading primitives). You'd need a cfg split:

```rust
#[cfg(target_arch = "wasm32")]
obj_stream_cache: std::cell::RefCell<HashMap<...>>,
#[cfg(not(target_arch = "wasm32"))]
obj_stream_cache: std::sync::RwLock<HashMap<...>>,
```

This is ugly but correct. Macros can hide the ugliness.

#### Path B — `parking_lot::RwLock` with WASM stub (cleaner)

`parking_lot` provides a `RwLock` that compiles on WASM (it stubs to a `RefCell` under `wasm32`), so you write one code path:

```toml
[dependencies]
parking_lot = "0.12"
```

```rust
use parking_lot::RwLock;
obj_stream_cache: RwLock<HashMap<u32, Vec<PdfObject>>>,
```

On native this is a real mutex; on WASM it's a no-op wrapper. No `cfg` splitting needed. **This is the recommended path.**

#### The `overrides` special case

`overrides` is set by the editor before a render pass and cleared after. With `RwLock` this pattern stays the same — just change `borrow_mut()` to `write()`. But document in the doc comment that callers must not hold a read lock while calling `set_overrides`.

**Test to add:** A `#[cfg(not(target_arch = "wasm32"))]` test that spawns two `std::thread` instances calling `get_object` concurrently on a shared `Arc<PdfDocument>`. This test should compile and pass (it currently won't even compile because `PdfDocument` is `!Sync`).

---

## TD-3 — String-Based Operator Dispatch in Interpreter

### Where in the code

[src/content/interpreter.rs:189](src/content/interpreter.rs#L189)

```rust
match op.operator.as_str() {
    "q" => ...
    "Q" => ...
    "cm" => ...
    // ... ~80 more arms
    _ => {}
}
```

### Why this is lower-priority than it looks

The `match` on `&str` in Rust is compiled by the optimizer into a jump table or a set of memcmp branches — in practice it's O(1) for small string sets (the compiler may emit a perfect hash). So the raw performance cost is negligible.

The real costs are:

1. **No exhaustiveness check.** If you add a new operator and forget to handle it, the compiler does not warn you. It silently falls into `_ => {}`.
2. **Typos are silent.** `"TJ"` vs `"Tj"` — both are valid strings, only one is a valid PDF operator. A typo in a match arm will silently no-op.
3. **No documentation of the complete operator set.** You have to grep to find all handled operators.

### Fix suggestion

**Define an `Operator` enum, generated from a table:**

```rust
// src/content/operator_table.rs  (new file, ~80 lines)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    // Graphics state
    SaveState,       // q
    RestoreState,    // Q
    SetCTM,          // cm
    SetLineWidth,    // w
    // ... all operators
    Unknown,
}

impl Operator {
    pub fn from_str(s: &str) -> Self {
        match s {
            "q"   => Operator::SaveState,
            "Q"   => Operator::RestoreState,
            "cm"  => Operator::SetCTM,
            "w"   => Operator::SetLineWidth,
            // ...
            _     => Operator::Unknown,
        }
    }
}
```

Then parse the operator string once in `parse_content_stream` (or in `Operation`) and store the enum variant. The dispatch becomes:

```rust
match op.operator_enum {
    Operator::SaveState    => self.gfx.save(),
    Operator::RestoreState => self.gfx.restore()?,
    Operator::SetCTM       => { ... }
    // compiler forces you to handle Unknown explicitly
    Operator::Unknown      => {}
}
```

**Migration strategy:** This is a pure refactor with no behavior change. Do it in one PR:
1. Add `operator_enum: Operator` field to `Operation` struct
2. Populate it during `parse_content_stream`
3. Change `dispatch` to match the enum
4. Delete the `match op.operator.as_str()` block

**Do NOT** change the `operator: String` field yet — the editor layer uses string operators to rebuild content streams (`"Tj".to_owned()`, etc.). Keep both until the editor also migrates to the enum.

---

## TD-4 — Content Stream Round-Trip is Not Lossless

### Where in the code

The path is: `parse_content_stream` → user modifies ops → `serialize_operations` → written back.

Key sites:
- [src/content/operators.rs:1–30](src/content/operators.rs#L1) — parser
- [src/writer/serializer.rs:10–18](src/writer/serializer.rs#L10) — real number formatting
- [src/editor/edit_session.rs](src/editor/edit_session.rs) — commit path

### What exactly is not lossless

1. **Comments are stripped.** PDF comments (`% ...`) are tokens in the lexer but discarded. Content streams rarely have comments, but if they do, they're gone after a round-trip.

2. **Real number precision changes.** The original stream may have `0.333333` (6 decimal places). After parsing to `f64` and re-serializing via `format_real`, you get the same string back *if* the precision is the same — but the lexer reads it as a `f64` which may have rounding. Specifically, `format_real` at [serializer.rs:14](src/writer/serializer.rs#L14) uses `{:.6}` and strips trailing zeros. `0.1000001` becomes `0.1` after the round-trip. This is fine for rendering, but breaks PDF signature verification which hashes the exact bytes.

3. **Operator whitespace is normalized.** The serializer emits one space between operands and one newline between operators. The original may have had tabs or multiple spaces.

4. **Inline image whitespace may differ** (as documented in TD-7 below).

### The real danger: signed PDFs

When the content stream of a signed page is modified and re-serialized, the byte range covered by `/ByteRange` changes. Any standard PDF signature verifier will reject the document as tampered — which is technically correct, but it's a silent integrity violation the user doesn't know about.

### Fix suggestions

**Fix A (immediate, low effort) — Detect and reject signed PDFs before edit:**

```rust
pub fn is_signed(doc: &PdfDocument) -> bool {
    // Check /AcroForm /SigFlags bit 1 (HasSignature)
    doc.catalog()
       .and_then(|c| c.get("AcroForm"))
       .and_then(|af| af.as_dict())
       .and_then(|d| d.get("SigFlags"))
       .and_then(|f| f.as_integer())
       .map(|f| f & 1 != 0)
       .unwrap_or(false)
}
```

In `text_edit_enter`, return an error if the page's content stream is covered by a signature. Emit a user-visible `"This PDF contains a digital signature. Editing will invalidate it."` message rather than silently corrupting the document.

**Fix B (medium effort) — Preserve raw bytes for untouched streams:**

The key insight: you only need to re-serialize streams that were actually modified. For all others, write the original bytes verbatim.

In `commit_edit_session`, track which stream indices were dirtied:

```rust
pub struct EditSession {
    pub streams: Vec<ParsedStream>,
    pub dirty_streams: HashSet<usize>,  // NEW: only these need re-serialization
}
```

In `commit_block`, set `session.dirty_streams.insert(stream_idx)` when modifying ops.

In `commit_edit_session`, for each stream:
```rust
if session.dirty_streams.contains(&i) {
    // re-serialize and compress
    let bytes = serialize_operations(&stream.ops);
    let new_stream = make_flate_stream(&bytes, HashMap::new())?;
    editor.add_object(PdfObject::Stream(Box::new(new_stream)));
} else {
    // write original bytes as-is (no re-serialization)
    // this preserves exact whitespace, comments, number formatting
    editor.replace_object(stream.original_id, stream.original_object.clone());
}
```

This requires storing `original_id` and the original `PdfObject::Stream` in `ParsedStream` (not just the decoded ops). It means the parsed structure carries slightly more data, but avoids all round-trip fidelity issues for untouched streams.

**Fix C (hard, for signature compliance) — Implement incremental signature update:**

Per PDF spec §12.8.1, you can add a new signature or mark an existing one as invalid by appending a `/DocMDP` transform. This is out of scope unless you need full signature compliance.

**Recommended: do Fix A now (prevents silent corruption), do Fix B in the same sprint.**

---

## TD-5 — `pool_len` as Cache Invalidation Key is a Heuristic

### Where in the code

[src/wasm/text_edit.rs:68–88](src/wasm/text_edit.rs#L68)

```rust
let pool_len = self.editor.writer.len();
if page_index == self.text_edit_page
    && self.text_edit_model_pool_len == pool_len
    && self.text_edit_model.is_some()
{
    // cache hit — return existing model
}
```

And for `edit_model_doc`:

```rust
let fresh = !matches!(&self.edit_model_doc, Some((n, _)) if *n == pool_len);
```

### The problem

`pool_len` is the number of objects in the writer pool. It increments when objects are added, but:

1. **Two different edits can produce the same pool length.** If you add one object then remove one (hypothetical future undo), `pool_len` returns to the same value but the content has changed. The cache would serve stale data.

2. **`set_object` (replace without adding) doesn't change `pool_len`.** The `PdfWriter::set_object` method [writer/document.rs:71](src/writer/document.rs#L71) replaces an existing entry in the pool without growing it. A caller using `set_object` instead of `add_object` would bypass the invalidation check.

3. It's an implicit contract, not enforced by the type system. Future code could change the pool without touching `pool_len` and introduce a hard-to-debug stale-cache bug.

### Fix suggestion

**Add a `generation: u64` counter to `PdfWriter` that increments on every mutation:**

```rust
// src/writer/document.rs

pub struct PdfWriter {
    pool: Vec<PoolEntry>,
    next_id: u32,
    generation: u64,  // NEW: bumped on every add/set/remove
}

impl PdfWriter {
    pub fn add_object(&mut self, obj: PdfObject) -> u32 {
        self.generation += 1;  // bump
        // ... existing code
    }

    pub fn set_object(&mut self, id: u32, obj: PdfObject) {
        self.generation += 1;  // bump — this was the missing case
        // ... existing code
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}
```

Then in `WasmEditor`, replace `text_edit_model_pool_len: usize` with `text_edit_model_generation: u64`, and compare against `self.editor.writer.generation()`. This is:
- Correct even for `set_object` replacements
- Correct even if objects were removed (future undo)
- Self-documenting (the name "generation" is a well-known invalidation pattern)

**Test to add:** Call `set_object` on the writer, then verify the generation changed. Verify that the text model cache is rebuilt after a `set_object` mutation.

---

## TD-6 — No Undo Stack

### Where in the code

There is no undo mechanism anywhere in the editor layer. Once `commit_block` writes into the CoW pool, the only way back is to reload the original file bytes.

### The real-world impact

From the user's perspective: "I edited a block of text, it looked wrong, I need to undo." Currently they must reload the entire document, losing all other edits too.

### Fix suggestion: Command Pattern with Snapshot

The right design is a **command stack** that stores reversible operations. There are two viable approaches:

#### Approach A — Full pool snapshot (simpler, higher memory)

Before each commit, clone the writer pool and push the clone onto an undo stack:

```rust
pub struct PdfEditor {
    // ...existing fields...
    undo_stack: Vec<WriterSnapshot>,   // NEW
    redo_stack: Vec<WriterSnapshot>,   // NEW (optional)
}

struct WriterSnapshot {
    pool: Vec<PoolEntry>,              // full clone of writer pool
    generation: u64,
}

impl PdfEditor {
    pub fn checkpoint(&mut self) {
        self.redo_stack.clear();
        self.undo_stack.push(WriterSnapshot {
            pool: self.writer.pool.clone(),
            generation: self.writer.generation,
        });
        if self.undo_stack.len() > 50 {  // cap history
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else { return false; };
        self.redo_stack.push(WriterSnapshot {
            pool: self.writer.pool.clone(),
            generation: self.writer.generation,
        });
        self.writer.pool = snap.pool;
        self.writer.generation = snap.generation + 1;
        true
    }
}
```

**Usage:** call `editor.checkpoint()` before every `commit_block` call (do it in `text_edit_commit` in the WASM layer). The user calls `editor.undo()` which pops the last snapshot and restores the pool.

**Memory cost:** Each snapshot is a clone of the writer pool. A typical edit session with 10 edits, each adding ~3 objects of ~2KB each, costs roughly `10 × 3 × 2KB = 60KB`. Negligible. Even 50 snapshots is under 3MB in the common case.

**Downside:** For very large embedded fonts (Tier 3 embed adds ~300KB of TTF data), one snapshot is 300KB. With 50 history entries that's 15MB. Acceptable but worth noting.

#### Approach B — Delta commands (lower memory, higher complexity)

Define a `Command` enum that knows how to apply and reverse itself:

```rust
enum Command {
    ReplaceObject { id: u32, old: PdfObject, new: PdfObject },
    AddObject { id: u32 },  // reverse: remove from pool
    RemoveObject { id: u32, obj: PdfObject },
}
```

Then the undo stack is `Vec<Vec<Command>>` (a transaction is a list of atomic changes). Reversing a transaction replays the `old` values.

This is more complex to implement but has O(delta) memory per snapshot rather than O(pool_size). **Approach A is recommended for now** — pool is small enough that cloning it is cheap.

**WASM API to add:**

```rust
#[wasm_bindgen]
impl WasmEditor {
    pub fn undo(&mut self) -> bool;      // true if something was undone
    pub fn redo(&mut self) -> bool;      // true if something was redone
    pub fn can_undo(&self) -> bool;
    pub fn can_redo(&self) -> bool;
    pub fn history_len(&self) -> usize;
}
```

---

## TD-7 — Inline Image `EI` Boundary Detection is Fragile

### Where in the code

[src/content/operators.rs:246–280](src/content/operators.rs#L246)

```rust
// Scan for EI preceded by whitespace
for i in 0..search.len().saturating_sub(1) {
    if matches!(search[i], b' ' | b'\n' | b'\r' | b'\t')
        && search[i + 1] == b'E'
        && i + 2 < search.len()
        && search[i + 2] == b'I'
    {
        // Verify EI is followed by whitespace or EOF
```

### The problem

JPEG image data (`/CS /RGB /BPC 8 /F /DCT`) can contain any byte sequence, including `\nEI\n`. The current scanner will terminate early on that false match, chopping the image data and causing a corrupted render.

The PDF spec (ISO 32000-1 §8.9.7) says the correct approach is:

> *"The amount of data shall be determined by the `/Filter` and the other parameters in the inline image dictionary, not by the presence of an `EI` token."*

That means: if `/Length` is present, read exactly that many bytes. If `/Length` is absent (which many writers omit), use the filter's natural termination (e.g., zlib decompressor signals end-of-stream).

### Fix suggestion

**Step 1 — Check for `/Length` first:**

```rust
// in parse_inline_image, after building `dict`
if let Some(PdfObject::Integer(len)) = dict.get("Length") {
    let byte_count = *len as usize;
    let data = remaining[data_start..data_start + byte_count].to_vec();
    // skip the data + whitespace + "EI"
    let skip = data_start + byte_count;
    let skip = skip_past_ei(remaining, skip);
    lexer.set_position(lexer.position() + skip);
    return Ok(Operation { operands: vec![...], operator: "BI".to_string() });
}
```

**Step 2 — For filters with natural termination (`/FlateDecode`, `/LZWDecode`, `/RunLength`), decompress until the filter says done:**

```rust
// Try to use the filter's own terminator as the boundary
if let Some(filter) = dict.get("Filter").and_then(|f| f.as_name()) {
    match filter {
        "FlateDecode" | "Fl" => {
            // Feed bytes into flate2 decoder; stop when decompressor finishes
            let data = read_until_deflate_end(&remaining[data_start..], dict.get("DecodeParms"))?;
            // ...
        }
        _ => { /* fall through to heuristic */ }
    }
}
```

**Step 3 — Keep the heuristic as a last resort** with an explicit log warning so you know when it triggers:

```rust
log::warn!(
    "inline image at offset {} has no /Length and unknown filter {:?} — using EI heuristic",
    lexer.position(),
    dict.get("Filter")
);
```

**Step 4 — For `/DCTDecode` (JPEG):** JPEG has its own end-of-image marker (`0xFF 0xD9`). You can scan for that instead of `EI`:

```rust
"DCTDecode" | "DCT" => {
    // Scan for JPEG EOI marker FF D9
    let pos = find_jpeg_eoi(&remaining[data_start..])
        .ok_or_else(|| PdfError::eof(lexer.position(), "unterminated JPEG in inline image"))?;
    let data = remaining[data_start..data_start + pos + 2].to_vec();
    // ...
}
```

**Test to add:** A crafted inline image whose JPEG data contains `\nEI\n` mid-stream. Verify the image is correctly read to completion.

---

## TD-8 — Entire File Loaded Into Memory

### Where in the code

[src/parser/objects.rs:201](src/parser/objects.rs#L201)

```rust
pub struct PdfDocument {
    data: Vec<u8>,   // ← entire file
    // ...
}
```

And:

```rust
pub fn parse(data: Vec<u8>) -> Result<Self>  // takes ownership of Vec<u8>
```

### The problem

A 200MB PDF with many embedded images requires 200MB of heap just to parse. The WASM memory limit defaults to 4GB but WASM engines often refuse to allocate more than 1–2GB in practice. On native, a 500MB PDF pushes the parser to its limit.

On top of that, the filter pipeline produces another copy: decoded stream bytes are cached in `decoded_stream_cache` (another `Vec<u8>` per stream). A 200MB PDF with 50MB of compressed streams expands to ~200MB decoded — totaling 400MB of heap for one document.

### Fix suggestion — Two-tier approach

#### Tier 1 (medium effort): Zero-copy stream decoding with `Cow<[u8]>`

The most impactful change without a full streaming rewrite: avoid cloning stream data when it doesn't need filtering.

Currently `get_object` always decodes and caches:

```rust
let decoded = stream.decode_with_doc(self)?;
self.decoded_stream_cache.borrow_mut().insert(id, decoded.clone());
```

If `/Filter` is absent, the stream's `raw_data` IS the decoded data. Instead of copying into cache, return a reference:

```rust
pub fn get_decoded_stream<'a>(&'a self, id: u32) -> Result<Cow<'a, [u8]>> {
    let stream = ...; // get PdfStream
    if stream.filter_names().is_empty() {
        // No filter: return reference to raw_data in-place, zero copy
        return Ok(Cow::Borrowed(&stream.raw_data));
    }
    // Has filter: decode and cache
    // ...
    Ok(Cow::Owned(decoded))
}
```

This doesn't reduce peak for compressed streams but eliminates the copy for uncompressed ones (common in text-heavy PDFs).

#### Tier 2 (large effort): Memory-mapped I/O via `memmap2`

The fundamental fix is to not hold the file bytes in a `Vec<u8>` at all. Use a memory-mapped file on native:

```rust
// Native only (not WASM)
#[cfg(not(target_arch = "wasm32"))]
pub struct PdfDocument {
    data: memmap2::Mmap,   // OS-managed, demand-paged
    // ...
}
```

`memmap2` is WASM-incompatible, so you need a cfg split:

```rust
pub struct PdfDocumentData {
    #[cfg(target_arch = "wasm32")]
    bytes: Vec<u8>,
    #[cfg(not(target_arch = "wasm32"))]
    bytes: memmap2::Mmap,
}
```

With `Mmap`, the OS loads only the pages actually accessed. A 200MB PDF where you read page 1 causes only ~few-KB of actual RAM usage. Decompressed streams still go into `decoded_stream_cache` as `Vec<u8>`, but the raw bytes are OS-managed.

**The real footprint formula becomes:**
```
Peak RAM ≈ (all decoded streams accessed) + (hot cache)
```
Instead of:
```
Peak RAM ≈ file_size + (all decoded streams accessed)
```

**Migration path:**
1. Extract a `FileData` newtype wrapping either `Vec<u8>` or `Mmap`
2. Implement `Deref<Target=[u8]>` on it so all existing `&self.data[offset..]` slices work unchanged
3. Change `PdfDocument::open_file(path) -> Result<Self>` to use `Mmap` on native
4. Keep `PdfDocument::parse(data: Vec<u8>)` for WASM (browser gives you bytes)

**Test to add:** Open a 100MB+ PDF with the native target and verify resident set size stays below `file_size × 0.5 + decoded_cache_size`.

---

## TD-9 — Font Metrics Estimated When Font Not Embedded

### Where in the code

[src/fonts/font_cache.rs](src/fonts/font_cache.rs) — when a font dict has no `/FontFile`, `/FontFile2`, or `/FontFile3` entry, the loader falls back to estimated metrics.

The fallback is currently a fixed advance width (typically 500 units) or proportional to character code, which produces visibly wrong text layout for non-Latin fonts and any non-proportional font.

### The two failure cases

1. **Non-embedded standard fonts (Type1, TrueType):** The PDF references `/Helvetica` but the viewer is supposed to substitute a metric-compatible font. The 14 standard fonts have well-known metrics tables that should be used here.

2. **Non-embedded custom fonts:** The PDF includes only the font dict and encoding but no program bytes. This is common in old PDFs generated by PostScript printers that assumed the printer had the font. No reliable fix exists short of font matching, but we can do better than 500 units.

### Fix suggestion

**Fix A — Use the `/Widths` array from the font dict even without embedded program bytes:**

PDF font dicts often include `/Widths` (for simple fonts) or `/W` (for CID fonts) even when the font program itself is not embedded. These widths are accurate for layout purposes:

```rust
// In font_cache.rs, when no FontFile is present:
// Instead of returning FallbackMetrics { default_width: 500 }
// check if the dict has /Widths:
if let Some(widths_obj) = font_dict.get("Widths") {
    let first_char = font_dict.get("FirstChar").and_then(|o| o.as_integer()).unwrap_or(0) as u32;
    let widths = parse_widths_array(widths_obj, first_char)?;
    return Ok(CachedFont { metrics: FontMetrics::from_widths(widths), ..Default::default() });
}
```

This covers the majority of non-embedded fonts in real-world PDFs since most generators include width tables even without embedding the program.

**Fix B — Load metrics from bundled Liberation font as a substitution:**

When a font dict says `/BaseFont /Helvetica` (or any of the 14 standard fonts), use the Liberation Sans metrics table as a substitution. Liberation is metrically compatible with Helvetica and is already bundled for the Tier-2 edit fallback:

```rust
fn substitute_metrics(base_font_name: &str) -> Option<&'static FontMetrics> {
    match normalize_font_name(base_font_name) {
        "Helvetica" | "Arial" => Some(&LIBERATION_SANS_METRICS),
        "Times-Roman" | "TimesNewRoman" => Some(&LIBERATION_SERIF_METRICS),
        "Courier" | "CourierNew" => Some(&LIBERATION_MONO_METRICS),
        _ => None,
    }
}
```

**Fix C — Log a structured warning with the font name and offset:**

When falling back to estimated metrics, emit a structured warning so issues are traceable:

```rust
log::warn!(
    "[pdf-core] font '{name}' at obj {id} has no embedded program and no /Widths; \
     using fallback metrics — text positions may be incorrect"
);
```

This doesn't fix the metrics but at least makes the problem diagnosable in production logs.

**Test to add:** A PDF with a non-embedded Helvetica font. Verify that frame `x` positions computed from `/Widths` match the expected advance widths to within 1 unit.

---

## TD-10 — WASM Blocks the JS Event Loop

### Where in the code

All WASM entry points are synchronous:

[src/wasm/document.rs](src/wasm/document.rs) — `render_page`, `extract_text`, `parse`  
[src/wasm/text_edit.rs](src/wasm/text_edit.rs) — `text_edit_enter`, `text_edit_commit`  
[src/wasm/editor.rs](src/wasm/editor.rs) — `save`

`render_page` on a complex page can take 200–800ms synchronously, freezing the browser tab.

### Fix suggestion — Offload to a Web Worker

The cleanest fix is entirely on the JavaScript side — the Rust/WASM layer doesn't need to change at all:

**Step 1 — Wrap the WASM module in a Web Worker:**

```javascript
// pdf-worker.js
import init, { WasmDocument, WasmEditor } from "./pkg/pdf_core.js";

let doc = null;
let editor = null;

self.onmessage = async (e) => {
    const { id, method, args } = e.data;
    try {
        let result;
        switch (method) {
            case "init":
                await init();
                break;
            case "parse":
                doc = WasmDocument.parse(new Uint8Array(args.bytes));
                break;
            case "render_page":
                result = doc.render_page(args.page_index, args.scale);
                // Transfer RGBA buffer without copy using Transferable
                self.postMessage({ id, result: result.buffer }, [result.buffer]);
                return;
            case "text_edit_enter":
                result = editor.text_edit_enter(args.page_index);
                break;
            // ... etc
        }
        self.postMessage({ id, result });
    } catch (err) {
        self.postMessage({ id, error: err.message });
    }
};
```

**Step 2 — Create a typed async proxy on the main thread:**

```javascript
// pdf-client.js
class PdfClient {
    #worker;
    #pending = new Map();   // id → {resolve, reject}
    #idCounter = 0;

    constructor() {
        this.#worker = new Worker(new URL("./pdf-worker.js", import.meta.url), { type: "module" });
        this.#worker.onmessage = ({ data: { id, result, error } }) => {
            const p = this.#pending.get(id);
            this.#pending.delete(id);
            error ? p.reject(new Error(error)) : p.resolve(result);
        };
    }

    #call(method, args, transfer = []) {
        return new Promise((resolve, reject) => {
            const id = ++this.#idCounter;
            this.#pending.set(id, { resolve, reject });
            this.#worker.postMessage({ id, method, args }, transfer);
        });
    }

    async renderPage(pageIndex, scale) {
        const buf = await this.#call("render_page", { page_index: pageIndex, scale });
        return new Uint8ClampedArray(buf);   // zero-copy via Transferable
    }

    async textEditEnter(pageIndex) {
        return this.#call("text_edit_enter", { page_index: pageIndex });
    }

    // ... etc
}
```

**Step 3 — Return RGBA buffers as `Transferable` (zero-copy):**

Currently `render_page` returns `Vec<u8>` which is copied across the WASM boundary. Change the WASM API to return an `ArrayBuffer` that can be `postMessage`d with a transfer list (zero-copy):

On the Rust side this requires returning a `js_sys::Uint8Array` instead of `Vec<u8>`:

```rust
#[wasm_bindgen]
impl WasmDocument {
    pub fn render_page(&self, page_index: usize, scale: f32) -> Result<js_sys::Uint8Array, JsError> {
        let pixels = render_page_impl(&self.doc, page_index, scale)?;
        // SAFETY: we create the array from Rust-owned memory that JS will then own
        let arr = js_sys::Uint8Array::from(pixels.as_slice());
        Ok(arr)
    }
}
```

**The additional WASM-specific improvements:**

1. **Chunked rendering:** Instead of returning a full-page buffer, render in horizontal strips of ~256px and `postMessage` each strip as it's ready. The browser can paint incrementally.

2. **Cancellable operations:** Each Worker message can carry a `cancelToken`. The Worker checks a shared `Atomics` flag between strips and aborts early if cancelled (for rapid page-flipping scenarios).

3. **Memory pressure:** Periodically call the Rust WASM allocator's trim function or just `doc = null; editor = null; await reinit()` after large operations. There's no WASM equivalent of `malloc_trim`, but releasing top-level Rust objects frees their allocation.

**What does NOT require Web Workers:** The `text_edit_enter` model build (fast, <50ms typically) and `text_edit_commit` (fast, <20ms). Only `render_page`, `save` (for large files), and `parse` (for large files) need offloading.

---

## Summary: Suggested Implementation Order

Given risk and effort, here is the recommended sprint ordering:

| Sprint | Items | Rationale |
|--------|-------|-----------|
| Sprint 1 (this week) | TD-1 (IndexMap), TD-5 (generation counter), TD-7 (inline image /Length) | Small changes, high correctness gain |
| Sprint 2 | TD-4 Fix A (detect signed PDFs), TD-4 Fix B (dirty stream tracking) | Prevents silent signature corruption |
| Sprint 3 | TD-6 (undo stack, Approach A) | High UX impact, medium effort |
| Sprint 4 | TD-2 (parking_lot RwLock) | Required before any native threading |
| Sprint 5 | TD-10 (Web Worker wrapper) | High UX impact, entirely JS-side |
| Backlog | TD-3 (operator enum), TD-8 (mmap), TD-9 (font metrics) | Lower immediate risk |

*TD-3 is a pure refactor — safe to do any time as a standalone PR.*  
*TD-8 is the largest change and should be planned as a separate milestone.*
