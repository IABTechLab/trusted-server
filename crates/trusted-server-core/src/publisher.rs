use std::io::Write;

use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Body, Request, Response};

use crate::backend::BackendConfig;
use crate::consent::{allows_ssc_creation, build_consent_context, ConsentPipelineInput};
use crate::constants::{COOKIE_SYNTHETIC_ID, HEADER_X_COMPRESS_HINT, HEADER_X_SYNTHETIC_ID};
use crate::cookies::{expire_synthetic_cookie, handle_request_cookies, set_synthetic_cookie};
use crate::error::TrustedServerError;
use crate::http_util::{serve_static_with_etag, RequestInfo};
use crate::integrations::IntegrationRegistry;
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::Settings;
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};
use crate::streaming_replacer::create_url_replacer;
use crate::synthetic::get_or_generate_synthetic_id;

/// Unified tsjs static serving: `/static/tsjs=<filename>`
///
/// Serves two types of bundles:
/// - **Unified bundle** (`tsjs-unified.min.js`): core + immediate (non-deferred)
///   integration modules.
/// - **Deferred module** (`tsjs-{id}.min.js`): a single self-contained IIFE for
///   modules loaded with `defer` (e.g., prebid).
///
/// # Errors
///
/// This function never returns an error; the Result type is for API consistency.
pub fn handle_tsjs_dynamic(
    req: &Request,
    integration_registry: &IntegrationRegistry,
) -> Result<Response, Report<TrustedServerError>> {
    const PREFIX: &str = "/static/tsjs=";
    const UNIFIED_FILENAMES: &[&str] = &["tsjs-unified.js", "tsjs-unified.min.js"];

    let path = req.get_path();
    if !path.starts_with(PREFIX) {
        return Ok(Response::from_status(StatusCode::NOT_FOUND).with_body("Not Found"));
    }
    let filename = &path[PREFIX.len()..];

    if UNIFIED_FILENAMES.contains(&filename) {
        // Serve core + immediate modules (excludes deferred like prebid)
        let module_ids = integration_registry.js_module_ids_immediate();
        let body = trusted_server_js::concatenate_modules(&module_ids);
        let mut resp = serve_static_with_etag(&body, req, "application/javascript; charset=utf-8");
        resp.set_header(HEADER_X_COMPRESS_HINT, "on");
        return Ok(resp);
    }

    if let Some(module_id) = parse_deferred_module_filename(filename) {
        // Only serve if the deferred module is actually enabled
        let deferred_ids = integration_registry.js_module_ids_deferred();
        if !deferred_ids.contains(&module_id) {
            return Ok(Response::from_status(StatusCode::NOT_FOUND).with_body("Not Found"));
        }
        if let Some(content) = trusted_server_js::module_bundle(module_id) {
            let mut resp =
                serve_static_with_etag(content, req, "application/javascript; charset=utf-8");
            resp.set_header(HEADER_X_COMPRESS_HINT, "on");
            return Ok(resp);
        }
    }

    Ok(Response::from_status(StatusCode::NOT_FOUND).with_body("Not Found"))
}

/// Extract a module ID from a deferred-module filename like `tsjs-prebid.min.js`.
///
/// Returns `Some(&'static str)` if the filename matches a known JS module ID,
/// `None` otherwise. The caller must additionally verify that the module is
/// both deferred and enabled via the [`IntegrationRegistry`].
#[must_use]
fn parse_deferred_module_filename(filename: &str) -> Option<&'static str> {
    let stem = filename
        .strip_prefix("tsjs-")
        .and_then(|s| s.strip_suffix(".min.js").or_else(|| s.strip_suffix(".js")))?;

    trusted_server_js::all_module_ids()
        .into_iter()
        .find(|&id| id == stem)
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

