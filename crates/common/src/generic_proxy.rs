//! Generic proxy handler for configuration-driven transparent proxying.
//!
//! This module provides a flexible proxy system that routes requests based on
//! configuration rather than hardcoded routes, making it easy to add new
//! integration partners without code changes.

use error_stack::{Report, ResultExt};
use fastly::http::{header, Method};
use fastly::{Request, Response};

use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;
use crate::settings::{ProxyMapping, Settings};

/// Handles generic transparent proxying based on configuration mappings.
///
/// This function:
/// 1. Finds a matching proxy mapping from settings
/// 2. Validates the HTTP method is allowed
/// 3. Extracts the path after the prefix
/// 4. Builds the target URL with query parameters
/// 5. Copies headers and body
/// 6. Forwards the request to the target
/// 7. Returns the response transparently
///
/// # Example Flow
///
/// ```text
/// Config:
///   prefix: "/permutive/api"
///   target: "https://api.permutive.com"
///
/// Request: GET /permutive/api/v2/projects?key=123
///     ↓
/// Extract path: /v2/projects
///     ↓
/// Build URL: https://api.permutive.com/v2/projects?key=123
///     ↓
/// Forward and return response
/// ```
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - No matching proxy mapping found
/// - HTTP method not allowed for this mapping
/// - Target URL construction fails
/// - Backend communication fails
pub async fn handle_generic_proxy(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let path = req.get_path();
    let method = req.get_method();

    log::info!("Generic proxy request: {} {}", method, path);

    // Find matching proxy mapping
    let mapping = find_proxy_mapping(settings, path, method)?;

    log::info!(
        "Matched proxy mapping: {} → {} ({})",
        mapping.prefix,
        mapping.target,
        mapping.description
    );

    // Extract target path
    let target_path = mapping
        .extract_target_path(path)
        .ok_or_else(|| TrustedServerError::Proxy {
            message: format!(
                "Failed to extract target path from {} with prefix {}",
                path, mapping.prefix
            ),
        })?;

    // Build full target URL with query parameters
    let target_url = build_target_url(&mapping.target, target_path, &req)?;

    log::info!("Forwarding to: {}", target_url);

    // Create new request to target
    let mut target_req = Request::new(method.clone(), &target_url);

    // Copy headers
    copy_request_headers(&req, &mut target_req);

    // Copy body for methods that support it
    if has_body(method) {
        let body = req.take_body();
        target_req.set_body(body);
    }

    // Get backend and forward request
    let backend_name = ensure_backend_from_url(&mapping.target)?;

    let target_response = target_req
        .send(backend_name)
        .change_context(TrustedServerError::Proxy {
            message: format!("Failed to forward request to {}", target_url),
        })?;

    log::info!(
        "Target responded with status: {}",
        target_response.get_status()
    );

    // Return response transparently
    Ok(target_response)
}

/// Finds a proxy mapping that matches the given path and method.
fn find_proxy_mapping<'a>(
    settings: &'a Settings,
    path: &str,
    method: &Method,
) -> Result<&'a ProxyMapping, Report<TrustedServerError>> {
    settings
        .proxy_mappings
        .iter()
        .find(|mapping| {
            mapping.matches_path(path) && mapping.supports_method(method.as_str())
        })
        .ok_or_else(|| {
            TrustedServerError::Proxy {
                message: format!(
                    "No proxy mapping found for {} {}. Available prefixes: [{}]",
                    method,
                    path,
                    settings
                        .proxy_mappings
                        .iter()
                        .map(|m| m.prefix.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
            .into()
        })
}

/// Builds the full target URL including path and query parameters.
fn build_target_url(
    base_url: &str,
    target_path: &str,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    // Get query string if present
    let query = req
        .get_url()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    // Build full URL
    let url = format!("{}{}{}", base_url, target_path, query);

    Ok(url)
}

/// Copies relevant headers from the original request to the target request.
fn copy_request_headers(from: &Request, to: &mut Request) {
    // Standard headers to forward
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

/// Helper function to check if any proxy mapping matches the given path.
pub fn has_proxy_mapping(settings: &Settings, path: &str) -> bool {
    settings
        .proxy_mappings
        .iter()
        .any(|mapping| mapping.matches_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ProxyMapping;

    #[test]
    fn test_proxy_mapping_matches_path() {
        let mapping = ProxyMapping {
            prefix: "/permutive/api".to_string(),
            target: "https://api.permutive.com".to_string(),
            methods: vec!["GET".to_string(), "POST".to_string()],
            description: "Test".to_string(),
        };

        assert!(mapping.matches_path("/permutive/api/v2/projects"));
        assert!(mapping.matches_path("/permutive/api"));
        assert!(!mapping.matches_path("/permutive/other"));
        assert!(!mapping.matches_path("/other/api"));
    }

    #[test]
    fn test_proxy_mapping_supports_method() {
        let mapping = ProxyMapping {
            prefix: "/test".to_string(),
            target: "https://example.com".to_string(),
            methods: vec!["GET".to_string(), "POST".to_string()],
            description: "Test".to_string(),
        };

        assert!(mapping.supports_method("GET"));
        assert!(mapping.supports_method("POST"));
        assert!(mapping.supports_method("get")); // case insensitive
        assert!(mapping.supports_method("post"));
        assert!(!mapping.supports_method("DELETE"));
        assert!(!mapping.supports_method("PUT"));
    }

    #[test]
    fn test_proxy_mapping_extract_target_path() {
        let mapping = ProxyMapping {
            prefix: "/permutive/api".to_string(),
            target: "https://api.permutive.com".to_string(),
            methods: vec!["GET".to_string()],
            description: "Test".to_string(),
        };

        assert_eq!(
            mapping.extract_target_path("/permutive/api/v2/projects"),
            Some("/v2/projects")
        );
        assert_eq!(mapping.extract_target_path("/permutive/api"), Some(""));
        assert_eq!(
            mapping.extract_target_path("/permutive/api/"),
            Some("/")
        );
        assert_eq!(mapping.extract_target_path("/other/path"), None);
    }

    #[test]
    fn test_build_target_url_without_query() {
        let base_url = "https://api.permutive.com";
        let target_path = "/v2/projects";
        let expected = "https://api.permutive.com/v2/projects";

        let url = format!("{}{}", base_url, target_path);
        assert_eq!(url, expected);
    }

    #[test]
    fn test_build_target_url_with_query() {
        let base_url = "https://api.permutive.com";
        let target_path = "/v2/projects";
        let query = "?key=123&foo=bar";
        let expected = "https://api.permutive.com/v2/projects?key=123&foo=bar";

        let url = format!("{}{}{}", base_url, target_path, query);
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
    }
}
