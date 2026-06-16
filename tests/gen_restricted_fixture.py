#!/usr/bin/env python3
"""Generate a minimal RC4-encrypted PDF fixture with restricted permissions.

Uses only Python standard library. Implements ISO 32000-1 §7.6.3 (RC4, R=3).

Run from the crate root:
    python3 tests/gen_restricted_fixture.py

Produces: tests/fixtures/restricted.pdf
User password: "user"
Owner password: "owner"
Permissions (P): -3904 — all operations denied (only reserved bits 7,8,13+ set)
"""

import hashlib
import os
import struct

FIXTURES_DIR = os.path.join(os.path.dirname(__file__), "fixtures")

USER_PASSWORD = b"user"
OWNER_PASSWORD = b"owner"
# P = -3904 = 0xFFFFF0C0  — all permission bits clear (deny all)
P_FLAGS = -3904

# Standard PDF password padding string (ISO 32000-1 §7.6.3.3)
PASSWORD_PADDING = bytes([
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41,
    0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80,
    0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
])


def rc4(key: bytes, data: bytes) -> bytes:
    """RC4 stream cipher."""
    s = list(range(256))
    j = 0
    for i in range(256):
        j = (j + s[i] + key[i % len(key)]) % 256
        s[i], s[j] = s[j], s[i]
    i = j = 0
    out = bytearray()
    for byte in data:
        i = (i + 1) % 256
        j = (j + s[i]) % 256
        s[i], s[j] = s[j], s[i]
        out.append(byte ^ s[(s[i] + s[j]) % 256])
    return bytes(out)


def pad_password(pw: bytes) -> bytes:
    """Pad or truncate to 32 bytes using the PDF padding string."""
    pw = pw[:32]
    return (pw + PASSWORD_PADDING)[:32]


def derive_file_key(user_pw: bytes, revision: int, key_bytes: int,
                    o_entry: bytes, p_flags: int, file_id: bytes) -> bytes:
    """Algorithm 2 (ISO 32000-1 §7.6.3.3): derive the file encryption key."""
    padded = pad_password(user_pw)
    h = hashlib.md5()
    h.update(padded)
    h.update(o_entry)
    h.update(struct.pack('<i', p_flags))
    h.update(file_id)
    digest = h.digest()
    if revision >= 3:
        for _ in range(50):
            digest = hashlib.md5(digest[:key_bytes]).digest()
    return digest[:key_bytes]


def compute_o_entry(owner_pw: bytes, user_pw: bytes,
                    revision: int, key_bytes: int) -> bytes:
    """Algorithm 3: compute the /O entry."""
    # Step 1: MD5 of padded owner password
    h = hashlib.md5(pad_password(owner_pw)).digest()
    # Step 2: For R>=3, iterate 50 rounds
    if revision >= 3:
        for _ in range(50):
            h = hashlib.md5(h[:key_bytes]).digest()
    owner_key = h[:key_bytes]
    # Step 3: RC4 encrypt padded user password with owner key
    result = rc4(owner_key, pad_password(user_pw))
    # Step 4: For R>=3, 20 rounds with modified key
    if revision >= 3:
        for k in range(1, 20):
            round_key = bytes(b ^ k for b in owner_key)
            result = rc4(round_key, result)
    return result


def compute_u_entry(file_key: bytes, revision: int, file_id: bytes) -> bytes:
    """Algorithm 5 (R>=3): compute the /U entry."""
    # MD5(PASSWORD_PADDING + file_id)
    h = hashlib.md5(PASSWORD_PADDING + file_id).digest()
    result = rc4(file_key, h)
    for k in range(1, 20):
        round_key = bytes(b ^ k for b in file_key)
        result = rc4(round_key, result)
    # Pad to 32 bytes
    return result + b'\x00' * 16


def hex_literal(data: bytes) -> bytes:
    """Convert bytes to PDF hex string literal."""
    return b'<' + data.hex().encode() + b'>'


def make_xref_entry(offset: int) -> bytes:
    """20-byte XRef in-use entry."""
    return f"{offset:010d} 00000 n \n".encode()


def write_restricted_pdf(path: str) -> None:
    key_bytes = 16  # 128-bit key
    revision = 3
    file_id = hashlib.md5(b"restricted-pdf-fixture-id").digest()

    # Compute O entry
    o_entry = compute_o_entry(OWNER_PASSWORD, USER_PASSWORD, revision, key_bytes)

    # Compute file key for user password
    file_key = derive_file_key(USER_PASSWORD, revision, key_bytes,
                               o_entry, P_FLAGS, file_id)

    # Compute U entry
    u_entry = compute_u_entry(file_key, revision, file_id)

    # Build PDF objects (not encrypted — content is trivial for fixture purposes)
    header = b"%PDF-1.4\n"

    # obj 1: Catalog
    obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"
    # obj 2: Pages
    obj2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"
    # obj 3: Page
    obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
    # obj 4: Encrypt dict
    p_bytes = struct.pack('<i', P_FLAGS)
    encrypt_obj = (
        b"4 0 obj\n"
        b"<< /Filter /Standard\n"
        b"   /V 2\n"
        b"   /R 3\n"
        b"   /Length 128\n"
        b"   /P " + str(P_FLAGS).encode() + b"\n"
        b"   /O " + hex_literal(o_entry) + b"\n"
        b"   /U " + hex_literal(u_entry) + b"\n"
        b">>\nendobj\n"
    )

    # Compute byte offsets
    offset_obj1 = len(header)
    offset_obj2 = offset_obj1 + len(obj1)
    offset_obj3 = offset_obj2 + len(obj2)
    offset_obj4 = offset_obj3 + len(obj3)
    xref_offset = offset_obj4 + len(encrypt_obj)

    # XRef table
    xref = (
        b"xref\n"
        b"0 5\n"
        b"0000000000 65535 f \n"
        + make_xref_entry(offset_obj1)
        + make_xref_entry(offset_obj2)
        + make_xref_entry(offset_obj3)
        + make_xref_entry(offset_obj4)
    )

    # Trailer
    file_id_hex = hex_literal(file_id)
    trailer = (
        b"trailer\n"
        b"<< /Size 5\n"
        b"   /Root 1 0 R\n"
        b"   /Encrypt 4 0 R\n"
        b"   /ID [" + file_id_hex + b" " + file_id_hex + b"]\n"
        b">>\n"
        b"startxref\n"
        + str(xref_offset).encode() + b"\n"
        b"%%EOF\n"
    )

    pdf = header + obj1 + obj2 + obj3 + encrypt_obj + xref + trailer

    os.makedirs(FIXTURES_DIR, exist_ok=True)
    with open(path, "wb") as f:
        f.write(pdf)
    print(f"Written {len(pdf)} bytes to {path}")
    print(f"User password: {USER_PASSWORD!r}")
    print(f"P flags: {P_FLAGS} (0x{P_FLAGS & 0xFFFFFFFF:08X})")
    print(f"All permission bits should be 0 (deny all)")


if __name__ == "__main__":
    out = os.path.join(FIXTURES_DIR, "restricted.pdf")
    write_restricted_pdf(out)
