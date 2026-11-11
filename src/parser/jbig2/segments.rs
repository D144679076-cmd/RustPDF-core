//! JBIG2 segment/file format (clause 7) and the segment-processing visitor
//! — port of pdf.js v3.11.174 `readSegmentHeader`, `readSegments`,
//! `processSegment` and `SimpleSegmentVisitor` from `src/core/jbig2.js`
//! (Apache 2.0).

use super::dec::{log2, Bitmap, DecodeCtx, Reader};
use super::generic::{decode_bitmap, AtPixel};
use super::halftone::{decode_halftone_region, decode_pattern_dictionary};
use super::huffman::{decode_tables_segment, read_u16, read_u32, HuffmanTable};
use super::symbol::{
    decode_symbol_dictionary, decode_text_region, get_symbol_dictionary_huffman_tables,
    get_text_region_huffman_tables, TextRegionHuffmanTables,
};
use crate::error::{PdfError, Result};
use std::collections::HashMap;

/// One parsed segment header (7.2).
#[derive(Debug)]
pub struct SegmentHeader {
    /// Segment number.
    pub number: u32,
    /// Segment type (7.2.2).
    pub kind: u8,
    /// Deferred-non-retain flag.
    pub deferred_non_retain: bool,
    /// Retain bits (bit 0 of each = retained).
    #[allow(dead_code)]
    pub retain_bits: Vec<u8>,
    /// Referred-to segment numbers.
    pub referred_to: Vec<u32>,
    /// Page association.
    #[allow(dead_code)]
    pub page_association: u32,
    /// Data length (may be computed for unknown-length generic regions).
    pub length: usize,
    /// Offset just past the header.
    pub header_end: usize,
}

/// A segment: header plus its byte range in `data`.
pub struct Segment<'a> {
    pub header: SegmentHeader,
    pub data: &'a [u8],
    pub start: usize,
    pub end: usize,
}

/// 7.4.1 Region segment information field.
#[derive(Debug, Clone, Copy)]
pub struct RegionInfo {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub combination_operator: u8,
}

const REGION_INFO_LEN: usize = 17;

fn byte_at(data: &[u8], pos: usize) -> Result<u8> {
    data.get(pos)
        .copied()
        .ok_or_else(|| PdfError::filter_error(pos, "JBIG2: truncated segment header"))
}

fn read_int8(data: &[u8], pos: usize) -> Result<i8> {
    Ok(byte_at(data, pos)? as i8)
}

