//! Error conversion utilities for Fastly.
//!
//! This module provides conversions from [`TrustedServerError`] to HTTP responses.

use error_stack::Report;
use fastly::Response;
use trusted_server_common::error::{IntoHttpResponse, TrustedServerError};

/// Converts a [`TrustedServerError`] into an HTTP error response.
pub fn to_error_response(report: Report<TrustedServerError>) -> Response {
    // Get the root error for status code and message
    let root_error = report.current_context();

    // Log the full error chain for debugging
    log::error!("Error occurred: {:?}", report);

    Response::from_status(root_error.status_code())
        .with_body_text_plain(&format!("{}\n", root_error.user_message()))
}
