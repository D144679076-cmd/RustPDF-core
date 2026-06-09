# Phase 1 — AES-256 Decryption

**Status:** Complete — 2026-06-05 (see `.doc/crypto-aes256-2026-06-05.md`)
**Effort:** ~2 days
**Tier gate:** Pro (add `license::require(Tier::Pro, "aes256_decrypt")?` in handler after fix)

## Context

File-key derivation for AES-256 R5/R6 is already fully implemented in `src/crypto/aes256.rs` (`derive_file_key_r5`, `derive_file_key_r6`). The bug is an early `Err(PdfError::Encrypted)` return in `src/crypto/handler.rs` that prevents reaching the actual decryption code.

Per ISO 32000-2 §7.6.5: AES-256 (R5/R6) uses the **file key directly** on each object — no per-object key derivation is needed (unlike AES-128 which calls `object_key_aes()`).

## Files to Modify

### `src/crypto/handler.rs`

1. **In `from_trailer()`** — find the block that early-returns `Err(Encrypted)` when `V == 5` or `R == 5 || R == 6`. Remove it. The existing key derivation call already handles R5/R6 correctly — just let it return `Ok(Some(handler))`.

2. **In `decrypt_string(&mut self, obj_num: u32, gen: u16, data: &mut Vec<u8>)`** — add the AES-256 arm:
   ```rust
   EncryptAlgorithm::Aes256 => {
       *data = super::aes256::aes256_cbc_decrypt(&self.file_key, data)?;
   }
   ```

3. **In `decrypt_stream(&mut self, obj_num: u32, gen: u16, data: &mut Vec<u8>)`** — same arm:
   ```rust
   EncryptAlgorithm::Aes256 => {
       *data = super::aes256::aes256_cbc_decrypt(&self.file_key, data)?;
   }
   ```

### `src/crypto/aes256.rs`

Verify `aes256_cbc_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>>`:
- First 16 bytes of `data` = IV.
- Remaining bytes = ciphertext.
- Decrypts using AES-256-CBC from the `aes` + `cbc` crates (already imported under `crypto` feature).
- Removes PKCS7 padding from output.
- If the function is not yet public, change `pub(crate)` to `pub`.

## Test Fixture

Create `tests/fixtures/encrypted_aes256.pdf` using:
```bash
qpdf --encrypt test test 256 -- tests/fixtures/minimal.pdf tests/fixtures/encrypted_aes256.pdf
```

## Tests to Add in `tests/real_pdf.rs`

```rust
#[cfg(feature = "crypto")]
#[test]
fn aes256_encrypted_pdf_opens_with_password() {
    let data = include_bytes!("fixtures/encrypted_aes256.pdf").to_vec();
    let doc = PdfDocument::parse_with_password(data, b"test").unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);
}

#[cfg(feature = "crypto")]
#[test]
fn aes256_encrypted_pdf_wrong_password_fails() {
    let data = include_bytes!("fixtures/encrypted_aes256.pdf").to_vec();
    let result = PdfDocument::parse_with_password(data, b"wrong");
    assert!(result.is_err());
}
```

## Verification

```bash
cargo test --features crypto -- aes256
cargo build --target wasm32-unknown-unknown --features wasm,crypto
```
