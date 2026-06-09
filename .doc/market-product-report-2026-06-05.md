# pdf-core — Product & Market Report

**Date:** 2026-06-05
**Updated:** 2026-06-06 — Phase 1 complete (all 6 features shipped); added development economics (Opus 4.8 token-cost analysis)
**Scope:** Current product status, competitive landscape, market opportunity, and go-to-market strategy

---

## 1. Current Product Status

### What We Have Built

A Rust-based PDF processing core library (`pdf-core`) targeting both native platforms and WebAssembly (`wasm32-unknown-unknown`). The codebase is **~39,600 lines of Rust across 92 source files**, organized into 12 independent modules.

> **Phase 1 status (2026-06-06): COMPLETE.** All six demo-sprint features shipped — AES-256 decryption, full-text search, form filling (read + write), annotation flatten, page split/extract, and the subscription/licensing layer. See `phase/phase1-*.md` and the matching `.doc/*-2026-06-0{5,6}.md` reports.

| Module | Status | Description |
|--------|--------|-------------|
| Parser | ✅ Complete | Full PDF 1.0–2.0 parsing, XRef table/stream, all object types, incremental updates |
| Content Stream | ✅ Complete | ~200 operators, full graphics state machine |
| Page Rendering | ✅ Solid | tiny-skia rasterizer, fontdue glyph rendering, LRU tile cache, shading partial |
| Text Extraction | ✅ Basic | Baseline clustering, word-level position data |
| Text Editing | ✅ Complete | In-place edit, rich text (bold/italic/size/color/align/underline/strike), drag-select |
| Text Search | ✅ Complete | **Phase 1** — substring search, case-insensitive, per-page bounds (regex/UI deferred) |
| Writer / Serializer | ✅ Complete | Full PDF output, incremental append or full rewrite |
| Page Operations | ✅ Complete | Add, delete, move, rotate, crop |
| Page Split / Extract | ✅ Complete | **Phase 1** — deep-copy page subset into new PDF with ID remapping |
| Annotations | ⚠️ Partial | 8 types + **flatten** (Phase 1): Text, Highlight, StrikeOut, Underline, Link, FreeText, Ink, Redact |
| Forms (AcroForm) | ✅ Complete | **Phase 1** — read + fill existing fields (text, checkbox, radio, combo/list) + create |
| Encryption | ✅ Complete | **Phase 1** — RC4 + AES-128 + **AES-256 (R5/R6)** decrypt |
| Merge / Redact | ✅ Complete | Multi-source merge with ID remapping; forensic-safe redaction |
| Licensing | ✅ Complete | **Phase 1** — offline HMAC validation, 3 tiers, feature gating, trial watermark |
| Metadata | ✅ Complete | /Info dict read/write |
| Bookmarks | ⚠️ Read-only | Parse only, no write API |
| WASM Bridge | ✅ Complete | Full JavaScript bindings via wasm-bindgen |
| Web Editor UI | ⚠️ Partial | Vue3/Quasar — viewer, annotations, thumbnails, outlines, version history |

### Overall Progress vs. Full Toolkit Parity

```
                                              (▲ = Phase 1 gain)
Core Parsing:       ████████████████████░░░  ~88%
Rendering:          ████████████████░░░░░░░  ~68%
Text:               █████████████░░░░░░░░░░  ~55%  ▲ (+ search)
Annotations:        ██████████░░░░░░░░░░░░░  ~45%  ▲ (+ flatten)
Forms:              ████████████░░░░░░░░░░░  ~55%  ▲ (+ read/fill)
Security / Crypto:  ████████░░░░░░░░░░░░░░░  ~38%  ▲ (+ AES-256)
Doc Operations:     ██████████░░░░░░░░░░░░░  ~45%  ▲ (+ split/extract)
Compliance (PDF/A): ░░░░░░░░░░░░░░░░░░░░░░░   ~0%
Conversion:         ████████░░░░░░░░░░░░░░░  ~30%
Platform / SDK:     ██████░░░░░░░░░░░░░░░░░  ~25%  ▲ (+ licensing)

OVERALL vs. full toolkit parity:  ~45–50%  (was ~35–40% pre-Phase 1)
```

### Test Coverage

- **683 test functions** across unit + integration suites (up from 555 — Phase 1 added search, forms, AES-256, flatten, and extract coverage)
- 6 PDF test fixtures (minimal.pdf, multipage.pdf, with_stream.pdf, Group-3.pdf, Laspeyres.pdf, Unit_1.pdf)
- 7 benchmark suites covering rendering, tile cache, page lookup, and edit preview
- 67 implementation reports in `.doc/`
- All tests pass with `cargo clippy -D warnings` (zero warnings)

### Structural Advantage

The codebase is **pure Rust with zero native dependencies**. Every dependency is WASM-compatible. This is the core technical differentiator against all established competitors, which are C++ libraries compiled to WASM via Emscripten.

---

## 2. Competitive Landscape

### 2.1 Market Overview

The PDF software market was approximately **$3.5 billion in 2023**, projected to reach **$6 billion by 2028** (CAGR ~11%). The SDK/developer toolkit segment is a subset — estimated $500M–$1B — and growing faster than the overall market driven by:

- Digital transformation in enterprise (replacing paper workflows)
- Remote work driving document collaboration demand
- AI/LLM pipelines requiring structured document ingestion
- Privacy regulation (GDPR, HIPAA) pushing companies toward client-side processing
- Edge computing growth creating new deployment targets

### 2.2 Player-by-Player Analysis

---

#### Apryse (formerly PDFTron) — *Market Leader*

**Founded:** 2001 | **HQ:** Vancouver, Canada | **Funding:** ~$95M (Series B 2021)

**What they offer:** The most complete PDF SDK on the market. Every platform, every feature. iOS, Android, Windows, Linux, macOS, Web (WASM), .NET, Java, Python, Node.js. 30+ annotation types, XFA dynamic forms, CAD PDF support, digital signatures (PAdES/CAdES), PDF/A validation, linearization, document comparison, barcode generation, Office conversion.

