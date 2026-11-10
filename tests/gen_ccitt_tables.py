#!/usr/bin/env python3
"""Extract CCITT Huffman tables from pdf.js ccitt.js -> Rust const arrays."""
import os, re, sys

SRC = os.path.join(os.path.dirname(__file__), "ccitt-reference.js")
OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/opencode/ccitt_tables.rs"
NAMES = ["twoDimTable", "whiteTable1", "whiteTable2", "blackTable1", "blackTable2", "blackTable3"]

VALUES = {"ccittEOL": -2, "ccittEOF": -1, "twoDimPass": 0, "twoDimHoriz": 1, "twoDimVert0": 2,
          "twoDimVertR1": 3, "twoDimVertL1": 4, "twoDimVertR2": 5, "twoDimVertL2": 6,
          "twoDimVertR3": 7, "twoDimVertL3": 8}

def to_int(tok):
    if tok.lstrip("-").isdigit():
        return int(tok)
    return VALUES[tok]

src = open(SRC).read()
entries = {}
for name in NAMES:
    m = re.search(r"const %s = \[(.*?)\n\];" % name, src, re.S)
    if not m:
        sys.exit("table %s not found" % name)
    body = m.group(1)
    pairs = re.findall(r"\[\s*([^,\[\]]+)\s*,\s*([^,\[\]]+)\s*\]", body)
    try:
        entries[name] = [(to_int(a.strip()), to_int(b.strip())) for a, b in pairs]
    except KeyError as e:
        sys.exit("%s: unknown symbol %s" % (name, e))

# sanity: expected sizes from pdf.js prefix-table structure
expected = {"twoDimTable": 128, "whiteTable1": 32, "whiteTable2": 512,
            "blackTable1": 128, "blackTable2": 192, "blackTable3": 64}
for name in NAMES:
    n = len(entries[name])
    status = "OK" if n == expected[name] else "MISMATCH (exp %d)" % expected[name]
    print(f"{name}: {n} entries {status}")

RUST_NAME = {t: t.upper() for t in NAMES}

def pack(pairs, per_line=12):
    lines, i = [], 0
    while i < len(pairs):
        chunk = ",".join(f"({a}, {b})" for a, b in pairs[i:i+per_line])
        lines.append("    " + chunk + ",")
        i += per_line
    return "\n".join(lines)

doc = {
    "twoDimTable": "7-bit prefix table: horizontal/vertical/pass mode codes (§4.2 of T.6).",
    "whiteTable1": "12-bit white-run prefix (7-bit index); extended white codes 64..1728.",
    "whiteTable2": "12-bit white-run prefix (9-bit index); extended white codes 64..1728.",
    "blackTable1": "13-bit black-run prefix (7-bit index); extended black codes 64..1728.",
    "blackTable2": "13-bit black-run prefix (8-bit index); extended black codes 64..1728.",
    "blackTable3": "13-bit black-run prefix (6-bit index); extended black codes 64..1728.",
}

with open(OUT, "w") as f:
    f.write("//! CCITT run-length coding tables (T.4/T.6), extracted from pdf.js\n"
            "//! v3.11.174 src/core/ccitt.js (Apache 2.0; original XPDF port by\n"
            "//! Glyph & Cog). Do not hand-edit — regenerate with\n"
            "//! `tests/gen_ccitt_tables.py`.\n"
            "//!\n"
            "//! Entries are `(consume_bits, value)`, where `value` is a run length\n"
            "//! (0..2560) or a negative marker: `-1` invalid/padding, `-2` EOL.\n"
            "//! Indexing matches pdf.js exactly.\n\n"
            "/// Table entry: bits to consume, run length or negative marker.\n"
            "pub type RunCode = (i16, i16);\n\n")
    for name in NAMES:
        pairs = entries[name]
        f.write(f"/// {doc[name]}\n"
                f"pub const {RUST_NAME[name]}: [RunCode; {len(pairs)}] = [\n")
        f.write(pack(pairs))
        f.write("];\n\n")

print("wrote", OUT)
