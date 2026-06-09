//! Tile geometry and LRU tile cache.
//!
//! `TileRect` describes a region in PDF user-space (points).  `TileCache`
//! stores rendered `PixmapBuffer`s keyed by `TileKey`, evicting the
//! least-recently-used tiles when total memory exceeds a configurable cap.

use std::collections::{HashMap, VecDeque};

use crate::document::page::Rect;
use crate::error::Result;

use super::canvas::{PixmapBuffer, TileOrigin};

/// A rectangular region in PDF user-space coordinates (points).
///
/// The origin is the lower-left corner of the tile (PDF convention).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileRect {
    /// Lower-left x in PDF user space.
    pub x: f64,
    /// Lower-left y in PDF user space.
    pub y: f64,
    /// Width in PDF user space.
    pub width: f64,
    /// Height in PDF user space.
    pub height: f64,
}

impl TileRect {
    /// Divide a page's `media_box` into a grid of square tiles.
    ///
    /// `tile_size_pts` is the desired tile edge length in PDF points.
    /// The last row and column may be smaller.
    pub fn tile_grid(media_box: &Rect, tile_size_pts: f64) -> Vec<TileRect> {
        let pw = media_box.width();
        let ph = media_box.height();
        let ts = tile_size_pts.max(1.0);

        let cols = (pw / ts).ceil() as usize;
        let rows = (ph / ts).ceil() as usize;

        let mut tiles = Vec::with_capacity(cols * rows);
        for row in 0..rows {
            for col in 0..cols {
                let x = media_box.x1 + col as f64 * ts;
                let y = media_box.y1 + row as f64 * ts;
                let w = (x + ts).min(media_box.x1 + pw) - x;
                let h = (y + ts).min(media_box.y1 + ph) - y;
                tiles.push(TileRect {
                    x,
                    y,
                    width: w,
                    height: h,
                });
            }
        }
        tiles
    }

    /// Convert this tile to a pixel-space `TileOrigin` and pixel dimensions.
    ///
    /// `page_height_pts` is the full page height in PDF points (needed for Y-flip).
    /// Returns `(origin, width_px, height_px)`.
    pub fn to_pixel_space(&self, page_height_pts: f64, scale: f32) -> (TileOrigin, u32, u32) {
        let origin_x = (self.x * scale as f64).round() as u32;
        // PDF y=0 is page bottom; screen y=0 is page top.
        // Top edge of this tile in screen-y = page_height - (self.y + self.height)
        let origin_y = ((page_height_pts - self.y - self.height) * scale as f64).round() as u32;
        let w = (self.width * scale as f64).round() as u32;
        let h = (self.height * scale as f64).round() as u32;
        (
            TileOrigin {
                x: origin_x,
                y: origin_y,
            },
            w.max(1),
            h.max(1),
        )
    }
}

/// Identifies a rendered tile.  Uses integer fields so it is hashable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileKey {
    /// 0-based page index.
    pub page_index: u32,
    /// Scale factor × 64, truncated to integer.
    pub scale_x64: u32,
    /// Tile lower-left x in PDF points × 100.
    pub tile_x100: i64,
    /// Tile lower-left y in PDF points × 100.
    pub tile_y100: i64,
}

impl TileKey {
    pub fn new(page_index: u32, scale: f32, tile: &TileRect) -> Self {
        TileKey {
            page_index,
            scale_x64: (scale * 64.0) as u32,
            tile_x100: (tile.x * 100.0).round() as i64,
            tile_y100: (tile.y * 100.0).round() as i64,
        }
    }
}

/// LRU cache of rendered tiles.
///
/// Tiles are stored in `entries` (ordered oldest→newest).  On access the key
/// is moved to the back.  When `total_bytes` exceeds `max_bytes`, entries are
/// evicted from the front.
///
/// `order` is a `VecDeque` so front-eviction (`pop_front`) and rear-insertion
/// (`push_back`) are both O(1).  LRU promotion on `get` still requires an O(n)
/// `retain` pass; for the typical cache sizes in PDF rendering (≤ ~100 tiles)
/// this is negligible compared to the render cost.
pub struct TileCache {
    /// Insertion-ordered keys for LRU tracking (front = oldest).
    order: VecDeque<TileKey>,
    /// Actual pixel buffers.
    entries: HashMap<TileKey, PixmapBuffer>,
    /// Current total pixel memory in bytes.
    total_bytes: usize,
    /// Maximum allowed pixel memory in bytes.
    pub max_bytes: usize,
}

