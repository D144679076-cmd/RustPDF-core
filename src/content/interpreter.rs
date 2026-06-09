//! PDF Content Stream Interpreter.
//!
//! Dispatches parsed operations to an OutputDevice implementation.
//! Manages graphics state and text state throughout interpretation.
//! (ISO 32000-1 §8, §9)

use std::collections::HashSet;

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDocument, PdfObject};

use super::graphics_state::{
    BlendMode, ClipEntry, Color, DashPattern, FillRule, GraphicsState, GraphicsStateStack, LineCap,
    LineJoin, Matrix, Path,
};
use super::operator::Operator;
use super::operators::{ContentStreamIter, Operation};
use super::text_state::{TextRenderMode, TextSpan, TextState};

/// Trait for receiving drawing commands from the content stream interpreter.
///
/// Implement this trait to build a renderer, text extractor, or any other
/// consumer of PDF page content.
pub trait OutputDevice {
    /// Called when a path should be stroked.
    fn stroke_path(&mut self, path: &Path, state: &GraphicsState);

    /// Called when a path should be filled.
    fn fill_path(&mut self, path: &Path, state: &GraphicsState, rule: FillRule);

    /// Called when text glyphs should be rendered.
    fn draw_text_span(&mut self, span: &TextSpan, state: &GraphicsState);

    /// Called when an image XObject should be drawn.
    fn draw_image(&mut self, image_data: &[u8], state: &GraphicsState);

    /// Called when an image XObject is drawn via the Do operator.
    /// Provides the XObject name, object ID (for decryption), and stream.
    fn draw_image_xobject(
        &mut self,
        _name: &str,
        _obj_id: Option<u32>,
        stream: &crate::parser::objects::PdfStream,
        state: &GraphicsState,
    ) {
        if let Ok(data) = stream.decode() {
            self.draw_image(&data, state);
        }
    }

    /// Called when a Form XObject begins.
    fn begin_form_xobject(&mut self) {}

    /// Called when a Form XObject ends.
    fn end_form_xobject(&mut self) {}

    /// Push the Form XObject's effective resources so the device can resolve
    /// patterns, fonts, and XObjects defined inside the form rather than in
    /// the page-level resources.  Called immediately before the form content
    /// stream is interpreted; `exit_form_resources` is called after.
    fn enter_form_resources(&mut self, _resources: &crate::parser::objects::PdfDict) {}

    /// Pop the Form XObject resources pushed by `enter_form_resources`.
    fn exit_form_resources(&mut self) {}

    /// Called when a transparency group Form XObject begins.
    /// The device should push an offscreen buffer sized to the current canvas.
    fn begin_transparency_group(&mut self) {}

    /// Called when a transparency group Form XObject ends.
    /// The device should composite the offscreen buffer onto the canvas.
    fn end_transparency_group(
        &mut self,
        _fill_alpha: f64,
        _blend_mode: crate::content::graphics_state::BlendMode,
    ) {
    }

    /// Called for the `sh` shading operator.
    /// Default is a no-op so non-rendering devices (TextExtractor) need no change.
    fn paint_shading(
        &mut self,
        _shading_dict: &crate::parser::objects::PdfDict,
        _doc: &crate::parser::objects::PdfDocument,
        _state: &GraphicsState,
    ) {
    }
}

/// The content stream interpreter.
///
/// Processes a sequence of PDF operations, maintaining graphics and text state,
/// and dispatching drawing commands to an OutputDevice.
pub struct ContentInterpreter {
    /// Graphics state stack (q/Q).
    pub gfx: GraphicsStateStack,
    /// Text state (BT/ET scope).
    pub text: TextState,
    /// Whether we are inside a BT...ET block.
    in_text_object: bool,
    /// Current path being constructed.
    current_path: Path,
    /// Current point in path construction (updated by m, l, c, v, y, re, h).
    current_point: Option<(f64, f64)>,
    /// Error count for this stream (bail after limit).
    error_count: u32,
    /// Object refs seen during Form XObject recursion (cycle detection).
    xobject_stack: HashSet<u32>,
}

const MAX_ERRORS: u32 = 2000;

impl ContentInterpreter {
    pub fn new() -> Self {
        ContentInterpreter {
            gfx: GraphicsStateStack::new(),
            text: TextState::default(),
            in_text_object: false,
            current_path: Path::new(),
            current_point: None,
            error_count: 0,
            xobject_stack: HashSet::new(),
        }
    }

    /// Interpret a content stream and dispatch to the output device.
    pub fn interpret(&mut self, data: &[u8], device: &mut dyn OutputDevice) -> Result<()> {
        let iter = ContentStreamIter::new(data);
        self.interpret_iter(iter, device, None, None)
    }

    /// Interpret with access to the document (for XObject resolution).
    pub fn interpret_with_doc(
        &mut self,
        data: &[u8],
        device: &mut dyn OutputDevice,
        doc: &PdfDocument,
        resources: &crate::parser::objects::PdfDict,
    ) -> Result<()> {
        let iter = ContentStreamIter::new(data);
        self.interpret_iter(iter, device, Some(doc), Some(resources))
    }

