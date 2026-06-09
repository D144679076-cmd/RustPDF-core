//! PDF Page Labels (ISO 32000-1 §12.4.2).
//!
//! Parses the `/PageLabels` number tree from the document catalog to provide
//! custom page numbering (Roman numerals, alphabetic, with prefixes).

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

const MAX_ITERATIONS: u32 = 50_000;

/// Page label numbering style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageLabelStyle {
    /// Decimal Arabic numerals (1, 2, 3, ...).
    Decimal,
    /// Uppercase Roman numerals (I, II, III, ...).
    UpperRoman,
    /// Lowercase Roman numerals (i, ii, iii, ...).
    LowerRoman,
    /// Uppercase letters (A, B, ..., Z, AA, AB, ...).
    UpperAlpha,
    /// Lowercase letters (a, b, ..., z, aa, ab, ...).
    LowerAlpha,
    /// No numeric portion — prefix only.
    None,
}

/// A single page label range entry.
#[derive(Debug, Clone)]
pub struct PageLabel {
    /// Numbering style for this range.
    pub style: PageLabelStyle,
    /// Label prefix prepended to the numeric portion.
    pub prefix: String,
    /// Starting value for the numeric portion (default 1).
    pub start: u32,
}

/// Parsed page label tree mapping page indices to label formats.
#[derive(Debug, Clone)]
pub struct PageLabelTree {
    /// Sorted (page_index, label) pairs from the number tree.
    ranges: Vec<(usize, PageLabel)>,
}

impl PageLabelTree {
    /// Parse the page label tree from the catalog's `/PageLabels` entry.
    ///
    /// Returns `Ok(None)` if no `/PageLabels` is present in the catalog.
    pub fn from_catalog(doc: &PdfDocument, catalog_dict: &PdfDict) -> Result<Option<Self>> {
        let labels_ref = match catalog_dict.get("PageLabels") {
            Some(obj) => obj.clone(),
            None => return Ok(None),
        };

        let labels_obj = doc.resolve(&labels_ref)?;
        let labels_dict = match labels_obj {
            PdfObject::Dictionary(d) => d,
            _ => return Ok(None),
        };

        let mut ranges = Vec::new();
        walk_number_tree(doc, &labels_dict, &mut ranges)?;
        ranges.sort_by_key(|(idx, _)| *idx);

        Ok(Some(PageLabelTree { ranges }))
    }

    /// Get the display label string for a zero-based page index.
    ///
    /// Returns a decimal label starting from "1" if no label range covers the page.
    pub fn label_for_page(&self, page_index: usize) -> String {
        let label = self.find_governing_label(page_index);

        match label {
            Some((range_start, pl)) => {
                let offset = (page_index - range_start) as u32;
                let number = pl.start + offset;
                let numeric_part = format_number(number, pl.style);
                format!("{}{}", pl.prefix, numeric_part)
            }
            None => format!("{}", page_index + 1),
        }
    }

    /// Find the governing label entry for a page index (binary search).
    fn find_governing_label(&self, page_index: usize) -> Option<(usize, &PageLabel)> {
        if self.ranges.is_empty() {
            return None;
        }

        let pos = match self
            .ranges
            .binary_search_by_key(&page_index, |(idx, _)| *idx)
        {
            Ok(i) => i,
            Err(i) => {
                if i == 0 {
                    return None;
                }
                i - 1
            }
        };

        let (start, label) = &self.ranges[pos];
        Some((*start, label))
    }
}

/// Walk a number tree (integer-keyed) and collect (key, PageLabel) pairs.
fn walk_number_tree(
    doc: &PdfDocument,
    node: &PdfDict,
    out: &mut Vec<(usize, PageLabel)>,
) -> Result<()> {
    let mut stack: Vec<PdfDict> = vec![node.clone()];
    let mut iterations = 0u32;

    while let Some(current) = stack.pop() {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            return Err(PdfError::invalid_token(
                0,
                "page labels number tree exceeded iteration limit",
            ));
        }

        // Leaf: /Nums array of (integer, dict) pairs
        if let Some(PdfObject::Array(nums)) = current.get("Nums") {
            let mut i = 0;
            while i + 1 < nums.len() {
                let page_idx = match &nums[i] {
                    PdfObject::Integer(n) => *n as usize,
                    _ => {
                        i += 2;
                        continue;
                    }
                };

                let label_obj = doc.resolve(&nums[i + 1])?;
                let label = parse_page_label(&label_obj);
                out.push((page_idx, label));
                i += 2;
            }
        }

        // Intermediate: /Kids array
        if let Some(PdfObject::Array(kids)) = current.get("Kids") {
            for kid in kids.iter().rev() {
                let kid_obj = doc.resolve(kid)?;
                if let PdfObject::Dictionary(d) = kid_obj {
                    stack.push(d);
                }
            }
        }
    }

    Ok(())
}

