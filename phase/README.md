# Implementation Phase Files

Each file is a self-contained implementation plan for Claude Sonnet to execute.
Files contain exact function signatures, algorithms, code snippets, and tests.

## Rules for All Phases

1. Read the phase file completely before writing any code.
2. Read all referenced existing files before modifying them.
3. Run `cargo fmt && cargo clippy -- -D warnings && cargo test` after each task.
4. Run `cargo build --target wasm32-unknown-unknown --features wasm` after any change that touches WASM-exposed code or Cargo.toml.
5. Write a `.doc/<scope>-<YYYY-MM-DD>.md` implementation report after completing each phase file.
6. Never use `unwrap()` or `expect()` outside `#[cfg(test)]`.
7. Every public function must have a `///` doc comment.
8. **Update the phase file status** — when a phase is complete, change its `**Status:**` line to `Complete — YYYY-MM-DD (see .doc/<report>.md)` and update the table in this README if the status column exists.

---

## Phase 1 — Demo Sprint (~1 month)

| File | Feature | Effort |
|------|---------|--------|
| [phase1-aes256-decryption.md](phase1-aes256-decryption.md) | Fix AES-256 encrypted PDF opening | 2 days |
| [phase1-full-text-search.md](phase1-full-text-search.md) | Ctrl+F in-document search | 4–5 days |
| [phase1-form-filling.md](phase1-form-filling.md) | Read + fill AcroForm fields | 2 weeks |
| [phase1-annotation-flatten.md](phase1-annotation-flatten.md) | Burn annotations into page content | 4–5 days |
| [phase1-page-split.md](phase1-page-split.md) | Extract pages into new PDF | 4–5 days |
| [phase1-licensing.md](phase1-licensing.md) | Subscription tiers + trial watermark | 5 days |

---

## Phase 2 — Production v1 (~6 months)

| File | Feature | Effort |
|------|---------|--------|
| [phase2-missing-annotation-types.md](phase2-missing-annotation-types.md) | Stamp, Polygon, Polyline, FileAttachment, Caret + full AP streams | 3 weeks |
| [phase2-digital-signatures.md](phase2-digital-signatures.md) | PKCS#7 / PAdES sign + verify | 3–4 months |
| [phase2-fdf-xfdf.md](phase2-fdf-xfdf.md) | FDF/XFDF import + export | 3 weeks |
| [phase2-optional-content-groups.md](phase2-optional-content-groups.md) | PDF layers show/hide | 4 weeks |
| [phase2-pdfa-compliance.md](phase2-pdfa-compliance.md) | PDF/A-1b/2b/3b validate + convert | 2–3 months |
| [phase2-soft-masks.md](phase2-soft-masks.md) | Soft mask transparency rendering | 3 weeks |
| [phase2-permissions-enforcement.md](phase2-permissions-enforcement.md) | Enforce /P permissions flags | 2 weeks |
| [phase2-bookmarks-write.md](phase2-bookmarks-write.md) | Create/edit outline tree | 2 weeks |
| [phase2-pdf-optimization.md](phase2-pdf-optimization.md) | Compress, deduplicate, GC objects | 3 weeks |

Also in Phase 2 (no separate files — add inline during Phase 1/2 work):
- **ToUnicode CMap full support** — extend `src/fonts/encoding.rs::parse_to_unicode_cmap()` for CJK text extraction
- **RTL/BiDi text** — add `unicode-bidi = "0.3"` dep, apply in `src/text/extractor.rs`
- **XRef stream output** — add `write_xref_stream()` to `src/writer/xref.rs`
- **Named destinations write** — add `insert_named_dest()` to `src/document/name_tree.rs`

---

## Phase 3 — Competitive v2 (~12 months)

| File | Feature | Effort |
|------|---------|--------|
| [phase3-regex-search.md](phase3-regex-search.md) | Regex pattern search | 3–4 days |
| [phase3-watermark-api.md](phase3-watermark-api.md) | User-facing text/image watermark API | 3–4 days |
| [phase3-form-flatten.md](phase3-form-flatten.md) | Burn form fields into page content | 3–4 days |
| [phase3-javascript-actions.md](phase3-javascript-actions.md) | JS engine for form validation/calculation | 4–6 months |
| [phase3-rest-api.md](phase3-rest-api.md) | HTTP server API + client libraries | 2–3 months |

Also in Phase 3 (no separate files):
- **Type 3 fonts** — extend renderer to run charproc content streams per glyph
- **ICC color profiles** — implement Lab→RGB + ICC LUT in `src/render/color_profile.rs`
- **Type 1 tiling patterns** — implement `/PaintType 1` pattern tiling in renderer
- **OpenType/CFF full rendering** — extend `src/fonts/cff_parser.rs` with Type 2 charstring executor
- **PDF linearization** — add `src/writer/linearize.rs` post-processing pass
- **Document comparison** — add `src/analysis/compare.rs` using LCS text diff
- **PDF → HTML** — add `src/export/html.rs`
- **Barcode generation** — add `src/writer/barcode.rs` using `rxing` crate
- **Text reflow** — add `src/text/reflow.rs` XY-cut layout algorithm

---

## Phase 4 — Full Parity (~2+ years)

| File | Feature | Effort |
|------|---------|--------|
| [phase4-xfa-forms.md](phase4-xfa-forms.md) | XFA dynamic form detection + server-side flatten | 6–12 months |
| [phase4-mobile-sdks.md](phase4-mobile-sdks.md) | iOS + Android SDKs (WASM-in-WebView + native FFI) | 6–12 months each |

Also in Phase 4:
- **PDF/UA accessibility** — tagged PDF structure tree, ActualText
- **Collaboration / realtime** — CRDT-based concurrent editing (automerge crate)
- **Certificate-based encryption** — RSA public key encryption of document key
- **Office → PDF** — LibreOffice server-side proxy
- **.NET / Java / Python client libraries** — thin HTTP client SDKs for Phase 3 REST API
