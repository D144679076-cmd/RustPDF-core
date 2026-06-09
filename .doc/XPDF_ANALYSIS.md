# xpdf 4.06 — Deep Technical Analysis

> Source: `/home/duy/Documents/Workspace/work/pdfEditor/xpdf-4.06/`
> Purpose: Reference study for the Rust reimplementation. Do NOT copy code — study behavior only. See REBUILD_PLAN.md §L1.

---

## 1. Overall Architecture

### High-Level Call Graph: PDF File → Pixels on Screen

```
main()
  └─ PDFDoc::PDFDoc(fileName)
      ├─ FileStream (raw PDF file stream)
      ├─ XRef::XRef() — Parse xref table & trailer
      │   ├─ getStartXref()       — Find 'startxref' keyword
      │   ├─ readXRef()           — Read xref table/stream
      │   ├─ readXRefStream()     — Handle compressed xrefs (PDF 1.5+)
      │   └─ constructXRef()      — Rebuild xref from object markers (recovery)
      ├─ Catalog::Catalog()       — Parse document catalog
      │   ├─ readPageTree()       — Walk Pages tree structure
      │   └─ getPage(pageNum)     — Retrieve page object (lazy-loaded)
      │
      └─ PDFDoc::displayPage(outputDev, pageNum)
          └─ Page::display()
              └─ Gfx::Gfx() + Gfx::display()
                  ├─ Parser::getObj()           — Parse content stream
                  │   ├─ Lexer::getObj()        — Tokenize
                  │   └─ [Operator dispatch table]
                  │
                  ├─ [Graphics state: q/Q/cm/w/J/j/M/d/gs]
                  ├─ [Color: cs/CS/sc/SC/scn/SCN/g/G/rg/RG/k/K]
                  ├─ [Path: m/l/c/v/y/h/re/S/s/f/F/B/n/W]
                  ├─ [Text: BT/ET/Td/TD/T*/Tf/Tj/TJ/Tm]
                  │   └─ GfxFont::getNextChar() — Decode char codes → Unicode
                  │
                  ├─ [XObject: Do]
                  │   ├─ Form XObject → recursively interpret
                  │   └─ Image XObject → rasterize
                  │
                  └─ OutputDev method calls:
                      ├─ stroke() / fill() / eoFill()
                      ├─ drawChar() / drawString()
                      ├─ drawImage() / drawImageMask()
                      └─ beginTransparencyGroup() / endTransparencyGroup()
                          └─ SplashOutputDev — rasterize to bitmap
```

### Component Ownership

| Component | Owns | Depends on |
|-----------|------|-----------|
| **PDFDoc** | XRef, Catalog, BaseStream | XRef, Catalog, Page, Gfx, OutputDev |
| **XRef** | Object entries, encryption key, object cache | Parser, Lexer, Decrypt, Object |
| **Catalog** | Page array (lazy), outlines, dests, forms | Page, XRef |
| **Page** | PageAttrs, contents ref, annots ref | Gfx |
| **Gfx** | Graphics state stack, resource stack, parser | GfxState, GfxFont, OutputDev, Stream |
| **GfxState** | CTM, colors, font, line params | GfxFont, GfxColorSpace |
| **OutputDev** (abstract) | — | GfxState, GfxFont, Stream |
| **SplashOutputDev** | Bitmap, font engine, glyph cache | Splash, SplashFont, GfxFont |
| **TextOutputDev** | Char list, word list, line list | — |
| **Stream** hierarchy | Filter chain | Object, Decrypt |

---

## 2. File-by-File Breakdown

### `Object.h` — PDF variant type
- `ObjType` enum: 14 types (Bool, Int, Real, String, Name, Null, Array, Dict, Stream, Ref, Cmd, Error, EOF, None)
- `Object` union wrapping all PDF types
- `Ref` struct: `{int num, int gen}`
- Key methods: `fetch(xref)` — dereference indirect ref; `arrayGet()`, `dictLookup()` — containers; `streamGetChar()`, `streamGetBlock()` — stream I/O

### `XRef.h` / `XRef.cc` — Object table
- `XRefEntry {offset, gen, type}` — points to object in file
- `XRefEntryType`: free, uncompressed, compressed (object stream)
- `XRefCacheEntry` — LRU cache of 16 recently fetched objects
- Key methods:
  - `fetch(num, gen, obj)` — main workhorse: look up, seek, decrypt, parse
  - `constructXRef()` — recovery: linear scan the file for `N G obj` patterns
  - `setEncryption()`, `okToPrint()`, `okToCopy()` — permissions
