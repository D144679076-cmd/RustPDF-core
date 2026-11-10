//! `/DecodeParms` parsing for CCITTFaxDecode (ISO 32000-1 §7.4.6).
//!
//! Split from the decoder in `ccitt.rs` to respect the 800-line module cap
//! (R4); the public API is re-exported there (`ccitt::FaxParms`).

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};

/// `/DecodeParms` entries relevant to CCITTFaxDecode (ISO 32000-1 §7.4.6).
#[derive(Debug, Clone, PartialEq)]
pub struct FaxParms {
    /// `/K`: 0 = pure 1-D (MH), > 0 = mixed 1-D/2-D (MR), < 0 = pure 2-D MMR (G4).
    pub k: i32,
    /// `/Columns`: pixels per row. Default 1728 (A4 fax width).
    pub columns: u32,
    /// `/Rows`: expected row count. 0 = unspecified (decode to EOF).
    pub rows: u32,
    /// `/BlackIs1`: when true, 1-bits are black (default: 1-bits are white).
    pub black_is1: bool,
    /// `/EncodedByteAlign`: each encoded row starts on a byte boundary.
    pub byte_align: bool,
    /// `/EndOfLine`: EOL codes are expected before each row.
    pub eoline: bool,
    /// `/EndOfBlock`: stream ends with EOFB; enables fast table lookups.
    pub eoblock: bool,
}

impl Default for FaxParms {
    fn default() -> Self {
        FaxParms {
            k: 0,
            columns: 1728,
            rows: 0,
            black_is1: false,
            byte_align: false,
            eoline: false,
            eoblock: true,
        }
    }
}

impl FaxParms {
    /// Parse from a `/DecodeParms` dictionary; `None` yields all defaults.
    ///
    /// # Errors
    /// Returns `PdfError` for out-of-range `/Columns` or `/Rows`, or entries
    /// whose type is neither number nor bool.
    pub fn from_dict(dict: Option<&PdfDict>) -> Result<Self> {
        let mut p = FaxParms::default();
        let Some(d) = dict else {
            return Ok(p);
        };
        if let Some(v) = d.get("K") {
            p.k = as_i64(v)
                .ok_or_else(|| PdfError::filter_error(0, "CCITT: /K must be a number"))?
                as i32;
        }
        if let Some(v) = d.get("Columns") {
            let c = as_i64(v)
                .ok_or_else(|| PdfError::filter_error(0, "CCITT: /Columns must be a number"))?;
            if !(1..=MAX_COLUMNS as i64).contains(&c) {
                return Err(PdfError::filter_error(
                    0,
                    format!("CCITT: /Columns {c} out of range"),
                ));
            }
            p.columns = c as u32;
        }
        if let Some(v) = d.get("Rows") {
            let r = as_i64(v)
                .ok_or_else(|| PdfError::filter_error(0, "CCITT: /Rows must be a number"))?;
            if !(0..=MAX_COLUMNS as i64).contains(&r) {
                return Err(PdfError::filter_error(
                    0,
                    format!("CCITT: /Rows {r} out of range"),
                ));
            }
            p.rows = r as u32;
        }
        if let Some(v) = d.get("BlackIs1") {
            p.black_is1 = as_bool(v, "BlackIs1")?;
        }
        if let Some(v) = d.get("EncodedByteAlign") {
            p.byte_align = as_bool(v, "EncodedByteAlign")?;
        }
        if let Some(v) = d.get("EndOfLine") {
            p.eoline = as_bool(v, "EndOfLine")?;
        }
        if let Some(v) = d.get("EndOfBlock") {
            p.eoblock = as_bool(v, "EndOfBlock")?;
        }
        Ok(p)
    }
}

fn as_i64(v: &PdfObject) -> Option<i64> {
    match v {
        PdfObject::Integer(i) => Some(*i),
        PdfObject::Real(r) if r.is_finite() && r.fract() == 0.0 => Some(*r as i64),
        _ => None,
    }
}

fn as_bool(v: &PdfObject, key: &str) -> Result<bool> {
    match v {
        PdfObject::Boolean(b) => Ok(*b),
        PdfObject::Integer(i) => Ok(*i != 0),
        PdfObject::Null => Ok(false),
        other => Err(PdfError::filter_error(
            0,
            format!("CCITT: /{key} must be a bool, got {other:?}"),
        )),
    }
}

/// Hard cap shared with the decoder so a hostile `/DecodeParms` cannot
/// trigger huge allocations.
pub(super) const MAX_COLUMNS: u32 = 1 << 16;
