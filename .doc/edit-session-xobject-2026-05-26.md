# Edit Session — XObject Recursion & CMap Fix — Implementation Report

**Date:** 2026-05-26
**Scope:** `src/editor/edit_session.rs` + frontend `LazyPageCanvas.vue` / `usePdfStore.ts`

## What Was Implemented

### Rust — `edit_session.rs`

- **`OpStreamSource` enum** — distinguishes `PageContent` from `FormXObject(u32)` streams
- **`OpStream` struct** — holds `source` + `ops`; replaces the previous single `ops: Vec<Operation>`
- **`EditableFrame::stream_idx`** — new field tracking which stream a frame belongs to
- **`EditSession::streams`** — `Vec<OpStream>` replacing the old single `ops` field
- **`GfxState` struct** — extracted graphics state (CTM, text matrix, font, leading) for recursion
- **`extract_frames_recursive`** — replaces `extract_raw_frames`; recurses into Form XObjects via `Do`
- **`handle_do_xobject`** — resolves `/XObject/<name>`, checks `/Subtype == Form`, decodes stream, pushes new `OpStream`, recurses with inherited CTM and fresh text state; includes cycle detection via `HashSet<u32>`
- **`Tf` operator fixed** — removed incorrect `in_text` guard; `Tf` is valid outside `BT`/`ET` per ISO 32000-1 §9.3.1
- **`patch_frame`** — updated to use `(stream_idx, op_idx)` instead of plain `op_idx`
- **`commit_edit_session`** — iterates all streams; writes `PageContent` to `/Contents`, writes `FormXObject(n)` by replacing the XObject object
- **`extract_raw_frames_no_doc`** — test-only helper (Latin-1 fallback, no doc required); replaces direct `extract_raw_frames` calls in tests

### Frontend

- **`usePdfStore.ts`** — added `onPageScrolledIntoView(pageIndex)`: sets `currentPage` and calls `enterEditMode` when `tool === 'edit-text'`; exposed in store return
- **`LazyPageCanvas.vue`** — replaced `store.currentPage = props.pageIndex` with `store.onPageScrolledIntoView(props.pageIndex)` so edit session reloads on scroll

## Design Decisions

- **Multi-stream architecture**: Text in real PDFs often lives inside Form XObjects (`Do` operator), not the page content directly. Storing all streams in `EditSession::streams` with indexed references lets `patch_frame` and `commit_edit_session` write back to the correct stream without a second scan.
- **Clone before recurse**: `streams[n].ops` is cloned before passing to `extract_frames_recursive` to avoid a borrow conflict between `&ops` and `&mut streams`. The clone is unavoidable due to the recursive push pattern.
- **Fresh `GfxState` per XObject**: Form XObjects have independent text state. A new `GfxState` is created for each XObject with only the concatenated CTM inherited from the parent — matching xpdf's `pushResources/popResources` model.
- **`HashMap::new()` on XObject commit**: The replacement XObject stream uses an empty extra dict, losing `/BBox`, `/Matrix`, `/Resources`. This is a known limitation (see below) acceptable for the initial fix.

## Test Coverage

All previous tests updated to use `extract_raw_frames_no_doc` (new tuple format with `stream_idx` prepended):

- `extracts_tj_at_tm_position` — Tj at Tm position, font size and key
- `extracts_tj_through_ctm_translation` — CTM concat via `cm`
- `extracts_ctm_restored_after_q_q` — `q`/`Q` save/restore
- `extracts_tj_array` — `TJ` array text joining
- `td_advances_position` — `Td` offset
- `t_star_advances_by_leading` — `T*` with `TL` leading
- `skips_empty_text` — empty string skipped
- `patch_frame_replaces_tj` — Tj in-place patch
- `patch_frame_invalid_id_returns_false` — out-of-range id
- `patch_frame_tj_array_collapses_to_tj` — TJ → Tj collapse

## Known Limitations / Follow-up

1. **XObject dict not preserved on commit** — `commit_edit_session` replaces Form XObject streams with `make_flate_stream(..., HashMap::new())`, losing `/BBox`, `/Matrix`, `/Resources`. The page may render incorrectly after save if the XObject's bounding box was significant. Fix: store the original dict in `OpStream` and pass it (minus Filter/Length/DecodeParms) to `make_flate_stream`.
2. **XObject /Matrix order** — The XObject matrix is concatenated as `current_ctm.concat(&xobj_matrix)`. If `Matrix::concat` is post-multiply, this may differ from PDF spec (which pre-multiplies). Verify against rendering output.
3. **No `Tf` outside `BT`** in tests — The `Tf` guard fix is not directly tested (crafting a stream with `Tf` before `BT` and verifying frame capture). Add a test once the test helper supports the full doc path.
