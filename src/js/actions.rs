//! PDF action dispatch — find action scripts in the document structure and run them.
//!
//! Handles:
//! - `/OpenAction` (document-open JavaScript)
//! - Field additional-actions (`/AA`): `/K` keystroke, `/V` validate, `/F` format, `/C` calculate
//! - Button `/AA /U` (mouse-up)

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDocument, PdfObject};

use super::engine::{JsActionResult, JsEngine, JsEvent};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Execute all JavaScript action scripts for `event` in the given document.
///
/// Scripts are collected from the document structure (OpenAction, field /AA,
/// etc.) and run sequentially.  If any script sets `event.rc = false` the
/// dispatch stops early and returns the partial result with `rc = false`.
pub fn dispatch_doc_event(
    doc: &PdfDocument,
    engine: &JsEngine,
    event: JsEvent,
) -> Result<JsActionResult> {
    let scripts = find_action_scripts(doc, &event)?;
    let mut combined = JsActionResult {
        rc: true,
        ..Default::default()
    };

    for script in scripts {
        let result = engine.run_action(&script, &event)?;
        combined.alerts.extend(result.alerts);
        combined.modified_fields.extend(result.modified_fields);
        if let Some(v) = result.value {
            combined.value = Some(v);
        }
        if !result.rc {
            combined.rc = false;
            break;
        }
    }

    Ok(combined)
}

// ---------------------------------------------------------------------------
// Script extraction helpers
// ---------------------------------------------------------------------------

/// Locate all JavaScript action script strings relevant to `event`.
///
/// Returns an empty `Vec` when no actions are found (non-error).
fn find_action_scripts(doc: &PdfDocument, event: &JsEvent) -> Result<Vec<String>> {
    match event {
        JsEvent::DocOpen => find_open_action_scripts(doc),
        JsEvent::DocClose => Ok(vec![]), // Not commonly embedded; extend as needed.
        JsEvent::PageOpen { .. } | JsEvent::PageClose { .. } => Ok(vec![]),
        JsEvent::FieldKeystroke { field_name, .. } => find_field_aa_scripts(doc, field_name, "K"),
        JsEvent::FieldValidate { field_name, .. } => find_field_aa_scripts(doc, field_name, "V"),
        JsEvent::FieldFormat { field_name, .. } => find_field_aa_scripts(doc, field_name, "F"),
        JsEvent::FieldCalculate { field_name } => find_field_aa_scripts(doc, field_name, "C"),
        JsEvent::ButtonMouseUp { field_name } => find_field_aa_scripts(doc, field_name, "U"),
    }
}

/// Extract JavaScript from `/Root /OpenAction`.
///
/// ISO 32000-1 §12.3.2: `/OpenAction` is either a destination array or an
/// action dict with `/S /JavaScript /JS <string-or-stream>`.
fn find_open_action_scripts(doc: &PdfDocument) -> Result<Vec<String>> {
    let root_ref = match doc.trailer.get("Root") {
        Some(r) => r.clone(),
        None => return Ok(vec![]),
    };
    let catalog = doc.resolve(&root_ref)?;
    let catalog_dict = match catalog.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(vec![]),
    };

    let action_obj = match catalog_dict.get("OpenAction") {
        Some(o) => o.clone(),
        None => return Ok(vec![]),
    };
    let action = doc.resolve(&action_obj)?;
    let action_dict = match action.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(vec![]), // Destination array, not an action
    };

    extract_js_from_action_dict(doc, &action_dict).map(|s| s.into_iter().collect())
}

/// Find the field dict for `field_name` in `/AcroForm /Fields` and return
/// JavaScript from the `/AA` additional-action entry keyed by `aa_key`.
///
/// `aa_key` is `"K"` (Keystroke), `"V"` (Validate), `"F"` (Format),
/// `"C"` (Calculate), or `"U"` (MouseUp).
fn find_field_aa_scripts(doc: &PdfDocument, field_name: &str, aa_key: &str) -> Result<Vec<String>> {
    let field_dict = match find_field_dict(doc, field_name)? {
        Some(d) => d,
        None => return Ok(vec![]),
    };

    let aa_ref = match field_dict.get("AA") {
        Some(o) => o.clone(),
        None => return Ok(vec![]),
    };
    let aa = doc.resolve(&aa_ref)?;
    let aa_dict = match aa.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(vec![]),
    };

    let action_ref = match aa_dict.get(aa_key) {
        Some(o) => o.clone(),
        None => return Ok(vec![]),
    };
    let action = doc.resolve(&action_ref)?;
    let action_dict = match action.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(vec![]),
    };

    Ok(extract_js_from_action_dict(doc, &action_dict)
        .unwrap_or_default()
        .into_iter()
        .collect())
}

