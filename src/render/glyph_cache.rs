//! Glyph rasterization via `fontdue`.
//!
//! Rasterizes individual glyphs into 8-bit alpha masks and caches them so
//! repeated draws of the same character at the same size cost nothing beyond
//! a hash-map lookup.  `fontdue` handles both TrueType and CFF/OpenType
//! outlines, which `ab_glyph` cannot.

use std::collections::HashMap;

#[cfg(feature = "render")]
use fontdue::Font as FdFont;
#[cfg(feature = "render")]
use std::rc::Rc;

/// Fast content hash of a font program's bytes (used as a cross-render cache key).
#[cfg(feature = "render")]
fn font_bytes_hash(bytes: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_usize(bytes.len());
    h.write(bytes);
    h.finish()
}

#[cfg(feature = "render")]
thread_local! {
    /// Parsed `fontdue` faces shared across ALL renders, keyed by a hash of the
    /// font-program bytes. Because the key is the font *content*, it survives the
    /// per-commit document reparse (same bytes → same key), so an embedded font is
    /// parsed once — not re-parsed on every render / keystroke. Bounded crudely
    /// (fonts are few and large); cleared wholesale past the cap.
    static PARSED_FONTS: std::cell::RefCell<HashMap<u64, Rc<FdFont>>>
        = std::cell::RefCell::new(HashMap::new());
}

/// Return a parsed face for `font_bytes`, parsing (and caching) only on a miss.
/// The bytes are hashed once per call (callers hold the `Rc` per render, so this
/// is not invoked per glyph).
#[cfg(feature = "render")]
fn get_or_parse_font(font_bytes: &[u8]) -> Option<Rc<FdFont>> {
    const MAX_FONTS: usize = 32;
    let key = font_bytes_hash(font_bytes);
    PARSED_FONTS.with(|c| {
        let cached = c.borrow().get(&key).map(Rc::clone);
        if let Some(rc) = cached {
            return Some(rc);
        }
        match FdFont::from_bytes(font_bytes, fontdue::FontSettings::default()) {
            Ok(f) => {
                let rc = Rc::new(f);
                let mut m = c.borrow_mut();
                if m.len() >= MAX_FONTS {
                    m.clear();
                }
                m.insert(key, Rc::clone(&rc));
                Some(rc)
            }
            Err(e) => {
                log::warn!("[glyph-cache] font parse failed: {}", e);
                None
            }
        }
    })
}

/// An 8-bit alpha mask for a single rasterized glyph.
#[derive(Debug, Clone)]
pub struct GlyphBitmap {
    /// Per-pixel coverage [0, 255].  Row-major, left-to-right, top-to-bottom.
    pub pixels: Vec<u8>,
    /// Width of the alpha mask in pixels.
    pub width: u32,
    /// Height of the alpha mask in pixels.
    pub height: u32,
    /// Horizontal offset from the pen position to the left edge of the mask.
    pub bearing_x: f32,
    /// Vertical offset from the baseline to the top edge of the mask.
    /// Positive values move the glyph upward (above the baseline).
    pub bearing_y: f32,
    /// Horizontal advance width in pixels for pen advancement.
    pub advance_x: f32,
}

/// Cache key: (font resource name, char, size in 1/64 px units).
type GlyphKey = (String, char, u32);

/// Cache key for GID-indexed rasterization: (font resource name, glyph index, size in 1/64 px).
type GlyphGidKey = (String, u16, u32);

/// Caches rasterized glyphs keyed by font name + character + size.
pub struct GlyphCache {
    /// Per-render handle to the parsed face for each resource name. The face
    /// itself lives in the cross-render [`PARSED_FONTS`] cache; this map just
    /// avoids re-hashing the font bytes for every glyph within one render.
    #[cfg(feature = "render")]
    fonts: HashMap<String, Rc<FdFont>>,
    /// Cached glyph bitmaps (char-based lookup).
    bitmaps: HashMap<GlyphKey, GlyphBitmap>,
    /// Cached glyph bitmaps (GID-based lookup — avoids collision with char keys).
    #[cfg(feature = "render")]
    gid_bitmaps: HashMap<GlyphGidKey, GlyphBitmap>,
}

