//! Annotation CRUD — add/delete/flatten PDF annotations on a page.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::content_builder::ContentBuilder;
use crate::writer::streams::make_flate_stream;

use super::document_editor::PdfEditor;

// ── Annotation type ───────────────────────────────────────────────────────────

/// Supported annotation subtypes.
#[derive(Debug, Clone)]
pub enum AnnotationType {
    /// A sticky-note comment.
    Text {
        contents: String,
        /// Whether the annotation is initially open.
        open: bool,
    },
    /// Highlight, strikeout, or underline markup.
    Highlight {
        color: [f64; 3],
        /// Quad points describing the covered area (ISO 32000-1 §12.5.6.10).
        quad_points: Vec<f64>,
    },
    /// Strikeout markup.
    StrikeOut {
        color: [f64; 3],
        quad_points: Vec<f64>,
    },
    /// Underline markup.
    Underline {
        color: [f64; 3],
        quad_points: Vec<f64>,
    },
    /// Hyperlink to a URI.
    Link { uri: String },
    /// Free-text annotation (appears directly on the page).
    FreeText {
        contents: String,
        /// Default appearance string (e.g. `"/Helvetica 12 Tf 0 0 0 rg"`).
        default_appearance: String,
        /// Text justification: 0 = left, 1 = centre, 2 = right (PDF /Q field).
        align: Option<u8>,
    },
    /// Freehand ink drawing.
    Ink {
        /// Strokes — each stroke is a list of `[x, y]` points.
        ink_list: Vec<Vec<[f64; 2]>>,
    },
    /// Marks an area for permanent redaction (ISO 32000-2 §12.5.6.23).
    ///
    /// Call `apply_redactions()` to burn the redaction in and produce a clean
    /// PDF with all marked content removed.
    Redact {
        /// RGB fill color of the overlay rectangle after redaction [0.0–1.0].
        /// Default black `[0.0, 0.0, 0.0]`.
        overlay_color: [f64; 3],
    },
    /// Stamp annotation — imprints a named label (e.g. "Approved", "Draft") on the page.
    Stamp {
        /// Predefined or custom stamp label.
        name: String,
        /// RGB ink color [0.0–1.0].
        color: [f64; 3],
    },
    /// Polygon annotation — a closed (or explicitly open) polygon overlay.
    Polygon {
        /// Vertex coordinates as `[x, y]` pairs in page space.
        vertices: Vec<[f64; 2]>,
        /// When `false` the polygon is rendered open (same visual as Polyline with close).
        closed: bool,
        stroke_color: [f64; 3],
        fill_color: Option<[f64; 3]>,
        line_width: f64,
    },
    /// Polyline annotation — an open multi-segment line overlay.
    Polyline {
        /// Vertex coordinates as `[x, y]` pairs in page space.
        vertices: Vec<[f64; 2]>,
        stroke_color: [f64; 3],
        line_width: f64,
    },
    /// FileAttachment annotation — embeds a file in the PDF at a page location.
    ///
    /// The raw `file_data` and `filename` are written as an EmbeddedFile stream
    /// by `add_annotation`; callers should not set `prebuilt_ref` directly.
    FileAttachment {
        /// Raw bytes of the file to embed.
        file_data: Vec<u8>,
        /// Filename shown in the attachment panel.
        filename: String,
        /// Annotation tooltip / contents string.
        description: String,
        /// Icon name: "PushPin", "Graph", "Paperclip", or "Tag".
        icon_name: String,
    },
    /// Caret annotation — marks a text insertion point.
    Caret {
        /// Symbol: "None" or "P" (paragraph mark).
        symbol: String,
    },
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builder for a PDF annotation dictionary.
pub struct AnnotationBuilder {
    annot_type: AnnotationType,
    rect: [f64; 4],
    author: Option<String>,
    subject: Option<String>,
    /// Pre-built indirect object ID used by `FileAttachment` to reference the
    /// embedded-file Filespec object created in `add_annotation`.
    prebuilt_ref: Option<u32>,
}

impl AnnotationBuilder {
    /// Create a new annotation of the given type at `rect` (`[x1, y1, x2, y2]`).
    pub fn new(annot_type: AnnotationType, rect: [f64; 4]) -> Self {
        Self {
            annot_type,
            rect,
            author: None,
            subject: None,
            prebuilt_ref: None,
        }
    }

