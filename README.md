# RustPDF-core

Rust + WASM PDF engine — the open-source core powering the [pdf-engine](https://github.com/D144679076-cmd/pdf-engine) commercial editor.

## What's inside

| Module | Purpose |
|--------|---------|
| `parser/` | PDF tokenization, object parsing, cross-reference tables |
| `document/` | Catalog, pages, metadata, outlines |
| `content/` | Content stream parsing (text, graphics, images) |
| `fonts/` | TrueType, CFF, Type1, CID font handling |
| `render/` | Rasterization via tiny-skia (feature: `render`) |
| `crypto/` | AES-256, RC4, digital signatures (feature: `crypto`) |
| `editor/` | Text and object editing (feature: `writer`) |
| `forms/` | PDF form fields (feature: `forms`) |
| `wasm/` | WebAssembly FFI bridge (feature: `wasm`) |

## Build

```bash
# Native
cargo build

# WASM
cargo build --target wasm32-unknown-unknown --features wasm,render

# Full WASM SDK (outputs to ../packages/sdk/pkg)
make wasm-sdk
```

## Features

| Flag | Enables |
|------|---------|
| `render` | PDF rasterization |
| `writer` | PDF modification and saving |
| `crypto` | Encryption / decryption |
| `forms` | Form field support |
| `signatures` | Digital signature verification |
| `wasm` | WebAssembly bindings |
| `wasm-render` | WASM + render |
| `wasm-viewer` | WASM read-only viewer |

## License

[Elastic License 2.0](LICENSE) — free to use and modify; cannot be offered as a competing commercial product or managed service.
