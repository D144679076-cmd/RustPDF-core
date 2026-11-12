//! Phase P4 (plan rank-improvement-2026-11-09): encrypt-on-write acceptance.
//!
//! Covers the plan's acceptance bullets:
//! - write-encrypted → decrypt with our own reader (round-trip, 3 suites)
//! - re-encrypt / same-handler full rewrite of an already-encrypted doc
//! - external sanity check with MuPDF (`mutool`), skipped when absent
//!
//! Requires `--features crypto,writer`.

#![cfg(all(feature = "crypto", feature = "writer"))]

use pdf_core::document::catalog::Catalog;
use pdf_core::parser::objects::{PdfDocument, PdfObject};
use pdf_core::writer::encrypt::{EncryptOptions, WriteAlgorithm};

/// Activate an Enterprise license once for the process so the gated
/// encryption path passes the license check. Idempotent.
fn ensure_enterprise_license() {
    use pdf_core::license::{activate, encode_license_key, validate_license_key, Tier};
    let key = encode_license_key(Tier::Enterprise, 0, "integration-test");
    let license = validate_license_key(&key).expect("test key must be valid");
    let _ = activate(license); // ignore error if already activated
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    ))
    .unwrap_or_else(|e| panic!("fixture {name} missing: {e}"))
}

/// First content-stream bytes of page 0 (uncompressed text_columns fixture).
fn page0_content_bytes(doc: &PdfDocument) -> Vec<u8> {
    let catalog = Catalog::from_document(doc).expect("catalog");
    let page_dict = catalog.get_page_dict(doc, 0).expect("page dict");
    let contents = page_dict
        .get("Contents")
        .expect("page has /Contents")
        .clone();
    match contents {
        PdfObject::Reference(id, _) => doc.get_stream_data(id).expect("stream data"),
        other => panic!("contents is not a reference: {other:?}"),
    }
}

/// All stream-body bytes keyed by object id (decrypted view). Used to prove
/// content survives re-encryption even for fixtures without /Contents.
fn all_stream_bodies(doc: &PdfDocument) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    for id in doc.all_object_ids() {
        if matches!(doc.get_object(id), Ok(PdfObject::Stream(_))) {
            let data = doc.get_stream_data(id).expect("stream data");
            out.push((id, data));
        }
    }
    out.sort_by_key(|(id, _)| *id);
    out
}

fn encrypt_and_parse(
    source: Vec<u8>,
    options: EncryptOptions,
    password: &str,
) -> (PdfDocument, Vec<u8>) {
    let mut editor = pdf_core::editor::PdfEditor::open(source).expect("open");
    ensure_enterprise_license();
    editor.set_encryption(options).expect("set_encryption");
    assert!(editor.encryption_pending());
    let out = editor.save_new().expect("save_new");
    let doc = PdfDocument::parse_with_password(out.clone(), password.as_bytes())
        .expect("parse encrypted output");
    (doc, out)
}

#[test]
fn fresh_encryption_roundtrip_rc4_128() {
    let source = fixture("text_columns.pdf");
    let original_content = page0_content_bytes(&PdfDocument::parse(source.clone()).unwrap());

    let (doc, out) = encrypt_and_parse(
        source,
        EncryptOptions {
            user_password: "secret".to_owned(),
            algorithm: WriteAlgorithm::Rc4_128,
            ..Default::default()
        },
        "secret",
    );
    assert_eq!(doc.page_count().unwrap(), 1);
    // Content stream decrypts back to the original bytes.
    assert_eq!(page0_content_bytes(&doc), original_content);
    // Output is genuinely encrypted: the raw bytes must not contain the
    // plaintext content.
    let needle = b"Alpha beta";
    assert!(
        !out.windows(needle.len()).any(|w| w == needle),
        "plaintext content leaked into encrypted output"
    );
    // Wrong password rejected.
    assert!(PdfDocument::parse_with_password(out, b"wrong").is_err());
}

