//! Image stream decoding for rendering.
//!
//! Handles inline images and XObject images from PDF content streams.
//! Supported input formats: raw pixel data (after FlateDecode / no filter),
//! DCTDecode (JPEG) via `zune-jpeg`.  Other filters produce a gray placeholder.

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfObject};

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
        Some("JPXDecode") => decode_jpx(raw),
        _ => decode_raw(raw, width, height, color_space, bits_per_component),
    }
}

/// Decode a JPEG 2000 stream (`/JPXDecode`) into RGBA. The codestream's own
/// SIZ header supplies the dimensions and component count, so the PDF
/// dictionary's width/height are only used as a sanity check.
fn decode_jpx(raw: &[u8]) -> Result<RgbaImage> {
    let img = crate::parser::jpx::decode(raw)?;
    let (w, h, n, interleaved) = img.interleaved_u8();
    if n >= 4 {
        log::warn!("JPX image with {n} components; using first 4 (CMYK order assumed)");
    }
    if w == 0 || h == 0 {
        return Ok(RgbaImage {
            data: Vec::new(),
            width: 0,
            height: 0,
        });
    }
    Ok(RgbaImage {
        data: expand_to_rgba(&interleaved, w, h, n),
        width: w,
        height: h,
    })
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
// Hard /Mask (stencil + chroma-key) — ISO 32000-1 §8.9.6.4
// ---------------------------------------------------------------------------

fn obj_int(o: &PdfObject) -> Option<i64> {
    match o {
        PdfObject::Integer(n) => Some(*n),
        PdfObject::Real(r) => Some(*r as i64),
        _ => None,
    }
}

fn dict_int(dict: &PdfDict, key: &str) -> Option<i64> {
    dict.get(key).and_then(obj_int)
}

/// Decode a hard `/Mask` stream — an image XObject used as the `/Mask` entry
/// of another image — into a single-channel alpha vector (255 = base image
/// visible, 0 = hidden) plus the mask's encoded dimensions.
///
/// Per §8.9.6.4 a mask sample value of 0 marks areas that shall be painted,
/// so the alpha is the inverted sample (`alpha = 255 - sample`); a
/// `/Decode [1 0]` array reverses the mapping. Supported encodings: 1-bit
/// packed stencil masks and 8-bit gray masks.
///
/// `raw` is the fully decoded (defiltered, decrypted) mask sample data.
pub fn decode_hard_mask(raw: &[u8], dict: &PdfDict) -> Option<(Vec<u8>, u32, u32)> {
    let w = dict_int(dict, "Width").unwrap_or(0).max(0) as u32;
    let h = dict_int(dict, "Height").unwrap_or(0).max(0) as u32;
    if w == 0 || h == 0 {
        log::warn!("/Mask stream has zero dimensions; ignoring");
        return None;
    }
    let bpc = dict_int(dict, "BitsPerComponent").unwrap_or(1) as u8;
    // /Decode [1 0] swaps the sample mapping (sample 1 → paint).
    let inverted = match dict.get("Decode") {
        Some(PdfObject::Array(arr)) if arr.len() >= 2 => match &arr[0] {
            PdfObject::Integer(n) => *n != 0,
            PdfObject::Real(r) => *r != 0.0,
            _ => false,
        },
        _ => false,
    };
    let n = (w as usize) * (h as usize);
    let samples: Vec<u8> = match bpc {
        1 => unpack_1bit_samples(raw, w, h),
        8 => {
            let mut s = vec![0u8; n];
            let copy = raw.len().min(n);
            s[..copy].copy_from_slice(&raw[..copy]);
            s
        }
        other => {
            log::warn!("/Mask with {} bpc is unsupported; ignoring", other);
            return None;
        }
    };
    let alpha: Vec<u8> = if inverted {
        samples
    } else {
        samples.iter().map(|&v| 255 - v).collect()
    };
    Some((alpha, w, h))
}

/// Expand a 1-bit packed mask stream into 0/255 sample values (MSB first,
/// rows padded to byte boundaries — §8.9.6.2). Missing bytes decode as 0.
fn unpack_1bit_samples(raw: &[u8], w: u32, h: u32) -> Vec<u8> {
    let row_bytes = (w as usize).div_ceil(8);
    let mut out = vec![0u8; (w as usize) * (h as usize)];
    for row in 0..h as usize {
        for col in 0..w as usize {
            let byte_idx = row * row_bytes + col / 8;
            let bit = if byte_idx < raw.len() {
                (raw[byte_idx] >> (7 - (col % 8))) & 1
            } else {
                0
            };
            out[row * w as usize + col] = if bit != 0 { 255 } else { 0 };
        }
    }
    out
}

/// Chroma-key `/Mask` array form (ISO 32000-1 §8.9.6.4): each `[min max]`
/// pair gives an inclusive range for one colour component; a pixel whose
/// every component sample falls inside its range is masked out (hidden).
///
/// `raw` holds 8-bit samples in `channels`-component interleaved order at
/// `w × h`. Returns the alpha vector (255 = visible) or `None` for
/// malformed ranges. Values are compared against the decoded component
/// samples (after indexed-palette expansion when applicable).
pub fn decode_chroma_key(
    raw: &[u8],
    w: u32,
    h: u32,
    channels: usize,
    ranges: &[PdfObject],
) -> Option<Vec<u8>> {
    if ranges.is_empty() || !ranges.len().is_multiple_of(2) {
        log::warn!(
            "/Mask array length {} is not a set of [min max] pairs",
            ranges.len()
        );
        return None;
    }
    let pairs = ranges.len() / 2;
    if pairs > channels {
        log::warn!(
            "/Mask array has {} ranges for {} components",
            pairs,
            channels
        );
        return None;
    }
    let mut lo_hi = Vec::with_capacity(pairs);
    for pair in ranges.chunks(2) {
        let lo = obj_int(&pair[0])?;
        let hi = obj_int(&pair[1])?;
        lo_hi.push((lo.min(hi), lo.max(hi)));
    }
    let npix = (w as usize) * (h as usize);
    let mut alpha = vec![255u8; npix];
    for (px, a) in alpha.iter_mut().enumerate() {
        let base = px * channels;
        let mut hide = true;
        for (c, &(lo, hi)) in lo_hi.iter().enumerate() {
            let s = raw.get(base + c).copied().unwrap_or(0) as i64;
            if s < lo || s > hi {
                hide = false;
                break;
            }
        }
        if hide {
            *a = 0;
        }
    }
    Some(alpha)
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

    #[test]
    fn unpack_1bit_samples_expands_msb_first_with_row_padding() {
        // 3 px wide (row_bytes = 1): bits 1,0,1 in row 0.
        let raw = [0b1010_0000u8];
        let out = unpack_1bit_samples(&raw, 3, 1);
        assert_eq!(out, vec![255, 0, 255]);
    }

    #[test]
    fn unpack_1bit_samples_missing_bytes_decode_as_zero() {
        let out = unpack_1bit_samples(&[], 2, 2);
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn hard_mask_stencil_inverts_sample_per_spec() {
        // §8.9.6.4: mask sample 0 → paint (alpha 255), 1 → hide (alpha 0).
        let mut dict = PdfDict::new();
        dict.insert("Width".to_string(), PdfObject::Integer(2));
        dict.insert("Height".to_string(), PdfObject::Integer(1));
        dict.insert("BitsPerComponent".to_string(), PdfObject::Integer(1));
        // bit 1 then bit 0 (MSB first)
        let raw = [0b1000_0000u8];
        let (alpha, w, h) = decode_hard_mask(&raw, &dict).expect("decode succeeds");
        assert_eq!((w, h), (2, 1));
        assert_eq!(alpha, vec![0, 255]);
    }

    #[test]
    fn hard_mask_decode_10_reverses_mapping() {
        let mut dict = PdfDict::new();
        dict.insert("Width".to_string(), PdfObject::Integer(2));
        dict.insert("Height".to_string(), PdfObject::Integer(1));
        dict.insert("BitsPerComponent".to_string(), PdfObject::Integer(1));
        dict.insert(
            "Decode".to_string(),
            PdfObject::Array(vec![PdfObject::Integer(1), PdfObject::Integer(0)]),
        );
        let raw = [0b1000_0000u8];
        let (alpha, _, _) = decode_hard_mask(&raw, &dict).expect("decode succeeds");
        assert_eq!(alpha, vec![255, 0]);
    }

    #[test]
    fn hard_mask_8bit_gray() {
        let mut dict = PdfDict::new();
        dict.insert("Width".to_string(), PdfObject::Integer(2));
        dict.insert("Height".to_string(), PdfObject::Integer(1));
        dict.insert("BitsPerComponent".to_string(), PdfObject::Integer(8));
        let (alpha, _, _) = decode_hard_mask(&[0u8, 255], &dict).expect("decode succeeds");
        assert_eq!(alpha, vec![255, 0]);
    }

    #[test]
    fn hard_mask_zero_dimensions_rejected() {
        let dict = PdfDict::new();
        assert!(decode_hard_mask(&[], &dict).is_none());
    }

    #[test]
    fn hard_mask_unsupported_bpc_rejected() {
        let mut dict = PdfDict::new();
        dict.insert("Width".to_string(), PdfObject::Integer(2));
        dict.insert("Height".to_string(), PdfObject::Integer(1));
        dict.insert("BitsPerComponent".to_string(), PdfObject::Integer(4));
        assert!(decode_hard_mask(&[0x12], &dict).is_none());
    }

    #[test]
    fn chroma_key_hides_pixels_inside_all_ranges() {
        // RGB samples: pure red, pure green, pure red.
        let raw = [255u8, 0, 0, 0, 255, 0, 255, 0, 0];
        let ranges = [
            PdfObject::Integer(250),
            PdfObject::Integer(255),
            PdfObject::Integer(0),
            PdfObject::Integer(5),
            PdfObject::Integer(0),
            PdfObject::Integer(5),
        ];
        let alpha = decode_chroma_key(&raw, 3, 1, 3, &ranges).expect("decode succeeds");
        // Red pixels fall inside all three ranges → hidden; green survives.
        assert_eq!(alpha, vec![0, 255, 0]);
    }

    #[test]
    fn chroma_key_partial_range_keeps_pixel() {
        // Gray samples where only the first component is ranged.
        let raw = [10u8, 200];
        let ranges = [PdfObject::Integer(0), PdfObject::Integer(10)];
        let alpha = decode_chroma_key(&raw, 2, 1, 1, &ranges).expect("decode succeeds");
        assert_eq!(alpha, vec![0, 255]);
    }

    #[test]
    fn chroma_key_odd_length_rejected() {
        let raw = [0u8, 0, 0];
        let ranges = [PdfObject::Integer(0)];
        assert!(decode_chroma_key(&raw, 1, 1, 3, &ranges).is_none());
    }

    #[test]
    fn chroma_key_more_ranges_than_channels_rejected() {
        let raw = [0u8];
        let ranges = [
            PdfObject::Integer(0),
            PdfObject::Integer(255),
            PdfObject::Integer(0),
            PdfObject::Integer(255),
        ];
        assert!(decode_chroma_key(&raw, 1, 1, 1, &ranges).is_none());
    }
}
