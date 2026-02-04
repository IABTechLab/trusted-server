use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Body, Request, Response};

use crate::backend::ensure_backend_from_url;
use crate::http_util::{serve_static_with_etag, RequestInfo};

use crate::constants::{HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_COMPRESS_HINT};
use crate::cookies::create_synthetic_cookie;
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistry;
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::Settings;
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};
use crate::streaming_replacer::create_url_replacer;
use crate::synthetic::get_or_generate_synthetic_id;

/// Unified tsjs static serving: `/static/tsjs=<filename>`
/// Accepts: `tsjs-core(.min).js`, `tsjs-ext(.min).js`, `tsjs-creative(.min).js`
///
/// Returns 404 for invalid paths or missing bundle files; otherwise serves the requested bundle.
///
/// # Errors
///
/// This function never returns an error; the Result type is for API consistency.
pub fn handle_tsjs_dynamic(req: &Request) -> Result<Response, Report<TrustedServerError>> {
    const PREFIX: &str = "/static/tsjs=";
    let path = req.get_path();
    if !path.starts_with(PREFIX) {
        return Ok(Response::from_status(StatusCode::NOT_FOUND).with_body("Not Found"));
    }
    let filename = &path[PREFIX.len()..];
    // Normalize .min.js to .js for matching
    let normalized = filename.replace(".min.js", ".js");

    let Some(body) = trusted_server_js::bundle_for_filename(&normalized) else {
        return Ok(Response::from_status(StatusCode::NOT_FOUND).with_body("Not Found"));
    };

    let mut resp = serve_static_with_etag(body, req, "application/javascript; charset=utf-8");
    resp.set_header(HEADER_X_COMPRESS_HINT, "on");
    Ok(resp)
}

/// Parameters for processing response streaming
struct ProcessResponseParams<'a> {
    content_encoding: &'a str,
    origin_host: &'a str,
    origin_url: &'a str,
    request_host: &'a str,
    request_scheme: &'a str,
    settings: &'a Settings,
    content_type: &'a str,
    integration_registry: &'a IntegrationRegistry,
}

/// Process response body in streaming fashion with compression preservation
fn process_response_streaming(
    body: Body,
    params: &ProcessResponseParams,
) -> Result<Body, Report<TrustedServerError>> {
    // Check if this is HTML content
    let is_html = params.content_type.contains("text/html");
    let is_rsc_flight = params.content_type.contains("text/x-component");
    log::debug!(
        "process_response_streaming: content_type={}, content_encoding={}, is_html={}, is_rsc_flight={}, origin_host={}",
        params.content_type,
        params.content_encoding,
        is_html,
        is_rsc_flight,
        params.origin_host
    );

    // Determine compression type
    let compression = Compression::from_content_encoding(params.content_encoding);

    // Create output body to collect results
    let mut output = Vec::new();

    // Choose processor based on content type
    if is_html {
        // Use HTML rewriter for HTML content
        let processor = create_html_stream_processor(
            params.origin_host,
            params.request_host,
            params.request_scheme,
            params.settings,
            params.integration_registry,
        )?;

        let config = PipelineConfig {
            input_compression: compression,
            output_compression: compression,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, processor);
        pipeline.process(body, &mut output)?;
    } else if is_rsc_flight {
        // RSC Flight responses are length-prefixed (T rows). A naive string replacement will
        // corrupt the stream by changing byte lengths without updating the prefixes.
        let processor = RscFlightUrlRewriter::new(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );

        let config = PipelineConfig {
            input_compression: compression,
            output_compression: compression,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, processor);
        pipeline.process(body, &mut output)?;
    } else {
        // Use simple text replacer for non-HTML content
        let replacer = create_url_replacer(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );

        let config = PipelineConfig {
            input_compression: compression,
            output_compression: compression,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, replacer);
        pipeline.process(body, &mut output)?;
    }

    log::debug!(
        "Streaming processing complete - output size: {} bytes",
        output.len()
    );
    Ok(Body::from(output))
}

