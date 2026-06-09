fn main() {
    // Inject the license HMAC secret at compile time.
    // Set PDF_CORE_LICENSE_SECRET in your shell or CI secrets before building.
    // Falls back to the placeholder so `cargo build` without the var still compiles
    // (useful for CI jobs that don't need real licensing, e.g. WASM size checks).
    let secret = std::env::var("PDF_CORE_LICENSE_SECRET")
        .unwrap_or_else(|_| "REPLACE_BEFORE_PRODUCTION_DO_NOT_SHIP_THIS".to_string());
    println!("cargo:rustc-env=PDF_CORE_LICENSE_SECRET={secret}");
    println!("cargo:rerun-if-env-changed=PDF_CORE_LICENSE_SECRET");
}
