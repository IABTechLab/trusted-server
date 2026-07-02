//! Error conversion utilities for Fastly.
//!
//! This module provides conversions from [`TrustedServerError`] to HTTP responses.

use error_stack::Report;
use fastly::Response;
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};

/// Converts a [`TrustedServerError`] into an HTTP error response.
pub fn to_error_response(report: &Report<TrustedServerError>) -> Response {
    // Get the root error for status code and message
    let root_error = report.current_context();

    // Log the full error chain for debugging
    log::error!("Error occurred: {:?}", report);

    Response::from_status(root_error.status_code())
        .with_body_text_plain(&format!("{}\n", root_error.user_message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_store_unavailable_renders_503() {
        // Locks the end-to-end mapping: a config-store read failure reaches the
        // client as 503 via `status_code()` — not bypassed by the adapter.
        let report = Report::new(TrustedServerError::ConfigStoreUnavailable {
            store_name: "app_config".to_string(),
            message: "unavailable or not seeded".to_string(),
        });

        let response = to_error_response(&report);

        assert_eq!(
            response.get_status(),
            fastly::http::StatusCode::SERVICE_UNAVAILABLE,
            "config-store read failure should render as 503 to the client"
        );
    }
}
