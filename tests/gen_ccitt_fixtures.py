#!/usr/bin/env python3
"""Build CCITT test-stream constants for src/parser/ccitt.rs tests.

Codes are recovered by brute-force SIMULATION of pdf.js v3.11.174's exact
ccitt.js table-lookup logic (whiteTable1/2, blackTable1/2/3, including the
band-offset tricks like blackTable2[(code>>1)-64]). Each generated stream is
then VALIDATED by running the actual pdf.js decoder under node; only
node-produced byte sequences are embedded as expected values in the Rust
tests (exact parity with the reference).
"""
import json, re, subprocess, sys, os

SRC = os.path.join(os.path.dirname(__file__), "ccitt-reference.js")
REF = os.path.join(os.path.dirname(__file__), ".ccittref")
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
    """pdf.js eoblock white lookup: exact simulation."""
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
    """run -> (bits, code) by simulating lookBits(N)=code<<(max_bits-N)."""
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
assert bits(64, WHITE) == "11011"
assert bits(1, BLACK) == "010" and bits(2, BLACK) == "11" and bits(3, BLACK) == "10"

class BitW:
    def __init__(self):
        self.s = ""
    def w(self, b):
        self.s += b
    def bytes(self):
        s = self.s + "0" * ((-len(self.s)) % 8)
        return bytes(int(s[i:i+8], 2) for i in range(0, len(s), 8))

def wrun(bw, n):
    assert n >= 0
    while n >= 64:
        mk = max(v for v in (64, 128, 256, 512, 1024, 1728) if v in WHITE and v <= n)
        bw.w(bits(mk, WHITE)); n -= mk
    bw.w(bits(n, WHITE))

def brun(bw, n):
    assert n >= 0
    while n >= 64:
        mk = max(v for v in (64, 128, 256, 512, 1024, 1728) if v in BLACK and v <= n)
        bw.w(bits(mk, BLACK)); n -= mk
    bw.w(bits(n, BLACK))

V0, VR1, VL1, HORZ, PASS = "1", "011", "010", "001", "0001"
EOL = "000000000001"
EXT_1D = "1"   # pdf.js: nextLine2D = !bit -> bit 1 = 1-D follows

# ---------------- node harness ----------------
os.makedirs(REF, exist_ok=True)
cc = open(SRC).read().replace('import { FormatError, info } from "../shared/util.js";',
                              'const FormatError = class extends Error {};\nconst info = () => {};')
open(f"{REF}/ccitt.js", "w").write(cc)
open(f"{REF}/run.mjs", "w").write(r'''
import { CCITTFaxDecoder } from "./ccitt.js";
import { readFileSync } from "fs";
const input = JSON.parse(readFileSync(0, "utf8"));
const src = { next: (() => { let i = 0; return () => i < input.data.length ? input.data[i++] : -1; })() };
const dec = new CCITTFaxDecoder(src, input.parms);
const out = [];
let n = 0;
while (n++ < 100000) {
  const b = dec.readNextChar();
  if (b === -1) break;
  out.push(b & 0xff);
}
console.log(JSON.stringify({ bytes: out, err: !!dec.err }));
''')

def node_run(stream, parms):
    payload = json.dumps({"data": list(stream), "parms": parms})
    r = subprocess.run(["node", f"{REF}/run.mjs"], input=payload, capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"node failed: {r.stderr[:300]}")
    res = json.loads(r.stdout)
    return bytes(res["bytes"]), res["err"]

# node round-trip check of every recovered black code (r=0 is a no-op run)
for r in sorted(BLACK):
    if r == 0:
        continue
    w = BitW()
    wrun(w, 1); brun(w, r); wrun(w, 5); w.w(EOL)
    cols = r + 6
    got, err = node_run(w.bytes(), {"K": 0, "Columns": cols, "Rows": 0})
    want_bits = "1" + "0" * r + "11111"
    want_bits += "0" * ((-len(want_bits)) % 8)
    assert got and got[0] == int(want_bits[:8], 2) and not err, f"black {r} {bits(r, BLACK)} -> {got.hex()}"

vectors = []

# V1: K=0, Columns=16, two 1-D rows, trailing EOL.
w = BitW()
wrun(w, 5); brun(w, 4); wrun(w, 7)
wrun(w, 2); brun(w, 3); wrun(w, 11)
w.w(EOL)
vectors.append(("ONE_D", w.bytes(), {"K": 0, "Columns": 16, "Rows": 0}, None))

