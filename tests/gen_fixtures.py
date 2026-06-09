"""Generate PDF fixture files for integration testing.

Run: python3 tests/gen_fixtures.py

Produces valid PDFs with correct byte offsets in the XRef table.
"""

import zlib
import os

FIXTURES_DIR = os.path.join(os.path.dirname(__file__), "fixtures")


def make_xref_entry(offset, gen, entry_type):
    """Generate a 20-byte XRef entry: 10+SP+5+SP+type+SP+LF = 20 bytes."""
    return f"{offset:010d} {gen:05d} {entry_type} \n".encode()


def write_minimal_pdf(path):
    """Single-page PDF with no content streams."""
    header = b"%PDF-1.4\n"

    obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"
    obj2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"
    obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"

    offset1 = len(header)
    offset2 = offset1 + len(obj1)
    offset3 = offset2 + len(obj2)
    xref_offset = offset3 + len(obj3)

    xref = b"xref\n0 4\n"
    xref += make_xref_entry(0, 65535, "f")
    xref += make_xref_entry(offset1, 0, "n")
    xref += make_xref_entry(offset2, 0, "n")
    xref += make_xref_entry(offset3, 0, "n")

    trailer = b"trailer\n<< /Size 4 /Root 1 0 R >>\n"
    trailer += f"startxref\n{xref_offset}\n%%EOF\n".encode()

    with open(path, "wb") as f:
        f.write(header + obj1 + obj2 + obj3 + xref + trailer)
    print(f"  Written: {path}")


def write_multipage_pdf(path):
    """3-page PDF with uncompressed text content streams."""
    header = b"%PDF-1.4\n"

    parts = []
    parts.append(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")
    parts.append(
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>\nendobj\n"
    )
    parts.append(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> >>\nendobj\n"
    )
    parts.append(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R /Resources << /Font << /F1 6 0 R >> >> >>\nendobj\n"
    )
    parts.append(
        b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R /Resources << /Font << /F1 6 0 R >> >> >>\nendobj\n"
    )
    parts.append(
        b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"
    )

    for i, text in enumerate(["Page 1", "Page 2", "Page 3"], start=7):
        content = f"BT /F1 12 Tf 100 700 Td ({text}) Tj ET".encode()
        stream_obj = f"{i} 0 obj\n<< /Length {len(content)} >>\nstream\n".encode()
        stream_obj += content
        stream_obj += b"\nendstream\nendobj\n"
        parts.append(stream_obj)

    offsets = []
    pos = len(header)
    for p in parts:
        offsets.append(pos)
        pos += len(p)

    xref_offset = pos
    num_objects = len(parts) + 1

    xref = f"xref\n0 {num_objects}\n".encode()
    xref += make_xref_entry(0, 65535, "f")
    for off in offsets:
        xref += make_xref_entry(off, 0, "n")

    trailer = f"trailer\n<< /Size {num_objects} /Root 1 0 R >>\n".encode()
    trailer += f"startxref\n{xref_offset}\n%%EOF\n".encode()

    with open(path, "wb") as f:
        f.write(header)
        for p in parts:
            f.write(p)
        f.write(xref + trailer)
    print(f"  Written: {path}")


def write_with_stream_pdf(path):
    """Single-page PDF with FlateDecode compressed content stream."""
    header = b"%PDF-1.4\n"

    content = b"BT /F1 24 Tf 100 700 Td (Hello, PDF World!) Tj ET"
    compressed = zlib.compress(content)

    parts = []
    parts.append(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")
    parts.append(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n")
    parts.append(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n"
    )

    stream_obj = (
        f"4 0 obj\n<< /Length {len(compressed)} /Filter /FlateDecode >>\nstream\n".encode()
    )
    stream_obj += compressed
    stream_obj += b"\nendstream\nendobj\n"
    parts.append(stream_obj)

    parts.append(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"
    )

    offsets = []
    pos = len(header)
    for p in parts:
        offsets.append(pos)
        pos += len(p)

    xref_offset = pos
    num_objects = len(parts) + 1

    xref = f"xref\n0 {num_objects}\n".encode()
    xref += make_xref_entry(0, 65535, "f")
    for off in offsets:
        xref += make_xref_entry(off, 0, "n")

    trailer = f"trailer\n<< /Size {num_objects} /Root 1 0 R >>\n".encode()
    trailer += f"startxref\n{xref_offset}\n%%EOF\n".encode()

    with open(path, "wb") as f:
        f.write(header)
        for p in parts:
            f.write(p)
        f.write(xref + trailer)
    print(f"  Written: {path}")


if __name__ == "__main__":
    os.makedirs(FIXTURES_DIR, exist_ok=True)
    print("Generating PDF fixtures...")
    write_minimal_pdf(os.path.join(FIXTURES_DIR, "minimal.pdf"))
    write_multipage_pdf(os.path.join(FIXTURES_DIR, "multipage.pdf"))
    write_with_stream_pdf(os.path.join(FIXTURES_DIR, "with_stream.pdf"))
    print("Done.")