impl GlyphCache {
    /// Create an empty glyph cache.
    pub fn new() -> Self {
        GlyphCache {
            #[cfg(feature = "render")]
            fonts: HashMap::new(),
            bitmaps: HashMap::new(),
            #[cfg(feature = "render")]
            gid_bitmaps: HashMap::new(),
        }
    }

    /// Rasterize (or return from cache) a glyph for the given char at `size_px`.
    ///
    /// `font_name` is the PDF resource name (e.g. `"F1"`).
    /// `font_bytes` is the raw TTF or CFF/OTF binary data extracted from the PDF.
    ///
    /// Returns `None` if the font can't be parsed or the glyph has no outline
    /// (spaces, control chars, or missing glyphs).
    #[cfg(feature = "render")]
    pub fn rasterize(
        &mut self,
        font_name: &str,
        font_bytes: &[u8],
        ch: char,
        size_px: f32,
    ) -> Option<&GlyphBitmap> {
        let size_key = (size_px * 64.0).round() as u32;
        let key: GlyphKey = (font_name.to_string(), ch, size_key);

        if self.bitmaps.contains_key(&key) {
            return self.bitmaps.get(&key);
        }

        // Resolve the parsed face (cached across renders by font-bytes hash).
        if !self.fonts.contains_key(font_name) {
            let parsed = get_or_parse_font(font_bytes)?;
            self.fonts.insert(font_name.to_string(), parsed);
        }
        let font = self.fonts.get(font_name)?;

        // Rasterize returns (Metrics, Vec<u8> alpha mask).
        let (metrics, pixels) = font.rasterize(ch, size_px);

        if metrics.width == 0 || metrics.height == 0 {
            // Space or zero-extent glyph: no bitmap, but fontdue gives us the real advance.
            let bitmap = GlyphBitmap {
                pixels: vec![],
                width: 0,
                height: 0,
                bearing_x: 0.0,
                bearing_y: 0.0,
                advance_x: metrics.advance_width,
            };
            self.bitmaps.insert(key.clone(), bitmap);
            return self.bitmaps.get(&key);
        }

        // fontdue coordinate system:
        //   metrics.xmin = horizontal offset from pen to left edge of bitmap
        //   metrics.ymin = bottom of glyph relative to baseline (can be negative)
        //   bearing_y    = distance from baseline to TOP of glyph (positive = above)
        let bearing_x = metrics.xmin as f32;
        let bearing_y = (metrics.ymin + metrics.height as i32) as f32;
        let advance_x = metrics.advance_width;

        let bitmap = GlyphBitmap {
            pixels,
            width: metrics.width as u32,
            height: metrics.height as u32,
            bearing_x,
            bearing_y,
            advance_x,
        };

        self.bitmaps.insert(key.clone(), bitmap);
        self.bitmaps.get(&key)
    }

