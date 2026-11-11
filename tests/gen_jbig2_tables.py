#!/usr/bin/env python3
"""Generate src/parser/jbig2/tables.rs from tests/jbig2-reference.js.

Extracts the Annex B.5 standard Huffman tables (B.1-B.15, `getStandardTable`)
from the pdf.js v3.11.174 reference and emits them as Rust const data. Line
format follows T.88 Annex B: (rangeLow, prefixLength, rangeLength, prefixCode)
plus flags; OOB lines carry only (prefixLength, prefixCode).
"""
import os, re, sys

SRC = os.path.join(os.path.dirname(__file__), "jbig2-reference.js")
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(__file__), "..", "src", "parser", "jbig2", "tables.rs")

src = open(SRC).read()
m = re.search(r"function getStandardTable\(number\) \{.*?class Reader", src, re.S)
body = m.group(0)

tables = {}
for case, tbl in re.findall(r"case (\d+):\s*\n\s*lines = \[(.*?)\n\s*\];", body, re.S):
    lines = []
    for tok in re.findall(r"\[([^\[\]]+)\]", tbl):
        parts = [p.strip() for p in tok.split(",")]
        parts = [p for p in parts if p]
        lines.append(parts)
    tables[int(case)] = lines

assert sorted(tables) == list(range(1, 16)), sorted(tables)

def val(tok):
    tok = tok.strip()
    if tok.startswith("0x"):
        return int(tok, 16)
    return int(tok)

out = ["//! Standard Huffman tables B.1-B.15 (ITU-T T.88 Annex B.5), generated",
       "//! from `tests/jbig2-reference.js` (pdf.js v3.11.174) by",
       "//! `tests/gen_jbig2_tables.py`. Do not edit by hand.",
       "",
       "/// One prefix-code line: `(rangeLow, prefixLength, rangeLength,",
       "/// prefixCode)`; OOB lines use only `(prefixLength, prefixCode)` and",
       "/// carry the `oob` flag, \"lower\" range lines the `lower` flag.",
       "#[derive(Debug, Clone, Copy)]",
       "pub struct HuffmanLine {",
       "    pub range_low: i32,",
       "    pub prefix_length: u8,",
       "    pub range_length: u8,",
       "    pub prefix_code: u32,",
       "    pub lower: bool,",
       "    pub oob: bool,",
       "}",
       ""]
for n in range(1, 16):
    out.append(f"/// Standard table B.{n}.")
    out.append("#[rustfmt::skip] // generated data, keep compact")
    out.append(f"pub static TABLE_B{n}: [HuffmanLine; {len(tables[n])}] = [")
    for p in tables[n]:
        if len(p) == 2:
            out.append(f"    HuffmanLine {{ range_low: 0, prefix_length: {val(p[0])}, range_length: 0, prefix_code: {val(p[1])}, lower: false, oob: true }},")
        else:
            lower = "true" if any("lower" in x for x in p) else "false"
            out.append(f"    HuffmanLine {{ range_low: {val(p[0])}, prefix_length: {val(p[1])}, range_length: {val(p[2])}, prefix_code: {val(p[3])}, lower: {lower}, oob: false }},")
    out.append("];")
out.append("")
out.append("/// All 15 standard tables indexed by table number minus one.")
out.append("pub static STANDARD_TABLES: [&[HuffmanLine]; 15] = [")
for n in range(1, 16):
    out.append(f"    &TABLE_B{n},")
out.append("];")
open(OUT, "w").write("\n".join(out))
print(f"wrote {OUT}: 15 tables, lines per table:",
      [len(tables[n]) for n in range(1, 16)])
