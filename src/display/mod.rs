//! Display list — a flat, renderer-ready representation of a PDF page.
//!
//! The [`DisplayList`] is built by running the content stream interpreter with
//! a [`DisplayListBuilder`] as the [`OutputDevice`]. Each [`DisplayItem`]
//! captures a single drawing command together with a snapshot of the graphics
//! state needed to render it, so the list is fully self-contained.
//!
//! # Usage
//! ```rust,ignore
//! let list = DisplayList::from_page(&doc, &page)?;
//! for item in &list.items {
//!     match item { ... }
//! }
//! ```

use crate::content::graphics_state::{
    Color, FillRule, GraphicsState, LineCap, LineJoin, Matrix, Path,
};
use crate::content::interpreter::{ContentInterpreter, OutputDevice};
use crate::content::text_state::TextSpan;
use crate::document::page::Page;
use crate::error::Result;
use crate::parser::objects::{PdfDocument, PdfStream};

// ---------------------------------------------------------------------------
// Stroke style snapshot
// ---------------------------------------------------------------------------

/// Snapshot of stroke-relevant graphics state parameters.
#[derive(Debug, Clone)]
pub struct StrokeStyle {
    /// Stroke color.
    pub color: Color,
    /// Line width in user space.
    pub line_width: f64,
    /// Line cap style.
    pub line_cap: LineCap,
    /// Line join style.
    pub line_join: LineJoin,
    /// Miter limit.
    pub miter_limit: f64,
    /// Dash array and phase.
    pub dash_array: Vec<f64>,
    pub dash_phase: f64,
    /// Stroke opacity [0.0, 1.0].
    pub alpha: f64,
}

impl StrokeStyle {
    fn from_state(gs: &GraphicsState) -> Self {
        StrokeStyle {
            color: gs.stroke_color.clone(),
            line_width: gs.line_width,
            line_cap: gs.line_cap,
            line_join: gs.line_join,
            miter_limit: gs.miter_limit,
            dash_array: gs.dash_pattern.array.clone(),
            dash_phase: gs.dash_pattern.phase,
            alpha: gs.stroke_alpha,
        }
    }
}

// ---------------------------------------------------------------------------
// Fill style snapshot
// ---------------------------------------------------------------------------

/// Snapshot of fill-relevant graphics state parameters.
#[derive(Debug, Clone)]
pub struct FillStyle {
    /// Fill color.
    pub color: Color,
    /// Fill opacity [0.0, 1.0].
    pub alpha: f64,
}

impl FillStyle {
    fn from_state(gs: &GraphicsState) -> Self {
        FillStyle {
            color: gs.fill_color.clone(),
            alpha: gs.fill_alpha,
        }
    }
}

// ---------------------------------------------------------------------------
// Text item
// ---------------------------------------------------------------------------

/// A single rendered text span ready for layout or rasterization.
#[derive(Debug, Clone)]
pub struct TextItem {
    /// Unicode text content.
    pub text: String,
    /// X position of the span's origin in user space (PDF coordinates).
    pub x: f64,
    /// Y position of the span's baseline in user space (PDF coordinates).
    pub y: f64,
    /// Font size in user space units.
    pub font_size: f64,
    /// Font resource name (e.g. `"F1"`).
    pub font_name: String,
    /// Rendered width of the span in user space.
    pub width: f64,
    /// Fill color for the text.
    pub color: Color,
    /// Current transformation matrix at the time of rendering.
    pub ctm: Matrix,
    /// Fill opacity.
    pub alpha: f64,
}

impl TextItem {
    fn from_span(span: &TextSpan, gs: &GraphicsState) -> Self {
        TextItem {
            text: span.text.clone(),
            x: span.x,
            y: span.y,
            font_size: span.font_size,
            font_name: span.font_name.clone(),
            width: span.width,
            color: gs.fill_color.clone(),
            ctm: gs.ctm,
            alpha: gs.fill_alpha,
        }
    }
}

// ---------------------------------------------------------------------------
// Image item
// ---------------------------------------------------------------------------

/// A decoded image ready for compositing.
#[derive(Debug, Clone)]
pub struct ImageItem {
    /// Raw decoded pixel data (format depends on the image XObject).
    pub data: Vec<u8>,
    /// Current transformation matrix — defines position, scale, and rotation.
    pub ctm: Matrix,
    /// Blend mode name (e.g. `"Normal"`).
    pub blend_mode: String,
    /// Opacity [0.0, 1.0].
    pub alpha: f64,
}

impl ImageItem {
    fn from_data(data: Vec<u8>, gs: &GraphicsState) -> Self {
        let blend_mode = format!("{:?}", gs.blend_mode);
        ImageItem {
            data,
            ctm: gs.ctm,
            blend_mode,
            alpha: gs.fill_alpha,
        }
    }
}

// ---------------------------------------------------------------------------
// DisplayItem
// ---------------------------------------------------------------------------

