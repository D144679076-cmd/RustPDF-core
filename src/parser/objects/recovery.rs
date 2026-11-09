//! XRef recovery / reconstruction for damaged PDF files.
//!
//! ISO 32000-2 §7.5.4 recovery path: when the primary (and alternate) XRef
//! chains cannot be loaded or do not verify, rebuild the object index by
//! scanning the file body for indirect-object markers (`N G obj`) and
//! synthesizing the trailer from every `trailer` dict, XRef-stream dict, or
//! (last resort) the `/Type /Catalog` object itself.
//!
//! Design mirrors pdf.js `repair.js` / MuPDF `pdf-xref.c` reconstruction:
//! recovery runs only when the normal path fails, so healthy documents take
//! exactly zero extra cost.
//!
//! Recovery ladder (each rung falls through on failure):
//! 1. Retry every `startxref` occurrence present anywhere in the file (not
//!    just the last 1024 bytes) — a truncated or corrupted *final* trailer
//!    often leaves earlier incremental sections intact.
//! 2. Try every XRef-section candidate: `xref` table keyword occurrences and
//!    backtracked `/Type /XRef` stream objects, newest first.
//! 3. Full body-marker scan: map every `N G obj` footprint (last occurrence
//!    wins — incremental updates append newer versions) and synthesize the
//!    trailer from collected dicts, else from the catalog object.
//!
//! Known limitations (tracked in `.doc/xref-recovery-2026-11-09.md`):
//! - Objects that live inside object streams (type-2 XRef-stream entries)
//!   cannot be body-scanned when the stream is damaged (contents are
//!   compressed); recovery only locates direct (uncompressed) objects.
//! - A compressed stream whose bytes coincidentally contain an `N G obj`
//!   byte-run can shadow a real marker. Mitigated by engaging this path only
//!   on otherwise-unopenable files; a false positive surfaces as a parse
//!   failure at object-access time — the same trade-off pdf.js makes.

use super::{build_xref, parse_indirect_object, parse_object_from_lexer, skip_pdf_whitespace};
use crate::error::Result;
use crate::parser::lexer::Lexer;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Load the XRef map + trailer with a recovery fallback engaged on failure.
///
/// `start` is the `startxref` offset (`None` when even the keyword itself is
/// missing (or unusable) — reconstruction then runs immediately). Tries the
/// normal `/Prev` chain first, verifies the recovered `/Root` reference
/// actually marks a `N G obj` header at its mapped offset, and runs the
/// reconstruction ladder when that does not produce a usable index.
pub(crate) fn load_xref(
    data: &[u8],
    start: Option<usize>,
) -> Result<(HashMap<u32, super::XRefEntry>, super::PdfDict)> {
    if let Some(start) = start {
        if let Ok((xref, trailer)) = build_xref(data, start) {
            if root_resolves(data, &xref, &trailer) {
                return Ok((xref, trailer));
            }
            log::warn!(
                "XRef loaded but /Root does not verify at its mapped offset — engaging recovery"
            );
        }
    }

    match rebuild(data) {
        Ok(v) => {
            log::warn!("XRef recovery engaged successfully");
            Ok(v)
        }
        Err(e) => {
            log::warn!("XRef recovery could not reconstruct a usable index: {}", e);
            // Keep whatever *structurally* valid state the normal path had —
            // the resolver produces precise errors for missing slots.
            match start {
                Some(start) => build_xref(data, start),
                None => Err(e),
            }
        }
    }
}

/// Run the full recovery ladder: startxref retries, XRef-section rescans,
/// then full body-marker reconstruction.
fn rebuild(data: &[u8]) -> Result<(HashMap<u32, super::XRefEntry>, super::PdfDict)> {
    // Rung 1: every `startxref` token anywhere in the file, newest first.
    let mut startxrefs = find_all(data, b"startxref");
    startxrefs.sort_unstable();
    for pos in startxrefs.into_iter().rev() {
        if let Some(off) = startxref_value(data, pos) {
            if let Ok((xref, trailer)) = build_xref(data, off) {
                if !xref.is_empty() && root_resolves(data, &xref, &trailer) {
                    return Ok((xref, trailer));
                }
            }
        }
    }

    // Rung 2: every XRef-section candidate, newest first. This recovers files
    // whose current trailer chain is corrupt while an older incremental
    // section still parses.
    let sections = xref_section_candidates(data);
    for off in sections.into_iter().rev() {
        if let Ok((xref, trailer)) = build_xref(data, off) {
            if !xref.is_empty() && root_resolves(data, &xref, &trailer) {
                return Ok((xref, trailer));
            }
        }
    }

    // Rung 3: full body-marker reconstruction.
    rebuild_by_scan(data)
}