- Key data: `XRefEntry *entries`, `Object trailerDict`, `Guchar fileKey[32]`, `ObjectStream *objStrs[128]` (LRU cache)

### `Lexer.h` / `Lexer.cc` — Tokenizer
- Tokenizes raw PDF bytes into discrete objects
- Input: one or more streams (content can span multiple streams per page)
- `getObj(obj)` → next token
- Token types: integer, real, string `(...)` or `<hex>`, name `/...`, operator, `[`, `]`, `<<`, `>>`
- Adobe quirk: `--123` → 0, `50-100` → 50 (lenient number parsing)
- Comments: `%` to end of line → ignored

### `Parser.h` / `Parser.cc` — Object builder
- Builds composite objects from Lexer tokens
- `getObj(obj, simpleOnly, fileKey, encAlgorithm, keyLength, objNum, objGen)`
  - Arrays: accumulate until `]`
  - Dicts: accumulate key-value pairs until `>>`
  - Streams: dict + `stream` keyword + seek `endstream`
  - Inline images: special BI/ID/EI state machine (binary data between ID and EI)
- Decrypts string content on-the-fly when `fileKey` is provided
- 2-token lookahead buffer

### `PDFDoc.h` / `PDFDoc.cc` — Top-level API
- Main entry point for applications
- `displayPage(outputDev, page, hDPI, vDPI, rotate, useMediaBox, crop, printing)`
- `isEncrypted()`, `isDamaged()`, `isLinearized()`
- Owns: XRef, Catalog, Outline

### `Catalog.h` / `Catalog.cc` — Document structure
- Lazy page loading: `pages[]` array, only populated on `getPage(i)`
- `readPageTree()` — recursive walk of Pages tree
- Exposes: dests, outlines, AcroForm, OCProperties, Metadata, StructTreeRoot

### `Page.h` / `Page.cc` — Single page
- `PDFRectangle {x1, y1, x2, y2}` for all box types
- `PageAttrs` — inherited attributes (MediaBox, CropBox, Resources, Rotate)
- `getContents(obj)` — content stream(s) (can be array)
- `display()` — creates Gfx interpreter, runs content stream

### `Stream.h` / `Stream.cc` — Filter chain

```
Stream (abstract)
  ├─ BaseStream
  │   ├─ FileStream      — buffered file I/O
  │   ├─ MemStream       — in-memory buffer
  │   └─ EmbedStream     — for inline images
  │
  └─ FilterStream (wraps another stream)
      ├─ FlateStream     — zlib deflate
      ├─ LZWStream       — LZW decompression
      ├─ CCITTFaxStream  — Group 3/4 fax
      ├─ DCTStream       — JPEG (libjpeg or built-in)
      ├─ JBIG2Stream     — JBIG2 binary images
      ├─ JPXStream       — JPEG 2000
      ├─ ASCII85Stream   — Base85 encoding
      ├─ ASCIIHexStream  — Hex encoding
      ├─ RunLengthStream — RLE
      ├─ DecryptStream   — AES/RC4 on-the-fly decryption
      └─ StreamPredictor — PNG predictor reversal (for images)
```

Filter chain: `addFilters(dict)` wraps each filter around the previous.
Data flows: `RawBytes → [Decrypt] → Filter[0] → Filter[1] → ... → plaintext`

**Decompression bomb protection**: Flate/LZW check if output > 200x input AND output > 50MB. Disabled for images (bounds known).

### `Gfx.h` / `Gfx.cc` — Content stream interpreter
- Static `Operator opTab[]` — sorted table, binary-searched by name
- `execOp()` — dispatches to method (e.g., `opMoveTo`, `opShowText`)
- 84+ operators handled
- Error limit: 500 errors per content stream before giving up
- Loop detection: `checkForContentStreamLoop()` — tracks object refs already on stack
- BX/EX: ignore unknown operators between them