/// A single drawing command in a [`DisplayList`].
#[derive(Debug, Clone)]
pub enum DisplayItem {
    /// Stroke a path.
    StrokePath {
        path: Path,
        style: StrokeStyle,
        /// CTM at the time of the stroke operation.
        ctm: Matrix,
    },
    /// Fill a path.
    FillPath {
        path: Path,
        style: FillStyle,
        rule: FillRule,
        /// CTM at the time of the fill operation.
        ctm: Matrix,
    },
    /// Draw a text span.
    DrawText(TextItem),
    /// Draw an image.
    DrawImage(ImageItem),
}

// ---------------------------------------------------------------------------
// DisplayList
// ---------------------------------------------------------------------------

/// A flat, ordered list of drawing commands for one PDF page.
///
/// Build with [`DisplayList::from_page`], then iterate [`DisplayList::items`]
/// to drive a renderer.
#[derive(Debug, Clone, Default)]
pub struct DisplayList {
    /// Drawing commands in paint order (back to front).
    pub items: Vec<DisplayItem>,
}

impl DisplayList {
    /// Build a display list by interpreting a page's content streams.
    pub fn from_page(doc: &PdfDocument, page: &Page) -> Result<Self> {
        let content = page.decode_contents(doc)?;
        let mut builder = DisplayListBuilder::new();
        let mut interp = ContentInterpreter::new();
        interp.interpret_with_doc(&content, &mut builder, doc, &page.resources.fonts)?;
        Ok(builder.finish())
    }

    /// Number of items in the list.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True if the list contains no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterate only text items.
    pub fn text_items(&self) -> impl Iterator<Item = &TextItem> {
        self.items.iter().filter_map(|item| match item {
            DisplayItem::DrawText(t) => Some(t),
            _ => None,
        })
    }

    /// Iterate only image items.
    pub fn image_items(&self) -> impl Iterator<Item = &ImageItem> {
        self.items.iter().filter_map(|item| match item {
            DisplayItem::DrawImage(img) => Some(img),
            _ => None,
        })
    }
}

// ---------------------------------------------------------------------------
// DisplayListBuilder (OutputDevice impl)
// ---------------------------------------------------------------------------

struct DisplayListBuilder {
    items: Vec<DisplayItem>,
}

impl DisplayListBuilder {
    fn new() -> Self {
        DisplayListBuilder { items: Vec::new() }
    }

    fn finish(self) -> DisplayList {
        DisplayList { items: self.items }
    }
}

impl OutputDevice for DisplayListBuilder {
    fn stroke_path(&mut self, path: &Path, state: &GraphicsState) {
        if path.is_empty() {
            return;
        }
        self.items.push(DisplayItem::StrokePath {
            path: path.clone(),
            style: StrokeStyle::from_state(state),
            ctm: state.ctm,
        });
    }

    fn fill_path(&mut self, path: &Path, state: &GraphicsState, rule: FillRule) {
        if path.is_empty() {
            return;
        }
        self.items.push(DisplayItem::FillPath {
            path: path.clone(),
            style: FillStyle::from_state(state),
            rule,
            ctm: state.ctm,
        });
    }

    fn draw_text_span(&mut self, span: &TextSpan, state: &GraphicsState) {
        if span.text.is_empty() {
            return;
        }
        self.items
            .push(DisplayItem::DrawText(TextItem::from_span(span, state)));
    }

    fn draw_image(&mut self, image_data: &[u8], state: &GraphicsState) {
        self.items.push(DisplayItem::DrawImage(ImageItem::from_data(
            image_data.to_vec(),
            state,
        )));
    }

