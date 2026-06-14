//! Integration tests for the digital signatures feature (Phase 2).
//!
//! Requires: `--features signatures`
//! Fixtures: `tests/fixtures/test_key.der`, `tests/fixtures/test_cert.der`

#[cfg(feature = "signatures")]
mod tests {
    use pdf_core::signatures::{sign_document, verify_signatures, SignatureOptions};
    use std::fs;
    use std::path::PathBuf;

    fn fixture(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        fs::read(&path).unwrap_or_else(|e| panic!("cannot read fixture {name}: {e}"))
    }

    fn options() -> SignatureOptions {
        SignatureOptions {
            rect: [10.0, 10.0, 200.0, 50.0],
            page_index: 0,
            field_name: "Sig1".to_owned(),
            reason: Some("Test signing".to_owned()),
            location: None,
            contact_info: None,
        }
    }

    #[test]
    fn sign_and_verify_round_trip() {
        // Activate a test Enterprise license so require() passes.
        let key = pdf_core::license::encode_license_key(
            pdf_core::license::Tier::Enterprise,
            0,
            "Test Suite",
        );
        pdf_core::license::activate(
            pdf_core::license::validate_license_key(&key).expect("valid test key"),
        )
        .ok(); // ignore if already activated from another test

        let pdf = fixture("minimal.pdf");
        let private_key_der = fixture("test_key.der");
        let cert_der = fixture("test_cert.der");

        let signed = sign_document(&pdf, &private_key_der, &cert_der, &options())
            .expect("sign_document should succeed");

        assert!(
            signed.len() > pdf.len(),
            "signed PDF must be larger than the original"
        );

        let results = verify_signatures(&signed).expect("verify_signatures should succeed");
        assert_eq!(results.len(), 1, "should find exactly one signature field");

        let result = &results[0];
        assert_eq!(result.field_name, "Sig1");
        assert!(
            result.signature_valid,
            "signature must be mathematically valid; error: {:?}",
            result.error
        );
        assert!(
            result.covers_whole_file,
            "signature must cover the whole file"
        );
    }

    #[test]
    fn tampered_pdf_fails_verification() {
        let key = pdf_core::license::encode_license_key(
            pdf_core::license::Tier::Enterprise,
            0,
            "Test Suite",
        );
        pdf_core::license::activate(
            pdf_core::license::validate_license_key(&key).expect("valid test key"),
        )
        .ok();

        let pdf = fixture("minimal.pdf");
        let private_key_der = fixture("test_key.der");
        let cert_der = fixture("test_cert.der");

        let mut signed = sign_document(&pdf, &private_key_der, &cert_der, &options())
            .expect("sign_document should succeed");

        // Flip a byte in the signed region (near the beginning, which is in range 1).
        if signed.len() > 10 {
            signed[10] ^= 0xff;
        }

        let results = verify_signatures(&signed).expect("verify_signatures should not crash");
        assert_eq!(results.len(), 1);
        assert!(
            !results[0].signature_valid,
            "tampered PDF should fail verification"
        );
    }
}