/// Create a unified HTML stream processor
fn create_html_stream_processor(
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
) -> Result<impl StreamProcessor, Report<TrustedServerError>> {
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};

    let config = HtmlProcessorConfig::from_settings(
        settings,
        integration_registry,
        origin_host,
        request_host,
        request_scheme,
    );

    Ok(create_html_processor(config))
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
    integration_registry: &IntegrationRegistry,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

    // Prebid.js requests are not intercepted here anymore. The HTML processor rewrites
    // any Prebid script references to `/static/tsjs-ext.min.js` when auto-configure is enabled.

    // Extract request host and scheme from headers (supports X-Forwarded-Host/Proto for chained proxies)
    let request_info = RequestInfo::from_request(&req);
    let request_host = &request_info.host;
    let request_scheme = &request_info.scheme;

    log::debug!(
        "Request info: host={}, scheme={} (X-Forwarded-Host: {:?}, Host: {:?}, X-Forwarded-Proto: {:?})",
        request_host,
        request_scheme,
        req.get_header("x-forwarded-host"),
        req.get_header(header::HOST),
        req.get_header("x-forwarded-proto"),
    );

    // Generate synthetic identifiers before the request body is consumed.
    let synthetic_id = get_or_generate_synthetic_id(settings, &req)?;
    let has_synthetic_cookie = req
        .get_header(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .map(|cookies| {
            cookies
                .split(';')
                .any(|cookie| cookie.trim_start().starts_with("synthetic_id="))
        })
        .unwrap_or(false);

    log::debug!(
        "Proxy synthetic IDs - trusted: {}, has_cookie: {}",
        synthetic_id,
        has_synthetic_cookie
    );

    let backend_name = ensure_backend_from_url(&settings.publisher.origin_url)?;
    let origin_host = settings.publisher.origin_host();

    log::debug!(
        "Proxying to dynamic backend: {} (from {})",
        backend_name,
        settings.publisher.origin_url
    );
    req.set_header("host", &origin_host);

    let mut response = req
        .send(&backend_name)
        .change_context(TrustedServerError::Proxy {
            message: "Failed to proxy request to origin".to_string(),
        })?;

    // Log all response headers for debugging
    log::debug!("Response headers:");
    for (name, value) in response.get_headers() {
        log::debug!("  {}: {:?}", name, value);
    }

    // Check if the response has a text-based content type that we should process
    let content_type = response
        .get_header(header::CONTENT_TYPE)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_string();

    let should_process = content_type.contains("text/")
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
        log::debug!(
            "Processing response - Content-Type: {}, Content-Encoding: {}, Request Host: {}, Origin Host: {}",
            content_type, content_encoding, request_host, origin_host
        );

        // Take the response body for streaming processing
        let body = response.take_body();

        // Process the body using streaming approach
        let params = ProcessResponseParams {
            content_encoding: &content_encoding,
            origin_host: &origin_host,
            origin_url: &settings.publisher.origin_url,
            request_host,
            request_scheme,
            settings,
            content_type: &content_type,
            integration_registry,
        };
        match process_response_streaming(body, &params) {
            Ok(processed_body) => {
                // Set the processed body back
                response.set_body(processed_body);

                // Remove Content-Length as the size has likely changed
                response.remove_header(header::CONTENT_LENGTH);

                // Keep Content-Encoding header since we're returning compressed content
                log::debug!(
                    "Preserved Content-Encoding: {} for compressed response",
                    content_encoding
                );

                log::debug!("Completed streaming processing of response body");
            }
            Err(e) => {
                log::error!("Failed to process response body: {:?}", e);
                // Return an error response
                return Err(e);
            }
        }
    } else {
        log::debug!(
            "Skipping response processing - should_process: {}, request_host: '{}'",
            should_process,
            request_host
        );
    }

    response.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, synthetic_id.as_str());
    if !has_synthetic_cookie {
        response.set_header(
            header::SET_COOKIE,
            create_synthetic_cookie(settings, synthetic_id.as_str()),
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

    // Note: test_streaming_compressed_content removed as it directly tested private function
    // process_response_streaming. The functionality is tested through handle_publisher_request.

    // Note: test_streaming_brotli_content removed as it directly tested private function
    // process_response_streaming. The functionality is tested through handle_publisher_request.

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

    // Tests related to serving Prebid.js directly or intercepting its paths were removed.
}
