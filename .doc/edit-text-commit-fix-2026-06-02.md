# Edit-Text Commit Corruption — Implementation Report

**Date:** 2026-06-02
**Scope:** text-edit commit path (frontend ordering) + page resource registration (`editor::content_draw`)

## What Was Implemented

Two independent bugs that combined to corrupt a page when editing CID/Type0 text
(e.g. the title "Vietnam: Banking System").

### Fix 1 — Frontend commit/cancel ordering (`web-editor`)

- `web-editor/src/components/AnnotationOverlay.vue` — rewrote `commitBlockEdit()`:
  - The surgical commit (`store.textEditCommit(rustId)`) now runs **before**
    `cancelBlockEdit()`. Previously `cancelBlockEdit()` ran first and nulled the Rust
    `active_text_edit` session, so the commit read a dead session and returned
    `{"committed":false,"missing":""}` without writing anything.
  - No-op edits (`newText === group.text`) tear down the session/UI and return early,
    never touching the document.
  - The destructive `replaceText` cover-and-redraw fallback is now gated on a genuinely
    non-empty `missing` set, so the success / no-op paths can never trigger it.

### Fix 2 — Resource registration resolves refs + inheritance (`pdf-core`)

- `src/editor/content_draw.rs`:
  - Added `resolve_dict(editor, &PdfObject) -> Option<PdfDict>` — resolves an inline dict
    or an indirect reference into an owned dict via the CoW `editor.get_object`.
  - Added `effective_resources(editor, &PdfDict) -> PdfDict` — resolves a page's effective
    `/Resources`, handling an inline dict, an indirect reference, and inheritance from an
    ancestor `/Pages` node (walks `/Parent`, depth-limited at 64).
  - Replaced the two near-identical `register_font_resource` / `register_xobject_resource`
    functions with one `register_resource_entry(editor, page_id, category, key, obj_id)`
    (`category` = `"Font"` | `"XObject"`); updated the three call sites in `draw_text`,
    `place_image`, `place_jpeg`.

## Design Decisions

- **Commit before cancel.** Root cause of the reported corruption. `textEditCommit`
  already `await refreshDoc()` internally on success, so tearing down the UI afterwards
  has no race with the rebuilt block model.
- **Gate the fallback on `missing`.** After the reorder, a `committed:false` result only
  ever carries a non-empty `missing` (Tier-3 embed genuinely failed). Gating makes the
  former empty-`missing` bug path a pure no-op and keeps a destructive Helvetica
  cover-redraw from ever landing on a CID block on the success path.
- **Inline a self-contained `/Resources` on the page.** When `/Resources` or its
  `/Font` / `/XObject` sub-dict is an indirect reference, or `/Resources` is inherited,
  the old code's strict inline-only match silently dropped the existing entries and wrote
  back only the new font — wiping every CID font on the page (images survived because
  `/XObject` was untouched), so on reparse the renderer fell back to simple WinAnsi
  encoding and rendered the whole page as a uniform −29 letter shift. The fix copies the
  resolved/inherited resources down onto the page and merges the new entry, isolating the
  change from any `/Resources` object shared by other pages.
- **One helper for both categories.** `register_font_resource` and
  `register_xobject_resource` were byte-for-byte identical except for the dict name;
  collapsing them removes the duplicated (and identically buggy) logic.

## Test Coverage

Added to `src/editor/content_draw.rs` `#[cfg(test)]` (happy-path tests retained):

- `draw_text_preserves_indirect_font_dict` — `/Resources/Font` as an indirect reference;
  asserts the existing `F1` survives and the new standard font is added.
- `draw_text_inherits_and_preserves_pages_resources` — page with no own `/Resources`,
  `/Pages` node carries `/Font {F1}` + `/XObject {Im1}`; asserts the page gains an inlined
  `/Resources` keeping `F1` (+ new font) and `Im1`.
- `place_image_preserves_indirect_xobject_dict` — `/Resources/XObject` as an indirect
  reference; asserts `Im1` survives alongside the newly placed image.

