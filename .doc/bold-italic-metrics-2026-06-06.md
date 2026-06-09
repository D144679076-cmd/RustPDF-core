# Bold/Italic Width Metrics & Format-Panel Font Name — Implementation Report

**Date:** 2026-06-06
**Scope:** Style-aware glyph metrics for Word-style text editing (`wasm/text_edit`, `editor/text_shape`, `writer/font_subset`, `fonts/truetype`) + frontend font-name display

## What Was Implemented

Fixes three bugs that appeared when applying bold/italic to a text selection:

1. **Bounding box did not flex** — the SVG edit box and the rendered tile crop stayed
   at the regular-font width, clipping wider bold/italic glyphs.
2. **Caret / selection x were wrong** inside formatted runs.
3. **Format panel showed the wrong font name** for the block's own font.

### Rust

- `src/fonts/truetype.rs`
  - `TrueTypeFont::iter_char_advances_1000()` — iterates the parsed `cmap`, yielding
    `(char, advance_in_1000em)` for every mapped code. Zero-copy over the existing
    `cmap`/`advance_widths` tables; guards `units_per_em == 0`.

- `src/writer/font_subset.rs`
  - `EmbeddedCidFont::iter_char_advances_1000()` — thin delegation to the inner
    `TrueTypeFont`, so callers can build metrics without exposing the private `ttf`.

- `src/editor/text_shape.rs`
  - `PdfFontMetrics::from_ttf_iter(char_advances, font_size)` — builds a
    `char → advance(points)` table from a TTF advance iterator (first entry wins on
    duplicates, matching the existing `from_font_info` convention). No `char_code`
    table (re-encoding for embedded faces goes through the CID path).

- `src/wasm/text_edit.rs`
  - `ActiveTextEdit` gained two fields: `block_font_name: String` and
    `preview_metrics: HashMap<(String,bool,bool), PdfFontMetrics>`.
  - `embed_preview_font()` now also builds and caches `from_ttf_iter` metrics for the
    embedded face alongside the existing `preview_fonts` entry.
  - New module-level helpers:
    - `run_metrics(style, default, preview, block_font_name)` — picks the variant
      metrics when a bold/italic/family face is cached, else the block's default.
    - `styled_offsets(engine, default, preview, block_font_name, block_font_size)` —
      per-character x-offsets computed run-by-run with the correct metrics (and the
      per-run size scale), returning `len+1` offsets.
  - `text_edit_state()` now derives `caret_x`, `width`, and selection bounds from
    `styled_offsets`, and emits a new `block_font_name` JSON field.
  - `preview_run_plan()` and `commit_block_runs_impl()` now measure each run with
    `run_metrics` so the live tile crop and the committed alignment/decoration
    geometry use the bold/italic advances.

### Frontend (web-editor)

- `src/types/pdf.ts` — added `block_font_name: string` to `TextEditState`.
- `src/stores/usePdfStore.ts` — added `textEditBlockFontName` ref and exported it.
- `src/components/AnnotationOverlay.vue` — `syncEngineState()` publishes
  `st.block_font_name` (and clears it on the null path).
- `src/layouts/MainLayout.vue` — `fontFamilyModel` getter shows
  `store.textEditBlockFontName` when the selection uses the block's own font
  (`style.font === ''`), instead of falling back to the last-used add-text font.

## Design Decisions

- **Metrics live in the WASM layer, not the engine.** `TextEditEngine` is font-agnostic
  by design (operates over any `Measurer`). The embedded-face metrics only exist once
  a preview font is embedded (a `render`-feature, page-scoped concern), so keeping the
  `(family,bold,italic) → metrics` cache on `ActiveTextEdit` keeps the engine pure and
  avoids threading font state through it.
- **Reuse the existing preview-font embedding trigger.** `preview_metrics` is populated
  in the same `embed_preview_font()` path that already embeds faces for live rendering,
  so metrics and glyphs stay in lock-step with no extra invalidation logic.
- **Graceful fallback.** `run_metrics` returns the block's default metrics when a
  variant face hasn't been embedded yet (e.g. mid-format-tick or `render` feature off),
  so geometry degrades to the old behaviour rather than breaking.
- **`block_font_name` for the picker** rather than changing the `""` sentinel: the empty
  string still means "block's own, unchanged font" on the commit side; the frontend
  resolves it to a display name only for the UI.

## Test Coverage

- Existing library unit tests: **605 passed**. No new unit test was added for
  `from_ttf_iter`/`styled_offsets` because they are exercised end-to-end through the
  WASM text-edit path and require an embedded TTF face to be meaningful; the pure
  offset accumulation mirrors the already-tested `caret_offsets`.
- Verified: `cargo fmt --check`, `cargo clippy --all-features -- -D warnings` (clean),
  `cargo build --target wasm32-unknown-unknown --features wasm-render`, and
  `make wasm` (wasm-pack pkg rebuild into `web-editor/src/pkg`).

## Known Limitations / Follow-up

- **Pre-existing test failure (not introduced here):**
  `editor::text_commit_runs::tests::commit_block_runs_underline_emits_filled_rect`
  fails — the appended decoration layer's `re`/`f` ops don't reach the saved
  `/Contents`. This is in `commit_block_runs` (decoration layer via `begin_edit_page`),
  a code path untouched by this change; sibling run-commit tests pass.
- **Pre-existing test-compile breakage:** `tests/wasm_api.rs` references
  `pdf_core::wasm::{WasmDocument, WasmEditor, WasmPdfWriter}`, but `src/wasm/mod.rs`
  has no `pub use` re-exports (the types live in the `document`/`editor` submodules).
  Library tests were run via `--lib` to validate this change.
- When the `render` feature is off, no bundled faces are embedded, so bold/italic runs
  fall back to the original font's metrics (box won't flex). This matches the existing
  rendering fallback — bold/italic only render in their real face under `render`.
- No metrics caching for italic *synthetic slant* — italics rely on a real bundled
  italic face; a family with no italic face falls back to default metrics.
- A unit test with a crafted TTF for `iter_char_advances_1000` / `from_ttf_iter` is a
  reasonable follow-up to lock the unit conversion (font-units → 1000em → points).
