//! 6.5 Symbol dictionary and 6.4 text region decoding — port of pdf.js
//! v3.11.174 `decodeSymbolDictionary`, `decodeTextRegion`,
//! `getSymbolDictionaryHuffmanTables` and `getTextRegionHuffmanTables`
//! from `src/core/jbig2.js` (Apache 2.0).

use super::dec::{
    ctx, decode_iaid, decode_integer, log2, read_uncompressed_bitmap, Bitmap, DecodeCtx,
};
use super::generic::{decode_bitmap, decode_refinement, AtPixel};
use super::huffman::{get_custom_huffman_table, HuffmanTable};
use crate::error::{PdfError, Result};
use std::collections::HashMap;

/// Huffman tables selected for a symbol dictionary segment
/// (reference `getSymbolDictionaryHuffmanTables` result).
pub struct SymbolDictHuffmanTables {
    pub table_delta_height: HuffmanTable,
    pub table_delta_width: HuffmanTable,
    pub table_bitmap_size: HuffmanTable,
    pub table_aggregate_instances: HuffmanTable,
}

/// Huffman tables selected for a text region segment
/// (reference `getTextRegionHuffmanTables` result).
pub struct TextRegionHuffmanTables {
    pub symbol_id_table: HuffmanTable,
    pub table_first_s: HuffmanTable,
    pub table_delta_s: HuffmanTable,
    pub table_delta_t: HuffmanTable,
}

/// 7.4.2.1.6 Symbol dictionary Huffman table selection.
///
/// # Errors
/// Invalid selectors or a missing custom table among `referred_to`.
pub fn get_symbol_dictionary_huffman_tables(
    huffman_dh: u8,
    huffman_dw: u8,
    bitmap_size_selector: bool,
    aggregation_instances_selector: bool,
    referred_to: &[u32],
    custom_tables: &HashMap<u32, HuffmanTable>,
) -> Result<SymbolDictHuffmanTables> {
    let mut custom_index = 0usize;
    let table_delta_height = match huffman_dh {
        0 | 1 => HuffmanTable::standard(huffman_dh as usize + 4)?,
        3 => {
            let t = get_custom_huffman_table(custom_index, referred_to, custom_tables)?;
            custom_index += 1;
            t.clone()
        }
        _ => {
            return Err(PdfError::filter_error(
                0,
                "JBIG2: invalid Huffman DH selector",
            ))
        }
    };
    let table_delta_width = match huffman_dw {
        0 | 1 => HuffmanTable::standard(huffman_dw as usize + 2)?,
        3 => {
            let t = get_custom_huffman_table(custom_index, referred_to, custom_tables)?;
            custom_index += 1;
            t.clone()
        }
        _ => {
            return Err(PdfError::filter_error(
                0,
                "JBIG2: invalid Huffman DW selector",
            ))
        }
    };
    let table_bitmap_size = if bitmap_size_selector {
        let t = get_custom_huffman_table(custom_index, referred_to, custom_tables)?;
        custom_index += 1;
        t.clone()
    } else {
        HuffmanTable::standard(1)? // standard table B.1
    };
    let table_aggregate_instances = if aggregation_instances_selector {
        let t = get_custom_huffman_table(custom_index, referred_to, custom_tables)?;
        t.clone()
    } else {
        HuffmanTable::standard(1)?
    };
    Ok(SymbolDictHuffmanTables {
        table_delta_height,
        table_delta_width,
        table_bitmap_size,
        table_aggregate_instances,
    })
}

