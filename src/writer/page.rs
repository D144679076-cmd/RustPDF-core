//! Page dictionary and resource assembly.

use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::document::PdfWriter;
use crate::writer::streams::make_flate_stream;

/// Builder for a single PDF page.
///
/// Collect fonts, XObjects, and content streams, then call
/// [`build`](Self::build) to write everything to a [`PdfWriter`] and get back
/// the page dictionary object ID.
#[derive(Debug, Default)]
pub struct PageBuilder {
    media_box: [f64; 4],
    crop_box: Option<[f64; 4]>,
    rotate: Option<i32>,
    /// `/Font` resource entries: resource-name → font object ID.
    fonts: Vec<(String, u32)>,
    /// `/XObject` resource entries: resource-name → XObject ID.
    xobjects: Vec<(String, u32)>,
    /// Uncompressed content stream bytes (in order).
    content_streams: Vec<Vec<u8>>,
}

impl PageBuilder {
    /// Create a page with the given width and height (in points).
    /// MediaBox is `[0, 0, width, height]`.
    pub fn new(width: f64, height: f64) -> Self {
        Self {
            media_box: [0.0, 0.0, width, height],
            ..Default::default()
        }
    }

    /// Override the default MediaBox.
    pub fn set_media_box(&mut self, rect: [f64; 4]) -> &mut Self {
        self.media_box = rect;
        self
    }

    /// Set an optional CropBox.
    pub fn set_crop_box(&mut self, rect: [f64; 4]) -> &mut Self {
        self.crop_box = Some(rect);
        self
    }

    /// Set page rotation (must be 0, 90, 180, or 270).
    pub fn set_rotate(&mut self, degrees: i32) -> &mut Self {
        self.rotate = Some(degrees);
        self
    }

    /// Add a font resource entry.
    ///
    /// `name` is the resource name used inside content streams (e.g. `"F1"`).
    /// `obj_id` is the font dictionary object ID in the writer.
    pub fn add_font(&mut self, name: impl Into<String>, obj_id: u32) -> &mut Self {
        self.fonts.push((name.into(), obj_id));
        self
    }

    /// Add an XObject resource entry (image or form).
    pub fn add_xobject(&mut self, name: impl Into<String>, obj_id: u32) -> &mut Self {
        self.xobjects.push((name.into(), obj_id));
        self
    }

    /// Append a content stream (uncompressed bytes).
    ///
    /// Multiple streams are written as separate stream objects and referenced
    /// as an array in `/Contents`.
    pub fn add_content(&mut self, bytes: Vec<u8>) -> &mut Self {
        self.content_streams.push(bytes);
        self
    }

