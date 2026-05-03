//! Publisher response handler.
//!
//! **Note on platform coupling:** This module is currently coupled to
//! `fastly::Body`/`Request`/`Response` at its handler boundaries — the entry
//! points ([`handle_publisher_request`], [`stream_publisher_body`]) still
//! accept and return `fastly::Body` and `fastly::Response`. The streaming
//! processor itself is generic: [`process_response_streaming`] writes into
//! any [`Write`] (a `Vec<u8>` for buffered routes, a `StreamingBody` for the
//! streaming route). The HTTP-type coupling will be addressed in the
//! platform HTTP-type migration alongside all other
//! `fastly::Request`/`Response`/`Body` migrations. It is not a
//! content-rewriting concern.

use std::io::Write;

use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Body, Request, Response};

use crate::backend::BackendConfig;
use crate::consent::{allows_ec_creation, build_consent_context, ConsentPipelineInput};
use crate::constants::{COOKIE_TS_EC, HEADER_X_COMPRESS_HINT, HEADER_X_TS_EC};
use crate::cookies::{expire_ec_cookie, handle_request_cookies, set_ec_cookie};
use crate::creative_opportunities::{CreativeOpportunitiesConfig, CreativeOpportunitySlot};
use crate::edge_cookie::get_or_generate_ec_id;
use crate::error::TrustedServerError;
use crate::http_util::{serve_static_with_etag, RequestInfo};
use crate::integrations::IntegrationRegistry;
use crate::platform::RuntimeServices;
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::Settings;
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};
use crate::streaming_replacer::create_url_replacer;
const SUPPORTED_ENCODING_VALUES: [&str; 3] = ["gzip", "deflate", "br"];

fn restrict_accept_encoding(req: &mut Request) {
    // If the client sent no Accept-Encoding, leave the request unchanged so the
    // origin responds without compression. Adding encodings here would cause the
    // origin to compress its response even though the client never asked for it,
    // and the client would then receive content it cannot decode.
    let Some(current) = req
        .get_header(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    req.set_header(
        header::ACCEPT_ENCODING,
        select_supported_accept_encoding(current),
    );
}

fn select_supported_accept_encoding(client_accept_encoding: &str) -> String {
    let supported_subset = SUPPORTED_ENCODING_VALUES
        .into_iter()
        .filter(|encoding| client_accepts_content_encoding(client_accept_encoding, encoding))
        .collect::<Vec<_>>();

    if supported_subset.is_empty() {
        return "identity".to_string();
    }

    supported_subset.join(", ")
}

fn client_accepts_content_encoding(header_value: &str, encoding: &str) -> bool {
    accept_encoding_qvalue(header_value, encoding)
        .or_else(|| accept_encoding_qvalue(header_value, "*"))
        .is_some_and(|qvalue| qvalue > 0.0)
}

fn accept_encoding_qvalue(header_value: &str, target: &str) -> Option<f32> {
    let mut matched_qvalue = None;

    for item in header_value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }

        let mut parts = item.split(';');
        let Some(token) = parts.next().map(str::trim) else {
            continue;
        };
        if !token.eq_ignore_ascii_case(target) {
            continue;
        }

        let mut qvalue = 1.0;
        for parameter in parts {
            let Some((name, value)) = parameter.trim().split_once('=') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("q") {
                if let Ok(parsed_qvalue) = value.trim().parse::<f32>() {
                    qvalue = parsed_qvalue;
                }
            }
        }

        // First match wins per RFC 7231 — duplicate tokens are non-normative,
        // but using first-match is the conventional interpretation.
        matched_qvalue = Some(qvalue);
        break;
    }

    matched_qvalue
}

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

/// Build the head script that exposes server-side ad slot metadata.
#[allow(dead_code)]
#[must_use]
pub(crate) fn build_head_globals_script(
    matched_slots: &[&CreativeOpportunitySlot],
    request_id: &str,
    co_config: &CreativeOpportunitiesConfig,
) -> String {
    #[derive(serde::Serialize)]
    struct HeadAdSlot<'a> {
        id: &'a str,
        gam_unit_path: String,
        div_id: String,
        formats: Vec<[u32; 2]>,
        targeting: &'a std::collections::HashMap<String, String>,
    }

    let slots = matched_slots
        .iter()
        .map(|slot| HeadAdSlot {
            id: &slot.id,
            gam_unit_path: slot.resolved_gam_unit_path(co_config),
            div_id: slot.resolved_div_id(),
            formats: slot
                .formats
                .iter()
                .map(|format| [format.width, format.height])
                .collect(),
            targeting: &slot.targeting,
        })
        .collect::<Vec<_>>();

    let slots_json = serde_json::to_string(&slots).expect("should serialize ad slots");
    let request_id_json = serde_json::to_string(request_id).expect("should serialize request ID");
    let escaped_slots_json = html_escape_for_script(&slots_json);
    let escaped_request_id_json = html_escape_for_script(&request_id_json);

    format!(
        "<script>window.__ts_ad_slots=JSON.parse(\"{escaped_slots_json}\");window.__ts_request_id=JSON.parse(\"{escaped_request_id_json}\");</script>"
    )
}