    /// Rasterize a glyph by its raw GID (glyph index) rather than Unicode char.
    ///
    /// Use for CID-encoded fonts where CID == GID (Identity-H encoding), bypassing
    /// the font's internal cmap table. Covers the ONLYOFFICE-style approach where
    /// the GID is looked up directly rather than via Unicode → cmap → GID.
    ///
    /// Returns `None` only on font parse failure; zero-extent glyphs (spaces, control
    /// characters) return `Some` with an empty pixel buffer and the correct advance.
    #[cfg(feature = "render")]
    pub fn rasterize_by_gid(
        &mut self,
        font_name: &str,
        font_bytes: &[u8],
        gid: u16,
        size_px: f32,
    ) -> Option<&GlyphBitmap> {
        let size_key = (size_px * 64.0).round() as u32;
        let key: GlyphGidKey = (font_name.to_string(), gid, size_key);

        if self.gid_bitmaps.contains_key(&key) {
            return self.gid_bitmaps.get(&key);
        }

        // Resolve the parsed face (cached across renders by font-bytes hash;
        // shared with rasterize() via the per-render `fonts` map).
        if !self.fonts.contains_key(font_name) {
            let parsed = get_or_parse_font(font_bytes)?;
            self.fonts.insert(font_name.to_string(), parsed);
        }
        let font = self.fonts.get(font_name)?;

        let (metrics, pixels) = font.rasterize_indexed(gid, size_px);

        if metrics.width == 0 || metrics.height == 0 {
            let bitmap = GlyphBitmap {
                pixels: vec![],
                width: 0,
                height: 0,
                bearing_x: 0.0,
                bearing_y: 0.0,
                advance_x: metrics.advance_width,
            };
            self.gid_bitmaps.insert(key.clone(), bitmap);
            return self.gid_bitmaps.get(&key);
        }

        let bearing_x = metrics.xmin as f32;
        let bearing_y = (metrics.ymin + metrics.height as i32) as f32;
        let bitmap = GlyphBitmap {
            pixels,
            width: metrics.width as u32,
            height: metrics.height as u32,
            bearing_x,
            bearing_y,
            advance_x: metrics.advance_width,
        };
        self.gid_bitmaps.insert(key.clone(), bitmap);
        self.gid_bitmaps.get(&key)
    }
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// FontBytesCache
// ---------------------------------------------------------------------------

/// Cache of decoded font stream bytes keyed by PDF resource name (e.g. `"F1"`).
///
/// Storing decoded TTF/CFF bytes here lets multiple render tiles on the same
/// page share the result of a single XRef walk + FlateDecode call per font,
/// rather than re-running FlateDecode for every tile.  Pass a `FontBytesCache`
/// (inside a [`RenderCache`]) across consecutive `render_tile_with_render_cache`
/// calls to get this benefit.
pub struct FontBytesCache {
    entries: HashMap<String, Option<Vec<u8>>>,
}

impl FontBytesCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        FontBytesCache {
            entries: HashMap::new(),
        }
    }

    /// Returns `true` if an entry (present bytes or recorded failure) exists for `key`.
    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Insert decoded bytes, or `None` to record a failed extraction attempt.
    pub fn insert(&mut self, key: String, bytes: Option<Vec<u8>>) {
        self.entries.insert(key, bytes);
    }

    /// Return the decoded bytes for `key`, or `None` if absent or extraction failed.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.entries.get(key)?.as_deref()
    }
}

impl Default for FontBytesCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// RenderCache
// ---------------------------------------------------------------------------

/// Combined cache that persists across render-tile calls on the same page.
///
/// Pass this into [`render_tile_with_render_cache`] and receive it back on
/// success.  Reusing it across tiles avoids re-rasterising glyphs
/// ([`GlyphCache`]) and re-decoding font streams ([`FontBytesCache`]).
///
/// [`render_tile_with_render_cache`]: crate::render::render_tile_with_render_cache
pub struct RenderCache {
    /// Rasterised glyph bitmaps shared across tiles.
    pub glyphs: GlyphCache,
    /// Decoded font stream bytes shared across tiles.
    pub font_bytes: FontBytesCache,
}

impl RenderCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        RenderCache {
            glyphs: GlyphCache::new(),
            font_bytes: FontBytesCache::new(),
        }
    }
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "render")]
    fn test_rasterize_letter_a() {
        // Try to read a test font from the fixture directory at runtime.
        // If absent, skip the test rather than fail.
        let path = format!(
            "{}/tests/fixtures/test_font.ttf",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return, // fixture absent — skip
        };
        let mut cache = GlyphCache::new();
        let bitmap = cache.rasterize("F1", &bytes, 'A', 24.0);
        assert!(bitmap.is_some(), "expected Some(GlyphBitmap) for 'A'");
        let bm = bitmap.unwrap();
        assert!(bm.width > 0);
        assert!(bm.height > 0);
        let has_coverage = bm.pixels.iter().any(|&p| p > 0);
        assert!(has_coverage, "expected non-zero coverage pixels");
    }

    #[test]
    #[cfg(feature = "render")]
    fn test_rasterize_invalid_font_returns_none() {
        let mut cache = GlyphCache::new();
        let result = cache.rasterize("Bad", b"not a ttf", 'A', 12.0);
        assert!(result.is_none());
    }

    #[test]
    #[cfg(feature = "render")]
    fn parsed_font_cache_reuses_same_rc() {
        let path = format!(
            "{}/tests/fixtures/test_font.ttf",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return, // fixture absent — skip
        };
        let a = get_or_parse_font(&bytes).expect("valid font parses");
        let b = get_or_parse_font(&bytes).expect("valid font parses");
        assert!(
            std::rc::Rc::ptr_eq(&a, &b),
            "same font bytes must reuse the cached parsed face"
        );
    }

    #[test]
    #[cfg(feature = "render")]
    fn parsed_font_cache_rejects_garbage() {
        assert!(get_or_parse_font(b"not a font").is_none());
    }
}
