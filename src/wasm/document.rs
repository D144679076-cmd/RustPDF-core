//! WasmDocument — load and inspect a PDF (read-only operations).

use wasm_bindgen::prelude::*;

use super::{json_opt_str, json_str, outline_to_json};

// ---------------------------------------------------------------------------
// WasmDocument — load and inspect a PDF
// ---------------------------------------------------------------------------

/// A loaded PDF document.
///
/// Create with [`WasmDocument::parse`] or [`WasmDocument::parse_with_password`].
#[wasm_bindgen]
pub struct WasmDocument {
    pub(crate) doc: crate::parser::objects::PdfDocument,
}

#[wasm_bindgen]
impl WasmDocument {
    /// Parse PDF bytes into a document.
    pub fn parse(bytes: &[u8]) -> Result<WasmDocument, JsError> {
        log::info!("[pdf-core] WasmDocument::parse — {} bytes", bytes.len());
        let doc = crate::parser::objects::PdfDocument::parse(bytes.to_vec()).map_err(|e| {
            log::error!("[pdf-core] WasmDocument::parse failed: {}", e);
            JsError::new(&e.to_string())
        })?;
        log::info!("[pdf-core] WasmDocument::parse — ok");
        Ok(WasmDocument { doc })
    }

    /// Parse an encrypted PDF using a password.
    #[cfg(feature = "crypto")]
    pub fn parse_with_password(bytes: &[u8], password: &[u8]) -> Result<WasmDocument, JsError> {
        let doc =
            crate::parser::objects::PdfDocument::parse_with_password(bytes.to_vec(), password)
                .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(WasmDocument { doc })
    }