/// Escape JSON so it can be embedded in a JavaScript string inside an HTML script.
#[allow(dead_code)]
#[must_use]
pub(crate) fn html_escape_for_script(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len());
    for ch in json.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '<' => escaped.push_str("\\u003C"),
            '>' => escaped.push_str("\\u003E"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[allow(dead_code)]
#[must_use]
pub(crate) fn build_bid_map(
    winning_bids: &std::collections::HashMap<String, crate::auction::types::Bid>,
    price_granularity: crate::price_bucket::PriceGranularity,
) -> crate::bid_cache::BidMap {
    winning_bids
        .iter()
        .filter_map(|(slot_id, bid)| {
            let cpm = bid.price?;
            Some((
                slot_id.clone(),
                serde_json::json!({
                    "hb_pb": crate::price_bucket::price_bucket(cpm, price_granularity),
                    "hb_bidder": bid.bidder.as_str(),
                    "hb_adid": bid.ad_id.as_deref().unwrap_or(""),
                    "burl": bid.burl.as_deref(),
                }),
            ))
        })
        .collect()
}

#[allow(dead_code)]
fn server_side_auction_allowed(consent_context: &crate::consent::ConsentContext) -> bool {
    consent_context
        .tcf
        .as_ref()
        .is_some_and(|tcf| tcf.has_purpose_consent(1))
}

#[allow(dead_code)]
fn apply_server_side_ad_cache_policy(
    response: &mut Response,
    slots_matched: bool,
    globals_injected: bool,
) {
    if !slots_matched {
        return;
    }

    if globals_injected || slots_matched {
        response.set_header(header::CACHE_CONTROL, "private, no-store");
    }
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
fn process_response_streaming<W: Write>(
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
    /// Response headers are ready for a streaming response. Covers processable
    /// content on any status (2xx or non-2xx — e.g., branded 404/500 HTML and
    /// error JSON still get URL rewriting) where the encoding is supported
    /// and either the content is non-HTML or no HTML post-processors are
    /// registered. The caller must:
    /// 1. Call `finalize_response()` on the response
    /// 2. Call `response.stream_to_client()` to get a `StreamingBody`
    /// 3. Call `stream_publisher_body()` with the body and streaming writer
    /// 4. Call `StreamingBody::finish()`
    Stream {
        /// Response with all headers set (EC ID, cookies, etc.)
        /// but body not yet written. `Content-Length` already removed.
        response: Response,
        /// Origin body to be piped through the streaming pipeline.
        body: Body,
        /// Parameters for `process_response_streaming`.
        params: OwnedProcessResponseParams,
    },
    /// Non-processable 2xx response (images, fonts, video). The adapter must
    /// reattach the body via `response.set_body(body)` before returning.
    /// `finalize_response()` and `send_to_client()` are applied at the outer
    /// response-dispatch level, not in this arm.
    ///
    /// `Content-Length` is preserved — the body is unmodified. Using
    /// `send_to_client()` instead of `stream_to_client()` avoids chunked
    /// encoding overhead. Fastly streams the body from its internal buffer
    /// without copying into WASM memory.
    PassThrough {
        /// Response with all headers set but body not yet written.
        response: Response,
        /// Origin body to stream directly to the client.
        body: Body,
    },
}

/// Routing decision for a proxied response.
///
/// Computed purely from response metadata — no side effects, no body is
/// consumed. [`handle_publisher_request`] calls [`classify_response_route`]
/// once and dispatches to the matching [`PublisherResponse`] arm. Tests
/// exercise the classifier directly so the gate formula lives in one place.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResponseRoute {
    /// 2xx non-processable (images, fonts, video), not 204/205. Origin body
    /// is streamed unmodified via [`PublisherResponse::PassThrough`].
    PassThrough,
    /// Processable content with supported encoding and either non-HTML or no
    /// HTML post-processors registered. Covers both 2xx and non-2xx (e.g.,
    /// branded 404/500 pages still get origin URL rewriting). Routed through
    /// [`PublisherResponse::Stream`].
    Stream,
    /// Response returned unmodified via [`PublisherResponse::Buffered`] — covers
    /// 204/205 (RFC-prohibited bodies), empty request host with non-processable
    /// content, and unsupported encodings.
    BufferedUnmodified,
    /// HTML with post-processors registered. Runs the full pipeline into a
    /// buffer, then returns [`PublisherResponse::Buffered`] with the processed body.
    BufferedProcessed,
}

/// Decide how a proxied response should be routed.
///
/// Pure: no header mutation, no body consumed. All inputs are extracted
/// from the origin response at the call site.
pub(crate) fn classify_response_route(
    status: StatusCode,
    content_type: &str,
    content_encoding: &str,
    request_host: &str,
    has_post_processors: bool,
) -> ResponseRoute {
    // 204 No Content (RFC 9110 §15.3.5) and 205 Reset Content (§15.3.6)
    // prohibit a message body. Excluded first so no later arm can emit one
    // regardless of Content-Type or post-processor registration.
    if status == StatusCode::NO_CONTENT || status == StatusCode::RESET_CONTENT {
        return ResponseRoute::BufferedUnmodified;
    }

    let should_process = is_processable_content_type(content_type);

    // Non-processable content: 2xx streams through unchanged; non-2xx falls
    // back to buffered (the origin's error body reaches the client as-is).
    if !should_process {
        if status.is_success() {
            return ResponseRoute::PassThrough;
        }
        return ResponseRoute::BufferedUnmodified;
    }

    // Processable content (2xx or non-2xx) still needs URL rewriting against
    // a known request host — without one, fall back to unmodified.
    if request_host.is_empty() {
        return ResponseRoute::BufferedUnmodified;
    }

    // Unsupported Content-Encoding: we cannot decompress, so processing would
    // treat compressed bytes as identity and produce garbled output.
    if !is_supported_content_encoding(content_encoding) {
        return ResponseRoute::BufferedUnmodified;
    }

    let is_html = content_type.contains("text/html");
    if is_html && has_post_processors {
        // HTML with post-processors: need the full document to inject.
        return ResponseRoute::BufferedProcessed;
    }

    ResponseRoute::Stream
}

/// Owned version of [`ProcessResponseParams`] for returning from
/// `handle_publisher_request` without lifetime issues.
pub struct OwnedProcessResponseParams {
    pub(crate) content_encoding: String,
    pub(crate) origin_host: String,
    pub(crate) origin_url: String,
    pub(crate) request_host: String,
    pub(crate) request_scheme: String,
    pub(crate) content_type: String,
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
/// Returns a [`PublisherResponse`] indicating how the response should be sent:
/// - [`PassThrough`](PublisherResponse::PassThrough) — 2xx non-processable content
///   (images, fonts, video). Body reattached unmodified for `send_to_client()`.
/// - [`Stream`](PublisherResponse::Stream) — processable content with supported
///   `Content-Encoding` and either non-HTML or no HTML post-processors.
///   Applies to both 2xx and non-2xx status (e.g., branded 404/500 HTML and
///   error JSON still get origin URL rewriting). Body piped through the
///   streaming pipeline.
/// - [`Buffered`](PublisherResponse::Buffered) — non-2xx responses, unsupported
///   encoding, or HTML with post-processors that need the full document.
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if the proxy request fails or the
/// origin backend is unreachable.
pub fn handle_publisher_request(
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

    // Prebid.js requests are not intercepted here anymore. The HTML processor removes
    // publisher-supplied Prebid scripts; the unified TSJS bundle includes Prebid.js when enabled.

    // Extract request host and scheme (uses Host header and TLS detection after edge sanitization)
    let request_info = RequestInfo::from_request(&req, &services.client_info);
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

    // Parse cookies once for reuse by both consent extraction and EC ID logic.
    let cookie_jar = handle_request_cookies(&req)?;

    // Capture the current EC cookie value for revocation handling.
    // This must come from the cookie itself (not the x-ts-ec header)
    // to ensure KV deletion targets the same identifier being revoked.
    let existing_ec_cookie = cookie_jar
        .as_ref()
        .and_then(|jar| jar.get(COOKIE_TS_EC))
        .map(|cookie| cookie.value().to_owned());

    // Generate EC identifiers before the request body is consumed.
    // Always generated for internal use (KV lookups, logging) even when
    // consent is absent — the cookie is only *set* when consent allows it.
    let ec_id = get_or_generate_ec_id(settings, services, &req)?;

    // Extract, decode, and log consent signals (TCF, GPP, US Privacy, GPC)
    // from the incoming request. The ConsentContext carries both raw strings
    // (for OpenRTB forwarding) and decoded data (for enforcement).
    // When a consent_store is configured, this also persists consent to KV
    // and falls back to stored consent when cookies are absent.
    let geo = services
        .geo()
        .lookup(services.client_info.client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });
    let consent_context = build_consent_context(&ConsentPipelineInput {
        jar: cookie_jar.as_ref(),
        req: &req,
        config: &settings.consent,
        geo: geo.as_ref(),
        ec_id: Some(ec_id.as_str()),
        kv_store: settings
            .consent
            .consent_store
            .as_deref()
            .map(|_| services.kv_store()),
    });
    let ec_allowed = allows_ec_creation(&consent_context);
    log::debug!("Proxy ec_allowed: {}", ec_allowed);

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
    // Only advertise encodings the rewrite pipeline can decode and re-encode.
    restrict_accept_encoding(&mut req);
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

    // Set EC ID / cookie headers BEFORE body processing.
    // These are body-independent (computed from request cookies + consent).
    apply_ec_headers(
        settings,
        services,
        &mut response,
        &ec_id,
        ec_allowed,
        existing_ec_cookie.as_deref(),
        &consent_context,
    );

    let content_type = response
        .get_header(header::CONTENT_TYPE)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_string();

    let status = response.get_status();
    let content_encoding = response
        .get_header(header::CONTENT_ENCODING)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_lowercase();
    let has_post_processors = integration_registry.has_html_post_processors();

    let route = classify_response_route(
        status,
        &content_type,
        &content_encoding,
        request_host,
        has_post_processors,
    );

    match route {
        ResponseRoute::PassThrough => {
            log::debug!(
                "Pass-through binary response - Content-Type: '{}', status: {}",
                content_type,
                status,
            );
            let body = response.take_body();
            Ok(PublisherResponse::PassThrough { response, body })
        }
        ResponseRoute::BufferedUnmodified => {
            // Misconfiguration: processable content returned unrewritten because
            // we have no Host header to rewrite URLs against. Surface at WARN so
            // mis-proxied pages are visible in production logs.
            if is_processable_content_type(&content_type) && request_host.is_empty() {
                log::warn!(
                    "Empty request host — returning processable content unmodified (Content-Type: '{}', status: {}). Check proxy Host header.",
                    content_type,
                    status,
                );
            } else if !is_supported_content_encoding(&content_encoding) {
                log::warn!(
                    "Unsupported Content-Encoding '{}' - returning response unmodified",
                    content_encoding,
                );
            } else {
                log::debug!(
                    "Skipping response processing - Content-Type: '{}', request_host: '{}', status: {}",
                    content_type,
                    request_host,
                    status,
                );
            }
            Ok(PublisherResponse::Buffered(response))
        }
        ResponseRoute::Stream => {
            log::debug!(
                "Streaming response - Content-Type: {}, Content-Encoding: {}, Request Host: {}, Origin Host: {}",
                content_type, content_encoding, request_host, origin_host
            );

            let body = response.take_body();
            response.remove_header(header::CONTENT_LENGTH);

            Ok(PublisherResponse::Stream {
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
            })
        }
        ResponseRoute::BufferedProcessed => {
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

            response.set_header(header::CONTENT_LENGTH, output.len().to_string());
            response.set_body(Body::from(output));

            Ok(PublisherResponse::Buffered(response))
        }
    }
}

/// Whether the content type requires processing (URL rewriting, HTML injection).
///
/// Text-based and JavaScript/JSON responses are processable; binary types
/// (images, fonts, video, etc.) pass through unchanged.
fn is_processable_content_type(content_type: &str) -> bool {
    content_type.contains("text/")
        || content_type.contains("application/javascript")
        || content_type.contains("application/json")
}

/// Whether the `Content-Encoding` is one the streaming pipeline can handle.
///
/// Unsupported encodings (e.g. `zstd` from a misbehaving origin) bypass the
/// rewrite pipeline entirely and are returned unchanged. Processing such
/// bodies as identity-encoded would produce garbled output.
fn is_supported_content_encoding(encoding: &str) -> bool {
    matches!(encoding, "" | "identity" | "gzip" | "deflate" | "br")
}

/// Apply EC ID and cookie headers to the response.
///
/// Extracted so headers can be set before streaming begins (headers must
/// be finalized before `stream_to_client()` commits them).
///
/// Consent-gated EC creation:
/// - Consent given → set EC ID header + cookie.
/// - Consent absent + existing cookie → revoke (expire cookie + delete KV entry).
/// - Consent absent + no cookie → do nothing.
fn apply_ec_headers(
    settings: &Settings,
    services: &RuntimeServices,
    response: &mut Response,
    ec_id: &str,
    ec_allowed: bool,
    existing_ec_cookie: Option<&str>,
    consent_context: &crate::consent::ConsentContext,
) {
    if ec_allowed {
        // Fastly's HeaderValue API rejects \r, \n, and \0, so the EC ID
        // cannot inject additional response headers.
        response.set_header(HEADER_X_TS_EC, ec_id);
        // Cookie persistence is skipped if the EC ID contains RFC 6265-illegal
        // characters. The header is still emitted when consent allows it.
        set_ec_cookie(settings, response, ec_id);
    } else if let Some(cookie_ec_id) = existing_ec_cookie {
        log::info!(
            "EC revoked for '{}': consent withdrawn (jurisdiction={})",
            cookie_ec_id,
            consent_context.jurisdiction,
        );
        expire_ec_cookie(settings, response);
        if settings.consent.consent_store.is_some() {
            crate::consent::kv::delete_consent_from_kv(services.kv_store(), cookie_ec_id);
        }
    } else {
        log::debug!(
            "EC skipped: no consent and no existing cookie (jurisdiction={})",
            consent_context.jurisdiction,
        );
    }
}

#[cfg(test)]
mod creative_opportunities_tests {
    use super::*;
    use crate::auction::types::{Bid, MediaType};
    use crate::consent::{ConsentContext, TcfConsent};
    use crate::creative_opportunities::{
        CreativeOpportunitiesConfig, CreativeOpportunityFormat, CreativeOpportunitySlot,
        SlotProviders,
    };
    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::noop_services;
    use crate::price_bucket::PriceGranularity;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::{header, Method, StatusCode};
    use serde_json::json;
    use std::collections::HashMap;

    fn make_creative_slot(id: &str) -> CreativeOpportunitySlot {
        CreativeOpportunitySlot {
            id: id.to_string(),
            gam_unit_path: Some(format!("/21765378893/{id}")),
            div_id: Some(format!("div-{id}")),
            page_patterns: vec!["/news/**".to_string()],
            formats: vec![
                CreativeOpportunityFormat {
                    width: 300,
                    height: 250,
                    media_type: MediaType::Banner,
                },
                CreativeOpportunityFormat {
                    width: 728,
                    height: 90,
                    media_type: MediaType::Banner,
                },
            ],
            floor_price: Some(1.25),
            targeting: HashMap::from([("pos".to_string(), "atf".to_string())]),
            providers: SlotProviders::default(),
        }
    }

    fn make_creative_config() -> CreativeOpportunitiesConfig {
        CreativeOpportunitiesConfig {
            gam_network_id: "21765378893".to_string(),
            auction_timeout_ms: Some(500),
            price_granularity: PriceGranularity::Dense,
        }
    }

    fn make_bid(slot_id: &str, price: Option<f64>, ad_id: Option<&str>) -> Bid {
        Bid {
            slot_id: slot_id.to_string(),
            price,
            currency: "USD".to_string(),
            creative: Some("<div>ad</div>".to_string()),
            adomain: Some(vec!["advertiser.example".to_string()]),
            bidder: "rubicon".to_string(),
            width: 300,
            height: 250,
            nurl: Some("https://bidder.example/win".to_string()),
            burl: Some("https://bidder.example/bill".to_string()),
            ad_id: ad_id.map(str::to_string),
            metadata: HashMap::new(),
        }
    }

    fn make_tcf_with_purpose_1(consented: bool) -> TcfConsent {
        TcfConsent {
            version: 2,
            cmp_id: 1,
            cmp_version: 1,
            consent_screen: 1,
            consent_language: "EN".to_string(),
            vendor_list_version: 42,
            tcf_policy_version: 4,
            created_ds: 0,
            last_updated_ds: 0,
            purpose_consents: vec![consented, true, true],
            purpose_legitimate_interests: vec![false; 3],
            vendor_consents: vec![],
            vendor_legitimate_interests: vec![],
            special_feature_opt_ins: vec![],
        }
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

        for (content_type, expected) in test_cases {
            assert_eq!(
                is_processable_content_type(content_type),
                expected,
                "Content-Type '{content_type}' should_process: expected {expected}",
            );
        }
    }

    #[test]
    fn build_head_globals_script_sets_slots_and_request_id_only() {
        let slot = make_creative_slot("atf_sidebar");
        let slots = vec![&slot];

        let script = build_head_globals_script(&slots, "req-123", &make_creative_config());

        assert!(
            script.contains("window.__ts_ad_slots"),
            "should set ad slots global"
        );
        assert!(
            script.contains("window.__ts_request_id"),
            "should set request ID global"
        );
        assert!(
            script.contains("JSON.parse(\""),
            "should parse escaped JSON from a string literal"
        );
        assert!(
            script.contains("\\\"id\\\":\\\"atf_sidebar\\\""),
            "should include slot ID in escaped JSON"
        );
        assert!(
            script.contains("\\\"gam_unit_path\\\":\\\"/21765378893/atf_sidebar\\\""),
            "should include resolved GAM unit path"
        );
        assert!(
            script.contains("\\\"formats\\\":[[300,250],[728,90]]"),
            "should include slot formats"
        );
        assert!(
            script.contains("\\\"targeting\\\":{\\\"pos\\\":\\\"atf\\\"}"),
            "should include static targeting"
        );
        assert!(
            script.contains("JSON.parse(\"\\\"req-123\\\"\")"),
            "should set request ID through JSON.parse"
        );
        assert!(
            !script.contains("window.__ts_bids"),
            "head globals must not inject bid data"
        );
    }

    #[test]
    fn html_escape_for_script_prevents_raw_script_breakout() {
        let unsafe_json = serde_json::to_string(&json!({
            "slot": "</script><script>alert(\"x\")</script>",
            "angle": "<tag>",
            "ampersand": "a&b",
            "line": "first\u{2028}second\u{2029}third",
        }))
        .expect("should serialize unsafe JSON fixture");

        let escaped = html_escape_for_script(&unsafe_json);

        assert!(
            !escaped.contains("</script>"),
            "escaped JSON string should not contain a raw script end tag"
        );
        assert!(
            !escaped.contains('<') && !escaped.contains('>'),
            "less-than and greater-than should be escaped for HTML script safety"
        );
        assert!(
            !escaped.contains('&'),
            "ampersands should be escaped for HTML script safety"
        );
        assert!(
            !escaped.contains('\u{2028}') && !escaped.contains('\u{2029}'),
            "line separators should be escaped for JavaScript string safety"
        );
        assert!(
            escaped.contains("\\u003C/script\\u003E"),
            "less-than and greater-than should be escaped for HTML script safety"
        );
        assert!(
            escaped.contains("\\u0026"),
            "ampersands should be escaped for HTML safety"
        );
        assert!(
            escaped.contains("\\u2028") && escaped.contains("\\u2029"),
            "line separators should be escaped for JavaScript string safety"
        );
    }

    #[test]
    fn build_bid_map_emits_prebid_targeting_fields() {
        let winning_bids = HashMap::from([(
            "atf_sidebar".to_string(),
            make_bid("atf_sidebar", Some(1.239), Some("ad-123")),
        )]);

        let bid_map = build_bid_map(&winning_bids, PriceGranularity::Dense);

        assert_eq!(
            bid_map.get("atf_sidebar"),
            Some(&json!({
                "hb_pb": "1.23",
                "hb_bidder": "rubicon",
                "hb_adid": "ad-123",
                "burl": "https://bidder.example/bill",
            })),
            "should emit Prebid targeting fields for priced bids"
        );
    }

    #[test]
    fn build_bid_map_uses_empty_ad_id_when_missing() {
        let winning_bids = HashMap::from([(
            "atf_sidebar".to_string(),
            make_bid("atf_sidebar", Some(1.0), None),
        )]);

        let bid_map = build_bid_map(&winning_bids, PriceGranularity::Dense);

        assert_eq!(
            bid_map
                .get("atf_sidebar")
                .and_then(|bid| bid.get("hb_adid")),
            Some(&json!("")),
            "should use empty hb_adid when the provider did not return one"
        );
    }

    #[test]
    fn build_bid_map_omits_bids_without_price() {
        let winning_bids = HashMap::from([
            (
                "priced".to_string(),
                make_bid("priced", Some(2.0), Some("ad-priced")),
            ),
            ("aps".to_string(), make_bid("aps", None, Some("ad-aps"))),
        ]);

        let bid_map = build_bid_map(&winning_bids, PriceGranularity::Dense);

        assert!(
            bid_map.contains_key("priced"),
            "should keep bids with a price"
        );
        assert!(
            !bid_map.contains_key("aps"),
            "should omit bids whose price must be decoded elsewhere"
        );
    }

    #[test]
    fn server_side_auction_allowed_requires_tcf_purpose_1_consent() {
        let no_tcf = ConsentContext::default();
        let denied = ConsentContext {
            tcf: Some(make_tcf_with_purpose_1(false)),
            ..ConsentContext::default()
        };
        let allowed = ConsentContext {
            tcf: Some(make_tcf_with_purpose_1(true)),
            ..ConsentContext::default()
        };

        assert!(
            !server_side_auction_allowed(&no_tcf),
            "absent TCF should deny server-side auction"
        );
        assert!(
            !server_side_auction_allowed(&denied),
            "TCF without Purpose 1 consent should deny server-side auction"
        );
        assert!(
            server_side_auction_allowed(&allowed),
            "TCF with Purpose 1 consent should allow server-side auction"
        );
    }

    #[test]
    fn cache_policy_sets_no_store_when_slots_matched_and_consent_denied() {
        let mut response = Response::from_status(StatusCode::OK);
        response.set_header(header::CACHE_CONTROL, "public, max-age=300");

        apply_server_side_ad_cache_policy(&mut response, true, false);

        assert_eq!(
            response.get_header_str(header::CACHE_CONTROL),
            Some("private, no-store"),
            "matched slots without injected globals should not be browser cached"
        );
    }

    #[test]
    fn cache_policy_sets_no_store_when_globals_are_injected() {
        let mut response = Response::from_status(StatusCode::OK);
        response.set_header(header::CACHE_CONTROL, "public, max-age=300");

        apply_server_side_ad_cache_policy(&mut response, true, true);

        assert_eq!(
            response.get_header_str(header::CACHE_CONTROL),
            Some("private, no-store"),
            "responses with request-scoped globals should not be browser cached"
        );
    }

    #[test]
    fn cache_policy_preserves_surrogate_headers() {
        let mut response = Response::from_status(StatusCode::OK);
        response.set_header(header::CACHE_CONTROL, "public, max-age=300");
        response.set_header("Surrogate-Control", "max-age=3600");
        response.set_header("Fastly-Surrogate-Control", "max-age=7200");

        apply_server_side_ad_cache_policy(&mut response, true, true);

        assert_eq!(
            response.get_header_str("Surrogate-Control"),
            Some("max-age=3600"),
            "should preserve origin Surrogate-Control"
        );
        assert_eq!(
            response.get_header_str("Fastly-Surrogate-Control"),
            Some("max-age=7200"),
            "should preserve origin Fastly-Surrogate-Control"
        );
    }

    #[test]
    fn cache_policy_preserves_origin_cache_headers_when_no_slots_match() {
        let mut response = Response::from_status(StatusCode::OK);
        response.set_header(header::CACHE_CONTROL, "public, max-age=300");
        response.set_header("Surrogate-Control", "max-age=3600");
        response.set_header("Fastly-Surrogate-Control", "max-age=7200");

        apply_server_side_ad_cache_policy(&mut response, false, false);

        assert_eq!(
            response.get_header_str(header::CACHE_CONTROL),
            Some("public, max-age=300"),
            "no-match responses should preserve browser cache headers"
        );
        assert_eq!(
            response.get_header_str("Surrogate-Control"),
            Some("max-age=3600"),
            "no-match responses should preserve Surrogate-Control"
        );
        assert_eq!(
            response.get_header_str("Fastly-Surrogate-Control"),
            Some("max-age=7200"),
            "no-match responses should preserve Fastly-Surrogate-Control"
        );
    }

    #[test]
    fn supported_content_encoding_accepts_known_values() {
        assert!(is_supported_content_encoding(""), "should accept empty");
        assert!(
            is_supported_content_encoding("identity"),
            "should accept identity"
        );
        assert!(is_supported_content_encoding("gzip"), "should accept gzip");
        assert!(
            is_supported_content_encoding("deflate"),
            "should accept deflate"
        );
        assert!(is_supported_content_encoding("br"), "should accept br");
    }

    #[test]
    fn supported_content_encoding_rejects_unknown_values() {
        assert!(!is_supported_content_encoding("zstd"), "should reject zstd");
        assert!(
            !is_supported_content_encoding("compress"),
            "should reject compress"
        );
        assert!(
            !is_supported_content_encoding("snappy"),
            "should reject snappy"
        );
    }

    #[test]
    fn unsupported_encoding_response_is_returned_unmodified() {
        // Processable (HTML) 2xx with unsupported encoding must route to
        // BufferedUnmodified — feeding zstd-compressed bytes to the rewriter
        // as identity would produce garbled output.
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "zstd",
                "example.com",
                false,
            ),
            ResponseRoute::BufferedUnmodified,
        );
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

    // Gate tests — exercise `classify_response_route` directly, the same
    // function `handle_publisher_request` calls. If the gate formula changes,
    // both production and tests are affected identically: no silent drift.

    #[test]
    fn route_streams_2xx_html_without_post_processors() {
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                false,
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_buffers_html_with_post_processors_for_processing() {
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                true,
            ),
            ResponseRoute::BufferedProcessed,
        );
    }

    #[test]
    fn route_streams_non_html_even_with_post_processors_registered() {
        // Post-processors only apply to HTML; JSON/JS can still stream.
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "application/json",
                "gzip",
                "example.com",
                true,
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_buffers_unmodified_on_unsupported_encoding() {
        // Unsupported encoding cannot be streamed (would be fed to rewriter
        // as identity and produce garbled output).
        assert_eq!(
            classify_response_route(StatusCode::OK, "text/html", "zstd", "example.com", false,),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_passes_through_non_processable_2xx() {
        // Binary content (images, fonts) on 2xx streams the origin body direct.
        assert_eq!(
            classify_response_route(StatusCode::OK, "image/png", "", "example.com", false,),
            ResponseRoute::PassThrough,
        );
    }

    #[test]
    fn route_buffers_non_processable_error_responses() {
        // Non-2xx never pass through — response needs to reach the client
        // as-is (with any error body the origin produced).
        assert_eq!(
            classify_response_route(StatusCode::NOT_FOUND, "image/png", "", "example.com", false,),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_excludes_204_from_pass_through() {
        // 204 No Content (RFC 9110 §15.3.5) prohibits a message body.
        assert_eq!(
            classify_response_route(
                StatusCode::NO_CONTENT,
                "image/png",
                "",
                "example.com",
                false,
            ),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_excludes_205_from_pass_through() {
        // 205 Reset Content (RFC 9110 §15.3.6) prohibits a message body.
        assert_eq!(
            classify_response_route(
                StatusCode::RESET_CONTENT,
                "image/png",
                "",
                "example.com",
                false,
            ),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_excludes_204_for_processable_content_types() {
        // 204 must stay body-less even when Content-Type would otherwise route
        // to Stream or BufferedProcessed.
        assert_eq!(
            classify_response_route(
                StatusCode::NO_CONTENT,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                false,
            ),
            ResponseRoute::BufferedUnmodified,
            "204 + HTML must not route to Stream",
        );
        assert_eq!(
            classify_response_route(
                StatusCode::NO_CONTENT,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                true,
            ),
            ResponseRoute::BufferedUnmodified,
            "204 + HTML + post-processors must not route to BufferedProcessed",
        );
    }

    #[test]
    fn route_excludes_205_for_processable_content_types() {
        assert_eq!(
            classify_response_route(
                StatusCode::RESET_CONTENT,
                "application/json",
                "",
                "example.com",
                false,
            ),
            ResponseRoute::BufferedUnmodified,
            "205 + JSON must not route to Stream",
        );
    }

    #[test]
    fn route_streams_non_2xx_processable_content() {
        // Branded 404 or 500 HTML with origin URLs must still be rewritten.
        // This matches the pre-streaming behavior on main.
        assert_eq!(
            classify_response_route(
                StatusCode::NOT_FOUND,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                false,
            ),
            ResponseRoute::Stream,
        );
        assert_eq!(
            classify_response_route(
                StatusCode::INTERNAL_SERVER_ERROR,
                "application/json",
                "gzip",
                "example.com",
                false,
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_processes_non_2xx_html_with_post_processors() {
        // Non-2xx HTML with post-processors still needs full-document processing
        // for head injection, same as 2xx.
        assert_eq!(
            classify_response_route(
                StatusCode::NOT_FOUND,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                true,
            ),
            ResponseRoute::BufferedProcessed,
        );
    }

    #[test]
    fn route_passes_through_non_processable_even_with_empty_request_host() {
        // Empty request_host blocks URL rewriting but pass-through does no
        // rewriting, so a non-processable 2xx still streams through.
        assert_eq!(
            classify_response_route(StatusCode::OK, "image/png", "", "", false,),
            ResponseRoute::PassThrough,
        );
    }

    #[test]
    fn route_buffers_processable_content_with_empty_request_host() {
        // Misconfiguration case — URL rewriting needs a host, so the
        // processable response falls back to unmodified pass-through.
        assert_eq!(
            classify_response_route(StatusCode::OK, "text/html", "gzip", "", false,),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn pass_through_preserves_body_and_content_length() {
        // Simulate the PassThrough path: take body, reattach, send.
        // Verify byte-for-byte identity and Content-Length preservation.
        let image_bytes: Vec<u8> = (0..=255).cycle().take(4096).collect();

        let mut response = Response::from_status(StatusCode::OK);
        response.set_header("content-type", "image/png");
        response.set_header("content-length", image_bytes.len().to_string());
        response.set_body(Body::from(image_bytes.clone()));

        // Simulate PassThrough: take body then reattach
        let body = response.take_body();
        // Body is unmodified — Content-Length stays correct
        assert_eq!(
            response
                .get_header_str("content-length")
                .expect("should have content-length"),
            "4096",
            "Content-Length should be preserved for pass-through"
        );

        // Reattach and verify body content
        response.set_body(body);
        let output = response.into_body().into_bytes();
        assert_eq!(
            output, image_bytes,
            "pass-through should preserve body byte-for-byte"
        );
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
    fn publisher_proxy_does_not_add_accept_encoding_when_absent() {
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        // No Accept-Encoding header set by the client.

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.get_header_str(header::ACCEPT_ENCODING),
            None,
            "publisher proxy should not inject Accept-Encoding when the client sent none"
        );
    }

    #[test]
    fn publisher_proxy_limits_accept_encoding_to_supported_values() {
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header(header::ACCEPT_ENCODING, "gzip, deflate, br, zstd");

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.get_header_str(header::ACCEPT_ENCODING),
            Some("gzip, deflate, br"),
            "publisher fallback should only advertise encodings the rewrite pipeline supports"
        );
    }

    #[test]
    fn publisher_proxy_preserves_identity_only_accept_encoding() {
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header(header::ACCEPT_ENCODING, "identity");

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.get_header_str(header::ACCEPT_ENCODING),
            Some("identity"),
            "publisher fallback should preserve identity-only clients"
        );
    }

    #[test]
    fn publisher_proxy_respects_supported_client_subset() {
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header(header::ACCEPT_ENCODING, "br, gzip;q=0, zstd");

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.get_header_str(header::ACCEPT_ENCODING),
            Some("br"),
            "publisher fallback should only advertise the supported encodings the client accepts"
        );
    }

    #[test]
    fn publisher_proxy_falls_back_to_identity_for_unsupported_client_encodings() {
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header(header::ACCEPT_ENCODING, "zstd");

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.get_header_str(header::ACCEPT_ENCODING),
            Some("identity"),
            "publisher fallback should request identity when the client only accepts unsupported encodings"
        );
    }

    #[test]
    fn revocation_targets_cookie_ec_id_not_header() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("x-ts-ec", "header_id");
        req.set_header("cookie", "ts-ec=cookie_id; other=value");

        let cookie_jar = handle_request_cookies(&req).expect("should parse cookies");
        let existing_ec_cookie = cookie_jar
            .as_ref()
            .and_then(|jar| jar.get(COOKIE_TS_EC))
            .map(|cookie| cookie.value().to_owned());

        let resolved_ec_id =
            get_or_generate_ec_id(&settings, &noop_services(), &req).expect("should resolve EC ID");

        assert_eq!(
            existing_ec_cookie.as_deref(),
            Some("cookie_id"),
            "should read revocation target from cookie value"
        );
        assert_eq!(
            resolved_ec_id, "header_id",
            "should still resolve request EC ID from header precedence"
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

    #[test]
    fn stream_publisher_body_preserves_gzip_round_trip() {
        use flate2::write::GzEncoder;
        use std::io::Write;

        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");

        // Compress CSS containing an origin URL that should be rewritten.
        // CSS uses the text URL replacer (not lol_html), so inline URLs are rewritten.
        let html = b"body { background: url('https://origin.example.com/page'); }";
        let mut compressed = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut compressed, flate2::Compression::default());
            encoder.write_all(html).expect("should compress");
            encoder.finish().expect("should finish compression");
        }

        let body = Body::from(compressed);
        let params = OwnedProcessResponseParams {
            content_encoding: "gzip".to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/css".to_string(),
        };

        let mut output = Vec::new();
        stream_publisher_body(body, &mut output, &params, &settings, &registry)
            .expect("should process gzip CSS");

        // Decompress output
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&output[..]);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .expect("should decompress output");

        assert!(
            decompressed.contains("proxy.example.com"),
            "should rewrite origin to proxy. Got: {decompressed}"
        );
        assert!(
            !decompressed.contains("origin.example.com"),
            "should not contain original host. Got: {decompressed}"
        );
    }

    /// Empty origin body on the streaming route must produce no output
    /// without erroring. Exercises the `Ok(0)` branch of `process_chunks`
    /// plus the processor's `is_last=true, chunk=[]` terminal call.
    #[test]
    fn stream_publisher_body_handles_empty_body() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");

        let params = OwnedProcessResponseParams {
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
        };

        let mut output = Vec::new();
        stream_publisher_body(Body::new(), &mut output, &params, &settings, &registry)
            .expect("should succeed on empty body");

        assert!(
            output.is_empty(),
            "empty origin body should produce empty streaming output. Got: {output:?}"
        );
    }

    /// Mid-stream decoder failure must surface as an error. The adapter
    /// relies on this: once headers are committed, it logs and drops the
    /// `StreamingBody` so the client sees a truncated response. If a decode
    /// failure silently emitted bytes, the client would see a malformed
    /// document instead.
    #[test]
    fn stream_publisher_body_surfaces_mid_stream_decode_error() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");

        // Claim gzip encoding but feed non-gzip bytes. The GzDecoder will
        // error as soon as it tries to read the gzip header.
        let params = OwnedProcessResponseParams {
            content_encoding: "gzip".to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html".to_string(),
        };

        let bogus_body = Body::from(b"<html>not gzip</html>".to_vec());
        let mut output = Vec::new();
        let result = stream_publisher_body(bogus_body, &mut output, &params, &settings, &registry);

        assert!(
            result.is_err(),
            "decoding bogus gzip as gzip should return Err so the adapter can drop the stream"
        );
    }

    /// Pass-through dispatch contract: the adapter treats `PublisherResponse::PassThrough`
    /// by reattaching the origin body unchanged and letting Fastly emit it.
    /// Simulate that step and assert byte identity plus Content-Length
    /// preservation. Distinct from `pass_through_preserves_body_and_content_length`
    /// which only tests the header preservation; this one walks the full
    /// take-then-reattach pattern the adapter uses.
    #[test]
    fn publisher_response_pass_through_reattach_preserves_bytes() {
        // Simulate a 2xx image/png response: Body::from(bytes), take_body(),
        // then set_body(body). `classify_response_route` already picks
        // PassThrough for this combination; this covers the adapter's
        // reattachment half of the contract.
        let image_bytes: Vec<u8> = (0..=127).cycle().take(2048).collect();

        let mut response = Response::from_status(StatusCode::OK);
        response.set_header(header::CONTENT_TYPE, "image/png");
        response.set_header(header::CONTENT_LENGTH, image_bytes.len().to_string());
        response.set_body(Body::from(image_bytes.clone()));

        // Mirror adapter: take body, then reattach.
        let body = response.take_body();
        response.set_body(body);

        assert_eq!(
            response
                .get_header_str(header::CONTENT_LENGTH)
                .expect("content-length should survive"),
            "2048"
        );
        let round_trip = response.into_body().into_bytes();
        assert_eq!(
            round_trip, image_bytes,
            "pass-through reattach must preserve bytes exactly"
        );
    }

    /// Buffered-processed dispatch contract: HTML with a registered post-processor
    /// routes through `BufferedProcessed`, and the handler path sets
    /// `Content-Length` from the processed body length. Verify that invariant
    /// via the classifier + `process_response_streaming` composition.
    #[test]
    fn buffered_processed_sets_content_length_from_processed_body() {
        // Configure nextjs so a post-processor is registered.
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &serde_json::json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");

        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");

        assert!(
            registry.has_html_post_processors(),
            "nextjs integration must register an HTML post-processor"
        );
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "",
                "proxy.example.com",
                registry.has_html_post_processors(),
            ),
            ResponseRoute::BufferedProcessed,
            "HTML with post-processors must route to BufferedProcessed"
        );

        // Feed a small HTML body through the same pipeline the
        // BufferedProcessed arm uses (Vec<u8> output).
        let html =
            b"<html><body><a href=\"https://origin.example.com/page\">link</a></body></html>";
        let body = Body::from(html.to_vec());

        let params = OwnedProcessResponseParams {
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
        };
        let mut output = Vec::new();
        stream_publisher_body(body, &mut output, &params, &settings, &registry)
            .expect("should process buffered HTML");

        assert!(
            !output.is_empty(),
            "buffered processed output must not be empty"
        );
        let as_str = std::str::from_utf8(&output).expect("output should be valid UTF-8");
        assert!(
            as_str.contains("proxy.example.com"),
            "origin must be rewritten. Got: {as_str}"
        );
        assert!(
            !as_str.contains("origin.example.com"),
            "origin host must not leak. Got: {as_str}"
        );
    }

    /// Document-state survives from the streaming pass into the post-processor.
    /// `NextJsRscPlaceholderRewriter` writes into `IntegrationDocumentState`
    /// during streaming; `NextJsHtmlPostProcessor` reads it and substitutes.
    /// Regression test: with post-processors registered, placeholders must
    /// be inserted during streaming and substituted out of the final output.
    #[test]
    fn document_state_placeholders_substitute_through_accumulating_path() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &serde_json::json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");

        // Small, single-fragment RSC script — placeholder path (not fallback).
        let html = br#"<html><body><script>self.__next_f.push([1,"1:{\"link\":\"https://origin.example.com/page\"}"])</script></body></html>"#;
        let params = OwnedProcessResponseParams {
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html".to_string(),
        };

        let mut output = Vec::new();
        stream_publisher_body(
            Body::from(html.to_vec()),
            &mut output,
            &params,
            &settings,
            &registry,
        )
        .expect("should process RSC push");

        let processed = String::from_utf8(output).expect("valid UTF-8");
        assert!(
            !processed.contains("__ts_rsc_payload_"),
            "placeholder must be substituted before reaching output. Got: {processed}"
        );
        assert!(
            processed.contains("proxy.example.com/page"),
            "origin URL must be rewritten in the substituted payload. Got: {processed}"
        );
        assert!(
            !processed.contains("origin.example.com"),
            "origin host must not leak. Got: {processed}"
        );
    }
}
