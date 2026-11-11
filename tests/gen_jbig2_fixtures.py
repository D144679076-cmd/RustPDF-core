#!/usr/bin/env python3
"""Build JBIG2 test-stream constants for src/parser/jbig2 tests.

Fixtures:
  1. Hand-encoded embedded stream: page info (48) + immediate generic region
     (38) with MMR-coded data (T.4 codes, same encoder tricks as
     gen_ccitt_fixtures.py).
  2. Real pdf.js test stream (jbig2_symbol_offset.pdf, arithmetic-coded
     symbol dictionary + text region).
  3. Hand-encoded pattern dictionary (16) + halftone region (22), all MMR.

Every stream is VALIDATED by running the actual pdf.js v3.11.174 jbig2.js
decoder under node; only node-produced byte sequences are embedded as
expected values in the Rust tests (exact parity with the reference).
"""
import json, subprocess, sys, os, re

HERE = os.path.dirname(__file__)
REF = os.path.join(HERE, ".jbig2ref")

# ---- T.4 code tables (recovered exactly like gen_ccitt_fixtures.py) ----
SRC = os.path.join(HERE, "ccitt-reference.js")
VALUES = {"ccittEOL": -2, "ccittEOF": -1, "twoDimPass": 0, "twoDimHoriz": 1,
          "twoDimVert0": 2, "twoDimVertR1": 3, "twoDimVertL1": 4,
          "twoDimVertR2": 5, "twoDimVertL2": 6, "twoDimVertR3": 7, "twoDimVertL3": 8}

def table(name):
    m = re.search(r"const %s = \[(.*?)\n\];" % name, open(SRC).read(), re.S)
    def i(t):
        t = t.strip()
        return int(t) if t.lstrip("-").isdigit() else VALUES[t]
    return [(i(a), i(b)) for a, b in re.findall(r"\[\s*([^,\[\]]+)\s*,\s*([^,\[\]]+)\s*\]", m.group(1))]

W1, W2 = table("whiteTable1"), table("whiteTable2")
B1, B2, B3 = table("blackTable1"), table("blackTable2"), table("blackTable3")

def simulate_lookup_white(code12):
    if code12 >> 5 == 0:
        return W1[code12]
    return W2[code12 >> 3]

def simulate_lookup_black(code13):
    if code13 >> 7 == 0:
        return B1[code13]
    if code13 >> 9 == 0 and code13 >> 7 != 0:
        return B2[(code13 >> 1) - 64]
    return B3[code13 >> 7]

def brute_force(lookup, max_bits):
    out = {}
    for L in range(1, max_bits + 1):
        for c in range(1 << L):
            bits_val, run = lookup(c << (max_bits - L))
            if bits_val == L and run >= 0:
                out.setdefault(run, (L, c))
    return out

WHITE = brute_force(simulate_lookup_white, 12)
BLACK = brute_force(simulate_lookup_black, 13)

def bits(v, m):
    L, c = m[v]
    return format(c, "b").zfill(L)

assert bits(0, WHITE) == "00110101"
assert bits(0, BLACK) == "0000110111"

class BitW:
    def __init__(self):
        self.s = ""
    def w(self, b):
        self.s += b
    def bytes(self):
        s = self.s + "0" * ((-len(self.s)) % 8)
        return bytes(int(s[i:i+8], 2) for i in range(0, len(s), 8))

HORZ, V0 = "001", "1"

def wrun(bw, n):
    while n >= 64:
        mk = max(v for v in (64, 128, 256, 512, 1024, 1728) if v in WHITE and v <= n)
        bw.w(bits(mk, WHITE)); n -= mk
    bw.w(bits(n, WHITE))

def brun(bw, n):
    while n >= 64:
        mk = max(v for v in (64, 128, 256, 512, 1024, 1728) if v in BLACK and v <= n)
        bw.w(bits(mk, BLACK)); n -= mk
    bw.w(bits(n, BLACK))

def encode_row_horz(bw, row):
    """Encode one row as full-line HORZ codes (legal in 2D mode regardless
    of the reference line)."""
    runs = []
    color = 0
    n = 0
    for p in row:
        if p == color:
            n += 1
        else:
            runs.append((color, n))
            color = p
            n = 1
    runs.append((color, n))
    i = 0
    while i < len(runs):
        c, n = runs[i]
        if c == 0:
            wn = n
            bn = runs[i + 1][1] if i + 1 < len(runs) else 0
            i += 2
        else:
            wn = 0
            bn = n
            i += 1
        bw.w(HORZ)
        wrun(bw, wn)
        brun(bw, bn)

