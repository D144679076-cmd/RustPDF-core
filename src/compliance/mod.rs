//! PDF/A compliance validation and conversion (ISO 19005-1/2/3).
//!
//! Provides validation against PDF/A-1b, 2b, and 3b standards, plus in-place
//! conversion for the common conformance requirements (output intents, XMP
//! metadata, JavaScript removal). Conversion requires the `writer` feature
//! and an Enterprise license.

pub mod icc;
pub mod pdfa;
pub mod xmp;

pub use pdfa::PdfAViolation;
pub use pdfa::{validate_pdfa_1b, validate_pdfa_2b, validate_pdfa_3b};

#[cfg(feature = "writer")]
pub use pdfa::{convert_to_pdfa_1b, convert_to_pdfa_2b, convert_to_pdfa_3b};