### `GfxState.h` / `GfxState.cc` — Graphics parameters
- `double ctm[6]` — current transformation matrix
- `GfxColorSpace` hierarchy: DeviceGray/RGB/CMYK, CalGray/CalRGB, Lab, ICCBased, Indexed, Separation, DeviceN, Pattern
- `GfxBlendMode` — 16 blend modes (Normal, Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, ColorBurn, HardLight, SoftLight, Difference, Exclusion, Hue, Saturation, Color, Luminosity)
- `GfxColor {GfxColorComp c[32]}` — fixed-point 16.16 values
- Text state: `font`, `fontSize`, `textMatrix[6]`, `lineMatrix[6]`, `charSpace`, `wordSpace`, `horizScaling`, `leading`, `textRender`, `rise`
- Stack linked list via `GfxState *saved`

### `GfxFont.h` / `GfxFont.cc` — Font objects
- `GfxFontType` enum: Type1, Type1C, Type3, TrueType, OpenType (TTF), OpenType (CFF), CIDType0, CIDType2
- `Gfx8BitFont` — 8-bit char codes, 256-entry encoding
- `GfxCIDFont` — 16-bit CID codes, uses CMap
- `getNextChar(s, len, &code, &u, uSize, &uLen, &dx, &dy, &ox, &oy)` — **critical**: returns bytes consumed, Unicode, glyph advance

### `OutputDev.h` — Abstract renderer interface
- Virtual methods: `startPage()`, `endPage()`, `stroke()`, `fill()`, `eoFill()`, `clip()`, `drawChar()`, `drawImage()`, `beginTransparencyGroup()`, `endTransparencyGroup()`, `setSoftMask()`
- `upsideDown()` — coordinate system direction
- `useDrawChar()` — individual glyph rendering vs. `drawString()`

### `SplashOutputDev.h` / `SplashOutputDev.cc` — Software rasterizer
- Color modes: Mono1, Mono8, RGB8, BGR8, CMYK8
- `SplashBitmap *bitmap` — output pixel buffer
- `SplashFontEngine *fontEngine` — font rasterization cache
- `T3FontCache *t3FontCache[8]` — Type 3 glyph cache
- `GList *transpGroupStack` — transparency group offscreen buffers

### `TextOutputDev.h` / `TextOutputDev.cc` — Text extractor
- `TextChar` — individual glyph with position, font, color, bounding box
- `TextWord` — logical word group
- `TextLine`, `TextBlock`, `TextPage` — layout hierarchy
- Layout modes: reading order, physical, table, simple, line-printer, raw
- `drawChar()` accumulates; `endPage()` runs layout analysis

### `Decrypt.h` / `Decrypt.cc` — Encryption
- RC4 40-bit, RC4 128-bit, AES-128 CBC, AES-256 CBC (R5/R6)
- Key derivation: MD5(padding + fileID + permissions + ownerKey)
- Per-object key: MD5(fileKey + objNum:3LE + objGen:2LE)
- `DecryptStream` — filter stream wrapping encrypted content

---

## 3. PDF Parsing Pipeline (Detailed)

```
1. HEADER SCAN
   - Open file, read first 1024 bytes
   - Search for "%PDF-" → extract version string

2. TRAILER SEARCH
   - Seek near end of file (last 1024 bytes)
   - Search backwards for "startxref" keyword
   - Read integer on next line → offset of XRef

3. XREF TABLE PARSING (XRef constructor)
   readXRef(offset):
     peek byte at offset:
       'x' → readXRefTable()
           parse lines: "objNum gen offset f|n"
           store in entries[objNum]
       digit → readXRefStream()
           parse as stream object with {Size, Root, Prev, ...}
           decompress, extract (offset, gen) pairs
     
     extract Root, Encrypt, ID from trailer dict
     if /Prev in trailer → recursively read previous XRef
   
   if XRef parse fails → constructXRef() [RECOVERY]:
     linear scan entire file for "N G obj" patterns
     build entries[] from found positions
     create synthetic trailer dict

4. OBJECT FETCHING: XRef::fetch(num, gen, obj)
   check LRU cache[16] → hit: return
   look up entries[num]:
     type=uncompressed → seek to offset, parse "N G obj ... endobj"
     type=compressed   → load ObjectStream, parse object at index
   if encrypted → wrap strings in DecryptStream
   store in cache, return

5. OBJECT STREAM (PDF 1.5+ compressed objects)
   stream dict: {Type /ObjStm, N count, First firstOffset, ...}
   contents: N pairs of (objNum, offset) then N object bodies
   ObjectStream decompresses once, caches all N objects
   XRef cache: objStrs[128] LRU

6. PARSER STATE MACHINE: Parser::getObj()
   simple:
     integer, real, boolean, null
     string "(hello)" with escapes or "<hex>"
     name "/FontName"
     indirect ref "3 0 R"
   complex:
     array "[1 2 3]"
     dict "<<key val>>"
     stream "<<dict>> stream DATA endstream"
   special:
     inline image: BI dict ID <binary> EI
       → special lexer state machine (not normal PDF syntax)

7. STREAM FILTER APPLICATION: addFilters(dict)
   for each filter in /Filter array:
     get DecodeParms[i]
     wrap previous stream: FlateStream, LZWStream, etc.
   if encrypted: insert DecryptStream before other filters

8. ENCRYPTION SETUP
   XRef detects /Encrypt dict
   Decrypt::makeFileKey() → fileKey[32] via MD5 + RC4/AES
   stored in xref->fileKey
   per-object: MD5(fileKey + objNum:3LE + objGen:2LE)
```

