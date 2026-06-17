//! QuickJS runtime wrapper for PDF JavaScript action execution.

use crate::error::{PdfError, Result};

// ---------------------------------------------------------------------------
// Public event / result types
// ---------------------------------------------------------------------------

/// A PDF JavaScript event passed to an action script.
#[derive(Debug, Clone)]
pub enum JsEvent {
    /// `/OpenAction` on document open.
    DocOpen,
    /// Document close (before save/discard).
    DocClose,
    /// Page `/AA /O` (page open).
    PageOpen { page_index: usize },
    /// Page `/AA /C` (page close).
    PageClose { page_index: usize },
    /// Field `/AA /K` — keystroke during text entry.
    FieldKeystroke {
        field_name: String,
        /// Full current field value before the keystroke.
        value: String,
        /// The character(s) being added.
        change: String,
    },
    /// Field `/AA /V` — validate when focus leaves the field.
    FieldValidate { field_name: String, value: String },
    /// Field `/AA /F` — format before display.
    FieldFormat { field_name: String, value: String },
    /// Field `/AA /C` — recalculate (triggered by another field change).
    FieldCalculate { field_name: String },
    /// Button widget mouse-up (submit, reset, or arbitrary JS).
    ButtonMouseUp { field_name: String },
}

/// Results produced by running a JavaScript action script.
#[derive(Debug, Default)]
pub struct JsActionResult {
    /// `event.rc` — `false` means reject the current change (keystroke/validate).
    pub rc: bool,
    /// Modified `event.value` written back by the script.
    pub value: Option<String>,
    /// Field values changed by `doc.getField(name).value = …`.
    pub modified_fields: Vec<(String, String)>,
    /// Messages queued by `app.alert(…)`.
    pub alerts: Vec<String>,
}

// ---------------------------------------------------------------------------
// JsEngine
// ---------------------------------------------------------------------------

/// Embedded QuickJS runtime for evaluating PDF JavaScript actions.
///
/// QuickJS is single-threaded; on WASM this matches the runtime model.
/// One engine instance is shared for the lifetime of a document session.
pub struct JsEngine {
    // SAFETY: rquickjs Runtime and Context are not Send; the engine must stay
    // on the thread that created it. On WASM there is only one thread.
    runtime: rquickjs::Runtime,
}

impl JsEngine {
    /// Create a new QuickJS runtime and register PDF global objects.
    pub fn new() -> Result<Self> {
        let runtime = rquickjs::Runtime::new().map_err(|e| PdfError::js_error(format!("{e}")))?;
        Ok(Self { runtime })
    }

    /// Execute `script` in the context of `event` and return the action result.
    ///
    /// The engine creates a fresh `Context` per call so scripts cannot leak
    /// state between unrelated action invocations.  Call overhead is dominated
    /// by QuickJS bytecode compilation (~µs range), not context allocation.
    pub fn run_action(&self, script: &str, event: &JsEvent) -> Result<JsActionResult> {
        let ctx = rquickjs::Context::full(&self.runtime)
            .map_err(|e| PdfError::js_error(format!("{e}")))?;

        ctx.with(|ctx| {
            // Register PDF globals and seed the event object.
            crate::js::pdf_api::register_globals(&ctx, event)
                .map_err(|e| PdfError::js_error(format!("{e}")))?;

            // Execute the action script.
            let eval_result: std::result::Result<rquickjs::Value, _> = ctx.eval(script.as_bytes());
            if let Err(e) = eval_result {
                // Log JS errors as warnings; they are non-fatal per Acrobat behaviour.
                log::warn!("[js-actions] script error: {e}");
            }

            // Read back results from the event and doc globals.
            crate::js::pdf_api::collect_result(&ctx).map_err(|e| PdfError::js_error(format!("{e}")))
        })
    }
}

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

impl PdfError {
    pub(crate) fn js_error(msg: impl Into<String>) -> Self {
        PdfError::InvalidToken {
            offset: 0,
            detail: format!("JS: {}", msg.into()),
        }
    }
}
