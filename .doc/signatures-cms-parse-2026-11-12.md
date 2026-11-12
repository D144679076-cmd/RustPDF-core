# Signatures: CMS Structured Parser + Verifier Wiring — Implementation Report

**Date:** 2026-11-12
**Scope:** Phase 5 kickoff (PAdES LTV/DSS, plan §Phase 5 of `.fix_plan/rank-improvement-plan-2026-11-09.md`) — new `src/signatures/cms_parse.rs`, `src/signatures/verifier.rs` rewiring, `src/signatures/cms.rs` OID additions, `src/wasm/editor.rs` JSON field

## What Was Implemented

- **`signatures::cms_parse`** (new, ~310 lines): `parse_cms_signed_data(cms_der)` walks a complete CMS `ContentInfo` (RFC 5652) and returns `ParsedCms`:
  - `certificates: Vec<&[u8]>` — every certificate from the `[0]` block, handling both builder shapes (bare cert TLV in `[0]` IMPLICIT and `SET OF Certificate` chains) plus nested context wrappers; slices are **complete self-contained TLVs** (tag+length+value) so `x509_cert::Certificate::from_der` decodes them directly. *(Fix during work: initial version pushed value-only slices — real fixture round-trip caught it via "cert parse: unexpected ASN.1 DER tag".)*
  - `crls: Vec<&[u8]>` — CRL pool from `[1]`, same TLV discipline; consumed later by the revocation check (plan §7), extracted now to avoid re-parsing.
  - `signer: ParsedSignerInfo` — `digest_algorithm` + `signature_algorithm` OID value bytes, raw `signature`, `signed_attrs` (`[0]` value; re-wrap with `der_set` for the RFC 5652 §5.4 signed bytes), `message_digest` attribute value, and `timestamp_token` (complete `TimeStampToken` ContentInfo TLV from `id-aa-timeStampToken` unsigned attrs).
  - `timestamp_gen_time(token_der)` — best-effort `genTime` extraction from the token's `TSTInfo` (`ContentInfo → [0] SignedData → encapContentInfo → eContent → TSTInfo → genTime`, GeneralizedTime or UTCTime, returned as the encoded string). **The TSA token's own signature is not validated** — the result is untrusted metadata until chain validation (plan §7).
- **`signatures::verifier`** rewired:
  - `verify_cms_signed_data` now parses via `cms_parse` instead of hand-walking DER (removes the internal `find_certificate`/`find_message_digest`/`skip_context_tags` duplicates).
  - Digest selected from the signer's `digestAlgorithm` OID: SHA-256/384/512 hash the two `/ByteRange` ranges inside the verifier (digest-aware, was hardcoded SHA-256); **SHA-1 digest explicitly rejected** as deprecated, as is `sha1WithRSAEncryption` for `signatureAlgorithm`.
  - Accepts `sha256WithRSA`/`sha384WithRSA`/`sha512WithRSA` OIDs (used by real-world signers) in addition to the builder's `rsaEncryption`; verification key digest = `signatureAlgorithm`'s digest when implied, else the signerInfo digest.
  - Signer certificate = first entry of the parsed pool (builder emits the signer's cert first). *Logged limitation:* sid (issuer/serial) matching against the pool is chain-validation work, deferred.
  - `SignatureVerification` gains `timestamp: Option<String>` — RFC 3161 `genTime` when a timestamp token is present and parseable (parse failure logs `log::debug!` and degrades to `None`; the signature itself still verifies).
- **`signatures::cms`**: SHA-384/SHA-512/SHA-1 digest OIDs + `sha*WithRSAEncryption` / `id-ct-TSTInfo` OIDs made live (removed `#[allow(dead_code)]` reservations; SHA-1 pair used for explicit reject).
- **WASM surface**: `sigs_to_json` emits `"timestamp"` when present (additive JSON field).

## Design Decisions

- **Structured parser over hand-walk**: single source of truth for CMS layout; the verifier's hand-rolled walk duplicated builder knowledge and was fragile to shape changes (e.g. the `SET OF Certificate` chain shape the builder already emits was invisible to the old `find_certificate`).
- **Zero-copy slices, but self-contained TLVs**: borrowing keeps allocation off the hot path while the `cur[..cur.len() - rest.len()]` extent guarantees each slice decodes standalone — the bug class the round-trip fixture caught.
- **SHA-1 reject rather than ignore**: PAdES/ETSI EN 319 122 treat SHA-1 as broken; explicit typed error instead of silent fallback.
- **Timestamp surfaced as metadata, not validation**: matches plan §7 sequencing (chain validation phase owns TSA signature/nonce/integrity checks); exposing `genTime` now gives the UI/REST layer something real without false security claims.
- **No new dependencies**: all parsing via the existing hand-rolled DER primitives in `cms.rs` (R2: WASM-safe, no build changes).

## Test Coverage

- `cms_parse` unit tests (8, all in-module per R5):
  - Happy path: builder-shaped CMS parses; cert pool, digest/signature OIDs, signedAttrs, `messageDigest`, signature bytes all extracted correctly.
  - Chain shape: `[0] IMPLICIT SET OF Certificate` with two members yields 2 certs; no-attrs SignerInfo tolerated (`message_digest: None`).
  - Timestamp: `id-aa-timeStampToken` unsigned attr located; `genTime` walked out of a hand-built `TSTInfo` (`"20261112100000Z"`).
  - Error paths: non-`SignedData` contentType rejected ("id-signedData"), missing `signerInfos` rejected, truncated CMS rejected, truncated timestamp token rejected.
- `tests/signatures.rs` integration (existing, still green): `sign_and_verify_round_trip` proves the new parser path against a real RSA key + cert fixture (this is the test that caught the TLV-stripping bug); `tampered_pdf_fails_verification` unchanged.

## Known Limitations / Follow-up

- No sid (issuer+serial) → certificate pool matching; first-cert assumption documented.
- `crls` extracted but unused until the revocation check (chain-validation phase, plan §7): CRL check + OCSP fetch, `verifier.rs` skip removal, DSS writer, TSA nonce/integrity validation, `x509-cert`/`spki` path building.
- `timestamp_gen_time` does not validate the TSA signature or the messageImprint binding — documented in the function doc.
- WASM `wasm` feature (js-actions/rquickjs) cannot build on this host (`rquickjs-sys` bindgen needs `stdio.h` for wasm32) — pre-existing environment issue, unrelated; canonical `wasm-no-js*` targets build clean with `signatures`.