---

## 4. Content Stream Interpreter — All Operators

### Graphics State

| Op | Args | Action |
|----|------|--------|
| `q` | — | Push graphics state |
| `Q` | — | Pop graphics state |
| `cm` | a b c d e f | Concatenate CTM |
| `w` | width | Line width |
| `J` | cap | Line cap: 0=Butt 1=Round 2=Square |
| `j` | join | Line join: 0=Miter 1=Round 2=Bevel |
| `M` | limit | Miter limit |
| `d` | array phase | Dash pattern |
| `ri` | intent | Rendering intent |
| `i` | flatness | Flatness tolerance |
| `gs` | name | Load ExtGState from Resources |

### Color

| Op | Args | Action |
|----|------|--------|
| `cs` / `CS` | name | Set fill/stroke colorspace |
| `sc` / `SC` | c... | Set fill/stroke color |
| `scn` / `SCN` | c... \| name | sc/SC + pattern/separation |
| `g` / `G` | gray | Fill/stroke gray |
| `rg` / `RG` | r g b | Fill/stroke RGB |
| `k` / `K` | c m y k | Fill/stroke CMYK |

### Path Construction

| Op | Args | Action |
|----|------|--------|
| `m` | x y | Move to |
| `l` | x y | Line to |
| `c` | x1 y1 x2 y2 x3 y3 | Cubic Bezier |
| `v` | x2 y2 x3 y3 | Cubic, control1 = current point |
| `y` | x1 y1 x3 y3 | Cubic, control2 = endpoint |
| `h` | — | Close subpath |
| `re` | x y w h | Rectangle |

### Path Painting

| Op | Args | Action |
|----|------|--------|
| `S` | — | Stroke |
| `s` | — | Close + stroke |
| `f` / `F` | — | Fill (nonzero winding) |
| `f*` | — | Fill (even-odd) |
| `B` | — | Fill + stroke (nonzero) |
| `B*` | — | Fill + stroke (even-odd) |
| `b` | — | Close + fill + stroke (nonzero) |
| `b*` | — | Close + fill + stroke (even-odd) |
| `n` | — | End path (no paint) |
| `W` | — | Clip (nonzero) |
| `W*` | — | Clip (even-odd) |

### Text

| Op | Args | Action |
|----|------|--------|
| `BT` | — | Begin text object |
| `ET` | — | End text object |
| `Tf` | font size | Set font + size |
| `Tm` | a b c d e f | Set text matrix (absolute) |
| `Td` | tx ty | Move text position (relative) |
| `TD` | tx ty | Move + set leading = -ty |
| `T*` | — | Next line (by leading) |
| `TL` | leading | Set leading |
| `Tc` | spacing | Character spacing |
| `Tw` | spacing | Word spacing |
| `Tz` | scale | Horizontal scaling (%) |
| `Tr` | mode | Text render mode 0-7 |
| `Ts` | rise | Text rise |
| `Tj` | string | Show string |
| `'` | string | Next line + show |
| `"` | aw ac string | Set spacing + show |
| `TJ` | array | Show with kerning offsets |

### Text Render Modes

| Mode | Fill | Stroke | Clip |
|------|------|--------|------|
| 0 | ✓ | — | — |
| 1 | — | ✓ | — |
| 2 | ✓ | ✓ | — |
| 3 | — | — | — |
| 4 | ✓ | — | ✓ |
| 5 | — | ✓ | ✓ |
| 6 | ✓ | ✓ | ✓ |
| 7 | — | — | ✓ |