#[test]
fn fresh_encryption_roundtrip_aes_128() {
    let source = fixture("text_columns.pdf");
    let original_content = page0_content_bytes(&PdfDocument::parse(source.clone()).unwrap());

    let (doc, out) = encrypt_and_parse(
        source,
        EncryptOptions {
            user_password: "secret".to_owned(),
            algorithm: WriteAlgorithm::Aes128,
            ..Default::default()
        },
        "secret",
    );
    assert_eq!(doc.page_count().unwrap(), 1);
    assert_eq!(page0_content_bytes(&doc), original_content);
    assert!(PdfDocument::parse_with_password(out, b"wrong").is_err());
}

#[test]
fn fresh_encryption_roundtrip_aes_256_r6() {
    let source = fixture("text_columns.pdf");
    let original_content = page0_content_bytes(&PdfDocument::parse(source.clone()).unwrap());

    let (doc, out) = encrypt_and_parse(
        source,
        EncryptOptions {
            user_password: "secret".to_owned(),
            algorithm: WriteAlgorithm::Aes256R6,
            ..Default::default()
        },
        "secret",
    );
    assert_eq!(doc.page_count().unwrap(), 1);
    assert_eq!(page0_content_bytes(&doc), original_content);
    assert!(PdfDocument::parse_with_password(out, b"wrong").is_err());
}

