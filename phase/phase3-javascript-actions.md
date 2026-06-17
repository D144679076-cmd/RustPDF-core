# Phase 3 — JavaScript Actions Engine

**Status:** Complete — 2026-06-17
**Effort:** ~4–6 months
**Tier gate:** Enterprise
**New dependency:** `rquickjs` (WASM-compatible QuickJS binding)

## Context

PDF documents can embed JavaScript for form validation, dynamic calculations, and document-level actions. Required for: form field validation, calculated fields, button actions, document-open actions. Specified in ISO 32000-1 §12.6 and Adobe's JS API reference.

## Dependency

```toml
[dependencies]
rquickjs = { version = "0.6", optional = true, features = ["bindgen", "classes", "properties"] }

[features]
js-actions = ["dep:rquickjs"]
wasm = ["...", "js-actions"]
```

`rquickjs` embeds QuickJS (C, compiled to WASM via cc crate). ~1MB overhead.

## New Module `src/js/`

```
src/js/
  mod.rs
  engine.rs    — QuickJS runtime wrapper
  pdf_api.rs   — PDF JavaScript API bindings (app, event, doc, field objects)
  actions.rs   — action dispatch (open, close, keystroke, validate, format, calculate)
```

## `src/js/engine.rs`

```rust
pub struct JsEngine {
    ctx: rquickjs::Ctx<'static>,
    // NOTE: QuickJS context is not Send; keep on single thread (WASM is single-threaded)
}

impl JsEngine {
    pub fn new() -> Result<Self> {
        let rt = rquickjs::Runtime::new()?;
        let ctx = rquickjs::Context::full(&rt)?;
        // Register PDF JS API globals: app, event, console
        ctx.with(|ctx| {
            crate::js::pdf_api::register_globals(&ctx)?;
            Ok::<_, rquickjs::Error>(())
        })?;
        Ok(Self { ctx: /* ... */ })
    }

    /// Execute a JavaScript action script.
    /// Returns any field modifications the script requested.
    pub fn run_action(&self, script: &str, event: &JsEvent) -> Result<JsActionResult> {
        self.ctx.with(|ctx| {
            // Set event object properties
            crate::js::pdf_api::set_event(&ctx, event)?;
            // Evaluate script
            ctx.eval::<(), _>(script)?;
            // Read back any modified field values from event object
            let result = crate::js::pdf_api::get_action_result(&ctx)?;
            Ok(result)
        })
    }
}

#[derive(Debug, Clone)]
pub enum JsEvent {
    DocOpen,
    DocClose,
    PageOpen { page_index: usize },
    PageClose { page_index: usize },
    FieldKeystroke { field_name: String, value: String, change: String },
    FieldValidate { field_name: String, value: String },
    FieldFormat { field_name: String, value: String },
    FieldCalculate { field_name: String },
    ButtonMouseUp { field_name: String },
}

#[derive(Debug, Default)]
pub struct JsActionResult {
    pub rc: bool,                              // event.rc — false means reject the change
    pub value: Option<String>,                 // modified field value
    pub modified_fields: Vec<(String, String)>, // [(field_name, new_value)]
    pub alerts: Vec<String>,                   // app.alert() calls
}
```

## `src/js/pdf_api.rs`

Register JavaScript globals that match Acrobat's API:

```javascript
// Globals registered by Rust:
// app.alert(msg, icon, type, title) → shows alert (Rust stores in result.alerts)
// app.setTimeOut(fn, delay) → no-op in WASM
// event.value — current field value
// event.change — current keystroke
// event.rc — reject-change flag (set to false to reject)
// doc.getField(name) → Field object
// Field.value — get/set field value
// Field.display — visibility
// util.printd(format, date) → date formatting
// util.printx(format, value) → masking
```

Implementation pattern (using `rquickjs` class binding):
```rust
pub fn register_globals(ctx: &rquickjs::Ctx) -> Result<()> {
    let globals = ctx.globals();
    // Register `app` object
    globals.set("app", AppObject::new())?;
    // Register `event` object (mutable, updated before each script run)
    globals.set("event", EventObject::default())?;
    // Register `console` (maps to log::debug!)
    globals.set("console", ConsoleObject)?;
    Ok(())
}
```

## `src/js/actions.rs`

```rust
/// Execute all JavaScript actions for a document event.
pub fn dispatch_doc_event(doc: &PdfDocument, engine: &JsEngine, event: JsEvent) -> Result<JsActionResult> {
    // Find action scripts for this event type in the document
    let scripts = find_action_scripts(doc, &event)?;
    let mut combined_result = JsActionResult::default();
    for script in scripts {
        let result = engine.run_action(&script, &event)?;
        combined_result.alerts.extend(result.alerts);
        if let Some(v) = result.value { combined_result.value = Some(v); }
        combined_result.modified_fields.extend(result.modified_fields);
        if !result.rc { combined_result.rc = false; break; }
    }
    Ok(combined_result)
}

fn find_action_scripts(doc: &PdfDocument, event: &JsEvent) -> Result<Vec<String>> {
    // For DocOpen: /Root /OpenAction /A /S /JavaScript /JS
    // For field events: field dict /AA (additional actions) /K (keystroke) /V (validate) /F (format) /C (calculate)
    // ...
    todo!()
}
```

## Integration with `WasmEditor`

```rust
// In WasmEditor: keep an optional JsEngine
pub struct WasmEditor {
    editor: PdfEditor,
    #[cfg(feature = "js-actions")]
    js_engine: Option<crate::js::JsEngine>,
}

#[wasm_bindgen]
pub fn enable_javascript(&mut self) -> Result<(), JsError> {
    #[cfg(feature = "js-actions")]
    {
        self.js_engine = Some(crate::js::JsEngine::new().map_err(|e| JsError::new(&e.to_string()))?);
        // Run document-open scripts
        if let Some(engine) = &self.js_engine {
            let result = crate::js::dispatch_doc_event(&self.editor.doc, engine, crate::js::JsEvent::DocOpen)
                .map_err(|e| JsError::new(&e.to_string()))?;
            // Apply any field modifications from doc-open scripts
            for (name, value) in result.modified_fields {
                let _ = self.set_field_value(&name, &value);
            }
        }
        Ok(())
    }
    #[cfg(not(feature = "js-actions"))]
    Err(JsError::new("JavaScript actions not compiled in"))
}
```

## Tests

```rust
#[cfg(feature = "js-actions")]
#[test]
fn js_engine_evaluates_simple_script() {
    let engine = JsEngine::new().unwrap();
    let event = JsEvent::FieldKeystroke { field_name: "Age".to_owned(), value: "25".to_owned(), change: "5".to_owned() };
    let result = engine.run_action("event.rc = true;", &event).unwrap();
    assert!(result.rc);
}

#[cfg(feature = "js-actions")]
#[test]
fn js_keystroke_can_reject_value() {
    let engine = JsEngine::new().unwrap();
    let event = JsEvent::FieldKeystroke { field_name: "Age".to_owned(), value: "abc".to_owned(), change: "c".to_owned() };
    // Script that only allows numeric input
    let script = r#"if (isNaN(event.value)) { event.rc = false; }"#;
    let result = engine.run_action(script, &event).unwrap();
    assert!(!result.rc);
}
```

## Verification

```bash
cargo test --features js-actions -- js_engine
cargo build --target wasm32-unknown-unknown --features wasm,js-actions
```