**Pricing:**
- No public pricing (sales-driven)
- Estimated: $2,000–$5,000/year for basic license, $10,000–$50,000+/year for enterprise
- Per-platform fees — paying separately for iOS, Android, Web adds up quickly
- No free tier; 30-day trial only

**Strengths:**
- Widest feature coverage in the market — if a PDF feature exists, Apryse probably supports it
- 20+ years of battle-testing with millions of documents
- Excellent for technical PDFs (CAD, engineering drawings)
- Large customer base (Adobe, HP, IBM, Microsoft partners)
- Comprehensive documentation

**Weaknesses:**
- Brutally expensive — out of reach for startups and indie developers
- WASM build is Emscripten-compiled C++, not native WASM — 8–15MB compressed, slow cold start, can't run on edge runtimes (Cloudflare Workers, Deno Deploy)
- API designed in 2001 — verbose, object-heavy, feels archaic to modern developers
- Per-platform licensing creates friction and unexpected costs
- Heavy binary (100MB+ native distribution)
- Innovation pace has slowed — they're maintaining, not pioneering
- Complex license terms; audits for enterprise customers

**Customer pain points:** "We love the features but the price is insane for our stage." "The API feels like 2005 Java." "I can't use it on Cloudflare Workers."

---

#### Nutrient (formerly PSPDFKit) — *Premium Experience Player*

**Founded:** 2011 | **HQ:** Vienna, Austria / New York | **Funding:** ~$97M (Series A 2022)

**What they offer:** Originally the best mobile PDF SDK (iOS/Android), expanded to Web and server. Known for beautiful, polished UI components and modern developer experience. Strong in healthcare, legal, and enterprise SaaS.

**Pricing:**
- Web SDK: ~$399–$1,199/month depending on tier
- Mobile: separate pricing per platform
- Server: separate pricing
- No truly free tier; 60-day trial

**Strengths:**
- Best-looking UI components out of any PDF SDK — white-label quality
- Best-in-class mobile (iOS/Android) — was the original killer app
- Modern developer experience — good documentation, sensible API
- Active product development
- Strong in regulated industries (healthcare, legal)
- Good WebSocket-based collaboration features

**Weaknesses:**
- Expensive for startups — $400/month minimum before validating product-market fit
- WASM build is also Emscripten-compiled C++ — same architectural weakness as Apryse
- Less complete than Apryse on technical PDFs (CAD, complex XFA)
- Server component required for some features — breaks pure offline/privacy use cases
- Pricing structure is confusing (Web + Mobile + Server = three separate bills)
- No edge computing support

**Customer pain points:** "$400/month is too much for a side project." "I need it to work without a server." "The WASM binary is 12MB and kills my Lighthouse score."

---

#### iText — *The Java/Enterprise Compliance Champion*

**Founded:** 2000 | **HQ:** Ghent, Belgium / Boston, USA | **Model:** Open core (AGPL + commercial)

**What they offer:** The dominant PDF library in the Java and .NET enterprise world. Extremely strong in PDF/A compliance, digital signatures (CAdES, PAdES, XAdES), and document generation. Popular in banking, insurance, and government.

**Pricing:**
- AGPL v3 for open source use (forces your code to be open source)
- Commercial license: ~$500–$2,000/year per developer (iText 7 Core)
- Enterprise plans with SLA support: $5,000–$20,000+/year

**Strengths:**
- Mature and battle-tested (25 years)
- Best PDF/A compliance validation and conversion
- Strong digital signature support (qualified signatures for eIDAS compliance)
- Large open source community and ecosystem
- Trusted by banks, governments, healthcare institutions
- AGPL model drives massive organic adoption

**Weaknesses:**
- **Java/.NET only** — no JavaScript, no WASM, no browser support whatsoever
- Any web use requires a server roundtrip — not viable for modern web-first development
- Rendering quality is mediocre — they generate PDFs more than they render them
- AGPL license forces either open-sourcing your product or paying
- API is verbose and Java-centric
- No mobile SDK
- No browser or edge deployment

**Customer pain points:** "I love iText but I need it in the browser." "AGPL is a license trap for commercial products." "The API feels like 2008 Java."

---

#### PDF.js (Mozilla) — *The Free Browser Viewer*

**Founded:** 2011 (open source) | **License:** Apache 2.0 | **Backing:** Mozilla

**What they offer:** A pure JavaScript PDF viewer that runs in the browser. Used in Firefox's built-in PDF viewer, by Wikipedia, Google Drive fallback, and millions of web applications.

**Pricing:** Free and open source forever.

**Strengths:**
- Completely free, MIT/Apache licensed
- Runs in every browser
- Highly trusted brand (Mozilla)
- Good rendering quality for a JavaScript implementation
- Massive adoption — battle-tested with every PDF on the internet

**Weaknesses:**
- **Viewer only** — zero editing capability
- Cannot create, modify, sign, fill forms, extract structured text, redact, or merge PDFs
- Slow on large files (JavaScript-based rendering)
- No PDF text extraction with layout
- No annotation writing
- Architecture makes adding editing nearly impossible

**Market role:** PDF.js trained an entire generation of web developers to expect browser-native PDF viewing, then left them stranded when they needed to do anything else. This is our on-ramp — PDF.js users who outgrow it are our most natural customers.

---

#### pdf-lib — *The Lightweight JavaScript Editor*

**Founded:** 2019 | **License:** MIT | **Status:** Community-maintained

**What they offer:** A pure JavaScript/TypeScript library for creating and modifying PDFs. Runs in browser and Node.js. Very popular for simple document generation tasks.

**Pricing:** Free and open source.

**Strengths:**
- Free, MIT license
- Pure JavaScript — no native deps, WASM optional
- Can create PDFs and do basic modifications (add text, images, pages)
- Good TypeScript types
- Lightweight (~500KB)

