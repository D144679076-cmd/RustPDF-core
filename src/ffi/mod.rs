//! Mobile SDK integration layer (Phase 4).
//!
//! Exposes pdf-core to native mobile platforms via two complementary mechanisms:
//! - [`c_api`] — raw `extern "C"` / `#[no_mangle]` functions (Phase 4a).
//! - [`uniffi_bridge`] — uniffi-annotated types for generated Swift/Kotlin
//!   wrappers (Phase 4b/4c). Enabled by the `mobile` feature.
//!
//! Neither submodule is compiled for WASM targets; guard any caller with
//! `#[cfg(not(target_arch = "wasm32"))]`.

#[cfg(all(feature = "ffi", not(target_arch = "wasm32")))]
pub mod c_api;

#[cfg(all(feature = "mobile", not(target_arch = "wasm32")))]
pub mod uniffi_bridge;
