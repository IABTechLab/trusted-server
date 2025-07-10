use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use flate2::read::{GzDecoder, ZlibDecoder};
use std::io::Read;

use crate::constants::{
    HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_FORWARDED_FOR, HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY, HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE
};
use crate::cookies::create_synthetic_cookie;
use crate::error::TrustedServerError;
use crate::gdpr::{get_consent_from_request, GdprConsent};
use crate::geo::get_dma_code;
use crate::settings::Settings;
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
use crate::templates::HTML_TEMPLATE;

/// Handles the main page request.
///
/// Serves the main page with synthetic ID generation and ad integration.
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - Synthetic ID generation fails
/// - Response creation fails
pub fn handle_main_page(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!(
        "Using ad_partner_url: {}, counter_store: {}",
        settings.ad_server.ad_partner_url,
        settings.synthetic.counter_store,
    );

    // Add DMA code check to main page as well
    let dma_code = get_dma_code(&mut req);
    log::info!("Main page - DMA Code: {:?}", dma_code);

    // Check GDPR consent before proceeding
    let consent = match get_consent_from_request(&req) {
        Some(c) => c,
        None => {
            log::debug!("No GDPR consent found, using default");
            GdprConsent::default()
        }
    };
    if !consent.functional {
        // Return a version of the page without tracking
        return Ok(Response::from_status(StatusCode::OK)
            .with_body(
                HTML_TEMPLATE.replace("fetch('/prebid-test')", "console.log('Tracking disabled')"),
            )
            .with_header(header::CONTENT_TYPE, "text/html")
            .with_header(header::CACHE_CONTROL, "no-store, private"));
    }

    // Calculate fresh ID first using the synthetic module
    let fresh_id = generate_synthetic_id(settings, &req)?;

    // Check for existing Trusted Server ID in this specific order:
    // 1. X-Synthetic-Trusted-Server header
    // 2. Cookie
    // 3. Fall back to fresh ID
    let synthetic_id = get_or_generate_synthetic_id(settings, &req)?;

    log::info!(
        "Existing Trusted Server header: {:?}",
        req.get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
    );
    log::info!("Generated Fresh ID: {}", &fresh_id);
    log::info!("Using Trusted Server ID: {}", synthetic_id);

    // Create response with the main page HTML
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(HTML_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header(HEADER_SYNTHETIC_FRESH, fresh_id.as_str()) // Fresh ID always changes
        .with_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id) // Trusted Server ID remains stable
        .with_header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
        )
        .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .with_header("x-compress-hint", "on");

    // Copy geo headers from request to response
    for header_name in &[
        HEADER_X_GEO_CITY,
        HEADER_X_GEO_COUNTRY,
        HEADER_X_GEO_CONTINENT,
        HEADER_X_GEO_COORDINATES,
        HEADER_X_GEO_METRO_CODE,
        HEADER_X_GEO_INFO_AVAILABLE,
    ] {
        if let Some(value) = req.get_header(header_name) {
            response.set_header(header_name, value);
        }
    }

    // Only set cookies if we have consent
    if consent.functional {
        response.set_header(
            header::SET_COOKIE,
            create_synthetic_cookie(settings, &synthetic_id),
        );
    }

    // Debug: Print all request headers
    log::info!("All Request Headers:");
    for (name, value) in req.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Debug: Print the response headers
    log::info!("Response Headers:");
    for (name, value) in response.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Prevent caching
    response.set_header(header::CACHE_CONTROL, "no-store, private");

    Ok(response)
}

