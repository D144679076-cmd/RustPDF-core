//! Generate the PDF 2.0 fixtures committed under `tests/fixtures/`.
//!
//! One-time generation tool (kept for reproducibility — re-run after writer
//! changes and diff the output):
//!
//! ```sh
//! cargo run --features writer --example generate_pdf2_fixtures
//! ```
//!
//! Produces:
//! - `pdf2-minimal.pdf` — `%PDF-2.0` header, catalog `/Version 2.0`, one page.
//! - `pdf2-associated-files.pdf` — same + document-level `/AF` with a
//!   `Supplement` Filespec carrying a real embedded UTF-8 text file.
//! - `pdf2-signed-ltv.pdf` — Document Security Store (`Catalog /DSS`: pooled
//!   `/Certs` `/OCSPs` `/CRLs` + one `/VRI` entry). The signature *field* holds
//!   opaque placeholder bytes: this fixture exists to exercise **DSS
//!   parsing** (PDF 2.0 Phase D), not CMS verification — cryptographically
//!   valid signing fixtures require the write-side signing path.
//!
//! The `pdf2-encrypted-r6.pdf` fixture from the plan is deferred until the
//! writer implements R6 encryption (plan §7: read side first, write side
//! second); the R6 *read* path stays covered by crafted-bytes unit tests.

use pdf_core::document::associated_files::AfRelationship;
use pdf_core::parser::objects::{PdfDict, PdfObject};
use pdf_core::writer::{EmitVersion, PdfWriter};

fn pages_with_one_page(writer: &mut PdfWriter) -> u32 {
    // Empty page content stream (unfiltered — fixture stays inspectable).
    let content = writer.add_object(PdfObject::Stream(Box::new(
        pdf_core::writer::streams::make_raw_stream(Vec::new(), {
            let mut d = PdfDict::new();
            d.insert("Length".to_owned(), PdfObject::Integer(0));
            d
        }),
    )));

    let page_id = writer.add_object(PdfObject::Dictionary({
        let mut d = PdfDict::new();
        d.insert("Type".to_owned(), PdfObject::Name("Page".to_owned()));
        d.insert(
            "MediaBox".to_owned(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(612),
                PdfObject::Integer(792),
            ]),
        );
        d.insert("Contents".to_owned(), PdfObject::Reference(content, 0));
        d
    }));

    writer.add_object(PdfObject::Dictionary({
        let mut d = PdfDict::new();
        d.insert("Type".to_owned(), PdfObject::Name("Pages".to_owned()));
        d.insert(
            "Kids".to_owned(),
            PdfObject::Array(vec![PdfObject::Reference(page_id, 0)]),
        );
        d.insert("Count".to_owned(), PdfObject::Integer(1));
        d
    }))
}

fn catalog(writer: &mut PdfWriter) -> u32 {
    let pages_id = pages_with_one_page(writer);
    writer.add_object(PdfObject::Dictionary({
        let mut d = PdfDict::new();
        d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
        d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
        d
    }))
}