### Other

| Op | Args | Action |
|----|------|--------|
| `Do` | name | Draw XObject (Form or Image) |
| `BI` / `ID` / `EI` | — | Inline image |
| `sh` | name | Shading fill |
| `BMC` / `BDC` | tag [props] | Begin marked content |
| `EMC` | — | End marked content |
| `MP` / `DP` | tag [props] | Marked content point |
| `d0` / `d1` | — | Type 3 font glyph metrics |
| `BX` / `EX` | — | Compatibility section |

---

## 5. Font System

### Glyph → Unicode Pipeline

```
Content stream char code
  ↓
GfxFont::getNextChar(s, len, &code, &u, uSize, &uLen, &dx, &dy)
  │
  ├─ 1. ToUnicode CMap  (highest priority)
  │      PDF CMap stream embedded in font dict
  │      maps char codes → Unicode sequences
  │
  ├─ 2. Encoding array  (8-bit fonts)
  │      256-entry name array
  │      glyph name → Adobe Glyph List → Unicode
  │
  ├─ 3. CMap (CID fonts)
  │      char codes → CIDs → Unicode via ToUnicode
  │
  └─ 4. Fallback
         use char code as Unicode directly
         (last resort, produces garbage for symbol fonts)
```

### Font File Resolution

```
GfxFont::locateFont():
  1. Check /FontFile, /FontFile2, /FontFile3 → embedded stream
  2. Check external font directory (config)
  3. Match base-14 font by name
  4. Substitute: serif → Times, sans → Helvetica, mono → Courier
  5. Return NULL → use bitmap substitute
```

### Standard 14 Fonts
- Times-Roman, Times-Bold, Times-Italic, Times-BoldItalic
- Helvetica, Helvetica-Bold, Helvetica-Oblique, Helvetica-BoldOblique
- Courier, Courier-Bold, Courier-Oblique, Courier-BoldOblique
- Symbol, ZapfDingbats

### TJ Operator — Kerning
```
TJ [ "Hello" -100 " " 200 "World" ]
     ^string   ^kern ^sp  ^kern   ^string
kern is in 1/1000 of font size units
negative = move right (tighten), positive = move left (loosen)
```

---

## 6. Rendering Pipeline

### Text Rendering (SplashOutputDev)
```
Gfx::opShowText("Hello")
  for each char code in string:
    font->getNextChar() → code, Unicode, (dx, dy)
    outputDev->drawChar(x, y, dx, dy, code, u, uLen)
      └─ SplashOutputDev::drawChar()
          ├─ GfxFont → glyph ID
          ├─ SplashFontEngine::getFont(font, size, matrix)
          ├─ SplashFont::getGlyph(GID) → bitmap (cached)
          └─ Splash::fillGlyph(x, y, bitmap, color, alpha)
    text_x += dx * fontSize * horizScaling
```

### Path Rendering
```
Gfx::opStroke()
  SplashOutputDev::stroke(state)
    ├─ Extract current path (accumulated m/l/c/h ops)
    ├─ SplashPath *spath = buildSplashPath(state->path)
    └─ Splash::stroke(spath)
        ├─ Flatten curves → line segments
        ├─ Apply dash pattern
        ├─ Apply cap/join
        ├─ Rasterize edges (antialiased if enabled)
        └─ Fill scan lines
```

### Transparency Groups
```
Form XObject with /Group /Transparency:
  OutputDev::beginTransparencyGroup()
    → SplashOutputDev: save parent bitmap, create offscreen bitmap
  
  Gfx::display(form stream) on offscreen
  
  OutputDev::endTransparencyGroup()
    → SplashOutputDev: composite offscreen → parent
                       using blend mode + alpha
```

### Image Rendering
```
Gfx::drawImageMask / drawImage
  └─ SplashOutputDev::drawImage(stream, width, height, colorMap)
      ├─ ImageStream: iterate pixel lines from stream
      ├─ Color conversion (indices → RGB/CMYK/gray)
      ├─ Scale to destination rect via CTM
      ├─ Filter: bilinear if /Interpolate true, else nearest
      └─ Composite onto bitmap
```

---

## 7. Encryption

### Algorithm Matrix