    /// Returns the number of pages in the document.
    pub fn page_count(&self) -> Result<usize, JsError> {
        self.doc
            .page_count()
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Returns document metadata as a JSON string.
    ///
    /// Return the document's operation permissions as a JSON object.
    ///
    /// For unencrypted documents all permissions are `true` (no restrictions).
    /// Keys: `can_print`, `can_modify`, `can_copy_text`, `can_annotate`,
    /// `can_fill_forms`, `can_assemble`.
    #[cfg(feature = "crypto")]
    pub fn get_permissions(&self) -> String {
        let perms = self
            .doc
            .permissions()
            .unwrap_or(crate::crypto::handler::Permissions {
                can_print: true,
                can_modify: true,
                can_copy_text: true,
                can_annotate: true,
                can_fill_forms: true,
                can_extract_accessibility: true,
                can_assemble: true,
                can_print_high_quality: true,
            });
        format!(
            r#"{{"can_print":{},"can_modify":{},"can_copy_text":{},"can_annotate":{},"can_fill_forms":{},"can_assemble":{}}}"#,
            perms.can_print,
            perms.can_modify,
            perms.can_copy_text,
            perms.can_annotate,
            perms.can_fill_forms,
            perms.can_assemble,
        )
    }

    /// Keys: `title`, `author`, `subject`, `keywords`, `creator`, `producer`.
    pub fn get_metadata(&self) -> String {
        match crate::document::metadata::Metadata::from_document(&self.doc) {
            Ok(meta) => format!(
                r#"{{"title":{},"author":{},"subject":{},"keywords":{},"creator":{},"producer":{}}}"#,
                json_opt_str(&meta.title),
                json_opt_str(&meta.author),
                json_opt_str(&meta.subject),
                json_opt_str(&meta.keywords),
                json_opt_str(&meta.creator),
                json_opt_str(&meta.producer),
            ),
            Err(_) => "{}".to_string(),
        }
    }

    /// Returns the document outline (bookmarks) as a JSON string.
    ///
    /// Each item: `{ title, dest_page, open, children }`.
    pub fn get_outline(&self) -> String {
        use crate::document::catalog::Catalog;
        use crate::document::outline::parse_outlines;
        let catalog = match Catalog::from_document(&self.doc) {
            Ok(c) => c,
            Err(_) => return "[]".to_string(),
        };
        match parse_outlines(&self.doc, &catalog.dict) {
            Ok(items) => outline_to_json(&items),
            Err(_) => "[]".to_string(),
        }
    }

    /// Returns the page size as `[width_pt, height_pt]` from the page's MediaBox.
    ///
    /// Values are in PDF user-space points (1 pt = 1/72 inch).
    /// Common sizes: A4 = `[595, 842]`, Letter = `[612, 792]`.
    pub fn page_size(&self, page_index: usize) -> Result<Vec<f64>, JsError> {
        use crate::document::catalog::Catalog;
        use crate::document::page::Page;
        let catalog =
            Catalog::from_document(&self.doc).map_err(|e| JsError::new(&e.to_string()))?;
        let page_dict = catalog
            .get_page_dict(&self.doc, page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let page =
            Page::from_dict(&self.doc, &page_dict).map_err(|e| JsError::new(&e.to_string()))?;
        Ok(vec![page.media_box.width(), page.media_box.height()])
    }

    /// Returns a JSON array of annotations on the given page (0-based index).
    ///
    /// Each element: `{ subtype, rect: [x1,y1,x2,y2], color: [r,g,b], quad_points?: [...] }`.
    /// Fields use PDF user-space coordinates (origin bottom-left).
    /// Returns `"[]"` when the page has no annotations or the array is missing.
    pub fn list_annotations(&self, page_index: usize) -> String {
        use crate::document::catalog::Catalog;
        use crate::parser::objects::PdfObject;

        let catalog = match Catalog::from_document(&self.doc) {
            Ok(c) => c,
            Err(_) => return "[]".to_string(),
        };
        let page_dict = match catalog.get_page_dict(&self.doc, page_index) {
            Ok(d) => d,
            Err(_) => return "[]".to_string(),
        };

        let annots_raw = match page_dict.get("Annots") {
            Some(v) => match self.doc.resolve(v) {
                Ok(resolved) => resolved,
                Err(_) => return "[]".to_string(),
            },
            None => return "[]".to_string(),
        };

        let annot_refs = match &annots_raw {
            PdfObject::Array(arr) => arr.clone(),
            _ => return "[]".to_string(),
        };

        let mut parts: Vec<String> = Vec::new();
        for annot_ref in &annot_refs {
            let annot_obj = match self.doc.resolve(annot_ref) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let annot_dict = match annot_obj.as_dict() {
                Some(d) => d.clone(),
                None => continue,
            };

            let subtype = match annot_dict.get("Subtype") {
                Some(PdfObject::Name(n)) => n.clone(),
                _ => continue,
            };

            let rect = match annot_dict.get("Rect") {
                Some(v) => match self.doc.resolve(v) {
                    Ok(PdfObject::Array(arr)) => {
                        let nums: Vec<f64> = arr
                            .iter()
                            .map(|o| match o {
                                PdfObject::Real(f) => *f,
                                PdfObject::Integer(i) => *i as f64,
                                _ => 0.0,
                            })
                            .collect();
                        if nums.len() >= 4 {
                            format!("[{},{},{},{}]", nums[0], nums[1], nums[2], nums[3])
                        } else {
                            "[0,0,0,0]".to_string()
                        }
                    }
                    _ => "[0,0,0,0]".to_string(),
                },
                None => "[0,0,0,0]".to_string(),
            };

            let color = match annot_dict.get("C") {
                Some(v) => match self.doc.resolve(v) {
                    Ok(PdfObject::Array(arr)) => {
                        let nums: Vec<f64> = arr
                            .iter()
                            .map(|o| match o {
                                PdfObject::Real(f) => *f,
                                PdfObject::Integer(i) => *i as f64,
                                _ => 0.0,
                            })
                            .collect();
                        if nums.len() >= 3 {
                            format!("[{},{},{}]", nums[0], nums[1], nums[2])
                        } else {
                            "[1,1,0]".to_string()
                        }
                    }
                    _ => "[1,1,0]".to_string(),
                },
                None => "[1,1,0]".to_string(),
            };

            let quad_field = match annot_dict.get("QuadPoints") {
                Some(v) => match self.doc.resolve(v) {
                    Ok(PdfObject::Array(arr)) => {
                        let nums: Vec<String> = arr
                            .iter()
                            .map(|o| match o {
                                PdfObject::Real(f) => f.to_string(),
                                PdfObject::Integer(i) => i.to_string(),
                                _ => "0".to_string(),
                            })
                            .collect();
                        format!(",\"quad_points\":[{}]", nums.join(","))
                    }
                    _ => String::new(),
                },
                None => String::new(),
            };
            parts.push(format!(
                r#"{{"subtype":{},"rect":{},"color":{}{}}}"#,
                json_str(&subtype),
                rect,
                color,
                quad_field,
            ));
        }

        format!("[{}]", parts.join(","))
    }

    /// Returns a JSON array of text words with position data for the given page.
    ///
    /// Each element: `{ "text", "x", "y", "width", "height", "font_size", "font_name" }`.
    /// Coordinates are in PDF user-space (origin bottom-left, y increases upward).
    /// `height` is approximated as `font_size`.
    pub fn extract_text_spans(&self, page_index: usize) -> Result<String, JsError> {
        use crate::document::catalog::Catalog;
        use crate::document::page::Page;
        use crate::text::TextExtractor;

        let catalog =
            Catalog::from_document(&self.doc).map_err(|e| JsError::new(&e.to_string()))?;
        let page_dict = catalog
            .get_page_dict(&self.doc, page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let page =
            Page::from_dict(&self.doc, &page_dict).map_err(|e| JsError::new(&e.to_string()))?;
        let extractor = TextExtractor::extract_from_page(&self.doc, &page)
            .map_err(|e| JsError::new(&e.to_string()))?;

        let words = extractor.words();
        let parts: Vec<String> = words
            .iter()
            .map(|w| {
                format!(
                    r#"{{"text":{},"x":{:.4},"y":{:.4},"width":{:.4},"height":{:.4},"font_size":{:.4},"font_name":{}}}"#,
                    super::json_str(&w.text),
                    w.x,
                    w.y,
                    w.width,
                    w.font_size,
                    w.font_size,
                    super::json_str(&w.font_name),
                )
            })
            .collect();
        Ok(format!("[{}]", parts.join(",")))
    }

    /// Resolve a PDF font resource key (e.g. `"F1"`) to the actual font name
    /// (e.g. `"Helvetica-Bold"`) by walking the page's `/Resources/Font` dict.
    ///
    /// Returns `"Helvetica"` as a safe fallback if the key cannot be resolved.
    pub fn resolve_font_name(&self, page_index: usize, resource_key: &str) -> String {
        crate::editor::resolve_font_name(&self.doc, page_index, resource_key)
    }

    /// Search all pages for occurrences of `query`.
    ///
    /// Returns a JSON array of match objects:
    /// `[{"page_index": N, "text": "...", "bounds": [x1,y1,x2,y2]}, ...]`.
    /// Coordinates are in PDF user-space (origin bottom-left).
    /// Set `case_sensitive` to `false` for case-insensitive matching.
    pub fn search_text(&self, query: &str, case_sensitive: bool) -> Result<String, JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.doc, |p| p.can_copy_text, "copy_text")?;
        let results = crate::text::search_document(&self.doc, query, case_sensitive)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let mut json = String::from("[");
        for (i, r) in results.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                r#"{{"page_index":{},"text":{},"bounds":[{},{},{},{}]}}"#,
                r.page_index,
                super::json_str(&r.text),
                r.bounds[0],
                r.bounds[1],
                r.bounds[2],
                r.bounds[3],
            ));
        }
        json.push(']');
        Ok(json)
    }

    /// Extracts plain text from a page (0-based index).
    pub fn extract_text(&self, page_index: usize) -> Result<String, JsError> {
        #[cfg(feature = "crypto")]
        super::check_permission(&self.doc, |p| p.can_copy_text, "copy_text")?;
        use crate::document::catalog::Catalog;
        use crate::document::page::Page;
        use crate::text::TextExtractor;

        let catalog =
            Catalog::from_document(&self.doc).map_err(|e| JsError::new(&e.to_string()))?;
        let page_dict = catalog
            .get_page_dict(&self.doc, page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let page =
            Page::from_dict(&self.doc, &page_dict).map_err(|e| JsError::new(&e.to_string()))?;
        let extractor = TextExtractor::extract_from_page(&self.doc, &page)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(extractor.plain_text())
    }
}