/// Parse a page label dictionary into a PageLabel struct.
fn parse_page_label(obj: &PdfObject) -> PageLabel {
    let dict = match obj {
        PdfObject::Dictionary(d) => d,
        _ => {
            return PageLabel {
                style: PageLabelStyle::Decimal,
                prefix: String::new(),
                start: 1,
            }
        }
    };

    let style = match dict.get("S").and_then(|s| s.as_name()) {
        Some("D") => PageLabelStyle::Decimal,
        Some("R") => PageLabelStyle::UpperRoman,
        Some("r") => PageLabelStyle::LowerRoman,
        Some("A") => PageLabelStyle::UpperAlpha,
        Some("a") => PageLabelStyle::LowerAlpha,
        _ => PageLabelStyle::None,
    };

    let prefix = match dict.get("P") {
        Some(PdfObject::String(bytes)) => {
            crate::document::text_string::decode_pdf_text_string(bytes)
        }
        _ => String::new(),
    };

    let start = match dict.get("St") {
        Some(PdfObject::Integer(n)) if *n >= 1 => *n as u32,
        _ => 1,
    };

    PageLabel {
        style,
        prefix,
        start,
    }
}

/// Format a number according to the given page label style.
fn format_number(n: u32, style: PageLabelStyle) -> String {
    match style {
        PageLabelStyle::Decimal => n.to_string(),
        PageLabelStyle::UpperRoman => to_roman(n, true),
        PageLabelStyle::LowerRoman => to_roman(n, false),
        PageLabelStyle::UpperAlpha => to_alpha(n, true),
        PageLabelStyle::LowerAlpha => to_alpha(n, false),
        PageLabelStyle::None => String::new(),
    }
}

/// Convert a number to Roman numeral representation (handles 1–4999).
fn to_roman(mut n: u32, upper: bool) -> String {
    const VALUES: &[(u32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];

    if n == 0 || n > 4999 {
        return n.to_string();
    }

    let mut result = String::new();
    for &(value, numeral) in VALUES {
        while n >= value {
            result.push_str(numeral);
            n -= value;
        }
    }

    if upper {
        result
    } else {
        result.to_lowercase()
    }
}

/// Convert a number to alphabetic representation (1=A, 26=Z, 27=AA, ...).
fn to_alpha(n: u32, upper: bool) -> String {
    if n == 0 {
        return String::new();
    }

    let mut result = String::new();
    let mut remaining = n - 1;

    loop {
        let ch = (remaining % 26) as u8;
        let letter = if upper { b'A' + ch } else { b'a' + ch };
        result.insert(0, letter as char);

        if remaining < 26 {
            break;
        }
        remaining = remaining / 26 - 1;
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_roman_upper() {
        assert_eq!(to_roman(1, true), "I");
        assert_eq!(to_roman(4, true), "IV");
        assert_eq!(to_roman(9, true), "IX");
        assert_eq!(to_roman(14, true), "XIV");
        assert_eq!(to_roman(42, true), "XLII");
        assert_eq!(to_roman(1999, true), "MCMXCIX");
    }

    #[test]
    fn test_to_roman_lower() {
        assert_eq!(to_roman(1, false), "i");
        assert_eq!(to_roman(3, false), "iii");
        assert_eq!(to_roman(4, false), "iv");
    }

    #[test]
    fn test_to_alpha() {
        assert_eq!(to_alpha(1, true), "A");
        assert_eq!(to_alpha(26, true), "Z");
        assert_eq!(to_alpha(27, true), "AA");
        assert_eq!(to_alpha(28, true), "AB");
        assert_eq!(to_alpha(52, true), "AZ");
        assert_eq!(to_alpha(53, true), "BA");
    }

    #[test]
    fn test_to_alpha_lower() {
        assert_eq!(to_alpha(1, false), "a");
        assert_eq!(to_alpha(26, false), "z");
        assert_eq!(to_alpha(27, false), "aa");
    }

    #[test]
    fn test_format_number_none_style() {
        assert_eq!(format_number(5, PageLabelStyle::None), "");
    }

    #[test]
    fn test_label_for_page_decimal() {
        let tree = PageLabelTree {
            ranges: vec![(
                0,
                PageLabel {
                    style: PageLabelStyle::Decimal,
                    prefix: String::new(),
                    start: 1,
                },
            )],
        };
        assert_eq!(tree.label_for_page(0), "1");
        assert_eq!(tree.label_for_page(4), "5");
    }

    #[test]
    fn test_label_for_page_roman_then_decimal() {
        let tree = PageLabelTree {
            ranges: vec![
                (
                    0,
                    PageLabel {
                        style: PageLabelStyle::LowerRoman,
                        prefix: String::new(),
                        start: 1,
                    },
                ),
                (
                    3,
                    PageLabel {
                        style: PageLabelStyle::Decimal,
                        prefix: String::new(),
                        start: 1,
                    },
                ),
            ],
        };
        assert_eq!(tree.label_for_page(0), "i");
        assert_eq!(tree.label_for_page(1), "ii");
        assert_eq!(tree.label_for_page(2), "iii");
        assert_eq!(tree.label_for_page(3), "1");
        assert_eq!(tree.label_for_page(4), "2");
    }

    #[test]
    fn test_label_for_page_with_prefix() {
        let tree = PageLabelTree {
            ranges: vec![(
                0,
                PageLabel {
                    style: PageLabelStyle::Decimal,
                    prefix: "Ch-".to_string(),
                    start: 1,
                },
            )],
        };
        assert_eq!(tree.label_for_page(0), "Ch-1");
        assert_eq!(tree.label_for_page(2), "Ch-3");
    }

    #[test]
    fn test_label_for_page_empty_tree() {
        let tree = PageLabelTree { ranges: vec![] };
        assert_eq!(tree.label_for_page(0), "1");
        assert_eq!(tree.label_for_page(9), "10");
    }
}
