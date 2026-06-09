//! Object ID remapping — shift all indirect references by a constant offset.
//!
//! Used by the merge engine to copy objects from multiple source documents
//! into a single writer pool without ID collisions.

use crate::parser::objects::{PdfDict, PdfObject};

/// Recursively shift every `Reference(id, gen)` inside `obj` by `+offset`.
///
/// All non-reference types are cloned unchanged. Arrays, dictionaries, and
/// stream dictionaries are walked recursively. Stream `raw_data` is byte
/// content and is copied verbatim.
pub fn remap_object(obj: &PdfObject, offset: u32) -> PdfObject {
    match obj {
        PdfObject::Reference(id, gen) => PdfObject::Reference(id + offset, *gen),
        PdfObject::Array(arr) => {
            PdfObject::Array(arr.iter().map(|o| remap_object(o, offset)).collect())
        }
        PdfObject::Dictionary(dict) => PdfObject::Dictionary(remap_dict(dict, offset)),
        PdfObject::Stream(s) => {
            let mut new_stream = *s.clone();
            new_stream.dict = remap_dict(&s.dict, offset);
            PdfObject::Stream(Box::new(new_stream))
        }
        other => other.clone(),
    }
}

/// Remap all values in a dictionary, leaving keys unchanged.
pub fn remap_dict(dict: &PdfDict, offset: u32) -> PdfDict {
    dict.iter()
        .map(|(k, v)| (k.clone(), remap_object(v, offset)))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_shifts_by_offset() {
        let obj = PdfObject::Reference(5, 0);
        assert_eq!(remap_object(&obj, 100), PdfObject::Reference(105, 0));
    }

    #[test]
    fn non_reference_unchanged() {
        assert_eq!(
            remap_object(&PdfObject::Integer(42), 99),
            PdfObject::Integer(42)
        );
        assert_eq!(
            remap_object(&PdfObject::Name("Foo".into()), 99),
            PdfObject::Name("Foo".into())
        );
    }

    #[test]
    fn array_recurses() {
        let obj = PdfObject::Array(vec![
            PdfObject::Reference(1, 0),
            PdfObject::Integer(7),
            PdfObject::Reference(2, 0),
        ]);
        assert_eq!(
            remap_object(&obj, 10),
            PdfObject::Array(vec![
                PdfObject::Reference(11, 0),
                PdfObject::Integer(7),
                PdfObject::Reference(12, 0),
            ])
        );
    }

    #[test]
    fn dict_recurses() {
        let mut d = PdfDict::new();
        d.insert("A".to_owned(), PdfObject::Reference(3, 0));
        d.insert("B".to_owned(), PdfObject::Name("X".to_owned()));
        let remapped = remap_dict(&d, 20);
        assert_eq!(remapped["A"], PdfObject::Reference(23, 0));
        assert_eq!(remapped["B"], PdfObject::Name("X".to_owned()));
    }

    #[test]
    fn nested_dict_in_array() {
        let mut inner = PdfDict::new();
        inner.insert("Ref".to_owned(), PdfObject::Reference(10, 0));
        let obj = PdfObject::Array(vec![PdfObject::Dictionary(inner)]);
        let remapped = remap_object(&obj, 5);
        if let PdfObject::Array(arr) = remapped {
            if let PdfObject::Dictionary(d) = &arr[0] {
                assert_eq!(d["Ref"], PdfObject::Reference(15, 0));
            } else {
                panic!("expected dictionary inside array");
            }
        } else {
            panic!("expected array");
        }
    }
}