/// 7.4.3.1.7 Symbol ID Huffman table decoding + 7.4.3.1.6 table selection.
///
/// # Errors
/// Invalid run-code lengths, invalid selectors, missing custom tables.
#[allow(clippy::too_many_arguments)]
pub fn get_text_region_huffman_tables(
    huffman_fs: u8,
    huffman_ds: u8,
    huffman_dt: u8,
    refinement: bool,
    referred_to: &[u32],
    custom_tables: &HashMap<u32, HuffmanTable>,
    number_of_symbols: usize,
    reader: &mut super::dec::Reader<'_>,
) -> Result<TextRegionHuffmanTables> {
    // Symbol ID Huffman table decoding: code lengths for RUNCODEs 0..=34.
    let mut run_codes: Vec<super::huffman::Line> = Vec::with_capacity(35);
    for i in 0..=34u32 {
        let code_length = reader.read_bits(4)? as u8;
        run_codes.push(super::huffman::Line::range(
            i as i64,
            code_length,
            0,
            0,
            false,
        ));
    }
    let run_codes_table = HuffmanTable::new(run_codes.clone(), false)?;

    // Interpret the RUNCODE codes and the additional bits (if any).
    let mut codes: Vec<super::huffman::Line> = Vec::new();
    let mut i = 0usize;
    while i < number_of_symbols {
        let code_length = run_codes_table.decode(reader)?.ok_or_else(|| {
            PdfError::filter_error(reader.position, "JBIG2: OOB in run-code table")
        })?;
        if code_length >= 32 {
            let (number_of_repeats, repeated_length): (u32, u8) = match code_length {
                32 => {
                    if i == 0 {
                        return Err(PdfError::filter_error(
                            reader.position,
                            "JBIG2: no previous value in symbol ID table",
                        ));
                    }
                    (reader.read_bits(2)? + 3, codes[i - 1].prefix_length)
                }
                33 => (reader.read_bits(3)? + 3, 0),
                34 => (reader.read_bits(7)? + 11, 0),
                _ => {
                    return Err(PdfError::filter_error(
                        reader.position,
                        "JBIG2: invalid code length in symbol ID table",
                    ))
                }
            };
            for _ in 0..number_of_repeats {
                codes.push(super::huffman::Line::range(
                    i as i64,
                    repeated_length,
                    0,
                    0,
                    false,
                ));
                i += 1;
            }
        } else {
            codes.push(super::huffman::Line::range(
                i as i64,
                code_length as u8,
                0,
                0,
                false,
            ));
            i += 1;
        }
    }
    reader.byte_align();
    let symbol_id_table = HuffmanTable::new(codes, false)?;

    // 7.4.3.1.6 Text region segment Huffman table selection.
    let mut custom_index = 0usize;
    let table_first_s = match huffman_fs {
        0 | 1 => HuffmanTable::standard(huffman_fs as usize + 6)?,
        3 => {
            let t = get_custom_huffman_table(custom_index, referred_to, custom_tables)?;
            custom_index += 1;
            t.clone()
        }
        _ => {
            return Err(PdfError::filter_error(
                0,
                "JBIG2: invalid Huffman FS selector",
            ))
        }
    };
    let table_delta_s = match huffman_ds {
        0..=2 => HuffmanTable::standard(huffman_ds as usize + 8)?,
        3 => {
            let t = get_custom_huffman_table(custom_index, referred_to, custom_tables)?;
            custom_index += 1;
            t.clone()
        }
        _ => {
            return Err(PdfError::filter_error(
                0,
                "JBIG2: invalid Huffman DS selector",
            ))
        }
    };
    let table_delta_t = match huffman_dt {
        0..=2 => HuffmanTable::standard(huffman_dt as usize + 11)?,
        3 => {
            let t = get_custom_huffman_table(custom_index, referred_to, custom_tables)?;
            t.clone()
        }
        _ => {
            return Err(PdfError::filter_error(
                0,
                "JBIG2: invalid Huffman DT selector",
            ))
        }
    };
    if refinement {
        // Load tables RDW, RDH, RDX and RDY.
        return Err(PdfError::filter_error(
            0,
            "JBIG2: refinement with Huffman is not supported",
        ));
    }
    Ok(TextRegionHuffmanTables {
        symbol_id_table,
        table_first_s,
        table_delta_s,
        table_delta_t,
    })
}