**Weaknesses:**
- No PDF rendering — cannot display a PDF as an image or canvas
- No text extraction
- No annotation reading/writing (only very basic support)
- No forms support
- No digital signatures
- No text search
- Not suitable for any complex PDF workflow
- Development pace has slowed significantly

**Market role:** The "gateway drug" — developers use pdf-lib for simple tasks then quickly need something more capable. Good referral opportunity.

---

#### MuPDF (Artifex) — *The Fast C Engine*

**Founded:** 1994 | **HQ:** San Francisco | **Model:** AGPL + commercial

**What they offer:** A lightweight C library for PDF rendering and viewing. Used in many mobile apps, e-readers, and embedded devices. Included in Kindle, various Android PDF apps.

**Pricing:**
- AGPL v3 for open source
- Commercial: custom pricing, estimated $5,000–$50,000+/year

**Strengths:**
- Extremely fast and memory-efficient — best performance of any open-source engine
- Small footprint — good for embedded/mobile
- Excellent rendering fidelity
- Has a JavaScript binding (mupdf.js) via WASM

**Weaknesses:**
- AGPL license problem (same as iText)
- Primarily a renderer — editing features are minimal
- C codebase — complex to integrate, unsafe, hard to contribute to
- API is low-level and painful to work with directly
- mupdf.js WASM binary is still Emscripten-compiled C

---

#### Foxit PDF SDK — *The Apryse Alternative*

**Founded:** 2001 | **HQ:** Fremont, California | **Model:** Commercial

**What they offer:** A direct competitor to Apryse with similar feature coverage. Less well-known in the Western developer market but popular in Asia.

**Pricing:** Similar to Apryse, estimated $2,000–$10,000+/year.

**Strengths:**
- Comprehensive feature set
- Slightly cheaper than Apryse in some configurations
- Good for government/enterprise contracts in Asian markets

**Weaknesses:**
- Smaller developer mindshare than Apryse in the West
- Documentation is weaker
- Same C++ native binary problem
- WASM support is minimal
- Less active developer community

---

#### pdfmake / jsPDF — *Simple JavaScript Creation*

- **Free, open source** (MIT)
- Creation only — no editing or rendering of existing PDFs
- Very limited: basic text, images, tables
- Popular for invoice/receipt generation
- Users immediately outgrow them for any real document workflow

---

### 2.3 Competitor Summary Table

| Player | Price/Year | WASM | Edge | Mobile | Signatures | PDF/A | Open Source | Render | Edit |
|--------|-----------|------|------|--------|-----------|-------|-------------|--------|------|
| Apryse | $2K–$50K+ | ⚠️ C++ port | ❌ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ |
| Nutrient | $5K–$15K+ | ⚠️ C++ port | ❌ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ |
| iText | $500–$20K+ | ❌ | ❌ | ❌ | ✅ | ✅ | AGPL | ⚠️ | ✅ |
| PDF.js | Free | ✅ native JS | ✅ | ✅ | ❌ | ❌ | ✅ MIT | ✅ | ❌ |
| MuPDF | $5K–$50K+ | ⚠️ C++ port | ❌ | ✅ | ❌ | ❌ | AGPL | ✅ | ⚠️ |
| pdf-lib | Free | ✅ native JS | ✅ | ✅ | ❌ | ❌ | ✅ MIT | ❌ | ⚠️ |
| Foxit | $2K–$10K+ | ⚠️ C++ port | ❌ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ |
| **pdf-core (ours)** | **TBD** | **✅ native Rust** | **✅** | **✅ (WebView)** | **🔄 Phase 2** | **🔄 Phase 2** | **TBD** | **✅** | **✅** |

---

## 3. Gap Analysis — What We're Missing vs. Full Parity

### Phase 1 — ✅ COMPLETE (2026-06-06)

All blocker features shipped. Phase 1 closed in **~1 week of wall-clock time** with Claude Code (Opus 4.8), not the ~1 month originally budgeted.

| Feature | Status | Report |
|---------|--------|--------|
| AES-256 decryption | ✅ Done 2026-06-05 | `crypto-aes256-2026-06-05.md` |
| Full-text search | ✅ Done 2026-06-06 (regex + web UI deferred to Phase 3) | `text-search-2026-06-06.md` |
| Form filling (read + write) | ✅ Done 2026-06-06 | `forms-phase1-2026-06-06.md` |
| Annotation flatten | ✅ Done 2026-06-05 | `annotation-flatten-2026-06-05.md` |
| Page split / extract | ✅ Done 2026-06-06 | `extract-pages-2026-06-06.md` |
| Subscription / licensing layer | ✅ Done 2026-06-06 | `license-2026-06-06.md` |

**Deferred to later phases (non-blocking):** user-facing watermark API (Phase 3 — trial watermark already exists in the licensing layer), bookmarks write API (Phase 2), regex search + search UI (Phase 3).

### Phase 2 Gaps (Production quality — 6 months)

