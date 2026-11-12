//! Digital signature support for PDF (ISO 32000-1 §12.8, RFC 5652 PKCS#7/CMS).
//!
//! Gated behind the `signatures` feature flag; requires an Enterprise license.
//!
//! ## Signing
//! ```ignore
//! use pdf_core::signatures::{SignatureOptions, sign_document};
//! let signed_pdf = sign_document(&pdf_bytes, &pkcs8_key, &cert_der, &options)?;
//! ```
//!
//! ## Verification
//! ```ignore
//! use pdf_core::signatures::verify_signatures;
//! for result in verify_signatures(&signed_pdf)? {
//!     println!("{}: valid={}", result.field_name, result.signature_valid);
//! }
//! ```
//!
//! ## Long-term validation material
//! `dss::parse_dss` exposes the Document Security Store (`Catalog /DSS`,
//! ISO 32000-2 §12.8.4) — pooled certs/OCSP/CRLs + per-signature `/VRI` —
//! read-only, lazily resolved.

pub(crate) mod cms;
pub(crate) mod cms_parse;
pub mod dss;
pub mod signer;
pub mod verifier;

pub use dss::{parse_dss, DocumentSecurityStore, DssItem, DssPayload, VriInfo};
pub use signer::{sign_document, SignatureOptions};
pub use verifier::{verify_signatures, SignatureVerification};
