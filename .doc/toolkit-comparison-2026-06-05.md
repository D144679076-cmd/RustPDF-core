# PDF Toolkit Capability Analysis

**Date:** 2026-06-05
**Scope:** Current product vs. Nutrient (PSPDFKit) and Apryse (PDFTron) toolkits

---

## 1. What We Have (Current State)

| Module | Status | Notes |
|--------|--------|-------|
| PDF Parser | **Complete** | XRef table/stream, all object types, incremental updates, full filter pipeline |
| Content Stream | **Complete** | ~200 operators, full graphics state machine |
| Page Rendering | **Solid** | tiny-skia rasterizer, glyph cache, LRU tile cache; shading partial |
| Text Extraction | **Basic** | Baseline clustering only, no layout reflow |
| Text Editing | **Complete** | In-place edit, rich text (bold/italic/size/color/align), drag-select |
| Writer / Serializer | **Complete** | Full PDF output, incremental append or full rewrite |
| Page Operations | **Complete** | Add, delete, move, rotate, crop |
| Annotations | **Partial** | 8 types: Text, Highlight, StrikeOut, Underline, Link, FreeText, Ink, Redact |
| Forms (AcroForm) | **Partial** | Text, Checkbox, Radio, List, Combo — creation only, no fill/flatten |
| Encryption | **Partial** | RC4 40/128-bit + AES-128 decrypt; AES-256 detected but not decrypted |
| Merge | **Complete** | Multi-source merge with ID remapping |
| Redaction | **Complete** | Forensic-safe zone redaction |
| Metadata | **Complete** | /Info dict read/write |
| Outlines / Bookmarks | **Read-only** | Parsing only, no write API |
| WASM Bridge | **Complete** | Full JS bindings via wasm-bindgen |
| Web Editor UI | **Partial** | Vue3/Quasar — viewer, editor, annotations, outlines, thumbnail, version history |

---

## 2. Feature Comparison Matrix

