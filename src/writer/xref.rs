//! XRef table and trailer serialization (ISO 32000-1 §7.5.4, §7.5.5).

use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::serializer::serialize_dict;

// ── XRef entry ────────────────────────────────────────────────────────────────

/// Format a single 20-byte in-use XRef entry.
///
/// Format: `oooooooooo ggggg n \r\n`
pub fn format_inuse_entry(offset: u64, generation: u16) -> [u8; 20] {
    let mut entry = [b' '; 20];
    // 10-digit offset
    let off_str = format!("{:010}", offset);
    entry[..10].copy_from_slice(off_str.as_bytes());
    entry[10] = b' ';
    // 5-digit generation
    let gen_str = format!("{:05}", generation);
    entry[11..16].copy_from_slice(gen_str.as_bytes());
    entry[16] = b' ';
    entry[17] = b'n';
    entry[18] = b' ';
    entry[19] = b'\n';
    entry
}

/// Format the mandatory free-head entry for object 0.
///
/// Always: `0000000000 65535 f \r\n`
pub fn format_free_head() -> [u8; 20] {
    let mut entry = [b' '; 20];
    entry[..10].copy_from_slice(b"0000000000");
    entry[10] = b' ';
    entry[11..16].copy_from_slice(b"65535");
    entry[16] = b' ';
    entry[17] = b'f';
    entry[18] = b' ';
    entry[19] = b'\n';
    entry
}

// ── Subsection grouping ───────────────────────────────────────────────────────

/// Group a sorted list of `(obj_id, offset)` pairs into contiguous subsections.
///
/// Returns `Vec<(start_id, Vec<(id, offset)>)>`.
fn group_subsections(mut entries: Vec<(u32, u64)>) -> Vec<(u32, Vec<(u32, u64)>)> {
    entries.sort_by_key(|(id, _)| *id);
    let mut subsections: Vec<(u32, Vec<(u32, u64)>)> = Vec::new();
    for (id, off) in entries {
        if let Some(last) = subsections.last_mut() {
            let expected_next = last.0 + last.1.len() as u32;
            if id == expected_next {
                last.1.push((id, off));
                continue;
            }
        }
        subsections.push((id, vec![(id, off)]));
    }
    subsections
}

// ── Public serializers ────────────────────────────────────────────────────────

/// Write a traditional XRef section (multiple subsections if IDs are non-contiguous).
///
/// `entries` — pairs of `(obj_id, absolute_byte_offset)` for in-use objects.
/// The free-head entry for object 0 is automatically prepended when object 0
/// is not present in `entries`.
pub fn write_xref_section(entries: &[(u32, u64)], out: &mut Vec<u8>) {
    out.extend_from_slice(b"xref\n");

    // Always write the free head for object 0 unless caller already covers it
    let has_zero = entries.iter().any(|(id, _)| *id == 0);
    let subsections = group_subsections(entries.to_vec());

    if !has_zero {
        // Prepend a standalone subsection "0 1" for the free head
        out.extend_from_slice(b"0 1\n");
        out.extend_from_slice(&format_free_head());
    }

    for (start_id, group) in &subsections {
        let header = format!("{} {}\n", start_id, group.len());
        out.extend_from_slice(header.as_bytes());
        for (_id, offset) in group {
            out.extend_from_slice(&format_inuse_entry(*offset, 0));
        }
    }
}

/// Write `trailer\n` followed by the serialized dictionary.
pub fn write_trailer_dict(dict: &PdfDict, out: &mut Vec<u8>) {
    out.extend_from_slice(b"trailer\n");
    serialize_dict(dict, out);
    out.push(b'\n');
}

/// Write `startxref\n{offset}\n%%EOF\n`.
pub fn write_startxref(xref_offset: u64, out: &mut Vec<u8>) {
    let s = format!("startxref\n{}\n%%EOF\n", xref_offset);
    out.extend_from_slice(s.as_bytes());
}