/// 7.2 Segment header parsing (reference `readSegmentHeader`).
pub fn read_segment_header(data: &[u8], start: usize) -> Result<SegmentHeader> {
    if start + 11 > data.len() {
        return Err(PdfError::filter_error(
            start,
            "JBIG2: truncated segment header",
        ));
    }
    let number = read_u32(data, start);
    let flags = data[start + 4];
    let kind = flags & 0x3f;
    // Segment types known to the reference's SegmentTypes table; anything
    // else is rejected at parse time like the reference.
    if !matches!(
        kind,
        0 | 4 | 6 | 7 | 16 | 20 | 22 | 23 | 36 | 38 | 39 | 48 | 49 | 50 | 51 | 53 | 62
    ) {
        return Err(PdfError::filter_error(
            start + 4,
            format!("JBIG2: invalid segment type: {kind}"),
        ));
    }
    let deferred_non_retain = flags & 0x80 != 0;

    let page_association_field_size = flags & 0x40 != 0;
    let referred_flags = data[start + 5];
    let mut referred_to_count = ((referred_flags >> 5) & 7) as usize;
    let mut retain_bits: Vec<u8> = vec![referred_flags & 31];
    let mut position = start + 6;
    if referred_flags == 7 {
        referred_to_count = (read_u32(data, position - 1) & 0x1fff_ffff) as usize;
        position += 3;
        let bytes = (referred_to_count + 7) >> 3;
        retain_bits[0] = byte_at(data, position)?;
        position += 1;
        let mut remaining = bytes - 1;
        while remaining > 0 {
            retain_bits.push(byte_at(data, position)?);
            position += 1;
            remaining -= 1;
        }
    } else if referred_flags == 5 || referred_flags == 6 {
        return Err(PdfError::filter_error(
            start + 5,
            "JBIG2: invalid referred-to flags",
        ));
    }

    let referred_to_segment_number_size = if number <= 256 {
        1usize
    } else if number <= 65536 {
        2
    } else {
        4
    };
    let mut referred_to = Vec::with_capacity(referred_to_count.min(1024));
    for _ in 0..referred_to_count {
        let num = match referred_to_segment_number_size {
            1 => u32::from(byte_at(data, position)?),
            2 => u32::from(read_u16(data, position)),
            _ => read_u32(data, position),
        };
        referred_to.push(num);
        position += referred_to_segment_number_size;
    }

    let page_association = if !page_association_field_size {
        let v = u32::from(byte_at(data, position)?);
        position += 1;
        v
    } else {
        let v = read_u32(data, position);
        position += 4;
        v
    };
    let mut length = read_u32(data, position) as usize;
    position += 4;

    if length == 0xffff_ffff {
        // 7.2.7 Unknown segment length — only valid for ImmediateGenericRegion.
        if kind == 38 {
            let region_info = read_region_segment_information(data, position)?;
            let flags_byte = byte_at(data, position + REGION_INFO_LEN)?;
            let mmr = flags_byte & 1 != 0;
            // Search for the segment end pattern: 0xFF 0xAC followed by the
            // region height (non-MMR), or just the height (MMR).
            let mut search: [u8; 6] = [0; 6];
            let search_len;
            if !mmr {
                search[0] = 0xff;
                search[1] = 0xac;
                search[2] = (region_info.height >> 24) as u8;
                search[3] = (region_info.height >> 16) as u8;
                search[4] = (region_info.height >> 8) as u8;
                search[5] = region_info.height as u8;
                search_len = 6;
            } else {
                search[0] = (region_info.height >> 24) as u8;
                search[1] = (region_info.height >> 16) as u8;
                search[2] = (region_info.height >> 8) as u8;
                search[3] = region_info.height as u8;
                search_len = 4;
            }
            let mut found = false;
            for i in position..data.len() {
                if i + search_len > data.len() {
                    break;
                }
                if data[i..i + search_len] == search[..search_len] {
                    length = i + search_len - start;
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(PdfError::filter_error(
                    position,
                    "JBIG2: segment end was not found",
                ));
            }
        } else {
            return Err(PdfError::filter_error(
                position,
                "JBIG2: invalid unknown segment length",
            ));
        }
    }
    Ok(SegmentHeader {
        number,
        kind,
        deferred_non_retain,
        retain_bits,
        referred_to,
        page_association,
        length,
        header_end: position,
    })
}

/// Read all segments from `data[start..end]` (reference `readSegments`).
///
/// `random_access` selects between sequential (length-prefix) and
/// random-access (data follows all headers) layouts.
pub fn read_segments<'a>(
    data: &'a [u8],
    start: usize,
    end: usize,
    random_access: bool,
) -> Result<Vec<Segment<'a>>> {
    let mut segments: Vec<Segment<'a>> = Vec::new();
    let mut position = start;
    while position < end {
        let header = read_segment_header(data, position)?;
        position = header.header_end;
        let mut seg = Segment {
            header,
            data,
            start: 0,
            end: 0,
        };
        if !random_access {
            seg.start = position;
            position += seg.header.length;
            seg.end = position;
        }
        let is_eof = seg.header.kind == 51;
        segments.push(seg);
        if is_eof {
            break; // end of file found
        }
    }
    if random_access {
        for seg in &mut segments {
            seg.start = position;
            position += seg.header.length;
            seg.end = position;
        }
    }
    Ok(segments)
}

