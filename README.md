# RustPDF-core

A fast, WASM-compatible PDF engine written in Rust. Powers the [pdf-engine](https://github.com/D144679076-cmd/pdf-engine) commercial editor.

[![License: ELv2](https://img.shields.io/badge/License-Elastic_v2-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![WASM](https://img.shields.io/badge/target-wasm32-green.svg)](https://webassembly.org)

---

## What It Does

RustPDF-core is a pure-Rust PDF library that parses, renders, and edits PDF documents. It compiles to both native binaries and WebAssembly, making it suitable for server-side processing and browser-based editors alike.

---

## Features

| Feature | Description |
|---------|-------------|
| **Parsing** | Full PDF 1.0–2.0 object model, cross-reference tables, object streams |
| **Rendering** | Rasterize pages to RGBA via [tiny-skia](https://github.com/RazrFalcon/tiny-skia) |
| **Text Extraction** | Span-level extraction with positions, fonts, and encoding |
| **Text Editing** | In-place text modification with style preservation |
| **Encryption** | AES-256, RC4, PDF user/owner password support |
| **Forms** | AcroForm field reading and filling, FDF/XFDF import |
| **Signatures** | Digital signature verification (CMS/PKCS#7) |
| **Writing** | Incremental and full PDF serialization |
| **WASM Bridge** | First-class `wasm-bindgen` bindings for browser use |

---

## Architecture

```
pdf-core/
├── parser/       # Tokenizer, object parser, xref, filters (Flate, LZW, DCT…)
├── document/     # Catalog, pages, metadata, outlines, name trees
├── content/      # Content stream interpreter (graphics state, text state)
├── fonts/        # TrueType, CFF, Type1, CID fonts, CMap, encoding
├── text/         # Text extraction, layout, search
├── render/       # Page rasterizer (tiny-skia), glyph cache, tile rendering
├── editor/       # Text edit engine, session management, write-back
├── writer/       # PDF serializer, font subsetting, stream compression
├── crypto/       # AES-256, RC4, key derivation
├── forms/        # AcroForm, FDF/XFDF, field appearance
├── signatures/   # CMS verifier, signer
├── wasm/         # wasm-bindgen FFI layer
└── license/      # License key validation, watermarking
```

---

## Quick Start

### Native (Rust)

Add to `Cargo.toml`:

```toml
[dependencies]
pdf-core = { git = "https://github.com/D144679076-cmd/RustPDF-core.git", features = ["render"] }
```

Parse and render a page:

```rust
use pdf_core::{Document, render::PageRenderer};

let data = std::fs::read("document.pdf")?;
let doc = Document::from_bytes(&data)?;
let page = doc.page(0)?;

let renderer = PageRenderer::new();
let image = renderer.render_page_rgba(&page, 150.0)?; // 150 DPI
// image.data() → Vec<u8> RGBA pixels
```

Extract text:

```rust
use pdf_core::{Document, text::TextExtractor};

let doc = Document::from_bytes(&data)?;
let spans = TextExtractor::extract_page(&doc, 0)?;
for span in spans {
    println!("{} @ ({}, {})", span.text, span.x, span.y);
}
```

### WebAssembly

Build with [wasm-pack](https://rustwasm.github.io/wasm-pack/):

```bash
wasm-pack build --target web --features wasm,render
```

Use in JavaScript/TypeScript:

```typescript
import init, { WasmDocument } from './pkg/pdf_core.js';

await init();

const bytes = new Uint8Array(await file.arrayBuffer());
const doc = WasmDocument.from_bytes(bytes);

console.log(`Pages: ${doc.page_count()}`);

// Render page 0 at 150 DPI → RGBA Uint8Array
const result = doc.render_page(0, 150.0);
const pixels = result.rgba_bytes();
```

---

## Feature Flags

Enable only what you need — keeps binary size minimal, especially for WASM:

```toml
[dependencies]
pdf-core = { ..., features = ["render", "crypto", "forms"] }
```

| Flag | Enables | Adds |
|------|---------|------|
| *(none)* | Parse + text extraction | minimal |
| `render` | Page rasterization | tiny-skia, fontdue |
| `writer` | PDF serialization + editing | — |
| `crypto` | Encryption/decryption | AES, SHA, RSA crates |
| `forms` | AcroForm + FDF/XFDF | requires `writer` |
| `signatures` | Digital signature verify | x509-cert, rsa |
| `wasm` | wasm-bindgen FFI | wasm-bindgen, js-sys |
| `wasm-render` | WASM + render (full SDK) | all of the above |
| `wasm-viewer` | WASM read-only viewer | parse + render, no editor |

---

## Building

### Prerequisites

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32-unknown-unknown

# wasm-pack (for WASM builds)
cargo install wasm-pack
```

### Native

```bash
cargo build --release
cargo test
```

### WASM

```bash
# Full SDK (render + edit)
wasm-pack build --target web --features wasm-render

# Read-only viewer (smaller)
wasm-pack build --target web --features wasm-viewer --out-dir pkg-viewer
```

### Verify WASM compatibility

```bash
cargo build --target wasm32-unknown-unknown --features wasm,render
```

---

## Development

### Code Rules

- All public functions return `Result<T, PdfError>` — no panics
- Every `unsafe` block requires a `// SAFETY:` comment
- No `println!` in library code — use `log::debug!` / `log::warn!`
- No file over 800 lines — split at 600
- Every parser function needs happy-path + error-path tests

### Run checks before committing

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --target wasm32-unknown-unknown
```

### Commit style

```
feat(parser): add object stream decoding
fix(render): correct glyph advance for CID fonts
```

Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`

---

## Error Handling

All errors carry a byte offset for precise diagnostics:

```rust
pub enum PdfError {
    Parse { message: String, offset: usize },
    Encryption { message: String, offset: usize },
    Render { message: String, offset: usize },
    // …
}
```

---

## Integration Tests

Real PDF fixtures live in `tests/fixtures/`:

| File | Purpose |
|------|---------|
| `minimal.pdf` | Smallest valid PDF |
| `multipage.pdf` | Multi-page layout |
| `encrypted_aes256.pdf` | AES-256 encrypted |
| `form.pdf` | AcroForm fields |

Run:

```bash
cargo test --all-features
```

---

## Commercial Use

This project is licensed under the **Elastic License 2.0**. You may use, modify, and distribute it freely. You may **not** offer it as a managed service or build a competing commercial product on top of it without a separate commercial license.

For commercial licensing inquiries, visit [pdf-engine](https://github.com/D144679076-cmd/pdf-engine).

---

## Related

- [pdf-engine](https://github.com/D144679076-cmd/pdf-engine) — Full commercial PDF editor built on this core (closed source)
