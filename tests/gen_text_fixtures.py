#!/usr/bin/env python3
"""Generate text-extraction acceptance fixtures (plan Phase 3).

Three minimal PDFs, hand-built as raw bytes (no external deps):

- text_columns.pdf  — two text columns (x≈50 / x≈350, aligned baselines)
- text_rtl.pdf      — Hebrew word emitted in *visual* order via a Type0
                      Identity-H font with a ToUnicode CMap (no embedded
                      font program needed for extraction)
- text_rotated.pdf  — horizontal + 90°/180°/270° rotated text runs

Run from the crate root:  python3 tests/gen_text_fixtures.py
"""

import os

OUT = os.path.join(os.path.dirname(__file__), "fixtures")


class PdfBuilder:
    """Minimal sequential PDF writer with a correct xref table."""

    def __init__(self):
        self.objects: list[bytes] = []  # object bodies, 1-based

    def add(self, body: bytes) -> int:
        self.objects.append(body)
        return len(self.objects)

    def stream(self, dict_extra: bytes, data: bytes) -> int:
        return self.add(b"<< " + dict_extra + b" /Length " + str(len(data)).encode() + b" >>\nstream\n" + data + b"\nendstream")

    def build(self) -> bytes:
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = [0]
        for i, body in enumerate(self.objects, start=1):
            offsets.append(len(out))
            out += str(i).encode() + b" 0 obj\n" + body + b"\nendobj\n"
        xref_pos = len(out)
        n = len(self.objects) + 1
        out += b"xref\n0 " + str(n).encode() + b"\n"
        out += b"0000000000 65535 f \n"
        for off in offsets[1:]:
            out += ("%010d 00000 n \n" % off).encode()
        out += b"trailer\n<< /Size " + str(n).encode() + b" /Root 1 0 R >>\nstartxref\n" + str(xref_pos).encode() + b"\n%%EOF\n"
        return bytes(out)


def make_columns() -> bytes:
    b = PdfBuilder()
    b.add(b"<< /Type /Catalog /Pages 2 0 R >>")
    b.add(b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
    content = (
        b"BT /F1 12 Tf\n"
        b"1 0 0 1 50 700 Tm (Alpha beta) Tj\n"
        b"1 0 0 1 350 700 Tm (Gamma delta) Tj\n"
        b"1 0 0 1 50 650 Tm (Epsilon) Tj\n"
        b"1 0 0 1 350 650 Tm (Zeta eta) Tj\n"
        b"ET\n"
    )
    b.add(b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>")
    b.add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
    b.stream(b"", content)
    return b.build()


def make_rtl() -> bytes:
    b = PdfBuilder()
    b.add(b"<< /Type /Catalog /Pages 2 0 R >>")
    b.add(b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
    # Visual order (leftmost glyph first): final mem, vav, lamed, shin.
    # CID 1->shin 2->lamed 3->vav 4->final mem, stream emits 4 3 2 1.
    content = b"BT /F1 12 Tf\n50 700 Td <0004000300020001> Tj\nET\n"
    b.add(b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>")
    b.add(
        b"<< /Type /Font /Subtype /Type0 /BaseFont /Hebrew-Test /Encoding /Identity-H "
        b"/DescendantFonts [6 0 R] /ToUnicode 7 0 R >>"
    )
    b.stream(b"", content)
    b.add(
        b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Hebrew-Test "
        b"/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 >>"
    )
    to_unicode = (
        b"/CIDInit /ProcSet findresource begin\n"
        b"12 dict begin\nbegincmap\n"
        b"/CMapName /Adobe-Identity def\n/CMapType 2 def\n"
        b"1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n"
        b"4 beginbfchar\n"
        b"<0001> <05E9>\n"  # shin
        b"<0002> <05DC>\n"  # lamed
        b"<0003> <05D5>\n"  # vav
        b"<0004> <05DD>\n"  # final mem
        b"endbfchar\n"
        b"endcmap\nend\nend\n"
    )
    b.stream(b"", to_unicode)
    return b.build()


def make_rotated() -> bytes:
    b = PdfBuilder()
    b.add(b"<< /Type /Catalog /Pages 2 0 R >>")
    b.add(b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
    content = (
        b"BT /F1 12 Tf\n"
        b"1 0 0 1 100 100 Tm (Horizontal) Tj\n"  # 0°
        b"0 1 -1 0 200 50 Tm (Upward) Tj\n"      # 90°
        b"-1 0 0 -1 400 400 Tm (Flipped) Tj\n"   # 180°
        b"0 -1 1 0 300 50 Tm (Downward) Tj\n"    # 270°
        b"ET\n"
    )
    b.add(b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>")
    b.add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
    b.stream(b"", content)
    return b.build()


def main():
    os.makedirs(OUT, exist_ok=True)
    for name, data in [
        ("text_columns.pdf", make_columns()),
        ("text_rtl.pdf", make_rtl()),
        ("text_rotated.pdf", make_rotated()),
    ]:
        path = os.path.join(OUT, name)
        with open(path, "wb") as f:
            f.write(data)
        print("wrote", path, len(data), "bytes")


if __name__ == "__main__":
    main()