# V2: K=-1, Columns=8. Row0: HORZ w4 b4 from imaginary all-white ref.
#      Row1: VR1 (a1=refLine[0]+1=5) then V0 (b3) -> w5 b3. Trailing EOL.
w = BitW()
w.w(HORZ + bits(4, WHITE) + bits(4, BLACK))
w.w(VR1 + V0)
w.w(EOL)
vectors.append(("G4_VR1", w.bytes(), {"K": -1, "Columns": 8, "Rows": 0}, None))

# V3: K=-1. Row1 via VL1 then V0 -> w3 b5 (11100000).
w = BitW()
w.w(HORZ + bits(4, WHITE) + bits(4, BLACK))
w.w(VL1 + V0)
w.w(EOL)
vectors.append(("G4_VL1", w.bytes(), {"K": -1, "Columns": 8, "Rows": 0}, None))

# V4: K=-1. Row1 via PASS -> w8 (pass over ref's black run).
w = BitW()
w.w(HORZ + bits(4, WHITE) + bits(4, BLACK))
w.w(PASS)
w.w(EOL)
vectors.append(("G4_PASS", w.bytes(), {"K": -1, "Columns": 8, "Rows": 0}, None))

# V5: K=-1. Two all-white rows via single V0 each.
w = BitW()
w.w(V0 + V0)
w.w(EOL)
vectors.append(("G4_V0_2ROWS", w.bytes(), {"K": -1, "Columns": 8, "Rows": 0}, None))

# V6: K=1 with EOL + 1-D extension bits before each row, trailing EOL.
w = BitW()
w.w(EOL + EXT_1D); wrun(w, 5); brun(w, 4); wrun(w, 7)
w.w(EOL + EXT_1D); wrun(w, 2); brun(w, 3); wrun(w, 11)
w.w(EOL)
vectors.append(("K1_EOL", w.bytes(), {"K": 1, "Columns": 16, "Rows": 0, "EndOfLine": True}, None))

# V7: EncodedByteAlign, Columns=4, rows w2b2 / w1b3, no EOLs (decoder
# byte-aligns after each row, so pad each row to a byte boundary).
w = BitW()
wrun(w, 2); brun(w, 2); w.w("0" * ((-len(w.s)) % 8))
wrun(w, 1); brun(w, 3)
vectors.append(("BYTE_ALIGN", w.bytes(), {"K": 0, "Columns": 4, "Rows": 0, "EncodedByteAlign": True}, None))

# V8: BlackIs1 (white pixels emitted as 0 bits).
w = BitW()
wrun(w, 5); brun(w, 4); wrun(w, 7)
w.w(EOL)
vectors.append(("BLACK_IS1", w.bytes(), {"K": 0, "Columns": 16, "Rows": 0, "BlackIs1": True}, None))

# V9: white makeup codes. Columns=260: w75 b3 w182 + EOL.
w = BitW()
wrun(w, 75); brun(w, 3); wrun(w, 182)
w.w(EOL)
vectors.append(("MAKEUP", w.bytes(), {"K": 0, "Columns": 260, "Rows": 0}, None))

# V10: black run 14 (node-verified code) + white 113.
w = BitW()
wrun(w, 3); brun(w, 14); wrun(w, 113)
w.w(EOL)
vectors.append(("BLACK_LONG", w.bytes(), {"K": 0, "Columns": 130, "Rows": 0}, None))

print("--- node validation ---")
ok = True
results = {}
for name, stream, parms, expect_override in vectors:
    got, err = node_run(stream, parms)
    results[name] = (stream, parms, got, err)
    print(f"{name}: {len(got)} bytes err={err} {got.hex()[:64]}{'...' if len(got) > 32 else ''}")

# Sanity: ROWS_LIMIT uses ONE_D stream; first row bytes = first 2 of node output.
one_d = results["ONE_D"]
print("ROWS_LIMIT prefix:", one_d[2][:2].hex())

print("\n--- Rust constants ---")
for name, (stream, parms, got, err) in results.items():
    print(f"pub const {name}_STREAM: [u8; {len(stream)}] = [{', '.join('0x%02x' % b for b in stream)}];")
    print(f"pub const {name}_EXPECT: [u8; {len(got)}] = [{', '.join('0x%02x' % b for b in got)}];")