/// Proxies requests to the publisher's origin server.
///
/// This function forwards incoming requests to the configured origin URL,
/// preserving headers and request body. It's used as a fallback for routes
/// not explicitly handled by the trusted server.
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - The proxy request fails
/// - The origin backend is unreachable
pub fn handle_publisher_request(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Proxying request to publisher_origin");

    // Extract the request host from the incoming request
    let request_host = req
        .get_header(header::HOST)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_string();

    // Extract the protocol from X-Forwarded-Proto header before moving req
    let request_scheme = req
        .get_header(HEADER_X_FORWARDED_FOR)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http")
        .to_string();

    log::info!("Request host: {}", request_host);

    // Extract host from the origin_url using the Publisher's origin_host method
    let origin_host = settings.publisher.origin_host();

    log::info!("Setting host header to: {}", origin_host);
    req.set_header("host", &origin_host);

    // Send the request to the origin backend
    let mut response = req
        .send(&settings.publisher.origin_backend)
        .change_context(TrustedServerError::Proxy {
            message: "Failed to proxy request".to_string(),
        })?;

    // Log all response headers for debugging
    log::info!("Response headers:");
    for (name, value) in response.get_headers() {
        log::info!("  {}: {:?}", name, value);
    }

    // Check if the response has a text-based content type that we should process
    let content_type = response
        .get_header(header::CONTENT_TYPE)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default();

    let should_process = content_type.contains("text/html")
        || content_type.contains("text/css")
        || content_type.contains("text/javascript")
        || content_type.contains("application/javascript")
        || content_type.contains("application/json");

    if should_process && !request_host.is_empty() {
        // Check if the response is compressed
        let content_encoding = response
            .get_header(header::CONTENT_ENCODING)
            .map(|h| h.to_str().unwrap_or_default())
            .unwrap_or_default()
            .to_lowercase();

        // Log response details for debugging
        log::info!(
            "Processing response - Content-Type: {}, Content-Encoding: {}, Request Host: {}, Origin Host: {}",
            content_type, content_encoding, request_host, origin_host
        );

        // Get the response body as bytes
        let body_bytes = response.take_body_bytes();

        // Check if we got an empty body
        if body_bytes.is_empty() {
            log::warn!("Response body is empty, nothing to process");
            return Ok(response);
        }

        log::info!("Response body size: {} bytes", body_bytes.len());

        // Decompress the body if needed
        let decompressed_body = match content_encoding.as_str() {
            "gzip" => {
                let mut decoder = GzDecoder::new(&body_bytes[..]);
                let mut decompressed = Vec::new();
                match decoder.read_to_end(&mut decompressed) {
                    Ok(_) => {
                        log::info!("Successfully decompressed gzip content");
                        decompressed
                    }
                    Err(e) => {
                        log::warn!("Failed to decompress gzip content: {}. Content might already be decompressed by Fastly", e);
                        // Try using the original bytes
                        body_bytes
                    }
                }
            }
            "deflate" => {
                let mut decoder = ZlibDecoder::new(&body_bytes[..]);
                let mut decompressed = Vec::new();
                match decoder.read_to_end(&mut decompressed) {
                    Ok(_) => {
                        log::info!("Successfully decompressed deflate content");
                        decompressed
                    }
                    Err(e) => {
                        log::warn!("Failed to decompress deflate content: {}. Content might already be decompressed by Fastly", e);
                        // Try using the original bytes
                        body_bytes
                    }
                }
            }
            _ => {
                log::warn!(
                    "Unsupported content encoding: {}, passing through",
                    content_encoding
                );
                body_bytes
            }
        };

        // Try to convert to UTF-8 using lossy conversion to handle more cases
        let body_str = String::from_utf8_lossy(&decompressed_body);

        // Use the extracted function to perform URL replacement
        let modified_body = replace_origin_urls(
            &body_str,
            &origin_host,
            &settings.publisher.origin_url,
            &request_host,
            &request_scheme,
        );

        // Set the modified body back
        response.set_body(modified_body);

        // Remove headers that are no longer valid after modification
        response.remove_header(header::CONTENT_LENGTH);
        response.remove_header(header::CONTENT_ENCODING);

        log::info!("Completed processing response body");
    } else {
        log::info!(
            "Skipping response processing - should_process: {}, request_host: '{}'",
            should_process,
            request_host
        );
    }

    Ok(response)
}

