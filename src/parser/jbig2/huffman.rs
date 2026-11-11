//! Annex B Huffman decoding — port of pdf.js v3.11.174 `HuffmanLine`,
//! `HuffmanTreeNode`, `HuffmanTable`, `decodeTablesSegment`,
//! `getStandardTable` and `getCustomHuffmanTable` from `src/core/jbig2.js`
//! (Apache 2.0).

use super::dec::Reader;
use super::tables::{self, HuffmanLine};
use crate::error::{PdfError, Result};

/// Owned Huffman line with prefix code assigned (B.3) or taken from the
/// standard tables. `range_low` is `i64` because custom-table lowest values
/// are full 32-bit unsigned quantities in the reference.
#[derive(Debug, Clone, Copy)]
pub struct Line {
    pub range_low: i64,
    pub prefix_length: u8,
    pub range_length: u8,
    pub prefix_code: u32,
    pub lower: bool,
    pub oob: bool,
}

impl From<HuffmanLine> for Line {
    fn from(l: HuffmanLine) -> Self {
        Line {
            range_low: l.range_low as i64,
            prefix_length: l.prefix_length,
            range_length: l.range_length,
            prefix_code: l.prefix_code,
            lower: l.lower,
            oob: l.oob,
        }
    }
}

impl Line {
    /// New OOB line `(prefix_length, prefix_code)`.
    pub fn oob(prefix_length: u8, prefix_code: u32) -> Self {
        Line {
            range_low: 0,
            prefix_length,
            range_length: 0,
            prefix_code,
            lower: false,
            oob: true,
        }
    }

    /// New normal/upper/lower-range line.
    #[allow(clippy::too_many_arguments)]
    pub fn range(
        range_low: i64,
        prefix_length: u8,
        range_length: u8,
        prefix_code: u32,
        lower: bool,
    ) -> Self {
        Line {
            range_low,
            prefix_length,
            range_length,
            prefix_code,
            lower,
            oob: false,
        }
    }
}

#[derive(Clone)]
struct Leaf {
    range_low: i64,
    range_length: u8,
    lower: bool,
}

#[derive(Clone)]
enum Node {
    Leaf(Leaf),
    Oob,
    Inner([Option<Box<Node>>; 2]),
}

/// A built Huffman decoding tree (reference `HuffmanTable`).
#[derive(Clone)]
pub struct HuffmanTable {
    root: Node,
}

impl HuffmanTable {
    /// Build a table from `lines`, assigning prefix codes first (Annex B.3)
    /// unless `prefix_codes_done` (standard tables carry their codes).
    ///
    /// # Errors
    /// When codes collide or the tree is malformed ("invalid Huffman data"
    /// is raised later at decode time; construction only fails on code
    /// overflow).
    pub fn new(mut lines: Vec<Line>, prefix_codes_done: bool) -> Result<Self> {
        if !prefix_codes_done {
            assign_prefix_codes(&mut lines);
        }
        let mut root = Node::Inner([None, None]);
        for line in &lines {
            if line.prefix_length > 0 {
                build_tree(&mut root, line, line.prefix_length - 1)?;
            }
        }
        Ok(HuffmanTable { root })
    }

    /// Build a tree from one of the 15 standard tables (codes pre-assigned).
    pub fn standard(number: usize) -> Result<Self> {
        let table = standard_table(number)?;
        Self::new(table.iter().copied().map(Line::from).collect(), true)
    }

    /// Decode one value; `None` means the out-of-band line was hit.
    ///
    /// # Errors
    /// Truncated data or an unassigned code path ("invalid Huffman data").
    fn decode_node(node: &Node, reader: &mut Reader<'_>) -> Result<Option<i64>> {
        match node {
            Node::Oob => Ok(None),
            Node::Leaf(leaf) => {
                let offset = reader.read_bits(u32::from(leaf.range_length))? as i64;
                Ok(Some(if leaf.lower {
                    leaf.range_low - offset
                } else {
                    leaf.range_low + offset
                }))
            }
            Node::Inner(children) => {
                let bit = reader.read_bit()? as usize;
                match &children[bit] {
                    Some(child) => Self::decode_node(child, reader),
                    None => Err(PdfError::filter_error(
                        reader.position,
                        "JBIG2: invalid Huffman data",
                    )),
                }
            }
        }
    }

    /// Decode one value from `reader`; `None` = out-of-band.
    pub fn decode(&self, reader: &mut Reader<'_>) -> Result<Option<i64>> {
        Self::decode_node(&self.root, reader)
    }
}

/// Annex B.3 Assigning the prefix codes (reference `assignPrefixCodes`).
fn assign_prefix_codes(lines: &mut [Line]) {
    let mut prefix_length_max: usize = 0;
    for line in lines.iter() {
        prefix_length_max = prefix_length_max.max(line.prefix_length as usize);
    }
    let mut histogram = vec![0u32; prefix_length_max + 1];
    for line in lines.iter() {
        histogram[line.prefix_length as usize] += 1;
    }
    histogram[0] = 0;

    let mut first_code: u32 = 0;
    for current_length in 1..=prefix_length_max {
        first_code = (first_code + histogram[current_length - 1]) << 1;
        let mut current_code = first_code;
        for line in lines.iter_mut() {
            if line.prefix_length as usize == current_length {
                line.prefix_code = current_code;
                current_code += 1;
            }
        }
    }
}