    fn draw_image_xobject(
        &mut self,
        _name: &str,
        _obj_id: Option<u32>,
        stream: &PdfStream,
        state: &GraphicsState,
    ) {
        if let Ok(data) = stream.decode() {
            self.items
                .push(DisplayItem::DrawImage(ImageItem::from_data(data, state)));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::graphics_state::{Color, GraphicsState, Path};
    use crate::content::text_state::TextSpan;

    fn default_gs() -> GraphicsState {
        GraphicsState::default()
    }

    #[test]
    fn test_stroke_style_from_state() {
        let mut gs = default_gs();
        gs.stroke_color = Color::Rgb(1.0, 0.0, 0.0);
        gs.line_width = 2.5;
        gs.stroke_alpha = 0.8;
        let style = StrokeStyle::from_state(&gs);
        assert_eq!(style.color, Color::Rgb(1.0, 0.0, 0.0));
        assert_eq!(style.line_width, 2.5);
        assert_eq!(style.alpha, 0.8);
    }

    #[test]
    fn test_fill_style_from_state() {
        let mut gs = default_gs();
        gs.fill_color = Color::Rgb(0.0, 0.0, 1.0);
        gs.fill_alpha = 0.5;
        let style = FillStyle::from_state(&gs);
        assert_eq!(style.color, Color::Rgb(0.0, 0.0, 1.0));
        assert_eq!(style.alpha, 0.5);
    }

    #[test]
    fn test_text_item_from_span() {
        let span = TextSpan {
            text: "Hello".to_string(),
            x: 10.0,
            y: 20.0,
            font_size: 12.0,
            font_size_px: 12.0,
            font_name: "F1".to_string(),
            width: 30.0,
            char_advances: vec![],
            char_advances_y: vec![],
            char_cids: vec![],
            render_matrix_2x2: [1.0, 0.0, 0.0, -1.0],
            stroke_text: false,
        };
        let mut gs = default_gs();
        gs.fill_color = Color::Gray(0.0);
        gs.fill_alpha = 1.0;
        let item = TextItem::from_span(&span, &gs);
        assert_eq!(item.text, "Hello");
        assert_eq!(item.x, 10.0);
        assert_eq!(item.y, 20.0);
        assert_eq!(item.font_size, 12.0);
        assert_eq!(item.font_name, "F1");
        assert_eq!(item.width, 30.0);
    }

    #[test]
    fn test_builder_collects_stroke() {
        let mut builder = DisplayListBuilder::new();
        let mut path = Path::new();
        path.move_to(0.0, 0.0);
        path.line_to(100.0, 0.0);
        builder.stroke_path(&path, &default_gs());
        let list = builder.finish();
        assert_eq!(list.items.len(), 1);
        assert!(matches!(list.items[0], DisplayItem::StrokePath { .. }));
    }

    #[test]
    fn test_builder_skips_empty_path() {
        let mut builder = DisplayListBuilder::new();
        builder.stroke_path(&Path::new(), &default_gs());
        builder.fill_path(&Path::new(), &default_gs(), FillRule::NonZero);
        let list = builder.finish();
        assert!(list.is_empty());
    }

    #[test]
    fn test_builder_collects_fill() {
        let mut builder = DisplayListBuilder::new();
        let mut path = Path::new();
        path.rect(0.0, 0.0, 100.0, 50.0);
        builder.fill_path(&path, &default_gs(), FillRule::EvenOdd);
        let list = builder.finish();
        assert_eq!(list.items.len(), 1);
        assert!(matches!(
            list.items[0],
            DisplayItem::FillPath {
                rule: FillRule::EvenOdd,
                ..
            }
        ));
    }

    #[test]
    fn test_builder_skips_empty_text() {
        let mut builder = DisplayListBuilder::new();
        let span = TextSpan {
            text: String::new(),
            x: 0.0,
            y: 0.0,
            font_size: 12.0,
            font_size_px: 12.0,
            font_name: "F1".to_string(),
            width: 0.0,
            char_advances: vec![],
            char_advances_y: vec![],
            char_cids: vec![],
            render_matrix_2x2: [1.0, 0.0, 0.0, -1.0],
            stroke_text: false,
        };
        builder.draw_text_span(&span, &default_gs());
        let list = builder.finish();
        assert!(list.is_empty());
    }

    #[test]
    fn test_builder_collects_text() {
        let mut builder = DisplayListBuilder::new();
        let span = TextSpan {
            text: "Hi".to_string(),
            x: 5.0,
            y: 10.0,
            font_size: 14.0,
            font_size_px: 14.0,
            font_name: "F2".to_string(),
            width: 20.0,
            char_advances: vec![],
            char_advances_y: vec![],
            char_cids: vec![],
            render_matrix_2x2: [1.0, 0.0, 0.0, -1.0],
            stroke_text: false,
        };
        builder.draw_text_span(&span, &default_gs());
        let list = builder.finish();
        assert_eq!(list.items.len(), 1);
        let texts: Vec<_> = list.text_items().collect();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].text, "Hi");
    }

    #[test]
    fn test_builder_collects_image() {
        let mut builder = DisplayListBuilder::new();
        builder.draw_image(&[0xFF, 0x00, 0x00], &default_gs());
        let list = builder.finish();
        assert_eq!(list.items.len(), 1);
        let images: Vec<_> = list.image_items().collect();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].data, vec![0xFF, 0x00, 0x00]);
    }

    #[test]
    fn test_display_list_from_minimal_pdf() {
        let data = std::fs::read(format!(
            "{}/tests/fixtures/minimal.pdf",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let doc = crate::parser::objects::PdfDocument::parse(data).unwrap();
        let catalog = crate::document::catalog::Catalog::from_document(&doc).unwrap();
        let page_dict = catalog.get_page_dict(&doc, 0).unwrap();
        let page = crate::document::page::Page::from_dict(&doc, &page_dict).unwrap();
        let list = DisplayList::from_page(&doc, &page).unwrap();
        // minimal.pdf has no content stream — list should be empty or have no items
        let _ = list.len(); // just confirm it doesn't panic
    }
}