    /// Write all content streams and the page dictionary into `writer`.
    ///
    /// Returns the object ID of the page dictionary.
    pub fn build(self, parent_id: u32, writer: &mut PdfWriter) -> Result<u32> {
        // Write content streams.
        let mut content_refs: Vec<PdfObject> = Vec::new();
        for raw in self.content_streams {
            let stream = make_flate_stream(&raw, PdfDict::new())?;
            let id = writer.add_object(PdfObject::Stream(Box::new(stream)));
            content_refs.push(PdfObject::Reference(id, 0));
        }

        // Build /Resources dict.
        let mut resources: PdfDict = PdfDict::new();

        if !self.fonts.is_empty() {
            let mut font_dict = PdfDict::new();
            for (name, id) in &self.fonts {
                font_dict.insert(name.clone(), PdfObject::Reference(*id, 0));
            }
            resources.insert("Font".to_owned(), PdfObject::Dictionary(font_dict));
        }

        if !self.xobjects.is_empty() {
            let mut xobj_dict = PdfDict::new();
            for (name, id) in &self.xobjects {
                xobj_dict.insert(name.clone(), PdfObject::Reference(*id, 0));
            }
            resources.insert("XObject".to_owned(), PdfObject::Dictionary(xobj_dict));
        }

        // Build page dictionary.
        let mut page: PdfDict = PdfDict::new();
        page.insert("Type".to_owned(), PdfObject::Name("Page".to_owned()));
        page.insert("Parent".to_owned(), PdfObject::Reference(parent_id, 0));

        // MediaBox
        let mb = self.media_box;
        page.insert(
            "MediaBox".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Real(mb[0]),
                PdfObject::Real(mb[1]),
                PdfObject::Real(mb[2]),
                PdfObject::Real(mb[3]),
            ]),
        );

        // CropBox (optional)
        if let Some(cb) = self.crop_box {
            page.insert(
                "CropBox".to_owned(),
                PdfObject::Array(vec![
                    PdfObject::Real(cb[0]),
                    PdfObject::Real(cb[1]),
                    PdfObject::Real(cb[2]),
                    PdfObject::Real(cb[3]),
                ]),
            );
        }

        // Rotate (optional)
        if let Some(rot) = self.rotate {
            page.insert("Rotate".to_owned(), PdfObject::Integer(rot as i64));
        }

        // Resources
        if !resources.is_empty() {
            page.insert("Resources".to_owned(), PdfObject::Dictionary(resources));
        }

        // Contents: single reference or array
        match content_refs.len() {
            0 => {} // no /Contents key for empty pages
            1 => {
                page.insert("Contents".to_owned(), content_refs.remove(0));
            }
            _ => {
                page.insert("Contents".to_owned(), PdfObject::Array(content_refs));
            }
        }

        Ok(writer.add_object(PdfObject::Dictionary(page)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_page_dict_has_required_keys() {
        let mut writer = PdfWriter::new();
        let parent_id = writer.add_object(PdfObject::Null); // placeholder
        let page_id = PageBuilder::new(595.0, 842.0)
            .build(parent_id, &mut writer)
            .unwrap();
        let obj = writer.get_object(page_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert_eq!(d.get("Type"), Some(&PdfObject::Name("Page".to_owned())));
            assert!(d.contains_key("MediaBox"));
            assert!(d.contains_key("Parent"));
            // No /Contents for empty page
            assert!(!d.contains_key("Contents"));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn page_with_font_has_resources() {
        let mut writer = PdfWriter::new();
        let parent_id = writer.add_object(PdfObject::Null);
        let font_id = writer.add_object(PdfObject::Null);
        let mut builder = PageBuilder::new(595.0, 842.0);
        builder.add_font("F1", font_id);
        let page_id = builder.build(parent_id, &mut writer).unwrap();
        let obj = writer.get_object(page_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            assert!(d.contains_key("Resources"));
            if let Some(PdfObject::Dictionary(res)) = d.get("Resources") {
                assert!(res.contains_key("Font"));
            } else {
                panic!("expected /Resources dict");
            }
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn single_content_stream_is_direct_ref() {
        let mut writer = PdfWriter::new();
        let parent_id = writer.add_object(PdfObject::Null);
        let mut builder = PageBuilder::new(595.0, 842.0);
        builder.add_content(b"q Q".to_vec());
        let page_id = builder.build(parent_id, &mut writer).unwrap();
        let obj = writer.get_object(page_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            match d.get("Contents") {
                Some(PdfObject::Reference(_, _)) => {} // good: direct reference
                other => panic!("expected direct ref, got {:?}", other),
            }
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn multiple_content_streams_become_array() {
        let mut writer = PdfWriter::new();
        let parent_id = writer.add_object(PdfObject::Null);
        let mut builder = PageBuilder::new(595.0, 842.0);
        builder.add_content(b"q Q".to_vec());
        builder.add_content(b"q Q".to_vec());
        let page_id = builder.build(parent_id, &mut writer).unwrap();
        let obj = writer.get_object(page_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            match d.get("Contents") {
                Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 2),
                other => panic!("expected array, got {:?}", other),
            }
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn media_box_values_match_constructor() {
        let mut writer = PdfWriter::new();
        let parent_id = writer.add_object(PdfObject::Null);
        let page_id = PageBuilder::new(612.0, 792.0)
            .build(parent_id, &mut writer)
            .unwrap();
        let obj = writer.get_object(page_id).unwrap();
        if let PdfObject::Dictionary(d) = obj {
            if let Some(PdfObject::Array(mb)) = d.get("MediaBox") {
                assert_eq!(mb[0], PdfObject::Real(0.0));
                assert_eq!(mb[2], PdfObject::Real(612.0));
                assert_eq!(mb[3], PdfObject::Real(792.0));
            } else {
                panic!("expected array for MediaBox");
            }
        } else {
            panic!("expected dict");
        }
    }
}
