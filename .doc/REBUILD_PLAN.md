# PDF Editor Rust Core — Rebuild Plan

## Context

This is a **full commercial rebuild** — not a reskin of ONLYOFFICE. The goal is
an entirely original PDF editor product:

- **Rust core library** (`pdf-core`) — replaces both xpdf and ONLYOFFICE's
  C++ PdfFile layer. Pure Rust, compiles to native + WebAssembly.
- **Original web editor** — React/Vite frontend, completely original UI design.
  No ONLYOFFICE code, icons, or visual style.
- **Original native editor** — Rust desktop app (egui/eframe), completely
  original design.

**Why full rebuild (not reuse ONLYOFFICE code):**
- ONLYOFFICE is AGPL v3 — any commercial closed-source product cannot use it
- xpdf is GPL v2 — same restriction
- Visual design, icons, and UI assets are ONLYOFFICE's separate copyright
- Commercial use requires 100% original implementation

**Reference source** (`core/PdfFile/`) is used only to understand **what
features to build and how the PDF spec should behave** — not as a source of
copied code. All Rust implementations must be written from the
**PDF specification (ISO 32000-1/32000-2)** directly.

**Current Rust project:**
`/home/duy/Documents/Workspace/work/pdfEditor/pdf-editor-rust-core/`
Phase 1 complete: lexer + XRef parser (1752 lines, 35 tests, zero compile errors).

**Immediate goal:** runnable demo — open a real PDF, render a page, extract
text, zero compile errors.

---

## 1. Problems Inventory (Technical Debt & Hard Parts)

### 1.1 Object Model Complexity
- PDF objects can be **indirect references** (`5 0 R`) that must be resolved by
  seeking to an offset from the XRef table — this requires bidirectional access
  between the parser and the XRef map.
- **Object streams** (PDF 1.5+, compressed indirect objects inside a stream)
  require two-level decompression: inflate the stream, then parse objects from
  the inner byte sequence.
- The current XRef parser returns a `HashMap<u32, u64>` but doesn't yet build
  the full object table including generation numbers or free-list.

### 1.2 Recursive / Self-Referential Structures
- PDF dictionaries can reference other dictionaries indirectly, and the page
  tree (`/Kids`) is a tree of indirect references.
- Rust's ownership model forbids naive cyclic graphs. Solution: use arena
  allocation (`typed-arena` or `id`-based indexing) instead of `Rc<RefCell<>>`.

### 1.3 Stream Filters (Stacked)
- A single stream may have `/Filter [/FlateDecode /LZWDecode /ASCII85Decode]` —
  filters must be applied in order.
- Supported filters needed for demo: `FlateDecode`, `DCTDecode` (JPEG),
  `ASCII85Decode`, `ASCIIHexDecode`.
- `JPXDecode` (JPEG2000) is complex — defer to post-demo phase.

### 1.4 Content Stream Interpreter (~50+ Operators)
The original uses xpdf's content interpreter which covers **every operator in the
PDF 1.7 specification at production grade**. Our Rust implementation must reach
full parity eventually. We implement incrementally: unknown operators log a
warning and skip — they never panic.

**Full production operator set (target parity with xpdf):**

| Category | Operators |
|----------|-----------|
| Text | `BT ET Tj TJ ' " Tc Tw Tz TL Tr Ts Tf Td TD Tm T*` |
| Path construction | `m l c v y h re` |
| Path painting | `S s F f f* B B* b b* n` |
| Clipping | `W W*` |
| Graphics state | `q Q cm w J j M d ri i gs` |
| Color | `CS cs SC SCN sc scn G g RG rg K k` |
| Shading pattern | `sh` |
| Inline image | `BI ID EI` |
| XObjects | `Do` |
| Marked content | `MP DP BMC BDC EMC` |
| Type 3 fonts | `d0 d1` |
| Compatibility | `BX EX` |

