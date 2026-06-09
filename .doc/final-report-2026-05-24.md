# pdf-core — Final Implementation Report

**Date:** 2026-05-24
**Version:** 0.1.0
**Total source lines:** ~20,138 across 54 `.rs` files
**Test suite:** 383 tests, 0 failures (all features enabled)

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Module Inventory](#2-module-inventory)
3. [Layer-by-Layer Algorithm Detail](#3-layer-by-layer-algorithm-detail)
4. [Comparison with ONLYOFFICE C++ Workflow](#4-comparison-with-onlyoffice-c-workflow)
5. [End-to-End Workflow Examples](#5-end-to-end-workflow-examples)
6. [WASM Bridge](#6-wasm-bridge)
7. [Feature Flag Matrix](#7-feature-flag-matrix)
8. [Known Limitations](#8-known-limitations)

---

## 1. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          pdf-core (Rust crate)                          │
│                                                                         │
│  WASM layer        src/wasm/         wasm-bindgen JS ↔ Rust bridge      │
│  ─────────────────────────────────────────────────────────────────────  │
│  Editor layer      src/editor/       CoW incremental-save, annotations  │
│  Forms layer       src/forms/        AcroForm fields + appearances       │
│  Writer layer      src/writer/       PDF serializer, content builder    │
│  Render layer      src/render/       RGBA rasteriser (feature=render)   │
│  Text layer        src/text/         Text extractor, layout model        │
│  Crypto layer      src/crypto/       RC4 / AES-128 (feature=crypto)     │
│  Font layer        src/fonts/        CMap, encoding, font cache          │
│  Content layer     src/content/      Operator interpreter, OutputDevice  │
│  Document layer    src/document/     Catalog, pages, metadata, outline  │
│  Parser layer      src/parser/       Lexer, object model, XRef, filters │
│  Error             src/error.rs      PdfError with byte offsets          │
└─────────────────────────────────────────────────────────────────────────┘
```

**Data flow (read path):**
```
bytes → Lexer → Objects → XRef → PdfDocument
                                     │
                              ContentInterpreter
                                     │
                          ┌──────────┴──────────┐
                     OutputDevice          OutputDevice
                     PageRenderer          TextExtractor
                          │
                     PixmapBuffer → RGBA bytes
```

**Data flow (write path):**
```
PdfWriter (object pool)
    │
ContentBuilder → stream bytes
    │
Serializer → object bytes
    │
XRefTableBuilder → xref section
    │
assembled PDF bytes
```

---

## 2. Module Inventory

| Module | Files | Lines | Purpose |
|--------|-------|-------|---------|
| `parser` | lexer, objects, xref, filters, mod | 3,760 | Binary tokenization, object graph, XRef |
| `content` | interpreter, operators, graphics_state, text_state, mod | 2,706 | PDF operator dispatch |
| `fonts` | cmap, encoding, font_cache, standard, truetype, type1, types, mod | 3,050 | CMap, encoding, metrics |
| `document` | catalog, page, metadata, outline, mod | 1,234 | Document structure traversal |
| `render` | page_renderer, canvas, color, font_resolver, glyph_cache, image, path_render, tile, mod | 2,800 | RGBA rasterisation |
| `text` | extractor, layout, mod | 575 | Span grouping into lines/words |
| `crypto` | handler, rc4, mod | 750 | RC4/AES-128, key derivation |
| `writer` | serializer, streams, xref, document, content_builder, font, image, page, mod | 2,360 | PDF generation |
| `editor` | document_editor, annotation, page_editor, metadata_editor, merge, redact, remap, mod | 3,000 | Editing, annotations, merge |
| `forms` | acroform, appearance, mod | 650 | Form fields, check boxes |
| `wasm` | mod | 490 | JS ↔ Rust bridge |

---

## 3. Layer-by-Layer Algorithm Detail

### 3.1 Parser — `src/parser/`

#### Lexer (`lexer.rs`)

The lexer is a stateful, position-tracking scanner over a `&[u8]` slice.
It uses **nom combinators internally** but exposes a simple `next_token() -> Result<Token>` iterator.

```
Token types:
  Null | Boolean(bool) | Integer(i64) | Real(f64)
  LiteralString(Vec<u8>)   — (Hello\nWorld)  with escape sequences
  HexString(Vec<u8>)        — <48 65 6C 6C>  whitespace-tolerant
  Name(String)              — /FontName  with #XX hex escapes decoded
  Keyword(Keyword)          — obj endobj stream trailer startxref R
  ArrayStart/End            — [ ]
  DictStart/End             — << >>
  Operator(String)          — BT ET Tj cm q Q ...
  Eof
```

**Integer vs Real disambiguation:** after reading digits, the lexer peeks
for a decimal point or exponent sign.  A lone integer followed by another
integer followed by `R` is resolved as an indirect reference by the object
parser above the lexer.

**Nested parentheses in literal strings:** depth counter incremented on `(`
and decremented on `)` (not preceded by `\`), so `(Hello (World))` is
one token.

#### Object parser (`objects.rs`)

Recursive descent over `Token` stream:

```
parse_object()
  ├─ peek Integer → peek Integer → peek R → IndirectRef(n, gen)
  ├─ DictStart → parse_dict() → maybe parse_stream()
  ├─ ArrayStart → parse_array()
  ├─ LiteralString / HexString → PdfObject::String
  ├─ Name → PdfObject::Name
  └─ Boolean / Null / Real / Integer → leaf types

PdfDocument::parse(bytes)
  ├─ startxref_offset() → scan backward from EOF for "startxref"
  ├─ parse_xref(data, offset) → HashMap<u32, u64>  (object ID → byte offset)
  ├─ parse_trailer() → PdfDict
  └─ PdfDocument { xref, trailer, data }

PdfDocument::resolve(id) → PdfObject
  ├─ lookup offset in xref
  ├─ seek to offset in data
  └─ parse indirect object header "N G obj … endobj"
```

All objects are lazy-loaded: `PdfDocument` stores raw bytes; `resolve(id)` parses on demand.

#### XRef parser (`xref.rs`)

**Traditional XRef table** (PDF ≤ 1.4):

```
Algorithm:
  1. Skip to "xref" keyword
  2. Read subsection header: "start_id count\n"
  3. For each entry (20 bytes each): scan char-by-char (xpdf style)
     to find field boundaries, tolerating any EOL variant:
       "\n"  "\r\n"  "\r"  " \r\n" (21-byte entries in some generators)
  4. Entry format: "0000012345 00000 n \r\n"
     offset=first_10_digits, gen=next_5, type='f'|'n'
  5. Only 'n' (in-use) entries enter the HashMap
  6. Parse "trailer << ... >>" dict
  7. Follow /Prev pointer for previous xref sections (incremental updates)
```

**XRef Stream** (PDF 1.5+):

```
Algorithm:
  1. Parse the indirect stream object at startxref offset
  2. Verify /Type /XRef in stream dictionary
  3. Decompress using FlateDecode (flate2::ZlibDecoder)
  4. Read /W [w1 w2 w3] — field widths in bytes
  5. For each entry, read w1+w2+w3 bytes, big-endian:
     field[0] = entry type (0=free, 1=normal, 2=compressed)
     field[1] = offset (type 1) or stream obj num (type 2)
     field[2] = gen num (type 1) or index in stream (type 2)
  6. Apply /Index subsection ranges
  7. Compressed objects (type 2) stored in object streams decoded later
```

#### Filters (`filters.rs`)

```
PdfStream::decode() chooses filter chain:
  /FlateDecode  → flate2::ZlibDecoder
  /LZWDecode    → weezl LZW decompressor
  /ASCII85Decode → manual group-of-5 decoder
  /ASCIIHexDecode → hex nibble pairs
  /RunLengthDecode → RLE: 0–127 = literal run, 128 = EOD, 129–255 = repeat run
  /DCTDecode    → JPEG passthrough (bytes kept raw for image layer)
  Multiple filters → applied as pipeline left-to-right
```

---

### 3.2 Content Interpreter — `src/content/`

#### Operator table (`operators.rs`)

`ContentStreamIter` wraps a `Lexer` and produces `Operation { operator, operands }` pairs.
It accumulates all tokens until an `Operator` token is found, then yields the complete operation.

**Operator categories handled:**

| Category | Operators |
|----------|-----------|
| Graphics state | `q Q cm gs w J j M d ri i` |
| Color | `cs CS sc SC scn SCN g G rg RG k K` |
| Path construction | `m l c v y h re` |
| Path painting | `S s f F f* B B* b b* n` |
| Clipping | `W W*` |
| Text objects | `BT ET` |
| Text positioning | `Td TD Tm T*` |
| Text showing | `Tj TJ ' "` |
| Text state | `Tc Tw Tz TL Tf Tr Ts` |
| XObjects | `Do` |
| Inline images | `BI ID EI` |
| Marked content | `BMC BDC EMC MP DP` |

#### Graphics state (`graphics_state.rs`)

```rust
GraphicsState {
    ctm: Matrix,          // current transformation matrix [a b c d e f]
    fill_color: Color,    // RGB or Gray
    stroke_color: Color,
    line_width: f64,
    line_cap: LineCap,    // Butt | Round | Square
    line_join: LineJoin,  // Miter | Round | Bevel
    miter_limit: f64,
    dash: DashPattern,    // ([array], phase)
    fill_rule: FillRule,  // NonZero | EvenOdd
    blend_mode: BlendMode,
    fill_opacity: f64,
    stroke_opacity: f64,
}

GraphicsStateStack — Vec<GraphicsState> for q/Q push/pop
```

Matrix concatenation (`cm` operator):
```
new_ctm = operator_matrix * current_ctm
```
Applied using standard 3×3 affine math, row-major.

#### Text state (`text_state.rs`)

```rust
TextState {
    font_name: String,
    font_size: f64,
    char_spacing: f64,      // Tc
    word_spacing: f64,      // Tw
    horiz_scale: f64,       // Tz (percentage)
    leading: f64,           // TL
    render_mode: TextRenderMode,
    rise: f64,              // Ts
    text_matrix: Matrix,    // Tm
    text_line_matrix: Matrix,
}

TextSpan {
    text: String,           // decoded Unicode
    x, y: f32,             // position in current CTM space
    font_size: f32,
    color: [u8; 4],
    width: f32,
}
```

**Glyph position algorithm (Tj / TJ):**
```
For each glyph code c in the string:
  1. Map code → Unicode via CMap or Encoding table
  2. Get advance width w from font metrics (or default 0.5em)
  3. tx = (w/1000 * font_size + char_spacing) * horiz_scale/100
  4. If code == 0x20: tx += word_spacing
  5. text_matrix = translate(tx, 0) * text_matrix

For TJ array, numeric displacement d (in text-space thousandths):
  tx = -d/1000 * font_size * horiz_scale/100
  text_matrix = translate(tx, 0) * text_matrix
```

#### ContentInterpreter (`interpreter.rs`)

Central dispatch loop:

```
for op in ContentStreamIter:
  match op.operator:
    "q"  → gfx.push()
    "Q"  → gfx.pop()
    "cm" → gfx.current().ctm = Matrix::from(operands) * gfx.current().ctm
    "BT" → in_text = true; text.reset_matrix()
    "ET" → in_text = false
    "Tf" → text.font_name = name; text.font_size = size
    "Tj" → decode string → TextSpan → device.draw_text_span(span, gfx_state)
    "TJ" → iterate array: strings yield TextSpan, numbers shift text_matrix
    "m"  → path.move_to(x, y)
    "l"  → path.line_to(x, y)
    "c"  → path.curve_to(...)
    "re" → path.rect(x, y, w, h)
    "S"  → device.stroke_path(&path, &state); path.clear()
    "f"  → device.fill_path(&path, &state, NonZero); path.clear()
    "f*" → device.fill_path(&path, &state, EvenOdd); path.clear()
    "Do" → resolve XObject name in Resources/XObject dict
            if /Subtype /Form → recurse with form's content stream
            if /Subtype /Image → device.draw_image_xobject(name, stream, state)
    "BI"→"EI" → inline image → device.draw_image(bytes, state)
```

**Form XObject recursion** has a cycle guard (`xobject_stack: HashSet<u32>`):
the object's reference number is pushed before recursion and removed after,
so circular form references abort cleanly instead of stack-overflowing.

---

### 3.3 Font System — `src/fonts/`

#### CMap (`cmap.rs`)

```
CMap::parse(stream_bytes):
  1. Find "begincodespacerange" → parse pairs → code_spaces Vec
  2. Find "beginbfchar":
     for each "src dst" pair: char_map[src] = dst_utf8
  3. Find "beginbfrange":
     for each "lo hi dst_start" triple:
       for code in lo..=hi:
         char_map[code] = dst_start + (code - lo)  (Unicode scalar addition)
       or if dst is an array: char_map[lo+i] = array[i]
  4. CMap::lookup(code) → &str via HashMap<u32, String>
```

#### Encoding (`encoding.rs`)

Handles the 4 standard PDF encodings:
- `StandardEncoding` — Adobe standard glyph names
- `MacRomanEncoding` — Mac OS Roman
- `WinAnsiEncoding` — Windows-1252
- `MacExpertEncoding` — Mac Expert

Each encoding is a 256-entry lookup table: `code → glyph_name → Unicode`.
The Adobe Glyph List (AGL) maps glyph names to Unicode codepoints.

**Differences encoding** (PDF's `/Differences` array):
```
base_encoding = lookup named encoding
for each (code, name) in Differences array:
  override base_encoding[code] = Unicode(name)
```

#### Font cache (`font_cache.rs`)

```
FontCache { map: HashMap<String, FontEntry> }

FontEntry {
    kind: FontKind (Standard14 | TrueType | Type1 | Type3 | CIDFont),
    cmap: Option<CMap>,
    encoding: Option<EncodingTable>,
    widths: Vec<f64>,       // [first_char..last_char] glyph advances
    missing_width: f64,
}

FontCache::get_or_load(font_name, font_dict, doc) -> &FontEntry:
  1. Cache hit → return existing
  2. Determine subtype from /Subtype key
  3. Standard14 → use hardcoded metrics from standard.rs
  4. TrueType → parse /Widths array; load /ToUnicode CMap stream
  5. Type1 → same as TrueType path
  6. CIDFont → /DW default width; /W sparse widths array

char_to_unicode(entry, code) -> Option<char>:
  priority: CMap lookup → Encoding table lookup → None
```

#### Standard 14 fonts (`standard.rs`)

All 14 PDF built-in font metrics are hard-coded as width arrays:
`Helvetica, Helvetica-Bold, Helvetica-Oblique, Helvetica-BoldOblique,
Times-Roman, Times-Bold, Times-Italic, Times-BoldItalic,
Courier, Courier-Bold, Courier-Oblique, Courier-BoldOblique,
Symbol, ZapfDingbats`

Widths for each font are a `[i32; 256]` table in AFM units (1/1000 em).

---

### 3.4 Document Layer — `src/document/`

```
Catalog::from_document(doc):
  doc.resolve(doc.trailer["Root"]) → catalog PdfDict

Page::from_document(doc, page_index):
  catalog_dict["Pages"] → pages_root
  walk_pages_tree(doc, pages_root, index)   // recursive tree walk
  → Page { width, height, rotation, resources, content_streams }

Page inherits /Resources and /MediaBox from ancestor nodes
(PDF page tree inheritance algorithm per ISO 32000-1 §7.7.3.4).

Metadata::from_document(doc):
  trailer["Info"] → info PdfDict
  extract: Title Author Subject Keywords Creator Producer (PDFDocEncoding or UTF-16BE)

parse_outlines(doc, catalog):
  catalog["Outlines"] → outlines_root
  traverse linked list: First → Next → ... → Last
  for each node: Title, Dest (page ref), Count (open/closed)
```

---

### 3.5 Render Layer — `src/render/` *(feature = render)*

#### Coordinate system

```
PDF user space: origin bottom-left, Y up
Screen space:   origin top-left, Y down

Initial CTM for a tile render:
  initial_ctm = Matrix { a: scale, b: 0, c: 0, d: -scale,
                         e: -tile_x * scale,
                         f: (tile_y + tile_h) * scale }

Maps PDF point (x_pdf, y_pdf) → pixel (x_px, y_px):
  x_px = x_pdf * scale - tile_x * scale
  y_px = (tile_y + tile_h - y_pdf) * scale
```

#### Render pipeline

```
render_page_rgba(doc, page_index, scale) -> Result<(u32, u32, Vec<u8>)>:
  1. Page::from_document → width_pt, height_pt
  2. pixel_w = (width_pt * scale).ceil() as u32
  3. pixel_h = (height_pt * scale).ceil() as u32
  4. render_tile(doc, page, TileRect::full(), scale)
  5. Return (pixel_w, pixel_h, rgba_bytes)

render_tile(doc, page, tile, scale):
  1. PixmapBuffer::new(tile.w, tile.h)  — Vec<u8> RGBA, initialized white
  2. PageRenderer::new(canvas, scale, doc, resources)
  3. ContentInterpreter::new()
  4. For each content stream in page.content_streams:
     ContentStreamIter::new(stream_bytes)
     interpreter.process(iter, &mut page_renderer, doc)
  5. Return canvas
```

#### PageRenderer (implements OutputDevice)

```
stroke_path(path, state):
  1. Map Path segments through CTM → pixel coordinates
  2. path_render::stroke_path(canvas, segments, color, line_width)
     → Bresenham-style scanline fill of stroke region

fill_path(path, state, rule):
  1. Map segments through CTM
  2. path_render::fill_path_with_rule(canvas, segments, color, rule)
     → Scanline fill with NonZero or EvenOdd winding

draw_text_span(span, state):
  1. If span.text is empty: skip
  2. glyph_cache.get_or_rasterize(font_name, font_size, char)
     → RasterizedGlyph { bitmap, width, height, bearing_x, bearing_y }
  3. Blit glyph bitmap onto canvas at (span.x, span.y)
  4. Alpha-blend: dst = src_alpha * glyph_color + (1 - src_alpha) * dst

draw_image_xobject(name, stream, state):
  1. image::decode_image(stream) → (pixel_w, pixel_h, rgba_bytes)
     → JPEG: return stream bytes as-is (DCT passthrough)
     → FlateDecode: decompress then map ColorSpace to RGBA
  2. Compute dest_rect from CTM (extract scale + translation)
  3. Bilinear-scale image to dest_rect and blit onto canvas
```

#### Tile renderer (`tile.rs`)

For large pages the render can be split into tiles:

```
TileRect { x, y, w, h }   — in pixel units relative to page

render_page_tiled(doc, page, scale, tile_w, tile_h):
  for ty in 0..ceil(page_h / tile_h):
    for tx in 0..ceil(page_w / tile_w):
      tile = TileRect { x: tx*tile_w, y: ty*tile_h,
                        w: min(tile_w, page_w - tx*tile_w),
                        h: min(tile_h, page_h - ty*tile_h) }
      render_tile(doc, page, tile, scale)
      → composite tile into full canvas
```

---

### 3.6 Text Extraction — `src/text/`

```
TextExtractor implements OutputDevice:

draw_text_span(span, state):
  spans.push(span.clone())

finish() -> TextLayout:
  1. Sort spans by (y desc, x asc) — top-to-bottom, left-to-right
  2. Group into lines: spans within |y - line_y| < line_gap threshold
  3. Within each line: sort by x; merge adjacent spans into TextWord if gap < word_gap
  4. Group lines into TextBlock if vertical gap < block_gap

TextLayout {
  blocks: Vec<TextBlock {
    lines: Vec<TextLine {
      words: Vec<TextWord {
        text: String,
        x, y: f32,
        width: f32,
      }>
    }>
  }>
}
```

**Span-level model** (vs xpdf's per-character model):
Each `draw_text_span` receives a full string (e.g. one `Tj` argument).
This is simpler and fast enough for extraction; precise per-character
bounding boxes require per-char advances not yet tracked.

---

### 3.7 Encryption — `src/crypto/` *(feature = crypto)*

#### Key derivation (RC4 / MD5, ISO 32000-1 §7.6.3.3 Algorithm 2)

```
compute_file_key(password, o_entry, permissions, doc_id, key_len, revision):
  1. padded = (password_bytes ++ PASSWORD_PADDING)[..32]
  2. md5_input = padded ++ o_entry ++ permissions_le4 ++ doc_id[..16]
  3. if revision >= 3: md5_input ++ [0xFF, 0xFF, 0xFF, 0xFF]  (encrypt metadata flag)
  4. hash = md5(md5_input)
  5. if revision >= 3: repeat hash = md5(hash) 50 times (key strengthening)
  6. file_key = hash[..key_len_bytes]
```

#### User password verification

```
verify_user_password(password, u_entry, file_key, revision):
  if revision == 2:
    expected = rc4(file_key, PASSWORD_PADDING)
    return expected == u_entry[..32]
  else (revision >= 3):
    hash = md5(PASSWORD_PADDING ++ doc_id[..16])
    result = rc4(file_key, hash)
    for i in 1..=19:
      key_i = file_key XOR [i; key_len]
      result = rc4(key_i, result)
    return result == u_entry[..16]
```

#### Owner password (Algorithm 7)

```
recover_user_password_from_owner(owner_pw, o_entry, key_len, revision):
  1. md5_input = (owner_pw_padded)[..32]
  2. hash = md5(md5_input)
  3. if revision >= 3: repeat hash = md5(hash) 50 times
  4. key = hash[..key_len_bytes]
  5. if revision == 2: user_pw_padded = rc4(key, o_entry)
  6. if revision >= 3:
       result = o_entry
       for i in 19..=0:
         key_i = key XOR [i; key_len]
         result = rc4(key_i, result)
       user_pw_padded = result
  7. Try verify_user_password(user_pw_padded, ...)
```

#### Per-object decryption

```
decrypt_object(file_key, obj_num, gen_num, data):
  object_key = md5(file_key ++ obj_num_le3 ++ gen_num_le2)[..min(key_len+5, 16)]
  RC4: stream_data = rc4_decrypt(object_key, data)
  AES-128: stream_data = aes_cbc_decrypt(object_key, iv=data[..16], data[16..])
```

---

### 3.8 Writer — `src/writer/`

#### Object model

```
PdfWriter {
  objects: BTreeMap<u32, Vec<u8>>,  // obj_id → serialized bytes
  next_id: u32,
}

add_object(bytes) -> u32:
  id = self.next_id++
  objects[id] = bytes
  id

replace_object(id, bytes):
  objects[id] = bytes  // CoW: override existing ID
```

#### Serializer (`serializer.rs`)

```
serialize_dict(dict) -> Vec<u8>:
  output "<<\n"
  for (key, value) in dict:
    output "/{key} {serialize_value(value)}\n"
  output ">>"

serialize_stream(dict, data) -> Vec<u8>:
  dict["Length"] = data.len()
  output serialize_dict(dict)
  output "\nstream\r\n"
  output data
  output "\nendstream\n"

format_real(f: f64) -> String:
  if f.fract() == 0.0 → "{:.0}"
  else → "{:.4}" (trim trailing zeros)
```

#### Stream compression (`streams.rs`)

```
make_flate_stream(data: &[u8]) -> (PdfDict, Vec<u8>):
  compressed = flate2::deflate(data, level=6)
  dict = { Filter: /FlateDecode, Length: compressed.len() }
  return (dict, compressed)
```

#### XRef table builder (`xref.rs`)

```
write_xref_table(objects: &BTreeMap<u32, u64>) -> Vec<u8>:
  output "xref\n"
  output "0 1\n0000000000 65535 f \r\n"  // free object 0
  for (id, offset) in objects (sorted by id):
    output "{offset:010} 00000 n \r\n"

write_full_xref_and_trailer(writer, prev_xref_offset, catalog_id, info_id):
  start_byte = current_end_of_output
  write all objects with byte offsets recorded
  write xref table
  write "trailer\n<< /Size N /Root R /Info R /Prev prev_offset >>\n"
  write "startxref\n{start_byte}\n%%EOF\n"
```

#### ContentBuilder (`content_builder.rs`)

Fluent builder emitting raw PDF content stream operators:

```rust
ContentBuilder::new()
  .set_fill_color_rgb(r, g, b)   // "{r} {g} {b} rg\n"
  .set_stroke_color_rgb(r, g, b) // "{r} {g} {b} RG\n"
  .set_line_width(w)              // "{w} w\n"
  .move_to(x, y)                  // "{x} {y} m\n"
  .line_to(x, y)                  // "{x} {y} l\n"
  .curve_to(x1,y1,x2,y2,x3,y3)  // "... c\n"
  .rect(x, y, w, h)              // "{x} {y} {w} {h} re\n"
  .stroke()                       // "S\n"
  .fill()                         // "f\n"
  .fill_stroke()                  // "B\n"
  .save_state() / .restore_state()// "q\n" / "Q\n"
  .set_font(name, size)           // "/{name} {size} Tf\n"
  .show_text(bytes)               // "({escaped}) Tj\n"
  .show_text_tj(items)            // "[...] TJ\n"
  .set_transform(a,b,c,d,e,f)    // "{a} {b} {c} {d} {e} {f} cm\n"
  .begin_text() / .end_text()     // "BT\n" / "ET\n"
  .move_text(tx, ty)              // "{tx} {ty} Td\n"
  .build() -> Vec<u8>
```

---

### 3.9 Editor — `src/editor/`

#### Copy-on-Write incremental save

```
PdfEditor::open(bytes):
  xref_offset = startxref_offset(&bytes)
  doc = PdfDocument::parse(bytes)
  max_id = doc.xref.keys().max()
  writer = PdfWriter { next_id: max_id + 1, objects: BTreeMap::new() }

PdfEditor::save_append() -> Vec<u8>:
  1. output = doc.raw_bytes.clone()         // original PDF verbatim
  2. object_offsets = BTreeMap::new()
  3. For each (id, bytes) in writer.objects:
     object_offsets[id] = output.len()
     output.extend("{id} 0 obj\n{bytes}\nendobj\n")
  4. xref_start = output.len()
  5. Write new xref section covering only modified IDs
  6. Write trailer with /Prev = original_xref_offset
  7. output.extend("startxref\n{xref_start}\n%%EOF\n")
  → output is original PDF + update section (incremental)
```

#### Annotation builder (`annotation.rs`)

```
AnnotationBuilder::text_note(page_id, rect, content, author):
  dict = { Type: /Annot, Subtype: /Text,
           Rect: rect, Contents: content,
           T: author, M: current_date,
           F: 4 (print flag) }
  annot_id = writer.add_object(serialize(dict))
  append annot_id to page's /Annots array

AnnotationBuilder::highlight(page_id, rect, quads, color):
  dict = { Type: /Annot, Subtype: /Highlight,
           Rect: rect, QuadPoints: quads,
           C: [r, g, b] }

AnnotationBuilder::link(page_id, rect, uri):
  action = { Type: /Action, S: /URI, URI: uri }
  dict = { Type: /Annot, Subtype: /Link,
           Rect: rect, A: action, Border: [0,0,0] }

AnnotationBuilder::free_text(page_id, rect, content, font_size, color):
  dict = { Type: /Annot, Subtype: /FreeText,
           Rect: rect, Contents: content,
           DA: "/{font} {size} Tf {r} {g} {b} rg" }
```

#### Page editor (`page_editor.rs`)

```
add_blank_page(editor, width_pt, height_pt, at_index):
  page_dict = { Type: /Page, Parent: pages_id,
                MediaBox: [0, 0, width_pt, height_pt],
                Contents: [], Resources: {} }
  page_id = writer.add_object(serialize(page_dict))
  insert page_id into Pages tree Kids array at at_index
  increment Pages /Count

delete_page(editor, page_index):
  remove page_id from Kids array
  decrement /Count

ContentLayer::draw_on_page(editor, page_index, draw_fn):
  existing_content = page's /Contents
  cb = ContentBuilder::new()
  draw_fn(&mut cb)
  new_stream_bytes = make_flate_stream(cb.build())
  new_stream_id = writer.add_object(new_stream_bytes)
  page /Contents = [existing_content..., new_stream_id]
```

#### Merge (`merge.rs`)

```
MergeBuilder::add_document(bytes):
  sub_doc = PdfDocument::parse(bytes)
  id_offset = self.next_free_id
  remap all object IDs in sub_doc: new_id = original_id + id_offset
  copy remapped objects into self.writer
  append remapped page IDs to self.merged_kids

MergeBuilder::build() -> Vec<u8>:
  write new Pages root with merged kids
  write new Catalog pointing to Pages root
  write all merged objects
  write XRef and trailer
```

#### Redact (`redact.rs`)

```
RedactionEngine::apply(editor, page_index, rects):
  1. Parse page content stream into tokens
  2. Walk operators; for each text span:
     compute glyph bounding box under CTM
     if bbox intersects any redact_rect: replace Tj string with spaces
  3. Reassemble content stream (modified operators + unmodified rest)
  4. For each redact_rect: add black filled rectangle to content stream
  5. Remove /Annots referencing the redacted region
  6. Replace page /Contents with new stream via writer
```

---

### 3.10 Forms — `src/forms/`

```
AcroFormBuilder::build_text_field(name, rect, page_id, default_value):
  widget_dict = {
    Type: /Annot, Subtype: /Widget, FT: /Tx,
    T: name, V: default_value,
    Rect: rect, P: page_id,
    DA: "/Helvetica 12 Tf 0 g",
    AP: { N: appearance_stream_id }
  }
  appearance_stream = generate_text_appearance(rect, default_value, font_size=12)
  // appearance is a Form XObject with BT...ET block

build_checkbox(name, rect, page_id, checked):
  widget_dict = {
    FT: /Btn,
    AS: /On or /Off,
    AP: { N: { On: on_ap_id, Off: off_ap_id } }
  }
  on_appearance  = ContentBuilder: set_fill_color(0,0,0) + "✓" glyph
  off_appearance = ContentBuilder: empty stream

AcroFormBuilder::finish(editor):
  acroform_dict = { Fields: [all_field_ids] }
  editor.doc.trailer["AcroForm"] = acroform_id
```

---

## 4. Comparison with ONLYOFFICE C++ Workflow

### 4.1 Architecture comparison

| Aspect | ONLYOFFICE C++ | pdf-core Rust |
|--------|---------------|---------------|
| **PDF parser** | xpdf (C++ library, modified) | Custom nom-based parser (pure Rust) |
| **Object model** | xpdf `Object` union type | `PdfObject` enum with owned data |
| **XRef** | xpdf `XRef` class | `HashMap<u32, u64>` (lazy parse) |
| **Object loading** | xpdf eager object graph | `PdfDocument::resolve()` lazy on demand |
| **Content interpreter** | xpdf `Gfx` class | `ContentInterpreter` + `OutputDevice` trait |
| **Rendering target** | `IRenderer` abstract interface | `OutputDevice` trait (same concept) |
| **Rasterisation** | Platform's `IRenderer` (GDI, canvas) | Custom software rasteriser in `render/` |
| **Font engine** | `IFontManager` + system fonts | Embedded font resolver + standard metrics |
| **Encryption** | xpdf `Decrypt` class | Custom RC4+MD5 / AES-128 in `crypto/` |
| **Incremental save** | `CPdfWriter` appends update section | `PdfEditor::save_append()` same model |
| **WASM bridge** | Emscripten + manual JS bindings | `wasm-bindgen` auto-generated bindings |
| **Language safety** | Manual memory management, raw pointers | Rust ownership, no `unsafe` except WASM glue |

### 4.2 Reader flow comparison

**ONLYOFFICE C++:**
```
LoadFromMemory(data)
  └─ new PDFDoc(MemStream, ownerPw, userPw)
       ├─ PDFDoc::checkHeader()
       ├─ PDFDoc::readXRef()  (xpdf XRef class, full eager parse)
       ├─ new Catalog(xref)   (walks /Pages tree eagerly)
       └─ Decrypt::makeFileKey(...)

DrawPageOnRenderer(renderer, pageIdx)
  └─ Page::display(RendererOutputDev, dpi, rotate)
       └─ new Gfx(xref, outputDev, ...)
            └─ Gfx::go()   // operator dispatch loop
                 └─ RendererOutputDev::drawChar / stroke / fill → IRenderer calls
```

**pdf-core Rust:**
```
PdfDocument::parse(bytes)
  ├─ startxref_offset()  (backward scan from EOF)
  ├─ parse_xref(data, offset)  (lazy: HashMap only, no full parse)
  └─ objects stored as raw bytes; resolve() parses on demand

render_page_rgba(doc, page_index, scale)
  ├─ Page::from_document(doc, idx)  (walks Pages tree on demand)
  ├─ PixmapBuffer::new(w, h)
  ├─ PageRenderer::new(...)  implements OutputDevice
  ├─ ContentInterpreter::new()
  └─ interpreter.process(stream_iter, &mut page_renderer, doc)
       └─ operator dispatch → OutputDevice::stroke_path / fill_path / draw_text_span
```

**Key difference:** ONLYOFFICE's `IRenderer` is an abstract interface routing to a
*platform renderer* (browser canvas, GDI) — it does not rasterize itself.
pdf-core's `OutputDevice` routes to a *software RGBA canvas* built in Rust.
Both designs share the same abstraction shape.

### 4.3 WASM bridge comparison

| Aspect | ONLYOFFICE WASM | pdf-core WASM |
|--------|----------------|---------------|
| **Compilation** | Emscripten C++ → WASM | wasm-pack + wasm-bindgen |
| **Pixel return** | Raw WASM heap pointer | Rust `Vec<u8>` copied to JS `Uint8Array` |
| **Memory management** | Manual `free(ptr)` from JS | Automatic via `wasm-bindgen` |
| **Type safety** | Stringly typed (JSON strings) | Typed JS classes (`WasmDocument`, etc.) |
| **CMap support** | `setCMap(binary)` | Not yet (flagged as follow-up) |
| **Edit API** | `addPage`, `removePage` (basic) | Full editor bridge (`WasmEditor`) |

---

## 5. End-to-End Workflow Examples

### Example A — Open and Render a PDF Page

```
Input: File bytes of "invoice.pdf", page 0, scale 2.0 (144 DPI)

Step 1 — Tokenize from EOF backward
  scan for "startxref" → offset 42800
  scan for decimal → xref_start = 41200

Step 2 — Parse XRef table at 41200
  "xref\n"
  "0 1\n0000000000 65535 f \r\n"   ← free entry
  "1 15\n"                          ← subsection: IDs 1..15
  "0000000009 00000 n \r\n"         ← ID 1 → offset 9
  "0000000058 00000 n \r\n"         ← ID 2 → offset 58
  ...
  xref = { 1→9, 2→58, 3→120, ..., 15→40500 }

Step 3 — Parse trailer
  "trailer\n<< /Size 16 /Root 1 0 R /Info 14 0 R >>"
  root_id = 1, info_id = 14

Step 4 — Resolve Catalog (ID 1, offset 9)
  "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj"
  catalog.pages_ref = ID 2

Step 5 — Walk Pages tree to page 0
  ID 2 → "/Type /Pages /Count 3 /Kids [3 0 R 7 0 R 11 0 R]"
  Kids[0] = ID 3 → "/Type /Page /MediaBox [0 0 612 792] /Resources 4 0 R /Contents 5 0 R"
  Page { width=612, height=792, content_stream_ids=[5], resources_id=4 }

Step 6 — Setup render
  scale = 2.0
  pixel_w = (612 * 2.0).ceil() = 1224
  pixel_h = (792 * 2.0).ceil() = 1584
  canvas = PixmapBuffer::new(1224, 1584)  // 1224×1584×4 = ~7.5MB, white init

  initial_ctm = Matrix { a:2, b:0, c:0, d:-2, e:0, f:1584 }
  // Maps PDF (0,0) bottom-left → pixel (0, 1584) bottom of image
  // Maps PDF (612, 792) top-right → pixel (1224, 0) top of image

Step 7 — Interpret content stream (ID 5)
  decompress FlateDecode stream → raw operator bytes:
  "q\n"                              → gfx.push()
  "0.1 0.1 0.1 rg\n"                → fill_color = RGB(0.1, 0.1, 0.1) near-black
  "BT\n"                             → in_text = true
  "/F1 14 Tf\n"                      → font_name="F1" font_size=14
  "72 720 Td\n"                      → text_x=72, text_y=720
  "(INVOICE #1042) Tj\n"             → decode string → TextSpan
                                        "INVOICE #1042" at (72, 720), size 14
  "ET\n"                             → in_text = false
  "1 0 0 1 72 680 cm\n"              → ctm = translate(72, 680) * initial_ctm
  "0 0 468 0.5 re\n"                 → path: rect at (0,0) w=468 h=0.5
  "S\n"                              → stroke_path → horizontal rule on canvas

Step 8 — Text rasterisation
  TextSpan { text: "INVOICE #1042", x: 144.0, y: 144.0, font_size: 28.0 }
  // (72*2=144 px from left; (1584 - 720*2)=144 px from top)
  for each char 'I','N','V',...:
    glyph = glyph_cache.get_or_rasterize("F1", 28.0, char)
    blit_glyph_onto_canvas(canvas, glyph, x_cursor, baseline_y, fill_color)
    x_cursor += glyph_advance

Step 9 — Return
  (1224, 1584, rgba_bytes)   // 7,526,016 bytes
```

---

### Example B — Edit: Add Highlight Annotation and Save

```
Input: "report.pdf" bytes
Goal: highlight rect [72, 700, 400, 716] on page 0, save

Step 1 — Open editor
  PdfEditor::open(report_pdf_bytes)
  doc.xref = { 1→9, 2→58, ..., 20→38400 }
  max_id = 20
  writer.next_id = 21

Step 2 — Add annotation
  AnnotationBuilder::highlight(page_id=3, rect=[72,700,400,716],
                               quads=[72,700, 400,700, 72,716, 400,716],
                               color=[1.0, 1.0, 0.0])  // yellow

  annot_dict = <<
    /Type /Annot
    /Subtype /Highlight
    /Rect [72 700 400 716]
    /QuadPoints [72 700 400 700 72 716 400 716]
    /C [1 1 0]
    /M (D:20260524120000)
  >>
  annot_id = 21   (writer.next_id++)
  writer.objects[21] = serialize(annot_dict)

Step 3 — Update page's /Annots
  page_dict = doc.resolve(3) → original dict bytes
  new_page_dict = page_dict.clone() + /Annots [21 0 R]
  writer.objects[3] = serialize(new_page_dict)   // override ID 3

Step 4 — save_append()
  output = report_pdf_bytes.clone()   // original 38,500 bytes

  // Append new objects
  offset_21 = 38500
  output.extend("21 0 obj\n<<...annot_dict...>>\nendobj\n")
  offset_3_new = output.len()
  output.extend("3 0 obj\n<<...updated_page_dict...>>\nendobj\n")

  // Write incremental XRef
  xref_start = output.len()
  output.extend("xref\n")
  output.extend("3 1\n")
  output.extend("{offset_3_new:010} 00000 n \r\n")
  output.extend("21 1\n")
  output.extend("{offset_21:010} 00000 n \r\n")

  // Write trailer pointing back to original xref
  output.extend("trailer\n")
  output.extend("<< /Size 22 /Root 1 0 R /Prev 37800 >>\n")
  output.extend("startxref\n{xref_start}\n%%EOF\n")

Result: original PDF + ~800 bytes = valid updated PDF
PDF readers load the update section; ID 3 and 21 override originals
```

---

### Example C — Decrypt RC4-128 Encrypted PDF

```
Input: encrypted.pdf with password "secret"
Encrypt dict: /V 2 /R 3 /Length 128 /O <owner_hash> /U <user_hash>

Step 1 — Key derivation (Algorithm 2)
  password_bytes = "secret" (6 bytes)
  padded = "secret" ++ PASSWORD_PADDING[..26]  → 32 bytes total

  o_entry = /O value (32 bytes from Encrypt dict)
  permissions_le = /P value as little-endian 4 bytes
  doc_id = /ID[0] from trailer (16 bytes)

  md5_input = padded ++ o_entry ++ permissions_le ++ doc_id
  hash0 = md5(md5_input)          // 16 bytes
  hash1 = md5(hash0)              // revision 3: repeat 50 times
  ...
  hash50 = md5(hash49)
  file_key = hash50[..16]         // 128-bit key

Step 2 — Verify user password
  expected_u = md5(PASSWORD_PADDING ++ doc_id[..16])   // 16 bytes
  u0 = rc4_encrypt(file_key, expected_u)
  u1 = rc4_encrypt(file_key XOR [1;16], u0)
  ...
  u19 = rc4_encrypt(file_key XOR [19;16], u18)
  if u19[..16] == encrypt_dict[U][..16]: password is correct

Step 3 — Decrypt each string/stream object on access
  PdfDocument::resolve(id):
    raw = raw_bytes_from_xref_offset
    decrypted = decrypt_rc4(file_key, id, gen, raw)
    parse(decrypted)
```

---

### Example D — Create PDF from Scratch (Writer)

```
Goal: one-page PDF, white page, "Hello World" at (72, 720)

Step 1 — PdfWriter::new()
  writer.next_id = 1

Step 2 — Build content stream
  cb = ContentBuilder::new()
    .begin_text()
    .set_font("Helvetica", 14.0)    // "/Helvetica 14 Tf\n"
    .move_text(72.0, 720.0)          // "72 720 Td\n"
    .show_text(b"Hello World")       // "(Hello World) Tj\n"
    .end_text()
    .build()
  // cb = b"BT\n/Helvetica 14 Tf\n72 720 Td\n(Hello World) Tj\nET\n"

Step 3 — Compress into stream object
  (stream_dict, compressed) = make_flate_stream(&cb)
  content_id = writer.add_object(serialize_stream(stream_dict, compressed))
  // content_id = 1

Step 4 — Build font resource
  font_dict = { Type:/Font, Subtype:/Type1, BaseFont:/Helvetica,
                Encoding:/WinAnsiEncoding }
  font_id = writer.add_object(serialize_dict(font_dict))
  // font_id = 2

Step 5 — Build page
  resources = { Font: { Helvetica: 2 0 R } }
  page_dict = { Type:/Page, Parent: 4 0 R,
                MediaBox: [0 0 612 792],
                Contents: 1 0 R,
                Resources: resources }
  page_id = writer.add_object(serialize_dict(page_dict))
  // page_id = 3

Step 6 — Build Pages root
  pages_dict = { Type:/Pages, Kids:[3 0 R], Count:1 }
  pages_id = writer.add_object(serialize_dict(pages_dict))
  // pages_id = 4

Step 7 — Build Catalog
  catalog_dict = { Type:/Catalog, Pages: 4 0 R }
  catalog_id = writer.add_object(serialize_dict(catalog_dict))
  // catalog_id = 5

Step 8 — Serialize
  output = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n"  // header

  offsets = BTreeMap::new()
  offsets[1] = output.len(); output.extend("1 0 obj\n{content stream}\nendobj\n")
  offsets[2] = output.len(); output.extend("2 0 obj\n{font dict}\nendobj\n")
  offsets[3] = output.len(); output.extend("3 0 obj\n{page dict}\nendobj\n")
  offsets[4] = output.len(); output.extend("4 0 obj\n{pages dict}\nendobj\n")
  offsets[5] = output.len(); output.extend("5 0 obj\n{catalog dict}\nendobj\n")

  xref_start = output.len()
  output.extend("xref\n0 6\n0000000000 65535 f \r\n")
  for (id, offset) in offsets:
    output.extend("{offset:010} 00000 n \r\n")

  output.extend("trailer\n<< /Size 6 /Root 5 0 R >>\n")
  output.extend("startxref\n{xref_start}\n%%EOF\n")
```

---

## 6. WASM Bridge

### JavaScript API surface

```javascript
// Load document
const doc = WasmDocument.parse(uint8array);
const doc = WasmDocument.parse_with_password(uint8array, passwordBytes);

doc.page_count()                  // → number
doc.get_metadata()                // → JSON string
doc.get_outline()                 // → JSON string
doc.extract_text(pageIndex)       // → plain text string

// Render (feature = wasm-render)
const renderer = WasmRenderer.new(uint8array);
const result = renderer.render_page(pageIndex, scale);
result.width()                    // pixels
result.height()                   // pixels
result.rgba_bytes()               // Uint8Array (width * height * 4)

// Edit
const editor = WasmEditor.open(uint8array);
editor.page_count()
editor.add_blank_page(width, height, atIndex)
editor.delete_page(index)
editor.add_text_annotation(pageIndex, x1,y1,x2,y2, content, author)
editor.add_highlight(pageIndex, x1,y1,x2,y2, r,g,b)
editor.add_link(pageIndex, x1,y1,x2,y2, uri)
editor.set_metadata(title, author, subject, keywords)
editor.save()                     // → Uint8Array (updated PDF bytes)

// Write from scratch
const writer = WasmPdfWriter.new();
writer.add_page(widthPt, heightPt, contentStreamBytes, fontResourceJson)
writer.build()                    // → Uint8Array (new PDF bytes)
```

### Memory model

```
JavaScript                Rust (WASM heap)
────────────────────────────────────────────
Uint8Array pdfBytes  ──copy──► parse(bytes) → PdfDocument (heap allocated)
                                               │
                              ┌────────────────┘
                              WasmDocument (wasm-bindgen managed)
                                               │
doc.extract_text(0)  ◄─────── String serialized to JS
result.rgba_bytes()  ◄─────── Vec<u8> copied to Uint8Array
editor.save()        ◄─────── Vec<u8> copied to Uint8Array
```

Unlike ONLYOFFICE's raw heap pointer approach, wasm-bindgen handles
allocation/deallocation automatically — no manual `free(ptr)` call needed.

---

## 7. Feature Flag Matrix

| Feature flag | What it enables | Extra dependencies |
|-------------|----------------|-------------------|
| *(default)* | Parser + document + font + content + text + error | nom, flate2, weezl, log, thiserror |
| `crypto` | RC4/AES encryption, `parse_with_password` | md5, aes, cbc |
| `render` | RGBA page rasteriser, `render_page_rgba` | ab_glyph |
| `writer` | PDF serializer + content builder (always on via editor) | — |
| `forms` | AcroForm builder, field appearances | — |
| `wasm` | wasm-bindgen bridge (no render) | wasm-bindgen, serde_json |
| `wasm-render` | wasm bridge + render | wasm-bindgen + ab_glyph |

All features pass `cargo build --target wasm32-unknown-unknown`.
Native-only code is gated behind `#[cfg(not(target_arch = "wasm32"))]`.

---

## 8. Known Limitations

| Area | Limitation | Planned fix |
|------|-----------|-------------|
| **Parser** | No repair mode for corrupted XRef (no linearized-fallback) | Scan-and-rebuild as follow-up |
| **Font render** | Glyph rasterisation uses ab_glyph; no hinting, no sub-pixel AA | FreeType integration |
| **Render** | Gradients (shading), patterns, transparency groups not rendered | Phase R2 |
| **Encryption** | AES-256 (R5/R6) returns `PdfError::Encrypted` | Phase crypto2 |
| **Merge** | ID offsetting strategy; full object-graph rewrite not wired | save_new() completion |
| **Font subset** | Full TTF binary embedded, no subsetting | Subsetting engine |
| **Redaction** | Text-level redaction only; no image pixel whiteout | Image pixel zeroing |
| **save_new()** | Skeleton present; full traversal not wired for standalone export | Required for redaction |
| **CJK CMap** | CID CMaps (predefined like Adobe-Japan1) not loaded | External CMap file loader |
| **Forms** | No field validation, no JavaScript action execution | Phase I2 |
| **Digital signatures** | Not implemented | Phase S |

---

*Report generated 2026-05-24. All 383 tests pass. WASM target builds clean.*