impl TileCache {
    /// Create a cache with the given memory cap (in bytes).
    pub fn new(max_bytes: usize) -> Self {
        TileCache {
            order: VecDeque::new(),
            entries: HashMap::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    /// Default cap: 64 MiB.
    pub fn default_cap() -> Self {
        Self::new(64 * 1024 * 1024)
    }

    /// Insert a rendered tile, evicting LRU entries if necessary.
    pub fn insert(&mut self, key: TileKey, buf: PixmapBuffer) {
        let tile_bytes = buf.width as usize * buf.height as usize * 4;

        // Remove existing entry with same key.
        if self.entries.contains_key(&key) {
            let old = self.entries.remove(&key).unwrap();
            self.total_bytes -= old.width as usize * old.height as usize * 4;
            self.order.retain(|k| k != &key);
        }

        self.total_bytes += tile_bytes;
        self.order.push_back(key.clone());
        self.entries.insert(key, buf);

        self.evict_until_under_limit();
    }

    /// Look up a tile.  Moves it to the MRU (back) position.
    pub fn get(&mut self, key: &TileKey) -> Option<&PixmapBuffer> {
        if self.entries.contains_key(key) {
            // Move key to back (most recently used).
            self.order.retain(|k| k != key);
            self.order.push_back(key.clone());
            self.entries.get(key)
        } else {
            None
        }
    }

    /// Fetch from cache or render on demand using the provided closure.
    ///
    /// The closure receives `(page_index, tile_rect)` and must produce a
    /// `Result<PixmapBuffer>`.  On success the buffer is inserted into the
    /// cache.
    pub fn get_or_render<F>(
        &mut self,
        key: TileKey,
        tile: TileRect,
        render: F,
    ) -> Result<&PixmapBuffer>
    where
        F: FnOnce(TileRect) -> Result<PixmapBuffer>,
    {
        if !self.entries.contains_key(&key) {
            let buf = render(tile)?;
            self.insert(key.clone(), buf);
        } else {
            // Touch for LRU.
            self.order.retain(|k| k != &key);
            self.order.push_back(key.clone());
        }
        Ok(self.entries.get(&key).unwrap())
    }

    /// Current number of cached tiles.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // ---------------------------------------------------------------------------

    fn evict_until_under_limit(&mut self) {
        while self.total_bytes > self.max_bytes && !self.order.is_empty() {
            // pop_front is O(1) on VecDeque, vs O(n) Vec::remove(0).
            if let Some(oldest) = self.order.pop_front() {
                if let Some(buf) = self.entries.remove(&oldest) {
                    self.total_bytes -= buf.width as usize * buf.height as usize * 4;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key(page: u32, tx: f64) -> TileKey {
        TileKey::new(
            page,
            1.0,
            &TileRect {
                x: tx,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        )
    }

    fn make_tile_buf(w: u32, h: u32) -> PixmapBuffer {
        PixmapBuffer::new(w, h).unwrap()
    }

    #[test]
    fn test_tile_grid_full_page() {
        let mb = Rect {
            x1: 0.0,
            y1: 0.0,
            x2: 612.0,
            y2: 792.0,
        };
        let tiles = TileRect::tile_grid(&mb, 256.0);
        // 3 cols (0-256, 256-512, 512-612), 4 rows (0-256, 256-512, 512-768, 768-792)
        assert_eq!(tiles.len(), 3 * 4);
    }

    #[test]
    fn test_tile_grid_total_coverage() {
        let mb = Rect {
            x1: 0.0,
            y1: 0.0,
            x2: 100.0,
            y2: 100.0,
        };
        let tiles = TileRect::tile_grid(&mb, 30.0);
        let total_area: f64 = tiles.iter().map(|t| t.width * t.height).sum();
        let diff = (total_area - 100.0 * 100.0).abs();
        assert!(
            diff < 1.0,
            "total tile area should equal page area, diff={}",
            diff
        );
    }

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = TileCache::new(1024 * 1024);
        let key = make_key(0, 0.0);
        cache.insert(key.clone(), make_tile_buf(10, 10));
        assert!(cache.get(&key).is_some());
    }

    #[test]
    fn test_cache_evicts_lru() {
        // Capacity for exactly two 10×10 tiles (10*10*4 = 400 bytes each).
        let mut cache = TileCache::new(800);
        let k1 = make_key(0, 0.0);
        let k2 = make_key(0, 100.0);
        let k3 = make_key(0, 200.0);
        cache.insert(k1.clone(), make_tile_buf(10, 10)); // 400 bytes
        cache.insert(k2.clone(), make_tile_buf(10, 10)); // 400 bytes – now at limit
        cache.insert(k3.clone(), make_tile_buf(10, 10)); // should evict k1
        assert!(cache.get(&k1).is_none(), "k1 should have been evicted");
        assert!(cache.get(&k2).is_some());
        assert!(cache.get(&k3).is_some());
    }

    #[test]
    fn test_tile_to_pixel_space() {
        let tile = TileRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let (origin, w, h) = tile.to_pixel_space(200.0, 2.0);
        assert_eq!(origin.x, 0);
        // y: (200 - 0 - 100) * 2 = 200
        assert_eq!(origin.y, 200);
        assert_eq!(w, 200);
        assert_eq!(h, 200);
    }
}