/// 7.4.1 Region segment information field reader.
pub fn read_region_segment_information(data: &[u8], start: usize) -> Result<RegionInfo> {
    if start + REGION_INFO_LEN > data.len() {
        return Err(PdfError::filter_error(
            start,
            "JBIG2: truncated region segment information",
        ));
    }
    Ok(RegionInfo {
        width: read_u32(data, start),
        height: read_u32(data, start + 4),
        x: read_u32(data, start + 8),
        y: read_u32(data, start + 12),
        combination_operator: data[start + 16] & 7,
    })
}

/// 7.4.9 Page information segment.
#[derive(Debug, Clone, Copy)]
pub struct PageInfo {
    pub width: u32,
    /// `None` when the height field is 0xffffffff (unknown).
    pub height: Option<u32>,
    pub lossless: bool,
    pub refinement: bool,
    pub default_pixel_value: u8,
    pub combination_operator: u8,
    pub requires_buffer: bool,
    pub combination_operator_override: bool,
}

/// Segment-processing visitor (reference `SimpleSegmentVisitor`): page
/// state plus the symbol/pattern/custom-table stores shared across
/// segments.
#[derive(Default)]
pub struct Visitor {
    pub page_info: Option<PageInfo>,
    /// Packed 1-bpp page buffer (rowSize = (width + 7) >> 3; 1 = black).
    pub buffer: Vec<u8>,
    pub page_width: u32,
    pub page_height: u32,
    symbols: HashMap<u32, Vec<Bitmap>>,
    patterns: HashMap<u32, Vec<Bitmap>>,
    custom_tables: HashMap<u32, HuffmanTable>,
}

impl Visitor {
    /// 7.4.9 `onPageInformation`: allocate the packed page buffer.
    pub fn on_page_information(&mut self, info: PageInfo) {
        self.page_info = Some(info);
        self.page_width = info.width;
        self.page_height = info.height.unwrap_or(0);
        let row_size = ((info.width + 7) >> 3) as usize;
        let len = row_size * self.page_height as usize;
        self.buffer.clear();
        self.buffer.resize(
            len,
            if info.default_pixel_value != 0 {
                0xff
            } else {
                0x00
            },
        );
    }

    /// OR/XOR a decoded region bitmap into the page buffer
    /// (reference `drawBitmap`). Out-of-page pixels are dropped, matching
    /// the reference's silent `Uint8ClampedArray` OOB semantics.
    pub fn draw_bitmap(&mut self, region: RegionInfo, bitmap: &Bitmap) -> Result<()> {
        let Some(page) = self.page_info else {
            return Err(PdfError::filter_error(0, "JBIG2: region before page info"));
        };
        let row_size = ((page.width + 7) >> 3) as usize;
        let combination_operator = if page.combination_operator_override {
            region.combination_operator
        } else {
            page.combination_operator
        };
        let width = region.width as usize;
        let height = region.height as usize;
        for i in 0..height {
            let page_row = region.y as usize + i;
            if page_row >= self.page_height as usize {
                continue;
            }
            for j in 0..width {
                if bitmap.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0) == 0 {
                    continue;
                }
                let px = region.x as usize + j;
                let byte_index = page_row * row_size + px / 8;
                if byte_index >= self.buffer.len() {
                    continue; // outside page — JS drops OOB writes
                }
                let mask = 128u8 >> (px % 8);
                self.buffer[byte_index] = match combination_operator {
                    0 => self.buffer[byte_index] | mask, // OR
                    2 => self.buffer[byte_index] ^ mask, // XOR
                    other => {
                        return Err(PdfError::filter_error(
                            0,
                            format!("JBIG2: operator {other} is not supported"),
                        ))
                    }
                };
            }
        }
        Ok(())
    }

    /// Build a [`DecodeCtx`] over the segment's data window.
    fn region_ctx<'a>(&self, data: &'a [u8], start: usize, end: usize) -> DecodeCtx<'a> {
        let mut cx = DecodeCtx::new(&super::dec::DecodingContext::new(data, start, end), None);
        cx.data = &data[start..end];
        cx
    }
}

