#!/usr/bin/env python3
"""Generate tests/fixtures/type3.pdf — a minimal Type 3 font.

Glyph box: filled square (glyph space 800×800, FontMatrix 0.001 → 0.8 pt).
Glyph tri: filled triangle with a `d1` (bbox clip) announcement.
Codes: 0x41 'A' → box, 0x42 'B' → tri. /Widths 800 each.
"""
import struct, zlib, os

def mk_pdf():
    heading = b"""%PDF-1.4
"""
    objects = {}

    def put(num, body):
        objects[num] = body

    def put_stream(num, dict_extra, data):
        objects[num] = b"<< /Length %d %s >>\nstream\n%s\nendstream" % (
            len(data), dict_extra.encode(), data)

    # --- charprocs: raw (uncompressed) for readability
    put(1, b"<< /Type /Font /Subtype /Type3 /FirstChar 0 /LastChar 255\n"
        b"  /Widths 5 0 R /FontMatrix [0.001 0 0 0.001 0 0]\n"
        b"  /FontBBox [0 0 1000 1000] /Encoding 6 0 R /CharProcs 2 0 R\n"
        b"  /Resources 7 0 R >>")
    put(2, b"<< /box 3 0 R /tri 4 0 R >>")
    put_stream(3, "", b"0 0 800 800 re f")
    put_stream(4, "", b"400 d1 0 0 800 950 re f")   # d1 announces wx=400, bbox 0..950 tall

    # Widths covers FirstChar..LastChar (code 0..255): box/tri at 65/66.
    widths_arr = b" ".join([b"0"] * 65 + [b"800", b"800"] + [b"0"] * (255 - 66))
    put(5, b"[%s]" % widths_arr)
    put(6, b"<< /BaseEncoding /StandardEncoding /Differences [65 /box 66 /tri] >>")
    put(7, b"<< >>")   # font resources empty
    put(8, b"<< /Type /Page /Parent 9 0 R /MediaBox [0 0 612 792]\n"
        b"  /Resources << /Font << /F1 1 0 R >> >> /Contents 10 0 R >>")
    put(9, b"<< /Type /Pages /Kids [8 0 R] /Count 1 >>")
    put_stream(10, "", b"BT /F1 24 Tf 100 700 Td (AB) Tj ET")
    put(11, b"<< /Type /Font /Subtype /Type3 /FirstChar 0 /LastChar 255\n"
        b"  /Widths 12 0 R /FontMatrix [0.001 0 0 0.001 0 0]\n"
        b"  /FontBBox [0 0 1000 1000] /Encoding 13 0 R /CharProcs 14 0 R\n"
        b"  /Resources 15 0 R >>")
    put(12, b"[" + b" ".join([b"800"] + ([b"0"] * 254) + [b"800"]) + b"]")
    put(13, b"<< /Differences [65 /box 66 /tri] >>")
    put(14, b"<< /box 3 0 R /tri 4 0 R >>")
    put(15, b"<< >>")
    put(16, b"<< /Type /Catalog /Pages 9 0 R >>")

    out = bytearray(heading)
    offsets = {}
    for num in sorted(objects):
        offsets[num] = len(out)
        out += b"%d 0 obj\n" % num
        out += objects[num]
        out += b"\nendobj\n"

    xref_pos = len(out)
    max_obj = max(objects)
    out += b"xref\n0 %d\n" % (max_obj + 1)
    out += b"0000000000 65535 f \n"
    for num in range(1, max_obj + 1):
        out += b"%010d 00000 n \n" % offsets.get(num, 0)
    out += b"trailer\n<< /Size %d /Root 16 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (max_obj + 1, xref_pos)
    return bytes(out)

if __name__ == "__main__":
    path = os.path.join(os.path.dirname(__file__), "fixtures", "type3.pdf")
    with open(path, "wb") as f:
        f.write(mk_pdf())
    print("wrote", path)
