use crate::error::TrustedServerError;
use crate::settings::Settings;
use fastly::http::{header, Method};
use fastly::{Request, Response};
use log;

/// Handles Didomi CMP reverse proxy requests
///
/// This module implements the reverse proxy functionality for Didomi CMP
/// according to their self-hosting documentation:
/// https://developers.didomi.io/api-and-platform/domains/self-hosting
pub struct DidomiProxy;

impl DidomiProxy {
    /// Handle requests to /consent/* paths
    ///
    /// Routes requests to either SDK or API origins based on path:
    /// - /consent/api/* → api.privacy-center.org
    /// - /consent/* → sdk.privacy-center.org
    pub async fn handle_consent_request(
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, error_stack::Report<TrustedServerError>> {
        let path = req.get_path();

        log::info!("Didomi proxy handling request: {}", path);
        // Force redeploy to fix intermittent issue

        log::info!("DEBUG: Starting path extraction");

        // Extract the consent path (remove /consent prefix)
        let consent_path = path.strip_prefix("/consent").unwrap_or(path);

        log::info!("DEBUG: consent_path = {}", consent_path);

        // Determine which origin to use
        let (backend_name, origin_path) = if consent_path.starts_with("/api/") {
            // API calls go to api.privacy-center.org with no caching
            ("didomi_api", consent_path)
        } else {
            // SDK files go to sdk.privacy-center.org with geo-based caching
            ("didomi_sdk", consent_path)
        };

        log::info!(
            "DEBUG: backend_name = {}, origin_path = {}",
            backend_name,
            origin_path
        );

        log::info!(
            "Routing to backend: {} with path: {}",
            backend_name,
            origin_path
        );

        log::info!("DEBUG: About to create proxy request");

        // Create the full URL for the request
        let backend_host = match backend_name {
            "didomi_sdk" => "sdk.privacy-center.org",
            "didomi_api" => "api.privacy-center.org",
            _ => {
                return Ok(
                    Response::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .with_header(header::CONTENT_TYPE, "text/plain")
                        .with_body("Unknown backend"),
                )
            }
        };

        let full_url = format!("https://{}{}", backend_host, origin_path);
        log::info!("Full URL constructed: {}", full_url);

        // Create the proxy request using Request::new like prebid module
        let mut proxy_req = Request::new(req.get_method().clone(), full_url);

        log::info!("Created proxy request with method: {:?}", req.get_method());

        // Copy query string
        if let Some(query) = req.get_query_str() {
            proxy_req.set_query_str(query);
        }

        // Set required headers according to Didomi documentation
        Self::set_proxy_headers(&mut proxy_req, &req, backend_name);

        // Send the request
        log::info!(
            "Sending request to backend: {} with path: {}",
            backend_name,
            origin_path
        );

        // Copy request body for POST/PUT requests
        if matches!(req.get_method(), &Method::POST | &Method::PUT) {
            proxy_req.set_body(req.into_body());
        }

        match proxy_req.send(backend_name) {
            Ok(mut response) => {
                log::info!(
                    "Received response from {}: {}",
                    backend_name,
                    response.get_status()
                );

                // Process the response according to Didomi requirements
                Self::process_response(&mut response, backend_name);

                Ok(response)
            }
            Err(e) => {
                log::error!("Error proxying request to {}: {:?}", backend_name, e);
                Err(error_stack::Report::new(TrustedServerError::FastlyError {
                    message: format!("Proxy error to {}: {}", backend_name, e),
                }))
            }
        }
    }

    /// Set proxy headers according to Didomi documentation
    fn set_proxy_headers(proxy_req: &mut Request, original_req: &Request, backend_name: &str) {
        // Host header is automatically set when using full URLs

        // Forward user IP in X-Forwarded-For header
        if let Some(client_ip) = original_req.get_client_ip_addr() {
            proxy_req.set_header("X-Forwarded-For", client_ip.to_string());
        }

        // Forward geographic information for SDK requests (for geo-based caching)
        if backend_name == "didomi_sdk" {
            // Copy geographic headers from Fastly
            let geo_headers = [
                ("X-Geo-Country", "FastlyGeo-CountryCode"),
                ("X-Geo-Region", "FastlyGeo-Region"),
                ("CloudFront-Viewer-Country", "FastlyGeo-CountryCode"),
            ];

            for (header_name, fastly_header) in geo_headers {
                if let Some(value) = original_req.get_header(fastly_header) {
                    proxy_req.set_header(header_name, value);
                }
            }
        }

        // Forward essential headers
        let headers_to_forward = [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::USER_AGENT,
            header::REFERER,
            header::ORIGIN,
            header::AUTHORIZATION,
        ];

        for header_name in headers_to_forward {
            if let Some(value) = original_req.get_header(&header_name) {
                proxy_req.set_header(&header_name, value);
            }
        }

        // DO NOT forward cookies (as per Didomi documentation)
        // proxy_req.remove_header(header::COOKIE);

        // Set content type for POST/PUT requests
        if matches!(original_req.get_method(), &Method::POST | &Method::PUT) {
            if let Some(content_type) = original_req.get_header(header::CONTENT_TYPE) {
                proxy_req.set_header(header::CONTENT_TYPE, content_type);
            }
        }

        log::info!("Proxy headers set for {}", backend_name);
    }

    /// Process response according to Didomi requirements
    fn process_response(response: &mut Response, backend_name: &str) {
        // Add CORS headers for SDK requests
        if backend_name == "didomi_sdk" {
            response.set_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
            response.set_header(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                "Content-Type, Authorization, X-Requested-With",
            );
            response.set_header(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                "GET, POST, PUT, DELETE, OPTIONS",
            );
        }

        // Log cache headers for debugging
        if let Some(cache_control) = response.get_header(header::CACHE_CONTROL) {
            log::info!("Cache-Control from {}: {:?}", backend_name, cache_control);
        }

        // Ensure cache headers are preserved (they will be returned to the client)
        // This is important for Didomi's caching requirements

        log::info!("Response processed for {}", backend_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consent_path_extraction() {
        let path = "/consent/api/events";
        let consent_path = path.strip_prefix("/consent").unwrap_or(path);
        assert_eq!(consent_path, "/api/events");

        let path = "/consent/24cd3901-9da4-4643-96a3-9b1c573b5264/loader.js";
        let consent_path = path.strip_prefix("/consent").unwrap_or(path);
        assert_eq!(
            consent_path,
            "/24cd3901-9da4-4643-96a3-9b1c573b5264/loader.js"
        );
    }

    #[test]
    fn test_backend_selection() {
        // API requests
        let api_path = "/api/events";
        assert!(api_path.starts_with("/api/"));

        // SDK requests
        let sdk_path = "/24cd3901-9da4-4643-96a3-9b1c573b5264/loader.js";
        assert!(!sdk_path.starts_with("/api/"));

        let sdk_path2 = "/sdk/version/core.js";
        assert!(!sdk_path2.starts_with("/api/"));
    }
}