    /// Set the annotation author (`/T` in PDF).
    pub fn author(mut self, a: &str) -> Self {
        self.author = Some(a.to_owned());
        self
    }

    /// Set the annotation subject (`/Subj` in PDF).
    pub fn subject(mut self, s: &str) -> Self {
        self.subject = Some(s.to_owned());
        self
    }

    /// Build the annotation dictionary.
    pub fn build(self) -> PdfDict {
        let mut d = PdfDict::new();

        d.insert("Type".to_owned(), PdfObject::Name("Annot".to_owned()));
        // Flags: bit 2 (Print) = 4 — annotation prints by default.
        d.insert("Flags".to_owned(), PdfObject::Integer(4));

        // Rect
        d.insert(
            "Rect".to_owned(),
            PdfObject::Array(self.rect.iter().map(|&v| PdfObject::Real(v)).collect()),
        );

        // Optional common fields
        if let Some(author) = self.author {
            d.insert("T".to_owned(), PdfObject::String(author.into_bytes()));
        }
        if let Some(subj) = self.subject {
            d.insert("Subj".to_owned(), PdfObject::String(subj.into_bytes()));
        }

        // Type-specific fields
        match self.annot_type {
            AnnotationType::Text { contents, open } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("Text".to_owned()));
                d.insert(
                    "Contents".to_owned(),
                    PdfObject::String(contents.into_bytes()),
                );
                d.insert("Open".to_owned(), PdfObject::Boolean(open));
            }
            AnnotationType::Highlight { color, quad_points } => {
                d.insert(
                    "Subtype".to_owned(),
                    PdfObject::Name("Highlight".to_owned()),
                );
                insert_color(&mut d, &color);
                insert_quad_points(&mut d, &quad_points);
            }
            AnnotationType::StrikeOut { color, quad_points } => {
                d.insert(
                    "Subtype".to_owned(),
                    PdfObject::Name("StrikeOut".to_owned()),
                );
                insert_color(&mut d, &color);
                insert_quad_points(&mut d, &quad_points);
            }
            AnnotationType::Underline { color, quad_points } => {
                d.insert(
                    "Subtype".to_owned(),
                    PdfObject::Name("Underline".to_owned()),
                );
                insert_color(&mut d, &color);
                insert_quad_points(&mut d, &quad_points);
            }
            AnnotationType::Link { uri } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("Link".to_owned()));
                // Action dict for URI
                let mut action = PdfDict::new();
                action.insert("Type".to_owned(), PdfObject::Name("Action".to_owned()));
                action.insert("S".to_owned(), PdfObject::Name("URI".to_owned()));
                action.insert("URI".to_owned(), PdfObject::String(uri.into_bytes()));
                d.insert("A".to_owned(), PdfObject::Dictionary(action));
            }
            AnnotationType::FreeText {
                contents,
                default_appearance,
                align,
            } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("FreeText".to_owned()));
                d.insert(
                    "Contents".to_owned(),
                    PdfObject::String(contents.into_bytes()),
                );
                d.insert(
                    "DA".to_owned(),
                    PdfObject::String(default_appearance.into_bytes()),
                );
                if let Some(q) = align {
                    d.insert("Q".to_owned(), PdfObject::Integer(q as i64));
                }
            }
            AnnotationType::Ink { ink_list } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("Ink".to_owned()));
                let strokes: Vec<PdfObject> = ink_list
                    .iter()
                    .map(|stroke| {
                        PdfObject::Array(
                            stroke
                                .iter()
                                .flat_map(|[x, y]| [PdfObject::Real(*x), PdfObject::Real(*y)])
                                .collect(),
                        )
                    })
                    .collect();
                d.insert("InkList".to_owned(), PdfObject::Array(strokes));
            }
            AnnotationType::Redact { overlay_color } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("Redact".to_owned()));
                d.insert(
                    "IC".to_owned(),
                    PdfObject::Array(overlay_color.iter().map(|&v| PdfObject::Real(v)).collect()),
                );
            }
            AnnotationType::Stamp { name, color } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("Stamp".to_owned()));
                d.insert("Name".to_owned(), PdfObject::Name(name));
                insert_color(&mut d, &color);
            }
            AnnotationType::Polygon {
                vertices,
                closed: _,
                stroke_color,
                fill_color,
                line_width,
            } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("Polygon".to_owned()));
                d.insert(
                    "Vertices".to_owned(),
                    PdfObject::Array(
                        vertices
                            .iter()
                            .flat_map(|v| [PdfObject::Real(v[0]), PdfObject::Real(v[1])])
                            .collect(),
                    ),
                );
                insert_color(&mut d, &stroke_color);
                if let Some(ic) = fill_color {
                    d.insert("IC".to_owned(), color_array(&ic));
                }
                d.insert("BS".to_owned(), border_style_dict(line_width));
            }
            AnnotationType::Polyline {
                vertices,
                stroke_color,
                line_width,
            } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("PolyLine".to_owned()));
                d.insert(
                    "Vertices".to_owned(),
                    PdfObject::Array(
                        vertices
                            .iter()
                            .flat_map(|v| [PdfObject::Real(v[0]), PdfObject::Real(v[1])])
                            .collect(),
                    ),
                );
                insert_color(&mut d, &stroke_color);
                d.insert("BS".to_owned(), border_style_dict(line_width));
            }
            AnnotationType::FileAttachment {
                description,
                icon_name,
                ..
            } => {
                d.insert(
                    "Subtype".to_owned(),
                    PdfObject::Name("FileAttachment".to_owned()),
                );
                if let Some(fs_id) = self.prebuilt_ref {
                    d.insert("FS".to_owned(), PdfObject::Reference(fs_id, 0));
                }
                d.insert("Name".to_owned(), PdfObject::Name(icon_name));
                d.insert(
                    "Contents".to_owned(),
                    PdfObject::String(description.into_bytes()),
                );
            }
            AnnotationType::Caret { symbol } => {
                d.insert("Subtype".to_owned(), PdfObject::Name("Caret".to_owned()));
                d.insert("Sy".to_owned(), PdfObject::Name(symbol));
            }
        }

        d
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn insert_color(d: &mut PdfDict, color: &[f64; 3]) {
    d.insert(
        "C".to_owned(),
        PdfObject::Array(color.iter().map(|&v| PdfObject::Real(v)).collect()),
    );
}

