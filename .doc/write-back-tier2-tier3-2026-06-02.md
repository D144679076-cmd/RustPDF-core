# Write-Back Tier 2 + Tier 3 (missing-glyph font handling) — Implementation Report

**Date:** 2026-06-02
**Scope:** editor::text_encode (Tier-2 embedded-program recovery),
writer::font_subset (NEW), editor::text_commit (commit_block_with_font /
register_page_font), wasm::text_edit (commit_block_embed_fallback)

## Problem
Tier 1 made edits saveable **only when every typed glyph already has a code in the
block's font**. A character that is not in the font's ToUnicode map returned
`committed:false` and the host cover-redrew in a substitute face — the edit did not
persist. Two cases remained:

1. The glyph **is** in the embedded font program but not exposed by the subset's
   ToUnicode CMap (very common: an Identity-H subset whose `cmap` carries far more
   glyphs than its `/ToUnicode` lists).
2. The glyph is in **no** embedded font at all (a brand-new character) and needs a
   font embedded from the bundled set.

Tier 2 closes (1) with zero new font bytes; Tier 3 closes (2) by embedding a
Type0/CIDFontType2 font.

## What Was Implemented

### Tier 2 — recover a glyph from the embedded program (no new bytes)
- **`editor/text_encode.rs`** — in the composite branch of `encode_in_font`, after
  the ToUnicode reverse-map lookup fails, fall back (for **Identity-H/V** fonts only,
  where CID == GID) to the embedded TrueType program's own `cmap`: `unicode → GID`,
  and use that GID directly as the 2-byte CID. New helpers:
  - `font_dict(doc, resources, key)` — resolve `/Font/<key>` to its dict.
  - `is_identity_h(doc, resources, key)` — gate the fallback to Identity-H/V.
  - `embedded_truetype_program(doc, resources, key)` — Type0 →
    `DescendantFonts[0]` → `FontDescriptor` → `FontFile2`, decoded and parsed via
    `TrueTypeFont::parse` (returns `None` for CFF-only/no-program fonts → Tier 3).
  The program is parsed **once** per commit, before the per-char loop.

### Tier 3 — embed a bundled font for truly-new glyphs
- **`writer/font_subset.rs` (NEW)** —
  - `EmbeddedCidFont { font_id, ttf }` with `can_encode(ch)` (real, non-`.notdef`
    glyph?) and `encode(text) -> Vec<u8>` (2-byte Identity-H GIDs, big-endian, never
    panics — unmapped chars → GID 0).
  - `embed_cidfont_for_chars(editor, ttf_bytes, base_name, chars) ->
    Result<EmbeddedCidFont>`: embeds the program **whole** as `/FontFile2` (flate,
    `/Length1` = raw size) with `CIDToGIDMap /Identity` (CID == source GID), builds a
    `FontDescriptor`, a `CIDFontType2` descendant (`DW 1000`, per-GID `/W` from
    `ttf.glyph_advance(gid)*1000/upm` for the used glyphs, sorted/deduped via a
    `BTreeMap`), a `ToUnicode` CMap, and the `Type0` wrapper (`Identity-H`). Returns
    the `Type0` object id.
  - `build_tounicode(ttf, chars)` — GID→unicode `beginbfchar` CMap (UTF-16BE hex,
    chunked at the 100-entry spec cap, smallest char wins on GID collision).
  - `utf16be_hex(ch)` — 4 hex for BMP, 8 for an astral surrogate pair.
- **`editor/text_commit.rs`** —
  - `commit_block_with_font(editor, model, page_index, block_id, font, text)`:
    registers the embedded font in the page's `/Resources/Font`, encodes the text via
    `font.encode`, surgically replaces the block's show op(s) (primary carries the
    bytes, rest blanked — same shape as `commit_block`), and **inserts** a
    `/<key> <size> Tf` operator immediately before the primary show op so the block
    renders with the embedded font. Restricted to the page content stream
    (`stream_idx == 0`); XObject-local resources are out of scope for this fallback.
  - `register_page_font(editor, page_index, font_id) -> String`: adds the font under
    a fresh `/EdN` key (reusing an existing entry that already points at `font_id`,
    so repeated edits of one block are idempotent).
- **`wasm/text_edit.rs`** — `text_edit_commit` now, on `!enc.is_complete()`, calls
  `commit_block_embed_fallback` (instead of immediately reporting missing):
  - `#[cfg(feature = "render")]` — `normalize_font_name(base_font)` → resolve a
    bundled font via `EmbeddedFontResolver.resolve(base_font, bold, italic)`, embed it
    over the **whole** block text with `embed_cidfont_for_chars`, verify it covers
    every non-whitespace char (`can_encode`) — if not, `Ok(false)` rather than a
    half-embed — then `commit_block_with_font`. Returns `committed:true` on success.
  - `#[cfg(not(feature = "render"))]` — `Ok(false)` (bundled resolver is render-only).
  On `Ok(false)`/error the host still gets `{committed:false,missing:"…"}`.

