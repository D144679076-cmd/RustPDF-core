# license-env — Implementation Report

**Date:** 2026-06-09
**Scope:** License secret env injection, WASM expiry fix, frontend activation UI

## What Was Implemented

### Rust (`pdf-editor-rust-core/`)
- `build.rs` — reads `PDF_CORE_LICENSE_SECRET` from env at compile time, injects via `cargo:rustc-env`; falls back to placeholder so builds without the var still compile
- `src/license/mod.rs` — `LICENSE_SECRET` now uses `env!("PDF_CORE_LICENSE_SECRET").as_bytes()` instead of hardcoded bytes
- `src/license/mod.rs` — `active_license() -> Option<&'static License>` accessor added
- `src/license/mod.rs` — `validate_license_key_with_time(key, now_unix_secs)` extracted from `validate_license_key`; accepts explicit timestamp so WASM can pass `Date.now()`
- `src/license/mod.rs` — `validate_license_key` refactored to delegate to `validate_license_key_with_time`
- `src/wasm/mod.rs` — `activate_license(key, now_unix_secs: f64)` now accepts JS timestamp for expiry checking
- `src/wasm/mod.rs` — `current_license_info() -> String` added; returns JSON with tier, licensee, expiry
- `pdf-editor-rust-core/.env.example` — documents `PDF_CORE_LICENSE_SECRET`
- `scripts/generate-key.sh` — convenience wrapper that sources `.env` and runs `keygen`

### Frontend (`web-editor/`)
- `src/stores/useLicenseStore.ts` — Pinia store: `tier`, `licensee`, `expiry`, `activationError`; `isPro`/`isEnterprise`/`isExpired` computed; `hydrate()` + `activate(key)` actions
- `src/boot/wasm.ts` — after `init_logging()`, activates `VITE_PDF_CORE_LICENSE_KEY` if set, then hydrates the license store with `current_license_info()`
- `src/components/LicenseActivation.vue` — modal: key input field, calls `licenseStore.activate()`, shows validation errors inline
- `src/components/LicenseStatusBadge.vue` — toolbar chip showing current tier; clicking it opens `LicenseActivation`
- `src/layouts/MainLayout.vue` — `LicenseStatusBadge` added to `header-actions`
- `web-editor/.env.example` — documents `VITE_PDF_CORE_LICENSE_KEY`

## Design Decisions

- **`build.rs` fallback to placeholder** — `cargo build` without the env var still compiles. Useful for CI jobs that run clippy/tests without needing a real secret. The fallback is the same string that was previously hardcoded, so existing test keys continue to work in that context.
- **`validate_license_key_with_time` not `validate_license_key(key, now)`** — keeping the zero-argument `validate_license_key` intact avoids breaking all native callers (tests, keygen binary). The new function is additive.
- **`now_unix_secs: f64` in WASM binding** — `wasm_bindgen` doesn't support `u64` across the JS boundary; `f64` covers all practical Unix timestamps safely (precise to 2^53 seconds ≈ year 285 million).
- **`current_license_info()` returns JSON string** — avoids `wasm_bindgen` struct serialisation complexity. The frontend `JSON.parse()` call is a single line with a known shape.
- **`VITE_PDF_CORE_LICENSE_KEY` activation is silent on failure** — a bad env key logs a warning but does not crash the app. The user still gets the Free tier and can activate manually.

## Test Coverage

New tests in `src/license/mod.rs`:
- `validate_with_time_rejects_expired` — `now > expiry` returns `Err`
- `validate_with_time_accepts_not_yet_expired` — `now < expiry` returns `Ok`
- `validate_with_time_zero_skips_expiry` — `now=0` skips check even for past expiry

All 342 existing tests pass unchanged.

## Known Limitations / Follow-up

- `VITE_PDF_CORE_LICENSE_KEY` is embedded in the JS bundle — anyone with browser devtools can read it. Acceptable for trusted/internal deployments; not suitable for a public SaaS with per-user keys.
- Enterprise feature gates are not yet wired — `Tier::Enterprise` exists in the enum but no `require(Tier::Enterprise, …)` calls have been added (no enterprise-only features implemented yet).
- No key revocation mechanism — once a key is generated, only expiry can invalidate it.
- The `OnceLock` means `activate_license` can only succeed once per WASM session (page load). A second call returns an error. This is by design for tamper-resistance but means hot-reloading in dev will show "already activated" warnings.
