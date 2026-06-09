# Text-Edit Revert Bug — Implementation Report

**Date:** 2026-06-07
**Scope:** text-edit revert bug (Fix 1–3 from `.fix_plan/implementation-plan.md`)

## What Was Implemented

- **Fix 1** (`pdf-editor-rust-core/src/wasm/text_edit.rs`, FAST-PATH ~line 99): Removed the `self.active_text_edit = None` reset from the FAST-PATH branch of `text_edit_enter`. The FULL-REBUILD path at line 163 already resets it; the FAST-PATH reset was creating a race window where a fire-and-forget commit could fire after the session was destroyed.
- **Fix 2** (`web-editor/src/components/AnnotationOverlay.vue`, `onOverlayClick`): Changed `void commitBlockEdit()` → `await commitBlockEdit()` and made the handler `async`. Closes the race between a background click-commit and a concurrent `reenterForPage()` call.
- **Fix 3** (`web-editor/src/components/AnnotationOverlay.vue`, `commitBlockEdit` failure branch): Added an `else` branch to the `if (missing)` guard so that `committed:false` with an empty `missing` string now shows a user-facing warning instead of silently reverting. Also fixed the `replaceText` fallback guard to be nested inside `if (missing)` instead of a separate top-level check.
- **Test** (`pdf-editor-rust-core/src/wasm/text_edit.rs`, `#[cfg(test)]`): Added `text_edit_enter_fast_path_preserves_active_session` — loads `Group-3.pdf`, does a FULL-REBUILD, opens a block, re-enters the same page/gen (FAST-PATH), and asserts `active_text_edit` is still `Some` with the original `block_id`.

## Design Decisions

- Fix 1 only removes the reset from the FAST-PATH. The FULL-REBUILD path still resets `active_text_edit` at line 163, so switching pages or rebuilding after a commit correctly clears the stale session.
- Fix 2 makes `onOverlayClick` `async`. The Vue event system handles async handlers via `void`-wrapped calls on the template side; the change is safe.
- Fix 3 restructures the `missing` / fallback block so the `replaceText` path is only reachable when `missing` is non-empty, eliminating the double-gate that previously allowed a no-op `replaceText` call on a `committed:false` + no-missing result.
- The new test is gated on `#[cfg(feature = "wasm")]` so it only runs with `cargo test --features wasm --lib`.

## Test Coverage

- `text_edit_enter_fast_path_preserves_active_session`: FAST-PATH re-entry preserves `active_text_edit` (regression guard for Fix 1). Verified passing: 1 passed, 554 filtered out.

## Known Limitations / Follow-up

- Fix 4 (gen jump investigation) is deferred — requires adding logging inside `flush_and_cache` and render paths to isolate the +6 gen jump after two block deletions. See `.fix_plan/implementation-plan.md` §Fix 4 for steps.
- The `render_metrics` dead-code warning on `ActiveTextEdit` is pre-existing: the field is read under `#[cfg(feature = "render")]`; `cargo clippy --features wasm-render -- -D warnings` is clean.
- `cargo test --features wasm` (all integration tests) has a pre-existing import error in `tests/wasm_api.rs` (`pdf_core::wasm` doesn't re-export types at top level). Not introduced by this change.