Digital signatures (PKCS#7/PAdES), FDF/XFDF import/export, PDF/A-1b/2b/3b validation and creation, appearance streams for all annotation types, soft masks, optional content groups (layers), full ToUnicode CMap support, RTL/BiDi text, XRef stream output, missing annotation types (Stamp, Polygon, Polyline, FileAttachment), document permissions enforcement, PDF optimization/compression.

### Phase 3 Gaps (Competitive parity — 12 months)

JavaScript actions engine, Type 3 fonts, ICC color profiles, PDF linearization, document comparison, REST server API, regex search, watermark API (user-facing), form flatten, text reflow/layout analysis, PDF → HTML, barcode generation, .NET/Java/Python client libraries, signature UI.

### Phase 4 Gaps (Full parity — 2+ years)

XFA dynamic forms, PDF/UA accessibility, mobile native SDKs (iOS/Android), real-time collaboration (CRDT), Office → PDF conversion, certificate-based encryption.

---

## 4. Market Opportunity — Where We Can Win

### 4.1 Gap 1: WASM-Native, Edge-Ready PDF Processing

**This is our biggest structural advantage and a completely unserved market.**

Every competitor ships C++ compiled to WASM via Emscripten. This creates hard limitations:
- Binary size: Apryse WASM is 8–15MB compressed; Nutrient is 10–12MB. Cold-start time is 2–5 seconds on mid-range devices.
- Cannot run on Cloudflare Workers, Deno Deploy, Fastly Compute@Edge — these runtimes restrict POSIX syscalls that Emscripten requires.
- Emscripten adds a compatibility shim layer that causes subtle bugs and inflates the binary with unused code.

Our pure Rust → WASM binary will be **2–4MB compressed** with sub-500ms cold start. We can run on:
- Every browser (Chrome, Firefox, Safari, Edge)
- Node.js and Bun
- Cloudflare Workers (4 million+ developers)
- Deno Deploy
- Fastly Compute@Edge
- AWS Lambda@Edge
- Any WASI-compatible runtime

**None of the existing players can do this.** This is not a small feature difference — it's a categorical capability that enables entirely new architectures.

Target developers: those building document-heavy SaaS who want zero server overhead, edge-deployed PDF processing, or browser-only workflows.

**Market size estimate:** 500K+ web developers globally who currently use Apryse/Nutrient or have no good option. Even capturing 1% = 5,000 customers at $50/month = $3M ARR.

---

### 4.2 Gap 2: AI/LLM Document Pipeline Component

**The fastest-growing adjacent market right now.**

Every company building RAG (Retrieval-Augmented Generation) systems needs to ingest PDFs. LangChain has 70,000+ GitHub stars. LlamaIndex has 30,000+. The document ingestion problem is widely acknowledged as unsolved.

Current solutions being used:
- **PyPDF2**: slow, lossy text extraction, destroys column layout
- **pdfminer.six**: better but still Python-only, loses reading order
- **unstructured.io**: SaaS with per-page pricing, sends documents to their cloud
- **Apache Tika**: Java dependency, overkill for most use cases

None of these produce structured, layout-aware output that respects reading order, column detection, and paragraph boundaries — which is exactly what you need for quality RAG chunking.

Our text extraction with layout analysis (Phase 3 text reflow module) + structured JSON output can be:
1. A Rust crate used directly by Rust AI backends
2. A Python package via PyO3 bindings (weeks of work)
3. A Cloudflare Worker API for serverless document processing

The TAM here is massive and growing. Every company building on top of LLMs needs document processing. Being the best PDF-to-structured-text converter in this ecosystem could drive significant organic adoption.

**Market size estimate:** Conservatively 10,000+ companies building AI document pipelines in 2024, growing to 100,000+ by 2026.

---

### 4.3 Gap 3: Privacy-First / 100% Offline Processing

**Regulatory compliance as a sales advantage.**

Healthcare (HIPAA), legal (attorney-client privilege), finance (SEC/FINRA, PCI-DSS), government — these industries cannot send documents to third-party cloud APIs without legal risk.

Apryse and Nutrient both:
- Have license server validation that phones home
- Have telemetry in some SDK versions
- Have cloud features that require document upload

Our architecture is:
- 100% client-side WASM — document bytes never leave the browser
- Zero telemetry, zero phone-home (by design)
- Self-hostable: static file hosting only, no backend required
- License validation is offline HMAC (no network call needed)

"Your patient documents never leave the browser" is not just a feature — it's a HIPAA compliance checkbox. Hospital procurement processes will pay a premium for this. One enterprise healthcare contract is worth $20K–$50K/year.

**Target buyers:** Hospital networks, law firms, insurance companies, government agencies, any company handling PII.

---

### 4.4 Gap 4: The Rust Ecosystem Has No Good PDF Library

**Developer mindshare through community ownership.**

Rust has exploded in popularity — ranked #1 "most loved language" in Stack Overflow surveys for 8 consecutive years. The crates.io ecosystem has 130,000+ crates. There is no production-quality PDF editing library in Rust:

- **lopdf**: unmaintained, read-only access only, no editing
- **printpdf**: creation only, no rendering, no parsing of existing PDFs
- **pdf-rs**: incomplete, abandoned, minimal feature set
- **pdfium-render**: Rust bindings for PDFium (Google's C++ library) — native dep problem

Any Rust developer who needs to process PDFs today must either use a Python subprocess, write unsafe FFI to MuPDF, or give up. We fill this gap completely.

Being the canonical answer to "PDF processing in Rust" on crates.io generates:
- GitHub stars and organic marketing
- Open source contributors who improve the product
- Conference talks and blog posts
- Enterprise customers who are building Rust-based backends

**Estimated Rust developer count:** 3+ million globally, growing ~40% year-over-year.

---

### 4.5 Gap 5: Cloudflare Workers / Edge Computing PDF API

**A completely unserved distribution channel.**

Cloudflare Workers has 4+ million registered developers. The platform runs JavaScript and WASM workloads at 300+ edge locations globally. It's the fastest-growing infrastructure platform in developer tooling.

No PDF toolkit runs natively on Cloudflare Workers today. Developers who need to generate thumbnails, extract text, watermark documents, or fill forms in a Worker have zero options. They must call out to an external API (introducing latency, cost, and privacy concerns) or give up.

We can publish:
1. A Cloudflare Workers template for PDF processing (one-click deploy)
2. An npm package optimized for the Workers runtime
3. Integration with Cloudflare R2 (their object storage) for PDF pipelines

Cloudflare has a marketplace and templates directory that gets significant developer traffic. Being the featured PDF solution there is essentially free distribution.

---

## 5. Strategic Positioning

### What We Are

**"The PDF toolkit that runs everywhere WASM runs — browser, edge, and server — with zero native dependencies, zero telemetry, and developer-first pricing."**

We are not trying to be Apryse. Apryse is 20+ years old with 100+ engineers and deep enterprise sales. We win by being the thing they structurally cannot be: lightweight, WASM-native, privacy-first, developer-affordable.

### What We Are Not

- Not a viewer only (that's PDF.js)
- Not a server-only tool (that's iText)
- Not a heavy enterprise SDK that requires a sales call (that's Apryse)

### Our Three Pillars

1. **Architecture-first:** Pure Rust → WASM. Runs everywhere. Smaller, faster, more portable than any C++ competitor.
2. **Privacy-first:** 100% client-side. Zero phone-home. Self-hostable. HIPAA/GDPR compliant by design.
3. **Developer-first:** Affordable pricing tiers. Great documentation. MIT-friendly licensing options.

---

## 6. Implementation Roadmap

Timelines below are **re-derived from observed Claude Code + Opus 4.8 velocity**, not traditional engineering estimates. The proof point: the full ~39,600-line codebase with 683 tests was built in ~2 weeks, and Phase 1 (6 features) closed in ~1 week — with **4 of the 6 features landing on a single day (2026-06-06)**.

> **Calibration correction:** the *original* plan budgeted Phase 1 at ~4 weeks **even with Claude Code**; it shipped in ~1 week. Even the Claude-assisted estimates were ~4× too conservative, so the Phase 2–4 calendars below have been compressed accordingly. The gating factor is no longer the easy/medium features (which batch several-per-day) but the handful of genuinely hard **T4** features — digital signatures, the JS engine, XFA, CRDT — that need careful build + review and cannot be rushed.

### Phase 1 — Demo Sprint — ✅ COMPLETE (~1 week, closed 2026-06-06)

Shipped: end-user beta with core editing features and licensing infrastructure.

| Feature | File | Status |
|---------|------|--------|
| AES-256 decryption fix | `phase/phase1-aes256-decryption.md` | ✅ 2026-06-05 |
| Full-text search | `phase/phase1-full-text-search.md` | ✅ 2026-06-06 |
| Form filling (read + write) | `phase/phase1-form-filling.md` | ✅ 2026-06-06 |
| Annotation flatten | `phase/phase1-annotation-flatten.md` | ✅ 2026-06-05 |
| Page split / extract | `phase/phase1-page-split.md` | ✅ 2026-06-06 |
| Subscription / licensing | `phase/phase1-licensing.md` | ✅ 2026-06-06 |

### Phase 2 — Production v1 (~2.5 weeks → end ~late-June 2026)

Digital signatures, FDF/XFDF, PDF/A, optional content groups, soft masks, bookmarks write, PDF optimization, full annotation type coverage, permissions enforcement. *Gate: digital signatures (~3–4 days, crypto review). The other 11 features batch in ~1.5 weeks.*

### Phase 3 — Competitive v2 (~3 weeks → end ~mid-July 2026)

REST server API, JavaScript actions, regex search, text reflow, form flatten, watermark API, ICC colors, Type 3 fonts, OpenType/CFF, PDF linearization, document comparison, .NET/Java/Python client libraries. *Gate: JS actions engine (~5 days). The remaining 13 features batch in ~2 weeks.*

### Phase 4 — Full Parity (~4 weeks → end ~mid-Aug 2026)

Mobile SDKs (iOS/Android), XFA forms, PDF/UA, collaboration/CRDT, Office conversion, certificate encryption. *Gates: XFA (~7–10 days) and CRDT (~5 days) dominate the critical path; mobile SDKs and cert encryption fill the gaps.*

All phase files with detailed implementation instructions are in `pdf-editor-rust-core/phase/`.

---

## 6.5 Development Economics — Token & Cost Analysis (Revised 2026-06-08)

This project's primary build cost is **Claude API tokens**, not engineering payroll. This section covers: how many tokens the full build requires, and what those tokens cost at standard Anthropic API pricing for each available model. Actual billing channel costs are not covered here — apply your own provider pricing against the token estimates below.

---

### Model Blended Rates

All calculations use a **90% input / 10% output split** with **75% prompt cache hit rate** (agentic sessions re-send history heavily). T4 features use Opus Fast Mode with lower cache efficiency (~25% hits) due to novel-algorithm sessions with frequent context resets.

| Model | Blended rate/M tokens | Notes |
|-------|-----------------------|-------|
| Sonnet 4.6 | **~$2.38/M** | 22.5% fresh × $3 + 67.5% cache × $0.30 + 10% out × $15 |
| Opus 4.8 Standard | **~$3.96/M** | 22.5% fresh × $5 + 67.5% cache × $0.50 + 10% out × $25 |
| Opus 4.8 Fast Mode (T4) | **~$11.98/M** | 67.5% fresh × $10 + 22.5% cache × $1 + 10% out × $50 |

---

### Why the 2×–5× Multiplier Does Not Apply to Agentic Sessions

The common rule of thumb — "1M tokens of final output costs 2M–5M tokens total" — holds for single-turn API calls but breaks in agentic Claude Code sessions. Every tool call re-sends the full conversation history:

| Token sink | Share of total |
|---|---|
| Conversation history re-sent on every turn | ~40–50% |
| File reads (same files re-read across iterations) | ~20–30% |
| Build output, test failures, clippy errors fed back | ~15–20% |
| **Actual new code / output generated** | **~5–10%** |

**Phase 1 calibration:** ~20M tokens consumed to produce ~50K tokens of final code — an agentic multiplier of **~400×**, not 2–5×. The final code is ~5–10% of total tokens; the rest is context re-processed on every agentic turn.

---

### Final Product Size at Full Parity

Current codebase: **42,168 lines across 92 Rust files**.

| Artifact | Lines | Tokens (~12/line) |
|---|---|---|
| Current codebase | 42,168 | ~506K |
| Remaining code to write (P2–P4) | ~45,000–60,000 | ~540K–720K |
| + Tests (~40% of code volume) | ~18,000–24,000 | ~216K–288K |
| + Docs, reports, WASM bindings | — | ~200K–300K |
| **Full project final product** | **~100,000–120,000 lines** | **~1.1M–1.5M tokens** |

---

### Feature Sizing Tiers

| Tier | Description | Tokens consumed | Cost — Sonnet 4.6 | Cost — Opus 4.8 Std |
|------|-------------|-----------------|-------------------|---------------------|
| T1 Simple | 1 file, few edits | 1M–2M | $2.38–$4.76 | $3.96–$7.92 |
| T2 Medium | 2–3 files + WASM bindings | 3M–6M | $7.14–$14.28 | $11.88–$23.76 |
| T3 Complex | multi-file, heavy iteration | 8M–15M | $19.04–$35.70 | $31.68–$59.40 |
| T4 Very Hard | novel algorithms / spec work | 4M–8M\* | $47.92–$95.84\* | $47.92–$95.84\* |

> \*T4 sized at **Opus 4.8 Fast Mode** (~$11.98/M, low cache efficiency). Token count is lower than T3 because Fast Mode costs ~5× more per token — fewer tokens but higher first-pass correctness reduces total iterations.

---

### Per-Phase Token & Cost Estimates

**Sonnet 4.6 only:**

| Phase | Active days | Tokens | Cost @ $2.38/M |
|-------|-------------|--------|----------------|
| Phase 1 ✅ | ~5 | ~20M | ~$48 |
| Phase 2 | ~11 | ~40M | ~$95 |
| Phase 3 | ~14 | ~55M | ~$131 |
| Phase 4 | ~14 | ~55M | ~$131 |
| **Remaining (P2–P4)** | **~39** | **~150M** | **~$357** |
| **Full project (P1–P4)** | **~44** | **~170M** | **~$405** |

**Opus 4.8 Standard only:**

| Phase | Active days | Tokens | Cost @ $3.96/M |
|-------|-------------|--------|----------------|
| Phase 1 ✅ | ~5 | ~20M | ~$79 |
| Phase 2 | ~11 | ~40M | ~$158 |
| Phase 3 | ~14 | ~55M | ~$218 |
| Phase 4 | ~14 | ~55M | ~$218 |
| **Remaining (P2–P4)** | **~39** | **~150M** | **~$594** |
| **Full project (P1–P4)** | **~44** | **~170M** | **~$673** |

**Mixed — Sonnet 4.6 for T1–T3, Opus 4.8 Fast Mode for T4 sprint days (~6 days total):**

| Phase | Sonnet tokens | Opus Fast tokens | Total tokens | Total cost |
|-------|--------------|-----------------|--------------|------------|
| Phase 2 | ~28M | ~8M | ~36M | ~$163 |
| Phase 3 | ~38M | ~8M | ~46M | ~$186 |
| Phase 4 | ~38M | ~8M | ~46M | ~$186 |
| **Remaining total** | **~104M** | **~24M** | **~128M** | **~$535** |
| **Full project** | **~124M** | **~24M** | **~148M** | **~$583** |

---

### Total Token Budget — Cross-Check

| Method | Remaining (P2–P4) | Full project |
|---|---|---|
| Daily rate × active days (4M/day) | ~156M–176M | ~176M–196M |
| Per-feature tier model (T1–T4 × 33 features) | ~120M–225M | ~140M–245M |
| **Realistic midpoint** | **~130M–175M** | **~150M–200M** |

**Working estimate: ~170M tokens for the full project to parity.** At standard API prices:

| Model strategy | ~170M tokens total cost |
|---|---|
| Sonnet 4.6 only | **~$405** |
| Opus 4.8 Standard only | **~$673** |
| Mixed (Sonnet T1–T3 + Opus Fast T4) | **~$583** |
| Opus 4.8 Fast Mode only | **~$2,037** |

---

### Calendar

**Token scope is fixed by the roadmap; pace determines calendar.** Remaining ~44 active build-days:

| Scenario | Pace | Remaining calendar | Full parity by |
|----------|------|--------------------|----------------|
| Conservative | part-time, every line reviewed | ~20 weeks | ~Oct–Nov 2026 |
| **Realistic** (Phase 1 pace) | steady, 3–4 days/week | **~12–15 weeks** | **~Sep 2026** |
| Aggressive | full-time sprint | ~9 weeks | ~mid-Aug 2026 |

T4 features (signatures, JS engine, XFA, CRDT) impose review gates regardless of pace.

---

### Sonnet 4.6 vs Opus 4.8 — Head-to-Head

| | Sonnet 4.6 | Opus 4.8 Standard | Opus 4.8 Fast Mode |
|---|---|---|---|
| **Input price** | $3/M | $5/M | $10/M |
| **Output price** | $15/M | $25/M | $50/M |
| **Cache read price** | $0.30/M | $0.50/M | $1/M |
| **Blended rate** (90/10, 75% cache) | **$2.38/M** | **$3.96/M** | **$11.98/M** |
| **Cost multiplier vs Sonnet** | 1× (baseline) | 1.66× | 5.03× |
| **Tokens per $10** | 4.2M | 2.5M | 0.83M |
| **Full project cost (~170M tokens)** | **~$405** | **~$673** | **~$2,037** |
| **Remaining P2–P4 cost (~150M tokens)** | **~$357** | **~$594** | **~$1,797** |
| **Best for** | T1–T3 (daily build work) | All phases if budget allows | T4 only (novel algorithms) |
| **Output quality** | Good — may need 2–3 iterations on complex tasks | Better — fewer iterations needed | Best first-pass on novel spec work |
| **Throughput at same budget** | Highest | 1.66× lower | 5× lower |
| **Recommendation** | ✅ Primary model | ⚠️ Use if iteration count is high | ✅ T4 sprint days only |

**Mixed strategy total (~170M tokens, ~6 T4 days on Opus Fast):**
- Sonnet tokens: ~146M × $2.38/M = **~$347**
- Opus Fast tokens: ~24M × $11.98/M = **~$288**
- **Total: ~$635** — sits between Sonnet-only ($405) and Opus Standard-only ($673) in cost, with better T4 quality than Sonnet alone

**When to switch to Opus:**
- T4 features where first-pass correctness saves >2 full debug iterations (each iteration ≈ 1–3M tokens = $2–7 at Sonnet pricing)
- A single saved debug day on a T4 feature can pay for switching to Opus Fast Mode on that sprint

---

### The Economic Case

| | pdf-core (Claude Code) | Traditional team |
|---|---|---|
| Token cost to full parity | **~$405–673** (Sonnet or Opus Std) | ~$360,000 (2 senior Rust eng × 12 mo @ $180K) |
| Time to parity | **~9–15 weeks** | ~24 months |
| Cost advantage | **~500–900×** | — |

The real investment is reviewer time (~2–3 hrs/day steering the agent), not compute.


#### Why the simple 2×–5× multiplier does not apply to agentic sessions

A common rule of thumb for API usage — "1M tokens of final output costs 2M–5M tokens total" — holds for single-turn requests but breaks completely for agentic Claude Code sessions. In agentic sessions every tool call re-sends the full conversation history:

| Token sink in a Claude Code session | Share of total consumed |
|---|---|
| Conversation history re-sent on every turn | ~40–50% |
| File reads (same files re-read across iterations) | ~20–30% |
| Build output, test failures, clippy errors fed back | ~15–20% |
| **Actual new code / output generated** | **~5–10%** |

**Observed from Phase 1 calibration:**
- Phase 1 consumed ~20M tokens (5 days × 4M/day at Pro rate limit)
- Phase 1 produced ~50K tokens of final code
- Actual agentic multiplier: **~400× final output** (vs. the naive 2–5×)

The final code is 5–10% of total tokens consumed. The remainder is context being re-processed on every agentic turn — conversation history, file re-reads, compiler output, and fix-iteration loops.

#### Total token estimate — three methods cross-checked

| Method | Basis | Remaining (P2–P4) | Full project |
|---|---|---|---|
| Simple 2×–5× formula | ×1.3M final output | 2.6M–6.5M | ← underestimates |
| Daily rate × active days | 4M/day × 44 remaining days | ~176M | ~196M (49 days) |
| Per-feature tier model | T1–T4 sizing × feature count | ~120M–225M | ~140M–245M |
| **Realistic range** | Cross-checked midpoints | **~130M–180M** | **~150M–200M** |

All three rate-based methods converge. The simple multiplier underestimates by ~30–50×.

#### Per-feature breakdown — tier model (remaining P2–P4)

| Tier | Feature count | Tokens each | Subtotal |
|------|--------------|-------------|---------|
| T1 Simple | ~5 | 1M–2M | ~7.5M |
| T2 Medium | ~12 | 3M–6M | ~54M |
| T3 Complex | ~12 | 8M–15M | ~138M |
| T4 Very Hard (Opus Fast Mode) | ~4 | 4M–8M at Fast pricing | ~24M |
| **Total remaining (P2–P4)** | **~33 features** | | **~120M–225M** |

Midpoint: **~150M tokens** for remaining work. Full project (including Phase 1 ~20M): **~170M tokens**.

T4 token counts appear lower than T3 because Fast Mode costs ~5× more per token — the same dollar buys fewer tokens but at higher first-pass correctness, reducing total iterations needed.

#### Full project token budget vs. FreeModel plan

| Phase | Features | Tokens | Days @ 4M/day | FreeModel actual cost |
|-------|----------|--------|---------------|-----------------------|
| Phase 1 ✅ | 6 | ~20M | ~5 | included in sub |
| Phase 2 | ~11 | ~40M | ~10 | ~£5–10 |
| Phase 3 | ~14 | ~55M | ~14 | ~£10–15 |
| Phase 4 | ~8 | ~55M | ~14 | ~£10–20 |
| **Full project** | **~39** | **~170M** | **~43–49** | **~£30–50 (~$38–63)** |

The 170M tokens at 4M/day = **~43 active days** — consistent with the 44-day P2–P4 roadmap estimate. The FreeModel plan rate limit is the binding constraint, not the work scope. Total out-of-pocket for the entire project to full toolkit parity: approximately **£35–55**.

---

## 7. Revenue Model

### Tier Structure

| Tier | Price | Features | Target |
|------|-------|---------|--------|
| Free | $0 | View, render, basic text extraction — with watermark on save | Open source, evaluation |
| Pro | $29–99/month | Full editing, search, forms, merge, split, flatten, redact, watermark, merge | Indie devs, startups |
| Enterprise | $499–999/month | Pro + digital signatures, PDF/A, layers, REST API, SLA | SMB, regulated industries |
| Custom | $10K–50K+/year | On-premise, custom integration, dedicated support | Enterprise healthcare/legal/gov |

### Revenue Channels

| Channel | Target Customer | Path to Revenue |
|---------|----------------|----------------|
| WASM SDK (web) | Startups / SaaS companies | Self-serve, credit card, no sales call needed |
| Rust crate (commercial) | Rust backend companies | License key via crates.io or GitHub Sponsors |
| AI document pipeline | LangChain/LlamaIndex ecosystem | Python package + API endpoint |
| Cloudflare Workers | Edge developers | Workers marketplace + npm package |
| Enterprise on-premise | Healthcare, legal, finance | Sales-driven, annual contracts |
| REST API (SaaS) | Any language (Python, .NET, Java) | Metered pricing per document operation |

### Realistic Revenue Projections

| Timeline | Milestone | Revenue |
|----------|-----------|---------|
| **Now (2026-06)** | **Phase 1 shipped** — core editing + licensing live | pre-revenue |
| Month 2 (~Jul) | Phase 2 complete, enterprise pipeline open | $2K–5K MRR |
| Month 4 (~Sep) | Phase 3 + REST API live, AI ecosystem traction | $15K–30K MRR |
| Month 6 (~Dec) | Phase 4 nearing parity, mobile preview | $50K–100K MRR |

*(Revenue ramp is conservative and gated by go-to-market, not by engineering — the build timeline above is now far ahead of the original 36-month plan.)*

**Path to $50K MRR (achievable at 18 months):**
- 300 Pro subscribers at $79/month = $23,700
- 50 Enterprise at $499/month = $24,950
- 2 custom enterprise contracts at $2,500/month = $5,000
- **Total: ~$53,650 MRR**

---

## 8. Go-To-Market Strategy

### Phase 1: Developer Awareness (Months 1–6)

1. **Publish to crates.io** as `pdf-core` — be the obvious answer on the first Google search for "PDF Rust crate"
2. **npm package** for the WASM bridge — `@pdf-core/wasm`
3. **GitHub presence** — good README, working examples, benchmarks vs. Apryse WASM
4. **Cloudflare Workers template** — submit to their official templates directory
5. **HackerNews "Show HN" launch** when Phase 1 ships — the "WASM-native, runs on Cloudflare Workers" angle will get traction
6. **Dev.to / Hashnode posts** — "Why we rewrote our PDF processing in Rust" type content

### Phase 2: Ecosystem Integration (Months 6–12)

1. **LangChain integration** — submit a PR to LangChain's document loaders with our PDF parser
2. **LlamaIndex integration** — same
3. **Unstructured.io comparison post** — "Process PDFs client-side with zero API calls"
4. **Python package on PyPI** via PyO3 — `pip install pdf-core`
5. **Cloudflare blog post** — reach out to their developer relations team

### Phase 3: Enterprise Sales (Months 12–18)

1. **HIPAA compliance documentation** — formal writeup of our privacy architecture
2. **Healthcare conference presence** — HIMSS, HealthIT Summit
3. **Legal tech outreach** — Clio, MyCase, PracticePanther integrations
4. **SOC 2 Type I certification** — required for enterprise healthcare/finance
5. **Partner channel** — white-label licensing for document management SaaS companies

### Pricing Philosophy

- **No sales call required for Pro** — credit card, instant access
- **Free tier is genuinely useful** (not artificially crippled) — builds trust and word of mouth
- **Open source the parser + renderer** (MIT) — drives adoption and contributions
- **Charge for the editor, WASM bridge, and cloud features** — this is where business value is concentrated
- **Never lock data** — PDFs are open format, our tool doesn't add proprietary metadata

---

## 9. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|-----------|
| Apryse cuts price to compete | Medium | High | Our architectural advantage (WASM-native, edge) cannot be replicated quickly by a C++ codebase |
| PDF.js adds editing features | Low | High | Mozilla has never shown interest in editing; it's a viewer-only project by design |
| Adobe re-enters SDK market | Low | Very High | Adobe is focused on Document Cloud SaaS, not developer SDKs; their last SDK attempt (PDFL) died |
| WebAssembly performance insufficient for large documents | Medium | Medium | Mitigate with streaming/lazy parsing, tile caching (already implemented), and Web Workers for off-main-thread rendering |
| Funding required before revenue | High | Medium | Phase 1 is achievable solo with Claude Code; first revenue possible before funding needed |
| Rust WASM ecosystem fragility | Low | Low | wasm-bindgen is maintained by the Rust/WASM working group with Mozilla backing |
| iText builds a WASM version | Low | Medium | Their Java codebase cannot be compiled to WASM; they would need a full rewrite |

---

## 10. Conclusion

The PDF toolkit market has a genuine gap at the intersection of **WASM-native processing**, **privacy-first architecture**, and **developer-affordable pricing**. The two dominant players (Apryse and Nutrient) are C++ companies that cannot credibly claim to be WASM-first — their WASM builds are afterthoughts on top of a decade-old native codebase.

We are building from the ground up in pure Rust, targeting WASM as a first-class platform. This is not just a feature — it is an architectural bet that positions us correctly for where computing is going: browser-native, edge-distributed, privacy-preserving document processing.

The narrowest and most defensible path to revenue:

1. **WASM-first developer toolkit** — own the "PDF in WASM/browser" search result
2. **AI document processing pipeline** — Python package + LangChain integration
3. **Privacy-first for regulated industries** — HIPAA/GDPR compliance by architecture
4. **Cloudflare Workers ecosystem** — 4M developers with no PDF solution today

With Claude Code (Sonnet 4.6 primary, Opus 4.8 Fast Mode for T4 tasks) routed through FreeModel, **Phase 1 shipped in ~1 week** (2026-06-06) — all six demo-sprint features complete, with 4 of the 6 landing in a single day. At that demonstrated pace the remaining roadmap to full toolkit parity is **~9–15 weeks** and approximately **£45–70 (~$57–88) in actual subscription cost** via FreeModel — equivalent to ~$480–662 in direct Anthropic API face value (see §6.5 for detailed model/plan breakdown). Full parity by **~Sep 2026**. A product with real paying customers is realistic within **weeks, not years**, and the total build cost via FreeModel is a rounding error against the ~$360K an equivalent engineering team would cost — roughly **4,000–5,000× cheaper**.

The market is not saturated at the layer we're operating. The opportunity is real — and the execution risk that normally kills projects like this (multi-year build, large team, high burn) has been structurally removed.

---

*Report prepared: 2026-06-05*
*Revised: 2026-06-06 — Phase 1 marked complete; development-economics section added (Opus 4.8 token-cost model)*
*Revised: 2026-06-06 — Token-cost model corrected to 5–10M tokens/active-day (was ~1M/week output-only mis-count); three-way funding comparison added (Claude Pro subscription vs. $100/wk company API budget vs. full API unconstrained)*
*Revised: 2026-06-07 — §6.5 fully rewritten: corrected model (Sonnet 4.6 primary, not Opus 4.8), corrected throughput (2–5M/day), added FreeModel subscription billing analysis, added Opus 4.8 / Fast Mode comparison, corrected total project cost (was $1,100–2,200; actual out-of-pocket via FreeModel ~$65–90 remaining)*
*Revised: 2026-06-08 — §6.5 rewritten to token/cost only (pricing channel removed); added Sonnet 4.6 vs Opus 4.8 head-to-head comparison table; total project estimate ~170M tokens ($405–673 at standard API prices depending on model)*
*Based on: codebase analysis, competitive research, market data, observed FreeModel Pro billing (£5/month, api-cc.freemodel.dev)*
