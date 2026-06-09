# Rich-Text Formatting (edit-text panel) — Implementation Report

**Date:** 2026-06-04
**Scope:** Per-selection text formatting (colour, font, size, bold/italic, underline/strikethrough, alignment) for Word-style PDF text editing, persisted into the saved PDF.

## What Was Implemented

### Rust core (`pdf-editor-rust-core`)
- **New `src/editor/text_style.rs`** — per-character style model:
  - `CharStyle { color, font: FontChoice, font_size, bold, italic, underline, strike }`,
    `enum FontChoice { Original, Family(String) }`, `enum Align { Left, Center, Right }`,
    `StyleRun { start, end, style }`, `ActiveStyle` (per-field `Option`, `None` = mixed).
  - `CharStyle::from_block`, `CharStyle::needs_embedded_font`, decoration geometry helpers
    (`decoration_thickness`, `underline_offset`, `strike_offset`).
- **`src/editor/text_edit_engine.rs`** — extended `TextEditEngine` with a `styles: Vec<CharStyle>`
  kept length-locked with `chars` (invariant asserted via `debug_assert`), a `typing_style`, and a
  block-level `align`. New API: `new_styled`, `apply_color`, `set_font`, `set_size`,
  `toggle_bold/italic/underline/strike`, `set_align`/`align`, `style_runs`, `active_style`. Every
  splice op (`insert`/`delete_back`/`delete_forward`/`delete_selection`) mirrors onto `styles`;
  caret-collapsing moves recompute `typing_style` from the char to the left.
- **New `src/editor/text_commit_runs.rs`** — multi-run write-back:
  - `build_run_ops(runs, align_dx) -> Vec<Operation>` (pure): emits per-run `rg`/`Tf`/`Tj`
    (coalescing unchanged colour/font) + optional leading alignment `Td`.
  - `commit_block_runs(...)`: splices the run ops in place of the block's primary show op (drops the
    rest), commits via `commit_edit_session`, then draws underline/strike as filled rects in an
    appended content layer. `ResolvedRun`, `DecoRect` types.
- **`src/editor/text_commit.rs`** — `register_page_font` promoted to `pub` (shared with the runs path).
- **`src/wasm/text_edit.rs`** — new `#[wasm_bindgen]` setters (`text_edit_apply_color`,
  `text_edit_set_font`, `text_edit_set_size`, `text_edit_toggle_bold/italic/underline/strike`,
  `text_edit_set_align`); `text_edit_state` JSON extended with a `"style"` object (`style_to_json`);
  `text_edit_open` seeds the engine via `new_styled`; `text_edit_commit` gained a **rich-text branch**
  (`commit_block_runs_impl`) that resolves each run's font (original via `encode_in_font`, or a
  bundled embedded font via `embed_cidfont_for_chars` + `register_page_font`, cached per
  family/bold/italic), measures widths, computes alignment + decoration rects, and commits; falls
  back to the plain/Tier-3 path when a run can't be resolved. `text_edit_render_block` consumes a new
  `preview_run_plan` for live colour/size/alignment.

