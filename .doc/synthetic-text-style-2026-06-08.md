# Synthetic bold/italic + decoupled decorations — Implementation Report

**Date:** 2026-06-08
**Scope:** rich-text formatting on the original embedded font (`editor/text_style`, `editor/text_commit_runs`, `editor/edit_session`, `editor/text_model`, `wasm/text_edit`, `render/page_renderer`, `content/text_state`, `content/interpreter`)

## Problem

Formatting a block whose font is an embedded CID font (e.g. `CIDFont+F2`, detected as bold) was broken:
- **Bold OFF broke the font** — the run was queued for a bundled substitute; the resolver's final fallback arm is `_ => DEJAVU_SANS`, so the text silently swapped to DejaVu Sans (losing CJK/Vietnamese glyphs).
- **Italic/underline never applied** — the substitute (`"CIDFont+F2-Italic"`) didn't exist as a bundled face; the embed produced DejaVu, and for non-Latin glyphs `can_encode` failed → `preview_run_plan` returned `None` / commit returned `Ok(false)`, dropping the per-run plan *and all decorations*.

Only bundled families (Helvetica/Times) worked, because those have real bold/italic faces.

## What Was Implemented

### Content model
- **`editor/text_style.rs`** — new `SyntheticStyle { bold, italic }` + `run_synthetic_style(style, orig_bold, orig_italic)`: an `Original`-font run fakes only the styling the embedded font lacks (bold when `!orig_bold`, italic when `!orig_italic`); bold/italic *off* on an intrinsically-styled font yields nothing (keep glyphs). `Family` runs never synthetic. Constants `OBLIQUE_SHEAR = 0.213` (tan 12°), `SYNTHETIC_BOLD_STROKE_FRAC = 0.03`.
- **`editor/text_commit_runs.rs`** — `ResolvedRun` gained `synthetic: SyntheticStyle`; new `RunLayout { tm: [f64;6], run_x_text: Vec<f64> }`. `build_run_ops` rewritten: synthetic-bold runs emit `RG`/`2 Tr`/`w` (stroke colour matched to fill, line width ∝ size) with trailing `0 Tr`/`0 w` reset; when any run is synthetic-italic, each run is positioned with an absolute `Tm` derived from the block text matrix (`Tm·[1 0 shear 1]` folds the shear into `c`,`d`; advances via `tm.a`/`tm.b`, so it composes with any CTM and needs no rotation guard).
- **Block text matrix capture** — `edit_session.rs` `RawFrame`/`EditableFrame` carry the pre-CTM `tm`; `text_model.rs` `EditBlock` gained `tm` (primary frame's matrix). This is the single source of truth for the per-run `Tm`.

### WASM orchestration (`wasm/text_edit.rs`)
- `commit_block_runs_impl`: an `Original`-font run **always** keeps the original font (`encode_in_font`) regardless of bold/italic, recording `synthetic`; only `Family` runs embed. Builds `RunLayout` and passes it to `build_run_ops`. Kills the DejaVu swap on bold-off.
- `preview_run_plan`: `Original` runs skip the preview-font cache (always original font + synthetic) so decorations are always produced; only `Family` runs consult `preview_fonts`. Builds `RunLayout`.
- `ensure_preview_fonts`: narrowed to embed substitutes for `Family` runs only.

### Renderer (`render/page_renderer.rs`, `content/*`)
- **Synthetic italic preview** — `draw_text_span` now reads the render-matrix `c` term (was discarded) and routes the non-rotated path through new `blit_glyph_sheared` (tiny-skia `Transform::from_row(1,0,−skew,1,…)`), so the live tile slants to match the sheared `Tm` written to the PDF.
- **Synthetic bold preview** — `TextSpan` gained `stroke_text` (from `render_mode.strokes()`); when set, `embolden_glyph` (box max-filter) thickens the glyph mask before blit. Approximation; the saved PDF uses real `2 Tr`/`w`.

## Design Decisions
- **Synthetic on original glyphs, never DejaVu swap** (user-confirmed): preserves Vietnamese diacritics / CJK. Bold/italic-off on an intrinsically-styled font is a deliberate no-op (can't thin/un-slant an embedded face) rather than a breaking substitution.
- **Per-run absolute `Tm` only when italic is present** — keeps the common (non-italic) path on PDF auto-advance, zero risk to existing blocks. Using the captured pre-CTM `tm` (not reconstructing from page-space x/y) makes the shear correct under arbitrary CTM and rotation.
- **Decorations decoupled** — original-font runs can no longer fail resolution, so underline/strike rects always reach the plan/commit.

## Test Coverage
- `text_style.rs`: `synthetic_style_bold_on_regular_and_off_on_bold`, `synthetic_style_italic_on_upright_only`, `synthetic_style_family_uses_real_face_not_synthetic`.
- `text_commit_runs.rs`: `build_run_ops_synthetic_italic_emits_skew_tm`, `build_run_ops_synthetic_bold_emits_tr2_and_w_then_resets`, `build_run_ops_mixed_runs_isolate_state`, `build_run_ops_synthetic_keeps_original_font_key` (+ existing run-ops tests updated for the new signature).
- `page_renderer.rs`: `embolden_glyph_thickens_isolated_pixel`, `blit_glyph_sheared_leans_top_right` (asserts top rows paint further right than bottom — verifies shear sign).
- `wasm/text_edit.rs`: `commit_italic_underline_keeps_original_font_and_commits` (fixture `Group-3.pdf`: select-all → italic + underline → commit returns `committed:true` and the saved page still selects the original font key).

Gates: `cargo fmt --check`, `cargo clippy --features writer,render,wasm` (clean), `cargo test --features writer,render` (631 pass), `cargo test --lib --features writer,render,wasm` (642 pass), `cargo build --target wasm32-unknown-unknown`, `make wasm` (pkg has default `init` export).

## Known Limitations / Follow-up
- Synthetic-bold preview is a mask dilation (visual approximation); the saved `2 Tr`/`w` is exact in compliant viewers.
- Decoration layer is still dropped by the single-stream `commit_edit_session` flush (pre-existing TODO in `text_commit_runs.rs`) — underline/strike show in the live preview but may not survive a full save+reopen until that flush preserves appended layers.
- Web panel (`MainLayout.vue`): Underline/Strike buttons are `:disabled="!editing"` and lack the Add-Text fallback that Bold/Italic have (only affects "Add Text" mode, not editing an existing block) — separate, deferred.
- `tests/wasm_api.rs` still doesn't compile under `--features wasm` (pre-existing; `src/wasm/mod.rs` dropped the `WasmDocument`/`WasmEditor`/`WasmPdfWriter` re-exports). Lib tests run via `--lib`.