// ---------------------------------------------------------------------------
// WasmRenderer — rasterise pages (requires `wasm-render` / `render` feature)
// ---------------------------------------------------------------------------

/// Result of rendering a page: pixel dimensions and raw RGBA bytes.
#[cfg(feature = "render")]
#[wasm_bindgen]
pub struct RenderResult {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    data: Vec<u8>,
}

#[cfg(feature = "render")]
#[wasm_bindgen]
impl RenderResult {
    /// Returns the raw RGBA byte buffer as a `Uint8Array`.
    ///
    /// Each pixel is four bytes `[R, G, B, A]` in row-major order.
    pub fn rgba_bytes(&self) -> js_sys::Uint8Array {
        js_sys::Uint8Array::from(self.data.as_slice())
    }
}

#[cfg(feature = "render")]
impl RenderResult {
    /// Construct from raw render output (used by `WasmEditor::render_page`).
    pub(crate) fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        RenderResult {
            width,
            height,
            data,
        }
    }

    /// Raw RGBA bytes (native/test access; JS uses `rgba_bytes`).
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.data
    }
}

/// Stateless renderer — call [`WasmRenderer::render_page`] directly.
#[cfg(feature = "render")]
#[wasm_bindgen]
pub struct WasmRenderer;

#[cfg(feature = "render")]
#[wasm_bindgen]
impl WasmRenderer {
    /// Render a page to RGBA pixels.
    ///
    /// `page_index` is 0-based.  `scale` controls resolution: `1.0` = 72 DPI,
    /// `2.0` = 144 DPI.
    pub fn render_page(
        doc: &WasmDocument,
        page_index: usize,
        scale: f64,
    ) -> Result<RenderResult, JsError> {
        let (w, h, data) = crate::render::render_page_rgba(&doc.doc, page_index, scale)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(RenderResult {
            width: w,
            height: h,
            data,
        })
    }
}
