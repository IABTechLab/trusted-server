//! Permutive API proxy for transparent first-party API proxying.
//!
//! This module handles proxying Permutive API calls from the first-party domain
//! to Permutive's API servers, preserving query parameters, headers, and request bodies.

use error_stack::{Report, ResultExt};
use fastly::http::{header, Method};
use fastly::{Request, Response};

use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;
use crate::settings::Settings;

const PERMUTIVE_API_BASE: &str = "https://api.permutive.com";

/// Handles transparent proxying of Permutive API requests.
///
/// This function:
/// 1. Extracts the path after `/permutive/api/`
/// 2. Preserves query parameters
/// 3. Copies request headers and body
/// 4. Forwards to `api.permutive.com`
/// 5. Returns response transparently
///
/// # Example Request Flow
///
/// ```text
/// Browser: GET /permutive/api/v2/projects/abc?key=123
///     ↓
/// Trusted Server processes and forwards:
///     ↓
/// Permutive: GET https://api.permutive.com/v2/projects/abc?key=123
/// ```
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - Path extraction fails
/// - Backend communication fails
/// - Request forwarding fails
pub async fn handle_permutive_api_proxy(
    _settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let original_path = req.get_path();
    let method = req.get_method();

    log::info!(
        "Proxying Permutive API request: {} {}",
        method,
        original_path
    );

    // Extract the path after /permutive/api
    let api_path = original_path
        .strip_prefix("/permutive/api")
        .ok_or_else(|| TrustedServerError::PermutiveApi {
            message: format!("Invalid Permutive API path: {}", original_path),
        })?;

    // Build the full Permutive API URL with query parameters
    let permutive_url = build_permutive_url(api_path, &req)?;

    log::info!("Forwarding to Permutive API: {}", permutive_url);

    // Create new request to Permutive
    let mut permutive_req = Request::new(method.clone(), &permutive_url);

    // Copy relevant headers
    copy_request_headers(&req, &mut permutive_req);

    // Copy body for POST requests
    if has_body(method) {
        let body = req.take_body();
        permutive_req.set_body(body);
    }

    // Get backend and forward request
    let backend_name = ensure_backend_from_url(PERMUTIVE_API_BASE)?;

    let permutive_response = permutive_req
        .send(backend_name)
        .change_context(TrustedServerError::PermutiveApi {
            message: format!("Failed to forward request to {}", permutive_url),
        })?;

    log::info!(
        "Permutive API responded with status: {}",
        permutive_response.get_status()
    );

    // Return response transparently
    Ok(permutive_response)
}

/// Builds the full Permutive API URL including query parameters.
fn build_permutive_url(
    api_path: &str,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    // Get query string if present
    let query = req
        .get_url()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    // Build full URL
    let url = format!("{}{}{}", PERMUTIVE_API_BASE, api_path, query);

    Ok(url)
}

/// Copies relevant headers from the original request to the Permutive request.
fn copy_request_headers(from: &Request, to: &mut Request) {
    // Headers that should be forwarded to Permutive
    let headers_to_copy = [
        header::CONTENT_TYPE,
        header::ACCEPT,
        header::USER_AGENT,
        header::AUTHORIZATION,
        header::ACCEPT_LANGUAGE,
        header::ACCEPT_ENCODING,
    ];

    for header_name in &headers_to_copy {
        if let Some(value) = from.get_header(header_name) {
            to.set_header(header_name, value);
        }
    }

    // Copy any X-* custom headers
    for header_name in from.get_header_names() {
        let name_str = header_name.as_str();
        if name_str.starts_with("x-") || name_str.starts_with("X-") {
            if let Some(value) = from.get_header(header_name) {
                to.set_header(header_name, value);
            }
        }
    }
}

/// Checks if the HTTP method typically includes a request body.
fn has_body(method: &Method) -> bool {
    matches!(method, &Method::POST | &Method::PUT | &Method::PATCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_extraction() {
        let test_cases = vec![
            ("/permutive/api/v2/projects", "/v2/projects"),
            ("/permutive/api/v1/track", "/v1/track"),
            ("/permutive/api/", "/"),
            ("/permutive/api", ""),
        ];

        for (input, expected) in test_cases {
            let result = input.strip_prefix("/permutive/api").unwrap_or("");
            assert_eq!(result, expected, "Failed for input: {}", input);
        }
    }

    #[test]
    fn test_url_building_without_query() {
        let api_path = "/v2/projects/abc";
        let expected = "https://api.permutive.com/v2/projects/abc";

        let url = format!("{}{}", PERMUTIVE_API_BASE, api_path);
        assert_eq!(url, expected);
    }

    #[test]
    fn test_url_building_with_query() {
        let api_path = "/v2/projects/abc";
        let query = "?key=123&foo=bar";
        let expected = "https://api.permutive.com/v2/projects/abc?key=123&foo=bar";

        let url = format!("{}{}{}", PERMUTIVE_API_BASE, api_path, query);
        assert_eq!(url, expected);
    }

    #[test]
    fn test_has_body() {
        assert!(has_body(&Method::POST));
        assert!(has_body(&Method::PUT));
        assert!(has_body(&Method::PATCH));
        assert!(!has_body(&Method::GET));
        assert!(!has_body(&Method::DELETE));
        assert!(!has_body(&Method::HEAD));
        assert!(!has_body(&Method::OPTIONS));
    }
}
