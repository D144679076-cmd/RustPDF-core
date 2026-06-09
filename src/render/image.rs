//! Image stream decoding for rendering.
//!
//! Handles inline images and XObject images from PDF content streams.
//! Supported input formats: raw pixel data (after FlateDecode / no filter),
//! DCTDecode (JPEG) via `zune-jpeg`.  Other filters produce a gray placeholder.

use crate::error::{PdfError, Result};

/// Decoded image ready for blitting into a `PixmapBuffer`.
pub struct RgbaImage {
    /// Row-major RGBA pixel data.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode an image stream into RGBA pixels.
///
/// `raw` is the already-decoded byte stream (after any compression filter has
/// been applied upstream).  `filter_hint` is the original PDF filter name
/// *before* decoding — `None` or `Some("FlateDecode")` means raw pixels are
/// expected; `Some("DCTDecode")` triggers JPEG decoding.
///
/// `color_space` is one of `"DeviceGray"`, `"DeviceRGB"`, `"DeviceCMYK"`.
pub fn decode_image(
    raw: &[u8],
    filter_hint: Option<&str>,
    width: u32,
    height: u32,
    color_space: &str,
    bits_per_component: u8,
) -> Result<RgbaImage> {
    match filter_hint {
        Some("DCTDecode") => decode_jpeg(raw, width, height),
        _ => decode_raw(raw, width, height, color_space, bits_per_component),
    }
}

// ---------------------------------------------------------------------------
// Cross-render decoded-image cache
// ---------------------------------------------------------------------------

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const IMG_CACHE_MAX_ENTRIES: usize = 64;
const IMG_CACHE_MAX_BYTES: usize = 192 * 1024 * 1024;

/// Tiny content-addressed LRU of decoded images.
struct ImageCache {
    map: HashMap<u64, (Rc<RgbaImage>, u64)>,
    tick: u64,
    bytes: usize,
}

impl ImageCache {
    fn new() -> Self {
        ImageCache {
            map: HashMap::new(),
            tick: 0,
            bytes: 0,
        }
    }

    fn get(&mut self, key: u64) -> Option<Rc<RgbaImage>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(&key).map(|e| {
            e.1 = t;
            Rc::clone(&e.0)
        })
    }

    fn insert(&mut self, key: u64, img: Rc<RgbaImage>) {
        let sz = img.data.len();
        if sz > IMG_CACHE_MAX_BYTES {
            return; // never cache a single image larger than the whole budget
        }
        self.tick += 1;
        while !self.map.is_empty()
            && (self.map.len() >= IMG_CACHE_MAX_ENTRIES || self.bytes + sz > IMG_CACHE_MAX_BYTES)
        {
            // Evict the least-recently-used entry.
            let lru = self
                .map
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| *k);
            match lru {
                Some(k) => {
                    if let Some((old, _)) = self.map.remove(&k) {
                        self.bytes = self.bytes.saturating_sub(old.data.len());
                    }
                }
                None => break,
            }
        }
        self.bytes += sz;
        self.map.insert(key, (img, self.tick));
    }
}

thread_local! {
    static IMAGE_CACHE: RefCell<ImageCache> = RefCell::new(ImageCache::new());
}

fn image_key(
    raw: &[u8],
    filter_hint: Option<&str>,
    width: u32,
    height: u32,
    color_space: &str,
    bpc: u8,
) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_usize(raw.len());
    h.write(raw);
    h.write(filter_hint.unwrap_or("").as_bytes());
    h.write_u32(width);
    h.write_u32(height);
    h.write(color_space.as_bytes());
    h.write_u8(bpc);
    h.finish()
}

/// Like [`decode_image`] but memoizes the decoded result across renders in a
/// thread-local LRU keyed by the *content* of `raw` plus the decode params.
///
/// Because the key is the image content, it survives the per-commit document
/// reparse (same image bytes → same key), so each distinct image is decoded once
/// and the `Rc` is shared on every later render/scroll/commit — eliminating the
/// dominant per-render cost on image-heavy pages. Output is byte-identical to
/// [`decode_image`].
pub fn decode_image_cached(
    raw: &[u8],
    filter_hint: Option<&str>,
    width: u32,
    height: u32,
    color_space: &str,
    bits_per_component: u8,
) -> Result<Rc<RgbaImage>> {
    let key = image_key(
        raw,
        filter_hint,
        width,
        height,
        color_space,
        bits_per_component,
    );
    if let Some(img) = IMAGE_CACHE.with(|c| c.borrow_mut().get(key)) {
        return Ok(img);
    }
    let img = Rc::new(decode_image(
        raw,
        filter_hint,
        width,
        height,
        color_space,
        bits_per_component,
    )?);
    IMAGE_CACHE.with(|c| c.borrow_mut().insert(key, Rc::clone(&img)));
    Ok(img)
}

// ---------------------------------------------------------------------------
// JPEG via zune-jpeg
// ---------------------------------------------------------------------------

