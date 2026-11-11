//! 6.6 Halftone region and pattern dictionary decoding — port of pdf.js
//! v3.11.174 `decodePatternDictionary` and `decodeHalftoneRegion` from
//! `src/core/jbig2.js` (Apache 2.0).

use super::dec::{log2, Bitmap, DecodeCtx};
use super::generic::{decode_bitmap, AtPixel};
use crate::error::{PdfError, Result};

/// 6.7.6 Pattern dictionary (reference `decodePatternDictionary`): returns
/// the decoded pattern bitmaps, indexed by pattern number.
///
/// `template` is the generic-region template index used for the collective
/// bitmap; `decoding_context` wraps the segment's coded data.
pub fn decode_pattern_dictionary(
    mmr: bool,
    pattern_width: u32,
    pattern_height: u32,
    max_pattern_index: u32,
    template: u8,
    cx: &mut DecodeCtx<'_>,
) -> Result<Vec<Bitmap>> {
    let mut at: Vec<AtPixel> = Vec::new();
    if !mmr {
        at.push(AtPixel {
            x: -(pattern_width as i32),
            y: 0,
        });
        if template == 0 {
            at.push(AtPixel { x: -3, y: -1 });
            at.push(AtPixel { x: 2, y: -2 });
            at.push(AtPixel { x: -2, y: -2 });
        }
    }
    let collective_width = (max_pattern_index + 1) * pattern_width;
    let collective_bitmap = decode_bitmap(
        mmr,
        collective_width,
        pattern_height,
        template,
        false,
        &at,
        &mut cx.decoder,
        &mut cx.cache,
        cx.data,
    )?;

    // Divide collective bitmap into patterns (reference uses subarray views;
    // the port copies the row slices).
    let mut patterns: Vec<Bitmap> = Vec::with_capacity(max_pattern_index as usize + 1);
    for i in 0..=max_pattern_index {
        let x_min = (pattern_width * i) as usize;
        let x_max = x_min + pattern_width as usize;
        let mut pattern_bitmap: Bitmap = Vec::with_capacity(pattern_height as usize);
        for row in &collective_bitmap {
            pattern_bitmap.push(row[x_min..x_max.min(row.len())].to_vec());
        }
        patterns.push(pattern_bitmap);
    }
    Ok(patterns)
}