## Design Decisions
- **Tier 2 before Tier 3 (cheap win first):** an Identity-H subset's program usually
  contains the glyph already, so recovering its GID costs zero new font bytes and
  keeps the original face. Only when the program genuinely lacks the glyph (or is
  CFF-only / not embedded) do we pay for Tier 3.
- **Embed the program *whole*, not a glyf/loca subset:** `CIDToGIDMap /Identity`
  makes the CID the source GID directly — always correct, no subsetter to get wrong.
  Larger output is the explicit, accepted trade-off in the plan; a true subset is
  follow-up.
- **Retarget the whole block to the embedded font, covering all its chars:** simpler
  and always-correct vs. run-splitting a single block across two fonts. We require
  the bundled font to cover the entire block (`can_encode` for every non-space char)
  before committing, so we never emit a block that's half-embedded / half-`.notdef`.
- **New `Tf` *inserted*, original `Tf` untouched:** this document has one `Tf` per
  `BT…ET`, so prepending `/<EdN> <size> Tf` before the block's show op switches only
  that run; the rest of the page keeps its original font. Page-content-only for now.
- **Reuse `commit_edit_session` + CoW:** both commit paths patch the parsed ops then
  reuse the existing serialize/CoW plumbing, so untouched streams round-trip
  byte-identically and `save_append` writes a valid incremental update.

## Test Coverage
- `writer/font_subset` (`#[cfg(all(test, feature = "render"))]`, needs the bundled
  resolver):
  - `embed_builds_type0_font_and_encodes` — embed Liberation Serif over "Hello";
    assert the registered object is `Type0` + `Identity-H` with `DescendantFonts` and
    `ToUnicode`, and that `encode("Hello")` is 10 bytes with a non-zero GID for 'H'.
  - `utf16be_hex_bmp_and_astral` — `'A'→0041`, `'好'→597D`, `'\u{1F600}'→D83DDE00`.
- `editor/text_commit`:
  - `commit_block_with_font_embeds_and_retargets` (`#[cfg(feature = "render")]`) —
    embed → retarget a block → `save_append` → assert `/Type0`, `/Identity-H`, and the
    new `/Ed0` resource key appear in the saved bytes (full round-trip).
  - (Tier-1 tests retained: `commit_block_replaces_show_text_simple_font`,
    `commit_block_unknown_id_errors`.)
- Tier-2 recovery is exercised end-to-end through `encode_in_font` on the real
  Group-3 CID fixture in the browser path; the unit-level `EncodeResult` invariants
  (`encode_result_complete_when_no_missing` / `_incomplete_with_missing`) are covered.
- **Gate:** `cargo fmt --check` clean; `cargo clippy --features writer -D warnings`
  and `--target wasm32-unknown-unknown --features wasm-render -D warnings` both clean;
  `cargo test --features "render writer"` → **526 passed / 0 failed** / 5 ignored;
  `make wasm` rebuilt `web-editor/src/pkg` (`wasm-render`, `wasm-opt` ok); web
  `vue-tsc` clean for all touched files (only pre-existing, unrelated
  `VersionHistoryPanel.vue` directive warnings under the npx-pulled TS 3.3.3).

## Known Limitations / Follow-up
- **Whole-program embed (no subsetting):** correct but larger than a glyf/loca subset.
  A true TrueType subsetter (and CFF subsetting) is deferred.
- **CFF-only composite fonts:** Tier 2 only recovers from `FontFile2` (TrueType); a
  CFF/`FontFile3` program won't be mined, so such glyphs go straight to Tier 3.
- **Tier 3 retargets the whole block to one bundled face:** if the bundled font
  doesn't cover *every* char in the block it bails (`committed:false`) rather than
  mixing fonts within the block. Per-run/per-glyph font splitting is follow-up.
- **Page content only:** `commit_block_with_font` rejects blocks whose show op lives
  in a Form XObject (`stream_idx != 0`) — those keep the cover-redraw fallback.
- **Bundled-font face ≠ original embedded face:** a Tier-3 glyph renders in the
  matched core font, not the document's original (often-subset) face; visually close
  for Latin/Vietnamese, may differ for CJK families.
- **Browser end-to-end** (edit a CID block → type a glyph the doc never used → save →
  reopen → confirm a Type0 font was embedded and the glyph persists) is the user's
  final verification step.