| Capability | Nutrient | Apryse | **Ours** |
|------------|:--------:|:------:|:--------:|
| **Core Parsing** | | | |
| PDF 1.0–2.0 | ✅ | ✅ | ✅ |
| XRef streams | ✅ | ✅ | ✅ read / ❌ write |
| AES-256 encrypted PDFs | ✅ | ✅ | ⚠️ detected, not decrypted |
| PDF linearization (Fast Web View) | ✅ | ✅ | ❌ |
| **Rendering** | | | |
| Page rasterization (RGBA) | ✅ | ✅ | ✅ |
| ICC color profiles | ✅ | ✅ | ❌ |
| CMYK accurate rendering | ✅ | ✅ | ⚠️ converts to RGB only |
| Type 3 fonts | ✅ | ✅ | ❌ |
| OpenType / CFF fonts | ✅ | ✅ | ⚠️ partial |
| CJK fonts (CID) | ✅ | ✅ | ⚠️ partial |
| Pattern fills (Type 1 / Type 2) | ✅ | ✅ | ⚠️ Type 2 only |
| Shading types 4–7 (mesh gradients) | ✅ | ✅ | ⚠️ Gouraud basic |
| Transparency / blending modes | ✅ | ✅ | ⚠️ partial |
| Soft masks | ✅ | ✅ | ❌ |
| Optional Content Groups (layers) | ✅ | ✅ | ❌ |
| **Text** | | | |
| Text extraction | ✅ | ✅ | ✅ basic |
| Full-text search (in-document) | ✅ | ✅ | ❌ |
| Regex search | ✅ | ✅ | ❌ |
| Text reflow / layout analysis | ✅ | ✅ | ❌ |
| RTL / BiDi text | ✅ | ✅ | ❌ |
| CJK text extraction | ✅ | ✅ | ⚠️ partial |
| ToUnicode CMap full support | ✅ | ✅ | ⚠️ partial |
| **Annotations** | | | |
| Total annotation types | 20+ | 30+ | 8 |
| Stamp annotations | ✅ | ✅ | ❌ |
| Polygon / Polyline | ✅ | ✅ | ❌ |
| FileAttachment | ✅ | ✅ | ❌ |
| Caret | ✅ | ✅ | ❌ |
| Watermark annotation | ✅ | ✅ | ❌ |
| Appearance streams (all types) | ✅ | ✅ | ⚠️ 4 types only |
| Flatten annotations to content | ✅ | ✅ | ❌ |
| **Forms** | | | |
| AcroForm read / fill | ✅ | ✅ | ⚠️ creation only |
| AcroForm flatten | ✅ | ✅ | ❌ |
| XFA dynamic forms | ✅ | ✅ | ❌ |
| FDF / XFDF import | ✅ | ✅ | ❌ |
| FDF / XFDF export | ✅ | ✅ | ❌ |
| JavaScript form validation | ✅ | ✅ | ❌ |
| Digital signature fields | ✅ | ✅ | ❌ |
| **Security** | | | |
| Digital signatures (PKCS#7 / PAdES) | ✅ | ✅ | ❌ |
| Signature validation | ✅ | ✅ | ❌ |
| Certificate-based encryption | ✅ | ✅ | ❌ |
| Document permissions enforcement | ✅ | ✅ | ❌ |
| **Document Operations** | | | |
| Page merge | ✅ | ✅ | ✅ |
| Page split / extract to new PDF | ✅ | ✅ | ❌ |
| Forensic redaction | ✅ | ✅ | ✅ |
| Watermark API | ✅ | ✅ | ❌ |
| Stamps | ✅ | ✅ | ❌ |
| Bookmarks write API | ✅ | ✅ | ❌ |
| Named destinations write | ✅ | ✅ | ❌ |
| JavaScript actions (doc / page events) | ✅ | ✅ | ❌ |
| PDF optimization / compression | ✅ | ✅ | ❌ |
| Document comparison | ✅ | ✅ | ❌ |
| **Compliance** | | | |
| PDF/A-1b, 2b, 3b validation | ✅ | ✅ | ❌ |
| PDF/A creation | ✅ | ✅ | ❌ |
| PDF/UA (accessibility) | ✅ | ✅ | ❌ |
| Tagged PDF structure | ✅ | ✅ | ❌ |
| **Conversion** | | | |
| PDF → image (raster) | ✅ | ✅ | ✅ |
| PDF → plain text | ✅ | ✅ | ✅ basic |
| PDF → HTML | ✅ | ✅ | ❌ |
| Image → PDF | ✅ | ✅ | ✅ |
| Office → PDF | ✅ | ✅ | ❌ |
| Barcode generation | ✅ | ✅ | ❌ |
| **Platform / SDK** | | | |
| Browser / WASM | ✅ | ✅ | ✅ |
| iOS SDK | ✅ | ✅ | ❌ |
| Android SDK | ✅ | ✅ | ❌ |
| .NET / Java bindings | ✅ | ✅ | ❌ |
| REST server API | ✅ | ✅ | ❌ |
| **UI Components** | | | |
| Viewer component | ✅ | ✅ | ✅ |
| Annotation toolbar | ✅ | ✅ | ⚠️ partial |
| Thumbnail sidebar | ✅ | ✅ | ✅ |
| Outline / bookmarks panel | ✅ | ✅ | ✅ read-only |
| Text search UI | ✅ | ✅ | ⚠️ panel exists, no backend |
| Form filling UI | ✅ | ✅ | ❌ |
| Signature UI | ✅ | ✅ | ❌ |
| Collaboration / realtime | ✅ | ✅ | ❌ |

---

## 3. Gap Analysis — What's Missing for Production

### Priority 1 — Blockers (cannot ship without these)

| Gap | Estimated Effort | Why Critical |
|-----|-----------------|-------------|
| AES-256 decryption | ~3 weeks | ~30% of modern PDFs use it; currently returns an error |
| Full-text search (Ctrl+F) | ~3 weeks | Every user expects in-document search |
| Form filling read + write | ~6 weeks | AcroForm fill is the #1 B2B use-case |
| Annotation flatten to content | ~2 weeks | Required before printing or distributing final docs |
| Page split / extract to new PDF | ~2 weeks | Basic document workflow |
| Watermark API | ~1 week | Common production requirement |
| Bookmarks write API | ~2 weeks | Authors need to create and edit outlines |

**P1 total: ~4–5 months**

---

### Priority 2 — Production Quality (needed within 6 months of launch)

| Gap | Estimated Effort | Why Needed |
|-----|-----------------|-----------|
| Digital signatures (PKCS#7 / PAdES) | ~3–4 months | Legal and enterprise market requirement |
| FDF / XFDF import and export | ~3 weeks | Standard for form data exchange |
| PDF/A-1b validation and creation | ~2–3 months | Government, legal, and archival markets |
| Appearance streams for all annotation types | ~4 weeks | Without them, annotations look broken in other readers |
| Soft masks | ~3 weeks | Many modern PDFs have invisible content without this |
| Optional Content Groups (PDF layers) | ~4 weeks | Technical / CAD PDFs are unusable without it |
| ToUnicode CMap full support | ~3 weeks | CJK and special-font text extraction currently broken |
| RTL / BiDi text support | ~6 weeks | Arabic and Hebrew markets blocked entirely |
| XRef stream output (write) | ~2 weeks | PDF 1.5+ writer conformance |
| Stamp annotations | ~2 weeks | Very common in enterprise workflows |
| FileAttachment annotations | ~2 weeks | Legal document packaging |
| Polygon / Polyline annotations | ~1 week | Standard markup tools |
| Document permissions enforcement | ~2 weeks | Read-only PDFs are not currently honored |
| PDF optimization / compression | ~3 weeks | Output file sizes are currently unoptimized |

**P2 total: ~9–12 months**

---

### Priority 3 — Competitive Parity (to fully match both toolkits)

| Gap | Estimated Effort |
|-----|-----------------|
| JavaScript actions engine (needs quickjs/duktape integration) | ~4–6 months |
| XFA dynamic forms | ~6–12 months |
| PDF/UA + Tagged PDF structure | ~3–4 months |
| ICC color profile support | ~2–3 months |
| Type 3 font rendering | ~2 months |
| OpenType / CFF full support | ~2–3 months |
| PDF linearization output | ~2–3 months |
| Document comparison | ~2–3 months |
| PDF → HTML conversion | ~4–6 months |
| Barcode generation | ~2 weeks |
| .NET / Java / Python bindings | ~3–6 months each |
| REST server API | ~2–3 months |
| Mobile SDKs (iOS + Android) | ~6–12 months each |
| Collaboration / realtime (CRDT or OT) | ~6–12 months |

**P3 total: ~3–5 years (full parity)**

---

## 4. Overall Progress Estimate

```
                    0%          50%         100%
Core Parsing:       ████████████████████░░░  ~88%
Rendering:          ████████████████░░░░░░░  ~68%
Text:               █████████░░░░░░░░░░░░░░  ~40%
Annotations:        ████████░░░░░░░░░░░░░░░  ~35%
Forms:              ████░░░░░░░░░░░░░░░░░░░  ~20%
Security / Crypto:  █████░░░░░░░░░░░░░░░░░░  ~22%
Doc Operations:     ███████░░░░░░░░░░░░░░░░  ~30%
Compliance (PDF/A): ░░░░░░░░░░░░░░░░░░░░░░░   ~0%
Conversion:         ████████░░░░░░░░░░░░░░░  ~30%
Platform / SDK:     ████░░░░░░░░░░░░░░░░░░░  ~15%

OVERALL vs. full toolkit parity:  ~35–40%
```

---

## 5. Roadmap Summary

| Phase | Target | Timeline |
|-------|--------|----------|
| **Shippable MVP** | P1 gaps closed — AES-256, search, forms, flatten, split, watermark | ~4–5 months |
| **Production v1** | P1 + critical P2 — signatures, PDF/A, layers, RTL, appearance streams | ~12–18 months |
| **Competitive v2** | P2 complete + JS actions, Type 3 fonts, ICC, REST API | ~2–3 years |
| **Full parity** | P3 complete — mobile, XFA, collaboration, all conversions | ~4–6 years |

---

## 6. Our Structural Advantage

Both Nutrient and Apryse represent 10–15 years of cumulative engineering. Full parity is not a realistic near-term goal. The practical strategy is to dominate a specific vertical rather than chase breadth.

**Key differentiator:** Pure Rust + WASM with zero native dependencies.

- Apryse still ships per-platform native binaries; its WASM build is a secondary target.
- Nutrient's WASM is a C++ port via Emscripten — a harder path to maintain than native WASM.
- Our WASM binary is first-class, not an afterthought — smaller, faster cold-start, easier to embed.

**Recommended focus:** "Best browser-native PDF editor for web applications" — capture the WASM-first market where neither incumbent is strong, then expand outward.