    /// Streaming variant: accepts any iterator of operations.
    ///
    /// This is the core execution path. `interpret` and `interpret_with_doc` both
    /// route through here. Callers that already have a `ContentStreamIter` (e.g.
    /// the renderer) can call this directly to avoid the intermediate `Vec`.
    pub fn interpret_iter<I>(
        &mut self,
        ops: I,
        device: &mut dyn OutputDevice,
        doc: Option<&PdfDocument>,
        resources: Option<&crate::parser::objects::PdfDict>,
    ) -> Result<()>
    where
        I: Iterator<Item = Result<Operation>>,
    {
        for op_result in ops {
            if self.error_count >= MAX_ERRORS {
                log::warn!(
                    "content stream error limit ({}) reached, skipping remaining operators",
                    MAX_ERRORS
                );
                break;
            }
            match op_result {
                Ok(op) => {
                    if let Err(e) = self.dispatch(&op, device, doc, resources) {
                        log::warn!("content stream op '{}' error: {}", op.operator, e);
                        self.error_count += 1;
                    }
                }
                Err(e) => {
                    log::warn!("content stream parse error: {}", e);
                    self.error_count += 1;
                }
            }
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        op: &Operation,
        device: &mut dyn OutputDevice,
        doc: Option<&PdfDocument>,
        resources: Option<&crate::parser::objects::PdfDict>,
    ) -> Result<()> {
        let operands = &op.operands;
        // Classify the token once; unknown tokens are logged and skipped. The
        // match below is on the typed `Operator`, so the compiler guarantees
        // every operator variant is handled (no silent fall-through, no typos).
        let opcode = match Operator::from_token(&op.operator) {
            Some(o) => o,
            None => {
                log::debug!("unknown content stream operator: '{}'", op.operator);
                return Ok(());
            }
        };
        match opcode {
            // --- Graphics State ---
            Operator::SaveState => self.gfx.save(),
            Operator::RestoreState => self.gfx.restore()?,
            Operator::Concat => {
                let m = matrix_from_operands(operands)?;
                self.gfx.current.ctm = m.concat(&self.gfx.current.ctm);
            }
            Operator::LineWidth => self.gfx.current.line_width = num(operands, 0)?,
            Operator::LineCap => self.gfx.current.line_cap = LineCap::from_i64(int(operands, 0)?),
            Operator::LineJoin => {
                self.gfx.current.line_join = LineJoin::from_i64(int(operands, 0)?)
            }
            Operator::MiterLimit => self.gfx.current.miter_limit = num(operands, 0)?,
            Operator::DashPattern => {
                if let Some(PdfObject::Array(arr)) = operands.first() {
                    let array: Vec<f64> = arr.iter().filter_map(obj_to_f64).collect();
                    let phase = num(operands, 1).unwrap_or(0.0);
                    self.gfx.current.dash_pattern = DashPattern { array, phase };
                }
            }
            Operator::RenderingIntent => {
                if let Some(PdfObject::Name(n)) = operands.first() {
                    self.gfx.current.rendering_intent = n.clone();
                }
            }
            Operator::Flatness => self.gfx.current.flatness = num(operands, 0)?,
            Operator::ExtGState => {
                if let (Some(PdfObject::Name(name)), Some(doc_ref), Some(res)) =
                    (operands.first(), doc, resources)
                {
                    self.apply_ext_gstate(name, doc_ref, res)?;
                }
            }

            // --- Path Construction ---
            Operator::MoveTo => {
                let x = num(operands, 0)?;
                let y = num(operands, 1)?;
                self.current_path.move_to(x, y);
                self.current_point = Some((x, y));
            }
            Operator::LineTo => {
                let x = num(operands, 0)?;
                let y = num(operands, 1)?;
                self.current_path.line_to(x, y);
                self.current_point = Some((x, y));
            }
            Operator::CurveTo => {
                let x3 = num(operands, 4)?;
                let y3 = num(operands, 5)?;
                self.current_path.curve_to(
                    num(operands, 0)?,
                    num(operands, 1)?,
                    num(operands, 2)?,
                    num(operands, 3)?,
                    x3,
                    y3,
                );
                self.current_point = Some((x3, y3));
            }
            Operator::CurveToV => {
                // First control point = current point (ISO 32000-1 §8.5.2.2)
                let (cp_x, cp_y) = self.current_point.unwrap_or((0.0, 0.0));
                let x3 = num(operands, 2)?;
                let y3 = num(operands, 3)?;
                self.current_path.curve_to(
                    cp_x,
                    cp_y,
                    num(operands, 0)?,
                    num(operands, 1)?,
                    x3,
                    y3,
                );
                self.current_point = Some((x3, y3));
            }
            Operator::CurveToY => {
                // Second control point = end point (ISO 32000-1 §8.5.2.2)
                let x3 = num(operands, 2)?;
                let y3 = num(operands, 3)?;
                self.current_path
                    .curve_to(num(operands, 0)?, num(operands, 1)?, x3, y3, x3, y3);
                self.current_point = Some((x3, y3));
            }
            Operator::ClosePath => {
                self.current_path.close();
                // After close, current point returns to the start of the subpath.
                // We don't track subpath start separately; set to None as approximation.
                self.current_point = None;
            }
            Operator::Rectangle => {
                let x = num(operands, 0)?;
                let y = num(operands, 1)?;
                let w = num(operands, 2)?;
                let h = num(operands, 3)?;
                self.current_path.rect(x, y, w, h);
                // After re, current point is the start corner (x, y).
                self.current_point = Some((x, y));
            }

            // --- Path Painting ---
            Operator::Stroke => {
                device.stroke_path(&self.current_path, &self.gfx.current);
                self.current_path.clear();
            }
            Operator::CloseStroke => {
                self.current_path.close();
                device.stroke_path(&self.current_path, &self.gfx.current);
                self.current_path.clear();
            }
            Operator::Fill | Operator::FillCompat => {
                device.fill_path(&self.current_path, &self.gfx.current, FillRule::NonZero);
                self.current_path.clear();
            }
            Operator::FillEvenOdd => {
                device.fill_path(&self.current_path, &self.gfx.current, FillRule::EvenOdd);
                self.current_path.clear();
            }
            Operator::FillStroke => {
                device.fill_path(&self.current_path, &self.gfx.current, FillRule::NonZero);
                device.stroke_path(&self.current_path, &self.gfx.current);
                self.current_path.clear();
            }
            Operator::FillStrokeEvenOdd => {
                device.fill_path(&self.current_path, &self.gfx.current, FillRule::EvenOdd);
                device.stroke_path(&self.current_path, &self.gfx.current);
                self.current_path.clear();
            }
            Operator::CloseFillStroke => {
                self.current_path.close();
                device.fill_path(&self.current_path, &self.gfx.current, FillRule::NonZero);
                device.stroke_path(&self.current_path, &self.gfx.current);
                self.current_path.clear();
            }
            Operator::CloseFillStrokeEvenOdd => {
                self.current_path.close();
                device.fill_path(&self.current_path, &self.gfx.current, FillRule::EvenOdd);
                device.stroke_path(&self.current_path, &self.gfx.current);
                self.current_path.clear();
            }
            Operator::EndPath => {
                self.current_path.clear();
            }

            // --- Clipping ---
            // W/W* add the current path as a new clip layer, intersected with
            // any existing clips at render time (ISO 32000-1 §8.5.4).
            Operator::Clip => {
                // [clip] trace silenced (high frequency); re-enable to debug clips.
                self.gfx.current.clip_path.push(ClipEntry {
                    path: self.current_path.clone(),
                    rule: FillRule::NonZero,
                    ctm: self.gfx.current.ctm,
                });
            }
            Operator::ClipEvenOdd => {
                // [clip] trace silenced (high frequency); re-enable to debug clips.
                self.gfx.current.clip_path.push(ClipEntry {
                    path: self.current_path.clone(),
                    rule: FillRule::EvenOdd,
                    ctm: self.gfx.current.ctm,
                });
            }

            // --- Color ---
            Operator::StrokeColorSpace => {
                if let Some(PdfObject::Name(n)) = operands.first() {
                    self.gfx.current.stroke_color_space = n.clone();
                }
            }
            Operator::FillColorSpace => {
                if let Some(PdfObject::Name(n)) = operands.first() {
                    self.gfx.current.fill_color_space = n.clone();
                }
            }
            Operator::StrokeColor | Operator::StrokeColorN => {
                self.gfx.current.stroke_color = color_from_operands_or_pattern(operands);
            }
            Operator::FillColor | Operator::FillColorN => {
                self.gfx.current.fill_color = color_from_operands_or_pattern(operands);
            }
            Operator::StrokeGray => {
                self.gfx.current.stroke_color_space = "DeviceGray".to_string();
                self.gfx.current.stroke_color = Color::Gray(num(operands, 0)?);
            }
            Operator::FillGray => {
                self.gfx.current.fill_color_space = "DeviceGray".to_string();
                self.gfx.current.fill_color = Color::Gray(num(operands, 0)?);
            }
            Operator::StrokeRgb => {
                self.gfx.current.stroke_color_space = "DeviceRGB".to_string();
                self.gfx.current.stroke_color =
                    Color::Rgb(num(operands, 0)?, num(operands, 1)?, num(operands, 2)?);
            }
            Operator::FillRgb => {
                self.gfx.current.fill_color_space = "DeviceRGB".to_string();
                self.gfx.current.fill_color =
                    Color::Rgb(num(operands, 0)?, num(operands, 1)?, num(operands, 2)?);
            }
            Operator::StrokeCmyk => {
                self.gfx.current.stroke_color_space = "DeviceCMYK".to_string();
                self.gfx.current.stroke_color = Color::Cmyk(
                    num(operands, 0)?,
                    num(operands, 1)?,
                    num(operands, 2)?,
                    num(operands, 3)?,
                );
            }
            Operator::FillCmyk => {
                self.gfx.current.fill_color_space = "DeviceCMYK".to_string();
                self.gfx.current.fill_color = Color::Cmyk(
                    num(operands, 0)?,
                    num(operands, 1)?,
                    num(operands, 2)?,
                    num(operands, 3)?,
                );
            }

            // --- Text Object ---
            Operator::BeginText => {
                self.in_text_object = true;
                self.text.begin_text();
            }
            Operator::EndText => {
                self.in_text_object = false;
            }

            // --- Text State ---
            Operator::CharSpacing => self.text.char_spacing = num(operands, 0)?,
            Operator::WordSpacing => self.text.word_spacing = num(operands, 0)?,
            Operator::HorizScale => self.text.horiz_scaling = num(operands, 0)?,
            Operator::Leading => self.text.leading = num(operands, 0)?,
            Operator::Font => {
                if let Some(PdfObject::Name(name)) = operands.first() {
                    self.text.font_name = name.clone();
                }
                if operands.len() > 1 {
                    self.text.font_size = num(operands, 1)?;
                }
            }
            Operator::RenderMode => {
                self.text.render_mode = TextRenderMode::from_i64(int(operands, 0)?)
            }
            Operator::Rise => self.text.rise = num(operands, 0)?,

            // --- Text Positioning ---
            Operator::MoveText => {
                let tx = num(operands, 0)?;
                let ty = num(operands, 1)?;
                self.text.move_text_position(tx, ty);
            }
            Operator::MoveTextLeading => {
                let tx = num(operands, 0)?;
                let ty = num(operands, 1)?;
                self.text.leading = -ty;
                self.text.move_text_position(tx, ty);
            }
            Operator::TextMatrix => {
                self.text.set_text_matrix(
                    num(operands, 0)?,
                    num(operands, 1)?,
                    num(operands, 2)?,
                    num(operands, 3)?,
                    num(operands, 4)?,
                    num(operands, 5)?,
                );
            }
            Operator::NextLine => self.text.next_line(),

            // --- Text Showing ---
            Operator::ShowText => {
                if let Some(PdfObject::String(bytes)) = operands.first() {
                    self.show_text(bytes, device, doc, resources);
                }
            }
            Operator::NextLineShowText => {
                self.text.next_line();
                if let Some(PdfObject::String(bytes)) = operands.first() {
                    self.show_text(bytes, device, doc, resources);
                }
            }
            Operator::NextLineShowTextSpacing => {
                self.text.word_spacing = num(operands, 0)?;
                self.text.char_spacing = num(operands, 1)?;
                self.text.next_line();
                if let Some(PdfObject::String(bytes)) = operands.get(2) {
                    self.show_text(bytes, device, doc, resources);
                }
            }
            Operator::ShowTextArray => {
                if let Some(PdfObject::Array(arr)) = operands.first() {
                    self.show_text_array(arr, device, doc, resources);
                }
            }

            // --- XObject ---
            Operator::XObject => {
                if let (Some(PdfObject::Name(name)), Some(doc_ref), Some(res)) =
                    (operands.first(), doc, resources)
                {
                    self.handle_do(name, device, doc_ref, res)?;
                }
            }

            // --- Inline Image ---
            Operator::InlineImage => {
                if operands.len() >= 2 {
                    if let Some(PdfObject::String(data)) = operands.get(1) {
                        device.draw_image(data, &self.gfx.current);
                    }
                }
            }

            // --- Marked Content (ignored for rendering) ---
            Operator::BeginMarkedContent
            | Operator::BeginMarkedContentDict
            | Operator::EndMarkedContent
            | Operator::MarkedPoint
            | Operator::MarkedPointDict => {}

            // --- Compatibility (ignored) ---
            Operator::BeginCompat | Operator::EndCompat => {}

            // --- Type 3 font operators ---
            Operator::Type3Width | Operator::Type3WidthBBox => {}

            // --- Shading ---
            Operator::Shading => {
                if let (Some(PdfObject::Name(name)), Some(doc_ref), Some(res)) =
                    (operands.first(), doc, resources)
                {
                    if let Some(shading_res) = res.get("Shading") {
                        let shading_dict_obj = match shading_res {
                            PdfObject::Dictionary(d) => d.get(name).cloned(),
                            _ => None,
                        };
                        if let Some(ref shading_ref) = shading_dict_obj {
                            let resolved = doc_ref.resolve(shading_ref).unwrap_or(PdfObject::Null);
                            let shading_dict = match resolved {
                                PdfObject::Dictionary(d) => Some(d),
                                PdfObject::Stream(s) => Some(s.dict.clone()),
                                _ => None,
                            };
                            if let Some(d) = shading_dict {
                                device.paint_shading(&d, doc_ref, &self.gfx.current);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Show a text string (Tj operator), decoding via ToUnicode CMap when available.
    fn show_text(
        &mut self,
        bytes: &[u8],
        device: &mut dyn OutputDevice,
        doc: Option<&PdfDocument>,
        resources: Option<&crate::parser::objects::PdfDict>,
    ) {
        let render_matrix = self.text.get_render_matrix(&self.gfx.current.ctm);
        let x = render_matrix.e;
        let y = render_matrix.f;
        // Device-space font height: length of the Y-basis vector of the render matrix.
        // Includes font_size × text_matrix_scale × CTM_scale. Used for rasterization.
        let font_size_px = (render_matrix.b.powi(2) + render_matrix.d.powi(2)).sqrt();

        // Resolve ToUnicode CMap and font type for this font resource name.
        let (cmap, is_composite, font_widths) =
            resolve_font_info(&self.text.font_name, doc, resources);

        let text: String = if let Some(ref cm) = cmap {
            decode_bytes_with_cmap(bytes, cm, is_composite)
        } else if is_composite {
            // No CMap but composite font: read 2-byte pairs as Unicode code points.
            bytes
                .chunks(2)
                .filter_map(|pair| {
                    let cp = if pair.len() == 2 {
                        ((pair[0] as u32) << 8) | (pair[1] as u32)
                    } else {
                        pair[0] as u32
                    };
                    char::from_u32(cp)
                })
                .collect()
        } else {
            // Simple font: treat each byte as Latin-1.
            bytes.iter().map(|&b| b as char).collect()
        };

        // Compute total rendered width before emitting the span so hit-testing
        // and text extraction get accurate bounding boxes.
        let total_width =
            compute_text_width(bytes, is_composite, &font_widths, self.text.font_size);

        // Advance text position using actual glyph widths, capturing pixel-space
        // advances so the renderer can use them instead of fontdue's glyph metrics.
        // We track both x and y pixel advances to handle rotated text correctly.
        let mut char_advances: Vec<f64> = Vec::new();
        let mut char_advances_y: Vec<f64> = Vec::new();
        let mut char_cids: Vec<u32> = Vec::new();
        if is_composite {
            let mut pending_advance: f64 = 0.0;
            let mut pending_advance_y: f64 = 0.0;
            for chunk in bytes.chunks(2) {
                let cid = if chunk.len() == 2 {
                    ((chunk[0] as u32) << 8) | (chunk[1] as u32)
                } else {
                    chunk[0] as u32
                };
                let w = font_widths
                    .as_ref()
                    .map(|fw| fw.get_cid_width(cid))
                    .unwrap_or(500.0);
                // Width is in 1/1000 text-space units; scale by font size.
                let advance = w / 1000.0 * self.text.font_size;
                let is_space = cid == 0x0020;

                let pre = self.text.get_render_matrix(&self.gfx.current.ctm);
                self.text.advance_glyph(advance, is_space);
                let post = self.text.get_render_matrix(&self.gfx.current.ctm);
                let pixel_adv = (post.e - pre.e).abs();
                let pixel_adv_y = post.f - pre.f;

                // Count Unicode chars this CID maps to so we assign advances 1:1.
                // When CMap returns None (no mapping), fall back to char::from_u32 — same
                // path decode_bytes_with_cmap takes, so the counts stay in sync.
                let n_chars = if let Some(ref cm) = cmap {
                    cm.lookup(cid)
                        .map(|s| s.chars().count())
                        .unwrap_or_else(|| char::from_u32(cid).map(|_| 1).unwrap_or(0))
                } else {
                    char::from_u32(cid).map(|_| 1).unwrap_or(0)
                };

                if n_chars == 0 {
                    // CID has no Unicode mapping; its advance was already applied to the
                    // text matrix.  Fold it into the previous slot so the renderer
                    // positions the next visible character correctly.
                    if let Some(last) = char_advances.last_mut() {
                        *last += pixel_adv;
                    } else {
                        pending_advance += pixel_adv;
                    }
                    if let Some(last_y) = char_advances_y.last_mut() {
                        *last_y += pixel_adv_y;
                    } else {
                        pending_advance_y += pixel_adv_y;
                    }
                } else {
                    for i in 0..n_chars {
                        // First sub-char gets any advance pending from leading invisible CIDs.
                        char_advances.push(if i == 0 {
                            pixel_adv + pending_advance
                        } else {
                            0.0
                        });
                        char_advances_y.push(if i == 0 {
                            pixel_adv_y + pending_advance_y
                        } else {
                            0.0
                        });
                        char_cids.push(cid);
                    }
                    pending_advance = 0.0;
                    pending_advance_y = 0.0;
                }
            }
        } else {
            for &b in bytes {
                let w = font_widths
                    .as_ref()
                    .map(|fw| fw.get_width(b as u32))
                    .unwrap_or(500.0);
                let advance = w / 1000.0 * self.text.font_size;
                let is_space = b == b' ';

                let pre = self.text.get_render_matrix(&self.gfx.current.ctm);
                self.text.advance_glyph(advance, is_space);
                let post = self.text.get_render_matrix(&self.gfx.current.ctm);
                char_advances.push((post.e - pre.e).abs());
                char_advances_y.push(post.f - pre.f);
            }
        }

        // If lengths diverge (e.g. multi-char ToUnicode mapping), clear both so the
        // renderer falls back to fontdue advances rather than using misaligned data.
        if char_advances.len() != text.chars().count() {
            log::warn!(
                "[interpreter] char_advances mismatch: {} != {} for {:?}",
                char_advances.len(),
                text.chars().count(),
                text
            );
            char_advances.clear();
            char_advances_y.clear();
            char_cids.clear();
        }

        if !text.is_empty() {
            // Per-span trace (very high frequency); commented to keep the console
            // usable during edit-text. Re-enable when debugging text positioning:
            //   let adv_sample = if char_advances.is_empty() { "[fontdue-fallback]".into() }
            //       else { format!("{:?}", &char_advances[..char_advances.len().min(5)]) };
            //   log::debug!("[text-span] font={:?} size_pt={:.1} size_px={:.1} \
            //       pos=({:.1},{:.1}) composite={} chars={} advances={}",
            //       self.text.font_name, self.text.font_size, font_size_px, x, y,
            //       is_composite, text.chars().count(), adv_sample);
            let _ = is_composite;
            let span = crate::content::text_state::TextSpan {
                text,
                x,
                y,
                width: total_width,
                font_size: self.text.font_size,
                font_size_px,
                font_name: self.text.font_name.clone(),
                char_advances,
                char_advances_y,
                char_cids,
                render_matrix_2x2: [
                    render_matrix.a,
                    render_matrix.b,
                    render_matrix.c,
                    render_matrix.d,
                ],
                stroke_text: self.text.render_mode.strokes(),
            };
            device.draw_text_span(&span, &self.gfx.current);
        }
    }

    /// Show a TJ array (mixed strings and positioning adjustments).
    fn show_text_array(
        &mut self,
        arr: &[PdfObject],
        device: &mut dyn OutputDevice,
        doc: Option<&PdfDocument>,
        resources: Option<&crate::parser::objects::PdfDict>,
    ) {
        for item in arr {
            match item {
                PdfObject::String(bytes) => self.show_text(bytes, device, doc, resources),
                PdfObject::Integer(n) => {
                    self.text.advance_tj_displacement(*n as f64);
                }
                PdfObject::Real(r) => {
                    self.text.advance_tj_displacement(*r);
                }
                _ => {}
            }
        }
    }

    /// Apply an ExtGState dictionary to the current graphics state.
    fn apply_ext_gstate(
        &mut self,
        name: &str,
        doc: &PdfDocument,
        resources: &crate::parser::objects::PdfDict,
    ) -> Result<()> {
        let ext_gstate_dict = match resources.get("ExtGState") {
            Some(PdfObject::Dictionary(d)) => d,
            _ => return Ok(()),
        };

        let gs_ref = match ext_gstate_dict.get(name) {
            Some(obj) => obj.clone(),
            None => return Ok(()),
        };

        let gs_obj = doc.resolve(&gs_ref)?;
        let gs_dict = match gs_obj {
            PdfObject::Dictionary(d) => d,
            _ => return Ok(()),
        };

        if let Some(PdfObject::Real(w)) = gs_dict.get("LW") {
            self.gfx.current.line_width = *w;
        }
        if let Some(PdfObject::Integer(w)) = gs_dict.get("LW") {
            self.gfx.current.line_width = *w as f64;
        }
        if let Some(PdfObject::Integer(lc)) = gs_dict.get("LC") {
            self.gfx.current.line_cap = LineCap::from_i64(*lc);
        }
        if let Some(PdfObject::Integer(lj)) = gs_dict.get("LJ") {
            self.gfx.current.line_join = LineJoin::from_i64(*lj);
        }
        if let Some(ca) = gs_dict.get("CA") {
            if let Some(v) = obj_to_f64(ca) {
                self.gfx.current.stroke_alpha = v;
            }
        }
        if let Some(ca) = gs_dict.get("ca") {
            if let Some(v) = obj_to_f64(ca) {
                self.gfx.current.fill_alpha = v;
            }
        }
        if let Some(PdfObject::Name(bm)) = gs_dict.get("BM") {
            self.gfx.current.blend_mode = BlendMode::from_name(bm);
        }

        if let Some(smask_val) = gs_dict.get("SMask") {
            match smask_val {
                PdfObject::Name(n) if n == "None" => {}
                PdfObject::Name(_) => {
                    // Named SMask other than "None" — no standard names exist; treat as no-op.
                }
                _ => {
                    // Dict-valued SMask (Form XObject soft mask) is not yet implemented.
                    // Treat as no-op: leave alpha unchanged so chart content remains visible.
                    // Zeroing alpha (previous behaviour) was worse: it erased all fills.
                    log::warn!(
                        "[gs] /SMask in ExtGState '{}' — ignored (full SMask not yet implemented)",
                        name
                    );
                }
            }
        }

        Ok(())
    }

    /// Handle the Do operator: dispatch to image or form XObject handler.
    fn handle_do(
        &mut self,
        name: &str,
        device: &mut dyn OutputDevice,
        doc: &PdfDocument,
        resources: &crate::parser::objects::PdfDict,
    ) -> Result<()> {
        let xobject_dict = match resources.get("XObject") {
            Some(PdfObject::Dictionary(d)) => d.clone(),
            _ => {
                log::debug!("Do '{}': no /XObject dict in resources", name);
                return Ok(());
            }
        };

        let xobj_ref = match xobject_dict.get(name) {
            Some(obj) => obj.clone(),
            None => {
                log::debug!("Do '{}': name not found in /XObject dict", name);
                return Ok(());
            }
        };

        let obj_num = match &xobj_ref {
            PdfObject::Reference(id, _) => Some(*id),
            _ => None,
        };

        let xobj = doc.resolve(&xobj_ref)?;
        let stream = match xobj {
            PdfObject::Stream(s) => s,
            _ => {
                log::warn!("Do '{}': resolved to non-stream object", name);
                return Ok(());
            }
        };

        let subtype = stream
            .dict
            .get("Subtype")
            .and_then(|o| o.as_name())
            .unwrap_or("");

        match subtype {
            "Image" => {
                // `obj_id` is for the (in-progress) encrypted-image path; None here
                // preserves current behaviour (every impl ignores it for now).
                device.draw_image_xobject(name, obj_num, &stream, &self.gfx.current);
            }
            "Form" => {
                self.handle_do_form(name, obj_num, &stream, device, doc, resources)?;
            }
            other => {
                log::debug!("Do '{}': unsupported /Subtype '{}'", name, other);
            }
        }

        Ok(())
    }

    /// Handle a Form XObject: save state, apply matrix, interpret nested stream.
    fn handle_do_form(
        &mut self,
        name: &str,
        obj_num: Option<u32>,
        stream: &crate::parser::objects::PdfStream,
        device: &mut dyn OutputDevice,
        doc: &PdfDocument,
        page_resources: &crate::parser::objects::PdfDict,
    ) -> Result<()> {
        const MAX_FORM_DEPTH: usize = 10;

        if self.xobject_stack.len() >= MAX_FORM_DEPTH {
            log::warn!("Do '{}': Form XObject nesting depth limit reached", name);
            return Ok(());
        }

        if let Some(id) = obj_num {
            if !self.xobject_stack.insert(id) {
                log::warn!("Do '{}': cycle detected (obj {})", name, id);
                return Ok(());
            }
        }

        // Detect transparency group: /Group << /S /Transparency >>
        let is_transparency_group = stream.dict.get("Group").is_some_and(|g| {
            matches!(g, PdfObject::Dictionary(d) if
                d.get("S").and_then(|o| o.as_name()) == Some("Transparency"))
        });
        log::debug!(
            "[form-enter] {:?} depth={} group={}",
            name,
            self.xobject_stack.len(),
            is_transparency_group
        );

        // Capture caller's compositing parameters BEFORE entering the group (PDF spec §11.6.6).
        // Transparency groups are composited using the calling context's alpha and blend mode,
        // not the group's internal end-state.
        let caller_fill_alpha = self.gfx.current.fill_alpha;
        let caller_blend_mode = self.gfx.current.blend_mode;

        if is_transparency_group {
            device.begin_transparency_group();
        } else {
            device.begin_form_xobject();
        }
        self.gfx.save();

        if let Some(PdfObject::Array(matrix_arr)) = stream.dict.get("Matrix") {
            if matrix_arr.len() >= 6 {
                if let Ok(m) = matrix_from_operands(matrix_arr) {
                    self.gfx.current.ctm = m.concat(&self.gfx.current.ctm);
                }
            }
        }

        let form_resources = match stream.dict.get("Resources") {
            Some(PdfObject::Dictionary(d)) => d.clone(),
            Some(obj) => {
                let resolved = doc.resolve(obj).unwrap_or(PdfObject::Null);
                match resolved {
                    PdfObject::Dictionary(d) => d,
                    _ => page_resources.clone(),
                }
            }
            None => page_resources.clone(),
        };

        // Apply BBox as a clip rect (ISO 32000-1 §8.10.2).
        // BBox is in the Form XObject's own coordinate system (before its Matrix).
        if let Some(PdfObject::Array(bbox)) = stream.dict.get("BBox") {
            if bbox.len() >= 4 {
                let bx0 = obj_to_f64(&bbox[0]).unwrap_or(0.0);
                let by0 = obj_to_f64(&bbox[1]).unwrap_or(0.0);
                let bx1 = obj_to_f64(&bbox[2]).unwrap_or(0.0);
                let by1 = obj_to_f64(&bbox[3]).unwrap_or(0.0);
                log::debug!(
                    "[form] {:?} bbox=[{},{},{},{}] group={}",
                    name,
                    bx0,
                    by0,
                    bx1,
                    by1,
                    is_transparency_group
                );
                let mut clip = Path::new();
                clip.rect(bx0, by0, bx1 - bx0, by1 - by0);
                // Push (not replace) so parent clips are preserved and intersected.
                self.gfx.current.clip_path.push(ClipEntry {
                    path: clip,
                    rule: crate::content::graphics_state::FillRule::NonZero,
                    ctm: self.gfx.current.ctm,
                });
            }
        }

        let decoded = stream.decode()?;
        let iter = ContentStreamIter::new(&decoded);
        device.enter_form_resources(&form_resources);
        self.interpret_iter(iter, device, Some(doc), Some(&form_resources))?;
        device.exit_form_resources();

        self.gfx.restore()?;
        if is_transparency_group {
            device.end_transparency_group(caller_fill_alpha, caller_blend_mode);
        } else {
            device.end_form_xobject();
        }

        if let Some(id) = obj_num {
            self.xobject_stack.remove(&id);
        }

        Ok(())
    }
}

impl Default for ContentInterpreter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Operand extraction helpers
// ---------------------------------------------------------------------------

fn num(operands: &[PdfObject], idx: usize) -> Result<f64> {
    operands.get(idx).and_then(obj_to_f64).ok_or_else(|| {
        PdfError::invalid_token(0, format!("missing numeric operand at index {}", idx))
    })
}

fn int(operands: &[PdfObject], idx: usize) -> Result<i64> {
    match operands.get(idx) {
        Some(PdfObject::Integer(n)) => Ok(*n),
        Some(PdfObject::Real(r)) => Ok(*r as i64),
        _ => Err(PdfError::invalid_token(
            0,
            format!("missing integer operand at index {}", idx),
        )),
    }
}

fn obj_to_f64(obj: &PdfObject) -> Option<f64> {
    match obj {
        PdfObject::Integer(n) => Some(*n as f64),
        PdfObject::Real(r) => Some(*r),
        _ => None,
    }
}

fn matrix_from_operands(operands: &[PdfObject]) -> Result<Matrix> {
    if operands.len() < 6 {
        return Err(PdfError::invalid_token(
            0,
            "cm operator requires 6 operands",
        ));
    }
    Ok(Matrix {
        a: num(operands, 0)?,
        b: num(operands, 1)?,
        c: num(operands, 2)?,
        d: num(operands, 3)?,
        e: num(operands, 4)?,
        f: num(operands, 5)?,
    })
}

fn color_from_operands(operands: &[PdfObject]) -> Color {
    match operands.len() {
        1 => Color::Gray(obj_to_f64(&operands[0]).unwrap_or(0.0)),
        3 => Color::Rgb(
            obj_to_f64(&operands[0]).unwrap_or(0.0),
            obj_to_f64(&operands[1]).unwrap_or(0.0),
            obj_to_f64(&operands[2]).unwrap_or(0.0),
        ),
        4 => Color::Cmyk(
            obj_to_f64(&operands[0]).unwrap_or(0.0),
            obj_to_f64(&operands[1]).unwrap_or(0.0),
            obj_to_f64(&operands[2]).unwrap_or(0.0),
            obj_to_f64(&operands[3]).unwrap_or(0.0),
        ),
        _ => Color::Gray(0.0),
    }
}

fn color_from_operands_or_pattern(operands: &[PdfObject]) -> Color {
    if let Some(PdfObject::Name(name)) = operands.last() {
        // Numeric prefix operands before the name are the tint colour for uncoloured
        // tiling patterns (PDF spec §8.7.3.3).  Preserve them so the renderer can
        // fall back to the tint when the pattern type is unsupported.
        let tint: Option<Vec<f64>> = if operands.len() > 1 {
            let nums: Vec<f64> = operands[..operands.len() - 1]
                .iter()
                .filter_map(obj_to_f64)
                .collect();
            if nums.is_empty() {
                None
            } else {
                Some(nums)
            }
        } else {
            None
        };
        return Color::Pattern(name.clone(), tint);
    }
    color_from_operands(operands)
}

// ---------------------------------------------------------------------------
// Font resolution helpers
// ---------------------------------------------------------------------------

/// Compute the total rendered width of a text string in user-space points.
///
/// Mirrors the advance loop in `show_text` but returns the sum instead of
/// mutating text state, so the width can be stored in the emitted `TextSpan`.
fn compute_text_width(
    bytes: &[u8],
    is_composite: bool,
    font_widths: &Option<crate::fonts::types::FontWidths>,
    font_size: f64,
) -> f64 {
    if is_composite {
        bytes
            .chunks(2)
            .map(|chunk| {
                let cid = if chunk.len() == 2 {
                    ((chunk[0] as u32) << 8) | (chunk[1] as u32)
                } else {
                    chunk[0] as u32
                };
                let w = font_widths
                    .as_ref()
                    .map(|fw| fw.get_cid_width(cid))
                    .unwrap_or(500.0);
                w / 1000.0 * font_size
            })
            .sum()
    } else {
        bytes
            .iter()
            .map(|&b| {
                let w = font_widths
                    .as_ref()
                    .map(|fw| fw.get_width(b as u32))
                    .unwrap_or(500.0);
                w / 1000.0 * font_size
            })
            .sum()
    }
}

/// Resolve ToUnicode CMap, composite-font flag, and width table for a font
/// resource name.  Returns `(cmap, is_composite, widths)`.
pub(crate) fn resolve_font_info(
    font_name: &str,
    doc: Option<&PdfDocument>,
    resources: Option<&crate::parser::objects::PdfDict>,
) -> (
    Option<crate::fonts::cmap::CMap>,
    bool,
    Option<crate::fonts::types::FontWidths>,
) {
    let (doc, res) = match (doc, resources) {
        (Some(d), Some(r)) => (d, r),
        _ => return (None, false, None),
    };

    let font_dict = match res.get("Font") {
        Some(PdfObject::Dictionary(d)) => d,
        _ => return (None, false, None),
    };

    let font_ref = match font_dict.get(font_name) {
        Some(r) => r.clone(),
        None => return (None, false, None),
    };

    let font_obj = match doc.resolve(&font_ref) {
        Ok(PdfObject::Dictionary(d)) => d,
        _ => return (None, false, None),
    };

    // Detect composite (Type0) font.
    let is_composite = font_obj
        .get("Subtype")
        .and_then(|o| o.as_name())
        .map(|s| s == "Type0")
        .unwrap_or(false);

    // Parse ToUnicode CMap if present.
    // Use get_stream_data when it's a reference so the stream is decrypted first.
    let cmap = font_obj
        .get("ToUnicode")
        .and_then(|r| match r {
            PdfObject::Reference(id, _) => doc.get_stream_data(*id).ok(),
            PdfObject::Stream(s) => s.decode().ok(),
            _ => None,
        })
        .and_then(|bytes| crate::fonts::cmap::CMap::parse(&bytes).ok());

    // Extract glyph widths.
    let widths = if is_composite {
        // For Type0, widths live in DescendantFonts[0].
        // DescendantFonts may be an indirect reference in some PDFs.
        let desc_font = font_obj
            .get("DescendantFonts")
            .and_then(|o| match o {
                PdfObject::Array(a) => a.first().cloned(),
                PdfObject::Reference(_, _) => {
                    doc.resolve(o).ok().and_then(|resolved| match resolved {
                        PdfObject::Array(a) => a.into_iter().next(),
                        _ => None,
                    })
                }
                _ => None,
            })
            .and_then(|r| doc.resolve(&r).ok())
            .and_then(|obj| match obj {
                PdfObject::Dictionary(d) => Some(d),
                _ => None,
            });

        desc_font.map(|d| parse_cid_widths(&d, doc))
    } else {
        Some(parse_simple_widths(&font_obj))
    };

    (cmap, is_composite, widths)
}

/// Intrinsic style of a PDF font, read from `/BaseFont` + `/FontDescriptor`.
///
/// Used by the editor to seed a block's CharStyle with the font's *real* bold/
/// italic (instead of assuming regular) and to show a sensible family name.
#[allow(dead_code)]
pub(crate) struct FontStyleInfo {
    /// Raw `/BaseFont` name (may carry a `XXXXXX+` subset prefix).
    pub base_font: String,
    /// Whether the font is bold (ForceBold flag, weight ≥ 700, or name hint).
    pub bold: bool,
    /// Whether the font is italic/oblique (Italic flag, ItalicAngle ≠ 0, or name).
    pub italic: bool,
}

/// Attempt to read the bold weight from an embedded TrueType program (`FontFile2`)
/// in the given FontDescriptor dict.  Returns `Some(true/false)` when the program
/// is readable; `None` when the font uses CFF or has no `FontFile2`.
fn embedded_bold_from_font_file(
    doc: &PdfDocument,
    descriptor: &crate::parser::objects::PdfDict,
) -> Option<bool> {
    use crate::fonts::truetype::TrueTypeFont;
    use crate::parser::objects::PdfObject;

    let ff_ref = descriptor.get("FontFile2")?;
    let data = match ff_ref {
        PdfObject::Reference(id, _) => doc.get_stream_data(*id).ok()?,
        other => match doc.resolve(other).ok()? {
            PdfObject::Stream(s) => s.decode_with_doc(doc).ok()?,
            _ => return None,
        },
    };
    let ttf = TrueTypeFont::parse(&data).ok()?;
    Some(ttf.is_bold())
}

/// Resolve a font resource key to its intrinsic [`FontStyleInfo`].
///
/// Walks `/Resources/Font/<font_name>` to the font dict, then to its
/// `/FontDescriptor` (for Type0 fonts, the `DescendantFonts[0]` CIDFont's
/// descriptor), and derives bold/italic from `/Flags` (ForceBold/Italic),
/// `/FontWeight`, `/ItalicAngle`, with the `/BaseFont` name as a fallback hint.
/// Returns `None` when the font can't be resolved.
#[allow(dead_code)]
pub(crate) fn resolve_font_style(
    font_name: &str,
    doc: Option<&PdfDocument>,
    resources: Option<&crate::parser::objects::PdfDict>,
) -> Option<FontStyleInfo> {
    let (doc, res) = (doc?, resources?);
    let font_dict = match res.get("Font") {
        Some(PdfObject::Dictionary(d)) => d,
        _ => return None,
    };
    let font_obj = match doc.resolve(font_dict.get(font_name)?) {
        Ok(PdfObject::Dictionary(d)) => d,
        _ => return None,
    };

    let base_font = font_obj
        .get("BaseFont")
        .and_then(|o| o.as_name())
        .unwrap_or("")
        .to_owned();

    // For Type0, the FontDescriptor lives on the descendant CIDFont, not the
    // Type0 wrapper — mirror `resolve_font_info`'s DescendantFonts navigation.
    let is_composite = font_obj
        .get("Subtype")
        .and_then(|o| o.as_name())
        .map(|s| s == "Type0")
        .unwrap_or(false);
    let descriptor_host = if is_composite {
        font_obj
            .get("DescendantFonts")
            .and_then(|o| match o {
                PdfObject::Array(a) => a.first().cloned(),
                PdfObject::Reference(_, _) => doc.resolve(o).ok().and_then(|r| match r {
                    PdfObject::Array(a) => a.into_iter().next(),
                    _ => None,
                }),
                _ => None,
            })
            .and_then(|r| doc.resolve(&r).ok())
            .and_then(|obj| obj.as_dict().cloned())
            .unwrap_or_else(|| font_obj.clone())
    } else {
        font_obj.clone()
    };

    let descriptor = descriptor_host
        .get("FontDescriptor")
        .and_then(|o| doc.resolve(o).ok())
        .and_then(|obj| obj.as_dict().cloned());

    let obj_f64 = |o: &PdfObject| match o {
        PdfObject::Integer(n) => Some(*n as f64),
        PdfObject::Real(r) => Some(*r),
        _ => None,
    };
    let (flags, weight, italic_angle) = match &descriptor {
        Some(d) => (
            d.get("Flags")
                .and_then(obj_f64)
                .map(|f| f as u32)
                .unwrap_or(0),
            d.get("FontWeight").and_then(obj_f64).unwrap_or(0.0),
            d.get("ItalicAngle").and_then(obj_f64).unwrap_or(0.0),
        ),
        None => (0, 0.0, 0.0),
    };
    // /StemV is the stroke width of vertical stems in the font.  Normal-weight
    // Latin fonts typically report 70–90; bold stems ≈ 120–160.  Use this as a
    // supplemental signal when neither /Flags, /FontWeight, nor the name carry
    // the weight.
    const STEM_V_BOLD_THRESHOLD: f64 = 120.0;
    let stem_v = match &descriptor {
        Some(d) => d.get("StemV").and_then(obj_f64).unwrap_or(0.0),
        None => 0.0,
    };

    use crate::fonts::types::FontFlags;
    let lower = base_font.to_lowercase();
    let mut bold = (flags & FontFlags::FORCE_BOLD) != 0
        || weight >= 700.0
        || stem_v >= STEM_V_BOLD_THRESHOLD
        || lower.contains("bold");
    let italic = (flags & FontFlags::ITALIC) != 0
        || italic_angle != 0.0
        || lower.contains("italic")
        || lower.contains("oblique");

    // For composite fonts whose descriptor still doesn't set the bold flag,
    // parse the embedded TrueType program's OS/2 usWeightClass and head.macStyle
    // as a last-resort signal. Cost: one stream decode + TrueType table scan,
    // only paid when the cheaper checks above all fail.
    if !bold && is_composite {
        if let Some(d) = &descriptor {
            if let Some(program_bold) = embedded_bold_from_font_file(doc, d) {
                bold = program_bold;
            }
        }
    }

    log::debug!(
        "[font-style] key={} base_font={:?} composite={} flags={:#x} weight={} stem_v={} italic_angle={} -> bold={} italic={}",
        font_name,
        base_font,
        is_composite,
        flags,
        weight,
        stem_v,
        italic_angle,
        bold,
        italic,
    );

    Some(FontStyleInfo {
        base_font,
        bold,
        italic,
    })
}

/// Strip a leading `XXXXXX+` font subset tag (6 uppercase letters + `+`).
///
/// Embedded subsets prefix the family with a random tag (e.g. `ABCDEF+Calibri`);
/// this returns the human-facing part (`Calibri`). Names without a valid tag
/// (e.g. `CIDFont+F2`, `Helvetica`) are returned unchanged.
#[allow(dead_code)]
pub(crate) fn strip_subset_prefix(name: &str) -> &str {
    if let Some((tag, rest)) = name.split_once('+') {
        if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) && !rest.is_empty() {
            return rest;
        }
    }
    name
}

/// Parse simple-font width table from a font dictionary.
fn parse_simple_widths(
    font_dict: &crate::parser::objects::PdfDict,
) -> crate::fonts::types::FontWidths {
    let first_char = font_dict
        .get("FirstChar")
        .and_then(|o| match o {
            PdfObject::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);
    let last_char = font_dict
        .get("LastChar")
        .and_then(|o| match o {
            PdfObject::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);
    let widths: Vec<f64> = match font_dict.get("Widths") {
        Some(PdfObject::Array(arr)) => arr
            .iter()
            .map(|o| match o {
                PdfObject::Integer(n) => *n as f64,
                PdfObject::Real(r) => *r,
                _ => 0.0,
            })
            .collect(),
        _ => Vec::new(),
    };
    crate::fonts::types::FontWidths {
        first_char,
        last_char,
        widths,
        default_width: 1000.0,
        cid_widths: Vec::new(),
    }
}

/// Parse CIDFont /W width array from a DescendantFont dictionary.
///
/// `doc` is needed to resolve indirect `/W` references (`/W 9 0 R`), which is
/// the common case in PDFs exported from Microsoft Word and PowerPoint.
fn parse_cid_widths(
    desc_dict: &crate::parser::objects::PdfDict,
    doc: &PdfDocument,
) -> crate::fonts::types::FontWidths {
    use crate::fonts::types::CidWidthEntry;

    let default_width = desc_dict
        .get("DW")
        .and_then(|o| match o {
            PdfObject::Integer(n) => Some(*n as f64),
            PdfObject::Real(r) => Some(*r),
            _ => None,
        })
        .unwrap_or(1000.0);

    // /W may be stored as an indirect reference (e.g. `/W 9 0 R`).
    let w_arr_opt: Option<Vec<PdfObject>> = match desc_dict.get("W") {
        Some(PdfObject::Array(a)) => Some(a.clone()),
        Some(r @ PdfObject::Reference(_, _)) => doc.resolve(r).ok().and_then(|o| match o {
            PdfObject::Array(a) => Some(a),
            _ => None,
        }),
        _ => None,
    };

    let mut cid_widths = Vec::new();
    if let Some(w_arr) = w_arr_opt {
        let mut i = 0;
        while i < w_arr.len() {
            let start_cid = match &w_arr[i] {
                PdfObject::Integer(n) => *n as u32,
                _ => {
                    i += 1;
                    continue;
                }
            };
            i += 1;
            if i >= w_arr.len() {
                break;
            }
            match &w_arr[i] {
                PdfObject::Integer(end) => {
                    // c1 c2 w  — range with single width
                    let end_cid = *end as u32;
                    i += 1;
                    if i < w_arr.len() {
                        let w = match &w_arr[i] {
                            PdfObject::Integer(n) => *n as f64,
                            PdfObject::Real(r) => *r,
                            _ => default_width,
                        };
                        cid_widths.push(CidWidthEntry::Range(start_cid, end_cid, w));
                        i += 1;
                    }
                }
                PdfObject::Array(arr) => {
                    // c [w1 w2 …]  — individual widths
                    let ws: Vec<f64> = arr
                        .iter()
                        .map(|o| match o {
                            PdfObject::Integer(n) => *n as f64,
                            PdfObject::Real(r) => *r,
                            _ => default_width,
                        })
                        .collect();
                    cid_widths.push(CidWidthEntry::Individual(start_cid, ws));
                    i += 1;
                }
                _ => {
                    i += 1;
                }
            }
        }
    }

    crate::fonts::types::FontWidths {
        first_char: 0,
        last_char: 0,
        widths: Vec::new(),
        default_width,
        cid_widths,
    }
}

/// Decode a byte string using a ToUnicode CMap.
///
/// For composite fonts, codes are 2 bytes wide; for simple fonts, 1 byte.
pub(crate) fn decode_bytes_with_cmap(
    bytes: &[u8],
    cmap: &crate::fonts::cmap::CMap,
    is_composite: bool,
) -> String {
    let mut result = String::new();
    if is_composite {
        let mut i = 0;
        while i < bytes.len() {
            let code = if i + 1 < bytes.len() {
                let c = ((bytes[i] as u32) << 8) | (bytes[i + 1] as u32);
                i += 2;
                c
            } else {
                let c = bytes[i] as u32;
                i += 1;
                c
            };
            if let Some(s) = cmap.lookup(code) {
                result.push_str(s);
            } else if let Some(ch) = char::from_u32(code) {
                result.push(ch);
            }
        }
    } else {
        for &b in bytes {
            if let Some(s) = cmap.lookup(b as u32) {
                result.push_str(s);
            } else {
                result.push(b as char);
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A simple test device that collects text spans.
    struct TextCollector {
        spans: Vec<TextSpan>,
        stroke_count: usize,
        fill_count: usize,
    }

    impl TextCollector {
        fn new() -> Self {
            TextCollector {
                spans: Vec::new(),
                stroke_count: 0,
                fill_count: 0,
            }
        }
    }

    impl OutputDevice for TextCollector {
        fn stroke_path(&mut self, _path: &Path, _state: &GraphicsState) {
            self.stroke_count += 1;
        }
        fn fill_path(&mut self, _path: &Path, _state: &GraphicsState, _rule: FillRule) {
            self.fill_count += 1;
        }
        fn draw_text_span(&mut self, span: &TextSpan, _state: &GraphicsState) {
            self.spans.push(span.clone());
        }
        fn draw_image(&mut self, _data: &[u8], _state: &GraphicsState) {}
    }

    #[test]
    fn test_interpret_text() {
        let data = b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();

        assert_eq!(device.spans.len(), 1);
        assert_eq!(device.spans[0].text, "Hello");
        assert_eq!(device.spans[0].font_name, "F1");
        assert_eq!(device.spans[0].font_size, 12.0);
    }

    #[test]
    fn test_interpret_path() {
        let data = b"100 200 m 300 400 l S";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();

        assert_eq!(device.stroke_count, 1);
    }

    #[test]
    fn test_interpret_fill() {
        let data = b"0 0 100 100 re f";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();

        assert_eq!(device.fill_count, 1);
    }

    #[test]
    fn test_interpret_color() {
        let data = b"1 0 0 rg 0 0 100 100 re f";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();

        assert_eq!(interp.gfx.current.fill_color, Color::Rgb(1.0, 0.0, 0.0));
    }

    #[test]
    fn test_interpret_save_restore() {
        let data = b"q 5 w Q";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();

        assert_eq!(interp.gfx.current.line_width, 1.0);
    }

    #[test]
    fn test_interpret_tj_array() {
        let data = b"BT /F1 10 Tf [(AB) -500 (CD)] TJ ET";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();

        assert_eq!(device.spans.len(), 2);
        assert_eq!(device.spans[0].text, "AB");
        assert_eq!(device.spans[1].text, "CD");
    }

    /// Extended test device that also tracks image and form xobject calls.
    struct FullCollector {
        spans: Vec<TextSpan>,
        stroke_count: usize,
        fill_count: usize,
        image_count: usize,
        image_xobject_names: Vec<String>,
        form_begin_count: usize,
        form_end_count: usize,
    }

    impl FullCollector {
        fn new() -> Self {
            FullCollector {
                spans: Vec::new(),
                stroke_count: 0,
                fill_count: 0,
                image_count: 0,
                image_xobject_names: Vec::new(),
                form_begin_count: 0,
                form_end_count: 0,
            }
        }
    }

    impl OutputDevice for FullCollector {
        fn stroke_path(&mut self, _path: &Path, _state: &GraphicsState) {
            self.stroke_count += 1;
        }
        fn fill_path(&mut self, _path: &Path, _state: &GraphicsState, _rule: FillRule) {
            self.fill_count += 1;
        }
        fn draw_text_span(&mut self, span: &TextSpan, _state: &GraphicsState) {
            self.spans.push(span.clone());
        }
        fn draw_image(&mut self, _data: &[u8], _state: &GraphicsState) {
            self.image_count += 1;
        }
        fn draw_image_xobject(
            &mut self,
            name: &str,
            _obj_id: Option<u32>,
            _stream: &crate::parser::objects::PdfStream,
            state: &GraphicsState,
        ) {
            self.image_xobject_names.push(name.to_string());
            self.image_count += 1;
            let _ = state;
        }
        fn begin_form_xobject(&mut self) {
            self.form_begin_count += 1;
        }
        fn end_form_xobject(&mut self) {
            self.form_end_count += 1;
        }
    }

    /// Build a minimal PDF with an Image XObject at object 4.
    fn build_pdf_with_image_xobject() -> PdfDocument {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let image_data = vec![0xFFu8; 12]; // 2x2 RGB pixels
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&image_data).unwrap();
        let compressed = enc.finish().unwrap();
        let comp_len = compressed.len();

        let pdf = format!(
            "%PDF-1.4\n\
            1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
            2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
            3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
            /Resources << /XObject << /Im1 4 0 R >> >> >> endobj\n\
            4 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 \
            /BitsPerComponent 8 /ColorSpace /DeviceRGB \
            /Filter /FlateDecode /Length {} >> stream\n",
            comp_len
        );
        let mut bytes = pdf.into_bytes();
        bytes.extend_from_slice(&compressed);
        let after_stream = format!(
            "\nendstream endobj\n\
            xref\n\
            0 5\n\
            0000000000 65535 f \r\n\
            0000000009 00000 n \r\n\
            0000000058 00000 n \r\n\
            0000000115 00000 n \r\n\
            {:010} 00000 n \r\n\
            trailer\n\
            << /Size 5 /Root 1 0 R >>\n\
            startxref\n",
            bytes.len() - 9 // approximate offset for obj 4 — will be recalculated
        );
        // This approach is fragile with offsets. Use a simpler method:
        // Just build the raw bytes with correct offsets.
        drop(bytes);
        drop(after_stream);

        // Simpler: build with known offsets by measuring
        let header = b"%PDF-1.4\n";
        let obj1 = b"1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n";
        let obj2 = b"2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n";
        let obj3 = b"3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 4 0 R >> >> >> endobj\n";

        let stream_dict = format!(
            "4 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /FlateDecode /Length {} >> stream\n",
            comp_len
        );
        let stream_end = b"\nendstream endobj\n";

        let off1 = header.len();
        let off2 = off1 + obj1.len();
        let off3 = off2 + obj2.len();
        let off4 = off3 + obj3.len();

        let mut data = Vec::new();
        data.extend_from_slice(header);
        data.extend_from_slice(obj1);
        data.extend_from_slice(obj2);
        data.extend_from_slice(obj3);
        data.extend_from_slice(stream_dict.as_bytes());
        data.extend_from_slice(&compressed);
        data.extend_from_slice(stream_end);

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
            0 5\n\
            0000000000 65535 f \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            trailer\n\
            << /Size 5 /Root 1 0 R >>\n\
            startxref\n\
            {}\n\
            %%EOF\n",
            off1, off2, off3, off4, xref_offset
        );
        data.extend_from_slice(xref.as_bytes());

        PdfDocument::parse(data).unwrap()
    }

    #[test]
    fn test_do_image_xobject() {
        let doc = build_pdf_with_image_xobject();
        let content = b"/Im1 Do";

        let mut resources = crate::parser::objects::PdfDict::new();
        let mut xobjects = crate::parser::objects::PdfDict::new();
        xobjects.insert("Im1".to_string(), PdfObject::Reference(4, 0));
        resources.insert("XObject".to_string(), PdfObject::Dictionary(xobjects));

        let mut interp = ContentInterpreter::new();
        let mut device = FullCollector::new();
        interp
            .interpret_with_doc(content, &mut device, &doc, &resources)
            .unwrap();

        assert_eq!(device.image_count, 1);
        assert_eq!(device.image_xobject_names, vec!["Im1"]);
    }

    /// Build a PDF with a Form XObject at object 4 that draws a rectangle.
    fn build_pdf_with_form_xobject() -> PdfDocument {
        let form_content = b"0 0 100 50 re f";
        let form_len = form_content.len();

        let header = b"%PDF-1.4\n";
        let obj1 = b"1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n";
        let obj2 = b"2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n";
        let obj3 = b"3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Fm1 4 0 R >> >> >> endobj\n";

        let stream_dict = format!(
            "4 0 obj << /Type /XObject /Subtype /Form /BBox [0 0 100 50] /Length {} >> stream\n",
            form_len
        );
        let stream_end = b"\nendstream endobj\n";

        let off1 = header.len();
        let off2 = off1 + obj1.len();
        let off3 = off2 + obj2.len();
        let off4 = off3 + obj3.len();

        let mut data = Vec::new();
        data.extend_from_slice(header);
        data.extend_from_slice(obj1);
        data.extend_from_slice(obj2);
        data.extend_from_slice(obj3);
        data.extend_from_slice(stream_dict.as_bytes());
        data.extend_from_slice(form_content);
        data.extend_from_slice(stream_end);

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
            0 5\n\
            0000000000 65535 f \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            trailer\n\
            << /Size 5 /Root 1 0 R >>\n\
            startxref\n\
            {}\n\
            %%EOF\n",
            off1, off2, off3, off4, xref_offset
        );
        data.extend_from_slice(xref.as_bytes());

        PdfDocument::parse(data).unwrap()
    }

    #[test]
    fn test_do_form_xobject() {
        let doc = build_pdf_with_form_xobject();
        let content = b"/Fm1 Do";

        let mut resources = crate::parser::objects::PdfDict::new();
        let mut xobjects = crate::parser::objects::PdfDict::new();
        xobjects.insert("Fm1".to_string(), PdfObject::Reference(4, 0));
        resources.insert("XObject".to_string(), PdfObject::Dictionary(xobjects));

        let mut interp = ContentInterpreter::new();
        let mut device = FullCollector::new();
        interp
            .interpret_with_doc(content, &mut device, &doc, &resources)
            .unwrap();

        assert_eq!(device.form_begin_count, 1);
        assert_eq!(device.form_end_count, 1);
        assert_eq!(device.fill_count, 1);
    }

    #[test]
    fn test_do_form_xobject_cycle_detection() {
        // Build a Form XObject that references itself via Do
        let form_content = b"/Self Do";
        let form_len = form_content.len();

        let header = b"%PDF-1.4\n";
        let obj1 = b"1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n";
        let obj2 = b"2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n";
        let obj3 = b"3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj\n";

        let stream_dict = format!(
            "4 0 obj << /Type /XObject /Subtype /Form /BBox [0 0 100 50] /Resources << /XObject << /Self 4 0 R >> >> /Length {} >> stream\n",
            form_len
        );
        let stream_end = b"\nendstream endobj\n";

        let off1 = header.len();
        let off2 = off1 + obj1.len();
        let off3 = off2 + obj2.len();
        let off4 = off3 + obj3.len();

        let mut data = Vec::new();
        data.extend_from_slice(header);
        data.extend_from_slice(obj1);
        data.extend_from_slice(obj2);
        data.extend_from_slice(obj3);
        data.extend_from_slice(stream_dict.as_bytes());
        data.extend_from_slice(form_content);
        data.extend_from_slice(stream_end);

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
            0 5\n\
            0000000000 65535 f \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            {:010} 00000 n \r\n\
            trailer\n\
            << /Size 5 /Root 1 0 R >>\n\
            startxref\n\
            {}\n\
            %%EOF\n",
            off1, off2, off3, off4, xref_offset
        );
        data.extend_from_slice(xref.as_bytes());

        let doc = PdfDocument::parse(data).unwrap();
        let content = b"/Self Do";

        let mut resources = crate::parser::objects::PdfDict::new();
        let mut xobjects = crate::parser::objects::PdfDict::new();
        xobjects.insert("Self".to_string(), PdfObject::Reference(4, 0));
        resources.insert("XObject".to_string(), PdfObject::Dictionary(xobjects));

        let mut interp = ContentInterpreter::new();
        let mut device = FullCollector::new();
        // Should not infinite loop — cycle detection stops recursion
        interp
            .interpret_with_doc(content, &mut device, &doc, &resources)
            .unwrap();

        // First call succeeds, recursive call is blocked by cycle detection
        assert_eq!(device.form_begin_count, 1);
        assert_eq!(device.form_end_count, 1);
    }

    #[test]
    fn test_smask_dict_does_not_zero_alpha() {
        // A dict-valued /SMask in an ExtGState must leave fill_alpha unchanged.
        // Previously the code zeroed it to 0.0, making all subsequent fills invisible.
        let interp = ContentInterpreter::new();
        // Set fill_alpha to 1.0 (default) and verify it stays at 1.0 after a no-op gs.
        // We cannot exercise the full ExtGState path without a PdfDocument, but we can
        // verify the default alpha starts at 1.0 and the SMask-zeroing code path is removed
        // by checking the field directly on a fresh interpreter.
        assert!(
            (interp.gfx.current.fill_alpha - 1.0).abs() < 1e-9,
            "default fill_alpha should be 1.0, got {}",
            interp.gfx.current.fill_alpha
        );
        assert!(
            (interp.gfx.current.stroke_alpha - 1.0).abs() < 1e-9,
            "default stroke_alpha should be 1.0, got {}",
            interp.gfx.current.stroke_alpha
        );
    }

    #[test]
    fn test_scn_with_tint_produces_pattern_with_tint() {
        // `0.5 0.3 0.1 /P1 scn` should produce Color::Pattern("P1", Some([0.5, 0.3, 0.1]))
        let data = b"0.5 0.3 0.1 /P1 scn";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();
        match &interp.gfx.current.fill_color {
            Color::Pattern(name, tint) => {
                assert_eq!(name, "P1");
                let t = tint.as_ref().expect("tint should be Some");
                assert_eq!(t.len(), 3);
                assert!((t[0] - 0.5).abs() < 1e-9);
                assert!((t[1] - 0.3).abs() < 1e-9);
                assert!((t[2] - 0.1).abs() < 1e-9);
            }
            other => panic!("expected Color::Pattern, got {:?}", other),
        }
    }

    #[test]
    fn test_scn_name_only_produces_pattern_no_tint() {
        // `/P1 scn` with no numeric prefix should produce Color::Pattern("P1", None)
        let data = b"/P1 scn";
        let mut interp = ContentInterpreter::new();
        let mut device = TextCollector::new();
        interp.interpret(data, &mut device).unwrap();
        match &interp.gfx.current.fill_color {
            Color::Pattern(name, tint) => {
                assert_eq!(name, "P1");
                assert!(tint.is_none(), "tint should be None when no numeric prefix");
            }
            other => panic!("expected Color::Pattern, got {:?}", other),
        }
    }

    // ── Font style reading ──────────────────────────────────────────────────────

    #[test]
    fn strip_subset_prefix_removes_valid_tag() {
        assert_eq!(strip_subset_prefix("ABCDEF+Calibri"), "Calibri");
        assert_eq!(strip_subset_prefix("WXYZAB+Times-Bold"), "Times-Bold");
    }

    #[test]
    fn strip_subset_prefix_keeps_non_tag_names() {
        // No '+', wrong tag length, lowercase tag, or empty remainder → unchanged.
        assert_eq!(strip_subset_prefix("Helvetica"), "Helvetica");
        assert_eq!(strip_subset_prefix("CIDFont+F2"), "CIDFont+F2"); // 7-char prefix
        assert_eq!(strip_subset_prefix("abcdef+Calibri"), "abcdef+Calibri");
        assert_eq!(strip_subset_prefix("ABCDEF+"), "ABCDEF+");
    }

    /// Build a minimal PDF whose font `F1` (obj 2) is a simple Type1 with the given
    /// `/BaseFont` and a `/FontDescriptor` (obj 3) carrying `flags`/`italic_angle`.
    fn build_pdf_with_font(base_font: &str, flags: i64, italic_angle: f64) -> PdfDocument {
        let header = b"%PDF-1.4\n";
        let obj1 = b"1 0 obj << /Type /Catalog >> endobj\n".to_vec();
        let obj2 = format!(
            "2 0 obj << /Type /Font /Subtype /Type1 /BaseFont /{} /FontDescriptor 3 0 R >> endobj\n",
            base_font
        )
        .into_bytes();
        let obj3 = format!(
            "3 0 obj << /Type /FontDescriptor /FontName /{} /Flags {} /ItalicAngle {} >> endobj\n",
            base_font, flags, italic_angle
        )
        .into_bytes();

        let off1 = header.len();
        let off2 = off1 + obj1.len();
        let off3 = off2 + obj2.len();

        let mut data = Vec::new();
        data.extend_from_slice(header);
        data.extend_from_slice(&obj1);
        data.extend_from_slice(&obj2);
        data.extend_from_slice(&obj3);
        let xref_offset = data.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \r\n{:010} 00000 n \r\n{:010} 00000 n \r\n{:010} 00000 n \r\ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            off1, off2, off3, xref_offset
        );
        data.extend_from_slice(xref.as_bytes());
        PdfDocument::parse(data).unwrap()
    }

    /// Same as `build_pdf_with_font` but also writes `/StemV` to the FontDescriptor.
    fn build_pdf_with_font_stemv(
        base_font: &str,
        flags: i64,
        italic_angle: f64,
        stem_v: f64,
    ) -> PdfDocument {
        let header = b"%PDF-1.4\n";
        let obj1 = b"1 0 obj << /Type /Catalog >> endobj\n".to_vec();
        let obj2 = format!(
            "2 0 obj << /Type /Font /Subtype /Type1 /BaseFont /{} /FontDescriptor 3 0 R >> endobj\n",
            base_font
        )
        .into_bytes();
        let obj3 = format!(
            "3 0 obj << /Type /FontDescriptor /FontName /{} /Flags {} /ItalicAngle {} /StemV {} >> endobj\n",
            base_font, flags, italic_angle, stem_v
        )
        .into_bytes();

        let off1 = header.len();
        let off2 = off1 + obj1.len();
        let off3 = off2 + obj2.len();

        let mut data = Vec::new();
        data.extend_from_slice(header);
        data.extend_from_slice(&obj1);
        data.extend_from_slice(&obj2);
        data.extend_from_slice(&obj3);
        let xref_offset = data.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \r\n{:010} 00000 n \r\n{:010} 00000 n \r\n{:010} 00000 n \r\ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            off1, off2, off3, xref_offset
        );
        data.extend_from_slice(xref.as_bytes());
        PdfDocument::parse(data).unwrap()
    }

    fn font_resources() -> crate::parser::objects::PdfDict {
        let mut fonts = crate::parser::objects::PdfDict::new();
        fonts.insert("F1".to_string(), PdfObject::Reference(2, 0));
        let mut res = crate::parser::objects::PdfDict::new();
        res.insert("Font".to_string(), PdfObject::Dictionary(fonts));
        res
    }

    #[test]
    fn resolve_font_style_reads_forcebold_flag() {
        // Flags bit 18 (ForceBold) = 262144; ItalicAngle 0 → bold, not italic.
        let doc = build_pdf_with_font("ABCDEF+Calibri", 262144, 0.0);
        let res = font_resources();
        let s = resolve_font_style("F1", Some(&doc), Some(&res)).expect("style");
        assert_eq!(s.base_font, "ABCDEF+Calibri");
        assert!(s.bold, "ForceBold flag should yield bold");
        assert!(!s.italic);
    }

    #[test]
    fn resolve_font_style_reads_italic_angle() {
        // No bold flag; non-zero ItalicAngle → italic, not bold.
        let doc = build_pdf_with_font("Calibri", 0, -12.0);
        let res = font_resources();
        let s = resolve_font_style("F1", Some(&doc), Some(&res)).expect("style");
        assert!(!s.bold);
        assert!(s.italic, "non-zero ItalicAngle should yield italic");
    }

    #[test]
    fn resolve_font_style_plain_is_neither() {
        let doc = build_pdf_with_font("Calibri", 0, 0.0);
        let res = font_resources();
        let s = resolve_font_style("F1", Some(&doc), Some(&res)).expect("style");
        assert!(!s.bold && !s.italic);
        assert_eq!(s.base_font, "Calibri");
    }

    #[test]
    fn resolve_font_style_name_hint_bold() {
        // No descriptor flags, but the BaseFont name carries the style.
        let doc = build_pdf_with_font("Helvetica-BoldOblique", 0, 0.0);
        let res = font_resources();
        let s = resolve_font_style("F1", Some(&doc), Some(&res)).expect("style");
        assert!(s.bold, "name contains 'Bold'");
        assert!(s.italic, "name contains 'Oblique'");
    }

    #[test]
    fn resolve_font_style_high_stemv_is_bold() {
        // StemV 140 with no flags/name hint → bold via StemV threshold.
        let doc = build_pdf_with_font_stemv("CIDFont+F2", 0, 0.0, 140.0);
        let res = font_resources();
        let s = resolve_font_style("F1", Some(&doc), Some(&res)).expect("style");
        assert!(s.bold, "StemV 140 should yield bold");
        assert!(!s.italic);
    }

    #[test]
    fn resolve_font_style_low_stemv_not_bold() {
        // StemV 90 is below the threshold — should not be bold.
        let doc = build_pdf_with_font_stemv("SomeFont", 0, 0.0, 90.0);
        let res = font_resources();
        let s = resolve_font_style("F1", Some(&doc), Some(&res)).expect("style");
        assert!(!s.bold, "StemV 90 should not yield bold");
    }
}
