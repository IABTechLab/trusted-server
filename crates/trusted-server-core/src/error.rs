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
    /// Client-side input/validation error resulting in a 400 Bad Request.
    ///
    /// **Note:** The `message` field is included in client-facing HTTP responses
    /// via [`IntoHttpResponse::user_message()`]. Keep it free of internal
    /// implementation details.
    #[display("Bad request: {message}")]
    BadRequest { message: String },
    /// Configuration errors that prevent the server from starting.
    #[display("Configuration error: {message}")]
    Configuration { message: String },

    /// Auction orchestration error.
    #[display("Auction error: {message}")]
    Auction { message: String },

    /// GAM (Google Ad Manager) integration error.
    #[display("GAM error: {message}")]
    Gam { message: String },
    /// GDPR consent handling error.
    ///
    /// **Note:** Unlike [`BadRequest`](Self::BadRequest), the detail `message`
    /// is intentionally suppressed in client-facing responses because consent
    /// strings may contain user data. Only the category name is returned.
    #[display("GDPR consent error: {message}")]
    GdprConsent { message: String },

    /// Invalid UTF-8 data encountered.
    #[display("Invalid UTF-8 data: {message}")]
    InvalidUtf8 { message: String },

    /// HTTP header value creation failed.
    #[display("Invalid HTTP header value: {message}")]
    InvalidHeaderValue { message: String },

    /// Key-value store operation failed.
    #[display("KV store error: {store_name} - {message}")]
    KvStore { store_name: String, message: String },

    /// Prebid integration error.
    #[display("Prebid error: {message}")]
    Prebid { message: String },

    /// Integration module error.
    #[display("Integration error ({integration}): {message}")]
    Integration {
        integration: String,
        message: String,
    },

    /// Proxy error.
    #[display("Proxy error: {message}")]
    Proxy { message: String },

    /// Request understood but not permitted — results in a 403 Forbidden response.
    #[display("Forbidden: {message}")]
    Forbidden { message: String },

    /// A redirect destination was blocked by the proxy allowlist.
    #[display("Redirect to `{host}` blocked: host not in proxy allowed_domains")]
    AllowlistViolation { host: String },

    /// Settings parsing or validation failed.
    #[display("Settings error: {message}")]
    Settings { message: String },

    /// Edge cookie ID generation or validation failed.
    #[display("Edge cookie error: {message}")]
    EdgeCookie { message: String },

    /// Requested partner was not found in the partner registry.
    #[display("Partner not found: {partner_id}")]
    PartnerNotFound { partner_id: String },

    /// A secret field still contains a known placeholder/default value.
    #[display("Insecure default value for: {field}")]
    InsecureDefault { field: String },
}

impl Error for TrustedServerError {}

/// Extension trait for converting [`TrustedServerError`] to HTTP responses.
#[allow(dead_code)]
pub trait IntoHttpResponse {
    /// Convert the error into an HTTP status code.
    fn status_code(&self) -> StatusCode;

    /// Get a safe, user-facing error message.
    ///
    /// Client errors (4xx) return a brief description; server/integration errors
    /// return a generic message. Full error details are preserved in server logs.
    fn user_message(&self) -> String;
}