**Demo phase minimum** (Phase 4 — enough for common PDFs to render correctly):
- Text: `BT`, `ET`, `Tj`, `TJ`, `Tf`, `Td`, `TD`, `Tm`, `T*`, `'`, `"`
- Graphics state: `q`, `Q`, `cm`, `w`, `j`, `J`, `d`, `ri`, `gs`
- Path: `m`, `l`, `c`, `v`, `y`, `h`, `re`, `S`, `s`, `f`, `f*`, `B`, `n`
- Color: `g`, `G`, `rg`, `RG`, `k`, `K`, `sc`, `SC`
- Image: `Do`, `BI`/`ID`/`EI`
- Clipping: `W`, `W*`

**Post-demo additions** (Phase 4b): `sh`, `d0/d1`, `BMC/BDC/EMC`, `SCN/scn`
(DeviceN/Pattern color spaces), `BX/EX`.

**Intentional non-goals** (match original's limits):
- JavaScript (`/JS` actions) — security risk, never implement
- 3D annotations (U3D/PRC) — out of scope
- Rich media (Flash/video) — out of scope
- PDF 2.0 new operators — defer until xpdf itself adds them

### 1.5 Font Handling
- **Standard 14 fonts**: must be embedded as fallback data (already in C++
  `Resources/Fonts*.h`). For Rust, embed as `include_bytes!()`.
- **TrueType/OTF subsetting**: required for writer. For reader/renderer use
  `fontdue` (pure Rust, no system deps, WASM-safe).
- **Type1/CFF**: `allsorts` crate handles parsing; rendering via `fontdue` after
  converting outlines.
- **CID / CJK fonts**: complex — ToUnicode CMap and predefined CMaps needed.
  Embed essential Adobe-Japan/Korea/GB CMaps as byte arrays.
- **glyph-to-Unicode mapping**: required for text extraction. Priority: ToUnicode
  CMap → Encoding → standard Encoding fallback.

### 1.6 Encryption
- PDF 1.1-1.6: RC4 (40-bit, 128-bit), AES-128. PDF 1.7: AES-256.
- No pure-Rust `rc4` in widely used crates — use `rc4` crate (MIT) or
  implement the 12-line algorithm inline.
- `aes` + `cbc` crates from the `RustCrypto` family (pure Rust, WASM-safe).
- Encryption is needed to read protected PDFs but can be deferred past demo.

### 1.7 Digital Signatures
- Requires PKCS#7 / CMS parsing → `cms` or `rasn` crates.
- X.509 certificate validation → `x509-cert` + `webpki`.
- Complex, defer past demo entirely.

### 1.8 Rendering Without C++ Freetype
- Original: `splash/` (xpdf's own rasterizer) + freetype.
- Rust alternatives: `tiny-skia` (2D rasterizer, pure Rust, WASM-safe) as the
  canvas; `fontdue` for font rasterization.
- `tiny-skia` supports paths, gradients, patterns, blending, transforms — covers
  the graphics model needed.

### 1.9 WASM Constraints
- No `std::fs` — all I/O must be via `&[u8]` passed from JS.
- No system threads by default — avoid `rayon`, use `wasm-bindgen-futures` for
  async.
- No system fonts — all fonts must be embedded or passed from the host.
- `wasm-bindgen` for JS interop; `js-sys` / `web-sys` for Canvas API.
- Binary size matters: strip debug, use `wasm-opt`, feature-gate heavy paths.

### 1.10 JPEG2000 (JPX)
- Pure-Rust JPEG2000 is immature. Options: link `openjpeg` via FFI, or use
  `jpeg2000` crate (thin wrapper). Defer until post-demo.

### 1.11 Linearized PDFs
- The XRef table may be at a non-end offset; must follow the linearization
  hint dict. Current XRef parser assumes classic end-of-file `startxref`.
- Needs: detect `/Linearized` dict at file start, fall back to end XRef.

### 1.12 Large File Memory Management
- For native: `memmap2` crate for zero-copy file access.
- For WASM: all data in `Vec<u8>` passed from JS — no mmap.
- Design: abstract over `&[u8]` so both paths work.

---

## 2. Recommended Rust Crates

| Subsystem | Crate | Reason |
|-----------|-------|--------|
| Parsing | `nom 7` | Already in use, composable, zero-alloc happy path |
| Error | `thiserror 1` | Already in use |
| Compression | `flate2 1` | Already in use (FlateDecode) |
| LZW | `weezl` | Pure Rust LZW/GIF decoder |
| JPEG | `jpeg-decoder` | Pure Rust, WASM-safe |
| Font parsing/render | `fontdue` | Pure Rust, no system deps, WASM-safe |
| Font shaping | `allsorts` | CFF/OTF shaping, pure Rust |
| 2D rendering | `tiny-skia` | Pure Rust rasterizer, WASM-safe |
| AES | `aes` + `cbc` (RustCrypto) | Pure Rust, WASM-safe |
| RC4 | `rc4` (RustCrypto) | Pure Rust |
| SHA/MD5 | `sha2`, `md-5` (RustCrypto) | Pure Rust |
| WASM bindings | `wasm-bindgen`, `js-sys`, `web-sys` | Standard |
| Async WASM | `wasm-bindgen-futures` | For async tasks in WASM |
| Native file I/O | `memmap2` | Zero-copy large file access |
| PNG output | `png` | Pure Rust encoder |
| Arena alloc | `typed-arena` | Safe arena for PDF object graph |
| Logging | `log` + `env_logger` / `console_log` | Unified logging native+WASM |
| Native GUI | `eframe` + `egui` | Pure Rust, cross-platform, WASM-safe |
| Native file dialog | `rfd` | Pure Rust, no GTK/Qt deps |

---

## 3. Module Structure (Target Architecture)

```
src/
├── lib.rs                  # Public API, feature flags
├── error.rs                # PdfError (already done)
├── parser/
│   ├── mod.rs
│   ├── lexer.rs            # DONE — tokenizer
│   ├── xref.rs             # DONE — XRef table/stream
│   ├── objects.rs          # TODO — indirect object resolution, full object model
│   └── filters.rs          # TODO — FlateDecode, DCT, ASCII85, ASCIIHex
├── document/
│   ├── mod.rs
│   ├── catalog.rs          # TODO — /Catalog, /Pages tree walker
│   ├── page.rs             # TODO — /Page dict, media box, resources
│   ├── metadata.rs         # TODO — /Info, XMP
│   └── outline.rs          # TODO — /Outlines (bookmarks)
├── content/
│   ├── mod.rs
│   ├── interpreter.rs      # TODO — content stream operator dispatch
│   ├── graphics_state.rs   # TODO — CTM, fill/stroke colors, line props
│   └── text_state.rs       # TODO — font, size, Tm, leading
├── fonts/
│   ├── mod.rs
│   ├── standard.rs         # TODO — embed 14 standard fonts
│   ├── truetype.rs         # TODO — TrueType/OTF via fontdue
│   ├── type1.rs            # TODO — Type1/CFF via allsorts
│   ├── cmap.rs             # TODO — ToUnicode, predefined CMaps
│   └── encoding.rs         # TODO — WinAnsi, MacRoman, PDFDoc encodings
├── render/
│   ├── mod.rs
│   ├── canvas.rs           # TODO — tiny-skia wrapper
│   └── image.rs            # TODO — XObject/inline image decoding
├── writer/
│   ├── mod.rs
│   ├── document.rs         # TODO — new PDF document builder
│   ├── page.rs             # TODO — page content stream builder
│   ├── objects.rs          # TODO — object serialization
│   └── xref_writer.rs      # TODO — XRef table/stream output
├── security/
│   ├── mod.rs
│   ├── encryption.rs       # TODO — RC4/AES decrypt
│   └── permissions.rs      # TODO — permission flags
├── wasm/
│   ├── mod.rs              # TODO — wasm-bindgen exports
│   └── api.rs              # TODO — JS-facing functions
└── bin/
    ├── demo.rs             # TODO — CLI demo (text extract + PNG render)
    └── viewer.rs           # TODO — native GUI viewer (egui)
```

---

## 4. Phased Implementation Plan

### Phase 1 — Lexer + XRef Parser ✅ DONE
- Lexer: 1103 lines, 30 tests
- XRef: 564 lines, 5 tests
- Error types with byte-offset context

### Phase 2 — Object Model (Prerequisite for Everything)
**Files**: `src/parser/objects.rs`, `src/parser/filters.rs`

**Deliverables**:
- `PdfObject` enum: `Null, Boolean, Integer, Real, String, Name, Array, Dict, Stream, Reference`
- `PdfDocument` struct holding raw bytes + XRef map
- `fn resolve(&self, obj: &PdfObject) -> Result<&PdfObject>` — follow indirect refs
- `fn get_stream_data(&self, obj_id: u32) -> Result<Vec<u8>>` — decode stream filters
- Filter pipeline: FlateDecode, ASCII85Decode, ASCIIHexDecode, LZWDecode
- Object stream support (compressed objects, PDF 1.5+)
- Tests: parse a real minimal PDF bytes, resolve its catalog

**Acceptance**: `cargo test` passes; can open PDF bytes and get the trailer dict.

### Phase 3 — Document Structure
**Files**: `src/document/`

**Deliverables**:
- Parse `/Catalog` → `/Pages` tree → individual `/Page` dicts
- `Page` struct: media box, crop box, resources dict (fonts, xobjects, colorspaces)
- `fn page_count(&self) -> usize`
- `fn get_page(&self, idx: usize) -> Result<Page>`
- Metadata extraction from `/Info` dict
- Tests: page count matches known PDF, media box dimensions correct

**Acceptance**: `cargo test` passes; can list all pages of a multi-page PDF.

### Phase 4 — Content Stream Interpreter
**Files**: `src/content/`

**Deliverables**:
- `GraphicsState` struct: CTM matrix, fill/stroke color, line width, dash, alpha
- `TextState` struct: font ref, font size, Tm matrix, leading, spacing
- Operator dispatch table (match on `&str` operator name)
- Text extraction: collect `TextSpan { text: String, x: f32, y: f32, size: f32 }`
- Path recording: collect `PathOp` for later rasterization
- Graceful unknown-operator skip with `log::warn!`
- Tests: extract text from a known PDF page

**Acceptance**: Text extracted from a real PDF page matches expected content.

### Phase 5 — Font System (Minimum for Demo)
**Files**: `src/fonts/`

**Deliverables**:
- Embed the 14 standard PDF fonts as `&[u8]` via `include_bytes!`
- `FontCache` that loads and caches fontdue `Font` structs
- `glyph_to_unicode(font: &PdfFont, gid: u16) -> Option<char>` using ToUnicode CMap
- Fallback: /Encoding dict → predefined encoding tables
- For rendering: `rasterize_glyph(font, gid, size_px) -> GlyphBitmap`
- Tests: rasterize a letter from a standard font at 12pt

**Acceptance**: Can rasterize text from a PDF page to pixels.

### Phase 6 — Rendering Engine
**Files**: `src/render/`

**Deliverables**:
- `Canvas` wrapping `tiny_skia::Pixmap`
- Execute path ops: `move_to`, `line_to`, `curve_to`, `close`, `fill`, `stroke`
- Draw glyphs at correct positions using fontdue output + tiny-skia blitting
- Handle CTM (current transformation matrix)
- Handle fill/stroke color (DeviceRGB, DeviceGray, DeviceCMYK)
- Inline image and XObject image rendering via JPEG/PNG decoders
- Tests: render page 0 of a test PDF to PNG, compare checksum

**Acceptance**: Produces a visually correct PNG from a real PDF page.

### Phase 7 — CLI Demo Binary
**Files**: `src/bin/demo.rs`

**Deliverables**:
```
pdf-core-demo <input.pdf> [--page N] [--output out.png] [--text]
```
- `--text`: print extracted text to stdout
- `--output out.png`: render page to PNG file
- Default: page 0, output `page.png`
- Returns exit code 0 on success, 1 on error

**Acceptance**: `cargo run --bin demo -- test.pdf --text` prints text.
`cargo run --bin demo -- test.pdf --output out.png` produces a viewable PNG.

### Phase 8 — Minimal PDF Writer
**Files**: `src/writer/`

**Deliverables**:
- `PdfBuilder` that can create a new 1-page PDF with text
- Serialize objects with cross-reference table
- `save_to_vec() -> Vec<u8>` and `save_to_file(path)` (native only)
- Embed standard fonts by reference (no subsetting in demo)
- Tests: round-trip — write PDF, read it back, verify page count = 1

**Acceptance**: `pdfinfo` (system tool) confirms generated PDF is valid.

### Phase 9 — WASM Bindings
**Files**: `src/wasm/`

**Deliverables**:
- `#[wasm_bindgen]` exports:
  - `PdfDocument::from_bytes(data: &[u8]) -> Result<PdfDocument, JsValue>`
  - `PdfDocument::page_count(&self) -> usize`
  - `PdfDocument::render_page(idx: usize, scale: f32) -> Uint8Array` (PNG bytes)
  - `PdfDocument::extract_text(idx: usize) -> String`
- `pkg/` output directory with `.wasm` + `.js` + `.d.ts`
  (built via `wasm-pack build --target web`)

**Acceptance**: `cargo build --target wasm32-unknown-unknown --features wasm` succeeds.

### Phase 10 — Web Editor (Original Frontend — Tech Stack TBD)
**Location**: separate repo or `apps/web/` at the workspace root

**Stack**: Decided after core is working. Candidates: React + Vite, Vue 3,
Svelte. All are compatible with wasm-bindgen output. 100% original design,
zero ONLYOFFICE code or assets.

**WASM integration contract** (TypeScript):
```typescript
import init, { PdfDocument } from '../pkg/pdf_core';
await init();
const doc = PdfDocument.from_bytes(uint8array);
const count = doc.page_count();
const png = doc.render_page(pageIndex, scale);  // Uint8Array (PNG)
const text = doc.extract_text(pageIndex);        // string
```

**Build flow**:
```bash
wasm-pack build pdf-editor-rust-core --target web --out-dir apps/web/src/pkg
cd apps/web && npm install && npm run dev
```

**UI features** (original design, no ONLYOFFICE assets):
- Drag-and-drop or click-to-open PDF
- Scrollable page view with canvas rendering
- Zoom slider (50%–200%)
- Page navigation (prev/next + page number input)
- Text extraction panel
- Dark/light mode

**Acceptance**: Drop a PDF → page renders in canvas → text panel shows content.

### Phase 11 — Native Editor Demo (Desktop GUI)
**Files**: `src/bin/viewer.rs`

**Tech stack**: `eframe` + `egui` — pure Rust, cross-platform, no GTK/Qt deps.

**UI layout**:
```
┌──────────────────────────────────────────────┐
│ [Open File]  [←] [→]  Page 1/N  [−] 100% [+]│
├────────────┬─────────────────────────────────┤
│ Thumbnails │   Page Canvas (rendered PNG)     │
│ (scrollable│   via egui::Image widget         │
│  strip)    │                                  │
└────────────┴─────────────────────────────────┘
```

**Cargo additions**:
```toml
[features]
native-viewer = ["dep:eframe", "dep:rfd", "render"]

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
eframe = { version = "0.27", optional = true }
rfd = { version = "0.14", optional = true }

[[bin]]
name = "viewer"
path = "src/bin/viewer.rs"
required-features = ["native-viewer"]
```

**Acceptance**: `cargo run --bin viewer --features native-viewer` opens a window.
Click "Open File", pick a PDF, page 0 renders. Zoom in/out works. No panic.

---

## 5. IP & Legal Rules (Commercial Product — Non-Negotiable)

### L1 — Two-Source Reference Policy

We use **both** ISO 32000 and xpdf as references, with different roles:

| Reference | Used for | How to use |
|-----------|----------|-----------|
| **ISO 32000-1/32000-2** | Spec-compliant behavior, data structures, operator semantics | Primary implementation source. Cite section numbers in code comments. |
| **xpdf source** (`core/PdfFile/lib/xpdf/`) | Real-world edge cases, recovery heuristics, broken PDF handling, behaviors not covered by ISO | **Study only** — read to understand WHAT problem exists, then implement independently in Rust. Never copy code. |

**The legal line for xpdf:**
- ✅ Read xpdf to discover that broken PDFs need XRef recovery → implement your own recovery in Rust
- ✅ Study xpdf's behavior on a malformed PDF → write a test case, implement the fix from scratch
- ✅ Note that xpdf scans forward for `endstream` when length is wrong → write your own scanner
- ❌ Copy xpdf functions, algorithms, or data structures verbatim into Rust
- ❌ Translate xpdf C++ to Rust line-by-line
- ❌ Reproduce xpdf's internal class hierarchy or variable names

This is **reference study** — the same approach used by poppler, MuPDF, and PDFium.
All of these learned from xpdf behavior without copying its GPL code.

### L2 — No Copying ONLYOFFICE Visual Design
All UI components, icons, color schemes, layouts, and visual assets must be
completely original. Do not replicate ONLYOFFICE's toolbar layout, icon set,
or branding. Use a UI library like shadcn/ui or Radix UI with a custom theme.

### L3 — Only Use Permissive-Licensed Dependencies
All Rust crates and npm packages must be MIT, Apache-2.0, BSD, or ISC licensed.
No GPL or LGPL dependencies in the commercial build. Audit with:
- Rust: `cargo deny check licenses`
- JS: `license-checker --onlyAllow 'MIT;Apache-2.0;BSD-2-Clause;BSD-3-Clause;ISC'`

### L4 — Cite Sources in Code Comments
Every non-obvious implementation must cite its source:
- Spec behavior: `// ISO 32000-1 §7.3.4.2 — indirect object syntax`
- xpdf-informed recovery: `// real-world: some generators write wrong stream length, scan for endstream`
- Edge case: `// observed in PDFs from Word 2007: missing endobj before next object`

---

## 6. Development Rules (Mandatory for All Future Work)

### R1 — No Panic in Library Code
All public functions must return `Result<T, PdfError>`. Never use `unwrap()` or
`expect()` in `src/` except in tests (`#[cfg(test)]`). Use `?` operator.

### R2 — WASM-Safe by Default
Every new dependency must be verified WASM-compatible before adding to
`Cargo.toml`. Run `cargo build --target wasm32-unknown-unknown` after every
Cargo.toml change. Isolate native-only code behind
`#[cfg(not(target_arch = "wasm32"))]`.

### R3 — Feature Flags for Heavy Subsystems
```toml
[features]
default = ["reader"]
reader  = []
render  = ["reader", "dep:fontdue", "dep:tiny-skia", "dep:png"]
writer  = ["reader"]
crypto  = ["dep:aes", "dep:cbc", "dep:rc4"]
wasm    = ["dep:wasm-bindgen", "dep:js-sys", "dep:web-sys"]
native-viewer = ["render", "dep:eframe", "dep:rfd"]
```

### R4 — One Module = One Concern
No file longer than 800 lines. Split at 600 lines. Each `mod.rs` only
declares submodules and re-exports the public API.

### R5 — Test-Driven for Parsers
Every `fn parse_*` must have at least one happy-path test with crafted bytes
and one error-path test. Use `assert_eq!` with `pretty_assertions`.

### R6 — Real PDF Integration Tests
Maintain `tests/fixtures/` with at least 3 real PDFs:
- `minimal.pdf` — 1-page, no compression, no encryption
- `multipage.pdf` — 5+ pages, mixed content
- `encrypted.pdf` — user-password protected (for crypto phase)

### R7 — Byte-Offset Errors Always
Every `PdfError` variant must carry `offset: usize`. When wrapping external
errors, record the offset at the call site.

### R8 — No `unsafe` Without Comment
All `unsafe` blocks need a `// SAFETY:` comment. Minimize unsafe to mmap
alignment and FFI boundaries.

### R9 — Logging Not Printing
Use `log::warn!` / `log::debug!` in library code. Never `println!`.

### R10 — Semantic Versioning for Public API
Every breaking API change bumps the minor version (pre-1.0) with a
`CHANGELOG.md` entry.

---

## 7. Critical Files to Create (Prioritized)

| Priority | File | Phase |
|----------|------|-------|
| 1 | `src/parser/objects.rs` | 2 |
| 2 | `src/parser/filters.rs` | 2 |
| 3 | `src/document/catalog.rs` | 3 |
| 4 | `src/document/page.rs` | 3 |
| 5 | `src/content/interpreter.rs` | 4 |
| 6 | `src/content/graphics_state.rs` | 4 |
| 7 | `src/content/text_state.rs` | 4 |
| 8 | `src/fonts/standard.rs` | 5 |
| 9 | `src/fonts/cmap.rs` | 5 |
| 10 | `src/render/canvas.rs` | 6 |
| 11 | `src/bin/demo.rs` | 7 |
| 12 | `src/wasm/api.rs` | 9 |
| 13 | `src/bin/viewer.rs` | 11 |

---

## 8. Cargo.toml Changes Needed

```toml
[features]
default = ["reader"]
reader = []
render = ["reader", "dep:fontdue", "dep:tiny-skia", "dep:png"]
writer = ["reader"]
crypto = ["dep:aes", "dep:cbc", "dep:rc4", "dep:md-5", "dep:sha2"]
wasm = ["dep:wasm-bindgen", "dep:js-sys"]
native-viewer = ["render", "dep:eframe", "dep:rfd"]

[dependencies]
nom = "7"
thiserror = "1"
flate2 = "1.0"
log = "0.4"
weezl = "0.1"

fontdue = { version = "0.7", optional = true }
tiny-skia = { version = "0.11", optional = true }
png = { version = "0.17", optional = true }
jpeg-decoder = { version = "0.3", optional = true }

aes = { version = "0.8", optional = true }
cbc = { version = "0.1", optional = true }
rc4 = { version = "0.1", optional = true }
md-5 = { version = "0.10", optional = true }
sha2 = { version = "0.10", optional = true }

wasm-bindgen = { version = "0.2", optional = true }
js-sys = { version = "0.3", optional = true }

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
eframe = { version = "0.27", optional = true }
rfd = { version = "0.14", optional = true }

[[bin]]
name = "demo"
path = "src/bin/demo.rs"
required-features = ["render"]

[[bin]]
name = "viewer"
path = "src/bin/viewer.rs"
required-features = ["native-viewer"]
```

---

## 9. Verification Plan (Demo Acceptance Criteria)

```bash
# 1. Zero compile errors across all features
cargo build --features "reader,render,writer"
cargo build --target wasm32-unknown-unknown --features "reader,wasm"
cargo build --bin viewer --features native-viewer

# 2. All tests pass
cargo test

# 3. CLI demo — text extraction
cargo run --bin demo --features render -- tests/fixtures/minimal.pdf --text

# 4. CLI demo — PNG render
cargo run --bin demo --features render -- tests/fixtures/minimal.pdf --output /tmp/out.png
file /tmp/out.png   # must say "PNG image data"

# 5. Native viewer
cargo run --bin viewer --features native-viewer
# manual: open a PDF, verify page renders, zoom works

# 6. WASM + web demo
wasm-pack build --target web --out-dir examples/web/pkg --features wasm
cd examples/web && python3 -m http.server 8080
# manual: http://localhost:8080, drop PDF, verify canvas render

# 7. No clippy errors
cargo clippy --features "reader,render,native-viewer" -- -D warnings

# 8. License audit (before any commercial release)
cargo install cargo-deny
cargo deny check licenses
```

---

## 10. Known Risks & Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Complex linearized PDFs fail XRef | Medium | Detect `/Linearized`, add secondary scan |
| fontdue missing glyph → blank text | High | Fall back to glyph bounding box rectangle |
| Stack overflow on recursive PDF structure | Low | Use iterative tree walking, not recursion |
| WASM binary >5MB | Medium | Feature flags + `wasm-opt` + strip debug |
| CJK text garbled | High (if CJK PDFs tested) | Embed minimal CMap tables, warn on miss |
| Encrypted PDF panic | Medium | Return `Err(PdfError::Encrypted)` cleanly |
| Content stream unknown operator | High | Log warn, continue — never panic |
| egui texture upload slow for large pages | Medium | Pre-scale PNG to viewport size before upload |
| CORS blocks WASM load from `file://` | High | Always serve via HTTP, never `file://` |
| GPL dep accidentally introduced | Medium | `cargo deny check licenses` in CI |

---

## 11. xpdf-Informed Real-World Behaviors to Implement

Full technical analysis of xpdf 4.06 internals is in **[XPDF_ANALYSIS.md](XPDF_ANALYSIS.md)**.
That file covers: complete call graph, all operators, font pipeline, rendering pipeline,
encryption, data structures, and all recovery heuristics with exact xpdf source file references.

These behaviors are NOT in ISO 32000 but required for real-world PDFs.
All Rust implementations must be written independently (see Rule L1).

### H1 — XRef Reconstruction (`xpdf/XRef.cc → constructXRef`)
When XRef table is corrupt or offsets wrong:
- Scan entire file for `"N G obj"` patterns
- Rebuild entries[] from found positions
- Create synthetic trailer dict
- Implementation priority: **Phase 2** (critical — most malformed PDFs hit this)

### H2 — Stream Length Recovery (`xpdf/Parser.cc`)
When declared `/Length` is wrong:
- After reading declared length, check for `endstream`
- If not found at expected offset, scan forward
- Use actual position of `endstream` as true length
- Implementation priority: **Phase 2**

### H3 — Truncated Stream Graceful EOF (`xpdf/Stream.cc`)
- Decompressor returns EOF, caller gets partial data
- Page renders partial content, no crash
- Implementation: handle `UnexpectedEof` from flate2 gracefully
- Priority: **Phase 2** (easy — just propagate partial result)

### H4 — Decompression Bomb Protection (`xpdf/Stream.cc`)
- Abort if output > 200x input size AND output > 50MB
- Applies to Flate and LZW; disabled for images (bounds known)
- Priority: **Phase 2** (safety, add to flate decoder from day one)

### H5 — Circular Reference / Loop Detection (`xpdf/Gfx.cc`)
- Track content stream object refs in a stack
- If current ref already in stack → skip, log error
- Also: `objectRecursionLimit = 500` in Parser
- Priority: **Phase 4** (when Form XObjects are implemented)

### H6 — Missing Mandatory Fields — Defaults
- Font dict missing `/Encoding` → use StandardEncoding
- Page missing `/Resources` → use empty Resources dict
- Encrypt dict missing keys → use empty password
- Priority: **add as each subsystem is implemented**

### H7 — Duplicate Object IDs
- Last definition in file wins (matches Acrobat behavior)
- During linear scan recovery, later offset overwrites earlier
- Priority: **Phase 2** (handle in constructXRef)

### H8 — Empty Password Encrypted PDFs
- Automatically try empty string for owner + user password
- Most "view-only protected" PDFs open without prompt
- Priority: **Phase 8** (encryption phase)

### H9 — Linearized PDF Detection (`xpdf/PDFDoc.cc`)
- Check first 1024 bytes for `/Linearized` dict
- If linearized: use hint tables for fast random access
- Fallback: always read end-of-file `startxref` (works for both)
- Priority: **Phase 3** (document structure phase)

### Recovery Implementation Order (based on xpdf study)
1. H1 XRef reconstruction — Phase 2
2. H2 Stream length recovery — Phase 2
3. H3 Truncated stream EOF — Phase 2
4. H4 Decompression bombs — Phase 2
5. H6 Missing fields — throughout
6. H7 Duplicate IDs — Phase 2
7. H5 Circular references — Phase 4
8. H8 Empty passwords — Phase 8
9. H9 Linearized PDFs — Phase 3

---

## 12. Reference Material (Study Only — Do NOT Copy Code)

| File | What to study (behavior, not code) |
|------|-------------------------------------|
| `core/PdfFile/PdfFile.h` | Full feature list: read/write/edit API surface |
| `core/PdfFile/PdfReader.h` | What metadata/structure a reader must expose |
| `core/PdfFile/PdfWriter.h` | What a writer must be capable of |
| `core/PdfFile/PdfEditor.h` | Page operations: merge, split, annotate, redact |
| `sdkjs/pdf/src/file.js` | What operations the JS layer needs from the engine |

**Primary reference**: ISO 32000-1 (PDF 1.7 spec) — the authoritative source
for all implementation decisions. Cite section numbers in code comments.