fn main() -> pdf_core::Result<()> {
    // --- pdf2-minimal.pdf -----------------------------------------------------
    let bytes = {
        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);
        let cat = catalog(&mut writer);
        writer.serialize_all(cat, None, None)?
    };
    std::fs::write("tests/fixtures/pdf2-minimal.pdf", &bytes).map_err(|e| {
        pdf_core::error::PdfError::write_error(format!("fixture write failed: {e}"))
    })?;
    println!("pdf2-minimal.pdf  ({} bytes)", bytes.len());

    // --- pdf2-associated-files.pdf --------------------------------------------
    let bytes = {
        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);
        let pages_id = pages_with_one_page(&mut writer);
        // Attach /AF to the catalog: created after children by construction.
        let spec = writer.add_associated_file(
            AfRelationship::Supplement,
            "dataset.csv",
            b"name,value\nalpha,1\nbeta,2\n",
        )?;
        let cat = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d.insert(
                "AF".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(spec, 0)]),
            );
            d
        }));
        writer.serialize_all(cat, None, None)?
    };
    std::fs::write("tests/fixtures/pdf2-associated-files.pdf", &bytes).map_err(|e| {
        pdf_core::error::PdfError::write_error(format!("fixture write failed: {e}"))
    })?;
    println!("pdf2-associated-files.pdf  ({} bytes)", bytes.len());

    // --- pdf2-signed-ltv.pdf (DSS fixture; placeholder signature bytes) --------
    let bytes = {
        let mut writer = PdfWriter::new();
        writer.set_emit_version(EmitVersion::V2_0);
        let pages_id = pages_with_one_page(&mut writer);

        // Pooled revocation material — ASCII stand-ins for DER blobs.
        let cert = writer.add_object(PdfObject::Stream(Box::new(
            pdf_core::writer::streams::make_raw_stream(b"DER-CERT-POOL".to_vec(), PdfDict::new()),
        )));
        let ocsp = writer.add_object(PdfObject::Stream(Box::new(
            pdf_core::writer::streams::make_raw_stream(b"DER-OCSP-POOL".to_vec(), PdfDict::new()),
        )));
        let crl = writer.add_object(PdfObject::Stream(Box::new(
            pdf_core::writer::streams::make_raw_stream(b"DER-CRL-POOL".to_vec(), PdfDict::new()),
        )));
        let ts = writer.add_object(PdfObject::Stream(Box::new(
            pdf_core::writer::streams::make_raw_stream(b"DER-TS-TOKEN".to_vec(), PdfDict::new()),
        )));

        let vri = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert(
                "Cert".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(cert, 0)]),
            );
            d.insert(
                "OCSP".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(ocsp, 0)]),
            );
            d.insert("TS".to_owned(), PdfObject::Reference(ts, 0));
            d
        }));
        let dss = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert(
                "Certs".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(cert, 0)]),
            );
            d.insert(
                "OCSPs".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(ocsp, 0)]),
            );
            d.insert(
                "CRLs".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(crl, 0)]),
            );
            d.insert(
                "VRI".to_owned(),
                PdfObject::Dictionary({
                    let mut v = PdfDict::new();
                    // Hex SHA-1 placeholder for the signature /Contents digest.
                    v.insert(
                        "AB12CD34EF560000000000000000000000000000".to_owned(),
                        PdfObject::Reference(vri, 0),
                    );
                    v
                }),
            );
            d
        }));

        // Signature field + AcroForm pointing at the DSS (envelope only —
        // /Contents bytes are placeholders, not a valid CMS structure).
        let sigstream = writer.add_object(PdfObject::Stream(Box::new(
            pdf_core::writer::streams::make_raw_stream(vec![0u8; 128], {
                let mut d = PdfDict::new();
                d.insert("Type".to_owned(), PdfObject::Name("Sig".to_owned()));
                d.insert(
                    "ByteRange".to_owned(),
                    PdfObject::Array(vec![
                        PdfObject::Integer(0),
                        PdfObject::Integer(64),
                        PdfObject::Integer(128),
                        PdfObject::Integer(64),
                    ]),
                );
                d
            }),
        )));
        let sigfield = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("FT".to_owned(), PdfObject::Name("Sig".to_owned()));
            d.insert("T".to_owned(), PdfObject::String(b"Signature1".to_vec()));
            d.insert("V".to_owned(), PdfObject::Reference(sigstream, 0));
            d
        }));
        let acro = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert(
                "Fields".to_owned(),
                PdfObject::Array(vec![PdfObject::Reference(sigfield, 0)]),
            );
            d
        }));

        let cat = writer.add_object(PdfObject::Dictionary({
            let mut d = PdfDict::new();
            d.insert("Type".to_owned(), PdfObject::Name("Catalog".to_owned()));
            d.insert("Pages".to_owned(), PdfObject::Reference(pages_id, 0));
            d.insert("DSS".to_owned(), PdfObject::Reference(dss, 0));
            d.insert("AcroForm".to_owned(), PdfObject::Reference(acro, 0));
            d
        }));
        writer.serialize_all(cat, None, None)?
    };
    std::fs::write("tests/fixtures/pdf2-signed-ltv.pdf", &bytes).map_err(|e| {
        pdf_core::error::PdfError::write_error(format!("fixture write failed: {e}"))
    })?;
    println!("pdf2-signed-ltv.pdf  ({} bytes)", bytes.len());

    Ok(())
}