/// 6.5 Decoding the symbol dictionary (reference `decodeSymbolDictionary`).
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub fn decode_symbol_dictionary(
    huffman: bool,
    refinement: bool,
    symbols: &[Bitmap],
    number_of_new_symbols: u32,
    number_of_exported_symbols: u32,
    huffman_tables: Option<&SymbolDictHuffmanTables>,
    template_index: u8,
    at: &[AtPixel],
    refinement_template_index: u8,
    refinement_at: &[AtPixel],
    cx: &mut DecodeCtx<'_>,
) -> Result<Vec<Bitmap>> {
    if huffman && refinement {
        return Err(PdfError::filter_error(
            0,
            "JBIG2: symbol refinement with Huffman is not supported",
        ));
    }
    let _ = number_of_exported_symbols;

    let mut new_symbols: Vec<Bitmap> = Vec::new();
    let mut current_height: i32 = 0;
    let mut symbol_code_length = log2(symbols.len() + number_of_new_symbols as usize);

    let mut symbol_widths: Vec<u32> = Vec::new();
    if huffman {
        // 6.5.8.2.3
        symbol_code_length = symbol_code_length.max(1);
    }

    while (new_symbols.len() as u32) < number_of_new_symbols {
        let delta_height = if huffman {
            let tables = huffman_tables.expect("tables present when huffman");
            let r = cx.reader_mut()?;
            match tables.table_delta_height.decode(r)? {
                Some(v) => v as i32,
                None => 0, // B.1 has no OOB line; reference decodes a value
            }
        } else {
            decode_integer(&mut cx.cache, ctx::IADH, &mut cx.decoder) // 6.5.6
                .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IADH overflow"))?
        };
        current_height += delta_height;
        let mut current_width: i32 = 0;
        let mut total_width: u32 = 0;
        let first_symbol = symbol_widths.len();

        loop {
            let delta_width = if huffman {
                let tables = huffman_tables.expect("tables present when huffman");
                let r = cx.reader_mut()?;
                match tables.table_delta_width.decode(r)? {
                    Some(v) => v as i32,
                    None => break, // OOB
                }
            } else {
                match decode_integer(&mut cx.cache, ctx::IADW, &mut cx.decoder) {
                    // 6.5.7
                    Some(v) => v,
                    None => break, // OOB
                }
            };
            current_width += delta_width;
            total_width += current_width as u32;

            if refinement {
                // 6.5.8.2 Refinement/aggregate-coded symbol bitmap
                let number_of_instances = decode_integer(&mut cx.cache, ctx::IAAI, &mut cx.decoder)
                    .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IAAI overflow"))?;
                if number_of_instances > 1 {
                    let combined: Vec<Bitmap> =
                        symbols.iter().chain(new_symbols.iter()).cloned().collect();
                    let bitmap = decode_text_region(
                        false,
                        true,
                        current_width as u32,
                        current_height as u32,
                        0,
                        number_of_instances as u32,
                        1, // strip size
                        &combined,
                        symbol_code_length,
                        false, // transposed
                        0,     // ds offset
                        1,     // top left 7.4.3.1.1
                        0,     // OR operator
                        None,
                        refinement_template_index,
                        refinement_at,
                        0, // log strip size
                        cx,
                    )?;
                    new_symbols.push(bitmap);
                } else {
                    let symbol_id = decode_iaid(&mut cx.cache, &mut cx.decoder, symbol_code_length);
                    let rdx = decode_integer(&mut cx.cache, ctx::IARDX, &mut cx.decoder) // 6.4.11.3
                        .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IARDX overflow"))?;
                    let rdy = decode_integer(&mut cx.cache, ctx::IARDY, &mut cx.decoder) // 6.4.11.4
                        .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IARDY overflow"))?;
                    let sid = symbol_id as usize;
                    let symbol = if sid < symbols.len() {
                        &symbols[sid]
                    } else {
                        &new_symbols[sid - symbols.len()]
                    };
                    let bitmap = decode_refinement(
                        current_width as u32,
                        current_height as u32,
                        refinement_template_index,
                        symbol,
                        rdx,
                        rdy,
                        false,
                        refinement_at,
                        &mut cx.decoder,
                        &mut cx.cache,
                    )?;
                    new_symbols.push(bitmap);
                }
            } else if huffman {
                // Store only symbol width; decode a collective bitmap when
                // the height class is done.
                symbol_widths.push(current_width as u32);
            } else {
                // 6.5.8.1 Direct-coded symbol bitmap
                let bitmap = decode_bitmap(
                    false,
                    current_width as u32,
                    current_height as u32,
                    template_index,
                    false,
                    at,
                    &mut cx.decoder,
                    &mut cx.cache,
                    &[],
                )?;
                new_symbols.push(bitmap);
            }
        }

        if huffman && !refinement {
            // 6.5.9 Height class collective bitmap
            let tables = huffman_tables.expect("tables present when huffman");
            let r = cx.reader_mut()?;
            let bitmap_size = tables
                .table_bitmap_size
                .decode(r)?
                .ok_or_else(|| PdfError::filter_error(r.position, "JBIG2: OOB in bitmap size"))?;
            r.byte_align();
            let collective_bitmap: Bitmap = if bitmap_size == 0 {
                // Uncompressed collective bitmap
                read_uncompressed_bitmap(r, total_width, current_height.max(0) as u32)?
            } else {
                // MMR collective bitmap over a bounded window.
                let bitmap_end = r.position + bitmap_size as usize;
                if bitmap_end > r.end {
                    return Err(PdfError::filter_error(
                        r.position,
                        "JBIG2: collective bitmap window past end of data",
                    ));
                }
                let window = &r.data[r.position..bitmap_end];
                let (bm, _consumed) = super::dec::decode_mmr_bitmap(
                    window,
                    total_width,
                    current_height.max(0) as u32,
                    false,
                )?;
                r.position = bitmap_end;
                bm
            };
            let number_of_symbols_decoded = symbol_widths.len();
            if first_symbol == number_of_symbols_decoded - 1 {
                // collectiveBitmap is a single symbol.
                new_symbols.push(collective_bitmap);
            } else {
                // Divide collectiveBitmap into symbols.
                let mut x_min = 0usize;
                for sw in &symbol_widths[first_symbol..] {
                    let x_max = x_min + *sw as usize;
                    let mut symbol_bitmap: Bitmap =
                        Vec::with_capacity(current_height.max(0) as usize);
                    for row in &collective_bitmap {
                        symbol_bitmap.push(row[x_min..x_max.min(row.len())].to_vec());
                    }
                    new_symbols.push(symbol_bitmap);
                    x_min = x_max;
                }
            }
            symbol_widths.clear();
        }
    }

    // 6.5.10 Exported symbols
    let total_symbols_length = symbols.len() + number_of_new_symbols as usize;
    let mut flags: Vec<bool> = Vec::with_capacity(total_symbols_length);
    let mut current_flag = false;
    // Standard table B.1 for Huffman export runs.
    let table_b1 = HuffmanTable::standard(1)?;
    while flags.len() < total_symbols_length {
        let run_length: i64 = if huffman {
            let r = cx.reader_mut()?;
            table_b1.decode(r)?.unwrap_or(0)
        } else {
            decode_integer(&mut cx.cache, ctx::IAEX, &mut cx.decoder).unwrap_or(0) as i64
        };
        let remaining = total_symbols_length - flags.len();
        for _ in 0..run_length.min(remaining as i64) {
            flags.push(current_flag);
        }
        current_flag = !current_flag;
    }
    let mut exported_symbols: Vec<Bitmap> = Vec::new();
    for (i, flag) in flags.iter().enumerate().take(symbols.len()) {
        if *flag {
            exported_symbols.push(symbols[i].clone());
        }
    }
    for (j, flag) in flags.iter().enumerate().skip(symbols.len()) {
        if *flag {
            exported_symbols.push(new_symbols[j - symbols.len()].clone());
        }
    }
    Ok(exported_symbols)
}

