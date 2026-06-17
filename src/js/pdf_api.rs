//! PDF JavaScript API globals registered in each QuickJS context.
//!
//! Instead of Rust closures (which would capture `Ctx` handles and create GC
//! cycles on context teardown), the entire API surface is injected as a plain
//! JS bootstrap script.  After the action script runs, results are read back
//! from well-known JS global variables.

use rquickjs::Ctx;

use super::engine::{JsActionResult, JsEvent};

// ---------------------------------------------------------------------------
// Bootstrap JS — defines app/event/doc/util/console globals
// ---------------------------------------------------------------------------

/// Injected before every action script.  Defines PDF JS API globals using
/// plain JS closures so no Rust `Ctx`-capturing closures enter the GC.
const BOOTSTRAP: &str = r#"
var __pdf_alerts = [];
var __pdf_fields = [];

var console = {
    log:   function(m) { /* suppressed in production */ },
    warn:  function(m) {},
    error: function(m) {}
};

var app = {
    alert: function(msg, icon, type, title) {
        __pdf_alerts.push(String(msg));
    },
    setTimeOut: function(fn, delay) {}
};

var util = {
    printd: function(fmt, val) { return ""; },
    printx: function(fmt, val) { return ""; }
};

var doc = {
    getField: function(name) {
        return {
            _name: name,
            value: "",
            setValue: function(v) {
                __pdf_fields.push({ name: this._name, value: String(v) });
            }
        };
    }
};
"#;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Inject the PDF JS API globals and seed the `event` object.
///
/// Evaluates `BOOTSTRAP` then sets `event.*` properties based on `ev`.
pub fn register_globals(ctx: &Ctx, ev: &JsEvent) -> rquickjs::Result<()> {
    // Inject API bootstrap.
    ctx.eval::<(), _>(BOOTSTRAP.as_bytes())?;

    // Build and set the event object.
    let event_js = build_event_js(ev);
    ctx.eval::<(), _>(event_js.as_bytes())?;

    Ok(())
}

/// Read post-script state from well-known globals and build a result.
pub fn collect_result(ctx: &Ctx) -> rquickjs::Result<JsActionResult> {
    let globals = ctx.globals();
    let mut result = JsActionResult {
        rc: true,
        ..Default::default()
    };

    // event.rc
    if let Ok(ev) = globals.get::<_, rquickjs::Object>("event") {
        result.rc = ev.get::<_, bool>("rc").unwrap_or(true);
        if let Ok(v) = ev.get::<_, String>("value") {
            if !v.is_empty() {
                result.value = Some(v);
            }
        }
    }

    // __pdf_alerts
    if let Ok(arr) = globals.get::<_, rquickjs::Array>("__pdf_alerts") {
        for i in 0..arr.len() {
            if let Ok(s) = arr.get::<String>(i) {
                result.alerts.push(s);
            }
        }
    }

    // __pdf_fields
    if let Ok(arr) = globals.get::<_, rquickjs::Array>("__pdf_fields") {
        for i in 0..arr.len() {
            if let Ok(pair) = arr.get::<rquickjs::Object>(i) {
                let name: String = pair.get("name").unwrap_or_default();
                let value: String = pair.get("value").unwrap_or_default();
                if !name.is_empty() {
                    result.modified_fields.push((name, value));
                }
            }
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a JS snippet that assigns `event` with the correct properties.
fn build_event_js(ev: &JsEvent) -> String {
    match ev {
        JsEvent::DocOpen | JsEvent::DocClose => {
            r#"var event = { name: "Doc", rc: true, value: "" };"#.to_owned()
        }
        JsEvent::PageOpen { page_index } | JsEvent::PageClose { page_index } => {
            format!(
                r#"var event = {{ name: "Page", pageNum: {}, rc: true, value: "" }};"#,
                page_index
            )
        }
        JsEvent::FieldKeystroke {
            field_name,
            value,
            change,
        } => {
            format!(
                r#"var event = {{ name: "Keystroke", target_name: {}, value: {}, change: {}, rc: true }};"#,
                js_str(field_name),
                js_str(value),
                js_str(change),
            )
        }
        JsEvent::FieldValidate { field_name, value } => {
            format!(
                r#"var event = {{ name: "Validate", target_name: {}, value: {}, rc: true }};"#,
                js_str(field_name),
                js_str(value),
            )
        }
        JsEvent::FieldFormat { field_name, value } => {
            format!(
                r#"var event = {{ name: "Format", target_name: {}, value: {}, rc: true }};"#,
                js_str(field_name),
                js_str(value),
            )
        }
        JsEvent::FieldCalculate { field_name } => {
            format!(
                r#"var event = {{ name: "Calculate", target_name: {}, value: "", rc: true }};"#,
                js_str(field_name),
            )
        }
        JsEvent::ButtonMouseUp { field_name } => {
            format!(
                r#"var event = {{ name: "Mouse Up", target_name: {}, rc: true }};"#,
                js_str(field_name),
            )
        }
    }
}

/// Wrap `s` in a JS string literal with escaping.
fn js_str(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}