| V | R | Algorithm | Key bits |
|---|---|-----------|----------|
| 1-4 | 2 | RC4 | 40 |
| 1-4 | 3 | RC4 | 128 |
| 4 | 4 | AES-128 CBC | 128 |
| 5 | 5 | AES-256 CBC | 256 |
| 5 | 6 | AES-256 CBC + SHA-256/512 | 256 |

### Key Derivation (R2/R3/R4)
```
fileKey = MD5(
  28-byte standard padding (truncated/padded userPassword to 32 bytes)
  + O entry from Encrypt dict (32 bytes)
  + P (permissions, 4 bytes LE)
  + first FileID (16 bytes)
  + [if R>=4: if EncryptMetadata=false, add 0xFFFFFFFF]
)
truncate to keyLength bytes
if R >= 3: MD5 the result 50 more times
```

### Per-Object Key
```
objKey = MD5(fileKey + objNum[3 bytes LE] + objGen[2 bytes LE])
         use first min(keyLength + 5, 16) bytes
```

---

## 8. Real-World Recovery Heuristics

These are behaviors xpdf implements for broken PDFs that violate ISO 32000.
All must be reimplemented independently in Rust.

### H1 — XRef Reconstruction (`XRef::constructXRef`)
**Trigger**: XRef table corrupt, missing, or offsets wrong  
**Action**: Scan entire file byte-by-byte for `"N G obj"` patterns, rebuild entries[] from found positions, create synthetic trailer  
**Source file**: `xpdf/XRef.cc` → `constructXRef()`

### H2 — Stream Length Recovery (`Parser.cc`)
**Trigger**: Declared `/Length` in stream dict is wrong (too small or too large)  
**Action**: After reading declared length, check for `endstream` keyword. If not found, scan forward. Use actual position of `endstream`.  
**Source file**: `xpdf/Parser.cc` → `getStream()`

### H3 — Truncated Stream Graceful EOF (`FlateStream`, `LZWStream`)
**Trigger**: Stream data ends before decompression is complete  
**Action**: Return EOF gracefully; caller gets partial data. Page renders partial content.  
**Source file**: `xpdf/Stream.cc`

### H4 — Decompression Bomb Protection (`FlateStream`, `LZWStream`)
**Trigger**: Output size > 200x input size AND output > 50MB  
**Action**: Abort decompression, return error  
**Source file**: `xpdf/Stream.cc` → `checkForDecompressionBomb()`

### H5 — Circular Reference Detection (`Gfx::checkForContentStreamLoop`)
**Trigger**: Form XObject A references Form XObject B which references A  
**Action**: Track object refs in `contentStreamStack`. If current ref already in stack → skip, log error  
**Source file**: `xpdf/Gfx.cc`

### H6 — Recursion Depth Limit (`Parser::getObj`)
**Trigger**: Deeply nested arrays/dicts or circular references in object graph  
**Action**: `objectRecursionLimit = 500` — at limit, return `objError`  
**Source file**: `xpdf/Parser.cc`

### H7 — Missing Mandatory Fields — Defaults
**Trigger**: Font dict missing `/Encoding`, page missing `/Resources`, etc.  
**Action**: Use sensible defaults (StandardEncoding, empty Resources dict)  
**Source file**: `xpdf/GfxFont.cc`, `xpdf/Page.cc`

### H8 — Duplicate Object IDs
**Trigger**: Two objects with same ID in file  
**Action**: Last definition in file wins (last `constructXRef` entry overwrites earlier)  
**Source file**: `xpdf/XRef.cc`

### H9 — Wrong/Missing `endobj`
**Trigger**: Generator omits `endobj` or places next object immediately after  
**Action**: Parser infers boundary from start of next `N G obj` pattern  
**Source file**: `xpdf/Parser.cc`

### H10 — Corrupt Inline Image (`BI/ID/EI`)
**Trigger**: Binary data between `ID` and `EI` contains bytes that look like PDF syntax  
**Action**: Lexer state machine: after `ID`, read bytes until true `EI` context (not just `EI` substring)  
**Source file**: `xpdf/Lexer.cc`, `xpdf/Parser.cc`

### H11 — Bad Array Sizes in ObjectStream
**Trigger**: `/N` in ObjStm says -1 or unreasonably large (> 1,000,000)  
**Action**: Reject, return error, skip object stream  
**Source file**: `xpdf/XRef.cc` → `ObjectStream`

