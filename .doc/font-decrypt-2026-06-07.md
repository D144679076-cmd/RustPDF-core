# Font Decrypt — Implementation Report

**Date:** 2026-06-07
**Scope:** Encrypted PDF — embedded font-program stream decryption (renderer + editor)

## What Was Implemented

- **`page_renderer.rs` — `get_ttf_bytes`**: Replaced `stream.decode().ok()` with a decrypt-first path: when `FontFile2`/`FontFile3` is an indirect reference, call `self.doc.get_stream_data(*id)` (decrypt → defilter) and return immediately. Inline fallback uses `stream.decode_with_doc(self.doc)` instead of bare `stream.decode()`.
- **`text_encode.rs` — `embedded_truetype_program`**: Same pattern for the editor path. `doc.get_stream_data(*id)` tried first when `FontFile2` is a reference; `decode_with_doc(doc)` as fallback.
- **`make wasm` rebuild**: Recompiled all three fixes (the two font edits above + the previously-landed image-XObject `obj_id` path) into `web-editor/src/pkg/pdf_core_bg.wasm`. The prior image fix was already correct in source but the deployed bundle was stale (source at 18:19, deployed wasm at 18:10).

## Design Decisions

- **Try `get_stream_data` on the reference first, fall through**: Same pattern used for the image and SMask fixes. For unencrypted PDFs `get_stream_data` returns identical bytes to `decode_with_doc` (no enc handler → same decrypt-skip path), so there is no regression.
- **`get_stream_data` not used for fallback branch**: The fallback is only reached for inline font-program streams (very rare) or when decryption explicitly returns `Err`. In that case resolving and defiltering the stream the old way is fine.
- **`FontFile` (Type1) not touched**: `get_ttf_bytes` only ever looked at `FontFile2`/`FontFile3`. Type1 parsing is a separate, not-yet-implemented path; out of scope here.

## Test Coverage

- 296 existing tests pass (2 ignored), no regressions.
- `cargo fmt --check` and `cargo clippy -- -D warnings` both clean.
- `cargo build --target wasm32-unknown-unknown` succeeds.
- Manual verification path: open encrypted PDF → password → text in embedded font, images visible, no `Illegal start bytes` in console.

## Known Limitations / Follow-up

- No automated test for the encrypted-font path (would require an encrypted fixture with an embedded `FontFile2`). Consider adding a render-smoke test asserting `get_ttf_bytes` returns `Some` for the encrypted fixture.
- `display/mod.rs` `draw_image_xobject` still ignores `obj_id`. Noted in the prior renderer-decrypt report; same TODO.
- CFF (`FontFile3`) path is covered by Change 1 structurally, but no CFF-specific test added.

## Cross-references

- [[renderer-decrypt-2026-06-07]] — prior fix for image XObjects + SMask + ToUnicode CMap.
- This wasm rebuild also activates the image fix from that report (the image fix was source-complete but deployed wasm was stale).