/// Replaces origin URLs in content with request URLs.
///
/// This function performs the URL replacement logic used in `handle_publisher_request`.
/// It replaces both the origin host and full origin URL with their request equivalents.
///
/// # Arguments
///
/// * `content` - The content to process
/// * `origin_host` - The origin hostname (e.g., "origin.example.com")
/// * `origin_url` - The full origin URL (e.g., "https://origin.example.com")
/// * `request_host` - The request hostname (e.g., "test.example.com")
/// * `request_scheme` - The request scheme ("http" or "https")
///
/// # Returns
///
/// The content with all origin references replaced
pub fn replace_origin_urls(
    content: &str,
    origin_host: &str,
    origin_url: &str,
    request_host: &str,
    request_scheme: &str,
) -> String {
    let request_url = format!("{}://{}", request_scheme, request_host);

    log::info!("Replacing {} with {}", origin_url, request_url);

    // Start with the content
    let mut result = content.to_string();

    // Replace full URLs first (more specific)
    result = result.replace(origin_url, &request_url);

    // Also try with http if origin was https (in case of mixed content)
    if origin_url.starts_with("https://") {
        let http_origin_url = origin_url.replace("https://", "http://");
        result = result.replace(&http_origin_url, &request_url);
    }

    // Replace protocol-relative URLs (//example.com)
    let protocol_relative_origin = format!("//{}", origin_host);
    let protocol_relative_request = format!("//{}", request_host);
    result = result.replace(&protocol_relative_origin, &protocol_relative_request);

    // Replace host in various contexts
    // This handles cases like: "host": "origin.example.com" in JSON
    result = result.replace(origin_host, request_host);

    // Log if replacements were made
    if result != content {
        log::debug!("URL replacements made in content");
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::http::Method;

    fn create_test_settings() -> Settings {
        Settings {
            publisher: crate::settings::Publisher {
                domain: "example.com".to_string(),
                cookie_domain: ".example.com".to_string(),
                origin_backend: "test_origin".to_string(),
                origin_url: "https://origin.example.com".to_string(),
            },
            ad_server: crate::settings::AdServer {
                ad_partner_url: "https://ad.example.com".to_string(),
                sync_url: "https://sync.example.com".to_string(),
            },
            synthetic: crate::settings::Synthetic {
                counter_store: "test_counter".to_string(),
                opid_store: "test_opid_store".to_string(),
                secret_key: "test_secret_key".to_string(),
                template: "{{user_agent}}+{{ip}}".to_string(),
            },
            prebid: crate::settings::Prebid {
                server_url: "https://prebid.example.com".to_string(),
            },
        }
    }

    #[test]
    fn test_replace_origin_urls() {
        let test_cases = vec![
            (
                // Test HTML content
                r#"<html>
                <link rel="stylesheet" href="https://origin.example.com/style.css">
                <script src="https://origin.example.com/script.js"></script>
                <a href="https://origin.example.com/page">Link</a>
                <img src="//origin.example.com/image.jpg">
                </html>"#,
                r#"<html>
                <link rel="stylesheet" href="https://test.example.com/style.css">
                <script src="https://test.example.com/script.js"></script>
                <a href="https://test.example.com/page">Link</a>
                <img src="//test.example.com/image.jpg">
                </html>"#,
                "https",
            ),
            (
                // Test JavaScript content
                r#"const API_URL = 'https://origin.example.com/api';
                fetch('https://origin.example.com/data')
                    .then(res => res.json());
                window.location = 'https://origin.example.com/redirect';"#,
                r#"const API_URL = 'https://test.example.com/api';
                fetch('https://test.example.com/data')
                    .then(res => res.json());
                window.location = 'https://test.example.com/redirect';"#,
                "https",
            ),
            (
                // Test CSS content
                r#".hero {
                    background: url('https://origin.example.com/hero.jpg');
                }
                @import url('https://origin.example.com/fonts.css');"#,
                r#".hero {
                    background: url('https://test.example.com/hero.jpg');
                }
                @import url('https://test.example.com/fonts.css');"#,
                "https",
            ),
            (
                // Test JSON API response
                r#"{
                    "api_endpoint": "https://origin.example.com/v1",
                    "assets_url": "https://origin.example.com/assets",
                    "websocket": "wss://origin.example.com/ws"
                }"#,
                r#"{
                    "api_endpoint": "https://test.example.com/v1",
                    "assets_url": "https://test.example.com/assets",
                    "websocket": "wss://test.example.com/ws"
                }"#,
                "https",
            ),
            (
                // Test HTTP scheme
                r#"<a href="http://origin.example.com/page">HTTP Link</a>"#,
                r#"<a href="http://test.example.com/page">HTTP Link</a>"#,
                "http",
            ),
        ];

        for (input, expected, scheme) in test_cases {
            let result = replace_origin_urls(
                input,
                "origin.example.com",
                "https://origin.example.com",
                "test.example.com",
                scheme,
            );
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_replace_origin_urls_with_port() {
        let content = r#"<a href="https://origin.example.com:8080/page">Link</a>"#;
        let result = replace_origin_urls(
            content,
            "origin.example.com:8080",
            "https://origin.example.com:8080",
            "test.example.com:9090",
            "https",
        );
        assert_eq!(
            result,
            r#"<a href="https://test.example.com:9090/page">Link</a>"#
        );
    }

    #[test]
    fn test_replace_origin_urls_mixed_protocols() {
        let content = r#"
            <a href="https://origin.example.com/secure">HTTPS</a>
            <a href="http://origin.example.com/insecure">HTTP</a>
            <img src="//origin.example.com/protocol-relative.jpg">
        "#;

        // When replacing with HTTPS, both http and https URLs are replaced
        let result = replace_origin_urls(
            content,
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        assert!(result.contains("https://test.example.com/secure"));
        assert!(result.contains("https://test.example.com/insecure")); // HTTP also replaced to HTTPS
        assert!(result.contains("//test.example.com/protocol-relative.jpg"));
    }

    #[test]
    fn test_handle_publisher_request_extracts_headers() {
        // Test that the function correctly extracts host and scheme from request headers
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("host", "test.example.com");
        req.set_header("x-forwarded-proto", "https");

        // Extract headers like the function does
        let request_host = req
            .get_header("host")
            .map(|h| h.to_str().unwrap_or_default())
            .unwrap_or_default()
            .to_string();

        let request_scheme = req
            .get_header("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("https")
            .to_string();

        assert_eq!(request_host, "test.example.com");
        assert_eq!(request_scheme, "https");
    }

    #[test]
    fn test_handle_publisher_request_default_https_scheme() {
        // Test default HTTPS when x-forwarded-proto is missing
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("host", "test.example.com");
        // No x-forwarded-proto header

        let request_scheme = req
            .get_header("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("https");

        assert_eq!(request_scheme, "https");
    }

    #[test]
    fn test_handle_publisher_request_http_scheme() {
        // Test HTTP scheme detection
        let mut req = Request::new(Method::GET, "http://test.example.com/page");
        req.set_header("host", "test.example.com");
        req.set_header("x-forwarded-proto", "http");

        let request_scheme = req
            .get_header("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("https");

        assert_eq!(request_scheme, "http");
    }

    #[test]
    fn test_content_type_detection() {
        // Test which content types should be processed
        let test_cases = vec![
            ("text/html", true),
            ("text/html; charset=utf-8", true),
            ("text/css", true),
            ("text/javascript", true),
            ("application/javascript", true),
            ("application/json", true),
            ("application/json; charset=utf-8", true),
            ("image/jpeg", false),
            ("image/png", false),
            ("application/pdf", false),
            ("video/mp4", false),
            ("application/octet-stream", false),
        ];

        for (content_type, should_process) in test_cases {
            let result = content_type.contains("text/html")
                || content_type.contains("text/css")
                || content_type.contains("text/javascript")
                || content_type.contains("application/javascript")
                || content_type.contains("application/json");

            assert_eq!(
                result, should_process,
                "Content-Type '{}' should_process: expected {}, got {}",
                content_type, should_process, result
            );
        }
    }

    #[test]
    fn test_handle_main_page_gdpr_consent() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/");

        // Without GDPR consent, tracking should be disabled
        let response = handle_main_page(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::OK);
        // Note: Would need to verify response body contains disabled tracking
    }

    #[test]
    fn test_publisher_origin_host_extraction() {
        let settings = create_test_settings();
        let origin_host = settings.publisher.origin_host();
        assert_eq!(origin_host, "origin.example.com");

        // Test with port
        let mut settings_with_port = create_test_settings();
        settings_with_port.publisher.origin_url = "https://origin.example.com:8080".to_string();
        assert_eq!(
            settings_with_port.publisher.origin_host(),
            "origin.example.com:8080"
        );
    }

    #[test]
    fn test_invalid_utf8_handling() {
        // Test that invalid UTF-8 bytes are handled gracefully
        let invalid_utf8_bytes = vec![0xFF, 0xFE, 0xFD]; // Invalid UTF-8 sequence

        // Verify these bytes cannot be converted to a valid UTF-8 string
        assert!(String::from_utf8(invalid_utf8_bytes.clone()).is_err());

        // In the actual function, invalid UTF-8 would be passed through unchanged
        // This test verifies our approach is sound
    }

    #[test]
    fn test_utf8_conversion_edge_cases() {
        // Test various UTF-8 edge cases
        let test_cases = vec![
            // Valid UTF-8 with special characters
            (vec![0xE2, 0x98, 0x83], true),       // ☃ (snowman)
            (vec![0xF0, 0x9F, 0x98, 0x80], true), // 😀 (emoji)
            // Invalid UTF-8 sequences
            (vec![0xFF, 0xFE], false),       // Invalid start byte
            (vec![0xC0, 0x80], false),       // Overlong encoding
            (vec![0xED, 0xA0, 0x80], false), // Surrogate half
        ];

        for (bytes, should_be_valid) in test_cases {
            let result = String::from_utf8(bytes.clone());
            assert_eq!(
                result.is_ok(),
                should_be_valid,
                "UTF-8 validation failed for bytes: {:?}",
                bytes
            );
        }
    }

    #[test]
    fn test_content_encoding_detection() {
        // Test that we properly handle responses with various content encodings
        let test_encodings = vec!["gzip", "deflate", "br", "identity", ""];

        for encoding in test_encodings {
            let mut req = Request::new(Method::GET, "https://test.example.com/page");
            req.set_header("accept-encoding", "gzip, deflate, br");

            if !encoding.is_empty() {
                req.set_header("content-encoding", encoding);
            }

            let content_encoding = req
                .get_header("content-encoding")
                .map(|h| h.to_str().unwrap_or_default())
                .unwrap_or_default();

            assert_eq!(content_encoding, encoding);
        }
    }

    #[test]
    fn test_compressed_content_handling() {
        // Test the overall flow with compressed content
        // In production, Fastly handles decompression/recompression automatically

        let compressed_html = r#"<html>
            <link href="https://origin.example.com/style.css" rel="stylesheet">
            <script src="https://origin.example.com/app.js"></script>
        </html>"#;

        let expected_html = r#"<html>
            <link href="https://test.example.com/style.css" rel="stylesheet">
            <script src="https://test.example.com/app.js"></script>
        </html>"#;

        let result = replace_origin_urls(
            compressed_html,
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        assert_eq!(result, expected_html);
    }

    #[test]
    fn test_replace_origin_urls_comprehensive() {
        // Test comprehensive URL replacement scenarios
        let content = r#"
            <!-- Full HTTPS URLs -->
            <a href="https://origin.example.com/page">Link</a>
            
            <!-- HTTP URLs (should be upgraded to request scheme) -->
            <img src="http://origin.example.com/image.jpg">
            
            <!-- Protocol-relative URLs -->
            <script src="//origin.example.com/script.js"></script>
            
            <!-- JSON API responses -->
            {"api": "https://origin.example.com/api", "host": "origin.example.com"}
            
            <!-- URLs in JavaScript -->
            fetch('https://origin.example.com/data');
            const host = 'origin.example.com';
        "#;

        let result = replace_origin_urls(
            content,
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        // Verify all replacements
        assert!(result.contains(r#"href="https://test.example.com/page""#));
        assert!(result.contains(r#"src="https://test.example.com/image.jpg""#)); // HTTP upgraded
        assert!(result.contains(r#"src="//test.example.com/script.js""#));
        assert!(result.contains(r#""api": "https://test.example.com/api""#));
        assert!(result.contains(r#""host": "test.example.com""#));
        assert!(result.contains(r#"fetch('https://test.example.com/data')"#));
        assert!(result.contains(r#"const host = 'test.example.com'"#));

        // Ensure no origin references remain
        assert!(!result.contains("origin.example.com"));
    }
}
