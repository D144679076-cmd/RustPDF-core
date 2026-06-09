//! PDF Page representation with inherited attribute resolution.
//!
//! A `Page` struct holds all the information needed to interpret or render
//! a single page: dimensions, rotation, resources, and content stream references.
//! Attributes not present on the page node itself are inherited from ancestor
//! /Pages nodes per ISO 32000-1 §7.7.3.4.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::catalog::resolve_inherited_attribute;

/// A rectangle defined by four coordinates [llx, lly, urx, ury].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Lower-left x coordinate.
    pub x1: f64,
    /// Lower-left y coordinate.
    pub y1: f64,
    /// Upper-right x coordinate.
    pub x2: f64,
    /// Upper-right y coordinate.
    pub y2: f64,
}

impl Rect {
    /// Width of the rectangle.
    pub fn width(&self) -> f64 {
        (self.x2 - self.x1).abs()
    }

    /// Height of the rectangle.
    pub fn height(&self) -> f64 {
        (self.y2 - self.y1).abs()
    }

    /// Parse a Rect from a PDF array of 4 numbers.
    pub fn from_pdf_array(arr: &[PdfObject]) -> Result<Self> {
        if arr.len() < 4 {
            return Err(PdfError::invalid_token(
                0,
                format!("rectangle array has {} elements, expected 4", arr.len()),
            ));
        }
        Ok(Rect {
            x1: pdf_number_to_f64(&arr[0])?,
            y1: pdf_number_to_f64(&arr[1])?,
            x2: pdf_number_to_f64(&arr[2])?,
            y2: pdf_number_to_f64(&arr[3])?,
        })
    }
}

/// Resources available to a page's content streams.
#[derive(Debug, Clone)]
pub struct PageResources {
    /// Font dictionary: name → font dict/ref.
    pub fonts: PdfDict,
    /// XObject dictionary: name → XObject ref.
    pub xobjects: PdfDict,
    /// ExtGState dictionary: name → graphics state dict.
    pub ext_g_state: PdfDict,
    /// ColorSpace dictionary: name → color space definition.
    pub color_spaces: PdfDict,
    /// Pattern dictionary.
    pub patterns: PdfDict,
    /// Shading dictionary.
    pub shadings: PdfDict,
    /// The raw resources dictionary for anything not covered above.
    pub raw: PdfDict,
}

impl PageResources {
    /// Build PageResources from a resolved /Resources dictionary.
    pub fn from_dict(dict: &PdfDict) -> Self {
        PageResources {
            fonts: extract_sub_dict(dict, "Font"),
            xobjects: extract_sub_dict(dict, "XObject"),
            ext_g_state: extract_sub_dict(dict, "ExtGState"),
            color_spaces: extract_sub_dict(dict, "ColorSpace"),
            patterns: extract_sub_dict(dict, "Pattern"),
            shadings: extract_sub_dict(dict, "Shading"),
            raw: dict.clone(),
        }
    }

    /// Empty resources (no fonts, no xobjects, etc.).
    pub fn empty() -> Self {
        PageResources {
            fonts: PdfDict::new(),
            xobjects: PdfDict::new(),
            ext_g_state: PdfDict::new(),
            color_spaces: PdfDict::new(),
            patterns: PdfDict::new(),
            shadings: PdfDict::new(),
            raw: PdfDict::new(),
        }
    }
}

/// A fully resolved PDF page with all inherited attributes applied.
#[derive(Debug, Clone)]
pub struct Page {
    /// The page's media box (required, defines page boundaries).
    pub media_box: Rect,
    /// Optional crop box (defaults to media_box if absent).
    pub crop_box: Rect,
    /// Page rotation in degrees (0, 90, 180, 270).
    pub rotate: i32,
    /// Page resources (fonts, images, graphics states, etc.).
    pub resources: PageResources,
    /// References to content stream objects for this page.
    pub content_refs: Vec<PdfObject>,
    /// The raw page dictionary.
    pub dict: PdfDict,
}

impl Page {
    /// Build a Page from a page dictionary, resolving inherited attributes.
    pub fn from_dict(doc: &PdfDocument, page_dict: &PdfDict) -> Result<Self> {
        let media_box = resolve_rect(doc, page_dict, "MediaBox")?
            .ok_or_else(|| PdfError::invalid_token(0, "page has no /MediaBox (even inherited)"))?;

        let crop_box = resolve_rect(doc, page_dict, "CropBox")?.unwrap_or(media_box);

        let rotate = resolve_inherited_attribute(doc, page_dict, "Rotate")?
            .and_then(|obj| obj.as_integer())
            .map(|r| r as i32)
            .unwrap_or(0);

        let resources = match resolve_inherited_attribute(doc, page_dict, "Resources")? {
            Some(PdfObject::Dictionary(d)) => PageResources::from_dict(&d),
            _ => PageResources::empty(),
        };

        let content_refs = match page_dict.get("Contents") {
            Some(PdfObject::Array(arr)) => arr.clone(),
            Some(obj) => vec![obj.clone()],
            None => vec![],
        };

        Ok(Page {
            media_box,
            crop_box,
            rotate,
            resources,
            content_refs,
            dict: page_dict.clone(),
        })
    }

