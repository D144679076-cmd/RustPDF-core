# Permissions Enforcement — Implementation Report

**Date:** 2026-06-16
**Scope:** Phase 2 — Document Permissions Enforcement

## What Was Implemented

### New Types and Functions
- `Permissions` struct (`src/crypto/handler.rs`) — typed representation of the `/P` integer: 8 bool fields for the ISO 32000-1 §7.6.3.3 Table 22 bits.
- `parse_permissions(p: i32) -> Permissions` — extracts bits 3, 4, 5, 6, 9, 10, 11, 12 from the integer.

### PdfDocument Changes (`src/parser/objects.rs`)
- `permissions: Option<Permissions>` field added under `#[cfg(feature = "crypto")]`.
- `parse_with_password` now reads `/P` from the resolved Encrypt dict after `EncryptionHandler::from_trailer` returns `Some(…)`, calls `parse_permissions`, and stores the result. Works for all revisions (RC4 R2–4 and AES-256 R5–6).
- `Clone` impl updated to copy the `permissions` field.
- `permissions() -> Option<Permissions>` getter added (crypto-gated).

### WASM Permission Guard (`src/wasm/mod.rs`)
- `check_permission(doc, selector_fn, feature_name)` — crypto-gated free function; returns `Err(JsError)` when the doc has permissions and the selector returns false; `Ok(())` otherwise (unencrypted = no restrictions).

### WASM Editor Checks (`src/wasm/editor.rs`)
Permission guard added at entry of:
| Method | Permission |
|--------|-----------|
| `add_text_annotation`, `add_text_box`, `add_highlight`, `add_strikeout`, `add_link`, `add_underline`, `add_redact`, `add_stamp`, `add_file_attachment`, `add_ink` | `can_annotate` |
| `set_field_value`, `import_fdf`, `import_xfdf` | `can_fill_forms` |
| `add_blank_page`, `delete_page`, `move_page`, `extract_pages` | `can_assemble` |
| `set_metadata` | `can_modify` |
- `get_permissions() -> String` method added (JSON object with 6 keys; all-true for unencrypted docs).

### WASM Document Checks (`src/wasm/document.rs`)
- `extract_text`, `search_text` guarded with `can_copy_text`.
- `get_permissions() -> String` method added (same shape as editor variant).

### Test Fixture
- `tests/gen_restricted_fixture.py` — pure-stdlib Python script that computes valid RC4 R=3 O/U entries for password `"user"` with P = −3904 (all operations denied) and writes `tests/fixtures/restricted.pdf`.
- `tests/fixtures/restricted.pdf` generated (677 bytes).

### Tests
- `src/crypto/handler.rs` — 5 new unit tests: `parse_permissions_deny_all`, `parse_permissions_allow_all`, `parse_permissions_print_only`, `parse_permissions_p_minus_3904`, `parse_permissions_individual_bits`.
- `tests/real_pdf.rs` — 3 new integration tests: `unencrypted_pdf_has_no_permission_restrictions`, `restricted_pdf_permissions_parsed`, `aes256_encrypted_pdf_permissions_parsed`.

## Design Decisions

- **`Option<Permissions>` not `Cell`**: Set once at construction; `Option` in the struct is simpler than `Cell`. Field is `Copy` so cloning is trivial.
- **Read P after `from_trailer`**: Re-reads `/P` from the already-resolved Encrypt dict in the trailer rather than threading the value through `EncryptionHandler`. Avoids changing the public API of `EncryptionHandler`.
- **`check_permission` in `wasm/mod.rs`**: Shared by both `editor.rs` and `document.rs` without introducing a new sub-module.
- **Unencrypted = no restrictions**: `check_permission` only denies when `doc.permissions()` returns `Some`. Callers that open unencrypted PDFs are never blocked.
- **All guard blocks are `#[cfg(feature = "crypto")]`**: Keeps the non-crypto build identical in behavior.

## Test Coverage

| Test | Path |
|------|------|
| `parse_permissions_deny_all` (happy path) | `src/crypto/handler.rs` |
| `parse_permissions_allow_all` | `src/crypto/handler.rs` |
| `parse_permissions_print_only` | `src/crypto/handler.rs` |
| `parse_permissions_p_minus_3904` | `src/crypto/handler.rs` |
| `parse_permissions_individual_bits` | `src/crypto/handler.rs` |
| `unencrypted_pdf_has_no_permission_restrictions` | `tests/real_pdf.rs` |
| `restricted_pdf_permissions_parsed` (RC4 fixture) | `tests/real_pdf.rs` |
| `aes256_encrypted_pdf_permissions_parsed` | `tests/real_pdf.rs` |

## Known Limitations / Follow-up

- The `restricted.pdf` fixture is valid for password authentication but the object content is not encrypted (only the Encrypt dict matters for this test). A fully encrypted fixture would require encrypting all string/stream objects, which is complex without the writer's encryption support.
- `delete_annotation` and `merge` WASM methods are not yet implemented in the WASM layer; checks will be added when those methods are added.
- `can_print` is enforced only at the permissions-query level; server-side PDF rendering does not yet gate on `can_print`.
- AES-256 PDFs (R5/R6): the `/Perms` entry can override `/P` per ISO 32000-2 §7.6.5; this implementation reads only `/P`.
