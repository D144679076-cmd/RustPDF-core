# Edit-Text Format Apply (live preview) — Implementation Report

**Date:** 2026-06-05
**Scope:** Make panel formatting (colour / size / alignment / bold / italic / font-family) update the on-screen text **live** when applied to a selection.

## Problem
The full apply chain (panel → store wrapper → WASM setter → engine mutates the selected run's `CharStyle` → `textEditFormatTick` watch → `afterEngineEdit` → `scheduleGlyphRender` → `ensureGlyphLayer`) was wired correctly, but applying a format changed nothing on screen.

Two distinct causes:
1. **Glyph-render cache keyed by text only.** `ensureGlyphLayer`'s key was `block|text|scale|dpr`; a format change mutates run styles but not the text, so the key matched → cache hit → the WASM re-raster (which renders the styles) was skipped.
2. **Preview rendered every run in the block's original font.** `text_edit_render_block` borrows `&self` and couldn't embed a substitute face, so bold/italic/family never changed the preview *face* (they only applied on commit).

## What Was Implemented
### Step 1 — colour / size / alignment live (frontend)
- `web-editor/src/components/AnnotationOverlay.vue` — added `store.textEditFormatTick` to the `ensureGlyphLayer` cache key so a format change invalidates the cached bitmap and re-renders via the existing `preview_run_plan` (which emits per-run `rg`/`Tf` + alignment `Td`).
- Stripped the `[dragsel]` debug logs (selection confirmed working).

### Step 2 — bold / italic / font-family live (Rust/WASM)
- `src/wasm/text_edit.rs`:
  - `ActiveTextEdit.preview_fonts: HashMap<(family,bold,italic), (/EdN key, EmbeddedCidFont)>` — per-session cache of embedded substitute faces.
  - `prepare_preview_fonts` → `ensure_preview_fonts` (scan style runs, embed each needed `(family,bold,italic)` once via `embed_preview_font`) → `rebuild_edit_model_doc` (save_append + reparse so the preview doc resolves the new `/EdN` font + page `/Resources/Font`). Hooked into `text_edit_set_font` / `text_edit_toggle_bold` / `text_edit_toggle_italic`.
  - `embed_preview_font` (render-gated) reuses `EmbeddedFontResolver` + `embed_cidfont_for_chars` + `register_page_font`; non-render stub returns `Ok(false)`.
  - `preview_run_plan` now renders a bold/italic/family run with its cached embedded face (`/EdN` key + `embedded.encode`), falling back to the original font until the face is embedded.
- Rebuilt the WASM pkg (`make wasm`).

## Design Decisions
- **Embed on the format action, not in the render.** Keeps `text_edit_render_block` `&self` (no signature/store change, lower risk) and bounds cost: a `(family,bold,italic)` face is embedded once and the preview doc is rebuilt once per new variant — not per keystroke. The render just reads the cache.
- **Left the tested commit path (`commit_block_runs_impl`) untouched.** Lower regression risk; the trade-off is a commit may embed its own copy of a face the preview already embedded (a harmless orphan/duplicate font resource — a dedup is a later optimisation).

## Test Coverage
- Existing Rust suite green: `cargo fmt --check`, `cargo clippy --features wasm-render -D warnings`, `cargo test --features writer,render` → **567 passed, 5 ignored**; `cargo build --target wasm32-unknown-unknown` clean.
- Frontend: `quasar build` succeeds with the rebuilt pkg. (`vue-tsc` shows unrelated pre-existing errors from a concurrent `history`/`pushSnapshot` refactor — not from this change; the Vite build is transpile-only and unaffected.)

## Known Limitations / Follow-up
- **Possible duplicate embedded faces** (preview vs. commit) — unify via a shared cache to dedup.
- **Typed-from-scratch bold/italic** (toggle with no selection, then type) only embeds the face on a font-affecting setter call; the first such char may briefly render non-bold until a subsequent format action. Calling `prepare_preview_fonts` from `text_edit_insert` would close this (one rebuild per new variant).
- **Per-variant preview-doc rebuild cost** (save_append + reparse) — fine for occasional toggles; could be replaced by the doc `set_overrides` layer to avoid the reparse.