/// Build a complete XRef+trailer block and append it to `out`.
///
/// `entries` — `(obj_id, absolute_offset)` for objects in this section.
/// `trailer_dict` — already-built trailer dictionary.
/// `xref_start` — absolute byte offset where the xref section begins
///   (i.e., `out.len()` before this call, or equivalently the write position
///    in the full output file).
pub fn write_full_xref_and_trailer(
    entries: &[(u32, u64)],
    trailer_dict: &PdfDict,
    xref_start: u64,
    out: &mut Vec<u8>,
) {
    write_xref_section(entries, out);
    write_trailer_dict(trailer_dict, out);
    write_startxref(xref_start, out);
}

/// Build a trailer dictionary.
///
/// - `size` — highest object number in the file + 1 (`/Size`).
/// - `root_id` — catalog object number (`/Root`).
/// - `info_id` — optional info dict object number (`/Info`).
/// - `prev_offset` — if `Some(n)`, adds `/Prev n` for incremental updates.
pub fn build_trailer_dict(
    size: u32,
    root_id: u32,
    info_id: Option<u32>,
    prev_offset: Option<u64>,
) -> PdfDict {
    let mut dict = PdfDict::new();
    dict.insert("Size".to_owned(), PdfObject::Integer(size as i64));
    dict.insert("Root".to_owned(), PdfObject::Reference(root_id, 0));
    if let Some(id) = info_id {
        dict.insert("Info".to_owned(), PdfObject::Reference(id, 0));
    }
    if let Some(prev) = prev_offset {
        dict.insert("Prev".to_owned(), PdfObject::Integer(prev as i64));
    }
    dict
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_head_format() {
        let entry = format_free_head();
        assert_eq!(&entry[..10], b"0000000000");
        assert_eq!(&entry[11..16], b"65535");
        assert_eq!(entry[17], b'f');
        assert_eq!(entry[19], b'\n');
    }

    #[test]
    fn inuse_entry_format() {
        let entry = format_inuse_entry(12345, 0);
        assert_eq!(&entry[..10], b"0000012345");
        assert_eq!(&entry[11..16], b"00000");
        assert_eq!(entry[17], b'n');
        assert_eq!(entry[19], b'\n');
    }

    #[test]
    fn inuse_entry_is_20_bytes() {
        let entry = format_inuse_entry(0, 0);
        assert_eq!(entry.len(), 20);
    }

    #[test]
    fn xref_section_contains_free_head() {
        let mut out = Vec::new();
        write_xref_section(&[(1, 15), (2, 100)], &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("xref\n"));
        assert!(s.contains("0 1\n")); // free-head subsection
        assert!(s.contains("65535 f")); // free head content
    }

    #[test]
    fn xref_subsection_groups_consecutive() {
        let mut out = Vec::new();
        write_xref_section(&[(1, 15), (2, 100), (3, 200)], &mut out);
        let s = String::from_utf8(out).unwrap();
        // Consecutive 1,2,3 should be one subsection "1 3"
        assert!(s.contains("1 3\n"), "expected subsection header '1 3'");
    }

    #[test]
    fn xref_non_consecutive_creates_two_subsections() {
        let mut out = Vec::new();
        write_xref_section(&[(1, 15), (5, 500)], &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("1 1\n"));
        assert!(s.contains("5 1\n"));
    }

    #[test]
    fn startxref_format() {
        let mut out = Vec::new();
        write_startxref(9999, &mut out);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "startxref\n9999\n%%EOF\n");
    }

    #[test]
    fn trailer_dict_has_required_keys() {
        let d = build_trailer_dict(10, 1, Some(2), Some(500));
        assert!(d.contains_key("Size"));
        assert!(d.contains_key("Root"));
        assert!(d.contains_key("Info"));
        assert!(d.contains_key("Prev"));
    }

    #[test]
    fn trailer_dict_no_optional_keys() {
        let d = build_trailer_dict(5, 1, None, None);
        assert!(!d.contains_key("Info"));
        assert!(!d.contains_key("Prev"));
    }
}