/// 6.6 Halftone region decoding (reference `decodeHalftoneRegion`).
///
/// Grid positions are computed in 8.8 fixed point: `x = (offsetX +
/// mg*vectorY + ng*vectorX) >> 8` (the swapped vector components match the
/// reference exactly).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
pub fn decode_halftone_region(
    mmr: bool,
    patterns: &[Bitmap],
    template: u8,
    region_width: u32,
    region_height: u32,
    default_pixel_value: u8,
    enable_skip: bool,
    combination_operator: u8,
    grid_width: u32,
    grid_height: u32,
    grid_offset_x: i32,
    grid_offset_y: i32,
    grid_vector_x: i32,
    grid_vector_y: i32,
    cx: &mut DecodeCtx<'_>,
) -> Result<Bitmap> {
    if enable_skip {
        return Err(PdfError::filter_error(0, "JBIG2: skip is not supported"));
    }
    if combination_operator != 0 {
        return Err(PdfError::filter_error(
            0,
            format!(
                "JBIG2: operator \"{combination_operator}\" is not supported in halftone region"
            ),
        ));
    }

    // Prepare bitmap.
    let mut region_bitmap: Bitmap = Vec::with_capacity(region_height as usize);
    for _ in 0..region_height {
        region_bitmap.push(vec![default_pixel_value; region_width as usize]);
    }

    let number_of_patterns = patterns.len();
    let pattern0 = patterns
        .first()
        .ok_or_else(|| PdfError::filter_error(0, "JBIG2: halftone region without patterns"))?;
    let pattern_width = pattern0.first().map_or(0, Vec::len);
    let pattern_height = pattern0.len();
    let bits_per_value = log2(number_of_patterns);

    let mut at: Vec<AtPixel> = Vec::new();
    if !mmr {
        at.push(AtPixel {
            x: if template <= 1 { 3 } else { 2 },
            y: -1,
        });
        if template == 0 {
            at.push(AtPixel { x: -3, y: -1 });
            at.push(AtPixel { x: 2, y: -2 });
            at.push(AtPixel { x: -2, y: -2 });
        }
    }

    // Annex C. Gray-scale Image Decoding Procedure. MMR bit planes live in
    // one continuous stream; only EOFB codes end each bitmap, so EOFBs must
    // be consumed — handled by `end_of_block = true` below. Planes decode
    // most-significant first (`grayScaleBitPlanes[i] = bitmap`, i
    // descending).
    let mut planes: Vec<Option<Bitmap>> = (0..bits_per_value).map(|_| None).collect();
    let mut mmr_position = 0usize;
    for i in (0..bits_per_value).rev() {
        let bitmap = if mmr {
            if mmr_position > cx.data.len() {
                return Err(PdfError::filter_error(
                    mmr_position,
                    "JBIG2: halftone MMR stream past end",
                ));
            }
            let window = &cx.data[mmr_position..];
            let (bm, consumed) =
                super::dec::decode_mmr_bitmap(window, grid_width, grid_height, true)?;
            mmr_position += consumed;
            bm
        } else {
            decode_bitmap(
                false,
                grid_width,
                grid_height,
                template,
                false,
                &at,
                &mut cx.decoder,
                &mut cx.cache,
                &[],
            )?
        };
        planes[i as usize] = Some(bitmap);
    }
    let gray_scale_bit_planes: Vec<Bitmap> = planes
        .into_iter()
        .map(|p| p.ok_or_else(|| PdfError::filter_error(0, "JBIG2: missing gray-scale plane")))
        .collect::<Result<_>>()?;

    // 6.6.5.2 Rendering the patterns.
    for mg in 0..grid_height {
        for ng in 0..grid_width {
            let mut bit = 0u32;
            let mut pattern_index = 0u32;
            for j in (0..bits_per_value).rev() {
                let plane = &gray_scale_bit_planes[j as usize];
                let pixel = plane
                    .get(mg as usize)
                    .and_then(|row| row.get(ng as usize))
                    .copied()
                    .unwrap_or(0) as u32;
                bit ^= pixel; // Gray decoding
                pattern_index |= bit << j;
            }
            if pattern_index as usize >= patterns.len() {
                continue;
            }
            let pattern_bitmap = &patterns[pattern_index as usize];
            // Fixed-point grid math (reference uses JS `>> 8` on doubles;
            // the operands are integers, so i64 arithmetic matches).
            let x = ((grid_offset_x as i64
                + mg as i64 * grid_vector_y as i64
                + ng as i64 * grid_vector_x as i64)
                >> 8) as i32;
            let y = ((grid_offset_y as i64 + mg as i64 * grid_vector_x as i64
                - ng as i64 * grid_vector_y as i64)
                >> 8) as i32;
            // Draw patternBitmap at (x, y).
            if x >= 0
                && (x + pattern_width as i32) <= region_width as i32
                && y >= 0
                && (y + pattern_height as i32) <= region_height as i32
            {
                for i in 0..pattern_height {
                    let region_row = &mut region_bitmap[(y + i as i32) as usize];
                    let pattern_row = &pattern_bitmap[i];
                    for j in 0..pattern_width {
                        region_row[(x + j as i32) as usize] |= pattern_row[j];
                    }
                }
            } else {
                for (i, pattern_row) in pattern_bitmap.iter().enumerate() {
                    let region_y = y + i as i32;
                    if region_y < 0 || region_y >= region_height as i32 {
                        continue;
                    }
                    for j in 0..pattern_width {
                        let region_x = x + j as i32;
                        if region_x >= 0 && region_x < region_width as i32 {
                            region_bitmap[region_y as usize][region_x as usize] |=
                                pattern_row.get(j).copied().unwrap_or(0);
                        }
                    }
                }
            }
        }
    }
    Ok(region_bitmap)
}
