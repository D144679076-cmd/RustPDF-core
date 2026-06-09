//! Error types for the PDF core library.
//!
//! Provides structured, descriptive error types with byte-offset context
//! to make debugging malformed PDFs straightforward.

/// The main error type for all PDF processing operations.
#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    /// A lexer-level error encountered during tokenization.
    #[error("Lexer error at byte offset {offset}: {message}")]
    LexerError {
        /// Byte offset in the input where the error was detected.
        offset: usize,
        /// Human-readable description of what went wrong.
        message: String,
    },

    /// The input ended unexpectedly while parsing a token.
    #[error("Unexpected end of input at byte offset {offset}: expected {expected}")]
    UnexpectedEof {
        /// Byte offset where EOF was encountered.
        offset: usize,
        /// What the parser was expecting to find.
        expected: String,
    },

    /// An invalid or unrecognized token was encountered.
    #[error("Invalid token at byte offset {offset}: {detail}")]
    InvalidToken {
        /// Byte offset of the invalid token.
        offset: usize,
        /// Description of why the token is invalid.
        detail: String,
    },

    /// A stream filter is not supported by this implementation.
    #[error("Unsupported filter '/{name}' at byte offset {offset}")]
    UnsupportedFilter {
        /// Byte offset of the stream object.
        offset: usize,
        /// Filter name (e.g. "CCITTFaxDecode").
        name: String,
    },

    /// A filter failed to decode its input data.
    #[error("Filter decode error at byte offset {offset}: {message}")]
    FilterError {
        /// Byte offset of the stream object.
        offset: usize,
        /// Description of the decode failure.
        message: String,
    },

    /// The document or object is encrypted and cannot be read without a password.
    #[error("Document is encrypted at byte offset {offset}")]
    Encrypted {
        /// Byte offset where the encryption dictionary was found.
        offset: usize,
    },

    /// A serialization or write operation failed.
    #[error("Write error: {message}")]
    WriteError {
        /// Human-readable description of the write failure.
        message: String,
    },

    /// A structural constraint was violated during editing.
    #[error("Invalid structure: {message}")]
    InvalidStructure {
        /// Description of the violated constraint.
        message: String,
    },

    /// A Pro or Enterprise license is required to use this feature.
    #[error("feature '{feature}' requires a Pro or Enterprise license")]
    LicenseRequired {
        /// The name of the feature that requires a higher tier.
        feature: &'static str,
    },
}

impl PdfError {
    /// Create a lexer error at the given offset with a descriptive message.
    pub fn lexer(offset: usize, message: impl Into<String>) -> Self {
        PdfError::LexerError {
            offset,
            message: message.into(),
        }
    }

    /// Create an unexpected-EOF error at the given offset.
    pub fn eof(offset: usize, expected: impl Into<String>) -> Self {
        PdfError::UnexpectedEof {
            offset,
            expected: expected.into(),
        }
    }

    /// Create an invalid-token error at the given offset.
    pub fn invalid_token(offset: usize, detail: impl Into<String>) -> Self {
        PdfError::InvalidToken {
            offset,
            detail: detail.into(),
        }
    }

    /// Create an unsupported-filter error.
    pub fn unsupported_filter(offset: usize, name: impl Into<String>) -> Self {
        PdfError::UnsupportedFilter {
            offset,
            name: name.into(),
        }
    }

    /// Create a filter-decode error.
    pub fn filter_error(offset: usize, message: impl Into<String>) -> Self {
        PdfError::FilterError {
            offset,
            message: message.into(),
        }
    }

    /// Create a write/serialization error.
    pub fn write_error(message: impl Into<String>) -> Self {
        PdfError::WriteError {
            message: message.into(),
        }
    }

    /// Create an invalid-structure error.
    pub fn invalid_structure(message: impl Into<String>) -> Self {
        PdfError::InvalidStructure {
            message: message.into(),
        }
    }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, PdfError>;
