//! Generic region (6.2) and generic refinement (6.3) decoding procedures —
//! port of pdf.js v3.11.174 `decodeBitmap` / `decodeBitmapTemplate0` /
//! `decodeRefinement` from `src/core/jbig2.js` (Apache 2.0).

use super::arith::ArithmeticDecoder;
use super::dec::{ctx, Bitmap, DecodingContext};
use crate::error::{PdfError, Result};

/// Adaptive-template pixel offset.
#[derive(Debug, Clone, Copy)]
pub struct AtPixel {
    /// X offset relative to the current pixel.
    pub x: i32,
    /// Y offset relative to the current pixel.
    pub y: i32,
}

/// 6.2.5 generic region coding templates (reference `CodingTemplates`).
pub static CODING_TEMPLATES: [&[AtPixel]; 4] = [
    &[
        AtPixel { x: -1, y: -2 },
        AtPixel { x: 0, y: -2 },
        AtPixel { x: 1, y: -2 },
        AtPixel { x: -2, y: -1 },
        AtPixel { x: -1, y: -1 },
        AtPixel { x: 0, y: -1 },
        AtPixel { x: 1, y: -1 },
        AtPixel { x: 2, y: -1 },
        AtPixel { x: -4, y: 0 },
        AtPixel { x: -3, y: 0 },
        AtPixel { x: -2, y: 0 },
        AtPixel { x: -1, y: 0 },
    ],
    &[
        AtPixel { x: -1, y: -2 },
        AtPixel { x: 0, y: -2 },
        AtPixel { x: 1, y: -2 },
        AtPixel { x: 2, y: -2 },
        AtPixel { x: -2, y: -1 },
        AtPixel { x: -1, y: -1 },
        AtPixel { x: 0, y: -1 },
        AtPixel { x: 1, y: -1 },
        AtPixel { x: 2, y: -1 },
        AtPixel { x: -3, y: 0 },
        AtPixel { x: -2, y: 0 },
        AtPixel { x: -1, y: 0 },
    ],
    &[
        AtPixel { x: -1, y: -2 },
        AtPixel { x: 0, y: -2 },
        AtPixel { x: 1, y: -2 },
        AtPixel { x: -2, y: -1 },
        AtPixel { x: -1, y: -1 },
        AtPixel { x: 0, y: -1 },
        AtPixel { x: 1, y: -1 },
        AtPixel { x: -2, y: 0 },
        AtPixel { x: -1, y: 0 },
    ],
    &[
        AtPixel { x: -3, y: -1 },
        AtPixel { x: -2, y: -1 },
        AtPixel { x: -1, y: -1 },
        AtPixel { x: 0, y: -1 },
        AtPixel { x: 1, y: -1 },
        AtPixel { x: -4, y: 0 },
        AtPixel { x: -3, y: 0 },
        AtPixel { x: -2, y: 0 },
        AtPixel { x: -1, y: 0 },
    ],
];

/// 6.3.2 generic refinement templates (reference `RefinementTemplates`):
/// `(coding pixels, reference pixels)` per template.
pub static REFINEMENT_TEMPLATES: [(&[AtPixel], &[AtPixel]); 2] = [
    (
        &[
            AtPixel { x: 0, y: -1 },
            AtPixel { x: 1, y: -1 },
            AtPixel { x: -1, y: 0 },
        ],
        &[
            AtPixel { x: 0, y: -1 },
            AtPixel { x: 1, y: -1 },
            AtPixel { x: -1, y: 0 },
            AtPixel { x: 0, y: 0 },
            AtPixel { x: 1, y: 0 },
            AtPixel { x: -1, y: 1 },
            AtPixel { x: 0, y: 1 },
            AtPixel { x: 1, y: 1 },
        ],
    ),
    (
        &[
            AtPixel { x: -1, y: -1 },
            AtPixel { x: 0, y: -1 },
            AtPixel { x: 1, y: -1 },
            AtPixel { x: -1, y: 0 },
        ],
        &[
            AtPixel { x: 0, y: -1 },
            AtPixel { x: -1, y: 0 },
            AtPixel { x: 0, y: 0 },
            AtPixel { x: 1, y: 0 },
            AtPixel { x: 0, y: 1 },
            AtPixel { x: 1, y: 1 },
        ],
    ),
];

