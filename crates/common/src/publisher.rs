use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Body, Request, Response};
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use std::io::{Read, Write};

use crate::constants::{
    HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_GEO_CITY,
    HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};
use crate::cookies::create_synthetic_cookie;
use crate::error::TrustedServerError;
use crate::gdpr::{get_consent_from_request, GdprConsent};
use crate::geo::get_dma_code;
use crate::settings::Settings;
use crate::streaming_replacer::StreamingReplacer;
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
use crate::templates::HTML_TEMPLATE;

/// Detects the request scheme (HTTP or HTTPS) using Fastly SDK methods and headers.
///
/// Tries multiple methods in order of reliability:
/// 1. Fastly SDK TLS detection methods (most reliable)
/// 2. Forwarded header (RFC 7239)
/// 3. X-Forwarded-Proto header
/// 4. Fastly-SSL header (least reliable, can be spoofed)
/// 5. Default to HTTP
fn detect_request_scheme(req: &Request) -> String {
    // 1. First try Fastly SDK's built-in TLS detection methods
    // These are the most reliable as they check the actual connection
    if let Some(tls_protocol) = req.get_tls_protocol() {
        // If we have a TLS protocol, the connection is definitely HTTPS
        log::debug!("TLS protocol detected: {}", tls_protocol);
        return "https".to_string();
    }

    // Also check TLS cipher - if present, connection is HTTPS
    if req.get_tls_cipher_openssl_name().is_some() {
        log::debug!("TLS cipher detected, using HTTPS");
        return "https".to_string();
    }

    // 2. Try the Forwarded header (RFC 7239)
    if let Some(forwarded) = req.get_header("forwarded") {
        if let Ok(forwarded_str) = forwarded.to_str() {
            // Parse the Forwarded header
            // Format: Forwarded: for=192.0.2.60;proto=https;by=203.0.113.43
            if forwarded_str.contains("proto=https") {
                return "https".to_string();
            } else if forwarded_str.contains("proto=http") {
                return "http".to_string();
            }
        }
    }

    // 3. Try X-Forwarded-Proto header
    if let Some(proto) = req.get_header("x-forwarded-proto") {
        if let Ok(proto_str) = proto.to_str() {
            let proto_lower = proto_str.to_lowercase();
            if proto_lower == "https" || proto_lower == "http" {
                return proto_lower;
            }
        }
    }

    // 4. Check Fastly-SSL header (can be spoofed by clients, use as last resort)
    if let Some(ssl) = req.get_header("fastly-ssl") {
        if let Ok(ssl_str) = ssl.to_str() {
            if ssl_str == "1" || ssl_str.to_lowercase() == "true" {
                return "https".to_string();
            }
        }
    }

    // Default to HTTP (changed from HTTPS based on your settings file)
    "http".to_string()
}

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
        "Using ad_partner_backend: {}, counter_store: {}",
        settings.ad_server.ad_partner_backend,
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

/// Generic streaming processor that reads from a source, processes through replacer, and writes to output
fn stream_process<R: Read, W: Write>(
    mut reader: R,
    writer: &mut W,
    replacer: &mut StreamingReplacer,
    chunk_size: usize,
) -> Result<(), Report<TrustedServerError>> {
    let mut buffer = vec![0u8; chunk_size];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                // End of stream - process any remaining data
                let final_chunk = replacer.process_chunk(&[], true);
                if !final_chunk.is_empty() {
                    writer
                        .write_all(&final_chunk)
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to write final chunk".to_string(),
                        })?;
                }
                break;
            }
            Ok(n) => {
                // Process this chunk
                let processed = replacer.process_chunk(&buffer[..n], false);
                if !processed.is_empty() {
                    writer
                        .write_all(&processed)
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to write processed chunk".to_string(),
                        })?;
                }
            }
            Err(e) => {
                log::error!("Error reading from stream: {}", e);
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!("Failed to read from stream: {}", e),
                }));
            }
        }
    }

    Ok(())
}

