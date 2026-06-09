# Text-Edit — Read Font/Size/Bold/Italic FROM the PDF + intrinsic-aware apply

**Date:** 2026-06-07
**Scope:** Reading intrinsic font style from the PDF and applying formatting relative to it (`content/interpreter`, `editor/text_model`, `editor/text_shape`, `editor/text_style`, `wasm/text_edit`, web-editor panel)

## Problem

The font picker showed the raw `/BaseFont` ("CIDFont+F2"), the size box was blank, and
toggling **B** lit the button but changed nothing. Root cause: the editor never read
the font's real style — `text_edit_open` seeded every char `bold=false,italic=false`, so
a bold title opened as "not bold" and the first B-toggle made the run *differ* from the
original, triggering a bundled-face swap that the CID re-encode couldn't satisfy → it
fell back to the original render (no visible change). Font family and size were likewise
taken raw/unscaled.

## What Was Implemented

### Reading (Rust)
- `content/interpreter.rs`: `resolve_font_style()` parses `/BaseFont` + `/FontDescriptor`
  (`/Flags` ForceBold 1<<18 / Italic 1<<6, `/FontWeight`≥700, `/ItalicAngle`≠0, plus
  BaseFont name hints), descending `DescendantFonts[0]` for Type0. `strip_subset_prefix()`
  drops a `XXXXXX+` subset tag for display. Both `pub(crate)` with `log::debug!`.
- `editor/text_shape.rs`: `font_style_for()` (mirrors `font_metrics_for`, reusing
  `page_resources`).
- `editor/text_model.rs`: `EditBlock` gained `bold`, `italic`, `display_font`; populated
  per resource key (cached) in `build_text_model`; `font_name` now prefers the descriptor
  `/BaseFont`. `blocks_to_json` emits the new fields.

### Seeding + reporting (Rust)
- `text_style.rs`: `CharStyle::from_block_styled(size, bold, italic)`.
- `wasm/text_edit.rs`: `ActiveTextEdit` gained `display_font`, `orig_bold`, `orig_italic`;
  `text_edit_open` seeds the engine with the intrinsic style; `text_edit_state` reports
  `block_font_name = display_font` and the seeded bold/italic flow through `active_style()`.

### Intrinsic-aware apply — the core fix (Rust)
- `run_needs_substitute(style, orig_bold, orig_italic)` = chosen family OR
  bold/italic differing from the font's intrinsic style. Applied in
  `commit_block_runs_impl`, `preview_run_plan`, `ensure_preview_fonts`, and `run_metrics`
  (replacing the old `!bold && !italic` / `needs_embedded_font()` checks). An already-bold
  title now renders with its **own** glyphs (no swap); toggling weight/slant genuinely
  differs → substitutes a bundled face and visibly changes. `preview_run_plan`'s "plain"
  check compares against the styled seed.

### Font size (Rust + frontend)
- The panel works in **visual** points: `text_edit_state` reports `font_size`/`ascent`/
  `descent` and `style.size` scaled by the text→page scale (`scale_x`, a uniform-matrix
  proxy); `text_edit_set_size` divides back to text space. Caret/width geometry is
  unchanged.
- `MainLayout.vue`: `fontSizeModel.get` rounds to 0.1 so it matches a `fontSizeOptions`
  entry (no blank `<select>`); `fontFamilyOptions` lists the available fonts + the block's
  current font.

### Unblock (unrelated, per user approval)
- A half-finished `OutputDevice::draw_image_xobject` refactor (new `Option<u32> obj_id`)
  broke `--no-default-features`. Behaviour-preserving completion: the two impls and the
  interpreter test device accept `_obj_id`; the call site passes `None` (every impl
  ignores it today). No behaviour change; the encrypted-image path can wire the real id
  later.

## Test Coverage
- `strip_subset_prefix_removes_valid_tag` / `_keeps_non_tag_names`.
- `resolve_font_style_reads_forcebold_flag` / `_reads_italic_angle` / `_plain_is_neither`
  / `_name_hint_bold` (crafted Type1 font + FontDescriptor).
- `run_needs_substitute` truth table (matches intrinsic → keep; differing weight/slant or
  chosen family → substitute).
- Full lib suite: **625 passed (wasm-render), 281 (default), 1 ignored**.
- Verified: `cargo fmt --check`, `cargo clippy --all-features -D warnings`,
  `cargo build --target wasm32-unknown-unknown`, `make wasm`, `vue-tsc --noEmit`.

## Known Limitations / Follow-up
- CID subset fonts with no family name display the cleaned BaseFont (e.g. "CIDFont+F2").
- Un-bolding/italicising a CID font swaps to a bundled face (its real family is unknown),
  so the typeface changes — expected.
- Visual size uses `scale_x` as the vertical-scale proxy (text matrices are ~uniform); the
  added `log::debug!` for `font_size/scale_x/visual_size` lets us confirm per-PDF and add a
  true `scale_y` if a non-uniform case shows up.
- Underline can't be read back from a PDF (it's drawn geometry, not a text property);
  seeded off. Applying underline works via decorations (prior round).
- The pre-existing `commit_block_runs_underline_emits_filled_rect` test is `#[ignore]`d in
  the tree (commit-time decoration layer); unrelated to this change.