/// 6.2.5.7 context bits reusable across scanlines per template
/// (reference `ReusedContexts`).
const REUSED_CONTEXTS: [u32; 4] = [0x9b25, 0x0795, 0x00e5, 0x0195];

/// 6.3.5.4 reusable context bits for refinement templates
/// (reference `RefinementReusedContexts`).
#[allow(dead_code)]
const REFINEMENT_REUSED_CONTEXTS: [u32; 2] = [0x0020, 0x0008];

/// Context-coding inputs for one generic-region decode: the MQ decoder, the
/// shared context cache and the raw window (for MMR). Mirrors the reference
/// `decodingContext` argument bundle.
pub struct GenericContext<'a> {
    /// MQ decoder over the region's coded bytes.
    pub decoder: ArithmeticDecoder<'a>,
    /// Shared adaptive-context cache.
    pub cache: super::dec::ContextCache,
    /// Raw coded-data window (used only for MMR-coded regions).
    pub data: &'a [u8],
    /// Window start (MMR).
    pub start: usize,
    /// Window end (MMR).
    pub end: usize,
}

impl<'a> GenericContext<'a> {
    /// Build from a [`DecodingContext`].
    pub fn new(dc: &DecodingContext<'a>) -> Self {
        GenericContext {
            decoder: dc.decoder(),
            cache: super::dec::ContextCache::default(),
            data: dc.data,
            start: dc.start,
            end: dc.end,
        }
    }
}

/// 6.2.5.7 optimised context-template-0 decode (reference
/// `decodeBitmapTemplate0`).
fn decode_bitmap_template0(
    width: u32,
    height: u32,
    decoder: &mut ArithmeticDecoder<'_>,
    contexts: &mut [u8],
) -> Bitmap {
    const OLD_PIXEL_MASK: u32 = 0x7bf7; // 01111 0111111 0111
    let w = width as usize;
    let zero_row = vec![0u8; w];
    let mut bitmap: Bitmap = Vec::with_capacity(height as usize);
    for i in 0..height {
        let mut row = vec![0u8; w];
        // Rows above the bitmap top are treated as all-zero (reference reads
        // `bitmap[negative]`, which is `undefined` → 0 in JS).
        let (row1, row2): (&[u8], &[u8]) = if i < 1 {
            (&zero_row, &zero_row)
        } else if i < 2 {
            (&bitmap[(i - 1) as usize], &zero_row)
        } else {
            (&bitmap[(i - 1) as usize], &bitmap[(i - 2) as usize])
        };
        // Fill contextLabel with pixels above/right of (X).
        let mut context_label: u32 = ((row2[0] as u32) << 13)
            | ((row2[1] as u32) << 12)
            | ((row2[2] as u32) << 11)
            | ((row1[0] as u32) << 7)
            | ((row1[1] as u32) << 6)
            | ((row1[2] as u32) << 5)
            | ((row1[3] as u32) << 4);
        for j in 0..w {
            let pixel = decoder.read_bit(contexts, context_label as usize);
            row[j] = pixel as u8;
            context_label = ((context_label & OLD_PIXEL_MASK) << 1)
                | if j + 3 < w {
                    (row2[j + 3] as u32) << 11
                } else {
                    0
                }
                | if j + 4 < w {
                    (row1[j + 4] as u32) << 4
                } else {
                    0
                }
                | pixel;
        }
        bitmap.push(row);
    }
    bitmap
}