#[test]
fn fresh_encryption_owner_password_works() {
    // Owner password ≠ user password: the reader must accept the user
    // password; the /OE entry makes the owner password usable externally
    // (mutool check below). NOTE: opening with the owner password is not yet
    // supported by our own reader (no Algorithm 7 / owner-path fallback) —
    // known limitation, see .doc report.
    let source = fixture("text_columns.pdf");
    let opts = EncryptOptions {
        user_password: "user-pw".to_owned(),
        owner_password: Some("owner-pw".to_owned()),
        algorithm: WriteAlgorithm::Aes256R6,
        ..Default::default()
    };
    let (doc, out) = encrypt_and_parse(source, opts, "user-pw");
    assert_eq!(doc.page_count().unwrap(), 1);
    // Wrong password rejected.
    assert!(PdfDocument::parse_with_password(out, b"nope").is_err());

    // External sanity: MuPDF must open with the OWNER password too.
    if std::process::Command::new("mutool")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("mutool not available — skipping owner-password external check");
        return;
    }
    let path = std::env::temp_dir().join("p4_owner.pdf");
    // Rebuild deterministically enough for the check: re-run the flow.
    let source = fixture("text_columns.pdf");
    let opts = EncryptOptions {
        user_password: "user-pw".to_owned(),
        owner_password: Some("owner-pw".to_owned()),
        algorithm: WriteAlgorithm::Aes256R6,
        ..Default::default()
    };
    let (_, out) = encrypt_and_parse(source, opts, "user-pw");
    std::fs::write(&path, &out).expect("write temp pdf");
    assert!(
        mutool_opens(&path, "owner-pw"),
        "MuPDF failed to open with the owner password"
    );
    assert!(
        mutool_opens(&path, "user-pw"),
        "MuPDF failed to open with the user password"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn permissions_bit_reaches_the_encrypt_dict() {
    let source = fixture("text_columns.pdf");
    let mut editor = pdf_core::editor::PdfEditor::open(source).expect("open");
    ensure_enterprise_license();
    editor
        .set_encryption(EncryptOptions {
            user_password: "secret".to_owned(),
            permissions: pdf_core::crypto::handler::Permissions::default(), // nothing allowed
            algorithm: WriteAlgorithm::Aes256R6,
            ..Default::default()
        })
        .expect("set_encryption");
    let out = editor.save_new().expect("save_new");
    let doc = PdfDocument::parse_with_password(out, b"secret").expect("parse");
    // Read /P straight from the trailer /Encrypt dict and decode it with the
    // same parser the reader uses.
    let encrypt_dict = match doc.trailer.get("Encrypt") {
        Some(PdfObject::Reference(id, _)) => match doc.get_object(*id).expect("encrypt object") {
            PdfObject::Dictionary(d) => d,
            other => panic!("/Encrypt is not a dict: {other:?}"),
        },
        Some(PdfObject::Dictionary(d)) => d.clone(),
        other => panic!("/Encrypt missing: {other:?}"),
    };
    let p = encrypt_dict
        .get("P")
        .and_then(|v| v.as_integer())
        .expect("/P integer") as i32;
    let perms = pdf_core::crypto::handler::parse_permissions(p);
    assert!(!perms.can_print);
    assert!(!perms.can_copy_text);
}

#[test]
fn save_new_reencrypts_existing_encrypted_document() {
    // Full rewrite of an already-encrypted doc must keep its content
    // protected (the old TODO(crypto) bug produced plaintext output).
    let source = fixture("encrypted_aes256.pdf"); // password "test"
    let original_doc = PdfDocument::parse_with_password(source.clone(), b"test").unwrap();
    let original_bodies = all_stream_bodies(&original_doc);

    let mut editor = pdf_core::editor::PdfEditor::open_with_password(source, b"test").unwrap();
    assert!(!editor.encryption_pending());
    let out = editor.save_new().expect("save_new");

    // Still encrypted with the SAME password.
    let doc = PdfDocument::parse_with_password(out.clone(), b"test").expect("re-parse");
    assert_eq!(all_stream_bodies(&doc), original_bodies);
    // Old password still valid, empty/wrong rejected.
    assert!(PdfDocument::parse_with_password(out.clone(), b"").is_err());
    assert!(PdfDocument::parse_with_password(out, b"nope").is_err());
}

#[test]
fn fresh_encryption_replaces_source_encryption() {
    // set_encryption on an already-encrypted doc re-keys it with the NEW
    // password (full rewrite from decrypted content).
    let source = fixture("encrypted_aes256.pdf"); // password "test"
    let original_doc = PdfDocument::parse_with_password(source.clone(), b"test").unwrap();
    let original_bodies = all_stream_bodies(&original_doc);

    let mut editor = pdf_core::editor::PdfEditor::open_with_password(source, b"test").unwrap();
    editor
        .set_encryption(EncryptOptions {
            user_password: "new-secret".to_owned(),
            algorithm: WriteAlgorithm::Aes256R6,
            ..Default::default()
        })
        .expect("set_encryption");
    let out = editor.save_new().expect("save_new");

    let doc = PdfDocument::parse_with_password(out.clone(), b"new-secret").expect("new pw works");
    assert_eq!(all_stream_bodies(&doc), original_bodies);
    assert!(PdfDocument::parse_with_password(out, b"test").is_err());
}

// ── External sanity: MuPDF (same authority as the render snapshot refs) ─────

fn mutool_opens(input: &std::path::Path, password: &str) -> bool {
    // `mutool draw` renders page 1; wrong/absent password fails with
    // "Cannot decrypt object" style errors and a non-zero exit code.
    let ok = std::process::Command::new("mutool")
        .args(["draw", "-p", password, input.to_str().unwrap(), "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match ok {
        Ok(output) => output.status.success(),
        Err(_) => false, // mutool missing → caller skips
    }
}

#[test]
fn mutool_external_sanity() {
    let Ok(mutool) = std::process::Command::new("mutool").arg("-v").output() else {
        eprintln!("mutool not available — skipping external sanity check");
        return;
    };
    assert!(mutool.status.success());

    let dir = std::env::temp_dir();
    for (algorithm, name) in [
        (WriteAlgorithm::Rc4_128, "p4_rc4.pdf"),
        (WriteAlgorithm::Aes128, "p4_aes128.pdf"),
        (WriteAlgorithm::Aes256R6, "p4_aes256.pdf"),
    ] {
        let source = fixture("text_columns.pdf");
        let mut editor = pdf_core::editor::PdfEditor::open(source).expect("open");
        ensure_enterprise_license();
        editor
            .set_encryption(EncryptOptions {
                user_password: "mupass".to_owned(),
                algorithm,
                ..Default::default()
            })
            .expect("set_encryption");
        let out = editor.save_new().expect("save_new");
        let path = dir.join(name);
        std::fs::write(&path, &out).expect("write temp pdf");

        assert!(
            mutool_opens(&path, "mupass"),
            "MuPDF failed to open {name} with the correct password"
        );
        assert!(
            !mutool_opens(&path, "wrongpw"),
            "MuPDF opened {name} with a wrong password"
        );
        let _ = std::fs::remove_file(&path);
    }
}
