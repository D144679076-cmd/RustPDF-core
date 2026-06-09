//! Page rendering — rasterizes PDF pages into RGBA pixel buffers.
//!
//! Enabled by the `render` Cargo feature.  All types are WASM-safe.
//!
//! ## Quick start
//!
//! ```rust,ignore
//! use pdf_core::{PdfDocument, render::{render_page, render_tile, tile::TileRect}};
//!
//! let doc = PdfDocument::parse(bytes)?;
//! let page = /* get Page from Catalog */;
//!
//! // Full page at 144 DPI
//! let buf = render_page(&doc, &page, 2.0)?;
//! // buf.data() returns raw RGBA bytes
//!
//! // Single tile
//! let tile = TileRect { x: 0.0, y: 0.0, width: 200.0, height: 200.0 };
//! let tile_buf = render_tile(&doc, &page, 2.0, tile)?;
//! ```

pub mod canvas;
pub mod color;
pub mod font_resolver;
pub mod glyph_cache;
pub mod image;
pub mod page_renderer;
pub mod path_render;
pub mod shading;
pub mod tile;

pub use canvas::PixmapBuffer;
#[cfg(not(target_arch = "wasm32"))]
pub use font_resolver::DirectoryFontResolver;
pub use font_resolver::{register_font, EmbeddedFontResolver, FontResolver};
pub use glyph_cache::{FontBytesCache, GlyphCache, RenderCache};
pub use page_renderer::{
    render_block_tile, render_page, render_page_rgba, render_page_with_cache,
    render_page_with_render_cache, render_page_with_resolver, render_tile, render_tile_content,
    render_tile_with_cache, render_tile_with_render_cache, render_tile_with_resolver,
};
pub use tile::{TileCache, TileKey, TileRect};