fn decode_jpeg(raw: &[u8], expected_w: u32, expected_h: u32) -> Result<RgbaImage> {
    let mut decoder = zune_jpeg::JpegDecoder::new(raw);

    let pixels = decoder
        .decode()
        .map_err(|e| PdfError::filter_error(0, format!("JPEG decode error: {:?}", e)))?;

    let info = decoder.info().ok_or_else(|| {
        PdfError::filter_error(0, "JPEG missing image info after decode".to_string())
    })?;

    let w = info.width as u32;
    let h = info.height as u32;

    // Log a warning if actual dimensions differ from what the PDF dict says.
    if w != expected_w || h != expected_h {
        log::warn!(
            "JPEG dimensions {}×{} differ from PDF dict {}×{}",
            w,
            h,
            expected_w,
            expected_h
        );
    }

    // Determine channel count from pixel buffer length.
    let channels = if w > 0 && h > 0 {
        pixels.len() / (w as usize * h as usize)
    } else {
        3
    };
    let rgba = expand_to_rgba(&pixels, w, h, channels);
    Ok(RgbaImage {
        data: rgba,
        width: w,
        height: h,
    })
}

// ---------------------------------------------------------------------------
// Raw pixel data
// ---------------------------------------------------------------------------

fn decode_raw(
    raw: &[u8],
    width: u32,
    height: u32,
    color_space: &str,
    bits_per_component: u8,
) -> Result<RgbaImage> {
    if width == 0 || height == 0 {
        return Ok(RgbaImage {
            data: Vec::new(),
            width: 0,
            height: 0,
        });
    }

    // Only 8 bpc is supported for now; higher bit depths are down-sampled.
    let bytes_per_sample = if bits_per_component <= 8 { 1usize } else { 2 };

    let channels: usize = match color_space {
        "DeviceGray" | "CalGray" => 1,
        "DeviceRGB" | "CalRGB" | "sRGB" => 3,
        "DeviceCMYK" => 4,
        _ => {
            log::warn!("unsupported color space '{}', treating as RGB", color_space);
            3
        }
    };

    let stride = width as usize * channels * bytes_per_sample;
    let expected = stride * height as usize;

    if raw.len() < expected {
        log::warn!(
            "image raw data too short: {} < {} ({}×{}×{})",
            raw.len(),
            expected,
            width,
            height,
            channels
        );
    }

    // Normalise to 8-bit samples if 16-bit.
    let samples: Vec<u8> = if bytes_per_sample == 2 {
        raw.chunks(2)
            .map(|pair| if pair.len() == 2 { pair[0] } else { 0 })
            .collect()
    } else {
        raw.to_vec()
    };

    let rgba = expand_to_rgba(&samples, width, height, channels);
    Ok(RgbaImage {
        data: rgba,
        width,
        height,
    })
}

// ---------------------------------------------------------------------------
// Channel → RGBA expansion
// ---------------------------------------------------------------------------