/// Extract the JavaScript string(s) from an action dictionary.
///
/// Per ISO 32000-1 §12.6.4.16: `/S /JavaScript` with `/JS` as a text string
/// or stream.  Also handles `/Next` chained actions.
fn extract_js_from_action_dict(
    doc: &PdfDocument,
    action_dict: &crate::parser::objects::PdfDict,
) -> Result<Vec<String>> {
    let s_name = action_dict.get("S").and_then(|o| o.as_name()).unwrap_or("");
    if s_name != "JavaScript" {
        return Ok(vec![]);
    }

    let js_ref = match action_dict.get("JS") {
        Some(o) => o.clone(),
        None => return Ok(vec![]),
    };
    let js_obj = doc.resolve(&js_ref)?;

    let mut scripts: Vec<String> = Vec::new();

    let script = match js_obj {
        PdfObject::String(bytes) => pdf_string_to_utf8(&bytes),
        PdfObject::Stream(ref s) => {
            let decoded = s
                .decode_with_doc(doc)
                .map_err(|e| PdfError::js_error(format!("JS stream decode: {e}")))?;
            String::from_utf8_lossy(&decoded).into_owned()
        }
        _ => return Ok(vec![]),
    };
    scripts.push(script);

    // Follow /Next chained actions (ISO 32000-1 §12.6.3).
    if let Some(next_ref) = action_dict.get("Next") {
        let next = doc.resolve(next_ref)?;
        match next {
            PdfObject::Dictionary(d) => {
                scripts.extend(extract_js_from_action_dict(doc, &d)?);
            }
            PdfObject::Array(arr) => {
                for item in &arr {
                    let resolved = doc.resolve(item)?;
                    if let Some(d) = resolved.as_dict() {
                        scripts.extend(extract_js_from_action_dict(doc, d)?);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(scripts)
}

/// Walk `/AcroForm /Fields` (depth-first) to find the dict for `field_name`.
///
/// Matches against `/T` (partial name) and the dot-joined full name.
fn find_field_dict(
    doc: &PdfDocument,
    field_name: &str,
) -> Result<Option<crate::parser::objects::PdfDict>> {
    let root_ref = match doc.trailer.get("Root") {
        Some(r) => r.clone(),
        None => return Ok(None),
    };
    let catalog = doc.resolve(&root_ref)?;
    let catalog_dict = match catalog.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(None),
    };
    let acroform_ref = match catalog_dict.get("AcroForm") {
        Some(o) => o.clone(),
        None => return Ok(None),
    };
    let acroform = doc.resolve(&acroform_ref)?;
    let acroform_dict = match acroform.as_dict() {
        Some(d) => d.clone(),
        None => return Ok(None),
    };
    let fields_ref = match acroform_dict.get("Fields") {
        Some(o) => o.clone(),
        None => return Ok(None),
    };
    let fields_obj = doc.resolve(&fields_ref)?;
    let fields_arr = match fields_obj {
        PdfObject::Array(a) => a,
        _ => return Ok(None),
    };

    search_fields(doc, &fields_arr, field_name, "")
}

/// Recursive depth-first search through the field tree.
fn search_fields(
    doc: &PdfDocument,
    arr: &[PdfObject],
    target: &str,
    parent_name: &str,
) -> Result<Option<crate::parser::objects::PdfDict>> {
    for item in arr {
        let resolved = doc.resolve(item)?;
        let dict = match resolved.as_dict() {
            Some(d) => d.clone(),
            None => continue,
        };

        let partial = match dict.get("T") {
            Some(PdfObject::String(b)) => pdf_string_to_utf8(b),
            Some(PdfObject::Name(n)) => n.clone(),
            _ => String::new(),
        };
        let full = if parent_name.is_empty() {
            partial.clone()
        } else {
            format!("{}.{}", parent_name, partial)
        };

        if partial == target || full == target {
            return Ok(Some(dict));
        }

        // Recurse into /Kids.
        if let Some(kids_ref) = dict.get("Kids") {
            let kids = doc.resolve(kids_ref)?;
            if let PdfObject::Array(kids_arr) = kids {
                if let Some(found) = search_fields(doc, &kids_arr, target, &full)? {
                    return Ok(Some(found));
                }
            }
        }
    }
    Ok(None)
}

/// Decode a PDF string byte slice to UTF-8.
///
/// Handles UTF-16BE (BOM `0xFE 0xFF`) and PDFDocEncoding (Latin-1 fallback).
fn pdf_string_to_utf8(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let words: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&words)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_engine_evaluates_simple_script() {
        let engine = JsEngine::new().unwrap();
        let event = JsEvent::FieldKeystroke {
            field_name: "Age".to_owned(),
            value: "25".to_owned(),
            change: "5".to_owned(),
        };
        let result = engine.run_action("event.rc = true;", &event).unwrap();
        assert!(result.rc);
    }

    #[test]
    fn js_keystroke_can_reject_value() {
        let engine = JsEngine::new().unwrap();
        let event = JsEvent::FieldKeystroke {
            field_name: "Age".to_owned(),
            value: "abc".to_owned(),
            change: "c".to_owned(),
        };
        let script = r#"if (isNaN(event.value)) { event.rc = false; }"#;
        let result = engine.run_action(script, &event).unwrap();
        assert!(!result.rc);
    }

    #[test]
    fn js_app_alert_captured() {
        let engine = JsEngine::new().unwrap();
        let event = JsEvent::DocOpen;
        let result = engine.run_action(r#"app.alert("hello");"#, &event).unwrap();
        assert_eq!(result.alerts, vec!["hello".to_string()]);
    }
}