/// 6.2 Generic region decoding procedure (reference `decodeBitmap`).
///
/// `at` holds the adaptive-template pixels for non-standard templates
/// (`template 0` requires 4, others 1). `decoder` and `cache` come from the
/// caller's [`GenericContext`]; `window` is the raw coded-data window (used
/// only when `mmr` is set).
#[allow(clippy::too_many_arguments)]
pub fn decode_bitmap(
    mmr: bool,
    width: u32,
    height: u32,
    template_index: u8,
    prediction: bool,
    at: &[AtPixel],
    decoder: &mut ArithmeticDecoder<'_>,
    cache: &mut super::dec::ContextCache,
    window: &[u8],
) -> Result<Bitmap> {
    if width == 0 || height == 0 {
        return Ok(Vec::new());
    }
    if mmr {
        // MMR is the same compression as CCITTFaxDecode /K -1. The window is
        // consumed whole (pdf.js reads exactly `height` rows; EndOfBlock is
        // false here — EOFB consumption happens for halftone planes only).
        let (bitmap, _consumed) = super::dec::decode_mmr_bitmap(window, width, height, false)?;
        return Ok(bitmap);
    }

    let contexts = cache.get_contexts(ctx::GB);

    // Fast path: standard template 0 without prediction.
    if template_index == 0
        && !prediction
        && at.len() == 4
        && at[0].x == 3
        && at[0].y == -1
        && at[1].x == -3
        && at[1].y == -1
        && at[2].x == 2
        && at[2].y == -2
        && at[3].x == -2
        && at[3].y == -2
    {
        return Ok(decode_bitmap_template0(width, height, decoder, contexts));
    }

    // Standard template + adaptive pixels, y-major then x-minor sorted
    // (non-standard but matches the reference for context reuse).
    let mut template: Vec<AtPixel> = CODING_TEMPLATES[template_index as usize].to_vec();
    template.extend_from_slice(at);
    template.sort_by(|a, b| a.y.cmp(&b.y).then(a.x.cmp(&b.x)));

    let template_len = template.len();
    let reuse_mask: u32 = template
        .iter()
        .enumerate()
        .filter(|(k, e)| {
            *k + 1 < template_len && e.y == template[k + 1].y && e.x == template[k + 1].x - 1
        })
        .map(|(k, _)| 1u32 << (template_len - 1 - k))
        .fold(0, |acc, b| acc | b);
    let changing: Vec<(i32, i32, u32)> = template
        .iter()
        .enumerate()
        .filter(|(k, _)| {
            !(*k + 1 < template_len
                && template[*k].y == template[*k + 1].y
                && template[*k].x == template[*k + 1].x - 1)
        })
        .map(|(k, e)| (e.x, e.y, 1u32 << (template_len - 1 - k)))
        .collect();

    let (min_x, max_x, min_y) = template
        .iter()
        .fold((i32::MAX, i32::MIN, i32::MAX), |(a, b, c), e| {
            (a.min(e.x), b.max(e.x), c.min(e.y))
        });
    let sbb_left = -min_x;
    let sbb_top = -min_y;
    let sbb_right = width as i32 - max_x;

    let pseudo_pixel_context = REUSED_CONTEXTS[template_index as usize];
    let w = width as usize;
    let mut bitmap: Bitmap = Vec::with_capacity(height as usize);
    let mut ltp = 0u32;
    let mut context_label: u32 = 0;

    for i in 0..height {
        if prediction {
            let sltp = decoder.read_bit(contexts, pseudo_pixel_context as usize);
            ltp ^= sltp;
            if ltp != 0 {
                // Duplicate the previous row (zeros on the first row).
                if i == 0 {
                    bitmap.push(vec![0u8; w]);
                } else {
                    bitmap.push(bitmap[i as usize - 1].clone());
                }
                continue;
            }
        }
        // Push the row before decoding: template entries at y = 0 read
        // already-decoded pixels of the current row (like the reference,
        // which pushes before the pixel loop).
        bitmap.push(vec![0u8; w]);
        for j in 0..w {
            let mut i0: i32;
            let mut j0: i32;
            if j as i32 >= sbb_left && (j as i32) < sbb_right && i as i32 >= sbb_top {
                context_label = (context_label << 1) & reuse_mask;
                for &(cx_off, cy_off, bit_mask) in &changing {
                    i0 = i as i32 + cy_off;
                    j0 = j as i32 + cx_off;
                    // Offsets are bounded by the sbb box (see reference).
                    if bitmap[i0 as usize][j0 as usize] != 0 {
                        context_label |= bit_mask;
                    }
                }
            } else {
                context_label = 0;
                // `shift` walks template_len-1 ..= 0 across the loop; i32 so
                // the final decrement doesn't underflow (JS ends at -1).
                let mut shift = template_len as i32 - 1;
                for e in &template {
                    j0 = j as i32 + e.x;
                    if j0 >= 0 && j0 < width as i32 {
                        i0 = i as i32 + e.y;
                        if i0 >= 0 && bitmap[i0 as usize][j0 as usize] != 0 {
                            context_label |= 1 << shift;
                        }
                    }
                    shift -= 1;
                }
            }
            let pixel = decoder.read_bit(contexts, context_label as usize);
            bitmap[i as usize][j] = pixel as u8;
        }
    }
    Ok(bitmap)
}