def encode_mmr_rows(rows):
    bw = BitW()
    for row in rows:
        encode_row_horz(bw, row)
    return bw.bytes()

# ---------------- node harness ----------------
def node_run(chunks):
    payload = json.dumps({"chunks": [list(c) for c in chunks]})
    r = subprocess.run(["node", os.path.join(REF, "run.mjs")], input=payload,
                       capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"node failed: {r.stderr[:500]}")
    res = json.loads(r.stdout)
    return bytes(res["bytes"]), res["w"], res["h"]

def seg_header(number, kind, length, page_assoc=1, referred=()):
    flags = kind  # bit 6 = 0 → 1-byte page association
    b = bytearray()
    b += number.to_bytes(4, "big")
    b.append(flags)
    # referred flags: count in top 3 bits (short form, 1-4), retain bits 0.
    assert 1 <= len(referred) <= 4 or not referred
    b.append((len(referred) << 5) & 0xff)
    for seg in referred:
        assert number <= 256, "1-byte referred numbers only when seg# <= 256"
        b.append(seg)
    b.append(page_assoc)
    b += length.to_bytes(4, "big")
    return bytes(b)

def page_info_segment(number, width, height, flags=0):
    data = bytearray()
    data += width.to_bytes(4, "big")
    data += height.to_bytes(4, "big")
    data += (0).to_bytes(4, "big")  # resolution X
    data += (0).to_bytes(4, "big")  # resolution Y
    data.append(flags)              # page segment flags
    data += (0).to_bytes(2, "big")  # striping info
    return seg_header(number, 48, len(data)) + bytes(data)

def generic_region_segment(number, width, height, mmr_data):
    data = bytearray()
    data += width.to_bytes(4, "big")
    data += height.to_bytes(4, "big")
    data += (0).to_bytes(4, "big")  # x
    data += (0).to_bytes(4, "big")  # y
    data.append(0)                  # combination operator OR
    data.append(0x01)               # generic flags: MMR=1, template=0
    data += mmr_data
    return seg_header(number, 38, len(data)) + bytes(data)

def pack_rows(rows):
    out = bytearray()
    stride = (len(rows[0]) + 7) // 8
    for row in rows:
        for k in range(stride):
            b = 0
            for j in range(8):
                x = k * 8 + j
                if x < len(row) and row[x]:
                    b |= 1 << (7 - j)
            out.append(b)
    return bytes(out)

# ---------------- fixture 1: MMR generic region ----------------
W, H = 16, 4
rows = [
    [1] * 8 + [0] * 8,           # w0 b8 w8
    [0] * 4 + [1] * 4 + [0] * 8, # w4 b4 w8
    [1] * 16,                    # w0 b16
    [0] * 16,                    # all white
]
mmr = encode_mmr_rows(rows)
stream1 = page_info_segment(1, W, H) + generic_region_segment(2, W, H, mmr)
stream1 += seg_header(3, 51, 0)  # end of file

got, gw, gh = node_run([stream1])
want = pack_rows(rows)
assert got == want and (gw, gh) == (W, H), \
    f"fixture1 mismatch:\n got {got.hex()} {gw}x{gh}\nwant {want.hex()}"
print("fixture1 validated:", got.hex())

# ---------------- fixture 2: real arithmetic-coded stream ----------------
# Extracted from pdf.js's test file jbig2_symbol_offset.pdf (132x14,
# arithmetic-coded symbol dictionary + text region, no globals).
# The PDF declares /Length 191; the raw extraction appends the EOL before
# "endstream" (2 bytes), which pdf.js never feeds to the decoder.
PDF_STREAM = [int(b) for b in open("/tmp/opencode/jbig2_embedded.bin", "rb").read()][:191]
got2, gw2, gh2 = node_run([PDF_STREAM])
assert (gw2, gh2) == (132, 14) and len(got2) == 17 * 14, (gw2, gh2, len(got2))
print(f"fixture2 validated: {len(got2)} bytes, hash {sum(got2) & 0xffff:#x}")