/// Dispatch one segment (reference `processSegment`).
#[allow(clippy::too_many_lines)]
pub fn process_segment(segment: &Segment<'_>, visitor: &mut Visitor) -> Result<()> {
    let header = &segment.header;
    let data = segment.data;
    let end = segment.end;
    let mut position = segment.start;

    match header.kind {
        0 => {
            // 7.4.2 Symbol dictionary segment syntax
            let dictionary_flags = read_u16(data, position); // 7.4.2.1.1
            let huffman = dictionary_flags & 1 != 0;
            let refinement = dictionary_flags & 2 != 0;
            let huffman_dh = ((dictionary_flags >> 2) & 3) as u8;
            let huffman_dw = ((dictionary_flags >> 4) & 3) as u8;
            let bitmap_size_selector = (dictionary_flags >> 6) & 1 != 0;
            let aggregation_instances_selector = (dictionary_flags >> 7) & 1 != 0;
            let template = ((dictionary_flags >> 10) & 3) as u8;
            let refinement_template = ((dictionary_flags >> 12) & 1) as u8;
            position += 2;
            let mut at: Vec<AtPixel> = Vec::new();
            if !huffman {
                let at_length = if template == 0 { 4 } else { 1 };
                for _ in 0..at_length {
                    at.push(AtPixel {
                        x: read_int8(data, position)? as i32,
                        y: read_int8(data, position + 1)? as i32,
                    });
                    position += 2;
                }
            }
            let mut refinement_at: Vec<AtPixel> = Vec::new();
            if refinement && refinement_template == 0 {
                for _ in 0..2 {
                    refinement_at.push(AtPixel {
                        x: read_int8(data, position)? as i32,
                        y: read_int8(data, position + 1)? as i32,
                    });
                    position += 2;
                }
            }
            let number_of_exported_symbols = read_u32(data, position);
            position += 4;
            let number_of_new_symbols = read_u32(data, position);
            position += 4;

            // 7.4.2.1.6 Huffman table selection (done here so custom-table
            // clones can be stored alongside the symbols).
            let tables = if huffman {
                Some(get_symbol_dictionary_huffman_tables(
                    huffman_dh,
                    huffman_dw,
                    bitmap_size_selector,
                    aggregation_instances_selector,
                    &header.referred_to,
                    &visitor.custom_tables,
                )?)
            } else {
                None
            };
            let reader = if huffman {
                Some(Reader::new(data, position, end))
            } else {
                None
            };
            let mut cx = visitor.region_ctx(data, position, end);
            cx.reader = reader;

            let mut input_symbols: Vec<Bitmap> = Vec::new();
            for seg in &header.referred_to {
                if let Some(sym) = visitor.symbols.get(seg) {
                    input_symbols.extend(sym.iter().cloned());
                }
            }

            let exported = decode_symbol_dictionary(
                huffman,
                refinement,
                &input_symbols,
                number_of_new_symbols,
                number_of_exported_symbols,
                tables.as_ref(),
                template,
                &at,
                refinement_template,
                &refinement_at,
                &mut cx,
            )?;
            visitor.symbols.insert(header.number, exported);
        }
        6 | 7 => {
            // 7.4.3 Immediate[Lossless]TextRegion
            let region = read_region_segment_information(data, position)?;
            position += REGION_INFO_LEN;
            let flags = read_u16(data, position);
            position += 2;
            let huffman = flags & 1 != 0;
            let refinement = flags & 2 != 0;
            let log_strip_size = ((flags >> 2) & 3) as u32;
            let strip_size = 1u32 << log_strip_size;
            let reference_corner = ((flags >> 4) & 3) as u8;
            let transposed = flags & 64 != 0;
            let combination_operator = ((flags >> 7) & 3) as u8;
            let default_pixel_value = ((flags >> 9) & 1) as u8;
            let ds_offset = (i32::from(flags) << 17) >> 27;
            let refinement_template = ((flags >> 15) & 1) as u8;
            // 7.4.3.1.2 Text region Huffman flags (present only when
            // /Huffman is set; read before the refinement AT pixels).
            let mut huffman_fs = 0u8;
            let mut huffman_ds = 0u8;
            let mut huffman_dt = 0u8;
            if huffman {
                let huffman_flags = read_u16(data, position);
                position += 2;
                huffman_fs = (huffman_flags & 3) as u8;
                huffman_ds = ((huffman_flags >> 2) & 3) as u8;
                huffman_dt = ((huffman_flags >> 4) & 3) as u8;
            }
            let mut refinement_at: Vec<AtPixel> = Vec::new();
            if refinement && refinement_template == 0 {
                for _ in 0..2 {
                    refinement_at.push(AtPixel {
                        x: read_int8(data, position)? as i32,
                        y: read_int8(data, position + 1)? as i32,
                    });
                    position += 2;
                }
            }
            let number_of_symbol_instances = read_u32(data, position);
            position += 4;

            let mut input_symbols: Vec<Bitmap> = Vec::new();
            for seg in &header.referred_to {
                if let Some(sym) = visitor.symbols.get(seg) {
                    input_symbols.extend(sym.iter().cloned());
                }
            }
            let symbol_code_length = log2(input_symbols.len());

            let mut cx = visitor.region_ctx(data, position, end);
            let tables: Option<TextRegionHuffmanTables> = if huffman {
                let r = Reader::new(data, position, end);
                let mut r = r;
                let t = get_text_region_huffman_tables(
                    huffman_fs,
                    huffman_ds,
                    huffman_dt,
                    refinement,
                    &header.referred_to,
                    &visitor.custom_tables,
                    input_symbols.len(),
                    &mut r,
                )?;
                cx.reader = Some(r);
                Some(t)
            } else {
                None
            };

            let bitmap = decode_text_region(
                huffman,
                refinement,
                region.width,
                region.height,
                default_pixel_value,
                number_of_symbol_instances,
                strip_size,
                &input_symbols,
                symbol_code_length,
                transposed,
                ds_offset,
                reference_corner,
                combination_operator,
                tables.as_ref(),
                refinement_template,
                &refinement_at,
                log_strip_size,
                &mut cx,
            )?;
            visitor.draw_bitmap(region, &bitmap)?;
        }
        16 => {
            // 7.4.4 Pattern dictionary segment syntax
            let flags = byte_at(data, position)?;
            position += 1;
            let mmr = flags & 1 != 0;
            let template = ((flags >> 1) & 3) as u8;
            let pattern_width = u32::from(byte_at(data, position)?);
            position += 1;
            let pattern_height = u32::from(byte_at(data, position)?);
            position += 1;
            let max_pattern_index = read_u32(data, position);
            position += 4;

            let mut cx = visitor.region_ctx(data, position, end);
            let patterns = decode_pattern_dictionary(
                mmr,
                pattern_width,
                pattern_height,
                max_pattern_index,
                template,
                &mut cx,
            )?;
            visitor.patterns.insert(header.number, patterns);
        }
        22 | 23 => {
            // 7.4.5 Halftone region segment syntax
            let region = read_region_segment_information(data, position)?;
            position += REGION_INFO_LEN;
            let flags = byte_at(data, position)?;
            position += 1;
            let mmr = flags & 1 != 0;
            let template = ((flags >> 1) & 3) as u8;
            let enable_skip = flags & 8 != 0;
            let combination_operator = ((flags >> 4) & 7) as u8;
            let default_pixel_value = ((flags >> 7) & 1) as u8;
            let grid_width = read_u32(data, position);
            position += 4;
            let grid_height = read_u32(data, position);
            position += 4;
            let grid_offset_x = read_u32(data, position) as i32;
            position += 4;
            let grid_offset_y = read_u32(data, position) as i32;
            position += 4;
            let grid_vector_x = i32::from(read_u16(data, position));
            position += 2;
            let grid_vector_y = i32::from(read_u16(data, position));
            position += 2;

            // Halftone region refers to exactly one PatternDictionary.
            let patterns = header
                .referred_to
                .first()
                .and_then(|seg| visitor.patterns.get(seg))
                .ok_or_else(|| {
                    PdfError::filter_error(
                        position,
                        "JBIG2: halftone region without referred pattern dictionary",
                    )
                })?
                .clone();

            let mut cx = visitor.region_ctx(data, position, end);
            let bitmap = decode_halftone_region(
                mmr,
                &patterns,
                template,
                region.width,
                region.height,
                default_pixel_value,
                enable_skip,
                combination_operator,
                grid_width,
                grid_height,
                grid_offset_x,
                grid_offset_y,
                grid_vector_x,
                grid_vector_y,
                &mut cx,
            )?;
            visitor.draw_bitmap(region, &bitmap)?;
        }
        38 | 39 => {
            // 7.4.6 Immediate[Lossless]GenericRegion
            let region = read_region_segment_information(data, position)?;
            position += REGION_INFO_LEN;
            let flags = byte_at(data, position)?;
            position += 1;
            let mmr = flags & 1 != 0;
            let template = ((flags >> 1) & 3) as u8;
            let prediction = flags & 8 != 0;
            let mut at: Vec<AtPixel> = Vec::new();
            if !mmr {
                let at_length = if template == 0 { 4 } else { 1 };
                for _ in 0..at_length {
                    at.push(AtPixel {
                        x: read_int8(data, position)? as i32,
                        y: read_int8(data, position + 1)? as i32,
                    });
                    position += 2;
                }
            }
            let mut cx = visitor.region_ctx(data, position, end);
            let bitmap = decode_bitmap(
                mmr,
                region.width,
                region.height,
                template,
                prediction,
                &at,
                &mut cx.decoder,
                &mut cx.cache,
                &data[position..end],
            )?;
            visitor.draw_bitmap(region, &bitmap)?;
        }
        48 => {
            // 7.4.9 Page information
            let width = read_u32(data, position);
            let height_raw = read_u32(data, position + 4);
            let page_flags = byte_at(data, position + 16)?;
            visitor.on_page_information(PageInfo {
                width,
                height: if height_raw == 0xffff_ffff {
                    None
                } else {
                    Some(height_raw)
                },
                lossless: page_flags & 1 != 0,
                refinement: page_flags & 2 != 0,
                default_pixel_value: (page_flags >> 2) & 1,
                combination_operator: (page_flags >> 3) & 3,
                requires_buffer: page_flags & 32 != 0,
                combination_operator_override: page_flags & 64 != 0,
            });
        }
        49 | 50 | 51 | 62 => {
            // EndOfPage / EndOfStripe / EndOfFile / extension comments —
            // nothing to do.
        }
        53 => {
            // Tables segment — custom Huffman table (Annex B.2).
            let table = decode_tables_segment(data, position, end)?;
            visitor.custom_tables.insert(header.number, table);
        }
        other => {
            return Err(PdfError::filter_error(
                segment.start,
                format!("JBIG2: segment type {other} is not implemented"),
            ))
        }
    }
    Ok(())
}

/// Process a segment list in order (reference `processSegments`).
pub fn process_segments(segments: &[Segment<'_>], visitor: &mut Visitor) -> Result<()> {
    for seg in segments {
        process_segment(seg, visitor)?;
    }
    Ok(())
}

/// Parse a JBIG2 embedded stream (optionally preceded by `/JBIG2Globals`
/// data) into the packed 1-bpp page buffer.
///
/// `chunks` are processed in order: the globals stream (if any) first, then
/// the image stream — matching pdf.js `parseJbig2Chunks`.
///
/// Returns `(width, height, packed_rows)` with 1 = black.
///
/// # Errors
/// Truncated/invalid segment data, unsupported segment types, decode
/// failures.
pub fn decode_chunks(chunks: &[&[u8]]) -> Result<(u32, u32, Vec<u8>)> {
    let mut visitor = Visitor::default();
    for &data in chunks {
        let segments = read_segments(data, 0, data.len(), false)?;
        process_segments(&segments, &mut visitor)?;
    }
    if visitor.page_info.is_none() {
        return Err(PdfError::filter_error(
            0,
            "JBIG2: no page information segment",
        ));
    }
    Ok((visitor.page_width, visitor.page_height, visitor.buffer))
}