impl IntoHttpResponse for TrustedServerError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Auction { .. } => StatusCode::BAD_GATEWAY,
            Self::BadRequest { .. } => StatusCode::BAD_REQUEST,
            Self::Configuration { .. } | Self::Settings { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Gam { .. } => StatusCode::BAD_GATEWAY,
            Self::GdprConsent { .. } => StatusCode::BAD_REQUEST,
            Self::InvalidHeaderValue { .. } => StatusCode::BAD_REQUEST,
            Self::InvalidUtf8 { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::KvStore { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Prebid { .. } => StatusCode::BAD_GATEWAY,
            Self::Integration { .. } => StatusCode::BAD_GATEWAY,
            Self::Proxy { .. } => StatusCode::BAD_GATEWAY,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::AllowlistViolation { .. } => StatusCode::FORBIDDEN,
            Self::EdgeCookie { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::PartnerNotFound { .. } => StatusCode::NOT_FOUND,
            Self::InsecureDefault { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn user_message(&self) -> String {
        match self {
            // Client errors (4xx) — safe to surface a brief description
            Self::BadRequest { message } => format!("Bad request: {message}"),
            // Consent strings may contain user data; return category only.
            Self::GdprConsent { .. } => "GDPR consent error".to_string(),
            Self::InvalidHeaderValue { .. } => "Invalid header value".to_string(),
            // Server/integration errors (5xx/502/503) — generic message only.
            // Full details are already logged via log::error! in to_error_response.
            _ => "An internal error occurred".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_code_returns_expected_http_status_for_each_variant() {
        let cases = [
            (
                TrustedServerError::BadRequest {
                    message: String::from("missing field"),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                TrustedServerError::Configuration {
                    message: String::from("missing setting"),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                TrustedServerError::Auction {
                    message: String::from("bid timeout"),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                TrustedServerError::Gam {
                    message: String::from("request failed"),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                TrustedServerError::GdprConsent {
                    message: String::from("missing consent string"),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                TrustedServerError::InvalidUtf8 {
                    message: String::from("invalid byte sequence"),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                TrustedServerError::InvalidHeaderValue {
                    message: String::from("non-ascii header"),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                TrustedServerError::KvStore {
                    store_name: String::from("sessions"),
                    message: String::from("timeout"),
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                TrustedServerError::Prebid {
                    message: String::from("adapter error"),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                TrustedServerError::Integration {
                    integration: String::from("example-integration"),
                    message: String::from("request failed"),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                TrustedServerError::Proxy {
                    message: String::from("upstream failed"),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                TrustedServerError::Forbidden {
                    message: String::from("missing permission"),
                },
                StatusCode::FORBIDDEN,
            ),
            (
                TrustedServerError::AllowlistViolation {
                    host: String::from("example.com"),
                },
                StatusCode::FORBIDDEN,
            ),
            (
                TrustedServerError::Settings {
                    message: String::from("parse failed"),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                TrustedServerError::EdgeCookie {
                    message: String::from("generation failed"),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                TrustedServerError::PartnerNotFound {
                    partner_id: String::from("example-partner"),
                },
                StatusCode::NOT_FOUND,
            ),
            (
                TrustedServerError::InsecureDefault {
                    field: String::from("example.secret"),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected_status) in cases {
            assert_eq!(
                error.status_code(),
                expected_status,
                "should map {error:?} to {expected_status}",
            );
        }
    }

    #[test]
    fn server_errors_return_generic_message() {
        let cases = [
            TrustedServerError::Configuration {
                message: "secret db path".into(),
            },
            TrustedServerError::KvStore {
                store_name: "users".into(),
                message: "timeout".into(),
            },
            TrustedServerError::Proxy {
                message: "upstream 10.0.0.1 refused".into(),
            },
            TrustedServerError::EdgeCookie {
                message: "seed file missing".into(),
            },
            TrustedServerError::Auction {
                message: "bid timeout".into(),
            },
            TrustedServerError::Gam {
                message: "api key invalid".into(),
            },
            TrustedServerError::Prebid {
                message: "adapter error".into(),
            },
            TrustedServerError::Integration {
                integration: "foo".into(),
                message: "connection refused".into(),
            },
            TrustedServerError::Settings {
                message: "parse failed".into(),
            },
            TrustedServerError::InvalidUtf8 {
                message: "byte 0xff".into(),
            },
        ];
        for error in &cases {
            assert_eq!(
                error.user_message(),
                "An internal error occurred",
                "should not leak details for {error:?}",
            );
        }
    }

    #[test]
    fn client_errors_return_safe_descriptions() {
        let error = TrustedServerError::BadRequest {
            message: "missing field".into(),
        };
        assert_eq!(error.user_message(), "Bad request: missing field");

        let error = TrustedServerError::GdprConsent {
            message: "no consent string".into(),
        };
        assert_eq!(error.user_message(), "GDPR consent error");

        let error = TrustedServerError::InvalidHeaderValue {
            message: "non-ascii".into(),
        };
        assert_eq!(error.user_message(), "Invalid header value");
    }
}
