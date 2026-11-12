# Encrypt on Write — Implementation Report

**Date:** 2026-11-12
**Scope:** Phase P4 (encrypt-on-write) of `.fix_plan/rank-improvement-plan-2026-11-09.md` — `src/writer/encrypt.rs` (new), `src/crypto/handler.rs`, `src/crypto/aes256.rs`, `src/editor/document_editor.rs`, `src/writer/document.rs`, `src/parser/objects.rs` + API wiring (wasm / REST / clients)

## What Was Implemented

### Compute-direction KDF (crypto layer)
- `crypto/handler.rs`: `ComputedLegacyEntries` + `compute_legacy_entries(user_pw, owner_pw, revision 2–4, key_bytes, p_flags, file_id)` — Algorithm 3 (`/O`, 20-round RC4 for R≥3), Algorithm 2 (file key, existing `derive_file_key` reused), Algorithm 4/5 (`/U`). Private helpers `compute_o_entry` / `compute_u_entry`.
- `crypto/aes256.rs`: `R5Entries` + `compute_entries_r5` / `compute_entries_r6` / core `compute_v5_entries` — Algorithms 10–13 (random 32-byte file key, `/U` = hash ‖ v-salt ‖ k-salt, `/UE` = AES-256-CBC zero-IV no-pad of the file key, `/O` (uses `U[0..48]` as hash input), `/OE`, `/Perms`). Helpers: `random_bytes` (getrandom with the same no-panic fallback as `random_iv`), `aes256_cbc_encrypt_zero_iv_32`, `aes256_encrypt_block` (single-block ECB for `/Perms`), `perms_clear_block` (qpdf layout: `P` LE 0–3, `FF FF FF FF` 4–7, `"Tadb"` 8–11, random 12–15).
- R6 passwords go through the existing `password::password_candidates_for_r6` (SASLprep normalisation, ISO 32000-2 §7.6.4.3.2), taking the first (normalised) candidate.

### Writer layer
- **`src/writer/encrypt.rs` (new, ~500 lines)**: `WriteAlgorithm` (`Rc4_128` V2/R3, `Aes128` V4/R4 with `/StdCF /AESV2`, `Aes256R6` V5/R6 with `/AESV3`) + `from_name`; `EncryptOptions` (default: AES-256 R6, all permissions, empty user password); `permissions_to_p` (inverse of `parse_permissions`, reserved/undefined high bits set per Table 22); `build_encryption` → `BuiltEncryption { dict, handler, file_id }` (pub(crate)); `encrypt_object_for_write(obj, id, gen, handler, encrypt_stream_bodies)` — recursive string/array/dict/stream crypt, moved from `document_editor` with the new bool flag (bodies already encrypted → leave verbatim; plaintext → encrypt).
- `writer/document.rs`: `serialize_all` refactored to `serialize_all_impl` + new `serialize_all_encrypted(root, info, encrypt_ref, &file_id)` which adds trailer `/Encrypt` (indirect) + `/ID` (16-byte string duplicated in the 2-element array).
- `writer/mod.rs`: `#[cfg(feature = "crypto")] pub mod encrypt;`