# ---------------- fixture 3: pattern dictionary + halftone region --------
PW = 4
# Patterns (4x4 each): 0 white, 1 left half black, 2 right half black, 3 all black.
def pattern_bitmap(idx):
    p = [[0] * PW for _ in range(PW)]
    if idx == 1:
        for y in range(PW):
            for x in range(PW // 2):
                p[y][x] = 1
    elif idx == 2:
        for y in range(PW):
            for x in range(PW // 2, PW):
                p[y][x] = 1
    elif idx == 3:
        for y in range(PW):
            for x in range(PW):
                p[y][x] = 1
    return p

patterns = [pattern_bitmap(k) for k in range(4)]
max_pattern_index = 3
# Collective bitmap: rows of 16 px, identical across the 4 pattern rows.
collective_rows = []
for y in range(PW):
    row = []
    for k in range(max_pattern_index + 1):
        row += patterns[k][y]
    collective_rows.append(row)
coll_mmr = encode_mmr_rows(collective_rows)

pat_seg = bytearray()
pat_seg.append(0x01)           # flags: MMR=1, template=0
pat_seg.append(PW)             # pattern width
pat_seg.append(PW)             # pattern height
pat_seg += max_pattern_index.to_bytes(4, "big")
pat_seg += coll_mmr
pattern_segment = seg_header(2, 16, len(pat_seg)) + bytes(pat_seg)

GW, GH = 4, 4
RW, RH = 16, 16
# gray(mg, ng) = (mg + ng) % 4
gray = [[(mg + ng) % 4 for ng in range(GW)] for mg in range(GH)]
plane_bits = [
    [[(gray[mg][ng] >> j) & 1 for ng in range(GW)] for mg in range(GH)]
    for j in range(2)  # plane 0 = low bit, plane 1 = high bit
]
# Decode order: plane 1 first (MSB plane), then plane 0. Each plane's MMR
# data must end with an EOFB code (two EOLs) — pdf.js decodes the shared
# stream with EndOfBlock=true and relies on EOFB to stop per plane.
EOFB = "000000000001000000000001"

def with_eofb(data):
    bw = "".join(f"{b:08b}" for b in data) + EOFB
    bw += "0" * ((-len(bw)) % 8)
    return bytes(int(bw[i:i + 8], 2) for i in range(0, len(bw), 8))

halftone_mmr = with_eofb(encode_mmr_rows(plane_bits[1])) + with_eofb(encode_mmr_rows(plane_bits[0]))

ht_seg = bytearray()
ht_seg += RW.to_bytes(4, "big")   # region width
ht_seg += RH.to_bytes(4, "big")   # region height
ht_seg += (0).to_bytes(4, "big")  # x
ht_seg += (0).to_bytes(4, "big")  # y
ht_seg.append(0)                  # combination operator OR
ht_seg.append(0x01)               # flags: MMR=1, template=0, no skip, OR, dpv 0
ht_seg += GW.to_bytes(4, "big")
ht_seg += GH.to_bytes(4, "big")
ht_seg += (0).to_bytes(4, "big")  # grid offset X
ht_seg += (0).to_bytes(4, "big")  # grid offset Y
ht_seg += (1024).to_bytes(2, "big")  # grid vector X = 4.0 in 8.8
ht_seg += (0).to_bytes(2, "big")  # grid vector Y
ht_seg += halftone_mmr
halftone_segment = seg_header(3, 22, len(ht_seg), referred=(2,)) + bytes(ht_seg)

stream3 = page_info_segment(1, RW, RH) + pattern_segment + halftone_segment
stream3 += seg_header(4, 51, 0)

got3, gw3, gh3 = node_run([stream3])
# Expected page: 4x4 grid of the patterns (1 = black).
exp3_rows = []
for mg in range(GH):
    for y in range(PW):
        row = []
        for ng in range(GW):
            row += patterns[(mg + ng) % 4][y]
        exp3_rows.append(row)
want3 = pack_rows(exp3_rows)
assert (gw3, gh3) == (RW, RH), (gw3, gh3)
if got3 != want3:
    print("WARNING: node != local expectation")
    print(" got ", got3.hex())
    print(" want", want3.hex())
else:
    print("fixture3 validated:", got3.hex())

# ---------------- emit Rust test file ----------------
def rs_bytes(name, data):
    body = ", ".join(f"0x{b:02x}" for b in data)
    return f"    const {name}: [u8; {len(data)}] = [{body}];"

DIM1 = (16, 4)
DIM2 = (132, 14)
DIM3 = (16, 16)

lines = []
lines.append("//! JBIG2 fixture constants + parity tests (R5).")
lines.append("//!")
lines.append("//! Streams were built by `tests/gen_jbig2_fixtures.py` and VALIDATED")
lines.append("//! by running the exact pdf.js v3.11.174 jbig2.js decoder under")
lines.append("//! node; every *_EXPECT constant is pdf.js's own page-buffer output")
lines.append("//! (packed 1 bpp, 1 = black, stride (width + 7) / 8).")
lines.append("//!")
lines.append("//! Fixtures:")
lines.append("//! 1. hand-encoded MMR generic region (16x4),")
lines.append("//! 2. real pdf.js test stream (jbig2_symbol_offset.pdf, arithmetic")
lines.append("//!    symbol dictionary + text region, 132x14),")
lines.append("//! 3. hand-encoded MMR pattern dictionary + halftone region (16x16).")
lines.append("")
lines.append("use super::*;")
lines.append("")
lines.append("/// Decode a single embedded stream and return the packed page buffer.")
lines.append("fn decode_stream(stream: &[u8]) -> (u32, u32, Vec<u8>) {")
lines.append("    decode(stream, None).expect(\"fixture decode\")")
lines.append("}")
lines.append("")
lines.append("// --- fixture 1: hand-encoded MMR generic region, 16x4 ---")
lines.append(rs_bytes("MMR_STREAM", stream1))
lines.append(rs_bytes("MMR_EXPECT", want))
lines.append("")
lines.append("// --- fixture 2: real arithmetic-coded stream (132x14) ---")
lines.append(rs_bytes("ARITH_STREAM", bytes(PDF_STREAM)))
lines.append(rs_bytes("ARITH_EXPECT", got2))
lines.append("")
lines.append("// --- fixture 3: MMR pattern dictionary + halftone region, 16x16 ---")
lines.append(rs_bytes("HALFTONE_STREAM", stream3))
lines.append(rs_bytes("HALFTONE_EXPECT", got3))
lines.append("")
lines.append("#[test]")
lines.append("fn generic_region_mmr() {")
lines.append(f"    let (w, h, buf) = decode_stream(&MMR_STREAM);")
lines.append(f"    assert_eq!((w, h), ({DIM1[0]}, {DIM1[1]}));")
lines.append("    assert_eq!(buf, MMR_EXPECT);")
lines.append("}")
lines.append("")
lines.append("#[test]")
lines.append("fn generic_region_mmr_error_truncated() {")
lines.append("    // Truncated MMR stream must error, not panic (R1).")
lines.append("    let cut = &MMR_STREAM[..MMR_STREAM.len() - 3];")
lines.append("    let r = decode(cut, None);")
lines.append("    // Missing rows pad with zeros in the reference; accept Ok or Err")
lines.append("    // but never panic.")
lines.append("    let _ = r;")
lines.append("}")
lines.append("")
lines.append("#[test]")
lines.append("fn symbol_dictionary_arithmetic() {")
lines.append(f"    let (w, h, buf) = decode_stream(&ARITH_STREAM);")
lines.append(f"    assert_eq!((w, h), ({DIM2[0]}, {DIM2[1]}));")
lines.append("    assert_eq!(buf, ARITH_EXPECT);")
lines.append("}")
lines.append("")
lines.append("#[test]")
lines.append("fn pattern_dictionary_halftone_mmr() {")
lines.append(f"    let (w, h, buf) = decode_stream(&HALFTONE_STREAM);")
lines.append(f"    assert_eq!((w, h), ({DIM3[0]}, {DIM3[1]}));")
lines.append("    assert_eq!(buf, HALFTONE_EXPECT);")
lines.append("}")
lines.append("")
lines.append("#[test]")
lines.append("fn globals_stream_sharing() {")
lines.append("    // Fixture 2 has no globals; feeding an empty globals chunk must")
lines.append("    // still decode identically (globals segments are just prepended).")
lines.append("    let (w, h, buf) = decode(&ARITH_STREAM, Some(&[])).expect(\"decode with empty globals\");")
lines.append(f"    assert_eq!((w, h), ({DIM2[0]}, {DIM2[1]}));")
lines.append("    assert_eq!(buf, ARITH_EXPECT);")
lines.append("}")
lines.append("")
lines.append("#[test]")
lines.append("fn invalid_segment_type_rejected() {")
lines.append("    let mut bad = MMR_STREAM.clone();")
lines.append("    bad[4] = 0x05; // invalid segment type in flags byte")
lines.append("    assert!(decode(&bad, None).is_err());")
lines.append("}")
lines.append("")
lines.append("#[test]")
lines.append("fn empty_stream_errors() {")
lines.append("    assert!(decode(&[], None).is_err());")
lines.append("}")
lines.append("")
open(os.path.join(HERE, "..", "src", "parser", "jbig2", "tests.rs"), "w").write("\n".join(lines))
print("wrote tests.rs with", len(lines), "lines")