fn insert_quad_points(d: &mut PdfDict, qp: &[f64]) {
    d.insert(
        "QuadPoints".to_owned(),
        PdfObject::Array(qp.iter().map(|&v| PdfObject::Real(v)).collect()),
    );
}

fn color_array(c: &[f64; 3]) -> PdfObject {
    PdfObject::Array(c.iter().map(|&v| PdfObject::Real(v)).collect())
}

fn border_style_dict(line_width: f64) -> PdfObject {
    let mut d = PdfDict::new();
    d.insert("Type".to_owned(), PdfObject::Name("Border".to_owned()));
    d.insert("W".to_owned(), PdfObject::Real(line_width));
    PdfObject::Dictionary(d)
}

/// Build and write an EmbeddedFile stream + Filespec dict to `editor`.
///
/// Returns the object ID of the Filespec dictionary (used as the `/FS` value
/// in the FileAttachment annotation).
fn build_embedded_file_stream(data: &[u8], filename: &str, editor: &mut PdfEditor) -> Result<u32> {
    use crate::parser::objects::PdfStream;
    use crate::writer::streams::encode_flate;

    let compressed = encode_flate(data)?;

    let mut ef_dict = PdfDict::new();
    ef_dict.insert(
        "Type".to_owned(),
        PdfObject::Name("EmbeddedFile".to_owned()),
    );
    ef_dict.insert(
        "Length".to_owned(),
        PdfObject::Integer(compressed.len() as i64),
    );
    ef_dict.insert(
        "Filter".to_owned(),
        PdfObject::Name("FlateDecode".to_owned()),
    );
    let mut params = PdfDict::new();
    params.insert("Size".to_owned(), PdfObject::Integer(data.len() as i64));
    ef_dict.insert("Params".to_owned(), PdfObject::Dictionary(params));
    let ef_id = editor.add_object(PdfObject::Stream(Box::new(PdfStream {
        dict: ef_dict,
        raw_data: compressed,
    })));

    let mut filespec = PdfDict::new();
    filespec.insert("Type".to_owned(), PdfObject::Name("Filespec".to_owned()));
    filespec.insert(
        "F".to_owned(),
        PdfObject::String(filename.as_bytes().to_vec()),
    );
    filespec.insert(
        "UF".to_owned(),
        PdfObject::String(filename.as_bytes().to_vec()),
    );
    let mut ef_ref = PdfDict::new();
    ef_ref.insert("F".to_owned(), PdfObject::Reference(ef_id, 0));
    filespec.insert("EF".to_owned(), PdfObject::Dictionary(ef_ref));

    Ok(editor.add_object(PdfObject::Dictionary(filespec)))
}