### Editor layer
- `PdfEditor` gains `encryption: Option<EncryptOptions>` + `set_encryption(impl Into<Option<EncryptOptions>>)` / `encryption_pending()` (both Enterprise-gated, RC4+PDF-2.0-output rejected early with a clear message in both `set_encryption` and `save_new`).
- `save_new` restructured into two passes:
  1. **Pool build** — writer overrides are plaintext; doc-sourced objects get their stream bodies **decrypted with the source handler** (at the object's original generation, via new `PdfDocument::object_generation(id)`) because `get_object` returns decrypted strings but still-encrypted bodies. Result: fully plaintext pool.
  2. **Encryption** — three cases: (a) `set_encryption` pending → fresh `/Encrypt` + uniform re-encrypt at written gen 0; (b) source document encrypted, no pending options → re-encrypt with the **source handler** (same file key, same `/Encrypt` dict, same `/ID` via new `source_file_id()` helper) — fixes the old `TODO(crypto)` where a full rewrite of an encrypted doc produced a structurally-encrypted-but-plaintext file; (c) neither → plain output.
  - `save_append` keeps its existing behaviour (re-encrypts new objects with the source handler); fresh encryption is full-rewrite-only (plaintext originals cannot be retroactively encrypted incrementally) — this is why `set_encryption` applies to `save_new` and the wasm surface exposes a dedicated `save_encrypted`.

### Parser layer
- `parser/objects.rs`: `pub fn object_generation(id) -> u16` (xref generation, 0 for free/unknown/compressed) — needed for correct source-handler body decryption.

### API wiring
- **WASM** (`wasm/editor.rs`): `WasmEditor::set_encryption(options_json)` / `clear_encryption()` / `encryption_pending()` / `save_encrypted()` (+ native `save_encrypted_bytes()`), with `parse_encryption_options` (hand-rolled JSON: `user_password`, `owner_password`, `algorithm`, nested `permissions` object via new `json_object_field` helper) and a warn in `save()` when encryption is pending but ignored by the incremental path.
- **REST**: `POST /api/v1/encrypt` (raw PDF body + query params `user_password`, `owner_password`, `algorithm`, `allow` = comma-separated permission list or `all`/`none`), returning encrypted PDF bytes; registered in routes + utoipa paths; 3 server tests (roundtrip, bad algorithm → 400, bad permission flag → 400).
- **Clients**: python `encrypt(pdf, user_password, *, owner_password, algorithm, allow)`, java `encrypt(pdf)` / `encrypt(pdf, userPassword, ownerPassword, algorithm, allow)`, dotnet `EncryptAsync(pdf, ...)`.

### Tests
- Unit: 4 legacy-compute round-trips through the reader verify oracle (`crypto::handler`), R5/R6 compute round-trips + `/Perms` layout (`crypto::aes256`), 4 dict-shape/self-verification tests per suite (`writer::encrypt`), `permissions_to_p`↔`parse_permissions` inverse test.
- Acceptance (`tests/encrypt_write.rs`, 8 tests): fresh-encrypt round-trip ×3 suites with content-stream byte compare against `text_columns.pdf` + plaintext-leak scan + wrong-password rejection; owner-password test (user pw via our reader, **owner pw verified externally via MuPDF**); `/P` permissions reach the `/Encrypt` dict; `save_new` re-encrypts an already-encrypted doc (same password, `""`/wrong rejected); `set_encryption` re-keys an encrypted doc; `mutool_external_sanity` (3 suites × correct/wrong password, skipped when mutool absent).
- WASM bridge (`tests/wasm_api.rs`): set/save_encrypted round-trip, clear_encryption (2 new tests + license fix for the 3 pre-existing annotation tests, see below).

## Design Decisions
- **qpdf as the compute-side authority.** pdf.js `crypto.js` only decrypts (no `/Perms` compute). `QPDF_encryption.cc` pinned the `/Perms` clear-block layout (byte 8 = `T`/`F` for EncryptMetadata) and the zero-IV/no-padding AES-256 mode for `/UE`/`/OE`.
- **Two-pass `save_new`.** First plan assumed one pass; the key discovery is that `doc.get_object` returns *decrypted strings but still-encrypted stream bodies* (bodies decrypt at `get_stream_data`). A single pass would double-encrypt bodies. Pass 1 normalises everything to plaintext (decrypting doc bodies with the source handler at the **original** generation), pass 2 encrypts uniformly at the **written** generation (0) — per-object keys (RC4/AES-128) must match what a reader derives from the *new* xref, not the original one.
- **Same-handler rewrite re-encrypts at gen 0 too** rather than copying encrypted bodies: mixed generations would break RC4/AES-128 readers, and the pool's written generation is uniformly 0.
- **Owner password ≠ user password:** written `/O`/`/OE`/`/Perms` are correct (MuPDF opens with the owner password). Our own *reader* does not yet open with an owner password (no Algorithm 7 / R6 owner-path fallback) — deferred, see limitations.
- **License tier Enterprise** for `set_encryption` + `save_new` encryption, mirroring signatures/PDF-A; bypassed in lib unit tests (`#[cfg(test)]`), activated explicitly in integration tests via `encode_license_key(Tier::Enterprise, …)` + `activate` (same pattern as `tests/merge_redact.rs`).
- **R5 (`compute_entries_r5`) implemented** alongside R6 even though the writer only emits R6 — completes the compute-side symmetry with the decrypt path and is covered by tests.
- **Pre-existing issues fixed in passing** (small, unblocking): `render/canvas.rs` empty-line-after-doc-comment clippy error (committed code, blocked `-D warnings`); `tests/wasm_api.rs` broken imports (`pdf_core::wasm::WasmDocument` — the re-exports disappeared in `d1a97d8`'s feature restructure; switched to `pdf_core::wasm::document::*` / `editor::*` module paths); the 3 annotation tests there were non-deterministically failing because `add_annotation` is Pro-gated and no license was activated in that process — added `ensure_enterprise_license()`.

## Test Coverage
- Happy path: 8 acceptance tests (3 suites round-trip, owner pw via MuPDF, permissions, re-encrypt ×2, mutool sanity ×3 suites), 2 wasm bridge tests, 3 REST tests.
- Error path: wrong password ×3 suites + mutool wrong-pw, bad revision (KDF), RC4+PDF-2.0 rejection, invalid REST algorithm/permission flags → 400, unknown wasm algorithm (unit-level on `WriteAlgorithm::from_name`).
- Regression: `save_new_reencrypts_existing_encrypted_document` pins the old `TODO(crypto)` bug (encrypted rewrite must not leak plaintext).

## Known Limitations / Follow-up
- **Reader does not open with the owner password** (legacy Algorithm 7 owner→user recovery and the R6 `/OE` path are not implemented in `EncryptionHandler::from_trailer`). Written files are correct (MuPDF-verified); a follow-up can add the owner path to the reader.
- `save_append` cannot fresh-encrypt (structural): fresh encryption is `save_new` only; documented on `set_encryption` and surfaced as a wasm warn on `save()`.
- The wasm-bindgen Err path (e.g. unknown algorithm JSON) cannot be integration-tested on the native rlib target — `JsError::new` → `__wbindgen_error_new` panics on non-wasm; covered at unit level instead.
- The `wasm` (js-actions/QuickJS) feature still cannot build on wasm32 (pre-existing; `wasm-no-js` is the verified no-JS payload per `d1a97d8`).
- RC4-40 (V=1/R=2) is decrypt-only; the writer offers RC4-128 as the weakest suite (PDF-2.0 output rejects all RC4).