### H12 — Empty Password Encrypted PDFs
**Trigger**: PDF is encrypted but owner+user passwords are empty  
**Action**: `makeFileKey()` automatically tries empty string. Most "protected" PDFs open without a password prompt.  
**Source file**: `xpdf/Decrypt.cc`

### H13 — Linearized PDF Detection
**Trigger**: PDF has XRef at beginning + linearization hints  
**Action**: Check first 1024 bytes for `/Linearized` dict. If present, use hint tables. Always also support non-linearized fallback.  
**Source file**: `xpdf/PDFDoc.cc`

---

## 9. Key Data Structures (for Rust translation reference)

### GfxState (graphics state snapshot)
```
ctm: [f64; 6]              // current transformation matrix
fill_color_space: ColorSpace
stroke_color_space: ColorSpace
fill_color: Color          // up to 32 components (fixed-point)
stroke_color: Color
fill_alpha: f32
stroke_alpha: f32
blend_mode: BlendMode      // 16 variants
line_width: f64
line_cap: LineCap          // Butt, Round, Square
line_join: LineJoin        // Miter, Round, Bevel
miter_limit: f64
line_dash: Vec<f64>
line_dash_phase: f64
flatness: f64
font: Arc<Font>
font_size: f64
text_matrix: [f64; 6]     // Tm
line_matrix: [f64; 6]     // current line matrix
char_space: f64
word_space: f64
horiz_scaling: f64
leading: f64
text_render: u8            // 0-7
rise: f64
saved: Option<Box<GfxState>>  // stack via linked list
```

### XRefEntry
```
offset: u64     // byte offset in file (or object stream number if compressed)
gen: u16        // generation number
kind: XRefKind  // Free, Uncompressed, Compressed { stream_num, index }
```

### Object (variant)
```
enum PdfObject {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    String(Vec<u8>),
    Name(String),
    Array(Vec<PdfObject>),
    Dict(IndexMap<String, PdfObject>),
    Stream { dict: Dict, offset: u64 },
    Ref { num: u32, gen: u16 },
    Cmd(String),   // operator name (used in content streams)
}
```

### TextWord
```
x_min, x_max, y_min, y_max: f64  // bounding box
text: Vec<char>                    // Unicode characters
edge: Vec<f64>                     // per-char x coordinate
font: FontInfo
font_size: f64
rotation: u16                      // 0, 90, 180, 270
color: (f64, f64, f64)             // RGB
space_after: bool
invisible: bool
```

---

## 10. Architecture Insights for Rust Rewrite

### Lazy Loading
Everything in xpdf is on-demand:
- Pages: only loaded when `getPage(i)` called
- Objects: only fetched when referenced (LRU cache of 16)
- Object streams: decompressed once, cached (LRU of 128)
- Fonts: only loaded when first used in content stream

→ **Rust**: use `HashMap<u32, PdfObject>` as object cache; `Option<Box<Page>>` in catalog array; Arc for shared font cache

### Stream Design
The filter chain in C++ uses virtual dispatch (each filter wraps the previous).

→ **Rust**: use `Box<dyn Read>` chain or a `FilterChain` enum. Either works. Enum is faster (no heap indirection per byte).

### OutputDev Pattern
The rendering backend is fully decoupled from the interpreter via an abstract interface.

→ **Rust**: define a `Renderer` trait with the same virtual methods. `SplashRenderer` (tiny-skia) and `TextRenderer` implement it. Gfx takes `&mut dyn Renderer`.

### Graphics State Stack
xpdf: linked list of `GfxState` via `GfxState *saved`.

→ **Rust**: `Vec<GraphicsState>` — push on `q`, pop on `Q`. Much simpler.

### Operator Dispatch
xpdf: sorted static array + binary search by name.

→ **Rust**: `match` on `&str` — compiler generates optimal jump table. No binary search needed.

### Error Recovery Priority
Based on xpdf study, implement in this order:
1. XRef reconstruction (H1) — needed immediately; most malformed PDFs hit this
2. Stream length recovery (H2) — common; PDFs from scanners often have wrong lengths
3. Truncated stream graceful EOF (H3) — easy, just handle `UnexpectedEof`
4. Recursion limits (H6) — safety net, add from day one
5. Missing mandatory fields (H7) — add as you implement each subsystem
6. Decompression bombs (H4) — add to flate decoder
7. Circular reference detection (H5) — add when Form XObjects are implemented
8. Rest: add as bugs are reported from real PDFs
