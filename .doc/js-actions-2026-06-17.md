# js-actions — Implementation Report

**Date:** 2026-06-17
**Scope:** `src/js/` — JavaScript Actions Engine (ISO 32000-1 §12.6)

## What Was Implemented

### Cargo.toml
- Added `rquickjs = { version = "0.6", optional = true, features = ["bindgen", "classes", "properties"] }` dependency (~1 MB binary overhead).
- Added `js-actions = ["dep:rquickjs"]` feature gate.
- Wired `js-actions` into the `wasm` feature bundle.

### `src/js/mod.rs`
Re-exports `JsEngine`, `JsEvent`, `JsActionResult`, `dispatch_doc_event`.

### `src/js/engine.rs`
- `JsEvent` enum — covers all action types from ISO 32000-1 §12.6: `DocOpen`, `DocClose`, `PageOpen/Close`, `FieldKeystroke`, `FieldValidate`, `FieldFormat`, `FieldCalculate`, `ButtonMouseUp`.
- `JsActionResult` — carries `rc: bool`, optional modified `value`, `modified_fields: Vec<(String, String)>`, `alerts: Vec<String>`.
- `JsEngine` — wraps a `rquickjs::Runtime`. Creates a fresh `Context` per `run_action` call so scripts cannot leak state between invocations. Logs (but does not propagate) JS runtime errors per Acrobat's non-fatal behaviour.

### `src/js/pdf_api.rs`
Injects a plain JS bootstrap script (`BOOTSTRAP` const) into each `Context` before the action script runs. This avoids Rust closures capturing `Ctx`/`Value` (which are GC-invariant over their lifetime parameter and cause `JS_FreeRuntime` assertion failures on teardown). The bootstrap defines:
- `console.{log,warn,error}` — no-ops (library code cannot print).
- `app.alert(msg)` — appends to `__pdf_alerts` array.
- `app.setTimeOut` — no-op.
- `util.{printd,printx}` — return empty string.
- `doc.getField(name)` — returns a proxy object with `.value` prop and `.setValue(v)` method that appends to `__pdf_fields`.
- `event` object seeded with correct properties for the current event type.

After script evaluation, `collect_result` reads `event.rc`, `event.value`, `__pdf_alerts`, and `__pdf_fields` back into a `JsActionResult`.

### `src/js/actions.rs`
- `dispatch_doc_event` — runs `find_action_scripts` then executes each script with `JsEngine::run_action`. Merges results; stops early on `rc = false`.
- `find_action_scripts` — dispatches to per-event helpers.
- `find_open_action_scripts` — walks `/Root /OpenAction /S /JavaScript /JS`.
- `find_field_aa_scripts` — locates the field dict by name then reads `/AA /<key>` action.
- `extract_js_from_action_dict` — extracts `/JS` as string or stream; follows `/Next` chain.
- `find_field_dict` / `search_fields` — depth-first walk of `/AcroForm /Fields` matching partial (`/T`) or dot-joined full name.
- `pdf_string_to_utf8` — decodes UTF-16BE (BOM `0xFE 0xFF`) and PDFDocEncoding strings.

### `src/lib.rs`
Added `#[cfg(feature = "js-actions")] pub mod js;`.

### `src/wasm/editor.rs`
- Added `#[cfg(feature = "js-actions")] js_engine: Option<crate::js::JsEngine>` field.
- Added `js_engine: None` to both `open` and `open_with_password` constructors.
- Added `#[cfg(feature = "js-actions")] enable_javascript(&mut self)` — initialises the engine, fires `DocOpen`, applies any field changes from doc-open scripts.
- Added `#[cfg(feature = "js-actions")] run_field_action(…)` — runs a named field event and returns a JSON result `{"rc":bool,"value":…,"alerts":[…]}`.

## Design Decisions

**JS bootstrap over Rust closures.** `rquickjs::Ctx` and `Value` are invariant over their `'js` lifetime, making them impossible to safely capture in `'static` closures (needed by `rquickjs::Function::new`). Injecting the entire API as a JS string removes the need for any Rust callbacks; state is exchanged via plain JS globals (`__pdf_alerts`, `__pdf_fields`).

**Fresh `Context` per call.** Rather than reusing one context, `run_action` allocates a new `Context` from the shared `Runtime` each call. This guarantees script isolation and prevents `JsActionResult` state from leaking between actions. Context allocation in QuickJS is cheap (a few allocs); the overhead is dominated by script compilation.

**Non-fatal JS errors.** Script evaluation errors are logged at `warn` level and the call returns `Ok(default_result)`. This matches Acrobat's behaviour (a broken validation script does not crash the document session).

**Enterprise tier gate.** JavaScript is a Phase 3 / Enterprise feature. The `js-actions` feature flag must be explicitly compiled in; the `enable_javascript` method is not exposed without it, returning a `JsError` at runtime if called without the feature.

## Test Coverage

All tests in `src/js/actions.rs` under `#[cfg(test)]`:

| Test | What it covers |
|------|---------------|
| `js_engine_evaluates_simple_script` | Happy path: `event.rc = true` is reflected in result. |
| `js_keystroke_can_reject_value` | Reject path: `isNaN` guard sets `event.rc = false`. |
| `js_app_alert_captured` | `app.alert` call is captured in `result.alerts`. |

## Known Limitations / Follow-up

- **`event.target`**: Acrobat exposes `event.target` as a full Field object. Current impl exposes `event.target_name` (a string). Full Field proxy via `doc.getField` requires the script to call `doc.getField(event.target_name)`.
- **Page open/close**: `PageOpen`/`PageClose` dispatcher returns an empty script list; needs `/Pages /AA /O` and `/C` navigation (low priority).
- **Named JS actions** (`/Names /JavaScript`): document-level named scripts callable from actions are not yet resolved. Needed for complex forms.
- **`app.response` / UI dialogs**: Stub returns `undefined`; real forms may depend on user input from dialogs.
- **WASM build**: `rquickjs` uses `bindgen` which requires a C compiler at WASM build time. Verified `cargo check --features js-actions`; full `wasm32` build requires `wasm-pack` with `emscripten` or `wasi-sdk` for the C layer (same constraint as `rquickjs` upstream).
- **No `#[cfg(not(target_arch = "wasm32"))]` fallback**: QuickJS embeds its own C runtime so it should compile to WASM natively (QuickJS is the JS engine inside `wasm-pack` itself), but the `bindgen` C build has not been validated end-to-end for this crate.