    /// Decode and concatenate all content streams for this page.
    ///
    /// Multiple content streams are concatenated with a newline separator
    /// as specified in ISO 32000-1 §7.8.2.
    ///
    /// For indirect-reference content streams (the common case) this uses
    /// `doc.get_stream_data`, which caches decoded bytes by object ID so
    /// repeated calls (e.g. one per render tile) skip FlateDecode entirely.
    pub fn decode_contents(&self, doc: &PdfDocument) -> Result<Vec<u8>> {
        let mut combined = Vec::new();

        for content_ref in &self.content_refs {
            match content_ref {
                PdfObject::Reference(id, _gen) => {
                    // Cached path: get_stream_data memoises decoded bytes by ID.
                    if !combined.is_empty() {
                        combined.push(b'\n');
                    }
                    combined.extend_from_slice(&doc.get_stream_data(*id)?);
                }
                _ => {
                    // Rare: direct inline stream — resolve and decode without caching.
                    match doc.resolve(content_ref)? {
                        PdfObject::Stream(stream) => {
                            if !combined.is_empty() {
                                combined.push(b'\n');
                            }
                            combined.extend_from_slice(&stream.decode_with_doc(doc)?);
                        }
                        _ => {
                            log::warn!("page /Contents element is not a stream, skipping");
                        }
                    }
                }
            }
        }

        Ok(combined)
    }

    /// Effective page width after applying rotation.
    pub fn width(&self) -> f64 {
        if self.rotate == 90 || self.rotate == 270 {
            self.media_box.height()
        } else {
            self.media_box.width()
        }
    }

    /// Effective page height after applying rotation.
    pub fn height(&self) -> f64 {
        if self.rotate == 90 || self.rotate == 270 {
            self.media_box.width()
        } else {
            self.media_box.height()
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_rect(doc: &PdfDocument, page_dict: &PdfDict, key: &str) -> Result<Option<Rect>> {
    match resolve_inherited_attribute(doc, page_dict, key)? {
        Some(PdfObject::Array(arr)) => Ok(Some(Rect::from_pdf_array(&arr)?)),
        _ => Ok(None),
    }
}

fn pdf_number_to_f64(obj: &PdfObject) -> Result<f64> {
    match obj {
        PdfObject::Integer(n) => Ok(*n as f64),
        PdfObject::Real(r) => Ok(*r),
        _ => Err(PdfError::invalid_token(
            0,
            format!("expected number in rectangle, found {:?}", obj),
        )),
    }
}

fn extract_sub_dict(parent: &PdfDict, key: &str) -> PdfDict {
    match parent.get(key) {
        Some(PdfObject::Dictionary(d)) => d.clone(),
        _ => PdfDict::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rect_from_array() {
        let arr = vec![
            PdfObject::Integer(0),
            PdfObject::Integer(0),
            PdfObject::Integer(612),
            PdfObject::Integer(792),
        ];
        let rect = Rect::from_pdf_array(&arr).unwrap();
        assert_eq!(rect.x1, 0.0);
        assert_eq!(rect.y1, 0.0);
        assert_eq!(rect.x2, 612.0);
        assert_eq!(rect.y2, 792.0);
        assert_eq!(rect.width(), 612.0);
        assert_eq!(rect.height(), 792.0);
    }

    #[test]
    fn test_rect_from_real_array() {
        let arr = vec![
            PdfObject::Real(0.0),
            PdfObject::Real(0.0),
            PdfObject::Real(595.276),
            PdfObject::Real(841.89),
        ];
        let rect = Rect::from_pdf_array(&arr).unwrap();
        assert!((rect.width() - 595.276).abs() < 0.001);
        assert!((rect.height() - 841.89).abs() < 0.001);
    }

    #[test]
    fn test_rect_too_few_elements() {
        let arr = vec![PdfObject::Integer(0), PdfObject::Integer(0)];
        assert!(Rect::from_pdf_array(&arr).is_err());
    }

    #[test]
    fn test_page_resources_empty() {
        let res = PageResources::empty();
        assert!(res.fonts.is_empty());
        assert!(res.xobjects.is_empty());
    }

    #[test]
    fn test_page_from_minimal_pdf() {
        let pdf_bytes = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R >> endobj\n\
xref\n\
0 4\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000139 00000 n \n\
trailer\n\
<< /Size 4 /Root 1 0 R >>\n\
startxref\n\
186\n\
%%EOF\n";

        let doc = PdfDocument::parse(pdf_bytes.to_vec()).unwrap();
        let catalog = super::super::catalog::Catalog::from_document(&doc).unwrap();
        let page_dict = catalog.get_page_dict(&doc, 0).unwrap();
        let page = Page::from_dict(&doc, &page_dict).unwrap();

        assert_eq!(page.media_box.width(), 612.0);
        assert_eq!(page.media_box.height(), 792.0);
        assert_eq!(page.rotate, 0);
    }
}
