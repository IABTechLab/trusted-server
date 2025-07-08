//! Error types for the trusted server.
//!
//! This module provides the main error type [`TrustedServerError`] used throughout
//! the application. All errors are designed to work with the `error-stack` crate
//! for rich error context and reporting.

use core::error::Error;
use derive_more::Display;
use http::StatusCode;

/// The main error type for trusted server operations.
///
/// This enum encompasses all possible errors that can occur during
/// request processing, configuration, and data handling.
#[allow(dead_code)]
#[derive(Debug, Display)]
pub enum TrustedServerError {
    /// Configuration errors that prevent the server from starting.
    #[display("Configuration error: {message}")]
    Configuration { message: String },

    /// The synthetic secret key is using the insecure default value.
    #[display("Synthetic secret key is set to the default value - this is insecure")]
    InsecureSecretKey,

    /// Invalid UTF-8 data encountered.
    #[display("Invalid UTF-8 data: {message}")]
    InvalidUtf8 { message: String },

    /// HTTP header value creation failed.
    #[display("Invalid HTTP header value: {message}")]
    InvalidHeaderValue { message: String },

    /// Settings parsing or validation failed.
    #[display("Settings error: {message}")]
    Settings { message: String },

    /// GDPR consent handling error.
    #[display("GDPR consent error: {message}")]
    GdprConsent { message: String },

    /// Synthetic ID generation or validation failed.
    #[display("Synthetic ID error: {message}")]
    SyntheticId { message: String },

    /// Prebid integration error.
    #[display("Prebid error: {message}")]
    Prebid { message: String },

    /// Key-value store operation failed.
    #[display("KV store error: {store_name} - {message}")]
    KvStore { store_name: String, message: String },

    /// Template rendering error.
    #[display("Template error: {message}")]
    Template { message: String },
}

impl Error for TrustedServerError {}

/// Extension trait for converting [`TrustedServerError`] to HTTP responses.
#[allow(dead_code)]
pub trait IntoHttpResponse {
    /// Convert the error into an HTTP status code.
    fn status_code(&self) -> StatusCode;

    /// Get the error message to show to users (uses the Display implementation).
    fn user_message(&self) -> String;
}

impl IntoHttpResponse for TrustedServerError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Configuration { .. } | Self::Settings { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InsecureSecretKey => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InvalidUtf8 { .. } => StatusCode::BAD_REQUEST,
            Self::InvalidHeaderValue { .. } => StatusCode::BAD_REQUEST,
            Self::GdprConsent { .. } => StatusCode::BAD_REQUEST,
            Self::SyntheticId { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Prebid { .. } => StatusCode::BAD_GATEWAY,
            Self::KvStore { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Template { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn user_message(&self) -> String {
        // Use the Display implementation which already has the specific error message
        self.to_string()
    }
}