// ---------------------------------------------------------------------------
// Rung-1/2 candidate discovery
// ---------------------------------------------------------------------------

/// Parse the decimal offset that follows a `startxref` keyword found at `pos`.
/// Rejects offsets beyond end-of-file — those are exactly what recovery is for.
fn startxref_value(data: &[u8], pos: usize) -> Option<usize> {
    let after = skip_pdf_whitespace(&data[pos + b"startxref".len()..]);
    let end = after
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .unwrap_or(after.len());
    if end == 0 {
        return None;
    }
    std::str::from_utf8(&after[..end])
        .ok()?
        .parse()
        .ok()
        .filter(|&n: &usize| n < data.len())
}

/// Collect byte offsets that plausibly begin a parseable XRef section:
/// `xref` table keywords and backtracked `/Type /XRef` stream objects.
fn xref_section_candidates(data: &[u8]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();

    let mut from = 0;
    while let Some(i) = find(data, from, b"xref") {
        from = i + 4;
        // Skip fragment hits (e.g. "startxref"): `xref` must be a keyword with
        // a whitespace boundary on both sides.
        if (i > 0 && !is_ws(data[i - 1])) || !data.get(i + 4).map(|&b| is_ws(b)).unwrap_or(false) {
            continue;
        }
        out.push(i);
    }

    let mut from = 0;
    while let Some(i) = find(data, from, b"/XRef") {
        from = i + 5;
        // Skip longer names starting with XRef (e.g. "/XRefStm").
        if data
            .get(i + 5)
            .map(|b| b.is_ascii_alphanumeric())
            .unwrap_or(false)
        {
            continue;
        }
        if let Some((_, _, obj_off)) = nearest_obj_ending_before(data, i) {
            if !out.contains(&obj_off) {
                out.push(obj_off);
            }
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// Rung 3: full body-marker reconstruction
// ---------------------------------------------------------------------------

fn rebuild_by_scan(data: &[u8]) -> Result<(HashMap<u32, super::XRefEntry>, super::PdfDict)> {
    let map = scan_object_footprints(data);
    if map.is_empty() {
        return Err(crate::error::PdfError::invalid_token(
            0,
            "recovery: no indirect objects found in file body",
        ));
    }
    log::debug!("recovery: {} object footprints found", map.len());

    let candidates = trailer_candidates(data);

    // Prefer the newest candidate whose /Root actually resolves in `map`.
    for dict in candidates.iter().rev() {
        if root_ref(dict).is_some() && root_resolves(data, &map, dict) {
            return Ok((map, dict.clone()));
        }
    }
    // Next: newest dict carrying /Root even if the marker check failed.
    if let Some(dict) = candidates.iter().rev().find(|d| root_ref(d).is_some()) {
        return Ok((map, dict.clone()));
    }
    // Then: merge all candidate dicts, newest wins per key, older fills gaps.
    if !candidates.is_empty() {
        let mut merged = super::PdfDict::new();
        for dict in candidates.iter().rev() {
            for (k, v) in dict.iter() {
                merged.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        return Ok((map, merged));
    }

    // Last resort: synthesize a minimal trailer from the catalog object.
    if let Some((id, gen, _)) = find_catalog(data) {
        let mut trailer = super::PdfDict::new();
        trailer.insert("Root".to_string(), super::PdfObject::Reference(id, gen));
        return Ok((map, trailer));
    }

    Err(crate::error::PdfError::invalid_token(
        0,
        "recovery: objects found but no trailer, /Root, or /Type /Catalog present",
    ))
}

/// Collect trailer-dict candidates in file order:
/// (a) every parseable dict after a `trailer` keyword,
/// (b) every XRef-stream object dict that carries trailer keys.
fn trailer_candidates(data: &[u8]) -> Vec<super::PdfDict> {
    let mut out = Vec::new();

    // (a) classic `trailer` keyword dictionaries.
    let mut from = 0;
    while let Some(i) = find(data, from, b"trailer") {
        from = i + 7;
        // Guaranteed keyword fragmentation guard the same way as `xref` above.
        if (i > 0 && !is_ws(data[i - 1])) || !data.get(i + 7).map(|&b| is_ws(b)).unwrap_or(false) {
            continue;
        }
        let slice = skip_pdf_whitespace(&data[from..]);
        let mut lexer = Lexer::new(slice);
        if let Ok(super::PdfObject::Dictionary(d)) = parse_object_from_lexer(&mut lexer) {
            out.push(d);
        }
    }

    // (b) XRef-stream dicts (PDF 1.5+) — trailer keys live in the stream dict.
    let mut from = 0;
    while let Some(i) = find(data, from, b"/XRef") {
        from = i + 5;
        if data
            .get(i + 5)
            .map(|b| b.is_ascii_alphanumeric())
            .unwrap_or(false)
        {
            continue;
        }
        let Some((_, _, obj_off)) = nearest_obj_ending_before(data, i) else {
            continue;
        };
        if let Ok(super::PdfObject::Dictionary(d)) = parse_indirect_object(data, obj_off) {
            let is_xref_stream =
                matches!(d.get("Type"), Some(super::PdfObject::Name(t)) if t == "XRef");
            if is_xref_stream
                && (d.contains_key("Root") || d.contains_key("Size") || d.contains_key("Encrypt"))
            {
                out.push(d);
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Footprint scan
// ---------------------------------------------------------------------------

/// Scan the entire file for `N G obj` indirect-object headers.
///
/// Returns the footprint index: object number → entry. The *last* occurrence
/// of each object number wins: incremental updates append newer versions at
/// higher offsets, so this matches the "newest wins" convention of the
/// `/Prev` chain walk.
fn scan_object_footprints(data: &[u8]) -> HashMap<u32, super::XRefEntry> {
    let mut map: HashMap<u32, super::XRefEntry> = HashMap::new();
    let mut from = 0;
    while let Some(i) = find(data, from, b"obj") {
        from = i + 3;
        // Note: "endobj" also contains "obj" — `obj_header_ending_at` rejects
        // it because the preceding bytes are not `digits ws digits obj`.
        if let Some((id, gen, obj_off)) = obj_header_ending_at(data, i + 3) {
            map.insert(
                id,
                super::XRefEntry::InUse {
                    offset: obj_off as u64,
                    generation: gen,
                },
            );
        }
    }
    map
}

/// If the bytes ending exactly at `end` (just past the `obj` keyword) form a
/// valid `N G obj` header, return `(obj_num, generation, header_start)`.
///
/// Parses backwards so it also works when called from a body scan or from a
/// backtracked name token (e.g. `/Type /Catalog` → nearest `N G obj` header).
fn obj_header_ending_at(data: &[u8], end: usize) -> Option<(u32, u16, usize)> {
    if end < 3 || &data[end - 3..end] != b"obj" {
        return None;
    }
    let mut p = end - 3;

    // Whitespace run between gen number and `obj`.
    if p == 0 || !skip_ws_backward(data, &mut p) {
        return None;
    }

    // Gen number digits.
    let gen_end = p;
    digits_backward(data, &mut p);
    let gen: u16 = match std::str::from_utf8(&data[p..gen_end]) {
        Ok(s) => s.parse().ok()?,
        Err(_) => return None,
    };
    if p == 0 || !skip_ws_backward(data, &mut p) {
        return None;
    }

    // Object number digits.
    let num_end = p;
    digits_backward(data, &mut p);
    let id: u32 = match std::str::from_utf8(&data[p..num_end]) {
        Ok(s) => s.parse().ok()?,
        Err(_) => return None,
    };

    // The bytes before the header must be a boundary so we don't claim e.g.
    // "endobj" or a PDF string literal as an object marker.
    if !header_boundary(data, p) {
        return None;
    }
    Some((id, gen, p))
}

/// Back up across a whitespace run. Returns true when at least one was skipped.
fn skip_ws_backward(data: &[u8], p: &mut usize) -> bool {
    let mut any = false;
    while *p > 0 && is_ws(data[*p - 1]) {
        *p -= 1;
        any = true;
    }
    any
}

/// Back up across a decimal digit run. `p` ends at the first digit.
fn digits_backward(data: &[u8], p: &mut usize) {
    while *p > 0 && data[*p - 1].is_ascii_digit() {
        *p -= 1;
    }
}

/// The byte preceding an `N G obj` header must be a delimiter or whitespace
/// (or the file start) so a marker cannot hide inside another token.
fn header_boundary(data: &[u8], p: usize) -> bool {
    if p == 0 {
        return true;
    }
    let b = data[p - 1];
    is_ws(b)
        || matches!(
            b,
            b'%' | b'/' | b'<' | b'>' | b'(' | b')' | b'[' | b']' | b'{' | b'}'
        )
}

// ---------------------------------------------------------------------------
// Trailer / catalog discovery
// ---------------------------------------------------------------------------

/// The `obj` header ending closest before `lim` (exclusive).
fn nearest_obj_ending_before(data: &[u8], lim: usize) -> Option<(u32, u16, usize)> {
    if lim < 3 {
        return None;
    }
    let rel = data[..lim].windows(3).rposition(|w| w == b"obj")?;
    obj_header_ending_at(data, rel + 3)
}

/// Forward incremental search for `needle` starting at (absolute) `from`.
fn find(data: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= data.len() {
        return None;
    }
    data[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn find_all(data: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = find(data, from, needle) {
        out.push(i);
        from = i + needle.len();
    }
    out
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

// ---------------------------------------------------------------------------
// Verification helpers
// ---------------------------------------------------------------------------

fn root_ref(trailer: &super::PdfDict) -> Option<(u32, u16)> {
    match trailer.get("Root") {
        Some(super::PdfObject::Reference(id, gen)) => Some((*id, *gen)),
        _ => None,
    }
}

/// Verify the /Root reference: its entry must exist (direct or compressed)
/// and, when direct, the mapped offset must actually hold the `id G obj`
/// header.
fn root_resolves(
    data: &[u8],
    xref: &HashMap<u32, super::XRefEntry>,
    trailer: &super::PdfDict,
) -> bool {
    let Some((id, _gen)) = root_ref(trailer) else {
        return false;
    };
    match xref.get(&id) {
        Some(super::XRefEntry::InUse { offset, .. }) => offset_holds_object(data, *offset, id),
        Some(super::XRefEntry::Compressed { .. }) => true,
        Some(super::XRefEntry::Free) | None => false,
    }
}

/// Cheap structural check that a byte offset holds the `id <gen> obj` header.
fn offset_holds_object(data: &[u8], off: u64, id: u32) -> bool {
    let off = off as usize;
    if off >= data.len() {
        return false;
    }
    let slice = skip_pdf_whitespace(&data[off..]);
    let num_end = slice
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .unwrap_or(slice.len());
    if num_end == 0 {
        return false;
    }
    if std::str::from_utf8(&slice[..num_end])
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        != Some(id)
    {
        return false;
    }
    let rest = skip_pdf_whitespace(&slice[num_end..]);
    let gen_end = rest.iter().position(|&b| !b.is_ascii_digit()).unwrap_or(0);
    skip_pdf_whitespace(&rest[gen_end..]).starts_with(b"obj")
}

// ---------------------------------------------------------------------------
// Catalog discovery (no trailer anywhere in file)
// ---------------------------------------------------------------------------

/// Locate a `/Type /Catalog` indirect object; returns
/// `(object number, generation, byte offset)`. Scans `/Catalog` name
/// occurrences newest-first and validates by parsing the surrounding object.
fn find_catalog(data: &[u8]) -> Option<(u32, u16, usize)> {
    let mut from = 0;
    let mut hits = Vec::new();
    while let Some(i) = find(data, from, b"Catalog") {
        hits.push(i);
        from = i + 7;
    }
    for i in hits.into_iter().rev() {
        let Some((id, gen, obj_off)) = nearest_obj_ending_before(data, i) else {
            continue;
        };
        let Ok(super::PdfObject::Dictionary(d)) = parse_indirect_object(data, obj_off) else {
            continue;
        };
        if matches!(d.get("Type"), Some(super::PdfObject::Name(n)) if n == "Catalog") {
            return Some((id, gen, obj_off));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Unit tests (per repo rule R5: happy + error paths)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::{PdfObject, XRefEntry};
    use super::*;
    type PdfDict = super::super::PdfDict;

    /// Two plain objects followed by nothing else.
    fn body() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"%PDF-1.4\n");
        v.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        v.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        v
    }

    fn trailer_dict(id: u32, gen: u16) -> PdfDict {
        let mut d = PdfDict::new();
        d.insert("Root".to_string(), PdfObject::Reference(id, gen));
        d
    }

    #[test]
    fn scan_finds_last_object_footprint() {
        let mut data = body();
        let pos = data.len();
        // Duplicate object 1 later in the file (incremental-style update).
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
        let footprints = scan_object_footprints(&data);
        match footprints.get(&1) {
            Some(XRefEntry::InUse { offset, .. }) => {
                assert!(*offset as usize >= pos);
            }
            e => panic!("expected InUse, got {:?}", e),
        }
        assert!(footprints.contains_key(&2));
    }

    #[test]
    fn obj_header_rejects_endobj() {
        let data = b"3 0 obj\n<< /Type /Pages >>\nendobj\n";
        // "3 0 obj" ends at offset 7 → a valid header.
        let (id, gen, off) = obj_header_ending_at(data, 7).expect("valid header");
        assert_eq!((id, gen, off), (3, 0, 0));
        // The "obj" inside "endobj" must be rejected.
        assert!(obj_header_ending_at(data, data.len() - 1).is_none());
    }

    #[test]
    fn rebuild_from_no_startxref() {
        let mut data = body();
        data.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n%%EOF\n");
        let (xref, trailer) = rebuild(&data).expect("recovery should succeed");
        assert!(xref.contains_key(&1) && xref.contains_key(&2));
        assert_eq!(trailer.get("Root"), Some(&PdfObject::Reference(1, 0)));
    }

    #[test]
    fn rebuild_from_stale_startxref() {
        // Valid XRef table + trailer, but `startxref` points past end of file.
        let mut data = body();
        data.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \r\n0000000009 00000 n \r\n0000000058 00000 n \r\ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n999999\n%%EOF\n");
        let (xref, _) = rebuild(&data).expect("rung 1/2 must recover fresh startxref");
        match xref.get(&1) {
            Some(XRefEntry::InUse { offset, generation }) => {
                assert_eq!((*offset, *generation), (9, 0));
            }
            e => panic!("object 1 not recovered: {:?}", e),
        }
    }

    #[test]
    fn rebuild_synthesizes_trailer_from_catalog() {
        // No XRef table, no trailer; catalog still discoverable.
        let data = body();
        let (xref, trailer) = rebuild(&data).expect("recovery from bare body");
        assert!(xref.contains_key(&1));
        assert_eq!(trailer.get("Root"), Some(&PdfObject::Reference(1, 0)));
    }

    #[test]
    fn rebuild_fails_cleanly_on_garbage() {
        let data: &[u8] = b"nothing to see here, no objects at all\n%%EOF\n";
        assert!(rebuild(data).is_err());
    }

    #[test]
    fn startxref_value_out_of_bounds_is_rejected() {
        let data = b"startxref\n999999\n%%EOF\n";
        let pos = find(data, 0, b"startxref").unwrap();
        assert_eq!(startxref_value(data, pos), None);
    }

    #[test]
    fn root_marker_mismatch_not_resolved() {
        // Object 1 claimed at offset 999 — the body has no such header there.
        let xref: HashMap<u32, XRefEntry> = [(
            1,
            XRefEntry::InUse {
                offset: 999,
                generation: 0,
            },
        )]
        .into();
        let trailer = trailer_dict(1, 0);
        let data = body();
        assert!(!root_resolves(&data, &xref, &trailer));
    }

    #[test]
    fn root_marker_match_resolves() {
        let data = body();
        let off = data
            .windows(4)
            .position(|w| w == b"1 0 ")
            .expect("marker present");
        let xref: HashMap<u32, XRefEntry> = [(
            1,
            XRefEntry::InUse {
                offset: off as u64,
                generation: 0,
            },
        )]
        .into();
        let trailer = trailer_dict(1, 0);
        assert!(root_resolves(&data, &xref, &trailer));
    }
}
