# Write-Back Tier 1 (surgical commit, existing glyphs) — Implementation Report

**Date:** 2026-06-02
**Scope:** editor::text_encode, editor::text_commit, wasm::text_edit (text_edit_commit),
web commit path

## Problem
Editing previewed correctly (Phase 0) but **saving was broken**: commit went through
`patch_frame` → `encode_pdf_string` (Latin-1/UTF-16BE). For this 100%-CID document
those bytes are not the font's CIDs, so saved edits were garbage on reopen.

## What Was Implemented
- **`editor/text_encode.rs` (NEW)** — `encode_in_font(doc, page, font_key, font_size,
  text) -> EncodeResult { bytes, missing }`. Simple fonts → 1-byte via
  `PdfFontMetrics::code_for_char` (reverse Encoding); composite/Type0 → 2-byte CID via
  `CMap::unicode_to_code` (inverted ToUnicode). Unencodable chars are reported in
  `missing` (deduped) instead of mis-encoded. Promotes/dedups the preview's
  `encode_for_block` logic into one shared, tested fn.
- **`editor/text_commit.rs` (NEW)** — `commit_block(editor, model, page_index,
  block_id, bytes)`: surgically replaces only the block's show op(s) in the parsed
  `OpStream` (first frame carries the bytes as one `Tj`, rest blanked; positioning/
  font ops untouched), then reuses `commit_edit_session` to serialize all streams and
  point the page/XObject at the new content via `PdfEditor` CoW. Untouched ops
  re-serialize unchanged. Returns `InvalidStructure` on unknown id / no show op.
- **`wasm/text_edit.rs`** — `WasmEditor::text_edit_commit(block_id) -> String`
  (`{"committed":bool,"missing":"…"}`): encodes the open engine's text via
  `encode_in_font`; if complete, calls `commit_block` and reports `committed:true`;
  else returns the missing chars (no write). Uses disjoint borrows of `editor` +
  `text_edit_model`.
- **Web** — store `textEditCommit(blockId)` (snapshots undo, parses result,
  `refreshDoc` on success); `commitBlockEdit` now calls it instead of the per-frame
  `commitTextEdit` loop. On `committed:false` it shows a Quasar warning listing the
  missing characters and falls back to cover-and-redraw (`replaceText`).

## Design Decisions
- **Surgical op-range replace** (user choice): minimal diff, preserves the rest of the
  content stream byte-for-byte; reuses `EditBlock.op_range`/`frame_ids` +
  `serialize_operations` + `commit_edit_session` rather than a new writer.
- **`missing` instead of silent mis-encode**: a glyph absent from the font must not be
  written as wrong bytes. The list drives the Tier-2/3 font work and a clear user
  notice now.
- **Reuse `commit_edit_session`**: it already serializes every stream and does the CoW
  page/XObject swap; `commit_block` only patches the ops first, so unchanged streams
  round-trip identically and `save_append` produces a valid incremental update.

## Test Coverage
- `text_encode`: `encode_result_complete_when_no_missing`,
  `encode_result_incomplete_with_missing`.
- `text_commit`: `commit_block_replaces_show_text_simple_font` (build model → commit
  "World" → `save_append` → assert `(World)` in saved bytes — full round-trip),
  `commit_block_unknown_id_errors`.
- Full suite: 523 passed / 0 failed (`--features "render writer"`). fmt clean; clippy
  `-D warnings` clean for `writer` and `wasm-render`. `make` rebuilt the pkg
  (`text_edit_commit` present); web `tsc` clean.

## Known Limitations / Follow-up
- **Missing glyphs** (typed char not in the embedded font) are NOT yet saveable —
  commit returns `committed:false` and the host cover-redraws in a substitute font.
  Tier 2 (borrow a sibling embedded font) and Tier 3 (`writer/font_subset.rs` — embed
  a CID subset from `core-fonts/`) close this.
- Multi-frame blocks collapse to a single `Tj` (kerning from a `TJ` array is lost on
  commit) — acceptable for edited text; original untouched blocks keep their `TJ`.
- Browser end-to-end (edit → save → reopen) still to be eyeballed by the user.