/// 6.4 Decoding a text region (reference `decodeTextRegion`).
///
/// `combination_operator`: 0 = OR, 2 = XOR. `reference_corner` bit 0 selects
/// the symbol's top (1) or bottom (0), bit 1 its right (1) / left (0) origin.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub fn decode_text_region(
    huffman: bool,
    refinement: bool,
    width: u32,
    height: u32,
    default_pixel_value: u8,
    number_of_symbol_instances: u32,
    strip_size: u32,
    input_symbols: &[Bitmap],
    symbol_code_length: u32,
    transposed: bool,
    ds_offset: i32,
    reference_corner: u8,
    combination_operator: u8,
    huffman_tables: Option<&TextRegionHuffmanTables>,
    refinement_template_index: u8,
    refinement_at: &[AtPixel],
    log_strip_size: u32,
    cx: &mut DecodeCtx<'_>,
) -> Result<Bitmap> {
    if huffman && refinement {
        return Err(PdfError::filter_error(
            0,
            "JBIG2: refinement with Huffman is not supported",
        ));
    }

    // Prepare bitmap
    let mut bitmap: Bitmap = Vec::with_capacity(height as usize);
    for _ in 0..height {
        bitmap.push(vec![default_pixel_value; width as usize]);
    }

    let mut strip_t: i32 = -if huffman {
        let tables = huffman_tables.expect("tables present when huffman");
        let r = cx.reader_mut()?;
        tables
            .table_delta_t
            .decode(r)?
            .ok_or_else(|| PdfError::filter_error(r.position, "JBIG2: OOB in delta T"))?
            as i32
    } else {
        decode_integer(&mut cx.cache, ctx::IADT, &mut cx.decoder) // 6.4.6
            .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IADT overflow"))?
    };
    let mut first_s: i32 = 0;
    let mut i: u32 = 0;
    let mut owned_symbols: Vec<Bitmap> = Vec::new();
    while i < number_of_symbol_instances {
        let delta_t = if huffman {
            let tables = huffman_tables.expect("tables present when huffman");
            let r = cx.reader_mut()?;
            tables
                .table_delta_t
                .decode(r)?
                .ok_or_else(|| PdfError::filter_error(r.position, "JBIG2: OOB in delta T"))?
                as i32
        } else {
            decode_integer(&mut cx.cache, ctx::IADT, &mut cx.decoder)
                .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IADT overflow"))?
        };
        strip_t += delta_t;

        let delta_first_s = if huffman {
            let tables = huffman_tables.expect("tables present when huffman");
            let r = cx.reader_mut()?;
            tables
                .table_first_s
                .decode(r)?
                .ok_or_else(|| PdfError::filter_error(r.position, "JBIG2: OOB in first S"))?
                as i32
        } else {
            decode_integer(&mut cx.cache, ctx::IAFS, &mut cx.decoder) // 6.4.7
                .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IAFS overflow"))?
        };
        first_s += delta_first_s;
        let mut current_s = first_s;

        loop {
            let mut current_t: i32 = 0; // 6.4.9
            if strip_size > 1 {
                current_t = if huffman {
                    let r = cx.reader_mut()?;
                    r.read_bits(log_strip_size)? as i32
                } else {
                    decode_integer(&mut cx.cache, ctx::IAIT, &mut cx.decoder)
                        .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IAIT overflow"))?
                };
            }
            let t = (strip_size as i64 * strip_t as i64 + current_t as i64) as i32;
            let symbol_id: u32 = if huffman {
                let tables = huffman_tables.expect("tables present when huffman");
                let r = cx.reader_mut()?;
                tables
                    .symbol_id_table
                    .decode(r)?
                    .ok_or_else(|| PdfError::filter_error(r.position, "JBIG2: OOB symbol id"))?
                    as u32
            } else {
                decode_iaid(&mut cx.cache, &mut cx.decoder, symbol_code_length)
            };
            let apply_refinement = refinement
                && decode_integer(&mut cx.cache, ctx::IARI, &mut cx.decoder)
                    .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IARI overflow"))?
                    != 0;

            let sid = symbol_id as usize;
            if sid >= input_symbols.len() {
                return Err(PdfError::filter_error(
                    0,
                    format!("JBIG2: symbol id {symbol_id} out of range"),
                ));
            }
            let mut symbol_bitmap: &Bitmap = &input_symbols[sid];
            let mut symbol_width = symbol_bitmap.first().map_or(0, Vec::len) as i32;
            let mut symbol_height = symbol_bitmap.len() as i32;
            if apply_refinement {
                let rdw = decode_integer(&mut cx.cache, ctx::IARDW, &mut cx.decoder) // 6.4.11.1
                    .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IARDW overflow"))?;
                let rdh = decode_integer(&mut cx.cache, ctx::IARDH, &mut cx.decoder) // 6.4.11.2
                    .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IARDH overflow"))?;
                let rdx = decode_integer(&mut cx.cache, ctx::IARDX, &mut cx.decoder)
                    .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IARDX overflow"))?;
                let rdy = decode_integer(&mut cx.cache, ctx::IARDY, &mut cx.decoder)
                    .ok_or_else(|| PdfError::filter_error(0, "JBIG2: IARDY overflow"))?;
                symbol_width += rdw;
                symbol_height += rdh;
                let refined = decode_refinement(
                    symbol_width.max(0) as u32,
                    symbol_height.max(0) as u32,
                    refinement_template_index,
                    symbol_bitmap,
                    (rdw >> 1) + rdx,
                    (rdh >> 1) + rdy,
                    false,
                    refinement_at,
                    &mut cx.decoder,
                    &mut cx.cache,
                )?;
                owned_symbols.push(refined);
                symbol_bitmap = owned_symbols.last().expect("just pushed");
            }

            let offset_t = t - if reference_corner & 1 != 0 {
                0
            } else {
                symbol_height - 1
            };
            let offset_s = current_s
                - if reference_corner & 2 != 0 {
                    symbol_width - 1
                } else {
                    0
                };

            if transposed {
                // Place Symbol Bitmap from T1,S1
                for s2 in 0..symbol_height {
                    let row_idx = offset_s + s2;
                    if row_idx < 0 || row_idx >= height as i32 {
                        continue;
                    }
                    if symbol_bitmap.get(s2 as usize).map_or(0, Vec::len) == 0 {
                        continue;
                    }
                    let max_width = (width as i32 - offset_t).min(symbol_width);
                    for t2 in 0..max_width {
                        let col = offset_t + t2;
                        if col < 0 || col >= width as i32 {
                            continue; // outside region — JS drops OOB writes
                        }
                        let sv = symbol_bitmap[s2 as usize][t2 as usize];
                        let cell = &mut bitmap[row_idx as usize][col as usize];
                        *cell = match combination_operator {
                            0 => *cell | sv, // OR
                            2 => *cell ^ sv, // XOR
                            other => {
                                return Err(PdfError::filter_error(
                                    0,
                                    format!("JBIG2: operator {other} is not supported"),
                                ))
                            }
                        };
                    }
                }
                current_s += symbol_height - 1;
            } else {
                for t2 in 0..symbol_height {
                    let row_idx = offset_t + t2;
                    if row_idx < 0 || row_idx >= height as i32 {
                        continue;
                    }
                    if symbol_bitmap.get(t2 as usize).map_or(0, Vec::len) == 0 {
                        continue;
                    }
                    for s2 in 0..symbol_width {
                        let col = offset_s + s2;
                        if col < 0 || col >= width as i32 {
                            continue;
                        }
                        let sv = symbol_bitmap[t2 as usize][s2 as usize];
                        let cell = &mut bitmap[row_idx as usize][col as usize];
                        *cell = match combination_operator {
                            0 => *cell | sv,
                            2 => *cell ^ sv,
                            other => {
                                return Err(PdfError::filter_error(
                                    0,
                                    format!("JBIG2: operator {other} is not supported"),
                                ))
                            }
                        };
                    }
                }
                current_s += symbol_width - 1;
            }
            i += 1;

            let delta_s = if huffman {
                let tables = huffman_tables.expect("tables present when huffman");
                let r = cx.reader_mut()?;
                match tables.table_delta_s.decode(r)? {
                    Some(v) => v as i32,
                    None => break, // OOB — end of strip
                }
            } else {
                match decode_integer(&mut cx.cache, ctx::IADS, &mut cx.decoder) {
                    // 6.4.8
                    Some(v) => v,
                    None => break, // OOB
                }
            };
            current_s += delta_s + ds_offset;
        }
    }
    Ok(bitmap)
}