/// Generate appearance stream bytes for an annotation type.
///
/// Returns `None` for types with no visual appearance (Link, Redact).
/// Gated on the `forms` feature because the appearance functions live there.
#[cfg(feature = "forms")]
fn generate_ap_bytes(annot_type: &AnnotationType, rect: [f64; 4]) -> Option<Vec<u8>> {
    use crate::forms::appearance;

    match annot_type {
        AnnotationType::Text { .. } => Some(appearance::text_note_appearance(rect)),
        AnnotationType::Highlight { color, quad_points } => {
            let quads: Vec<[f64; 8]> = quad_points
                .chunks(8)
                .filter(|c| c.len() == 8)
                .map(|c| [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
                .collect();
            Some(appearance::highlight_appearance_quad(&quads, rect, *color))
        }
        AnnotationType::FreeText { contents, .. } => Some(appearance::freetext_appearance(
            contents,
            rect,
            10.0,
            [0.0, 0.0, 0.0],
        )),
        AnnotationType::Ink { ink_list } => {
            Some(appearance::ink_appearance(ink_list, rect, [0.0, 0.0, 0.0]))
        }
        AnnotationType::Stamp { name, color } => {
            Some(appearance::stamp_appearance(name, rect, *color))
        }
        AnnotationType::Polygon {
            vertices,
            stroke_color,
            fill_color,
            line_width,
            ..
        } => Some(appearance::polygon_appearance(
            vertices,
            rect,
            *stroke_color,
            *fill_color,
            *line_width,
        )),
        AnnotationType::Polyline {
            vertices,
            stroke_color,
            line_width,
        } => Some(appearance::polyline_appearance(
            vertices,
            rect,
            *stroke_color,
            *line_width,
        )),
        AnnotationType::Caret { .. } => Some(appearance::caret_appearance(rect)),
        AnnotationType::FileAttachment { icon_name, .. } => {
            Some(appearance::file_attachment_appearance(rect, icon_name))
        }
        // StrikeOut and Underline: visual AP deferred to a future phase.
        // Link and Redact: no AP stream needed.
        _ => None,
    }
}

// ── Public operations ─────────────────────────────────────────────────────────

/// Add an annotation to page `page_index`.
///
/// Writes the annotation dict as a new object, then updates the page's
/// `/Annots` array (or creates it). For `FileAttachment` annotations the
/// embedded-file stream and Filespec dict are written to the document before
/// building the annotation dict. Returns the annotation object ID.
pub fn add_annotation(
    editor: &mut PdfEditor,
    page_index: usize,
    mut annot: AnnotationBuilder,
) -> Result<u32> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "add_annotation")?;
    let (page_id, mut page_dict) = editor.get_page_dict(page_index)?;

    #[cfg(feature = "forms")]
    let rect = annot.rect;

    // Pre-process FileAttachment: build embedded file stream and Filespec dict
    // before building the annotation dict so the /FS reference is available.
    {
        let fs_id = if let AnnotationType::FileAttachment {
            file_data,
            filename,
            ..
        } = &annot.annot_type
        {
            Some(build_embedded_file_stream(file_data, filename, editor)?)
        } else {
            None
        };
        if let Some(id) = fs_id {
            annot.prebuilt_ref = Some(id);
        }
    }

    // Capture AP bytes before consuming `annot` (requires forms feature).
    #[cfg(feature = "forms")]
    let ap_bytes_opt: Option<Vec<u8>> = generate_ap_bytes(&annot.annot_type, rect);

    // Build the annotation dict, including a back-reference to the page.
    let mut annot_dict = annot.build();
    annot_dict.insert("P".to_owned(), PdfObject::Reference(page_id, 0));

    // Attach an appearance stream (/AP) when one was generated.
    #[cfg(feature = "forms")]
    if let Some(ap_bytes) = ap_bytes_opt {
        let w = rect[2] - rect[0];
        let h = rect[3] - rect[1];
        let mut stream_dict = PdfDict::new();
        stream_dict.insert("Subtype".to_owned(), PdfObject::Name("Form".to_owned()));
        stream_dict.insert(
            "BBox".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Real(0.0),
                PdfObject::Real(0.0),
                PdfObject::Real(w),
                PdfObject::Real(h),
            ]),
        );
        let ap_stream = make_flate_stream(&ap_bytes, stream_dict)?;
        let ap_id = editor.add_object(PdfObject::Stream(Box::new(ap_stream)));
        let mut ap_dict = PdfDict::new();
        ap_dict.insert("N".to_owned(), PdfObject::Reference(ap_id, 0));
        annot_dict.insert("AP".to_owned(), PdfObject::Dictionary(ap_dict));
    }

    let annot_id = editor.add_object(PdfObject::Dictionary(annot_dict));

    // Get or create the /Annots array.
    let mut annots: Vec<PdfObject> = match page_dict.get("Annots") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        Some(PdfObject::Reference(id, _gen)) => {
            // Annots stored as indirect object — resolve it.
            match editor.get_object(*id)? {
                PdfObject::Array(arr) => arr,
                _ => vec![],
            }
        }
        _ => vec![],
    };
    annots.push(PdfObject::Reference(annot_id, 0));

    page_dict.insert("Annots".to_owned(), PdfObject::Array(annots));
    editor.replace_object(page_id, PdfObject::Dictionary(page_dict));

    Ok(annot_id)
}

