#!/usr/bin/env python3
"""Generate AES-256 (R6) encrypted PDF fixture for integration testing.

Implements ISO 32000-2 §7.6.4 (Algorithm 2.B) using only the `cryptography`
standard library.  Run from the crate root:

    python3 tests/gen_aes256_fixture.py

Produces: tests/fixtures/encrypted_aes256.pdf
User password: "test"
"""

import hashlib
import os
import secrets
import struct

from cryptography.hazmat.backends import default_backend
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

FIXTURES_DIR = os.path.join(os.path.dirname(__file__), "fixtures")
PASSWORD = b"test"
P_FLAGS = -3904  # allow all operations


# ---------------------------------------------------------------------------
# Low-level AES helpers
# ---------------------------------------------------------------------------

def _aes128_cbc_encrypt(key: bytes, iv: bytes, data: bytes) -> bytes:
    """AES-128-CBC encrypt; pads data to 16-byte boundary with zeros."""
    rem = len(data) % 16
    if rem:
        data = data + b"\x00" * (16 - rem)
    cipher = Cipher(algorithms.AES(key), modes.CBC(iv), backend=default_backend())
    enc = cipher.encryptor()
    return enc.update(data) + enc.finalize()


def _aes256_cbc_encrypt_nopad(key: bytes, iv: bytes, data: bytes) -> bytes:
    """AES-256-CBC encrypt; pads data to 16-byte boundary with zeros."""
    rem = len(data) % 16
    if rem:
        data = data + b"\x00" * (16 - rem)
    cipher = Cipher(algorithms.AES(key), modes.CBC(iv), backend=default_backend())
    enc = cipher.encryptor()
    return enc.update(data) + enc.finalize()


def _aes256_ecb_encrypt(key: bytes, data: bytes) -> bytes:
    """AES-256-ECB encrypt (used for /Perms field)."""
    cipher = Cipher(algorithms.AES(key), modes.ECB(), backend=default_backend())
    enc = cipher.encryptor()
    return enc.update(data) + enc.finalize()


# ---------------------------------------------------------------------------
# Algorithm 2.B (ISO 32000-2 §7.6.4.3.4)
# ---------------------------------------------------------------------------

def compute_hash_r6(password: bytes, salt: bytes, u_entry: bytes) -> bytes:
    """Iterative hash used for R6 password verification and key derivation."""
    password = password[:127]
    k = hashlib.sha256(password + salt + u_entry).digest()

    round_num = 0
    while True:
        unit = password + k + u_entry
        input_data = unit * 64
        e = _aes128_cbc_encrypt(k[:16], k[16:32], input_data)

        selector = sum(e[:16]) % 3
        if selector == 0:
            k = hashlib.sha256(e).digest()
        elif selector == 1:
            k = hashlib.sha384(e).digest()
        else:
            k = hashlib.sha512(e).digest()

        round_num += 1
        if round_num >= 64 and e[-1] + 32 <= round_num:
            break

    return k[:32]


# ---------------------------------------------------------------------------
# /U, /UE, /O, /OE, /Perms construction
# ---------------------------------------------------------------------------

def make_u_ue(password: bytes, file_key: bytes):
    """Return (/U, /UE) 48-byte and 32-byte entries."""
    validation_salt = secrets.token_bytes(8)
    key_salt = secrets.token_bytes(8)
    u_hash = compute_hash_r6(password, validation_salt, b"")
    u_entry = u_hash + validation_salt + key_salt

    intermediate = compute_hash_r6(password, key_salt, b"")
    iv_zero = b"\x00" * 16
    ue_entry = _aes256_cbc_encrypt_nopad(intermediate, iv_zero, file_key)[:32]

    return u_entry, ue_entry


def make_o_oe(password: bytes, file_key: bytes, u_entry: bytes):
    """Return (/O, /OE) 48-byte and 32-byte entries."""
    validation_salt = secrets.token_bytes(8)
    key_salt = secrets.token_bytes(8)
    o_hash = compute_hash_r6(password, validation_salt, u_entry[:48])
    o_entry = o_hash + validation_salt + key_salt

    intermediate = compute_hash_r6(password, key_salt, u_entry[:48])
    iv_zero = b"\x00" * 16
    oe_entry = _aes256_cbc_encrypt_nopad(intermediate, iv_zero, file_key)[:32]

    return o_entry, oe_entry


def make_perms(file_key: bytes, p_flags: int, encrypt_meta: bool) -> bytes:
    """16-byte /Perms value, AES-256-ECB encrypted."""
    plaintext = bytearray(16)
    struct.pack_into("<i", plaintext, 0, p_flags)
    plaintext[4:8] = b"\xff\xff\xff\xff"
    plaintext[8] = ord("T") if encrypt_meta else ord("F")
    plaintext[9:12] = b"adb"
    plaintext[12:16] = b"\x00\x00\x00\x00"
    return _aes256_ecb_encrypt(file_key, bytes(plaintext))


# ---------------------------------------------------------------------------
# PDF encoding helpers
# ---------------------------------------------------------------------------

def hex_string(data: bytes) -> bytes:
    """Encode bytes as a PDF hex string: <aabbcc...>"""
    return b"<" + data.hex().encode() + b">"


def make_xref_entry(offset: int, gen: int, entry_type: str) -> bytes:
    return f"{offset:010d} {gen:05d} {entry_type} \n".encode()


# ---------------------------------------------------------------------------
# Build the encrypted PDF
# ---------------------------------------------------------------------------

def write_encrypted_aes256_pdf(path: str, password: bytes = PASSWORD) -> None:
    file_key = secrets.token_bytes(32)
    u_entry, ue_entry = make_u_ue(password, file_key)
    o_entry, oe_entry = make_o_oe(password, file_key, u_entry)
    perms = make_perms(file_key, P_FLAGS, True)
    doc_id = secrets.token_bytes(16)

    encrypt_dict = (
        b"<< /Filter /Standard"
        b" /V 5"
        b" /R 6"
        b" /Length 256"
        b" /P " + str(P_FLAGS).encode() +
        b" /EncryptMetadata true"
        b" /U " + hex_string(u_entry) +
        b" /UE " + hex_string(ue_entry) +
        b" /O " + hex_string(o_entry) +
        b" /OE " + hex_string(oe_entry) +
        b" /Perms " + hex_string(perms) +
        b" >>"
    )

    # The Rust `from_trailer` handler reads /Encrypt directly from the trailer
    # dict (no indirect-reference resolution at that stage), so we inline the
    # encrypt dictionary in the trailer rather than using an indirect object.
    header = b"%PDF-1.7\n"

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

    id_entry = b"[" + hex_string(doc_id) + b" " + hex_string(doc_id) + b"]"
    trailer = (
        b"trailer\n<< /Size 4 /Root 1 0 R /Encrypt " + encrypt_dict +
        b" /ID " + id_entry + b" >>\n"
        b"startxref\n" + str(xref_offset).encode() + b"\n%%EOF\n"
    )

    with open(path, "wb") as f:
        f.write(header + obj1 + obj2 + obj3 + xref + trailer)
    print(f"  Written: {path}")


if __name__ == "__main__":
    os.makedirs(FIXTURES_DIR, exist_ok=True)
    print("Generating AES-256 encrypted PDF fixture...")
    write_encrypted_aes256_pdf(
        os.path.join(FIXTURES_DIR, "encrypted_aes256.pdf"), PASSWORD
    )
    print("Done. User password: 'test'")