/// 6.3.2 Generic refinement region decoding (reference `decodeRefinement`).
///
/// `offset_x`/`offset_y` place the reference bitmap relative to the coded
/// one. LTP prediction (`prediction`) is rejected like the reference.
#[allow(clippy::too_many_arguments)]
pub fn decode_refinement(
    width: u32,
    height: u32,
    template_index: u8,
    reference_bitmap: &Bitmap,
    offset_x: i32,
    offset_y: i32,
    prediction: bool,
    at: &[AtPixel],
    decoder: &mut ArithmeticDecoder<'_>,
    cache: &mut super::dec::ContextCache,
) -> Result<Bitmap> {
    if width == 0 || height == 0 {
        return Ok(Vec::new());
    }
    let (coding_tpl, ref_tpl) = &REFINEMENT_TEMPLATES[template_index as usize];
    let mut coding: Vec<AtPixel> = coding_tpl.to_vec();
    let mut reference: Vec<AtPixel> = ref_tpl.to_vec();
    if template_index == 0 {
        if at.len() < 2 {
            return Err(PdfError::filter_error(0, "JBIG2: refinement AT missing"));
        }
        coding.push(at[0]);
        reference.push(at[1]);
    }

    let reference_width = reference_bitmap.first().map_or(0, Vec::len);
    let reference_height = reference_bitmap.len();

    let contexts = cache.get_contexts(ctx::GR);

    let w = width as usize;
    let mut bitmap: Bitmap = Vec::with_capacity(height as usize);
    let mut ltp = 0u32;
    for i in 0..height {
        if prediction {
            let pseudo = REFINEMENT_REUSED_CONTEXTS[template_index as usize];
            let sltp = decoder.read_bit(contexts, pseudo as usize);
            ltp ^= sltp;
            if ltp != 0 {
                return Err(PdfError::filter_error(
                    0,
                    "JBIG2: prediction is not supported",
                ));
            }
        }
        let mut row = vec![0u8; w];
        for (j, cell) in row.iter_mut().enumerate() {
            let mut context_label: u32 = 0;
            for e in &coding {
                let i0 = i as i32 + e.y;
                let j0 = j as i32 + e.x;
                if i0 < 0 || j0 < 0 || j0 >= width as i32 {
                    context_label <<= 1; // out of bound pixel
                } else {
                    context_label = (context_label << 1) | bitmap[i0 as usize][j0 as usize] as u32;
                }
            }
            for e in &reference {
                let i0 = i as i32 + e.y - offset_y;
                let j0 = j as i32 + e.x - offset_x;
                if i0 < 0 || i0 >= reference_height as i32 || j0 < 0 || j0 >= reference_width as i32
                {
                    context_label <<= 1; // out of bound pixel
                } else {
                    context_label =
                        (context_label << 1) | reference_bitmap[i0 as usize][j0 as usize] as u32;
                }
            }
            let pixel = decoder.read_bit(contexts, context_label as usize);
            *cell = pixel as u8;
        }
        bitmap.push(row);
    }
    Ok(bitmap)
}