/// Remove the annotation object `annot_id` from page `page_index`.
///
/// The annotation object itself is not deleted (it becomes unreachable),
/// only the reference in `/Annots` is removed.
pub fn delete_annotation(editor: &mut PdfEditor, page_index: usize, annot_id: u32) -> Result<()> {
    let (page_id, mut page_dict) = editor.get_page_dict(page_index)?;

    let mut annots: Vec<PdfObject> = match page_dict.get("Annots") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => {
            return Err(PdfError::invalid_structure(format!(
                "annotation {} not found on page {} (no annotations present)",
                annot_id, page_index
            )));
        }
    };

    let before = annots.len();
    annots.retain(|o| match o {
        PdfObject::Reference(id, _) => *id != annot_id,
        _ => true,
    });

    if annots.len() == before {
        return Err(PdfError::invalid_structure(format!(
            "annotation {} not found on page {}",
            annot_id, page_index
        )));
    }

    page_dict.insert("Annots".to_owned(), PdfObject::Array(annots));
    editor.replace_object(page_id, PdfObject::Dictionary(page_dict));

    Ok(())
}

// ── Annotation flatten ────────────────────────────────────────────────────────

/// Flatten all annotations on a single page into the content stream.
///
/// Annotation visuals are appended to the page's `/Contents` stream and
/// `/Annots` is removed, so annotations appear in all viewers without a
/// separate annotation layer.
pub fn flatten_annotations(editor: &mut PdfEditor, page_index: usize) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "flatten_annotations")?;
    let (page_id, mut page_dict) = editor.get_page_dict(page_index)?;

    // Resolve /Annots — may be inline or an indirect reference.
    let annots: Vec<PdfObject> = match page_dict.get("Annots") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        Some(PdfObject::Reference(id, _)) => match editor.get_object(*id) {
            Ok(PdfObject::Array(arr)) => arr,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    if annots.is_empty() {
        return Ok(());
    }

    // Build drawing ops for each annotation.
    let mut cb = ContentBuilder::new();
    cb.save();
    for annot_ref in &annots {
        let annot_id = match annot_ref {
            PdfObject::Reference(n, _) => *n,
            _ => continue,
        };
        let annot_obj = match editor.get_object(annot_id) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let annot_dict = match &annot_obj {
            PdfObject::Dictionary(d) => d.clone(),
            _ => continue,
        };
        let subtype = match annot_dict.get("Subtype") {
            Some(PdfObject::Name(n)) => n.clone(),
            _ => continue,
        };
        flatten_one_annotation(&mut cb, &annot_dict, &subtype);
    }
    cb.restore();

    let drawing_bytes = cb.build();
    if !drawing_bytes.is_empty() {
        let stream = make_flate_stream(&drawing_bytes, PdfDict::new())?;
        let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

        // Append the new stream to /Contents (keep existing content intact).
        let new_contents = match page_dict.get("Contents") {
            Some(PdfObject::Array(arr)) => {
                let mut a = arr.clone();
                a.push(PdfObject::Reference(stream_id, 0));
                PdfObject::Array(a)
            }
            Some(single) => {
                PdfObject::Array(vec![single.clone(), PdfObject::Reference(stream_id, 0)])
            }
            None => PdfObject::Reference(stream_id, 0),
        };
        page_dict.insert("Contents".to_owned(), new_contents);
    }

    page_dict.shift_remove("Annots");
    editor.replace_object(page_id, PdfObject::Dictionary(page_dict));
    Ok(())
}

