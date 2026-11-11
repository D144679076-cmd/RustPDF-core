//! Standard Huffman tables B.1-B.15 (ITU-T T.88 Annex B.5), generated
//! from `tests/jbig2-reference.js` (pdf.js v3.11.174) by
//! `tests/gen_jbig2_tables.py`. Do not edit by hand.

/// One prefix-code line: `(rangeLow, prefixLength, rangeLength,
/// prefixCode)`; OOB lines use only `(prefixLength, prefixCode)` and
/// carry the `oob` flag, "lower" range lines the `lower` flag.
#[derive(Debug, Clone, Copy)]
pub struct HuffmanLine {
    pub range_low: i32,
    pub prefix_length: u8,
    pub range_length: u8,
    pub prefix_code: u32,
    pub lower: bool,
    pub oob: bool,
}

/// Standard table B.1.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B1: [HuffmanLine; 4] = [
    HuffmanLine { range_low: 0, prefix_length: 1, range_length: 4, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 16, prefix_length: 2, range_length: 8, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 272, prefix_length: 3, range_length: 16, prefix_code: 6, lower: false, oob: false },
    HuffmanLine { range_low: 65808, prefix_length: 3, range_length: 32, prefix_code: 7, lower: false, oob: false },
];
/// Standard table B.2.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B2: [HuffmanLine; 7] = [
    HuffmanLine { range_low: 0, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 1, prefix_length: 2, range_length: 0, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 3, range_length: 0, prefix_code: 6, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 4, range_length: 3, prefix_code: 14, lower: false, oob: false },
    HuffmanLine { range_low: 11, prefix_length: 5, range_length: 6, prefix_code: 30, lower: false, oob: false },
    HuffmanLine { range_low: 75, prefix_length: 6, range_length: 32, prefix_code: 62, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 6, range_length: 0, prefix_code: 63, lower: false, oob: true },
];
/// Standard table B.3.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B3: [HuffmanLine; 9] = [
    HuffmanLine { range_low: -256, prefix_length: 8, range_length: 8, prefix_code: 254, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 1, prefix_length: 2, range_length: 0, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 3, range_length: 0, prefix_code: 6, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 4, range_length: 3, prefix_code: 14, lower: false, oob: false },
    HuffmanLine { range_low: 11, prefix_length: 5, range_length: 6, prefix_code: 30, lower: false, oob: false },
    HuffmanLine { range_low: -257, prefix_length: 8, range_length: 32, prefix_code: 255, lower: true, oob: false },
    HuffmanLine { range_low: 75, prefix_length: 7, range_length: 32, prefix_code: 126, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 6, range_length: 0, prefix_code: 62, lower: false, oob: true },
];
/// Standard table B.4.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B4: [HuffmanLine; 6] = [
    HuffmanLine { range_low: 1, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 2, range_length: 0, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 3, range_length: 0, prefix_code: 6, lower: false, oob: false },
    HuffmanLine { range_low: 4, prefix_length: 4, range_length: 3, prefix_code: 14, lower: false, oob: false },
    HuffmanLine { range_low: 12, prefix_length: 5, range_length: 6, prefix_code: 30, lower: false, oob: false },
    HuffmanLine { range_low: 76, prefix_length: 5, range_length: 32, prefix_code: 31, lower: false, oob: false },
];
/// Standard table B.5.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B5: [HuffmanLine; 8] = [
    HuffmanLine { range_low: -255, prefix_length: 7, range_length: 8, prefix_code: 126, lower: false, oob: false },
    HuffmanLine { range_low: 1, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 2, range_length: 0, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 3, range_length: 0, prefix_code: 6, lower: false, oob: false },
    HuffmanLine { range_low: 4, prefix_length: 4, range_length: 3, prefix_code: 14, lower: false, oob: false },
    HuffmanLine { range_low: 12, prefix_length: 5, range_length: 6, prefix_code: 30, lower: false, oob: false },
    HuffmanLine { range_low: -256, prefix_length: 7, range_length: 32, prefix_code: 127, lower: true, oob: false },
    HuffmanLine { range_low: 76, prefix_length: 6, range_length: 32, prefix_code: 62, lower: false, oob: false },
];
/// Standard table B.6.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B6: [HuffmanLine; 14] = [
    HuffmanLine { range_low: -2048, prefix_length: 5, range_length: 10, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: -1024, prefix_length: 4, range_length: 9, prefix_code: 8, lower: false, oob: false },
    HuffmanLine { range_low: -512, prefix_length: 4, range_length: 8, prefix_code: 9, lower: false, oob: false },
    HuffmanLine { range_low: -256, prefix_length: 4, range_length: 7, prefix_code: 10, lower: false, oob: false },
    HuffmanLine { range_low: -128, prefix_length: 5, range_length: 6, prefix_code: 29, lower: false, oob: false },
    HuffmanLine { range_low: -64, prefix_length: 5, range_length: 5, prefix_code: 30, lower: false, oob: false },
    HuffmanLine { range_low: -32, prefix_length: 4, range_length: 5, prefix_code: 11, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 2, range_length: 7, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 128, prefix_length: 3, range_length: 7, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 256, prefix_length: 3, range_length: 8, prefix_code: 3, lower: false, oob: false },
    HuffmanLine { range_low: 512, prefix_length: 4, range_length: 9, prefix_code: 12, lower: false, oob: false },
    HuffmanLine { range_low: 1024, prefix_length: 4, range_length: 10, prefix_code: 13, lower: false, oob: false },
    HuffmanLine { range_low: -2049, prefix_length: 6, range_length: 32, prefix_code: 62, lower: true, oob: false },
    HuffmanLine { range_low: 2048, prefix_length: 6, range_length: 32, prefix_code: 63, lower: false, oob: false },
];
/// Standard table B.7.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B7: [HuffmanLine; 15] = [
    HuffmanLine { range_low: -1024, prefix_length: 4, range_length: 9, prefix_code: 8, lower: false, oob: false },
    HuffmanLine { range_low: -512, prefix_length: 3, range_length: 8, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: -256, prefix_length: 4, range_length: 7, prefix_code: 9, lower: false, oob: false },
    HuffmanLine { range_low: -128, prefix_length: 5, range_length: 6, prefix_code: 26, lower: false, oob: false },
    HuffmanLine { range_low: -64, prefix_length: 5, range_length: 5, prefix_code: 27, lower: false, oob: false },
    HuffmanLine { range_low: -32, prefix_length: 4, range_length: 5, prefix_code: 10, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 4, range_length: 5, prefix_code: 11, lower: false, oob: false },
    HuffmanLine { range_low: 32, prefix_length: 5, range_length: 5, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: 64, prefix_length: 5, range_length: 6, prefix_code: 29, lower: false, oob: false },
    HuffmanLine { range_low: 128, prefix_length: 4, range_length: 7, prefix_code: 12, lower: false, oob: false },
    HuffmanLine { range_low: 256, prefix_length: 3, range_length: 8, prefix_code: 1, lower: false, oob: false },
    HuffmanLine { range_low: 512, prefix_length: 3, range_length: 9, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 1024, prefix_length: 3, range_length: 10, prefix_code: 3, lower: false, oob: false },
    HuffmanLine { range_low: -1025, prefix_length: 5, range_length: 32, prefix_code: 30, lower: true, oob: false },
    HuffmanLine { range_low: 2048, prefix_length: 5, range_length: 32, prefix_code: 31, lower: false, oob: false },
];
/// Standard table B.8.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B8: [HuffmanLine; 21] = [
    HuffmanLine { range_low: -15, prefix_length: 8, range_length: 3, prefix_code: 252, lower: false, oob: false },
    HuffmanLine { range_low: -7, prefix_length: 9, range_length: 1, prefix_code: 508, lower: false, oob: false },
    HuffmanLine { range_low: -5, prefix_length: 8, range_length: 1, prefix_code: 253, lower: false, oob: false },
    HuffmanLine { range_low: -3, prefix_length: 9, range_length: 0, prefix_code: 509, lower: false, oob: false },
    HuffmanLine { range_low: -2, prefix_length: 7, range_length: 0, prefix_code: 124, lower: false, oob: false },
    HuffmanLine { range_low: -1, prefix_length: 4, range_length: 0, prefix_code: 10, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 2, range_length: 1, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 5, range_length: 0, prefix_code: 26, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 6, range_length: 0, prefix_code: 58, lower: false, oob: false },
    HuffmanLine { range_low: 4, prefix_length: 3, range_length: 4, prefix_code: 4, lower: false, oob: false },
    HuffmanLine { range_low: 20, prefix_length: 6, range_length: 1, prefix_code: 59, lower: false, oob: false },
    HuffmanLine { range_low: 22, prefix_length: 4, range_length: 4, prefix_code: 11, lower: false, oob: false },
    HuffmanLine { range_low: 38, prefix_length: 4, range_length: 5, prefix_code: 12, lower: false, oob: false },
    HuffmanLine { range_low: 70, prefix_length: 5, range_length: 6, prefix_code: 27, lower: false, oob: false },
    HuffmanLine { range_low: 134, prefix_length: 5, range_length: 7, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: 262, prefix_length: 6, range_length: 7, prefix_code: 60, lower: false, oob: false },
    HuffmanLine { range_low: 390, prefix_length: 7, range_length: 8, prefix_code: 125, lower: false, oob: false },
    HuffmanLine { range_low: 646, prefix_length: 6, range_length: 10, prefix_code: 61, lower: false, oob: false },
    HuffmanLine { range_low: -16, prefix_length: 9, range_length: 32, prefix_code: 510, lower: true, oob: false },
    HuffmanLine { range_low: 1670, prefix_length: 9, range_length: 32, prefix_code: 511, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 2, range_length: 0, prefix_code: 1, lower: false, oob: true },
];
/// Standard table B.9.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B9: [HuffmanLine; 22] = [
    HuffmanLine { range_low: -31, prefix_length: 8, range_length: 4, prefix_code: 252, lower: false, oob: false },
    HuffmanLine { range_low: -15, prefix_length: 9, range_length: 2, prefix_code: 508, lower: false, oob: false },
    HuffmanLine { range_low: -11, prefix_length: 8, range_length: 2, prefix_code: 253, lower: false, oob: false },
    HuffmanLine { range_low: -7, prefix_length: 9, range_length: 1, prefix_code: 509, lower: false, oob: false },
    HuffmanLine { range_low: -5, prefix_length: 7, range_length: 1, prefix_code: 124, lower: false, oob: false },
    HuffmanLine { range_low: -3, prefix_length: 4, range_length: 1, prefix_code: 10, lower: false, oob: false },
    HuffmanLine { range_low: -1, prefix_length: 3, range_length: 1, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 1, prefix_length: 3, range_length: 1, prefix_code: 3, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 5, range_length: 1, prefix_code: 26, lower: false, oob: false },
    HuffmanLine { range_low: 5, prefix_length: 6, range_length: 1, prefix_code: 58, lower: false, oob: false },
    HuffmanLine { range_low: 7, prefix_length: 3, range_length: 5, prefix_code: 4, lower: false, oob: false },
    HuffmanLine { range_low: 39, prefix_length: 6, range_length: 2, prefix_code: 59, lower: false, oob: false },
    HuffmanLine { range_low: 43, prefix_length: 4, range_length: 5, prefix_code: 11, lower: false, oob: false },
    HuffmanLine { range_low: 75, prefix_length: 4, range_length: 6, prefix_code: 12, lower: false, oob: false },
    HuffmanLine { range_low: 139, prefix_length: 5, range_length: 7, prefix_code: 27, lower: false, oob: false },
    HuffmanLine { range_low: 267, prefix_length: 5, range_length: 8, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: 523, prefix_length: 6, range_length: 8, prefix_code: 60, lower: false, oob: false },
    HuffmanLine { range_low: 779, prefix_length: 7, range_length: 9, prefix_code: 125, lower: false, oob: false },
    HuffmanLine { range_low: 1291, prefix_length: 6, range_length: 11, prefix_code: 61, lower: false, oob: false },
    HuffmanLine { range_low: -32, prefix_length: 9, range_length: 32, prefix_code: 510, lower: true, oob: false },
    HuffmanLine { range_low: 3339, prefix_length: 9, range_length: 32, prefix_code: 511, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 2, range_length: 0, prefix_code: 0, lower: false, oob: true },
];
/// Standard table B.10.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B10: [HuffmanLine; 21] = [
    HuffmanLine { range_low: -21, prefix_length: 7, range_length: 4, prefix_code: 122, lower: false, oob: false },
    HuffmanLine { range_low: -5, prefix_length: 8, range_length: 0, prefix_code: 252, lower: false, oob: false },
    HuffmanLine { range_low: -4, prefix_length: 7, range_length: 0, prefix_code: 123, lower: false, oob: false },
    HuffmanLine { range_low: -3, prefix_length: 5, range_length: 0, prefix_code: 24, lower: false, oob: false },
    HuffmanLine { range_low: -2, prefix_length: 2, range_length: 2, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 5, range_length: 0, prefix_code: 25, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 6, range_length: 0, prefix_code: 54, lower: false, oob: false },
    HuffmanLine { range_low: 4, prefix_length: 7, range_length: 0, prefix_code: 124, lower: false, oob: false },
    HuffmanLine { range_low: 5, prefix_length: 8, range_length: 0, prefix_code: 253, lower: false, oob: false },
    HuffmanLine { range_low: 6, prefix_length: 2, range_length: 6, prefix_code: 1, lower: false, oob: false },
    HuffmanLine { range_low: 70, prefix_length: 5, range_length: 5, prefix_code: 26, lower: false, oob: false },
    HuffmanLine { range_low: 102, prefix_length: 6, range_length: 5, prefix_code: 55, lower: false, oob: false },
    HuffmanLine { range_low: 134, prefix_length: 6, range_length: 6, prefix_code: 56, lower: false, oob: false },
    HuffmanLine { range_low: 198, prefix_length: 6, range_length: 7, prefix_code: 57, lower: false, oob: false },
    HuffmanLine { range_low: 326, prefix_length: 6, range_length: 8, prefix_code: 58, lower: false, oob: false },
    HuffmanLine { range_low: 582, prefix_length: 6, range_length: 9, prefix_code: 59, lower: false, oob: false },
    HuffmanLine { range_low: 1094, prefix_length: 6, range_length: 10, prefix_code: 60, lower: false, oob: false },
    HuffmanLine { range_low: 2118, prefix_length: 7, range_length: 11, prefix_code: 125, lower: false, oob: false },
    HuffmanLine { range_low: -22, prefix_length: 8, range_length: 32, prefix_code: 254, lower: true, oob: false },
    HuffmanLine { range_low: 4166, prefix_length: 8, range_length: 32, prefix_code: 255, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 2, range_length: 0, prefix_code: 2, lower: false, oob: true },
];
/// Standard table B.11.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B11: [HuffmanLine; 13] = [
    HuffmanLine { range_low: 1, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 2, range_length: 1, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 4, prefix_length: 4, range_length: 0, prefix_code: 12, lower: false, oob: false },
    HuffmanLine { range_low: 5, prefix_length: 4, range_length: 1, prefix_code: 13, lower: false, oob: false },
    HuffmanLine { range_low: 7, prefix_length: 5, range_length: 1, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: 9, prefix_length: 5, range_length: 2, prefix_code: 29, lower: false, oob: false },
    HuffmanLine { range_low: 13, prefix_length: 6, range_length: 2, prefix_code: 60, lower: false, oob: false },
    HuffmanLine { range_low: 17, prefix_length: 7, range_length: 2, prefix_code: 122, lower: false, oob: false },
    HuffmanLine { range_low: 21, prefix_length: 7, range_length: 3, prefix_code: 123, lower: false, oob: false },
    HuffmanLine { range_low: 29, prefix_length: 7, range_length: 4, prefix_code: 124, lower: false, oob: false },
    HuffmanLine { range_low: 45, prefix_length: 7, range_length: 5, prefix_code: 125, lower: false, oob: false },
    HuffmanLine { range_low: 77, prefix_length: 7, range_length: 6, prefix_code: 126, lower: false, oob: false },
    HuffmanLine { range_low: 141, prefix_length: 7, range_length: 32, prefix_code: 127, lower: false, oob: false },
];
/// Standard table B.12.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B12: [HuffmanLine; 13] = [
    HuffmanLine { range_low: 1, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 2, range_length: 0, prefix_code: 2, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 3, range_length: 1, prefix_code: 6, lower: false, oob: false },
    HuffmanLine { range_low: 5, prefix_length: 5, range_length: 0, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: 6, prefix_length: 5, range_length: 1, prefix_code: 29, lower: false, oob: false },
    HuffmanLine { range_low: 8, prefix_length: 6, range_length: 1, prefix_code: 60, lower: false, oob: false },
    HuffmanLine { range_low: 10, prefix_length: 7, range_length: 0, prefix_code: 122, lower: false, oob: false },
    HuffmanLine { range_low: 11, prefix_length: 7, range_length: 1, prefix_code: 123, lower: false, oob: false },
    HuffmanLine { range_low: 13, prefix_length: 7, range_length: 2, prefix_code: 124, lower: false, oob: false },
    HuffmanLine { range_low: 17, prefix_length: 7, range_length: 3, prefix_code: 125, lower: false, oob: false },
    HuffmanLine { range_low: 25, prefix_length: 7, range_length: 4, prefix_code: 126, lower: false, oob: false },
    HuffmanLine { range_low: 41, prefix_length: 8, range_length: 5, prefix_code: 254, lower: false, oob: false },
    HuffmanLine { range_low: 73, prefix_length: 8, range_length: 32, prefix_code: 255, lower: false, oob: false },
];
/// Standard table B.13.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B13: [HuffmanLine; 13] = [
    HuffmanLine { range_low: 1, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 3, range_length: 0, prefix_code: 4, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 4, range_length: 0, prefix_code: 12, lower: false, oob: false },
    HuffmanLine { range_low: 4, prefix_length: 5, range_length: 0, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: 5, prefix_length: 4, range_length: 1, prefix_code: 13, lower: false, oob: false },
    HuffmanLine { range_low: 7, prefix_length: 3, range_length: 3, prefix_code: 5, lower: false, oob: false },
    HuffmanLine { range_low: 15, prefix_length: 6, range_length: 1, prefix_code: 58, lower: false, oob: false },
    HuffmanLine { range_low: 17, prefix_length: 6, range_length: 2, prefix_code: 59, lower: false, oob: false },
    HuffmanLine { range_low: 21, prefix_length: 6, range_length: 3, prefix_code: 60, lower: false, oob: false },
    HuffmanLine { range_low: 29, prefix_length: 6, range_length: 4, prefix_code: 61, lower: false, oob: false },
    HuffmanLine { range_low: 45, prefix_length: 6, range_length: 5, prefix_code: 62, lower: false, oob: false },
    HuffmanLine { range_low: 77, prefix_length: 7, range_length: 6, prefix_code: 126, lower: false, oob: false },
    HuffmanLine { range_low: 141, prefix_length: 7, range_length: 32, prefix_code: 127, lower: false, oob: false },
];
/// Standard table B.14.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B14: [HuffmanLine; 5] = [
    HuffmanLine { range_low: -2, prefix_length: 3, range_length: 0, prefix_code: 4, lower: false, oob: false },
    HuffmanLine { range_low: -1, prefix_length: 3, range_length: 0, prefix_code: 5, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 1, prefix_length: 3, range_length: 0, prefix_code: 6, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 3, range_length: 0, prefix_code: 7, lower: false, oob: false },
];
/// Standard table B.15.
#[rustfmt::skip] // generated data, keep compact
pub static TABLE_B15: [HuffmanLine; 13] = [
    HuffmanLine { range_low: -24, prefix_length: 7, range_length: 4, prefix_code: 124, lower: false, oob: false },
    HuffmanLine { range_low: -8, prefix_length: 6, range_length: 2, prefix_code: 60, lower: false, oob: false },
    HuffmanLine { range_low: -4, prefix_length: 5, range_length: 1, prefix_code: 28, lower: false, oob: false },
    HuffmanLine { range_low: -2, prefix_length: 4, range_length: 0, prefix_code: 12, lower: false, oob: false },
    HuffmanLine { range_low: -1, prefix_length: 3, range_length: 0, prefix_code: 4, lower: false, oob: false },
    HuffmanLine { range_low: 0, prefix_length: 1, range_length: 0, prefix_code: 0, lower: false, oob: false },
    HuffmanLine { range_low: 1, prefix_length: 3, range_length: 0, prefix_code: 5, lower: false, oob: false },
    HuffmanLine { range_low: 2, prefix_length: 4, range_length: 0, prefix_code: 13, lower: false, oob: false },
    HuffmanLine { range_low: 3, prefix_length: 5, range_length: 1, prefix_code: 29, lower: false, oob: false },
    HuffmanLine { range_low: 5, prefix_length: 6, range_length: 2, prefix_code: 61, lower: false, oob: false },
    HuffmanLine { range_low: 9, prefix_length: 7, range_length: 4, prefix_code: 125, lower: false, oob: false },
    HuffmanLine { range_low: -25, prefix_length: 7, range_length: 32, prefix_code: 126, lower: true, oob: false },
    HuffmanLine { range_low: 25, prefix_length: 7, range_length: 32, prefix_code: 127, lower: false, oob: false },
];

/// All 15 standard tables indexed by table number minus one.
pub static STANDARD_TABLES: [&[HuffmanLine]; 15] = [
    &TABLE_B1, &TABLE_B2, &TABLE_B3, &TABLE_B4, &TABLE_B5, &TABLE_B6, &TABLE_B7, &TABLE_B8,
    &TABLE_B9, &TABLE_B10, &TABLE_B11, &TABLE_B12, &TABLE_B13, &TABLE_B14, &TABLE_B15,
];