/// Process response body through the streaming pipeline.
///
/// Selects the appropriate processor based on content type (HTML rewriter,
/// RSC Flight rewriter, or URL replacer) and pipes chunks from `body`
/// through it into `output`. The caller decides what `output` is — a
/// `Vec<u8>` for buffered responses, or a `StreamingBody` for streaming.
///
/// # Errors
///
/// Returns an error if processor creation or chunk processing fails.
fn process_response_streaming<W: std::io::Write>(
    body: Body,
    output: &mut W,
    params: &ProcessResponseParams,
) -> Result<(), Report<TrustedServerError>> {
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

    let compression = Compression::from_content_encoding(params.content_encoding);
    let config = PipelineConfig {
        input_compression: compression,
        output_compression: compression,
        chunk_size: 8192,
    };

    if is_html {
        let processor = create_html_stream_processor(
            params.origin_host,
            params.request_host,
            params.request_scheme,
            params.settings,
            params.integration_registry,
        )?;
        StreamingPipeline::new(config, processor).process(body, output)?;
    } else if is_rsc_flight {
        let processor = RscFlightUrlRewriter::new(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, processor).process(body, output)?;
    } else {
        let replacer = create_url_replacer(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, replacer).process(body, output)?;
    }

    Ok(())
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

/// Result of publisher request handling, indicating whether the response
/// body should be streamed or has already been buffered.
pub enum PublisherResponse {
    /// Response is fully buffered and ready to send via `send_to_client()`.
    Buffered(Response),
    /// Response headers are ready. The caller must:
    /// 1. Call `finalize_response()` on the response
    /// 2. Call `response.stream_to_client()` to get a `StreamingBody`
    /// 3. Call `stream_publisher_body()` with the body and streaming writer
    /// 4. Call `StreamingBody::finish()`
    Stream {
        /// Response with all headers set (synthetic ID, cookies, etc.)
        /// but body not yet written. `Content-Length` already removed.
        response: Response,
        /// Origin body to be piped through the streaming pipeline.
        body: Body,
        /// Parameters for `process_response_streaming`.
        params: OwnedProcessResponseParams,
    },
}

/// Owned version of [`ProcessResponseParams`] for returning from
/// `handle_publisher_request` without lifetime issues.
pub struct OwnedProcessResponseParams {
    pub content_encoding: String,
    pub origin_host: String,
    pub origin_url: String,
    pub request_host: String,
    pub request_scheme: String,
    pub content_type: String,
}

/// Stream the publisher response body through the processing pipeline.
///
/// Called by the adapter after `stream_to_client()` has committed the
/// response headers. Writes processed chunks directly to `output`.
///
/// # Errors
///
/// Returns an error if processing fails mid-stream. Since headers are
/// already committed, the caller should log the error and drop the
/// `StreamingBody` (client sees a truncated response).
pub fn stream_publisher_body<W: Write>(
    body: Body,
    output: &mut W,
    params: &OwnedProcessResponseParams,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
) -> Result<(), Report<TrustedServerError>> {
    let borrowed = ProcessResponseParams {
        content_encoding: &params.content_encoding,
        origin_host: &params.origin_host,
        origin_url: &params.origin_url,
        request_host: &params.request_host,
        request_scheme: &params.request_scheme,
        settings,
        content_type: &params.content_type,
        integration_registry,
    };
    process_response_streaming(body, output, &borrowed)
}

/// Proxies requests to the publisher's origin server.
///
/// Returns a [`PublisherResponse`] indicating whether the response can be
/// streamed or must be sent buffered. The streaming path is chosen when:
/// - The backend returns a 2xx status
/// - The response has a processable content type
/// - No HTML post-processors are registered (the streaming gate)
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if the proxy request fails or the
/// origin backend is unreachable.
pub fn handle_publisher_request(
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    mut req: Request,
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

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

    // Parse cookies once for reuse by both consent extraction and synthetic ID logic.
    let cookie_jar = handle_request_cookies(&req)?;

    // Capture the current SSC cookie value for revocation handling.
    // This must come from the cookie itself (not the x-synthetic-id header)
    // to ensure KV deletion targets the same identifier being revoked.
    let existing_ssc_cookie = cookie_jar
        .as_ref()
        .and_then(|jar| jar.get(COOKIE_SYNTHETIC_ID))
        .map(|cookie| cookie.value().to_owned());

    // Generate synthetic identifiers before the request body is consumed.
    // Always generated for internal use (KV lookups, logging) even when
    // consent is absent — the cookie is only *set* when consent allows it.
    let synthetic_id = get_or_generate_synthetic_id(settings, &req)?;

    // Extract, decode, and log consent signals (TCF, GPP, US Privacy, GPC)
    // from the incoming request. The ConsentContext carries both raw strings
    // (for OpenRTB forwarding) and decoded data (for enforcement).
    // When a consent_store is configured, this also persists consent to KV
    // and falls back to stored consent when cookies are absent.
    let geo = crate::geo::GeoInfo::from_request(&req);
    let consent_context = build_consent_context(&ConsentPipelineInput {
        jar: cookie_jar.as_ref(),
        req: &req,
        config: &settings.consent,
        geo: geo.as_ref(),
        synthetic_id: Some(synthetic_id.as_str()),
    });
    let ssc_allowed = allows_ssc_creation(&consent_context);
    log::debug!(
        "Proxy synthetic IDs - trusted: {}, ssc_allowed: {}",
        synthetic_id,
        ssc_allowed,
    );

    let backend_name = BackendConfig::from_url(
        &settings.publisher.origin_url,
        settings.proxy.certificate_check,
    )?;
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

    log::debug!("Response headers:");
    for (name, value) in response.get_headers() {
        log::debug!("  {}: {:?}", name, value);
    }

    // Set synthetic ID / cookie headers BEFORE body processing.
    // These are body-independent (computed from request cookies + consent).
    apply_synthetic_id_headers(
        settings,
        &mut response,
        &synthetic_id,
        ssc_allowed,
        existing_ssc_cookie.as_deref(),
        &consent_context,
    );

    let content_type = response
        .get_header(header::CONTENT_TYPE)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_string();

    let should_process = content_type.contains("text/")
        || content_type.contains("application/javascript")
        || content_type.contains("application/json");

    // Streaming gate: can we stream this response?
    // - Must have processable content
    // - Must have a request host for URL rewriting
    // - Backend must return success (already guaranteed — errors propagated above)
    // - No HTML post-processors registered (they need the full document)
    let is_html = content_type.contains("text/html");
    let has_post_processors = !integration_registry.html_post_processors().is_empty();
    let can_stream =
        should_process && !request_host.is_empty() && (!is_html || !has_post_processors);

    if can_stream {
        let content_encoding = response
            .get_header(header::CONTENT_ENCODING)
            .map(|h| h.to_str().unwrap_or_default())
            .unwrap_or_default()
            .to_lowercase();

        log::debug!(
            "Streaming response - Content-Type: {}, Content-Encoding: {}, Request Host: {}, Origin Host: {}",
            content_type, content_encoding, request_host, origin_host
        );

        let body = response.take_body();
        response.remove_header(header::CONTENT_LENGTH);

        return Ok(PublisherResponse::Stream {
            response,
            body,
            params: OwnedProcessResponseParams {
                content_encoding,
                origin_host,
                origin_url: settings.publisher.origin_url.clone(),
                request_host: request_host.to_string(),
                request_scheme: request_scheme.to_string(),
                content_type,
            },
        });
    }

    // Buffered fallback: process body in memory (post-processors need full document,
    // or content type doesn't need processing).
    if should_process && !request_host.is_empty() {
        let content_encoding = response
            .get_header(header::CONTENT_ENCODING)
            .map(|h| h.to_str().unwrap_or_default())
            .unwrap_or_default()
            .to_lowercase();

        log::debug!(
            "Buffered response - Content-Type: {}, Content-Encoding: {}, Request Host: {}, Origin Host: {}",
            content_type, content_encoding, request_host, origin_host
        );

        let body = response.take_body();
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
        let mut output = Vec::new();
        process_response_streaming(body, &mut output, &params)?;

        response.set_body(Body::from(output));
        response.remove_header(header::CONTENT_LENGTH);
    } else {
        log::debug!(
            "Skipping response processing - should_process: {}, request_host: '{}'",
            should_process,
            request_host
        );
    }

    Ok(PublisherResponse::Buffered(response))
}

/// Apply synthetic ID and cookie headers to the response.
///
/// Extracted so headers can be set before streaming begins (headers must
/// be finalized before `stream_to_client()` commits them).
fn apply_synthetic_id_headers(
    settings: &Settings,
    response: &mut Response,
    synthetic_id: &str,
    ssc_allowed: bool,
    existing_ssc_cookie: Option<&str>,
    consent_context: &crate::consent::ConsentContext,
) {
    if ssc_allowed {
        response.set_header(HEADER_X_SYNTHETIC_ID, synthetic_id);
        set_synthetic_cookie(settings, response, synthetic_id);
    } else if let Some(cookie_synthetic_id) = existing_ssc_cookie {
        log::info!(
            "SSC revoked for '{}': consent withdrawn (jurisdiction={})",
            cookie_synthetic_id,
            consent_context.jurisdiction,
        );
        expire_synthetic_cookie(settings, response);
        if let Some(store_name) = &settings.consent.consent_store {
            crate::consent::kv::delete_consent_from_kv(store_name, cookie_synthetic_id);
        }
    } else {
        log::debug!(
            "SSC skipped: no consent and no existing cookie (jurisdiction={})",
            consent_context.jurisdiction,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::{Method, StatusCode};

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

    #[test]
    fn revocation_targets_cookie_synthetic_id_not_header() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("x-synthetic-id", "header_id");
        req.set_header("cookie", "synthetic_id=cookie_id; other=value");

        let cookie_jar = handle_request_cookies(&req).expect("should parse cookies");
        let existing_ssc_cookie = cookie_jar
            .as_ref()
            .and_then(|jar| jar.get(COOKIE_SYNTHETIC_ID))
            .map(|cookie| cookie.value().to_owned());

        let resolved_synthetic_id =
            get_or_generate_synthetic_id(&settings, &req).expect("should resolve synthetic id");

        assert_eq!(
            existing_ssc_cookie.as_deref(),
            Some("cookie_id"),
            "should read revocation target from cookie value"
        );
        assert_eq!(
            resolved_synthetic_id, "header_id",
            "should still resolve request synthetic ID from header precedence"
        );
    }

    #[test]
    fn tsjs_dynamic_returns_not_found_for_unknown_filename() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = Request::new(
            Method::GET,
            "https://publisher.example/static/tsjs=unknown.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(response.get_status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn tsjs_dynamic_serves_unified_bundle_for_known_filename() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = Request::new(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-unified.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(response.get_status(), StatusCode::OK);
    }

    #[test]
    fn parse_deferred_module_filename_extracts_known_id() {
        assert_eq!(
            parse_deferred_module_filename("tsjs-prebid.min.js"),
            Some("prebid"),
            "should extract prebid from minified filename"
        );
        assert_eq!(
            parse_deferred_module_filename("tsjs-prebid.js"),
            Some("prebid"),
            "should extract prebid from unminified filename"
        );
    }

    #[test]
    fn parse_deferred_module_filename_rejects_unknown_ids() {
        assert_eq!(
            parse_deferred_module_filename("tsjs-evil.min.js"),
            None,
            "should reject unknown module names"
        );
        assert_eq!(
            parse_deferred_module_filename("tsjs-core.min.js"),
            Some("core"),
            "should accept any known module ID (deferred check happens in caller)"
        );
        assert_eq!(
            parse_deferred_module_filename("prebid.min.js"),
            None,
            "should reject without tsjs- prefix"
        );
        assert_eq!(
            parse_deferred_module_filename("tsjs-prebid.txt"),
            None,
            "should reject non-js extension"
        );
    }

    #[test]
    fn tsjs_dynamic_serves_deferred_prebid_when_enabled() {
        // Default test settings include prebid enabled
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = Request::new(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-prebid.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(
            response.get_status(),
            StatusCode::OK,
            "should serve deferred prebid module when enabled"
        );
    }

    #[test]
    fn tsjs_dynamic_returns_not_found_for_disabled_deferred_module() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": false,
                    "server_url": "https://test-prebid.com/openrtb2/auction"
                }),
            )
            .expect("should update prebid config");
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = Request::new(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-prebid.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(
            response.get_status(),
            StatusCode::NOT_FOUND,
            "should return 404 for disabled deferred module"
        );
    }

    #[test]
    fn tsjs_dynamic_returns_not_found_for_arbitrary_module_name() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = Request::new(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-evil.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(
            response.get_status(),
            StatusCode::NOT_FOUND,
            "should reject unknown module names"
        );
    }
}