/// Insert `line` into the tree at `shift` (reference `HuffmanTreeNode
/// .buildTree`).
fn build_tree(node: &mut Node, line: &Line, shift: u8) -> Result<()> {
    let bit = ((line.prefix_code >> shift) & 1) as usize;
    if shift == 0 {
        let slot = match node {
            Node::Inner(children) => &mut children[bit],
            Node::Leaf(_) | Node::Oob => {
                return Err(PdfError::filter_error(
                    0,
                    "JBIG2: duplicate Huffman prefix code",
                ))
            }
        };
        if slot.is_some() {
            return Err(PdfError::filter_error(
                0,
                "JBIG2: duplicate Huffman prefix code",
            ));
        }
        *slot = Some(Box::new(if line.oob {
            Node::Oob
        } else {
            Node::Leaf(Leaf {
                range_low: line.range_low,
                range_length: line.range_length,
                lower: line.lower,
            })
        }));
        Ok(())
    } else {
        // Intermediate node — recurse.
        let inner = match node {
            Node::Inner(children) => &mut children[bit],
            Node::Leaf(_) | Node::Oob => {
                return Err(PdfError::filter_error(
                    0,
                    "JBIG2: duplicate Huffman prefix code",
                ))
            }
        };
        if inner.is_none() {
            *inner = Some(Box::new(Node::Inner([None, None])));
        }
        let child = inner.as_mut().expect("just assigned");
        build_tree(child, line, shift - 1)
    }
}

/// One of the 15 standard tables B.1–B.15 (`&STANDARD_TABLES[n - 1]`).
///
/// # Errors
/// Numbers outside 1..=15 ("standard table B.n does not exist").
pub fn standard_table(number: usize) -> Result<&'static [HuffmanLine]> {
    if !(1..=15).contains(&number) {
        return Err(PdfError::filter_error(
            0,
            format!("JBIG2: standard table B.{number} does not exist"),
        ));
    }
    Ok(tables::STANDARD_TABLES[number - 1])
}

/// B.2 Code-table structure — parse a custom Tables segment
/// (reference `decodeTablesSegment`).
pub fn decode_tables_segment(data: &[u8], start: usize, end: usize) -> Result<HuffmanTable> {
    if start + 9 > end {
        return Err(PdfError::filter_error(
            start,
            "JBIG2: truncated Huffman table segment",
        ));
    }
    let flags = data[start];
    let lowest_value = read_u32(data, start + 1) as i64;
    let highest_value = read_u32(data, start + 5) as i64;
    let mut reader = Reader::new(data, start + 9, end);

    let prefix_size_bits = u32::from((flags >> 1) & 7) + 1;
    let range_size_bits = u32::from((flags >> 4) & 7) + 1;

    let mut lines: Vec<Line> = Vec::new();
    // Normal table lines.
    let mut current_range_low = lowest_value;
    loop {
        let prefix_length = reader.read_bits(prefix_size_bits)? as u8;
        let range_length = reader.read_bits(range_size_bits)? as u8;
        lines.push(Line::range(
            current_range_low,
            prefix_length,
            range_length,
            0,
            false,
        ));
        current_range_low += 1 << range_length;
        if current_range_low >= highest_value {
            break;
        }
    }
    // Lower range table line.
    let prefix_length = reader.read_bits(prefix_size_bits)? as u8;
    lines.push(Line::range(lowest_value - 1, prefix_length, 32, 0, true));
    // Upper range table line.
    let prefix_length = reader.read_bits(prefix_size_bits)? as u8;
    lines.push(Line::range(highest_value, prefix_length, 32, 0, false));

    if flags & 1 != 0 {
        // Out-of-band table line.
        let prefix_length = reader.read_bits(prefix_size_bits)? as u8;
        lines.push(Line::oob(prefix_length, 0));
    }

    HuffmanTable::new(lines, false)
}

/// Return the `index`-th custom table among the referred-to segments
/// (reference `getCustomHuffmanTable`).
///
/// # Errors
/// When no such table exists among the referred-to segments.
pub fn get_custom_huffman_table<'t>(
    index: usize,
    referred_to: &[u32],
    custom_tables: &'t std::collections::HashMap<u32, HuffmanTable>,
) -> Result<&'t HuffmanTable> {
    let mut current_index = 0;
    for seg in referred_to {
        if let Some(table) = custom_tables.get(seg) {
            if index == current_index {
                return Ok(table);
            }
            current_index += 1;
        }
    }
    Err(PdfError::filter_error(
        0,
        "JBIG2: can't find custom Huffman table",
    ))
}

/// Big-endian `u32` at `pos` (mirrors `readUint32` on a `Uint8Array`).
pub(crate) fn read_u32(data: &[u8], pos: usize) -> u32 {
    let b = |i: usize| -> u32 { u32::from(*data.get(i).unwrap_or(&0)) };
    (b(pos) << 24) | (b(pos + 1) << 16) | (b(pos + 2) << 8) | b(pos + 3)
}

/// Big-endian `u16` at `pos` (mirrors `readUint16` on a `Uint8Array`).
pub(crate) fn read_u16(data: &[u8], pos: usize) -> u16 {
    let b = |i: usize| -> u16 { u16::from(*data.get(i).unwrap_or(&0)) };
    (b(pos) << 8) | b(pos + 1)
}