Full gate: `cargo fmt --check`, `cargo clippy -- -D warnings` (clean), `cargo test`
(269 passed, 2 ignored), `cargo build --target wasm32-unknown-unknown` (ok). WASM package
rebuilt via `make wasm` into `web-editor/src/pkg`.

### Fix 3 — Re-entering edit mode showed the pre-edit text (`pdf-core`)

After a surgical commit, the rendered page showed the edited text but clicking the
block to edit again resurrected the original. Root cause: `text_edit_enter` built the
editable model from the pristine `self.editor.doc`, while commits live in the editor's
copy-on-write **writer pool** (`commit_edit_session` replaces `/Contents` there). The
renderer reparses `save()` bytes (sees the edit); the edit model read `doc` (did not).

- `src/wasm/editor.rs` — added `WasmEditor.edit_model_doc: Option<(usize, PdfDocument)>`:
  the document reparsed from pending edits, tagged with the writer-pool size it reflects.
  Made `original_bytes` `pub(crate)` for the reparse.
- `src/wasm/text_edit.rs`:
  - `text_edit_enter` now builds the model from the **current** state: when the writer
    pool is non-empty it `save_append`s + reparses once per pool-size change (cached),
    else it uses the pristine `doc`.
  - Added `text_edit_doc()` accessor returning the reparsed doc when present, else
    `editor.doc`; routed the model build, `text_edit_open` metrics, `text_edit_commit`
    encoding, and `text_edit_render_block` preview through it so all four read one
    consistent document.

Design notes: `save_append` is repeatable and non-mutating (snapshots the pool, returns
the original unchanged when empty), so calling it from `text_edit_enter` is safe. The
pool-size cache key means re-entry/prefetch after one edit reparses only once, not per
page. This also fixes a latent Tier-3 case (re-editing an embedded-font block) where the
new font key existed only in the writer pool.

### Fix 4 — Legacy `exit_edit_mode` clobbered the surgical edit (`pdf-core`)

Symptom: the edit rendered correctly, then a second render reverted it to the original,
and a saved copy contained the original data. Root cause: the legacy frame-edit session
(`enter_edit_mode`) is built from the **pristine** `editor.doc` purely to feed overlay
metadata and is never patched (the Word-style flow uses `text_edit_*`, and
`commit_text_edit`/`patch_frame` is dead). But `exit_edit_mode` (fired on tool-switch via
the store's `exitEditMode`) unconditionally called `commit_edit_session`, re-serializing
those pristine ops back over the page `/Contents` — overwriting the surgical
`text_edit_commit`. The log showed `surgical commit_block OK` → correct render →
`exit_edit_mode page=0` → page re-saved → re-render = original.

- `src/editor/edit_session.rs` — added `EditSession::dirty`; `patch_frame` sets it on a
  real change. Initialized `false` in both production constructors and the test fixtures.
- `src/wasm/editor.rs` — `exit_edit_mode` now writes back only when `session.dirty`,
  otherwise drops the session (an unmodified session was built only for overlay metadata).

This is independent of and complementary to Fix 1 (commit ordering): Fix 1 made the
surgical commit run; Fix 4 stops the legacy path from undoing it on tool-switch.

## Known Limitations / Follow-up

- `effective_resources` copies the full inherited `/Resources` onto the page (correctness
  over size). For pages with very large shared resource dicts this slightly inflates the
  incremental update; acceptable and isolated per page.
- The legitimate `replaceText` cover-redraw (scanned/standard-font pages where a glyph
  truly can't be embedded) still draws a Helvetica substitute; with Fix 2 it no longer
  damages the rest of the page, but it remains a visual fallback, not a faithful render of
  unencodable glyphs. Tracked as part of the write-back tiers ([[project_edit_text_wordstyle]]).
- End-to-end browser verification (edit title → expect surgical commit, no `replaceText`
  log, no scramble after page navigation, clean reopen) to be run against the rebuilt pkg.