/// Flatten annotations on all pages.
///
/// Calls [`flatten_annotations`] for every page in the document.
pub fn flatten_all_annotations(editor: &mut PdfEditor) -> Result<()> {
    #[cfg(feature = "crypto")]
    crate::license::require(crate::license::Tier::Pro, "flatten_all_annotations")?;
    let page_count = editor.page_count()?;
    for i in 0..page_count {
        flatten_annotations(editor, i)?;
    }
    Ok(())
}

// ── Per-subtype drawing ───────────────────────────────────────────────────────

fn flatten_one_annotation(cb: &mut ContentBuilder, dict: &PdfDict, subtype: &str) {
    let rect = parse_rect(dict);
    let color = parse_color(dict, "C");

    match subtype {
        "Highlight" => {
            let quads = parse_quad_points(dict);
            cb.set_fill_rgb(color[0], color[1], color[2]);
            for quad in quads {
                let x = quad[0].min(quad[2]).min(quad[4]).min(quad[6]);
                let y = quad[1].min(quad[3]).min(quad[5]).min(quad[7]);
                let w = quad[0].max(quad[2]).max(quad[4]).max(quad[6]) - x;
                let h = quad[1].max(quad[3]).max(quad[5]).max(quad[7]) - y;
                cb.rect(x, y, w, h).fill();
            }
        }
        "StrikeOut" => {
            let quads = parse_quad_points(dict);
            cb.set_stroke_rgb(color[0], color[1], color[2])
                .set_line_width(1.0);
            for quad in quads {
                let mid_y = (quad[1] + quad[7]) / 2.0;
                cb.move_to(quad[0], mid_y).line_to(quad[2], mid_y).stroke();
            }
        }
        "Underline" => {
            let quads = parse_quad_points(dict);
            cb.set_stroke_rgb(color[0], color[1], color[2])
                .set_line_width(0.8);
            for quad in quads {
                cb.move_to(quad[0], quad[1])
                    .line_to(quad[2], quad[3])
                    .stroke();
            }
        }
        "FreeText" => {
            if let Some(PdfObject::String(contents)) = dict.get("Contents") {
                let text = String::from_utf8_lossy(contents);
                let font_size = 10.0_f64;
                cb.set_fill_rgb(0.0, 0.0, 0.0)
                    .begin_text()
                    .set_font("Helv", font_size)
                    .move_text_pos(rect[0] + 2.0, rect[1] + 2.0)
                    .show_text(text.as_bytes())
                    .end_text();
            }
        }
        "Ink" => {
            if let Some(PdfObject::Array(ink_list)) = dict.get("InkList") {
                cb.set_stroke_rgb(color[0], color[1], color[2])
                    .set_line_width(1.5);
                for stroke_obj in ink_list {
                    if let PdfObject::Array(pts) = stroke_obj {
                        let coords: Vec<f64> = pts
                            .iter()
                            .filter_map(|p| match p {
                                PdfObject::Real(r) => Some(*r),
                                PdfObject::Integer(i) => Some(*i as f64),
                                _ => None,
                            })
                            .collect();
                        if coords.len() >= 2 {
                            cb.move_to(coords[0], coords[1]);
                            let mut i = 2;
                            while i + 1 < coords.len() {
                                cb.line_to(coords[i], coords[i + 1]);
                                i += 2;
                            }
                            cb.stroke();
                        }
                    }
                }
            }
        }
        "Stamp" => {
            let name = match dict.get("Name") {
                Some(PdfObject::Name(n)) => n.clone(),
                _ => "STAMP".to_owned(),
            };
            let font_size = ((rect[3] - rect[1]) * 0.6).clamp(8.0, 24.0);
            cb.save()
                .set_stroke_rgb(color[0], color[1], color[2])
                .set_line_width(1.5)
                .rect(
                    rect[0] + 2.0,
                    rect[1] + 2.0,
                    rect[2] - rect[0] - 4.0,
                    rect[3] - rect[1] - 4.0,
                )
                .stroke()
                .set_fill_rgb(color[0], color[1], color[2])
                .begin_text()
                .set_font("Helv", font_size)
                .move_text_pos(rect[0] + 4.0, rect[1] + 2.0)
                .show_text(name.as_bytes())
                .end_text()
                .restore();
        }
        "Polygon" | "PolyLine" => {
            if let Some(PdfObject::Array(verts)) = dict.get("Vertices") {
                let coords: Vec<f64> = verts.iter().filter_map(pdf_num).collect();
                if coords.len() >= 4 {
                    let lw = if let Some(PdfObject::Dictionary(bs)) = dict.get("BS") {
                        match bs.get("W") {
                            Some(PdfObject::Real(w)) => *w,
                            Some(PdfObject::Integer(w)) => *w as f64,
                            _ => 1.0,
                        }
                    } else {
                        1.0
                    };
                    cb.set_stroke_rgb(color[0], color[1], color[2])
                        .set_line_width(lw)
                        .move_to(coords[0], coords[1]);
                    let mut i = 2;
                    while i + 1 < coords.len() {
                        cb.line_to(coords[i], coords[i + 1]);
                        i += 2;
                    }
                    if subtype == "Polygon" {
                        cb.close_path();
                    }
                    cb.stroke();
                }
            }
        }
        "Caret" => {
            let w = rect[2] - rect[0];
            let h = rect[3] - rect[1];
            cb.save()
                .set_stroke_rgb(0.0, 0.0, 0.5)
                .set_line_width(1.0)
                .move_to(rect[0], rect[1])
                .line_to(rect[0] + w / 2.0, rect[1] + h)
                .line_to(rect[0] + w, rect[1])
                .stroke()
                .restore();
        }
        "FileAttachment" => {
            let cx = (rect[0] + rect[2]) / 2.0;
            let h = rect[3] - rect[1];
            let w = rect[2] - rect[0];
            cb.save()
                .set_fill_rgb(0.5, 0.5, 0.5)
                .rect(cx - 2.0, rect[1], 4.0, h * 0.6)
                .fill()
                .set_fill_rgb(0.3, 0.3, 0.3)
                .rect(cx - w * 0.25, rect[1] + h * 0.55, w * 0.5, h * 0.4)
                .fill()
                .restore();
        }
        // Link, Text (sticky-note icon), Redact, Widget — non-visual or handled elsewhere.
        _ => {}
    }
}