### Frontend (`web-editor`)
- `src/types/pdf.ts` — `TextEditStyle` interface + `style` field on `TextEditState`.
- `src/stores/usePdfStore.ts` — wrappers (`textEditApplyColor/SetFont/SetSize/ToggleBold/…/SetAlign`),
  a shared `textEditActiveStyle` ref (selection's resolved style for the panel) and a
  `textEditFormatTick` counter.
- `src/components/AnnotationOverlay.vue` — `syncEngineState` publishes `style` to the store; a watcher
  on `textEditFormatTick` re-syncs + re-rasters the preview after a panel action; `cancelBlockEdit`
  clears the active style.
- `src/layouts/MainLayout.vue` — the Text Styles panel now drives the engine while a block is open and
  reflects the selection's resolved style (mixed-aware); Underline/Strikethrough enabled during edit;
  falls back to `store.textStyle` for the "Add Text" tool.
- Rebuilt the WASM pkg into `web-editor/src/pkg` (`make wasm`, target `wasm-render`).

## Design Decisions
- **Parallel `Vec<CharStyle>` over an interval list** — the engine is splice-based and index-addressed,
  so mirroring each splice keeps styles in sync by construction; runs are coalesced as a view only at
  commit/preview/state time. Buffers are single-line, so O(n) splices are negligible.
- **Resolution split from emission** — encoding original-font runs borrows the doc immutably while
  embedding needs `&mut PdfEditor`; the two can't co-borrow, so the WASM layer resolves runs first
  (drop doc borrow → embed) and hands ready-made `ResolvedRun`s to `commit_block_runs`.
- **No inter-run positioning** — PDF advances the text cursor by each run's glyph widths, so the run
  sequence needs only `rg`/`Tf`/`Tj`; the original `Tm`/`Td` origin is preserved.
- **Decorations as an appended layer** — underline/strike are filled rects in a fresh content stream
  at identity CTM (MediaBox space), matching the space `block.x/y` are reported in; avoids illegal
  path ops inside `BT…ET`.
- **Per-run missing-glyph fallback** — a run whose glyph is absent from the original font is embedded
  individually (strictly more capable than the old whole-block fallback).

## Test Coverage
- `text_style`: `from_block_*`, `needs_embedded_font_*`, `align_round_trips_through_str`,
  `decoration_geometry_scales_with_size`.
- `text_edit_engine` (style-sync): `styles_stay_length_locked_through_edits`,
  `apply_color_only_colors_selection`, `insert_inherits_typing_style_after_format`,
  `caret_into_plain_text_types_plain`, `toggle_bold_clears_when_uniform_sets_when_mixed`,
  `active_style_reports_mixed_as_none`, `active_style_no_selection_reports_typing_style`,
  `style_runs_empty_buffer_is_empty`, `selection_insert_replaces_styles_with_typing`.
- `text_commit_runs`: `build_run_ops_single_run`, `…_two_runs_emit_color_change_between`,
  `…_alignment_prepends_td`, `commit_block_runs_two_run_block_saves_expected_sequence`,
  `…_underline_emits_filled_rect`, `…_unknown_id_errors`,
  `commit_block_runs_embedded_bold_run_persists` (full embed + multi-run + save round-trip,
  render-gated).
- Full suite: `cargo fmt --check`, `cargo clippy --features wasm-render -D warnings`,
  `cargo test --features writer,render` → **555 passed, 5 ignored**;
  `cargo build --target wasm32-unknown-unknown --features wasm-render` clean;
  web-editor `quasar build` succeeds; `vue-tsc` clean on all changed files.

## Known Limitations / Follow-up
- **Live preview shows colour/size/alignment only.** A font-family or bold/italic swap and
  underline/strikethrough appear on **commit**, not in the live tile — `text_edit_render_block`
  borrows `&self` and can neither embed fonts nor open a decoration layer. Follow-up: a `&mut` preview
  path with a cached preview font + transient resource injection.
- **`edit → commit → reopen` is not round-trip-stable for multi-font blocks.** `group_blocks` splits on
  font-key change, so a committed multi-font block reopens as several blocks. Correct on screen and on
  save; a marked-content `/Span` marker to re-group is a deferred option.
- **Untouched runs commit as `0 0 0 rg`** — the block model carries no incoming colour, so opening
  defaults to black; non-black/gray/CMYK original text would darken if recommitted. Follow-up: parse
  the block's incoming fill into `CharStyle`.
- **Alignment/decoration assume axis-aligned horizontal text** (single-line engine); the alignment
  shift is clamped + warns when it would push the origin off-page. Rotated/sheared blocks are out of
  scope.
- **Link** (annotation over the selection) is **not** implemented — it is an annotation feature
  orthogonal to text formatting; the Link button stays disabled. Deferred follow-up.
