# pdf-core — Architecture Deep Dive

**Date:** 2026-06-05  
**Scope:** Full system — parser, renderer, editor, WASM API, algorithms, tech debt

---

## Table of Contents

1. [Project Overview](#1-project-overview)
2. [Module Map](#2-module-map)
3. [Layer 1 — Parser: Bytes → Objects](#3-layer-1--parser-bytes--objects)
4. [Layer 2 — Renderer: Content Stream → Pixels](#4-layer-2--renderer-content-stream--pixels)
5. [Layer 3 — Editor: Text Model & Mutation](#5-layer-3--editor-text-model--mutation)
6. [Layer 4 — Writer: Objects → PDF Bytes](#6-layer-4--writer-objects--pdf-bytes)
7. [Layer 5 — WASM API: Rust → JavaScript](#7-layer-5--wasm-api-rust--javascript)
8. [Fonts Subsystem](#8-fonts-subsystem)
9. [End-to-End Data Flow](#9-end-to-end-data-flow)
10. [Algorithmic Complexity](#10-algorithmic-complexity)
11. [Problems & Technical Debt](#11-problems--technical-debt)

---

## 1. Project Overview

```
Codebase:   Rust, ~36,400 LOC, 92 source files
Target:     Native (Linux/macOS/Windows) + WASM (wasm32-unknown-unknown)
Crate type: cdylib + rlib
Features:   render | writer | crypto | wasm | forms  (all optional)
Key deps:   nom, thiserror, flate2, weezl, fontdue, tiny-skia, wasm-bindgen
```

**Core idea:** Three fully-independent subsystems coordinated by the editor layer.

```
Raw PDF bytes
      │
      ▼
  [ PARSER ]  ──→  PdfDocument  ──→  [ RENDERER ]  ──→  PixmapBuffer (RGBA)
                        │
                    [ EDITOR ]
                        │
                   [ WRITER ]  ──→  PDF bytes (incremental append)
```

The editor never touches parser internals. The renderer never calls writer code. This isolation is a hard design constraint mirroring the ONLYOFFICE C++ original.

---

## 2. Module Map

| Module | Files | LOC | Purpose |
|--------|-------|-----|---------|
| `parser` | 5 | 4,900 | Tokenizer, object parser, xref, filter pipeline |
| `content` | 4 | 2,900 | Content stream ops, graphics state machine |
| `render` | 11 | 6,500 | Page rasterizer, canvas, glyph cache, images, shading |
| `editor` | 15 | 8,200 | Text model, session, commit, annotation, redact |
| `writer` | 8 | 3,200 | Object serialization, xref generation, font writing |
| `document` | 8 | 2,000 | Catalog, pages tree, metadata, outlines |
| `fonts` | 8 | 4,600 | Encoding, CFF, TrueType, CMap, font cache |
| `text` | 3 | 900 | Text extraction, layout |
| `forms` | 3 | 950 | AcroForm, annotations |
| `crypto` | 4 | 800 | RC4, AES-256, decryption handler |
| `wasm` | 4 | 3,800 | JS bindings via wasm-bindgen |
| `display` | 1 | 474 | Debug/display traits |
| **TOTAL** | **92** | **36,400** | |

### Largest files (hotspots)

| File | LOC | Role |
|------|-----|------|
| `render/page_renderer.rs` | 2,472 | OutputDevice impl, glyph blit, pattern fill |
| `content/interpreter.rs` | 1,836 | PDF operator dispatch (~200 operators) |
| `parser/objects.rs` | 1,734 | PdfObject, PdfDocument, xref resolution |
| `wasm/text_edit.rs` | 1,683 | JS API: enter/open/commit/cancel |
| `fonts/encoding.rs` | 1,255 | WinAnsi, MacRoman, Adobe Glyph List |
| `editor/edit_session.rs` | 1,155 | Frame extraction from content streams |
| `parser/lexer.rs` | 1,108 | Byte-level tokenizer |
| `editor/redact.rs` | 788 | Zone-based content suppression |
| `render/shading.rs` | 745 | Gouraud/Coons gradient rasterization |

---

## 3. Layer 1 — Parser: Bytes → Objects

### 3.1 Entry Point

```rust
PdfDocument::parse(data: Vec<u8>) -> Result<Self>
PdfDocument::parse_with_password(data, password) -> Result<Self>
```

**Five-step pipeline:**

```
Raw bytes
  │
  ├─[1] startxref locator   scan backwards from EOF for "startxref\nNNN"
  │
  ├─[2] XRef load           parse all xref sections, follow /Prev chain
  │
  ├─[3] Trailer merge       unify /Root, /Info, /Encrypt from all trailers
  │
  ├─[4] Encryption check    if /Encrypt present + crypto feature, try empty password
  │
  └─[5] Lazy caches init    object_stream_cache, decoded_stream_cache, page_refs
```

### 3.2 Lexer (`src/parser/lexer.rs`, 1,108 LOC)

**Token enum:**

```
Null | Boolean(bool) | Integer(i64) | Real(f64)
LiteralString(Vec<u8>) | HexString(Vec<u8>)
Name(String)
ArrayStart | ArrayEnd | DictStart | DictEnd
Keyword(Obj|EndObj|Stream|EndStream|Xref|Trailer|StartXref|R)
Operator(String)    ← BT, ET, Tj, cm, q, Q …
Eof
```

**Key functions:**

| Function | Complexity | Notes |
|----------|-----------|-------|
| `new(data)` | O(1) | stores slice pointer |
| `next_token()` | O(T) | T = token bytes, single-pass |
| `peek_token()` | O(T) | lookahead, no advance |
| `tokenize_all()` | O(N) | N = stream size |

**nom combinators used:**

- `parse_name()` — `/` + chars, handles `#HH` hex escapes
- `parse_literal_string()` — `(...)`, tracks nested parens depth
- `parse_hex_string()` — `<...>`, whitespace-tolerant, odd-length zero-padded
- `parse_number()` — integer/float with sign (no locale dependency)
- `parse_identifier()` — keywords and operator names

Errors always include `offset: usize` computed as `data.len() - remaining.len()`.

### 3.3 Object Model (`src/parser/objects.rs`, 1,734 LOC)

**PdfObject enum:**

```rust
pub enum PdfObject {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(Vec<u8>),          // PDFDocEncoding | UTF-16BE | raw
    Name(String),             // without leading /
    Array(Vec<PdfObject>),
    Dictionary(HashMap<String, PdfObject>),  // unordered (insertion order lost)
    Stream(Box<PdfStream>),
    Reference(u32, u16),      // (object_id, generation)
}

pub struct PdfStream {
    pub dict: PdfDict,        // /Filter, /Length, /DecodeParms, /Subtype
    pub raw_data: Vec<u8>,    // bytes between `stream` and `endstream`
}
```

**PdfDocument struct:**

```rust
pub struct PdfDocument {
    data: Vec<u8>,                                   // original file, immutable
    xref: HashMap<u32, XRefEntry>,                  // obj_id → location
    pub trailer: PdfDict,                            // unified from /Prev chain
    obj_stream_cache: RefCell<HashMap<u32, Vec<PdfObject>>>,  // PDF 1.5+ ObjStm
    decoded_stream_cache: RefCell<HashMap<u32, Vec<u8>>>,     // filter outputs
    overrides: RefCell<HashMap<u32, PdfObject>>,     // CoW edits for render
    page_refs: RefCell<Option<Vec<PdfObject>>>,      // flattened page table
}
```

**XRefEntry enum:**

```rust
enum XRefEntry {
    InUse { offset: u64, generation: u16 },          // traditional xref
    Compressed { stream_obj_num: u32, index: u32 },  // PDF 1.5+ ObjStm
    Free,
}
```

**Key methods:**

| Method | Complexity | Notes |
|--------|-----------|-------|
| `get_object(id)` | O(1) + O(M) | dict lookup + stream decompression |
| `resolve(obj)` | O(D) | D = indirection depth (≤10 in real PDFs) |
| `page_count()` | O(N) cached | tree walk, subsequent calls O(1) |
| `max_object_id()` | O(1) | used to avoid ID collision in incremental updates |

### 3.4 XRef Parser (`src/parser/xref.rs`, 646 LOC)

Two formats handled:

**Traditional XRef (PDF 1.0–1.4):**
```
xref
0 10
0000000000 65535 f        ← free entry
0000100000 00000 n        ← in-use at byte 100000
...
trailer << /Size 10 /Root 1 0 R /Prev 0 >>
```
20-byte fixed-width entries, parsed with nom.

**XRef Streams (PDF 1.5+):**
```
/Type /XRef  /W [1 4 2]   ← 7-byte entries: type(1) + offset(4) + gen(2)
```
Decompressed via `/FlateDecode`, entries decoded per `/W` widths. Handles type=0 (free), type=1 (in-use), type=2 (in ObjStm).

**`/Prev` chain:** Recursively followed until offset=0. All entries merged; later entries win (newest xref section shadows older ones).

**Complexity:** O(N) where N = total xref entries across all sections.

### 3.5 Filter Pipeline (`src/parser/filters.rs`, 558 LOC)

```rust
pub fn apply_pipeline(filters: &[&str], data: &[u8]) -> Result<Vec<u8>>
```

Each filter output feeds the next:

| Filter | Algorithm | Library | Complexity |
|--------|-----------|---------|-----------|
| FlateDecode / Fl | zlib RFC 1950 | flate2 | O(N) |
| ASCII85Decode / A85 | base-85 | custom | O(N) |
| ASCIIHexDecode / AHx | hex pairs | custom | O(N) |
| LZWDecode / LZW | TIFF LZW (MSB-first) | weezl | O(N) |
| RunLengthDecode / RL | RLE 1–128 | custom | O(N) |
| DCTDecode | JPEG pass-through | renderer decodes | — |
| JPXDecode | JPEG2000 pass-through | not supported | — |

---

## 4. Layer 2 — Renderer: Content Stream → Pixels

### 4.1 Rendering Pipeline

```
Page dict  ──→  MediaBox + Resources + /Contents
                                           │
                          ┌────────────────┘
                          │
                    Content bytes
                          │
               ContentInterpreter::interpret()
                          │ operator dispatch
                    ┌─────┴──────┐
               graphics ops    text ops
                    │               │
              PageRenderer      draw_text_span()
               fill_path()          │
               stroke_path()    font metrics
               draw_image()     glyph rasterize
                    │               │
                    └────┬──────────┘
                         │
                   PixmapBuffer (RGBA)
                   tile composition
                         │
                    Final image
```

### 4.2 PageRenderer (`src/render/page_renderer.rs`, 2,472 LOC)

```rust
struct PageRenderer<'doc> {
    canvas: PixmapBuffer,                       // RGBA raster (tiny-skia)
    glyph_cache: GlyphCache,                    // (font_key, glyph_id, size) → rasterized glyph
    scale: f32,                                 // DPI multiplier (1.0 = 72 DPI)
    doc: &'doc PdfDocument,
    resources_raw: Arc<PdfDict>,                // page-level font/xobject lookup
    font_resolver: Box<dyn FontResolver>,       // fallback for missing fonts
    transparency_stack: Vec<(PixmapBuffer, f64, BlendMode)>,  // offscreen compositing
    font_bytes_cache: FontBytesCache,           // decoded TTF/CFF binaries
    resource_stack: Vec<Arc<PdfDict>>,          // Form XObject resource nesting
}
```

**Initial CTM (Coordinate Transform Matrix) setup:**

PDF user-space has origin at bottom-left with Y increasing upward. Device pixels have origin at top-left with Y increasing downward.

```
initial_ctm = [scale, 0, 0, -scale, -tile_x·scale, (tile_y + tile_h)·scale]
```

This flips the Y axis and applies tile offset so only the requested tile region is rasterized.

**OutputDevice trait (interface contract):**

```rust
pub trait OutputDevice {
    fn stroke_path(&mut self, path: &Path, state: &GraphicsState);
    fn fill_path(&mut self, path: &Path, state: &GraphicsState, rule: FillRule);
    fn draw_text_span(&mut self, span: &TextSpan, state: &GraphicsState);
    fn draw_image(&mut self, image_data: &[u8], state: &GraphicsState);
    fn draw_image_xobject(&mut self, name: &str, stream: &PdfStream, state: &GraphicsState);
    fn begin_form_xobject(&mut self);
    fn end_form_xobject(&mut self);
    fn begin_transparency_group(&mut self);
    fn end_transparency_group(&mut self, fill_alpha: f64, blend_mode: BlendMode);
    fn paint_shading(&mut self, shading_dict: &PdfDict, doc: &PdfDocument, state: &GraphicsState);
}
```

**Public rendering functions:**

| Function | Complexity | Notes |
|----------|-----------|-------|
| `render_page(doc, page, scale)` | O(W·H) | full page raster |
| `render_tile(doc, page, scale, rect)` | O(tile_area) | viewport tile |
| `render_tile_with_cache(...)` | O(tile_area) | shared glyph cache across tiles |

### 4.3 Content Stream Interpreter (`src/content/interpreter.rs`, 1,836 LOC)

```rust
pub struct ContentInterpreter {
    pub gfx: GraphicsStateStack,
    pub text: TextState,
    in_text_object: bool,       // inside BT...ET
    current_path: Path,
    current_point: Option<(f64, f64)>,
    error_count: u32,
    xobject_stack: HashSet<u32>,  // cycle detection for Form XObjects
}
```

**All ~200 PDF operators, by category:**

| Category | Operators | Effect |
|----------|-----------|--------|
| Text Object | `BT`, `ET` | begin/end text scope |
| Text State | `Tf`, `Tm`, `Td`, `TD`, `T*`, `TL`, `Tc`, `Tw`, `Tz`, `Tr`, `Ts` | font, matrix, spacing |
| Text Show | `Tj`, `TJ`, `'`, `"` | emit TextSpan, advance position |
| Path Construct | `m`, `l`, `c`, `v`, `y`, `h`, `re` | move, line, Bézier, rect |
| Path Paint | `S`, `s`, `f`, `F`, `f*`, `B`, `B*`, `b`, `b*`, `n` | stroke, fill, close+paint |
| Clipping | `W`, `W*` | set clip path (nonzero / even-odd) |
| Graphics State | `q`, `Q`, `cm`, `w`, `J`, `j`, `M`, `d`, `ri`, `i`, `gs` | save/restore, CTM, line style |
| Color | `g`, `G`, `rg`, `RG`, `k`, `K`, `cs`, `CS`, `sc`, `SC`, `scn`, `SCN` | fill/stroke in any color space |
| Images | `BI`, `ID`, `EI`, `Do` | inline image, XObject |
| Shading | `sh` | paint gradient shading |
| Marked Content | `BMC`, `BDC`, `EMC`, `MP`, `DP` | structure tags (mostly no-op) |

**Key methods:**

```rust
fn interpret(data, device) -> Result<()>
fn interpret_with_doc(data, device, doc, resources) -> Result<()>
fn dispatch(op, device, doc, resources) -> Result<()>
```

**Complexity:** O(N) — one pass, O(1) per operator.

### 4.4 Graphics State Machine (`src/content/graphics_state.rs`, 520 LOC)

```rust
pub struct Matrix {
    pub a: f64, pub b: f64,
    pub c: f64, pub d: f64,
    pub e: f64, pub f: f64,   // [a b 0 / c d 0 / e f 1] row-major
}

pub struct GraphicsState {
    pub ctm: Matrix,
    pub fill_color: Color,       // Gray | Rgb | Cmyk | Pattern
    pub stroke_color: Color,
    pub fill_alpha: f64,
    pub stroke_alpha: f64,
    pub line_width: f64,
    pub line_cap: LineCap,       // Butt | Round | Square
    pub line_join: LineJoin,     // Miter | Round | Bevel
    pub miter_limit: f64,
    pub dash_pattern: DashPattern,
    pub blend_mode: BlendMode,   // Normal | Multiply | Screen | …
    pub clip_path: Vec<ClipEntry>,
}
```

`q` pushes a copy; `Q` pops and restores. Stack depth is unbounded (real PDFs typically ≤20).

**Matrix operations:**
- `concat(a, b) → Matrix` — O(1), 6 multiplications
- `transform_point(x, y) → (f64, f64)` — O(1)

### 4.5 Text Position Tracking

Text matrix lifecycle per page:

```
BT          → reset text_matrix = identity, line_matrix = identity
Tm a b c d e f  → text_matrix = [a b c d e f]
Td dx dy    → text_matrix.translate(dx, dy); line_matrix = text_matrix
TD dx dy    → Td + set leading = -dy
T*          → Td(0, -leading)  (next line)
Tj <str>    → emit TextSpan at (text_matrix.e, text_matrix.f), advance by glyph widths
TJ [...]    → Tj with per-glyph kerning array
ET          → text object ends
```

TextSpan emitted at CTM-transformed coordinates (device pixels), not PDF user-space.

### 4.6 Glyph → Unicode Resolution (4-level fallback)

```
char_code
    │
    ├─[1] ToUnicode CMap   direct CID→Unicode lookup (most accurate; present in >80% of modern PDFs)
    │
    ├─[2] Encoding + AGL   encoding array gives glyph name → Adobe Glyph List → Unicode
    │
    ├─[3] Identity-H CMap  for CJK fonts; CID is Unicode codepoint (predefined tables)
    │
    └─[4] Direct code      use char_code as Unicode (unreliable; last resort)
```

Each level is O(1) hash lookup.

### 4.7 Shading Renderer (`src/render/shading.rs`, 745 LOC)

| Type | Name | Algorithm |
|------|------|-----------|
| Type 2 | Axial gradient | Sample on axis, interpolate color |
| Type 3 | Radial gradient | Sample radially, interpolate color |
| Type 4 | Free-form Gouraud | Triangle mesh, per-vertex color barycentric blend |
| Type 5 | Lattice Gouraud | Grid mesh, same blend |
| Type 6 | Coons patch mesh | Bicubic patch subdivision |
| Type 7 | Tensor-product patch | Same, with 4 additional control points |

Types 4–7 use recursive patch subdivision (O(P · 4^D) where P = patch count, D = subdivision depth).

---

## 5. Layer 3 — Editor: Text Model & Mutation

### 5.1 Edit Session (`src/editor/edit_session.rs`, 1,155 LOC)

**EditableFrame** — one PDF show operator (Tj/TJ):

```rust
pub struct EditableFrame {
    pub text: String,           // Unicode decoded
    pub x: f64,                 // device pixels (CTM-transformed)
    pub y: f64,
    pub font_size_px: f64,
    pub font_name: String,
    pub font_key: String,       // resource name e.g. "F1"
    pub stream_idx: usize,      // which /Contents stream
    pub stream_op_index: usize, // operator index in that stream
    pub scale_x: f64,           // text→device horizontal scale
}
```

**EditBlock** — grouped frames that form one visual text unit:

```rust
pub struct EditBlock {
    pub id: usize,
    pub text: String,           // concatenated from member frames
    pub x: f64,                 // block origin (PDF user-space)
    pub y: f64,
    pub width: f64,             // right_edge - left_edge
    pub font_size: f64,
    pub font_key: String,
    pub font_name: String,
    pub stream_idx: usize,
    pub op_range: (usize, usize),   // first..last operator index
    pub frame_ids: Vec<usize>,
    pub scale_x: f64,
    pub composite: bool,        // Type0/CID font flag
}
```

**Frame grouping rule** (mirrors web overlay clustering):

Two consecutive frames belong in the same block when:
1. Same content stream (`stream_idx` equal)
2. Same font resource key (`font_key` equal)
3. Vertical drift within `0.4 × font_size`
4. Horizontal gap within `[-0.1 × font_size, 2.0 × font_size]` of previous right edge

**`build_text_model` algorithm:**

```
build_edit_session(doc, page_index)
   → scan all /Contents streams
   → dispatch through ContentInterpreter (recording TextSpan)
   → measure each frame width via font metrics
   → group into EditBlocks by rule above
   → return TextModel { blocks, session }
```

**Complexity:** O(F · log F) where F = frame count (font metric lookups are cached).

### 5.2 Document Editor (`src/editor/document_editor.rs`)

```rust
pub struct PdfEditor {
    pub doc: PdfDocument,           // original (immutable)
    pub writer: PdfWriter,          // accumulates changed/new objects
    pub mode: EditMode,             // WriteAppend | WriteNew
    original_xref_offset: u64,
    catalog_id: u32,
    pages_id: u32,
    info_id: Option<u32>,
}
```

**Copy-on-Write reads:**
1. Check `writer.pool` for object ID
2. If found: return writer's version (pending edit wins)
3. Else: return `doc.get_object(id)` (original)

**Save modes:**

| Scenario | Mode | Mechanism |
|----------|------|-----------|
| Text edit, annotations, page ops | WriteAppend | Append new objects + xref with `/Prev` |
| Redaction, merge | WriteNew | Full PDF rewrite with ID renumbering |

**`save_append` algorithm:**

```
1. If writer pool empty → return original bytes unchanged
2. Serialize pool objects at base_offset = original.len()
3. Build xref section covering only new/modified IDs
4. Build trailer with /Prev = original_xref_offset
5. Return concat(original, serialized_pool, xref, trailer, startxref)
```

Result is a valid PDF. Any conforming reader resolves the chain: `/Prev` points to old xref; new xref shadows only changed entries.

**Key methods:**

| Method | What it does |
|--------|-------------|
| `open(data)` | parse doc, extract IDs, init writer |
| `get_object(id)` | CoW read (writer pool first) |
| `replace_object(id, obj)` | queue CoW replacement |
| `add_object(obj) → u32` | allocate fresh ID, queue it |
| `get_page_dict(index)` | walk page tree with CoW awareness |
| `save_append(original)` | incremental append |

### 5.3 Text Commit (`src/editor/text_commit.rs`, 250 LOC)

**Surgical commit pattern:**

```rust
pub fn commit_block(
    editor: &mut PdfEditor,
    model: &mut TextModel,
    page_index: usize,
    block_id: usize,
    bytes: &[u8],   // already encoded in block's font encoding
) -> Result<()>
```

**Algorithm:**

```
1. Locate block by ID in model.blocks
2. Find all Tj/TJ operators in block.op_range
3. Replace PRIMARY operator's operand with new bytes
4. Blank secondary operators (empty string operands) — preserves positioning ops
5. Reserialize the stream: serialize_operations(ops) → Vec<u8>
6. make_flate_stream(bytes) → PdfStream
7. editor.add_object(stream) → new_id
8. editor.replace_object(content_stream_id, Reference(new_id))
```

Secondary operators are blanked rather than removed so their preceding `Tf`/`Tm`/`Td` operators remain in the stream (preserving layout state for any text that follows).

**Three-tier font encoding fallback:**

```
Tier 1  →  Encode in original font
              └─ fast, zero overhead, correct for 80%+ of edits
Tier 2  →  Fall back to bundled Liberation font (Sans/Serif/Mono)
              └─ covers ASCII for scanned PDFs with no embedded font
Tier 3  →  Embed new CID font (commit_block_with_font)
              └─ insert /EdN Tf before primary op
              └─ register font in page /Resources/Font
              └─ encode in Identity-H CID encoding
```

**Complexity:** O(N) where N = operator count in content stream.

### 5.4 Text Edit Flow (full sequence)

```
JS: text_edit_enter(editor, page_idx)
    │
    └─ build_text_model
         ├─ reparses from save_append output if edits pending
         └─ returns JSON: [{id, text, x, y, width, font_name, …}]

JS: text_edit_open(editor, block_id)
    └─ returns block content + font advance widths for inline editor

JS: text_edit_commit(editor, block_id, new_text)
    ├─ encode new_text in block font (Tier 1)
    ├─ if fails: try Liberation (Tier 2)
    ├─ if fails: embed CID font (Tier 3), return {missing: [chars]}
    └─ on success: commit_block → CoW chain updated → JSON {committed: true}

JS: text_edit_cancel(editor)
    └─ tear down session, no CoW changes written
```

### 5.5 Page Editor (`src/editor/page_editor.rs`, 461 LOC)

**ContentLayer pattern (draw-on-top):**

```rust
let layer = begin_edit_page(editor, page_index)?;
layer.builder.set_fill_rgb(1.0, 0.0, 0.0).rect(10.0, 10.0, 100.0, 50.0).fill();
layer.commit(&mut editor)?;
```

On commit, the new stream is appended to the page's `/Contents` array. PDF renders array in order, so new content paints on top of existing.

**Other page operations:**

| Function | What it does |
|----------|-------------|
| `add_blank_page(editor, w, h)` | allocate page + update pages node, return page ID |
| `delete_page(editor, index)` | remove from page tree, update `/Count` |
| `rotate_page(editor, index, angle)` | modify `/Rotate` in page dict |
| `set_crop_box(editor, index, rect)` | modify `/CropBox` |

### 5.6 Annotation Editor (`src/editor/annotation.rs`, 449 LOC)

**AnnotationType enum covers:**

```
Highlight | Underline | StrikeOut | Squiggly (all: color + quad_points)
Note (icon, color)
FreeText (text, font_size, color)
Circle | Square (color, fill)
Line (color, start, end)
Ink (color, Vec<Vec<[f64;2]>>)
Redact
```

**add_annotation algorithm:**

```
1. Allocate annotation object dict (/Type /Annot, /Subtype, /Rect, /C, …)
2. Get page dict (CoW)
3. Resolve or create /Annots array
4. Append annotation reference
5. Update page dict via replace_object
```

### 5.7 Redaction (`src/editor/redact.rs`, 788 LOC)

```rust
pub fn apply_redactions(
    editor: &mut PdfEditor,
    zones: &[RedactZone],   // [{page_index, rect:[x1,y1,x2,y2]}]
    fill_color: [f64; 3],
) -> Result<()>
```

**Algorithm:** Uses `WriteNew` mode — full PDF rewrite.

```
For each page:
  1. Parse content stream into operations
  2. Dispatch through suppression OutputDevice
     └─ tracks path/text bounding boxes
     └─ skips operators whose bbox intersects any redact zone
  3. Emit new content stream without redacted content
  4. Overwrite with black/colored rectangle where zone was
5. Serialize complete new PDF (no incremental append)
```

**Complexity:** O(D · P) where D = total document size, P = page count.

---

## 6. Layer 4 — Writer: Objects → PDF Bytes

### 6.1 PdfWriter (`src/writer/document.rs`)

```rust
pub struct PdfWriter {
    pool: Vec<PoolEntry>,   // {id, gen, obj}
    next_id: u32,
}
```

| Method | What it does |
|--------|-------------|
| `new()` | IDs start at 1 (new doc) |
| `new_from_max_id(max)` | IDs start at max+1 (incremental) |
| `reserve_id() → u32` | allocate without object |
| `add_object(obj) → u32` | allocate + store |
| `set_object(id, obj)` | replace/insert at specific ID |
| `serialize_all(root, info, prev_xref)` | full PDF bytes |

**`serialize_all` algorithm:**

```
1. Write PDF header: %PDF-1.7\n%<binary comment>
2. Sort objects by ID (deterministic output)
3. For each object: write "ID GEN obj\n" + object + "\nendobj"
4. Record byte offset of each object
5. Compute /Size = max_id + 1
6. Write xref section:
     xref\n0 N\n0000000000 65535 f \n...per-id 20-byte entries
7. Write trailer: << /Size N /Root R /Info R /Prev prev_xref >>
8. Write startxref\nOFFSET\n%%EOF
```

### 6.2 Object Serialization (`src/writer/serializer.rs`)

| Object type | Output format |
|-------------|--------------|
| Null | `null` |
| Boolean | `true` / `false` |
| Integer | decimal ASCII |
| Real | 6 decimal places, trailing zeros trimmed |
| String | `(...)` if printable ASCII + balanced parens; else `<hex>` |
| Name | `/name` with `#HH` escapes for special chars |
| Array | `[ item1 item2 ... ]` |
| Dictionary | `<< /Key1 val1 /Key2 val2 >>` |
| Stream | `<< dict >> stream\n<bytes>\nendstream` |
| Reference | `ID GEN R` |

**Complexity:** O(N) where N = serialized size.

### 6.3 Content Builder (`src/writer/content_builder.rs`, 535 LOC)

Fluent API for emitting PDF operators:

```rust
let bytes = ContentBuilder::new()
    .save()
    .set_fill_rgb(1.0, 0.0, 0.0)
    .rect(10.0, 10.0, 100.0, 50.0)
    .fill()
    .restore()
    .build();
```

Covers all path, text, image, and graphics state operators. `build()` serializes to bytes in O(N).

### 6.4 Font Writing

**Standard 14:** Reference dict only; no embedded data (readers have them built-in).

**TrueType embedding algorithm:**

```
1. Parse TTF tables (sfnt format)
2. Embed font program as FontFile2 stream (/Subtype /CIDFontProgram)
3. Create FontDescriptor dict with metrics (ascent, descent, bbox, flags)
4. Create Font dict: /Type /Font /Subtype /CIDFontType2 /DescendantFonts /ToUnicode
5. Write all objects, return font dict ID
```

**Complexity:** O(G · log G) where G = glyph count.

---

## 7. Layer 5 — WASM API: Rust → JavaScript

### 7.1 Exported Types

**WasmDocument (read-only):**

| Method | Return | Notes |
|--------|--------|-------|
| `parse(bytes)` | `WasmDocument` | entry point |
| `page_count()` | `usize` | |
| `get_metadata()` | JSON string | title, author, subject, creator, dates |
| `get_outline()` | JSON string | bookmarks tree |
| `page_size(index)` | `[width, height]` | in PDF points |
| `list_annotations(index)` | JSON string | all annot types |
| `extract_text_spans(index)` | JSON string | TextSpan array with positions |
| `extract_text(index)` | string | plain text |

**WasmEditor (read-write):**

```rust
pub struct WasmEditor {
    editor: PdfEditor,
    edit_model_doc: Option<(usize, PdfDocument)>,  // reparsed doc after edits
}
```

| Method | Notes |
|--------|-------|
| `open(bytes)` | |
| `save()` | returns PDF bytes |
| `add_annotation(...)` | |
| `delete_annotation(...)` | |
| `add_blank_page(w, h)` | |
| `delete_page(index)` | |
| `set_metadata(...)` | |
| `draw_text(...)` | ContentLayer append |
| `draw_rect(...)` | ContentLayer append |

**WasmTextEdit functions:**

```rust
text_edit_enter(editor, page_index) → JSON { blocks: [{id, text, x, y, width, …}] }
text_edit_open(editor, block_id)    → JSON { content, advances: [f64] }
text_edit_commit(editor, block_id, new_text) → JSON { committed, missing: [char] }
text_edit_cancel(editor)            → ()
```

### 7.2 Build Output

```bash
wasm-pack build --features wasm
```

Produces:
- `pkg/pdf_core.js` — JS wrapper with memory management
- `pkg/pdf_core_bg.wasm` — binary module
- `pkg/pdf_core.d.ts` — TypeScript types

**Size breakdown:**

| Feature | Size impact |
|---------|------------|
| Parser + document + fonts (baseline) | ~1.0 MB |
| `render` | +2.5 MB |
| `writer` | +0.8 MB |
| `crypto` | +0.3 MB |
| `wasm` | +0.2 MB |
| `forms` | +0.1 MB |
| **Full build** | **~4.5 MB** uncompressed / **~1.8 MB** gzip |

### 7.3 WASM Constraints

- No async I/O (single-threaded WASM execution model)
- No native file I/O (all data passed as `&[u8]`)
- No native fonts (must embed or use web fonts)
- `RefCell` panics if borrow overlaps — acceptable only in single-threaded WASM
- All errors returned as JSON `{"error": "..."}` — never thrown as JS exceptions from internal failures

---

## 8. Fonts Subsystem

### 8.1 Eight submodules

| File | LOC | Purpose |
|------|-----|---------|
| `encoding.rs` | 1,255 | WinAnsi, MacRoman, Symbol, AGL lookup table |
| `cmap.rs` | 616 | ToUnicode / Identity-H CMap parser |
| `cff.rs` | 620 | CFF/Type2 glyph width extraction |
| `truetype.rs` | ~450 | TTF/OTF sfnt table parsing |
| `type1.rs` | ~300 | Type 1 / PostScript metric extraction |
| `font_cache.rs` | 500 | Lazy loading, per-font metrics cache |
| `standard.rs` | ~200 | 14 standard PDF font metric tables |
| `types.rs` | ~200 | FontType, FontDescriptor, FontWidths |

### 8.2 Font Loading (lazy)

```
First get_font(doc, font_dict)
    │
    ├─ parse /Type, /Subtype, /Encoding, /Widths, /W
    ├─ load font program if /FontDescriptor /FontFile exists
    │     ├─ FontFile  → Type 1 parser
    │     ├─ FontFile2 → TrueType parser
    │     └─ FontFile3 → CFF parser
    ├─ parse /ToUnicode CMap stream
    └─ insert into font_cache (Arc<CachedFont>)

Subsequent get_font → O(1) cache hit
```

### 8.3 Glyph Width Resolution

```
char_code
    │
    ├─ CID font (/W array): [(cid, [width…]) or (cid_start, cid_end, width)]
    │     → binary search, O(log N)
    │
    ├─ Simple font (/Widths array, FirstChar offset):
    │     → direct index, O(1)
    │
    └─ Default width (/DW or 1000 units)
```

Widths in 1/1000 font units → converted to points by `width × font_size / 1000`.

---

## 9. End-to-End Data Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                         INPUT: raw PDF bytes                    │
└──────────────────────────────┬──────────────────────────────────┘
                               │
                    PdfDocument::parse(data)
                               │
               ┌───────────────▼───────────────┐
               │           PdfDocument          │
               │  xref map | trailer | caches   │
               └───────┬───────────────┬────────┘
                       │               │
            ┌──────────▼──────┐   ┌───▼────────────────────────┐
            │    RENDERER     │   │          EDITOR             │
            │                 │   │                             │
            │ get_object(id)  │   │  PdfEditor::open(data)      │
            │ decode stream   │   │    writer = PdfWriter::new  │
            │ ContentInterp.  │   │                             │
            │   operator      │   │  build_text_model           │
            │   dispatch      │   │    → build_edit_session     │
            │                 │   │    → ContentInterpreter     │
            │ draw_text_span  │   │      (scan all ops)         │
            │ fill_path       │   │    → group into EditBlocks  │
            │ draw_image      │   │                             │
            │                 │   │  text_edit_commit           │
            │ PixmapBuffer    │   │    → encode bytes           │
            │ (RGBA pixels)   │   │    → replace Tj operand     │
            └─────────────────┘   │    → reserialize stream     │
                                  │    → make_flate_stream      │
                                  │    → editor.add_object      │
                                  │                             │
                                  │  save_append                │
                                  │    → serialize pool         │
                                  │    → build xref + trailer   │
                                  │    → concat to original     │
                                  └────────────┬────────────────┘
                                               │
                               ┌───────────────▼──────────────────┐
                               │     OUTPUT: valid PDF bytes       │
                               │     (incremental append format)   │
                               └──────────────────────────────────┘
```

---

## 10. Algorithmic Complexity

### Core algorithms

| Function | Time | Space | Notes |
|----------|------|-------|-------|
| `PdfDocument::parse` | O(N) | O(N) | N = file size; single xref pass |
| `get_object(id)` | O(1) + O(M) | O(1) | hash lookup + optional stream decompress |
| `resolve(ref)` | O(D) | O(1) | D = indirection depth (typical ≤3, max ≈10) |
| `Lexer::next_token` | O(T) | O(1) | T = token length |
| `parse_content_stream` | O(N) | O(N) | N = stream bytes |
| `ContentInterpreter::interpret` | O(N) | O(S) | S = graphics state stack depth |
| `build_text_model` | O(F log F) | O(F) | F = frame count |
| `render_page` | O(W · H) | O(W · H) | page pixel area |
| `render_tile` | O(T) | O(T) | tile pixel area |
| Glyph rasterize (first) | O(G) | O(G) | G = glyph outline complexity |
| Glyph rasterize (cached) | O(1) | O(1) | per-font glyph cache |
| `commit_block` | O(N) | O(N) | N = stream operator count |
| `save_append` | O(P · M) | O(P · M) | P = pool size, M = avg object size |
| `apply_redactions` | O(D · P) | O(D) | full rewrite, D = doc size, P = pages |
| CMap parse | O(N) | O(N) | N = CMap stream bytes |
| CFF width extraction | O(G) | O(G) | G = glyph count |
| Font cache lookup | O(1) | — | after first load |
| Filter pipeline | O(N · K) | O(N) | N = data size, K = filter count (≤3) |
| Shading (Gouraud) | O(P · 4^D) | O(P) | P = patches, D = subdivision depth |
| Object serialization | O(N) | O(N) | N = serialized size |

### Bottlenecks

1. **Page rasterization** is O(W · H) — dominated by glyph blit and alpha compositing at high DPI
2. **Re-rendering after edit** requires reparsing `save_append` output — O(file_size) on each edit if not cached
3. **Redaction** forces full rewrite — O(D · P)
4. **Font embedding (Tier 3)** requires TTF parsing + glyph table scan — O(G log G)

---

## 11. Problems & Technical Debt

### 11.1 Fixed Issues (recent, per `.doc/` reports)

| Issue | Root cause | Fix |
|-------|-----------|-----|
| CID text scrambled on edit | Frontend: `cancelBlockEdit` before `commitBlockEdit`; backend: font resolution missed indirect refs + inheritance | Reorder commit/cancel; `effective_resources()` merges inherited + local |
| Re-entry shows old text | TextModel built from pristine `doc`, not writer pool | Reparse from `save_append()` output; cache per pool size |
| CJK text wrong shifts | Off-by-one in CMap range decoding | Corrected loop bounds in `parse_cmap_range` |
| Slow re-render after edit | Font/glyph decoded per tile, not shared | `RenderCache` shared across all tiles on a page (~80% speedup) |

### 11.2 Open / Incomplete Features

| Feature | Status | Risk |
|---------|--------|------|
| **Font subsetting** | Partial — full TTF embedded, no glyph subsetting | Large WASM payloads if many fonts embedded |
| **Digital signatures** | Not started | Requires PKCS#7 + ECC; estimated 2+ weeks |
| **CJK fallback embedding** | Basic — CID/Identity-H works, but no CID font fallback when original font missing | Broken CJK edit in scanned PDFs |
| **Gradient shading (Types 4–7)** | ~80% — patch types nearly done | Visual artifacts in complex gradients |
| **Tiling patterns** | Skeleton — rendered as solid fill | Patterned backgrounds look wrong |
| **Type 3 fonts** | Not supported — treated as fallback | Custom symbol fonts lost |
| **Linearized PDFs** | Parsed but no special handling | Slightly slower xref resolution |
| **RC4-40 bit encryption** | Untested | May fail on old PDFs |
| **Permission bit enforcement** | Not implemented | Ignores /Encrypt /P field |
| **Undo / history** | Not implemented | No rollback once `commit_block` runs |

### 11.3 Structural Technical Debt

**1. `HashMap` for PdfObject Dictionary — insertion order not preserved**

PDF dict ordering is significant for some constructs (e.g. Content stream dicts with `/Filter`). The current `HashMap<String, PdfObject>` loses insertion order. Should be `IndexMap` or `Vec<(String, PdfObject)>`.

**2. `RefCell` for caches inside `PdfDocument`**

Interior mutability works in WASM's single-threaded model but will panic if ever called from multiple threads (e.g. native multi-threaded rendering). A future `Arc<Mutex<>>` migration is needed for parallel tile rendering on native.

**3. String-based operator dispatch in interpreter**

`dispatch(op: &str, ...)` does `match op { "BT" => ..., "ET" => ... }` string comparison for ~200 operators. Should be an enum with a pre-built lookup table for O(1) dispatch and exhaustiveness checking. Current approach has O(N_operators) worst case.

**4. Content stream round-trip fidelity**

`parse_content_stream → modify ops → serialize_operations` is not lossless:
- Comments are stripped
- Inline image whitespace may differ
- Real number formatting may change (6 decimal vs original)

This can invalidate checksums in signed PDFs and produces larger deltas than necessary.

**5. `edit_model_doc` reparsing on every re-entry**

`WasmEditor::edit_model_doc` caches the reparsed document but invalidates it on every pool change. A pool-size comparison is used as the cache key — this is a heuristic, not a reliable invalidation signal. A generation counter on the writer pool would be more robust.

**6. No Undo**

`commit_block` writes directly to the CoW pool. There is no snapshot mechanism. The only "undo" is to reload the original bytes. For a production editor, a command-pattern undo stack is essential.

**7. Inline image EI boundary detection**

The `EI` scanner uses a whitespace-preceded-`EI` heuristic. JPEG data can contain `EI` bytes, so the scanner may terminate early. The correct approach (per the PDF spec) is to read exactly `/Length` bytes from `ID`, but many real PDFs omit `/Length` in inline images.

**8. No streaming parser — full file in memory**

`PdfDocument` holds the entire file as `Vec<u8>`. For multi-hundred-MB PDFs this is a problem. A proper PDF parser should support memory-mapped or seekable I/O with on-demand object loading.

**9. Missing: incremental save for redaction**

Redaction forces `WriteNew` (full rewrite). A standards-compliant redaction could be done incrementally (mark as redacted via annotation, then flatten), but the current design rewrites everything.

**10. Font metrics estimated when font missing**

If a referenced font is not embedded and is not one of the Standard 14, glyph widths are estimated (typically 500 units or proportional to character code). This produces incorrect text positioning for rendering and incorrect frame boundaries for editing.

### 11.4 Known WASM-specific Issues

- All parsing + rendering is synchronous; large PDFs block the JS event loop
- No Web Worker integration — caller must wrap in a worker manually
- WASM memory grows but never shrinks (Rust allocator behavior); long-lived sessions accumulate memory
- No streaming render — full page rasterized before any pixels returned to JS

---

*Generated from source analysis of pdf-editor-rust-core commit HEAD, 2026-06-05.*