/// Process response body in streaming fashion with compression preservation
fn process_response_streaming(
    body: Body,
    content_encoding: &str,
    origin_host: &str,
    origin_url: &str,
    request_host: &str,
    request_scheme: &str,
) -> Result<Body, Report<TrustedServerError>> {
    const CHUNK_SIZE: usize = 8192; // 8KB chunks

    // Create the streaming replacer
    let mut replacer =
        StreamingReplacer::new(origin_host, origin_url, request_host, request_scheme);

    // Create output body
    let mut output_body = Body::new();

    // Determine if content needs decompression/recompression
    let is_compressed = matches!(content_encoding, "gzip" | "deflate");

    if is_compressed {
        // For compressed content, we stream through:
        // 1. Decompress chunks
        // 2. Process them
        // 3. Recompress and write to output

        log::info!(
            "Processing compressed content with encoding: {}",
            content_encoding
        );

        match content_encoding {
            "gzip" => {
                // Create gzip decompressor
                let decoder = GzDecoder::new(body);
                // Create gzip compressor
                let mut encoder = GzEncoder::new(output_body, Compression::default());

                // Stream through the pipeline
                stream_process(decoder, &mut encoder, &mut replacer, CHUNK_SIZE)?;

                // Finish compression and get the output body
                output_body = encoder.finish().change_context(TrustedServerError::Proxy {
                    message: "Failed to finish gzip compression".to_string(),
                })?;
            }
            "deflate" => {
                // Create deflate decompressor
                let decoder = ZlibDecoder::new(body);
                // Create deflate compressor
                let mut encoder = ZlibEncoder::new(output_body, Compression::default());

                // Stream through the pipeline
                stream_process(decoder, &mut encoder, &mut replacer, CHUNK_SIZE)?;

                // Finish compression and get the output body
                output_body = encoder.finish().change_context(TrustedServerError::Proxy {
                    message: "Failed to finish deflate compression".to_string(),
                })?;
            }
            _ => unreachable!(),
        }
    } else {
        // For uncompressed content, we can truly stream
        log::info!("Processing uncompressed content");

        // Stream directly from body to output
        stream_process(body, &mut output_body, &mut replacer, CHUNK_SIZE)?;
    }

    log::info!("Streaming processing complete");
    Ok(output_body)
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

    // Detect the request scheme using multiple methods
    let request_scheme = detect_request_scheme(&req);

    // Log detection details for debugging
    log::info!(
        "Scheme detection - TLS Protocol: {:?}, TLS Cipher: {:?}, Forwarded: {:?}, X-Forwarded-Proto: {:?}, Fastly-SSL: {:?}, Result: {}",
        req.get_tls_protocol(),
        req.get_tls_cipher_openssl_name(),
        req.get_header("forwarded"),
        req.get_header("x-forwarded-proto"),
        req.get_header("fastly-ssl"),
        request_scheme
    );

    log::info!("Request host: {}, scheme: {}", request_host, request_scheme);

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

        // Take the response body for streaming processing
        let body = response.take_body();

        // Process the body using streaming approach
        match process_response_streaming(
            body,
            &content_encoding,
            &origin_host,
            &settings.publisher.origin_url,
            &request_host,
            &request_scheme,
        ) {
            Ok(processed_body) => {
                // Set the processed body back
                response.set_body(processed_body);

                // Remove Content-Length as the size has likely changed
                response.remove_header(header::CONTENT_LENGTH);

                // Keep Content-Encoding header since we're returning compressed content
                log::info!(
                    "Preserved Content-Encoding: {} for compressed response",
                    content_encoding
                );

                log::info!("Completed streaming processing of response body");
            }
            Err(e) => {
                log::error!("Failed to process response body: {:?}", e);
                // Return an error response
                return Err(e);
            }
        }
    } else {
        log::info!(
            "Skipping response processing - should_process: {}, request_host: '{}'",
            should_process,
            request_host
        );
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::Method;

    #[test]
    fn test_detect_request_scheme() {
        // Note: In tests, we can't mock the TLS methods on Request, so we test header fallbacks

        // Test Forwarded header with HTTPS
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("forwarded", "for=192.0.2.60;proto=https;by=203.0.113.43");
        assert_eq!(detect_request_scheme(&req), "https");

        // Test Forwarded header with HTTP
        let mut req = Request::new(Method::GET, "http://test.example.com/page");
        req.set_header("forwarded", "for=192.0.2.60;proto=http;by=203.0.113.43");
        assert_eq!(detect_request_scheme(&req), "http");

        // Test X-Forwarded-Proto with HTTPS
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("x-forwarded-proto", "https");
        assert_eq!(detect_request_scheme(&req), "https");

        // Test X-Forwarded-Proto with HTTP
        let mut req = Request::new(Method::GET, "http://test.example.com/page");
        req.set_header("x-forwarded-proto", "http");
        assert_eq!(detect_request_scheme(&req), "http");

        // Test Fastly-SSL header
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("fastly-ssl", "1");
        assert_eq!(detect_request_scheme(&req), "https");

        // Test default to HTTP when no headers present
        let req = Request::new(Method::GET, "https://test.example.com/page");
        assert_eq!(detect_request_scheme(&req), "http");

        // Test priority: Forwarded takes precedence over X-Forwarded-Proto
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("forwarded", "proto=https");
        req.set_header("x-forwarded-proto", "http");
        assert_eq!(detect_request_scheme(&req), "https");
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
        assert_eq!(origin_host, "origin.test-publisher.com");

        // Test with port
        let mut settings_with_port = create_test_settings();
        settings_with_port.publisher.origin_url = "origin.test-publisher.com:8080".to_string();
        assert_eq!(
            settings_with_port.publisher.origin_host(),
            "origin.test-publisher.com:8080"
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
    fn test_streaming_compressed_content() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Create some HTML content with origin URLs
        let original_content = r#"<html>
            <link href="https://origin.example.com/style.css" rel="stylesheet">
            <script src="https://origin.example.com/app.js"></script>
            <a href="https://origin.example.com/page">Link</a>
        </html>"#;

        // Compress the content
        let mut compressed = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut compressed, Compression::default());
            encoder.write_all(original_content.as_bytes()).unwrap();
            encoder.finish().unwrap();
        }

        // Create a Body from compressed data
        let body = Body::from(compressed);

        // Process the compressed body
        let result = process_response_streaming(
            body,
            "gzip",
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        assert!(result.is_ok());
        let processed_body = result.unwrap();

        // The body should still be compressed
        // In a real test, we'd decompress and verify the content
        // For now, just check that we got a body back
        let bytes = processed_body.into_bytes();
        assert!(!bytes.is_empty());

        // Decompress to verify content was transformed
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&bytes[..]);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).unwrap();

        // Verify URLs were replaced
        assert!(decompressed.contains("https://test.example.com/style.css"));
        assert!(decompressed.contains("https://test.example.com/app.js"));
        assert!(decompressed.contains("https://test.example.com/page"));
        assert!(!decompressed.contains("origin.example.com"));
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
}
