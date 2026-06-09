//! PDF Font system.
//!
//! - [`encoding`] — standard PDF encodings and Adobe Glyph List
//! - [`standard`] — 14 standard PDF font metrics
//! - [`cmap`] — ToUnicode CMap and CID CMap parsing
//! - [`truetype`] — TrueType/OpenType font table parsing
//! - [`type1`] — Type1 font metric extraction
//! - [`cff`] — CFF (Compact Font Format) glyph width extraction
//! - [`font_cache`] — font loading, caching, and Unicode resolution
//! - [`types`] — core font types (FontType, FontDescriptor, FontWidths)

pub mod cff;
pub mod cmap;
pub mod encoding;
pub mod font_cache;
pub mod standard;
pub mod truetype;
pub mod type1;
pub mod types;
