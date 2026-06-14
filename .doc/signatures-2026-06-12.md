# signatures — Implementation Report

**Date:** 2026-06-12
**Scope:** PDF digital signatures (PKCS#7 / CMS / PAdES) — Phase 2

## What Was Implemented

### New files

- **`src/signatures/cms.rs`** — Manual DER encoder/decoder + CMS `SignedData` builder
  - OID constants: `OID_SHA256`, `OID_RSA_ENCRYPTION`, `OID_ID_DATA`, `OID_SIGNED_DATA`, `OID_CONTENT_TYPE`, `OID_MESSAGE_DIGEST`, `OID_SIGNING_TIME`
  - DER primitives: `der_len`, `der_tlv`, `der_seq`, `der_set`, `der_oid`, `der_octet`, `der_null`, `der_int_pos`, `der_int_u32`, `der_ctx0`, `der_utctime`
  - DER parser: `der_parse`, `der_parse_len`
  - Public API: `build_cms_signed_data(content_hash, private_key_der, cert_der, signing_time) -> Result<Vec<u8>>`

- **`src/signatures/signer.rs`** — PDF incremental-update signer
  - Types: `SignatureOptions { rect, page_index, field_name, reason, location, contact_info }`
  - Constants: `RESERVED_SIG_SIZE = 8192`, `BYTERANGE_PLACEHOLDER` (53 bytes, four 12-digit integers)
  - Public API: `sign_document(doc_bytes, private_key_der, cert_der, options) -> Result<Vec<u8>>`
  - Internals: `build_signature_field`, `add_to_page_annots`, `add_to_acroform`, `find_contents_placeholder`, `find_byterange_placeholder`, `format_byterange`, `patch_bytes`, `hex_encode`

- **`src/signatures/verifier.rs`** — PDF signature verifier
  - Types: `SignatureVerification { field_name, signature_valid, covers_whole_file, signer_name, error }`
  - Public API: `verify_signatures(doc_bytes) -> Result<Vec<SignatureVerification>>`
  - Internals: `get_acroform`, `verify_field`, `verify_cms_signed_data`, `find_message_digest`, `find_certificate`, `skip_context_tags`, `extract_common_name`

- **`src/signatures/mod.rs`** — re-exports public API

- **`tests/signatures.rs`** — integration tests

- **`tests/fixtures/test_key.der`** — 2048-bit RSA PKCS#8 DER private key (test only)
- **`tests/fixtures/test_cert.der`** — self-signed X.509 certificate, CN=Test Signer (test only)

### Modified files

- **`Cargo.toml`** — added `rsa 0.9`, `x509-cert 0.2` optional deps; `signatures = ["dep:rsa", "dep:x509-cert", "crypto", "writer"]`
- **`src/lib.rs`** — added `#[cfg(feature = "signatures")] pub mod signatures`
- **`src/wasm/editor.rs`** — added `sign_pdf` and `verify_signatures` WASM bindings (feature-gated)
- **`src/editor/text_model.rs`** — added `#[allow(dead_code)]` to pre-existing `tm` field
- **`src/editor/document_editor.rs`** — added `#[cfg_attr(not(feature = "crypto"), allow(unused_mut))]` for pre-existing issue

## Design Decisions

- **Manual DER encoding** rather than `der` crate: the `der` crate is a proc-macro-heavy pull that constrains the type structure. CMS SignedData needs very precise control over which fields are [0] IMPLICIT vs SET-rewrapped (RFC 5652 §5.4), and hand-rolled TLV is 300 lines vs fighting the derive macros.

- **Placeholder strategy**: `ByteRange` uses four `PdfObject::Integer(999_999_999_999)` values, which serialize to exactly 53 bytes (the placeholder constant). `Contents` uses `PdfObject::String(vec![0u8; 8192])`, which the serializer hex-encodes as `<000…0>` (16384 zeros). Both are unique in any valid PDF and patchable in-place without shifting offsets.

- **`signing_time: Option<u64>`** — WASM has no system clock; passing `None` omits the optional `signingTime` attribute rather than requiring `wasm-bindgen-futures` or a JS callback.

- **`RESERVED_SIG_SIZE = 8192`** — covers a 4096-bit RSA signature (512 bytes) plus a typical certificate chain (≈ 2–4 KB) with headroom. Exceeding this is an explicit `PdfError` rather than silent truncation.

- **`signatures` feature includes `writer`** — `sign_document` opens the document through `PdfEditor` (which requires `writer`) and calls `save_append`. The dependency is explicit rather than implied.

- **Verifier `find_certificate` returns `val` directly** — the builder writes the cert DER directly inside a `[0]` context tag (`der_ctx0(cert_der)`). The verifier reads the `[0]` body as-is; no additional `der_parse` unwrap is needed.

- **RFC 5652 §5.4 signing**: signedAttrs are signed as a SET (tag `0x31`), not as the `[0] IMPLICIT` form stored in the `SignerInfo`. Both builder and verifier apply the tag substitution explicitly.

## Test Coverage

### Unit tests (`src/signatures/signer.rs`)

| Test | Coverage |
|------|----------|
| `format_byterange_correct_length` | Output is exactly 53 bytes, starts with `[`, ends with `]` |
| `format_byterange_parses_back` | The four numbers round-trip through the padded format |
| `find_contents_placeholder_found` | Locates `<000…0>` at the correct byte offset |
| `find_byterange_placeholder_found` | Locates the 53-byte placeholder |
| `hex_encode_smoke` | `[0xde, 0xad, 0xbe, 0xef]` → `"DEADBEEF"` |

### Unit tests (`src/signatures/cms.rs`)

| Test | Coverage |
|------|----------|
| `der_len_short` | Short-form DER length (< 128) |
| `der_len_long` | Long-form DER length (128, 256) |
| `der_int_pos_positive` | High-bit bytes get `0x00` prefix |
| `der_parse_roundtrip` | Encode then parse a SEQUENCE |
| `utctime_smoke` | Unix epoch 0 → `700101000000Z` |

### Integration tests (`tests/signatures.rs`)

| Test | Coverage |
|------|----------|
| `sign_and_verify_round_trip` | Signs `minimal.pdf`; verifies; asserts `signature_valid = true`, `covers_whole_file = true` |
| `tampered_pdf_fails_verification` | Flips byte 10 after signing; asserts `signature_valid = false` |

## Known Limitations / Follow-up

- **No CRL / OCSP stapling**: the embedded certificate is self-signed and carries no revocation information. Production use needs an OCSP response embedded in the `[1]` crls slot.
- **Single signer only**: `verify_signatures` returns after the first `signerInfo` entry. Multi-signer CMS documents are not handled.
- **No LTV (Long-Term Validation)**: PAdES-LTV requires Document Security Store (DSS) extension (ISO 32000-2 §12.8.4.3). Not in scope for Phase 2.
- **signing_time omitted on WASM**: Applications that need a provable timestamp must embed an RFC 3161 timestamp token, which requires a network call — deferred to a future phase.
- **4096-bit RSA limit**: `RESERVED_SIG_SIZE = 8192` fits 4096-bit RSA + one cert. Longer chains or 8192-bit keys would need a larger constant (and a re-sign of any existing documents).
