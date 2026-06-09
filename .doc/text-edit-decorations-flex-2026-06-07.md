# Text-Edit Round 2 — Font Recognition, Live Underline & Flex Height — Implementation Report

**Date:** 2026-06-07
**Scope:** Word-style text editor — formatting recognition, live decorations, vertical flex (`wasm/text_edit`, `wasm/mod`, `render/font_resolver`, `editor/text_commit_runs`, web-editor)

## What Was Implemented

Follows the width-metrics fix. Addresses four remaining complaints: font/size not
recognized in the panel, bold/italic/underline not applying immediately (underline
never at all), and the box not flexing height for larger fonts / underlines.

### Rust
- `render/font_resolver.rs` — `EMBEDDED_FONT_FAMILIES` const: the font families the
  embedded (WASM) resolver can actually render (Liberation Sans/Serif/Mono aliases +
  DejaVu Sans).
- `wasm/mod.rs` — `available_fonts()` WASM export returning that list as JSON
  (`render` on) or the 3 base-14 names (`render` off).
- `editor/text_commit_runs.rs` — `build_decoration_ops(&[DecoRect]) -> Vec<Operation>`:
  inline `q/rg/re/f/Q` per rect, in page user-space. Reused by the live preview.
- `wasm/text_edit.rs`:
  - `PreviewPlan` gained `decorations: Vec<DecoRect>`; `preview_run_plan()` now computes
    underline/strike rects with the SAME geometry as `commit_block_runs_impl`
    (per-run width via `run_metrics`, `underline_offset`/`strike_offset`/
    `decoration_thickness`).
  - `text_edit_render_block()` appends `build_decoration_ops(&p.decorations)` to the
    preview content (after the text, outside `BT…ET`) so decorations show live.
  - `text_edit_state()` now reports `ascent`/`descent` (page-space points) sized from
    the largest run font (matches the render tile’s `box_fs` factors).

### Frontend (web-editor)
- `stores/usePdfStore.ts` — `availableFonts` ref + `loadAvailableFonts()` (lazy import
  of `available_fonts()`), exported.
- `layouts/MainLayout.vue` — font/size `<select>`s now `v-for` over `fontFamilyOptions`
  (all available fonts + the block’s current font) and `fontSizeOptions` (defaults +
  current size), so a "Calibri"/11pt block displays correctly. `loadAvailableFonts()`
  called on mount.
- `types/pdf.ts` — `TextEditState` gained `ascent`/`descent`.
- `components/AnnotationOverlay.vue`:
  - `liveAscentPts`/`liveDescentPts` refs set in `syncEngineState()`.
  - `editBoxH`/`editBoxTop` computeds (fallback to the static box pre-sync).
  - White cover now fills the FULL cleared vertical range (fixes original page text
    bleeding through behind larger/underlined text); dashed SVG box, caret, and
    selection use the live height/top.
  - Format-tick watch renders EAGERLY (`runGlyphRender()` not the rAF schedule), so
    B/I/U/colour/size apply on the same click.

## Design Decisions
- **Rust is the source of truth for available fonts.** The picker only offers faces the
  WASM build can render; native-only directory fonts (186) are intentionally excluded
  since they don’t exist in the browser.
- **Decorations reuse commit geometry.** `build_decoration_ops` + the shared
  `underline_offset`/`strike_offset`/`decoration_thickness` keep live preview and saved
  output pixel-consistent; appended outside `BT…ET` in page space so the tile CTM maps
  them like commit’s separate layer.
- **`descent = 0.30·fs`** already exceeds the underline depth (`0.12·fs` offset +
  `0.05·fs` thickness ≈ `0.17·fs`), so no separate underline allowance is needed.
- **Cover fills the cleared range** rather than tracking exact glyph extents — simplest
  robust fix for the bleed-through, using bounds already maintained for `clearRect`.

## Test Coverage
- `build_decoration_ops_empty_is_empty`, `build_decoration_ops_emits_q_rg_re_f_q_per_rect`
  (text_commit_runs) — happy path + empty path for the new op builder.
- Full lib suite under `--features wasm-render`: **614 passed**, 2 new.
- Verified: `cargo fmt --check`, `cargo clippy --all-features -D warnings` (clean),
  `cargo build --target wasm32-unknown-unknown --features wasm-render`, `make wasm`,
  `vue-tsc --noEmit` (no errors in changed files).

## Known Limitations / Follow-up
- **Pre-existing failure (untouched):** `commit_block_runs_underline_emits_filled_rect`
  — the commit-time appended decoration layer doesn’t reach saved `/Contents`. The live
  preview path added here is independent and works; the commit-layer bug predates this
  change and remains open.
- Several picker fonts map to the same Liberation face (Arial≈Helvetica, Times New
  Roman≈Times-Roman); listed by familiar name even though they render identically.
- Height grows around the baseline, so a font far larger than the original line spacing
  visually overlaps neighbouring lines while editing — expected for an edit overlay.
- Runtime-registered host fonts (`register_font`) are not yet enumerated by
  `available_fonts()`.