fn expand_to_rgba(pixels: &[u8], width: u32, height: u32, channels: usize) -> Vec<u8> {
    let n = (width * height) as usize;
    let mut out = Vec::with_capacity(n * 4);

    match channels {
        1 => {
            // Grayscale → RGBA
            for i in 0..n {
                let g = pixels.get(i).copied().unwrap_or(0);
                out.extend_from_slice(&[g, g, g, 255]);
            }
        }
        3 => {
            // RGB → RGBA
            for i in 0..n {
                let base = i * 3;
                let r = pixels.get(base).copied().unwrap_or(0);
                let g = pixels.get(base + 1).copied().unwrap_or(0);
                let b = pixels.get(base + 2).copied().unwrap_or(0);
                out.extend_from_slice(&[r, g, b, 255]);
            }
        }
        4 => {
            // CMYK → RGBA
            for i in 0..n {
                let base = i * 4;
                let c = pixels.get(base).copied().unwrap_or(0) as f32 / 255.0;
                let m = pixels.get(base + 1).copied().unwrap_or(0) as f32 / 255.0;
                let y = pixels.get(base + 2).copied().unwrap_or(0) as f32 / 255.0;
                let k = pixels.get(base + 3).copied().unwrap_or(0) as f32 / 255.0;
                let r = ((1.0 - c) * (1.0 - k) * 255.0) as u8;
                let g = ((1.0 - m) * (1.0 - k) * 255.0) as u8;
                let b = ((1.0 - y) * (1.0 - k) * 255.0) as u8;
                out.extend_from_slice(&[r, g, b, 255]);
            }
        }
        _ => {
            // Unknown: fill gray placeholder
            log::warn!("unknown channel count {}, using gray placeholder", channels);
            for _ in 0..n {
                out.extend_from_slice(&[128, 128, 128, 255]);
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// SMask application
// ---------------------------------------------------------------------------

/// Nearest-neighbour scale a single-channel (gray) mask from `(sw × sh)` to `(dw × dh)`.
///
/// Used to reconcile an SMask whose encoded dimensions differ from the image it
/// is being applied to, rather than silently discarding the mask entirely.
pub fn scale_gray_mask(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh];
    for dy in 0..dh {
        let sy = (dy * sh) / dh;
        for dx in 0..dw {
            let sx = (dx * sw) / dw;
            out[dy * dw + dx] = src.get(sy * sw + sx).copied().unwrap_or(255);
        }
    }
    out
}

/// Apply a decoded grayscale SMask to an RGBA pixel buffer in-place.
///
/// `smask` must be a single-channel (gray) byte slice with one byte per pixel.
/// The gray value becomes the alpha of the corresponding RGBA pixel.
/// `rgba` must have length `smask.len() * 4`.
pub fn apply_smask(rgba: &mut [u8], smask: &[u8]) {
    for (i, &gray) in smask.iter().enumerate() {
        if let Some(a) = rgba.get_mut(i * 4 + 3) {
            *a = gray;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_gray_1x1() {
        let raw = [0x80u8]; // mid-gray
        let img = decode_image(&raw, None, 1, 1, "DeviceGray", 8).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 1);
        assert_eq!(&img.data, &[0x80, 0x80, 0x80, 0xFF]);
    }

    #[test]
    fn image_cache_hit_reuses_rc_and_is_byte_identical() {
        // Use bytes unlikely to collide with other tests so the cache state is clean.
        let raw = [11u8, 22, 33, 44, 55, 66];
        let a = decode_image_cached(&raw, None, 2, 1, "DeviceRGB", 8).unwrap();
        let b = decode_image_cached(&raw, None, 2, 1, "DeviceRGB", 8).unwrap();
        // Same key → the exact same cached allocation (proves no re-decode).
        assert!(Rc::ptr_eq(&a, &b), "repeated key must return the cached Rc");
        // ...and equals an uncached decode (pure memoization, no output change).
        let direct = decode_image(&raw, None, 2, 1, "DeviceRGB", 8).unwrap();
        assert_eq!(a.data, direct.data);
        // A different param (colorspace) is a different key → separate entry.
        let c = decode_image_cached(&raw, None, 2, 1, "DeviceGray", 8).unwrap();
        assert!(!Rc::ptr_eq(&a, &c), "different params must not collide");
    }

    #[test]
    fn test_decode_rgb_2x1() {
        let raw = [255u8, 0, 0, 0, 255, 0]; // red, green
        let img = decode_image(&raw, None, 2, 1, "DeviceRGB", 8).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(&img.data[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img.data[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn test_decode_cmyk_white() {
        let raw = [0u8, 0, 0, 0]; // CMYK white
        let img = decode_image(&raw, None, 1, 1, "DeviceCMYK", 8).unwrap();
        assert_eq!(&img.data[0..3], &[255, 255, 255]);
    }

    #[test]
    fn test_decode_cmyk_black() {
        let raw = [0u8, 0, 0, 255]; // CMYK black (K=1)
        let img = decode_image(&raw, None, 1, 1, "DeviceCMYK", 8).unwrap();
        assert_eq!(&img.data[0..3], &[0, 0, 0]);
    }

    #[test]
    fn scale_gray_mask_2x2_to_4x4_nearest_neighbour() {
        // 2×2 gray mask: top-left=0, top-right=64, bottom-left=128, bottom-right=255
        let src = [0u8, 64, 128, 255];
        let dst = scale_gray_mask(&src, 2, 2, 4, 4);
        assert_eq!(dst.len(), 16);
        // Nearest-neighbour: each src pixel maps to a 2×2 block.
        // Row 0 (sy=0): [0, 0, 64, 64]
        assert_eq!(&dst[0..4], &[0, 0, 64, 64]);
        // Row 1 (sy=0): same
        assert_eq!(&dst[4..8], &[0, 0, 64, 64]);
        // Row 2 (sy=1): [128, 128, 255, 255]
        assert_eq!(&dst[8..12], &[128, 128, 255, 255]);
        // Row 3 (sy=1): same
        assert_eq!(&dst[12..16], &[128, 128, 255, 255]);
    }

    #[test]
    fn scale_gray_mask_identity_when_same_size() {
        let src = [10u8, 20, 30, 40];
        let dst = scale_gray_mask(&src, 2, 2, 2, 2);
        assert_eq!(dst, src);
    }

    #[test]
    fn test_apply_smask_full_alpha() {
        let mut rgba = vec![255u8, 0, 0, 255]; // opaque red pixel
        apply_smask(&mut rgba, &[255]);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn test_apply_smask_zero_alpha() {
        let mut rgba = vec![255u8, 0, 0, 255]; // opaque red pixel
        apply_smask(&mut rgba, &[0]);
        assert_eq!(rgba[3], 0); // fully transparent
    }

    #[test]
    fn test_apply_smask_partial() {
        let mut rgba = vec![255u8, 0, 0, 255];
        apply_smask(&mut rgba, &[128]);
        assert_eq!(rgba[3], 128);
        // RGB unchanged
        assert_eq!(&rgba[0..3], &[255, 0, 0]);
    }
}