// ── Dictionary parsing helpers ────────────────────────────────────────────────

fn parse_rect(dict: &PdfDict) -> [f64; 4] {
    match dict.get("Rect") {
        Some(PdfObject::Array(a)) => {
            let n: Vec<f64> = a.iter().filter_map(pdf_num).collect();
            if n.len() >= 4 {
                [n[0], n[1], n[2], n[3]]
            } else {
                [0.0; 4]
            }
        }
        _ => [0.0; 4],
    }
}

fn parse_color(dict: &PdfDict, key: &str) -> [f64; 3] {
    match dict.get(key) {
        Some(PdfObject::Array(a)) if a.len() >= 3 => [
            pdf_num(&a[0]).unwrap_or(0.0),
            pdf_num(&a[1]).unwrap_or(0.0),
            pdf_num(&a[2]).unwrap_or(0.0),
        ],
        _ => [0.0, 0.0, 0.0],
    }
}

/// Parse `/QuadPoints` into groups of 8 coordinates.
fn parse_quad_points(dict: &PdfDict) -> Vec<[f64; 8]> {
    match dict.get("QuadPoints") {
        Some(PdfObject::Array(a)) => {
            let nums: Vec<f64> = a.iter().filter_map(pdf_num).collect();
            nums.chunks(8)
                .filter(|c| c.len() == 8)
                .map(|c| [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
                .collect()
        }
        _ => vec![],
    }
}

fn pdf_num(o: &PdfObject) -> Option<f64> {
    match o {
        PdfObject::Real(r) => Some(*r),
        PdfObject::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::document_editor::PdfEditor;
    use crate::editor::page_editor::add_blank_page;
    use crate::parser::objects::PdfDocument;
    use std::fs;

    fn load(name: &str) -> Vec<u8> {
        fs::read(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    fn editor_with_page() -> (PdfEditor, Vec<u8>) {
        let data = load("minimal.pdf");
        let original = data.clone();
        let mut editor = PdfEditor::open(data).unwrap();
        add_blank_page(&mut editor, 0, 595.0, 842.0).unwrap();
        (editor, original)
    }

    #[test]
    fn redact_annotation_has_correct_subtype() {
        let annot = AnnotationBuilder::new(
            AnnotationType::Redact {
                overlay_color: [0.0, 0.0, 0.0],
            },
            [10.0, 20.0, 200.0, 50.0],
        )
        .build();
        assert_eq!(
            annot.get("Subtype"),
            Some(&PdfObject::Name("Redact".to_owned()))
        );
        match annot.get("IC") {
            Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 3),
            _ => panic!("expected /IC array with 3 components"),
        }
        assert!(annot.contains_key("Rect"));
    }

    #[test]
    fn highlight_annotation_has_required_keys() {
        let annot = AnnotationBuilder::new(
            AnnotationType::Highlight {
                color: [1.0, 1.0, 0.0],
                quad_points: vec![10.0, 700.0, 200.0, 700.0, 10.0, 710.0, 200.0, 710.0],
            },
            [10.0, 700.0, 200.0, 710.0],
        )
        .build();

        assert_eq!(
            annot.get("Subtype"),
            Some(&PdfObject::Name("Highlight".to_owned()))
        );
        assert!(annot.contains_key("QuadPoints"));
        assert!(annot.contains_key("C"));
        assert!(annot.contains_key("Rect"));
    }

    #[test]
    fn link_annotation_has_uri_action() {
        let annot = AnnotationBuilder::new(
            AnnotationType::Link {
                uri: "https://example.com".to_owned(),
            },
            [0.0, 0.0, 100.0, 20.0],
        )
        .build();
        assert_eq!(
            annot.get("Subtype"),
            Some(&PdfObject::Name("Link".to_owned()))
        );
        assert!(annot.contains_key("A"));
        if let Some(PdfObject::Dictionary(action)) = annot.get("A") {
            assert_eq!(action.get("S"), Some(&PdfObject::Name("URI".to_owned())));
        } else {
            panic!("expected action dict");
        }
    }

    #[test]
    fn add_annotation_updates_page_annots() {
        let (mut editor, original) = editor_with_page();
        let annot = AnnotationBuilder::new(
            AnnotationType::Text {
                contents: "hello".to_owned(),
                open: false,
            },
            [10.0, 10.0, 50.0, 50.0],
        );
        let annot_id = add_annotation(&mut editor, 0, annot).unwrap();
        assert!(annot_id > 0);

        // Check page dict now has /Annots with 1 entry
        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        match page_dict.get("Annots") {
            Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 1),
            _ => panic!("expected /Annots array"),
        }
    }

    #[test]
    fn add_annotation_save_append_parseable() {
        let (mut editor, original) = editor_with_page();
        let annot = AnnotationBuilder::new(
            AnnotationType::Text {
                contents: "note".to_owned(),
                open: false,
            },
            [10.0, 10.0, 50.0, 50.0],
        );
        add_annotation(&mut editor, 0, annot).unwrap();
        let result = editor.save_append(&original).unwrap();
        PdfDocument::parse(result).unwrap();
    }

    #[test]
    fn delete_annotation_removes_from_annots() {
        let (mut editor, _original) = editor_with_page();
        let annot = AnnotationBuilder::new(
            AnnotationType::Text {
                contents: "bye".to_owned(),
                open: false,
            },
            [10.0, 10.0, 50.0, 50.0],
        );
        let annot_id = add_annotation(&mut editor, 0, annot).unwrap();
        delete_annotation(&mut editor, 0, annot_id).unwrap();

        let (_, page_dict) = editor.get_page_dict(0).unwrap();
        match page_dict.get("Annots") {
            Some(PdfObject::Array(arr)) => assert_eq!(arr.len(), 0),
            None => {} // empty array may have been removed
            _ => panic!("unexpected /Annots type"),
        }
    }

    #[test]
    fn delete_nonexistent_annotation_errors() {
        let (mut editor, _) = editor_with_page();
        let err = delete_annotation(&mut editor, 0, 9999).unwrap_err();
        assert!(matches!(err, PdfError::InvalidStructure { .. }));
    }
}
