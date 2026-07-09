//! Publisher response handler.
//!
//! Publisher fallback has three delivery modes that must remain explicit at
//! the API boundary:
//! - pass-through for non-processable `2xx` content
//! - streamed processing for stream-safe processable responses
//! - buffered responses for unsupported encodings or `204/205`
//!
//! Unsupported `Content-Encoding` values must bypass rewriting entirely. The
//! streaming processor treats unknown encodings as identity, so publisher code
//! must gate them out before the body enters the rewrite pipeline.
//!
//! **Note on platform coupling:** The handler boundaries use portable HTTP
//! types: [`handle_publisher_request`] and [`stream_publisher_body`] take and
//! return `http::Request`/`http::Response` over `EdgeBody`, and platform I/O is
//! reached through `RuntimeServices` rather than `fastly::*` directly. The
//! streaming processor itself is generic: `process_response_streaming` writes
//! into any [`Write`] (a `Vec<u8>` for buffered routes, a streaming writer for
//! the streaming route). It is not a content-rewriting concern.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{header, HeaderValue, Method, Request, Response, StatusCode, Uri};

use crate::auction::endpoints::{
    merge_auction_eids, resolve_auction_eids, resolve_client_auction_eids,
};
use crate::auction::orchestrator::{
    AuctionOrchestrator, DispatchAuctionOutcome, DispatchedAuction,
};
use crate::auction::telemetry::{
    build_auction_events, emit_auction_events_best_effort_lazy, AuctionObservationContext,
    AuctionSource, AuctionTerminalOutcome,
};
use crate::auction::types::{
    AuctionContext, AuctionRequest, Bid, DeviceInfo, PublisherInfo, SiteInfo, UserInfo,
};
use crate::cache_policy::{
    cache_control_headers_are_private_or_no_store, CachePolicy, EdgeCacheHeader,
};
use crate::consent::{consent_allows_server_side_auction, gate_eids_by_consent};
use crate::constants::{COOKIE_TS_EIDS, HEADER_X_COMPRESS_HINT};
use crate::cookies::handle_request_cookies;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::registry::PartnerRegistry;
use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::http_util::{is_navigation_request, serve_static_with_etag, RequestInfo};
use crate::integrations::IntegrationRegistry;
use crate::platform::{GeoInfo, PlatformBackendSpec, PlatformHttpRequest, RuntimeServices};
use crate::price_bucket::{price_bucket, PriceGranularity};
use crate::publisher_late_binding::{
    BidPlaceholder, HtmlInjectionTracker, PlaceholderLateBinder, PlaceholderScan,
    SSAT_HELD_TAIL_CAP_BYTES,
};
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::Settings;
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};
use crate::streaming_replacer::create_url_replacer;

const SUPPORTED_ENCODING_VALUES: [&str; 3] = ["gzip", "deflate", "br"];
const DEFAULT_PUBLISHER_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(15);

/// Read buffer size for streaming body processing and brotli internal buffers.
/// Both the `Decompressor` and `CompressorWriter` use this value so all
/// brotli I/O layers operate on consistently-sized chunks.
const STREAM_CHUNK_SIZE: usize = 8192;

fn body_as_reader(body: EdgeBody) -> std::io::Cursor<bytes::Bytes> {
    std::io::Cursor::new(body.into_bytes().unwrap_or_default())
}

fn not_found_response() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Not Found"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

fn restrict_accept_encoding(req: &mut Request<EdgeBody>) {
    // If the client sent no Accept-Encoding, leave the request unchanged so the
    // origin responds without compression. Adding encodings here would cause the
    // origin to compress its response even though the client never asked for it,
    // and the client would then receive content it cannot decode.
    let Some(current) = req
        .headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
    else {
        return;
    };
    req.headers_mut().insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_str(&select_supported_accept_encoding(&current))
            .expect("supported accept-encoding should be a valid header value"),
    );
}

fn ensure_vary_accept_encoding(headers: &mut http::HeaderMap) {
    let already_varies = headers.get_all(header::VARY).iter().any(|value| {
        value.to_str().ok().is_some_and(|value| {
            value.split(',').any(|token| {
                let token = token.trim();
                token == "*" || token.eq_ignore_ascii_case("accept-encoding")
            })
        })
    });
    if !already_varies {
        headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
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
    req: &Request<EdgeBody>,
    integration_registry: &IntegrationRegistry,
    edge_header: EdgeCacheHeader,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    const PREFIX: &str = "/static/tsjs=";
    const UNIFIED_FILENAMES: &[&str] = &["tsjs-unified.js", "tsjs-unified.min.js"];

    let path = req.uri().path();
    if !path.starts_with(PREFIX) {
        return Ok(not_found_response());
    }
    let filename = &path[PREFIX.len()..];

    if UNIFIED_FILENAMES.contains(&filename) {
        // Serve core + immediate modules (excludes deferred like prebid)
        let module_ids = integration_registry.js_module_ids_immediate();
        let body = trusted_server_js::concatenate_modules(&module_ids);
        let hash = trusted_server_js::concatenated_hash(&module_ids);
        return Ok(serve_tsjs_static(req, &body, &hash, edge_header));
    }

    if let Some(module_id) = parse_deferred_module_filename(filename) {
        // Only serve if the deferred module is actually enabled
        let deferred_ids = integration_registry.js_module_ids_deferred();
        if !deferred_ids.contains(&module_id) {
            return Ok(not_found_response());
        }
        if let (Some(content), Some(hash)) = (
            trusted_server_js::module_bundle(module_id),
            trusted_server_js::single_module_hash(module_id),
        ) {
            return Ok(serve_tsjs_static(req, content, hash, edge_header));
        }
    }

    Ok(not_found_response())
}

fn serve_tsjs_static(
    req: &Request<EdgeBody>,
    body: &str,
    expected_hash: &str,
    edge_header: EdgeCacheHeader,
) -> Response<EdgeBody> {
    let mut resp = serve_static_with_etag(
        body,
        req,
        "application/javascript; charset=utf-8",
        edge_header,
    );
    if request_version_hash(req).is_some_and(|hash| hash == expected_hash) {
        CachePolicy::public_immutable(Duration::from_secs(31_536_000))
            .apply_to_headers(resp.headers_mut(), edge_header);
    }
    resp.headers_mut()
        .insert(HEADER_X_COMPRESS_HINT, HeaderValue::from_static("on"));
    resp
}

fn request_version_hash(req: &Request<EdgeBody>) -> Option<&str> {
    req.uri().query()?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "v").then_some(value)
    })
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

/// Parameters for processing response streaming.
struct ProcessResponseParams<'a> {
    input_compression: Compression,
    output_compression: Compression,
    origin_host: &'a str,
    origin_url: &'a str,
    request_host: &'a str,
    request_scheme: &'a str,
    settings: &'a Settings,
    content_type: &'a str,
    integration_registry: &'a IntegrationRegistry,
    ad_slots_script: Option<&'a str>,
    ad_bids_state: &'a Arc<Mutex<Option<String>>>,
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
    body: EdgeBody,
    output: &mut W,
    params: &ProcessResponseParams,
) -> Result<(), Report<TrustedServerError>> {
    let is_html = is_html_content_type(params.content_type);
    let is_rsc_flight =
        content_type_contains_ascii_case_insensitive(params.content_type, "text/x-component");
    log::debug!(
        "process_response_streaming: content_type={}, input_compression={:?}, output_compression={:?}, is_html={}, is_rsc_flight={}",
        params.content_type,
        params.input_compression,
        params.output_compression,
        is_html,
        is_rsc_flight
    );

    let config = PipelineConfig {
        input_compression: params.input_compression,
        output_compression: params.output_compression,
        chunk_size: 8192,
    };

    if is_html {
        let processor = create_html_stream_processor(
            params.origin_host,
            params.request_host,
            params.request_scheme,
            params.settings,
            params.integration_registry,
            params.ad_slots_script.map(str::to_string),
            params.ad_bids_state.clone(),
        )?;
        StreamingPipeline::new(config, processor).process(body_as_reader(body), output)?;
    } else if is_rsc_flight {
        // RSC Flight responses are length-prefixed (T rows). A naive string replacement will
        // corrupt the stream by changing byte lengths without updating the prefixes.
        let processor = RscFlightUrlRewriter::new(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, processor).process(body_as_reader(body), output)?;
    } else {
        let replacer = create_url_replacer(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, replacer).process(body_as_reader(body), output)?;
    }

    Ok(())
}

/// Create a unified HTML stream processor.
///
/// Builds the config via [`HtmlProcessorConfig::from_settings`] and then
/// layers the auction-hold streaming fields on top via
/// [`HtmlProcessorConfig::with_ad_state`], so the canonical builder stays the
/// single source of truth: a future field added to `from_settings` is
/// inherited here automatically.
fn create_html_stream_processor(
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    ad_slots_script: Option<String>,
    ad_bids_state: Arc<Mutex<Option<String>>>,
) -> Result<impl StreamProcessor, Report<TrustedServerError>> {
    create_html_stream_processor_with_options(HtmlStreamProcessorOptions {
        origin_host,
        request_host,
        request_scheme,
        settings,
        integration_registry,
        ad_slots_script,
        ad_bids_state,
        late_binding: None,
    })
}

struct HtmlLateBindingOptions<'a> {
    placeholder: &'a BidPlaceholder,
    tracker: Arc<HtmlInjectionTracker>,
}

struct HtmlStreamProcessorOptions<'a> {
    origin_host: &'a str,
    request_host: &'a str,
    request_scheme: &'a str,
    settings: &'a Settings,
    integration_registry: &'a IntegrationRegistry,
    ad_slots_script: Option<String>,
    ad_bids_state: Arc<Mutex<Option<String>>>,
    late_binding: Option<HtmlLateBindingOptions<'a>>,
}

fn create_html_stream_processor_with_options(
    options: HtmlStreamProcessorOptions<'_>,
) -> Result<impl StreamProcessor, Report<TrustedServerError>> {
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};

    let mut config = HtmlProcessorConfig::from_settings(
        options.settings,
        options.integration_registry,
        options.origin_host,
        options.request_host,
        options.request_scheme,
    )
    .with_ad_state(options.ad_slots_script, options.ad_bids_state);

    if let Some(late_binding) = options.late_binding {
        config = config
            .with_bid_placeholder(
                late_binding.placeholder.as_html().to_owned(),
                late_binding.tracker,
            )
            .without_post_processing();
    }

    Ok(create_html_processor(config))
}

/// Result of publisher request handling, indicating whether the response body
/// should be streamed or has already been buffered.
pub enum PublisherResponse {
    /// Response is fully buffered and ready to send via `send_to_client()`.
    Buffered(Response<EdgeBody>),
    /// Response headers are ready for a streaming response. Covers processable
    /// content on any status (2xx or non-2xx — e.g., branded 404/500 HTML and
    /// error JSON still get URL rewriting) where the encoding is supported.
    /// Post-processors run inside the streaming processor, so processable HTML
    /// is streamed regardless of whether any are registered. The caller must:
    /// 1. Call `finalize_response()` on the response
    /// 2. Call `response.stream_to_client()` to get a `StreamingBody`
    /// 3. Call `stream_publisher_body()` with the body and streaming writer
    /// 4. Call `StreamingBody::finish()`
    ///
    /// **Interim (PR 15):** `body` has already been fully materialised into
    /// WASM heap by the platform HTTP client.  `stream_publisher_body` reads
    /// from an in-memory buffer, not a live origin stream.  The origin-side
    /// peak is bounded by `MAX_PLATFORM_RESPONSE_BODY_BYTES`.
    Stream {
        /// Response with all headers set (EC ID, cookies, etc.)
        /// but body not yet written. `Content-Length` already removed.
        response: Response<EdgeBody>,
        /// Origin body to be piped through the streaming pipeline.
        body: EdgeBody,
        /// Parameters for [`process_response_streaming`].
        params: Box<OwnedProcessResponseParams>,
    },
    /// Non-processable 2xx response (images, fonts, video). The adapter must
    /// reattach the body via setting the body before returning.
    /// `finalize_response()` and `send_to_client()` are applied at the outer
    /// response-dispatch level, not in this arm.
    ///
    /// `Content-Length` is preserved — the body is unmodified.
    ///
    /// **Interim (PR 15):** `body` has been fully materialised into WASM heap.
    /// Previously, binary assets streamed lazily from origin with no WASM
    /// buffering.  This path is now bounded by `MAX_PLATFORM_RESPONSE_BODY_BYTES`;
    /// assets exceeding that limit return an error instead of exhausting heap.
    PassThrough {
        /// Response with all headers set but body not yet written.
        response: Response<EdgeBody>,
        /// Origin body to stream directly to the client.
        body: EdgeBody,
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
    /// `2xx` non-processable content (images, fonts, video), not `204/205`.
    PassThrough,
    /// Processable content with supported encoding.
    Stream,
    /// Response returned unmodified via [`PublisherResponse::Buffered`].
    BufferedUnmodified,
}

/// Decide how a proxied response should be routed.
///
/// Pure: no header mutation, no body consumed. All inputs are extracted from
/// the origin response at the call site.
pub(crate) fn classify_response_route(
    status: StatusCode,
    content_type: &str,
    content_encoding: &str,
    request_host: &str,
) -> ResponseRoute {
    if status == StatusCode::NO_CONTENT || status == StatusCode::RESET_CONTENT {
        return ResponseRoute::BufferedUnmodified;
    }

    let should_process = is_processable_content_type(content_type);

    if !should_process {
        if status.is_success() {
            return ResponseRoute::PassThrough;
        }
        return ResponseRoute::BufferedUnmodified;
    }

    if request_host.is_empty() {
        return ResponseRoute::BufferedUnmodified;
    }

    if !is_supported_content_encoding(content_encoding) {
        return ResponseRoute::BufferedUnmodified;
    }

    ResponseRoute::Stream
}

fn response_cache_control_is_private_or_no_store(response: &Response<EdgeBody>) -> bool {
    cache_control_headers_are_private_or_no_store(response.headers())
}

fn apply_publisher_asset_cache_policy(
    settings: &Settings,
    path: &str,
    cache_rule_method: bool,
    edge_header: EdgeCacheHeader,
    response: &mut Response<EdgeBody>,
) -> Result<(), Report<TrustedServerError>> {
    if !cache_rule_method || response_cache_control_is_private_or_no_store(response) {
        return Ok(());
    }

    let status = response.status();
    if !(status.is_success() || status == StatusCode::NOT_MODIFIED) {
        return Ok(());
    }

    if let Some(policy) = settings.asset_cache_policy_for_path(path)? {
        policy.apply_to_headers(response.headers_mut(), edge_header);
    }

    Ok(())
}

/// Owned version of [`ProcessResponseParams`] for returning from
/// [`handle_publisher_request`] without lifetime issues.
pub struct OwnedProcessResponseParams {
    pub(crate) input_compression: Compression,
    pub(crate) output_compression: Compression,
    pub(crate) origin_host: String,
    pub(crate) origin_url: String,
    pub(crate) request_host: String,
    pub(crate) request_scheme: String,
    pub(crate) content_type: String,
    pub(crate) ad_slots_script: Option<String>,
    pub(crate) ad_bids_state: Arc<Mutex<Option<String>>>,
    /// Observation context for the in-flight auction.
    pub(crate) auction_observation: Option<AuctionObservationContext>,
    /// Auction request snapshot used for telemetry after collection.
    pub(crate) auction_request: Option<AuctionRequest>,
    /// In-flight SSP bids dispatched before `pending_origin.wait()`.
    /// The streaming phase collects these and writes bids to `ad_bids_state`
    /// before processing the last body chunk, so `</body>` injection sees live bids.
    pub(crate) dispatched_auction: Option<DispatchedAuction>,
    /// Price granularity used to bucket bids when building `tsjs.bids`.
    pub(crate) price_granularity: PriceGranularity,
}

/// Buffers a [`PublisherResponse`] into a single [`Response`], collecting the
/// dispatched server-side auction before buffering.
///
/// Handles all three variants: returns [`PublisherResponse::Buffered`] unchanged,
/// pipes [`PublisherResponse::Stream`] through the streaming pipeline into
/// memory, and reattaches [`PublisherResponse::PassThrough`] bodies directly.
///
/// The buffered size is capped by `settings.publisher.max_buffered_body_bytes`
/// (16 MiB by default), so processable origin responses cannot grow the buffer
/// without bound and exhaust the Wasm heap.
///
/// `method` preserves metadata for bodiless responses: `HEAD` and bodiless
/// statuses (204, 304) carry no body but may advertise the `GET` representation's
/// length, so they skip the buffer and length rewrite.
///
/// Every adapter (Axum, Cloudflare, Spin, and the Fastly `EdgeZero` path) calls
/// this: it drives
/// [`stream_publisher_body_async`], which awaits
/// [`AuctionOrchestrator::collect_dispatched_auction`], writes the winning bids
/// into `ad_bids_state`, and injects them before `</body>`.
///
/// # Errors
///
/// Returns an error if the streaming pipeline fails to process the response
/// body, or if the processed body exceeds the configured buffer cap.
pub async fn buffer_publisher_response_async(
    publisher_response: PublisherResponse,
    method: &Method,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
) -> Result<Response<EdgeBody>, Report<crate::error::TrustedServerError>> {
    match publisher_response {
        PublisherResponse::Buffered(response) => Ok(response),
        PublisherResponse::Stream {
            mut response,
            body,
            mut params,
        } => {
            if !response_carries_body(method, response.status()) {
                if params.dispatched_auction.is_some() {
                    // A bodiless response (HEAD navigation, 204/304) has no
                    // `</body>` to inject bids into, so the dispatched SSP
                    // requests are wasted — surface it for quota observability,
                    // matching the pass-through / buffered-unmodified arms.
                    log::warn!(
                        "Server-side auction dispatched but response is bodiless (method: {}, status: {}); in-flight SSP bid requests will not be collected",
                        method,
                        response.status(),
                    );
                }
                return Ok(response);
            }
            let mut output = BoundedWriter::new(settings.publisher.max_buffered_body_bytes);
            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                settings,
                integration_registry,
                orchestrator,
                services,
            )
            .await?;
            let bytes = output.into_inner();
            response.headers_mut().insert(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from(bytes.len() as u64),
            );
            *response.body_mut() = EdgeBody::from(bytes);
            Ok(response)
        }
        PublisherResponse::PassThrough { mut response, body } => {
            *response.body_mut() = body;
            Ok(response)
        }
    }
}

/// Returns `true` when a buffered publisher response should carry a body and a
/// recomputed `Content-Length`.
///
/// `HEAD` responses and bodiless statuses (204, 304) carry no body; rewriting
/// their `Content-Length` to the (empty) buffered length would mislead clients
/// and caches, so the origin metadata is preserved instead.
fn response_carries_body(method: &Method, status: StatusCode) -> bool {
    *method != Method::HEAD
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::NOT_MODIFIED
}

/// A [`Write`] sink that buffers into a `Vec<u8>` but fails once the configured
/// byte limit would be exceeded.
///
/// Used to bound in-WASM-heap buffering of decoded/re-written publisher bodies.
/// A highly-compressible origin response can sit under the platform raw-body cap
/// yet expand past a safe heap size after decode and post-processing; this writer
/// turns that into a recoverable error instead of an out-of-memory abort.
pub struct BoundedWriter {
    inner: Vec<u8>,
    limit: usize,
}

impl BoundedWriter {
    /// Creates a writer that accepts at most `limit` bytes before erroring.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Vec::new(),
            limit,
        }
    }

    /// Consumes the writer and returns the buffered bytes.
    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.inner
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.inner.len() + buf.len() > self.limit {
            return Err(std::io::Error::other(
                "publisher body exceeded maximum buffered size",
            ));
        }
        self.inner.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Stream the publisher response body through the processing pipeline.
///
/// Called by the adapter after `stream_to_client()` has committed the response
/// headers. Runs synchronously against an already-materialised body; the async
/// I/O happened upstream in [`handle_publisher_request`]. Writes processed
/// chunks directly to `output`.
///
/// # Errors
///
/// Returns an error if processing fails mid-stream. Since headers are already
/// committed, the caller should log the error and drop the `StreamingBody`
/// (client sees a truncated response).
pub fn stream_publisher_body<W: Write>(
    body: EdgeBody,
    output: &mut W,
    params: &OwnedProcessResponseParams,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
) -> Result<(), Report<TrustedServerError>> {
    let borrowed = ProcessResponseParams {
        input_compression: params.input_compression,
        output_compression: params.output_compression,
        origin_host: &params.origin_host,
        origin_url: &params.origin_url,
        request_host: &params.request_host,
        request_scheme: &params.request_scheme,
        settings,
        content_type: &params.content_type,
        integration_registry,
        ad_slots_script: params.ad_slots_script.as_deref(),
        ad_bids_state: &params.ad_bids_state,
    };
    process_response_streaming(body, output, &borrowed)
}

/// Stream publisher body with parser-safe SSAT bid late binding.
///
/// HTML SSAT responses insert a per-request placeholder from a `lol_html`
/// parser-confirmed body end-tag handler. This function scans processed,
/// uncompressed HTML output for that placeholder, collects any dispatched
/// auction at that point, replaces the placeholder with the bids script, and
/// resumes streaming. Raw origin bytes are never scanned for `</body>` syntax.
///
/// For non-HTML content types, any dispatched auction is collected before body
/// bytes are written because there is no parser-confirmed body close for bid
/// insertion. If no SSAT slots matched this HTML response, the function falls
/// back to the synchronous pipeline unchanged.
///
/// # Errors
///
/// Returns an error if processing fails mid-stream. Headers may already be
/// committed at that point; streaming callers should log the error and drop the
/// client body.
pub async fn stream_publisher_body_async<W: Write>(
    body: EdgeBody,
    output: &mut W,
    params: &mut OwnedProcessResponseParams,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
) -> Result<(), Report<TrustedServerError>> {
    let dispatched = params.dispatched_auction.take();
    let telemetry = AuctionTelemetryCarry {
        observation: params.auction_observation.take(),
        auction_request: params.auction_request.take(),
    };

    let is_html = is_html_content_type(&params.content_type);
    let needs_ssat_late_binding = is_html && params.ad_slots_script.is_some();

    if !needs_ssat_late_binding {
        if let Some(dispatched) = dispatched {
            // Non-HTML or no-slot responses have no SSAT body-close injection
            // point. Collect before writing bytes so telemetry completes and the
            // existing direct-state pipeline can preserve legacy behavior.
            let placeholder = mediator_placeholder_request();
            let result = orchestrator
                .collect_dispatched_auction(
                    dispatched,
                    services,
                    &make_collect_context(settings, services, &placeholder),
                )
                .await;
            if let (Some(observation), Some(auction_request)) =
                (telemetry.observation, telemetry.auction_request.as_ref())
            {
                emit_auction_events_best_effort_lazy(services, || {
                    build_auction_events(
                        observation,
                        AuctionTerminalOutcome::Completed {
                            request: auction_request,
                            result: &result,
                        },
                    )
                })
                .await;
            }

            write_bids_to_state(
                &result.winning_bids,
                params.price_granularity,
                &params.ad_bids_state,
                settings.debug.inject_adm_for_testing,
            );
        }
        return stream_publisher_body(body, output, params, settings, integration_registry);
    }

    if integration_registry.has_html_post_processors() {
        return buffer_html_late_binding_with_postprocessors(
            body,
            output,
            BufferedLateBindingContext {
                params,
                settings,
                integration_registry,
                orchestrator,
                services,
                dispatched,
                telemetry,
            },
        )
        .await;
    }

    let placeholder = BidPlaceholder::new();
    let tracker = Arc::new(HtmlInjectionTracker::default());
    let mut processor =
        match create_html_stream_processor_with_options(HtmlStreamProcessorOptions {
            origin_host: &params.origin_host,
            request_host: &params.request_host,
            request_scheme: &params.request_scheme,
            settings,
            integration_registry,
            ad_slots_script: params.ad_slots_script.as_deref().map(str::to_string),
            ad_bids_state: params.ad_bids_state.clone(),
            late_binding: Some(HtmlLateBindingOptions {
                placeholder: &placeholder,
                tracker: Arc::clone(&tracker),
            }),
        }) {
            Ok(processor) => processor,
            Err(err) => {
                if let Some(dispatched) = dispatched {
                    emit_abandoned_auction(
                        services,
                        telemetry.observation,
                        dispatched,
                        "processor_init_error",
                    )
                    .await;
                }
                return Err(err);
            }
        };

    stream_html_with_auction_hold(
        body,
        output,
        &mut processor,
        params.input_compression,
        params.output_compression,
        AuctionCollectCtx {
            dispatched,
            telemetry,
            price_granularity: params.price_granularity,
            ad_bids_state: &params.ad_bids_state,
            orchestrator,
            services,
            settings,
        },
        LateBindingStreamConfig {
            placeholder: &placeholder,
            tracker: Arc::clone(&tracker),
            fallback: LateBindingFallbackContext {
                origin_host: &params.origin_host,
                request_host: &params.request_host,
                request_scheme: &params.request_scheme,
                integration_registry,
                ad_slots_script: params.ad_slots_script.as_deref(),
            },
        },
    )
    .await
}

struct BufferedLateBindingContext<'a> {
    params: &'a OwnedProcessResponseParams,
    settings: &'a Settings,
    integration_registry: &'a IntegrationRegistry,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    dispatched: Option<DispatchedAuction>,
    telemetry: AuctionTelemetryCarry,
}

async fn buffer_html_late_binding_with_postprocessors<W: Write>(
    body: EdgeBody,
    output: &mut W,
    mut ctx: BufferedLateBindingContext<'_>,
) -> Result<(), Report<TrustedServerError>> {
    use crate::html_processor::{
        create_html_processor, run_html_post_processors, HtmlProcessorConfig,
    };
    use crate::integrations::IntegrationDocumentState;

    let decoded = match decode_body_to_vec(
        body,
        ctx.params.input_compression,
        ctx.settings.publisher.max_buffered_body_bytes,
    ) {
        Ok(decoded) => decoded,
        Err(err) => {
            if let Some(dispatched) = ctx.dispatched.take() {
                emit_abandoned_auction(
                    ctx.services,
                    ctx.telemetry.observation.take(),
                    dispatched,
                    "buffered_decode_error",
                )
                .await;
            }
            return Err(err);
        }
    };

    let placeholder = BidPlaceholder::new();
    let tracker = Arc::new(HtmlInjectionTracker::default());
    let document_state = IntegrationDocumentState::default();
    let config = HtmlProcessorConfig::from_settings(
        ctx.settings,
        ctx.integration_registry,
        &ctx.params.origin_host,
        &ctx.params.request_host,
        &ctx.params.request_scheme,
    )
    .with_ad_state(
        ctx.params.ad_slots_script.clone(),
        Arc::clone(&ctx.params.ad_bids_state),
    )
    .with_bid_placeholder(placeholder.as_html().to_owned(), Arc::clone(&tracker))
    .without_post_processing()
    .with_document_state(document_state.clone());
    let mut processor = create_html_processor(config);
    let held_tail_cap =
        SSAT_HELD_TAIL_CAP_BYTES.min(ctx.settings.publisher.max_buffered_body_bytes);
    let mut binder = PlaceholderLateBinder::new(&placeholder, held_tail_cap);
    let mut late_bound = Vec::new();
    let fallback = LateBindingFallbackContext {
        origin_host: &ctx.params.origin_host,
        request_host: &ctx.params.request_host,
        request_scheme: &ctx.params.request_scheme,
        integration_registry: ctx.integration_registry,
        ad_slots_script: ctx.params.ad_slots_script.as_deref(),
    };
    let mut state = LateBindingState {
        dispatched: ctx.dispatched,
        telemetry: ctx.telemetry,
        price_granularity: ctx.params.price_granularity,
        ad_bids_state: &ctx.params.ad_bids_state,
        orchestrator: ctx.orchestrator,
        services: ctx.services,
        settings: ctx.settings,
        tracker: Arc::clone(&tracker),
        fallback,
        decoded_input_bytes: decoded.len(),
        processed_output_bytes: 0,
    };

    for chunk in decoded.chunks(STREAM_CHUNK_SIZE) {
        let processed =
            processor
                .process_chunk(chunk, false)
                .change_context(TrustedServerError::Proxy {
                    message: "Failed to process buffered publisher HTML".to_string(),
                });
        let processed = match processed {
            Ok(processed) => processed,
            Err(err) => {
                abandon_pending_late_binding_auction(&mut state, "buffered_process_error").await;
                return Err(err);
            }
        };
        write_late_bound_processed_output(&mut late_bound, &mut binder, &processed, &mut state)
            .await?;
    }

    let final_out = processor
        .process_chunk(&[], true)
        .change_context(TrustedServerError::Proxy {
            message: "Failed to process buffered publisher HTML".to_string(),
        });
    let final_out = match final_out {
        Ok(final_out) => final_out,
        Err(err) => {
            abandon_pending_late_binding_auction(&mut state, "buffered_process_error").await;
            return Err(err);
        }
    };
    write_late_bound_processed_output(&mut late_bound, &mut binder, &final_out, &mut state).await?;
    let found_placeholder = binder.replaced();
    let remainder = binder.finish();
    write_checked_processed_output_or_abandon(
        &mut late_bound,
        &remainder,
        &mut state,
        "processed_output_error",
    )
    .await?;
    if !found_placeholder {
        if state.tracker.bid_placeholder_inserted() {
            log::warn!(
                "SSAT bid placeholder was inserted but not observed in buffered output; using EOF fallback"
            );
        }
        resolve_late_bound_bids(&mut state).await;
        let tail = build_eof_fallback_tail(
            state.tracker.head_injected(),
            &state.fallback,
            state.ad_bids_state,
        );
        write_checked_processed_output_or_abandon(
            &mut late_bound,
            tail.as_bytes(),
            &mut state,
            "processed_output_error",
        )
        .await?;
    }

    let post_processors = ctx.integration_registry.html_post_processors();
    let post_processed = run_html_post_processors(
        late_bound,
        &post_processors,
        &ctx.params.origin_host,
        &ctx.params.request_host,
        &ctx.params.request_scheme,
        &document_state,
        ctx.settings.publisher.max_buffered_body_bytes,
    )
    .change_context(TrustedServerError::Proxy {
        message: "Failed to post-process buffered publisher HTML".to_string(),
    })?;

    encode_processed_html_to_writer(&post_processed, ctx.params.output_compression, output)
}

fn decode_body_to_vec(
    body: EdgeBody,
    compression: Compression,
    limit: usize,
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    use brotli::Decompressor;
    use flate2::read::{GzDecoder, ZlibDecoder};

    let body = body_as_reader(body);
    match compression {
        Compression::None => read_to_vec_bounded(body, limit),
        Compression::Gzip => read_to_vec_bounded(GzDecoder::new(body), limit),
        Compression::Deflate => read_to_vec_bounded(ZlibDecoder::new(body), limit),
        Compression::Brotli => {
            read_to_vec_bounded(Decompressor::new(body, STREAM_CHUNK_SIZE), limit)
        }
    }
}

fn read_to_vec_bounded<R: Read>(
    mut reader: R,
    limit: usize,
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    let mut output = Vec::new();
    let mut buffer = vec![0u8; STREAM_CHUNK_SIZE];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(output),
            Ok(n) => {
                if output.len() + n > limit {
                    return Err(Report::new(TrustedServerError::Proxy {
                        message: "publisher decoded input exceeded maximum buffered size"
                            .to_string(),
                    }));
                }
                output.extend_from_slice(&buffer[..n]);
            }
            Err(err) => {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!("Failed to decode publisher body: {err}"),
                }));
            }
        }
    }
}

fn encode_processed_html_to_writer<W: Write>(
    bytes: &[u8],
    compression: Compression,
    output: &mut W,
) -> Result<(), Report<TrustedServerError>> {
    use brotli::enc::writer::CompressorWriter;
    use brotli::enc::BrotliEncoderParams;
    use flate2::write::{GzEncoder, ZlibEncoder};

    match compression {
        Compression::None => output
            .write_all(bytes)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to write buffered publisher HTML".to_string(),
            }),
        Compression::Gzip => {
            let mut encoder = GzEncoder::new(output, flate2::Compression::default());
            encoder
                .write_all(bytes)
                .change_context(TrustedServerError::Proxy {
                    message: "Failed to write gzip publisher HTML".to_string(),
                })?;
            encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize gzip encoder".to_string(),
            })?;
            Ok(())
        }
        Compression::Deflate => {
            let mut encoder = ZlibEncoder::new(output, flate2::Compression::default());
            encoder
                .write_all(bytes)
                .change_context(TrustedServerError::Proxy {
                    message: "Failed to write deflate publisher HTML".to_string(),
                })?;
            encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize deflate encoder".to_string(),
            })?;
            Ok(())
        }
        Compression::Brotli => {
            let params = BrotliEncoderParams {
                quality: 4,
                lgwin: 22,
                ..Default::default()
            };
            let mut encoder = CompressorWriter::with_params(output, STREAM_CHUNK_SIZE, &params);
            encoder
                .write_all(bytes)
                .change_context(TrustedServerError::Proxy {
                    message: "Failed to write brotli publisher HTML".to_string(),
                })?;
            encoder.flush().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize brotli encoder".to_string(),
            })?;
            let _ = encoder.into_inner();
            Ok(())
        }
    }
}

/// Builds the canonical mediator placeholder [`Request`] passed to the collect
/// phase via [`make_collect_context`].
///
/// The URI is the compile-time constant
/// [`MEDIATOR_PLACEHOLDER_URL`](crate::auction::types::MEDIATOR_PLACEHOLDER_URL),
/// so the builder is infallible; a default-URI fallback would trip
/// [`make_collect_context`]'s `debug_assert_eq!`.
fn mediator_placeholder_request() -> Request<EdgeBody> {
    Request::builder()
        .uri(crate::auction::types::MEDIATOR_PLACEHOLDER_URL)
        .body(EdgeBody::empty())
        .expect("MEDIATOR_PLACEHOLDER_URL should be a valid URI")
}

/// Build a minimal [`AuctionContext`] for the collect phase.
///
/// See [`AuctionContext::request`]: the orchestrator's collect path runs
/// after `send_async` has already consumed the real client request, so this
/// context carries a synthetic placeholder. The orchestrator itself
/// instantiates a fresh placeholder when it actually invokes a mediator —
/// this argument is plumbing for the (presently unused) case where the
/// orchestrator needs the caller's request shape.
fn make_collect_context<'a>(
    settings: &'a Settings,
    services: &'a RuntimeServices,
    placeholder: &'a Request<EdgeBody>,
) -> AuctionContext<'a> {
    debug_assert_eq!(
        placeholder.uri().to_string(),
        crate::auction::types::MEDIATOR_PLACEHOLDER_URL,
        "make_collect_context must be given the canonical placeholder; \
         callers must not forward a real client request through the collect path"
    );
    AuctionContext {
        settings,
        request: placeholder,
        timeout_ms: 0,
        provider_responses: None,
        services,
    }
}

/// Well-known crawler User-Agent fragments. Best-effort: an attacker can
/// trivially spoof their UA, so this is for opt-out signalling to honest
/// crawlers (preventing SSP auctions burning partner quota on their behalf),
/// not security.
pub(crate) const BOT_USER_AGENT_FRAGMENTS: &[&str] =
    &["Googlebot", "Bingbot", "AhrefsBot", "SemrushBot", "DotBot"];

/// Returns true when the request's User-Agent matches any well-known crawler
/// fragment in [`BOT_USER_AGENT_FRAGMENTS`].
pub(crate) fn is_bot_user_agent(req: &Request<EdgeBody>) -> bool {
    let ua = req
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    BOT_USER_AGENT_FRAGMENTS
        .iter()
        .any(|frag| ua.contains(frag))
}

/// Returns true when the request advertises itself as a prefetch via either
/// the standard `Sec-Purpose` or the legacy `Purpose` header.
pub(crate) fn is_prefetch_request(req: &Request<EdgeBody>) -> bool {
    let header = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("prefetch"))
    };
    header("sec-purpose") || header("purpose")
}

/// Returns true only when the publisher request should run the full
/// server-side ad stack: auction dispatch plus initial ad-slot injection.
///
/// `auction_enabled` is the global `[auction].enabled` kill switch — when
/// false, no automatic server-side auction or ad-slot injection runs.
pub(crate) fn should_run_server_side_ad_stack(
    is_get: bool,
    is_navigation: bool,
    is_prefetch: bool,
    is_bot: bool,
    has_matched_slots: bool,
    consent_allows_auction: bool,
    auction_enabled: bool,
) -> bool {
    is_get
        && is_navigation
        && !is_prefetch
        && !is_bot
        && has_matched_slots
        && consent_allows_auction
        && auction_enabled
}

/// Write winning bids from an auction result into the shared `ad_bids_state` lock.
pub(crate) fn write_bids_to_state(
    winning_bids: &std::collections::HashMap<String, Bid>,
    price_granularity: PriceGranularity,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
    inject_adm: bool,
) {
    log::debug!(
        "write_bids_to_state: {} winning bid(s): [{}]",
        winning_bids.len(),
        winning_bids.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    let bid_map = build_bid_map(winning_bids, price_granularity, inject_adm);
    let bids_script = build_bids_script(&bid_map);
    *ad_bids_state.lock().expect("should lock bid state") = Some(bids_script);
}

/// Prepend an HTML comment summarising the auction result onto the shared
/// `ad_bids_state` so it lands directly before the injected bids `<script>`.
///
/// `path_label` differentiates the streaming-with-auction-hold path (`stream`)
/// from the buffered path (`buffered`) in the resulting `<!-- ts-debug: -->`
/// marker so on-page debugging can tell which code path produced the bids.
pub(crate) fn prepend_auction_debug_comment(
    path_label: &str,
    result: &crate::auction::orchestrator::OrchestrationResult,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
) {
    let ssp_count = result.provider_responses.len();
    let mediator_info = match &result.mediator_response {
        Some(r) => format!("ok({}_bids)", r.bids.len()),
        None => "none".to_string(),
    };
    let debug_comment = format!(
        "<!-- ts-debug: path={path_label} ssp={ssp_count} mediator={mediator_info} winning={} time={}ms -->",
        result.winning_bids.len(),
        result.total_time_ms,
    );
    let mut state = ad_bids_state
        .lock()
        .expect("should lock bid state for debug");
    match &mut *state {
        Some(script) => {
            *script = format!("{debug_comment}\n{script}");
        }
        None => {
            // invariant: write_bids_to_state is always called before this and
            // always sets Some(_); this branch is unreachable in production.
            *state = Some(debug_comment);
        }
    }
}

/// Telemetry context carried from dispatch to collect.
struct AuctionTelemetryCarry {
    observation: Option<AuctionObservationContext>,
    auction_request: Option<AuctionRequest>,
}

impl AuctionTelemetryCarry {
    fn take(&mut self) -> Self {
        Self {
            observation: self.observation.take(),
            auction_request: self.auction_request.take(),
        }
    }
}

/// Bundles the auction-collection dependencies passed through the streaming helpers.
struct AuctionCollectCtx<'a> {
    dispatched: Option<DispatchedAuction>,
    telemetry: AuctionTelemetryCarry,
    price_granularity: PriceGranularity,
    ad_bids_state: &'a Arc<Mutex<Option<String>>>,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    settings: &'a Settings,
}

#[derive(Clone, Copy)]
struct LateBindingFallbackContext<'a> {
    origin_host: &'a str,
    request_host: &'a str,
    request_scheme: &'a str,
    integration_registry: &'a IntegrationRegistry,
    ad_slots_script: Option<&'a str>,
}

struct LateBindingStreamConfig<'a> {
    placeholder: &'a BidPlaceholder,
    tracker: Arc<HtmlInjectionTracker>,
    fallback: LateBindingFallbackContext<'a>,
}

/// Run the parser-safe placeholder late-binding loop for HTML bodies.
async fn stream_html_with_auction_hold<W: Write, P: StreamProcessor>(
    body: EdgeBody,
    output: &mut W,
    processor: &mut P,
    input_compression: Compression,
    output_compression: Compression,
    ctx: AuctionCollectCtx<'_>,
    late_binding: LateBindingStreamConfig<'_>,
) -> Result<(), Report<TrustedServerError>> {
    use brotli::enc::writer::CompressorWriter;
    use brotli::enc::BrotliEncoderParams;
    use brotli::Decompressor;
    use flate2::read::{GzDecoder, ZlibDecoder};
    use flate2::write::{GzEncoder, ZlibEncoder};

    let placeholder = late_binding.placeholder;
    let tracker = late_binding.tracker;
    let fallback_ctx = late_binding.fallback;
    let body = body_as_reader(body);
    match (input_compression, output_compression) {
        (Compression::None, Compression::None) => {
            body_close_hold_loop(
                body,
                output,
                processor,
                placeholder,
                Arc::clone(&tracker),
                ctx,
                fallback_ctx,
            )
            .await
        }
        (Compression::Gzip, Compression::Gzip) => {
            let decoder = GzDecoder::new(body);
            let mut encoder = GzEncoder::new(&mut *output, flate2::Compression::default());
            body_close_hold_loop(
                decoder,
                &mut encoder,
                processor,
                placeholder,
                Arc::clone(&tracker),
                ctx,
                fallback_ctx,
            )
            .await?;
            encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize gzip encoder".to_string(),
            })?;
            Ok(())
        }
        (Compression::Deflate, Compression::Deflate) => {
            let decoder = ZlibDecoder::new(body);
            let mut encoder = ZlibEncoder::new(&mut *output, flate2::Compression::default());
            body_close_hold_loop(
                decoder,
                &mut encoder,
                processor,
                placeholder,
                Arc::clone(&tracker),
                ctx,
                fallback_ctx,
            )
            .await?;
            encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize deflate encoder".to_string(),
            })?;
            Ok(())
        }
        (Compression::Brotli, Compression::Brotli) => {
            let decoder = Decompressor::new(body, STREAM_CHUNK_SIZE);
            let params = BrotliEncoderParams {
                quality: 4,
                lgwin: 22,
                ..Default::default()
            };
            let mut encoder =
                CompressorWriter::with_params(&mut *output, STREAM_CHUNK_SIZE, &params);
            body_close_hold_loop(
                decoder,
                &mut encoder,
                processor,
                placeholder,
                Arc::clone(&tracker),
                ctx,
                fallback_ctx,
            )
            .await?;
            let _ = encoder.into_inner();
            Ok(())
        }
        (Compression::Gzip, Compression::None) => {
            body_close_hold_loop(
                GzDecoder::new(body),
                output,
                processor,
                placeholder,
                Arc::clone(&tracker),
                ctx,
                fallback_ctx,
            )
            .await
        }
        (Compression::Deflate, Compression::None) => {
            body_close_hold_loop(
                ZlibDecoder::new(body),
                output,
                processor,
                placeholder,
                Arc::clone(&tracker),
                ctx,
                fallback_ctx,
            )
            .await
        }
        (Compression::Brotli, Compression::None) => {
            body_close_hold_loop(
                Decompressor::new(body, STREAM_CHUNK_SIZE),
                output,
                processor,
                placeholder,
                Arc::clone(&tracker),
                ctx,
                fallback_ctx,
            )
            .await
        }
        _ => Err(Report::new(TrustedServerError::Proxy {
            message: "Unsupported compression transformation".to_string(),
        })),
    }
}

struct LateBindingState<'a> {
    dispatched: Option<DispatchedAuction>,
    telemetry: AuctionTelemetryCarry,
    price_granularity: PriceGranularity,
    ad_bids_state: &'a Arc<Mutex<Option<String>>>,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    settings: &'a Settings,
    tracker: Arc<HtmlInjectionTracker>,
    fallback: LateBindingFallbackContext<'a>,
    decoded_input_bytes: usize,
    processed_output_bytes: usize,
}

/// Core parser-safe placeholder late-binding loop.
async fn body_close_hold_loop<R: Read, W: Write, P: StreamProcessor>(
    mut reader: R,
    writer: &mut W,
    processor: &mut P,
    placeholder: &BidPlaceholder,
    tracker: Arc<HtmlInjectionTracker>,
    ctx: AuctionCollectCtx<'_>,
    fallback_ctx: LateBindingFallbackContext<'_>,
) -> Result<(), Report<TrustedServerError>> {
    let AuctionCollectCtx {
        dispatched,
        telemetry,
        price_granularity,
        ad_bids_state,
        orchestrator,
        services,
        settings,
    } = ctx;
    let held_tail_cap = SSAT_HELD_TAIL_CAP_BYTES.min(settings.publisher.max_buffered_body_bytes);
    let mut binder = PlaceholderLateBinder::new(placeholder, held_tail_cap);
    let mut state = LateBindingState {
        dispatched,
        telemetry,
        price_granularity,
        ad_bids_state,
        orchestrator,
        services,
        settings,
        tracker,
        fallback: fallback_ctx,
        decoded_input_bytes: 0,
        processed_output_bytes: 0,
    };
    let mut buffer = vec![0u8; STREAM_CHUNK_SIZE];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let final_out =
                    processor
                        .process_chunk(&[], true)
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to finalize processor".to_string(),
                        });
                let final_out = match final_out {
                    Ok(final_out) => final_out,
                    Err(err) => {
                        abandon_pending_late_binding_auction(&mut state, "stream_finalize_error")
                            .await;
                        return Err(err);
                    }
                };
                write_late_bound_processed_output(writer, &mut binder, &final_out, &mut state)
                    .await?;

                let found_placeholder = binder.replaced();
                let remainder = binder.finish();
                write_checked_processed_output_or_abandon(
                    writer,
                    &remainder,
                    &mut state,
                    "processed_output_error",
                )
                .await?;

                if !found_placeholder {
                    if state.tracker.bid_placeholder_inserted() {
                        log::warn!(
                            "SSAT bid placeholder was inserted but not observed in processed output; using EOF fallback"
                        );
                    }
                    resolve_late_bound_bids(&mut state).await;
                    let tail = build_eof_fallback_tail(
                        state.tracker.head_injected(),
                        &state.fallback,
                        state.ad_bids_state,
                    );
                    write_checked_processed_output_or_abandon(
                        writer,
                        tail.as_bytes(),
                        &mut state,
                        "processed_output_error",
                    )
                    .await?;
                }
                break;
            }
            Ok(n) => {
                state.decoded_input_bytes = state.decoded_input_bytes.saturating_add(n);
                if state.decoded_input_bytes > state.settings.publisher.max_buffered_body_bytes {
                    if let Some(dispatched) = state.dispatched.take() {
                        emit_abandoned_auction(
                            state.services,
                            state.telemetry.observation.take(),
                            dispatched,
                            "decoded_input_cap_exceeded",
                        )
                        .await;
                    }
                    return Err(Report::new(TrustedServerError::Proxy {
                        message: "publisher decoded input exceeded maximum streaming size"
                            .to_string(),
                    }));
                }

                let processed = processor.process_chunk(&buffer[..n], false).change_context(
                    TrustedServerError::Proxy {
                        message: "Failed to process chunk".to_string(),
                    },
                );
                let processed = match processed {
                    Ok(processed) => processed,
                    Err(err) => {
                        if let Some(dispatched) = state.dispatched.take() {
                            emit_abandoned_auction(
                                state.services,
                                state.telemetry.observation.take(),
                                dispatched,
                                "stream_process_error",
                            )
                            .await;
                        }
                        return Err(err);
                    }
                };
                write_late_bound_processed_output(writer, &mut binder, &processed, &mut state)
                    .await?;
            }
            Err(e) => {
                if let Some(dispatched) = state.dispatched.take() {
                    emit_abandoned_auction(
                        state.services,
                        state.telemetry.observation.take(),
                        dispatched,
                        "stream_read_error",
                    )
                    .await;
                }
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!("Failed to read origin body: {e}"),
                }));
            }
        }
    }

    writer.flush().change_context(TrustedServerError::Proxy {
        message: "Failed to flush output".to_string(),
    })?;
    Ok(())
}

async fn write_late_bound_processed_output<W: Write>(
    writer: &mut W,
    binder: &mut PlaceholderLateBinder,
    processed: &[u8],
    state: &mut LateBindingState<'_>,
) -> Result<(), Report<TrustedServerError>> {
    let scan = match binder.push(processed) {
        Ok(scan) => scan,
        Err(err) => {
            abandon_pending_late_binding_auction(state, "placeholder_scan_error").await;
            return Err(err);
        }
    };

    match scan {
        PlaceholderScan::Emit(bytes) => {
            write_checked_processed_output_or_abandon(
                writer,
                &bytes,
                state,
                "processed_output_error",
            )
            .await
        }
        PlaceholderScan::Found { before, after } => {
            write_checked_processed_output_or_abandon(
                writer,
                &before,
                state,
                "processed_output_error",
            )
            .await?;
            resolve_late_bound_bids(state).await;
            let replacement = build_body_close_replacement_tail(state);
            write_checked_processed_output(writer, replacement.as_bytes(), state)?;
            write_checked_processed_output(writer, &after, state)
        }
    }
}

async fn resolve_late_bound_bids(state: &mut LateBindingState<'_>) {
    let Some(dispatched) = state.dispatched.take() else {
        return;
    };
    collect_stream_auction(
        dispatched,
        state.telemetry.take(),
        state.price_granularity,
        state.ad_bids_state,
        state.orchestrator,
        state.services,
        state.settings,
    )
    .await;
}

async fn abandon_pending_late_binding_auction(
    state: &mut LateBindingState<'_>,
    reason: &'static str,
) {
    let Some(dispatched) = state.dispatched.take() else {
        return;
    };
    emit_abandoned_auction(
        state.services,
        state.telemetry.observation.take(),
        dispatched,
        reason,
    )
    .await;
}

async fn write_checked_processed_output_or_abandon<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    state: &mut LateBindingState<'_>,
    reason: &'static str,
) -> Result<(), Report<TrustedServerError>> {
    match write_checked_processed_output(writer, bytes, state) {
        Ok(()) => Ok(()),
        Err(err) => {
            abandon_pending_late_binding_auction(state, reason).await;
            Err(err)
        }
    }
}

fn current_bids_script(ad_bids_state: &Arc<Mutex<Option<String>>>) -> String {
    ad_bids_state
        .lock()
        .expect("should lock bid state")
        .clone()
        .unwrap_or_else(build_empty_bids_script)
}

fn build_body_close_replacement_tail(state: &LateBindingState<'_>) -> String {
    if state.tracker.head_injected() {
        return current_bids_script(state.ad_bids_state);
    }

    log::info!("SSAT bid injection used missing-head body-close fallback");
    state.tracker.mark_head_injected();
    build_bootstrap_and_bids_tail(&state.fallback, state.ad_bids_state)
}

fn build_eof_fallback_tail(
    head_bootstrap_observed: bool,
    fallback_ctx: &LateBindingFallbackContext<'_>,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
) -> String {
    if head_bootstrap_observed {
        log::info!("SSAT bid injection used EOF fallback after head bootstrap");
        return current_bids_script(ad_bids_state);
    }

    log::info!("SSAT bid injection used missing-head EOF fallback");
    build_bootstrap_and_bids_tail(fallback_ctx, ad_bids_state)
}

fn build_bootstrap_and_bids_tail(
    fallback_ctx: &LateBindingFallbackContext<'_>,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
) -> String {
    let document_state = crate::integrations::IntegrationDocumentState::default();
    let mut tail = crate::html_processor::build_head_bootstrap_snippet(
        fallback_ctx.integration_registry,
        fallback_ctx.origin_host,
        fallback_ctx.request_host,
        fallback_ctx.request_scheme,
        &document_state,
        fallback_ctx.ad_slots_script,
    );
    tail.push_str(&current_bids_script(ad_bids_state));
    tail
}

fn write_checked_processed_output<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    state: &mut LateBindingState<'_>,
) -> Result<(), Report<TrustedServerError>> {
    if bytes.is_empty() {
        return Ok(());
    }

    state.processed_output_bytes = state.processed_output_bytes.saturating_add(bytes.len());
    if state.processed_output_bytes > state.settings.publisher.max_buffered_body_bytes {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "publisher processed output exceeded maximum streaming size".to_string(),
        }));
    }

    writer
        .write_all(bytes)
        .change_context(TrustedServerError::Proxy {
            message: "Failed to write processed publisher output".to_string(),
        })?;
    Ok(())
}

async fn emit_abandoned_auction(
    services: &RuntimeServices,
    observation: Option<AuctionObservationContext>,
    dispatched: DispatchedAuction,
    reason: &'static str,
) {
    let Some(observation) = observation else {
        return;
    };
    let (request, provider_responses, abandoned_providers, elapsed_ms) = dispatched.abandon();
    emit_auction_events_best_effort_lazy(services, || {
        build_auction_events(
            observation,
            AuctionTerminalOutcome::Abandoned {
                request: &request,
                provider_responses: &provider_responses,
                abandoned_providers: &abandoned_providers,
                reason,
                elapsed_ms,
            },
        )
    })
    .await;
}

async fn collect_stream_auction(
    dispatched: DispatchedAuction,
    telemetry: AuctionTelemetryCarry,
    price_granularity: PriceGranularity,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
    settings: &Settings,
) {
    log::info!("body_close_hold_loop: collecting dispatched auction before held body tail");
    let placeholder = mediator_placeholder_request();
    let collect_ctx = make_collect_context(settings, services, &placeholder);
    let result = orchestrator
        .collect_dispatched_auction(dispatched, services, &collect_ctx)
        .await;
    if let (Some(observation), Some(auction_request)) =
        (telemetry.observation, telemetry.auction_request.as_ref())
    {
        emit_auction_events_best_effort_lazy(services, || {
            build_auction_events(
                observation,
                AuctionTerminalOutcome::Completed {
                    request: auction_request,
                    result: &result,
                },
            )
        })
        .await;
    }
    log::info!(
        "body_close_hold_loop: collect complete - {} winning bid(s)",
        result.winning_bids.len()
    );
    write_bids_to_state(
        &result.winning_bids,
        price_granularity,
        ad_bids_state,
        settings.debug.inject_adm_for_testing,
    );

    if settings.debug.auction_html_comment {
        prepend_auction_debug_comment("stream", &result, ad_bids_state);
    }
}

/// Delivery-compression behavior explicitly supported by an adapter.
///
/// This capability is independent of [`EdgeCacheHeader`]. Adapters must declare
/// it directly so cache-header selection cannot accidentally enable Fastly-only
/// response delivery behavior.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DeliveryCompressionCapability {
    /// Fastly dynamic compression honors `X-Compress-Hint: on` at delivery.
    FastlyDynamic,
    /// The adapter has no compatible delivery-compression facility.
    Unavailable,
}

/// Adapter-specific publisher response behavior.
///
/// Bundles cache-header selection with an explicit delivery-compression
/// capability while keeping those independent platform concerns visible.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PublisherAdapterOptions {
    /// Shared-cache header family emitted by publisher cache policies.
    pub edge_cache_header: EdgeCacheHeader,
    /// Delivery-compression facility available on the current adapter.
    pub delivery_compression: DeliveryCompressionCapability,
}

fn should_offload_ssat_compression(
    settings: &Settings,
    adapter_options: PublisherAdapterOptions,
    should_run_ad_stack: bool,
) -> bool {
    settings.publisher.ssat_compression_offload_enabled
        && adapter_options.delivery_compression == DeliveryCompressionCapability::FastlyDynamic
        && should_run_ad_stack
}

/// Auction dispatch context passed to [`handle_publisher_request`].
pub struct AuctionDispatch<'a> {
    /// Orchestrator that dispatches and collects SSP bid requests.
    pub orchestrator: &'a crate::auction::orchestrator::AuctionOrchestrator,
    /// Creative opportunity slot definitions matched against the request path.
    pub slots: &'a [crate::creative_opportunities::CreativeOpportunitySlot],
    /// Partner registry for KV-backed EID resolution. `None` skips KV enrichment.
    pub registry: Option<&'a PartnerRegistry>,
}

/// Proxies requests to the publisher's origin server.
///
/// Returns a [`PublisherResponse`] indicating how the response should be sent:
/// - [`PublisherResponse::PassThrough`] — non-processable `2xx` content
/// - [`PublisherResponse::Stream`] — processable content with supported
///   encodings and no full-document buffering requirement
/// - [`PublisherResponse::Buffered`] — unsupported encodings, non-`2xx`
///   unprocessable content, `204/205`, or HTML that requires full-document
///   post-processing
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if the proxy request fails or the
/// origin backend is unreachable.
pub async fn handle_publisher_request(
    settings: &Settings,
    services: &RuntimeServices,
    kv: Option<&KvIdentityGraph>,
    ec_context: &mut EcContext,
    auction: AuctionDispatch<'_>,
    mut req: Request<EdgeBody>,
    adapter_options: PublisherAdapterOptions,
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");
    let edge_header = adapter_options.edge_cache_header;

    // Prebid.js requests are not intercepted here anymore. The HTML processor removes
    // publisher-supplied Prebid scripts; the unified TSJS bundle includes Prebid.js when enabled.

    // Extract request host and scheme (uses Host header and TLS detection after edge sanitization)
    let request_info = RequestInfo::from_request(&req, services.client_info());
    let request_host = &request_info.host;
    let request_scheme = &request_info.scheme;

    log::debug!(
        "Request info: host={}, scheme={} (X-Forwarded-Host: {:?}, Host: {:?}, X-Forwarded-Proto: {:?})",
        request_host,
        request_scheme,
        req.headers().get("x-forwarded-host"),
        req.headers().get(header::HOST),
        req.headers().get("x-forwarded-proto"),
    );

    let is_navigation = is_navigation_request(&req);

    // EC generation is the caller's responsibility — it must run only for real
    // browsers on document navigations, and that real-browser decision lives in
    // the adapter (TLS/JA4/device gate). Generating here, with only the
    // navigation signal, would mint an IP-derived EC for clients the adapter
    // classified as non-real browsers and forward it to SSPs/APS even though EC
    // operations were blocked for them. The adapter calls
    // `EcContext::generate_if_needed` (real-browser-gated) before dispatching to
    // this handler; subresource requests are likewise filtered there.
    let ec_allowed = ec_context.ec_allowed();
    log::debug!(
        "Proxy EC state: has_ec_id={}, ec_allowed={ec_allowed}",
        ec_context.ec_value().is_some(),
    );

    let consent_context = ec_context.consent().clone();
    let ec_id = ec_context.ec_value().filter(|_| ec_allowed);
    let cookie_jar = handle_request_cookies(&req)?;
    let geo = ec_context.geo_info().cloned();

    let parsed_origin = url::Url::parse(&settings.publisher.origin_url).change_context(
        TrustedServerError::Proxy {
            message: format!("Invalid origin_url: {}", settings.publisher.origin_url),
        },
    )?;
    let origin_scheme = parsed_origin.scheme().to_string();
    let origin_host_without_port = parsed_origin.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "Missing host in origin_url".to_string(),
        })
    })?;
    let backend_name = services
        .backend()
        .ensure(&PlatformBackendSpec {
            scheme: origin_scheme.clone(),
            host: origin_host_without_port.to_string(),
            port: parsed_origin.port(),
            host_header_override: settings.publisher.origin_host_header_override.clone(),
            certificate_check: settings.proxy.certificate_check,
            first_byte_timeout: DEFAULT_PUBLISHER_FIRST_BYTE_TIMEOUT,
            between_bytes_timeout: DEFAULT_PUBLISHER_FIRST_BYTE_TIMEOUT,
        })
        .change_context(TrustedServerError::Proxy {
            message: "backend registration failed".to_string(),
        })?;
    let origin_host = settings.publisher.origin_host();
    let origin_host_header = settings.publisher.origin_host_header();
    let origin_path_and_query = req
        .uri()
        .path_and_query()
        .map(http::uri::PathAndQuery::as_str)
        .unwrap_or("/");
    let target_uri = format!("{origin_scheme}://{origin_host}{origin_path_and_query}")
        .parse::<Uri>()
        .change_context(TrustedServerError::Proxy {
            message: "invalid publisher origin uri".to_string(),
        })?;

    log::debug!("Proxying request to configured publisher backend");

    let request_path = req.uri().path().to_string();
    let is_get = req.method() == http::Method::GET;
    let cache_rule_method = req.method() == Method::GET || req.method() == Method::HEAD;

    let is_prefetch = is_prefetch_request(&req);
    let is_bot = is_bot_user_agent(&req);

    let matched_slots: Vec<_> = if settings.creative_opportunities.is_some() && is_get {
        crate::creative_opportunities::match_slots(auction.slots, &request_path)
            .into_iter()
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    // Fail closed for GDPR-relevant traffic: GDPR/unknown jurisdictions and
    // requests carrying an EU TCF signal require effective TCF Purpose 1
    // (storage/access) before firing. Known non-GDPR jurisdictions are free.
    let consent_allows_auction = consent_allows_server_side_auction(&consent_context);

    let should_run_ad_stack = should_run_server_side_ad_stack(
        is_get,
        is_navigation,
        is_prefetch,
        is_bot,
        !matched_slots.is_empty(),
        consent_allows_auction,
        auction.orchestrator.is_enabled(),
    );
    let should_run_auction = should_run_ad_stack;
    let compression_offload_active =
        should_offload_ssat_compression(settings, adapter_options, should_run_ad_stack);
    // Diagnostic: shows which gate suppresses the server-side auction. Pair with
    // the `EC context: ... jurisdiction=...` line from EC-context construction
    // when `consent_allows_auction=false`.
    log::debug!(
        "server-side ad-stack gate: is_get={is_get} is_navigation={is_navigation} \
         is_prefetch={is_prefetch} is_bot={is_bot} matched_slots={} \
         consent_allows_auction={consent_allows_auction} orchestrator_enabled={} \
         -> should_run_auction={should_run_auction}",
        matched_slots.len(),
        auction.orchestrator.is_enabled(),
    );

    if matched_slots.is_empty() && settings.creative_opportunities.is_some() {
        log::debug!(
            "No creative opportunity slots matched path '{}' — skipping auction and injection",
            request_path
        );
    }

    let auction_timeout_ms = settings
        .creative_opportunities
        .as_ref()
        .and_then(|co| co.auction_timeout_ms)
        .unwrap_or(settings.auction.timeout_ms);

    let ad_bids_state: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let price_granularity = settings
        .creative_opportunities
        .as_ref()
        .map(|co| co.price_granularity)
        .unwrap_or_default();

    // Dispatch SSP bid requests while req still has the original client headers
    // (User-Agent, x-forwarded-for, cookies, etc.).  The borrow ends when
    // dispatch_auction returns — DispatchedAuction holds no lifetime — so req
    // can be mutated and sent to origin immediately after.
    let mut auction_observation: Option<AuctionObservationContext> = None;
    let mut auction_request_for_telemetry: Option<AuctionRequest> = None;
    let mut dispatched_auction = if matched_slots.is_empty() {
        None
    } else {
        let observation = AuctionObservationContext::from_parts(
            AuctionSource::InitialNavigation,
            request_host,
            &request_path,
            matched_slots.len(),
            ec_context,
        );

        if should_run_auction {
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &matched_slots,
                request_path: &request_path,
            };
            let mut auction_request = build_auction_request(
                &slots_ctx,
                ec_id,
                &consent_context,
                &request_info,
                req.headers()
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok()),
            );
            apply_auction_eids_and_device(
                &mut auction_request,
                &AuctionEidTargeting {
                    cookie_jar: cookie_jar.as_ref(),
                    ec_id,
                    kv,
                    partner_registry: auction.registry,
                    ec_context,
                    services,
                    geo: geo.as_ref(),
                    path_label: "Server-side",
                },
            );
            let auction_context = AuctionContext {
                settings,
                request: &req,
                timeout_ms: auction_timeout_ms,
                provider_responses: None,
                services,
            };
            match auction
                .orchestrator
                .dispatch_auction(&auction_request, &auction_context)
                .await
            {
                DispatchAuctionOutcome::Dispatched(dispatched) => {
                    auction_request_for_telemetry = Some(auction_request);
                    auction_observation = Some(observation);
                    Some(dispatched)
                }
                DispatchAuctionOutcome::DispatchFailed {
                    request,
                    provider_responses,
                    elapsed_ms,
                } => {
                    emit_auction_events_best_effort_lazy(services, || {
                        build_auction_events(
                            observation,
                            AuctionTerminalOutcome::DispatchFailed {
                                request: &request,
                                provider_responses: &provider_responses,
                                reason: "dispatch_failed",
                                elapsed_ms,
                            },
                        )
                    })
                    .await;
                    None
                }
                DispatchAuctionOutcome::NotStarted => {
                    let elapsed_ms = observation.elapsed_ms();
                    emit_auction_events_best_effort_lazy(services, || {
                        build_auction_events(
                            observation,
                            AuctionTerminalOutcome::DispatchFailed {
                                request: &auction_request,
                                provider_responses: &[],
                                reason: "no_provider_dispatched",
                                elapsed_ms,
                            },
                        )
                    })
                    .await;
                    None
                }
            }
        } else {
            let skip_reason = if !auction.orchestrator.is_enabled() {
                "auction_disabled"
            } else if !consent_allows_auction {
                "consent_denied"
            } else if is_bot {
                "bot"
            } else if is_prefetch {
                "prefetch"
            } else {
                "not_ad_stack_eligible"
            };
            let elapsed_ms = observation.elapsed_ms();
            emit_auction_events_best_effort_lazy(services, || {
                build_auction_events(
                    observation,
                    AuctionTerminalOutcome::Skipped {
                        reason: skip_reason,
                        elapsed_ms,
                    },
                )
            })
            .await;
            None
        }
    };
    log::info!(
        "dispatch_auction: {}",
        if dispatched_auction.is_some() {
            "Some — auction running async"
        } else {
            "None — not dispatched or skipped"
        }
    );

    // Only advertise encodings the rewrite pipeline can decode. Compression
    // offload changes the processed HTML representation sent to Fastly, not
    // origin negotiation, so non-HTML responses retain their negotiated
    // encoding.
    restrict_accept_encoding(&mut req);
    // Strip the internal `fastly-ssl` scheme signal before forwarding to the
    // origin. On the EdgeZero path the entry point re-injects this header from
    // trusted Fastly TLS metadata so in-process scheme detection works; the
    // legacy path never sets it. Either way it is an internal edge signal that
    // must not leak to publisher backends.
    req.headers_mut().remove("fastly-ssl");
    *req.uri_mut() = target_uri;
    req.headers_mut().insert(
        header::HOST,
        HeaderValue::from_str(&origin_host_header).change_context(TrustedServerError::Proxy {
            message: "invalid publisher origin host header".to_string(),
        })?,
    );

    // SSP requests are already racing through the platform HTTP client, so
    // origin TTFB tracks origin latency rather than the auction timeout.
    let mut response = match services
        .http_client()
        .send(PlatformHttpRequest::new(req, backend_name))
        .await
    {
        Ok(platform_response) => platform_response.response,
        Err(err) => {
            if let Some(dispatched) = dispatched_auction.take() {
                emit_abandoned_auction(
                    services,
                    auction_observation.take(),
                    dispatched,
                    "origin_proxy_error",
                )
                .await;
            }
            return Err(err.change_context(TrustedServerError::Proxy {
                message: "Failed to proxy request to origin".to_string(),
            }));
        }
    };

    log::debug!(
        "Publisher origin response received: status={}, header_count={}",
        response.status(),
        response.headers().len()
    );

    let ad_slots_script = if should_run_ad_stack {
        settings
            .creative_opportunities
            .as_ref()
            .map(|co_config| build_ad_slots_script(&matched_slots, co_config))
    } else {
        None
    };

    // §4.7: HTML carrying inline per-user bid data must never be shared-cached.
    // `private, max-age=0` is deliberate (not `no-store`): it keeps the page
    // BFCache-eligible while restricting reuse to the same user's browser with
    // revalidation; edge-cache header removal handles shared CDN caches.
    //
    // Gate on `should_run_ad_stack` rather than content-type alone: when no slot
    // matched, the feature is disabled, or this is not an ad-eligible navigation,
    // no per-user `tsjs.adSlots`/`tsjs.bids` are injected, so forcing private
    // here would needlessly strip shared cacheability from ordinary publisher
    // HTML. Applies regardless of the auction *outcome* (empty bids still inject
    // per-user slot state). The separate EC-cookie cache net in the adapter's
    // `finalize_response` keeps first-visit identity responses private.
    let origin_content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if should_run_ad_stack && is_html_content_type(origin_content_type) {
        CachePolicy::private_revalidate().apply_to_headers(response.headers_mut(), edge_header);
    }

    apply_publisher_asset_cache_policy(
        settings,
        &request_path,
        cache_rule_method,
        edge_header,
        &mut response,
    )?;

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_string();

    let status = response.status();
    let content_encoding = response
        .headers()
        .get(header::CONTENT_ENCODING)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_lowercase();
    let route = classify_response_route(status, &content_type, &content_encoding, request_host);

    match route {
        ResponseRoute::PassThrough => {
            log::debug!(
                "Pass-through binary response - Content-Type: '{}', status: {}",
                content_type,
                status,
            );
            if let Some(dispatched) = dispatched_auction.take() {
                // should_run_auction is decided from request signals before the
                // origin content-type is known. A pass-through (2xx non-HTML)
                // response has no `</body>` to inject bids into, so the dispatched
                // SSP requests are wasted — surface it for quota observability.
                log::warn!(
                    "Server-side auction dispatched but response routed to pass-through (Content-Type: '{}', status: {}); in-flight SSP bid requests will not be collected",
                    content_type,
                    status,
                );
                emit_abandoned_auction(
                    services,
                    auction_observation.take(),
                    dispatched,
                    "pass_through_response",
                )
                .await;
            }
            let (parts, body) = response.into_parts();
            let response = Response::from_parts(parts, EdgeBody::empty());
            Ok(PublisherResponse::PassThrough { response, body })
        }
        ResponseRoute::BufferedUnmodified => {
            // Unsupported or unprocessable responses must bypass rewriting
            // entirely rather than entering the pipeline as identity bytes.
            if is_processable_content_type(&content_type) && request_host.is_empty() {
                log::warn!(
                    "Empty request host — returning processable content unmodified (Content-Type: '{}', status: {}). Check proxy Host header.",
                    content_type,
                    status,
                );
            } else if !is_supported_content_encoding(&content_encoding) {
                log::warn!("Unsupported Content-Encoding; returning response unmodified");
            } else {
                log::debug!(
                    "Skipping response processing - Content-Type: '{}', status: {}",
                    content_type,
                    status,
                );
            }
            if let Some(dispatched) = dispatched_auction.take() {
                // Same wasted-dispatch case as the pass-through arm: an
                // unprocessable/non-2xx response can't carry injected bids, so
                // the in-flight SSP requests are left uncollected.
                log::warn!(
                    "Server-side auction dispatched but response routed to buffered-unmodified (Content-Type: '{}', status: {}); in-flight SSP bid requests will not be collected",
                    content_type,
                    status,
                );
                emit_abandoned_auction(
                    services,
                    auction_observation.take(),
                    dispatched,
                    "buffered_unmodified_response",
                )
                .await;
            }
            Ok(PublisherResponse::Buffered(response))
        }
        ResponseRoute::Stream => {
            log::debug!(
                "Streaming response - Content-Type: {}, Content-Encoding: {}",
                content_type,
                content_encoding
            );

            let offload_stream = compression_offload_active && is_html_content_type(&content_type);
            let input_compression = Compression::from_content_encoding(&content_encoding);
            let output_compression = if offload_stream {
                Compression::None
            } else {
                input_compression
            };
            let body = std::mem::replace(response.body_mut(), EdgeBody::empty());
            response.headers_mut().remove(header::CONTENT_LENGTH);
            if offload_stream {
                response.headers_mut().remove(header::CONTENT_ENCODING);
                response
                    .headers_mut()
                    .insert(HEADER_X_COMPRESS_HINT, HeaderValue::from_static("on"));
                ensure_vary_accept_encoding(response.headers_mut());
            }

            Ok(PublisherResponse::Stream {
                response,
                body,
                params: Box::new(OwnedProcessResponseParams {
                    input_compression,
                    output_compression,
                    origin_host,
                    origin_url: settings.publisher.origin_url.clone(),
                    request_host: request_host.to_string(),
                    request_scheme: request_scheme.to_string(),
                    content_type,
                    ad_slots_script: ad_slots_script.clone(),
                    ad_bids_state: ad_bids_state.clone(),
                    auction_observation,
                    auction_request: auction_request_for_telemetry,
                    dispatched_auction,
                    price_granularity,
                }),
            })
        }
    }
}

/// Bundle of the per-request creative-opportunity inputs that travel together.
///
/// Extracted so `build_auction_request` stays under the project's
/// 7-argument cap (`matched_slots` + `request_path` live for the same
/// request scope and are passed together everywhere).
pub(crate) struct MatchedSlotsContext<'a> {
    pub matched_slots: &'a [crate::creative_opportunities::CreativeOpportunitySlot],
    pub request_path: &'a str,
}

/// Borrowed inputs for [`apply_auction_eids_and_device`], bundled to keep the
/// helper within the project's 7-argument cap.
struct AuctionEidTargeting<'a> {
    cookie_jar: Option<&'a CookieJar>,
    ec_id: Option<&'a str>,
    kv: Option<&'a KvIdentityGraph>,
    partner_registry: Option<&'a PartnerRegistry>,
    ec_context: &'a EcContext,
    services: &'a RuntimeServices,
    geo: Option<&'a GeoInfo>,
    /// Prefix for the consent-stripped warning (e.g. `"Server-side"`).
    path_label: &'a str,
}

/// Resolves client + KV EIDs, consent-gates them onto `auction_request`, and
/// attaches the client IP/geo to its device record.
///
/// Shared verbatim by the initial-page and page-bids dispatch paths so the EID
/// resolution and consent gating live in one place; `path_label` only varies
/// the consent-stripped warning message.
fn apply_auction_eids_and_device(
    auction_request: &mut AuctionRequest,
    targeting: &AuctionEidTargeting<'_>,
) {
    let ts_eids_value = targeting
        .cookie_jar
        .and_then(|j| j.get(COOKIE_TS_EIDS))
        .map(|c| c.value().to_owned());
    let client_eids = if targeting.ec_id.is_some() {
        resolve_client_auction_eids(None, ts_eids_value.as_deref())
    } else {
        None
    };
    let kv_eids = resolve_auction_eids(
        targeting.kv,
        targeting.partner_registry,
        targeting.ec_context,
    );
    let merged_eids = merge_auction_eids(client_eids, kv_eids);
    let had_eids = merged_eids.as_ref().is_some_and(|v| !v.is_empty());
    auction_request.user.eids =
        gate_eids_by_consent(merged_eids, auction_request.user.consent.as_ref());
    if had_eids && auction_request.user.eids.is_none() {
        log::warn!(
            "{} auction EIDs stripped by TCF consent gating",
            targeting.path_label
        );
    }
    let client_ip = targeting
        .services
        .client_info()
        .client_ip
        .map(|ip| ip.to_string());
    if client_ip.is_some() || targeting.geo.is_some() {
        let device = auction_request.device.get_or_insert(DeviceInfo {
            user_agent: None,
            ip: None,
            geo: None,
        });
        device.ip = client_ip;
        device.geo = targeting.geo.cloned();
    }
}

/// Build an [`AuctionRequest`] from matched creative opportunity slots.
pub(crate) fn build_auction_request(
    slots_ctx: &MatchedSlotsContext<'_>,
    ec_id: Option<&str>,
    consent_context: &crate::consent::ConsentContext,
    request_info: &crate::http_util::RequestInfo,
    user_agent: Option<&str>,
) -> AuctionRequest {
    let slots = slots_ctx
        .matched_slots
        .iter()
        .map(crate::creative_opportunities::CreativeOpportunitySlot::to_ad_slot)
        .collect();
    let page_url = format!(
        "{}://{}{}",
        request_info.scheme, request_info.host, slots_ctx.request_path
    );
    let ec_id = ec_id.filter(|id| !id.is_empty());
    let request_id = ec_id.map_or_else(
        || format!("ts-req-{}", uuid::Uuid::new_v4().simple()),
        |id| format!("ts-{id}"),
    );
    AuctionRequest {
        id: request_id,
        slots,
        publisher: PublisherInfo {
            domain: request_info.host.clone(),
            page_url: Some(page_url.clone()),
        },
        user: UserInfo {
            id: ec_id.map(str::to_string),
            consent: Some(consent_context.clone()),
            eids: None,
        },
        device: user_agent.filter(|ua| !ua.is_empty()).map(|ua| DeviceInfo {
            user_agent: Some(ua.to_string()),
            ip: None,
            geo: None,
        }),
        site: Some(SiteInfo {
            domain: request_info.host.clone(),
            page: page_url,
        }),
        context: std::collections::HashMap::new(),
    }
}

/// Escape a JSON string so it is safe to embed inside a JS double-quoted string literal
/// inside an HTML `<script>` block.
///
/// Escapes required:
/// - `\` and `"` — prevent JS string termination / invalid escape sequences
/// - `<`, `>`, `&` — prevent `</script>` injection breaking out of the script context
/// - U+2028, U+2029 — line/paragraph separators that are valid JSON but terminate
///   a JS string literal in some parsers
///
/// All substitutions use `\uXXXX` form, which is valid inside both JSON strings
/// and JS string literals. The result is always safe to write as `JSON.parse("…")`.
fn html_escape_for_script(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(ch),
        }
    }
    out
}

/// Build a price-bucketed bid map from winning bids.
///
/// Returns a JSON object map of slot ID → bid metadata including the bucketed
/// CPM (`hb_pb`), bidder (`hb_bidder`), and optional ad ID, nurl, and burl.
pub(crate) fn build_bid_map(
    winning_bids: &std::collections::HashMap<String, Bid>,
    granularity: crate::price_bucket::PriceGranularity,
    include_adm: bool,
) -> serde_json::Map<String, serde_json::Value> {
    winning_bids
        .iter()
        .filter_map(|(slot_id, bid)| {
            bid.price.map(|cpm| {
                let bucket = price_bucket(cpm, granularity);
                let mut obj = serde_json::Map::new();
                obj.insert("hb_pb".to_string(), serde_json::Value::String(bucket));
                obj.insert(
                    "hb_bidder".to_string(),
                    serde_json::Value::String(bid.bidder.clone()),
                );
                // hb_adid: use PBS Cache UUID when present — the Prebid Universal Creative uses
                // this as the cache lookup key, NOT the OpenRTB bid ID (bid.ad_id). Fall back to
                // bid.ad_id for APS and other non-PBS providers.
                let hb_adid = bid.cache_id.as_deref().or(bid.ad_id.as_deref());
                if let Some(id) = hb_adid {
                    obj.insert(
                        "hb_adid".to_string(),
                        serde_json::Value::String(id.to_string()),
                    );
                }

                // Cache endpoint coordinates — only present for PBS bids with Prebid Cache enabled.
                // The Prebid Universal Creative constructs:
                //   https://<hb_cache_host><hb_cache_path>?uuid=<hb_adid>
                if let Some(ref host) = bid.cache_host {
                    obj.insert(
                        "hb_cache_host".to_string(),
                        serde_json::Value::String(host.clone()),
                    );
                }
                if let Some(ref path) = bid.cache_path {
                    obj.insert(
                        "hb_cache_path".to_string(),
                        serde_json::Value::String(path.clone()),
                    );
                }
                if let Some(ref nurl) = bid.nurl {
                    obj.insert("nurl".to_string(), serde_json::Value::String(nurl.clone()));
                }
                if let Some(ref burl) = bid.burl {
                    obj.insert("burl".to_string(), serde_json::Value::String(burl.clone()));
                }
                // Include raw creative markup only for explicit debug injection.
                // The pbRender bridge can use it while PBS Cache is unavailable.
                if include_adm {
                    if let Some(ref adm) = bid.creative {
                        obj.insert("adm".to_string(), serde_json::Value::String(adm.clone()));
                    }
                    obj.insert(
                        "debug_bid".to_string(),
                        serde_json::json!({
                            "slot_id": bid.slot_id,
                            "price": bid.price,
                            "currency": bid.currency,
                            "creative": bid.creative,
                            "adomain": bid.adomain,
                            "bidder": bid.bidder,
                            "width": bid.width,
                            "height": bid.height,
                            "nurl": bid.nurl,
                            "burl": bid.burl,
                            "ad_id": bid.ad_id,
                            "cache_id": bid.cache_id,
                            "cache_host": bid.cache_host,
                            "cache_path": bid.cache_path,
                            "metadata": bid.metadata,
                        }),
                    );
                }
                (slot_id.clone(), serde_json::Value::Object(obj))
            })
        })
        .collect()
}

/// Build the `tsjs.bids` `<script>` tag from a bucketed bid map.
///
/// The JSON is embedded via `JSON.parse(…)` so the browser parser never sees
/// raw `</script>` sequences inside the string.
pub(crate) fn build_bids_script(bid_map: &serde_json::Map<String, serde_json::Value>) -> String {
    let json = serde_json::to_string(bid_map)
        .expect("serde_json::to_string of Map<String,Value> should be infallible");
    let escaped = html_escape_for_script(&json);
    format!(
        "<script>(window.tsjs=window.tsjs||{{}}).bids=JSON.parse(\"{}\");(function(){{var f=window.tsjs.adInit;if(typeof f===\"function\")f();}})();</script>",
        escaped
    )
}

/// Build the empty-bids `<script>` tag used when no bids were returned.
///
/// Shares the same shape as [`build_bids_script`] so any change to the script
/// format stays in one place.
pub(crate) fn build_empty_bids_script() -> String {
    build_bids_script(&serde_json::Map::new())
}

/// Builds the client-facing JSON wire shape for one creative-opportunity slot.
///
/// Shared verbatim by [`build_ad_slots_script`] (initial page render) and
/// [`handle_page_bids`] (SPA navigation) so the slot wire shape has a single
/// definition and the two paths cannot silently diverge. Property names match
/// what the client-side TSJS bundle expects: `gam_unit_path`, `div_id`,
/// `formats`, and `targeting`.
fn build_slot_json(
    slot: &crate::creative_opportunities::CreativeOpportunitySlot,
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
) -> serde_json::Value {
    let gam_path = slot.resolved_gam_unit_path(&co_config.gam_network_id);
    let div_id = slot.resolved_div_id();
    let formats: Vec<serde_json::Value> = slot
        .formats
        .iter()
        .map(|f| serde_json::json!([f.width, f.height]))
        .collect();
    let targeting: serde_json::Map<String, serde_json::Value> = slot
        .targeting
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::json!({
        "id": slot.id,
        "gam_unit_path": gam_path,
        "div_id": div_id,
        "formats": formats,
        "targeting": targeting,
    })
}

/// Build the `tsjs.adSlots` `<script>` tag from matched slots.
///
/// Property names match what the client-side TSJS bundle expects:
/// `gam_unit_path`, `div_id`, `formats`, and `targeting`.
pub(crate) fn build_ad_slots_script(
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
) -> String {
    let slots: Vec<serde_json::Value> = matched_slots
        .iter()
        .map(|slot| build_slot_json(slot, co_config))
        .collect();
    let json = serde_json::to_string(&slots)
        .expect("serde_json::to_string of Vec<Value> should be infallible");
    let escaped = html_escape_for_script(&json);
    format!(
        "<script>(window.tsjs=window.tsjs||{{}}).adSlots=JSON.parse(\"{}\");</script>",
        escaped
    )
}

/// Whether the content type requires processing (URL rewriting, HTML injection).
///
/// Text-based and JavaScript/JSON responses are processable; binary types
/// (images, fonts, video, etc.) pass through unchanged.
fn is_processable_content_type(content_type: &str) -> bool {
    let normalized = content_type.to_ascii_lowercase();
    normalized.contains("text/")
        || normalized.contains("application/javascript")
        || normalized.contains("application/json")
}

fn is_html_content_type(content_type: &str) -> bool {
    content_type_contains_ascii_case_insensitive(content_type, "text/html")
}

fn content_type_contains_ascii_case_insensitive(content_type: &str, needle: &str) -> bool {
    content_type.to_ascii_lowercase().contains(needle)
}

/// Whether the `Content-Encoding` is one the streaming pipeline can handle.
///
/// Unsupported encodings (e.g. `zstd` from a misbehaving origin) bypass the
/// rewrite pipeline entirely and are returned unchanged. Processing such bodies
/// as identity-encoded would produce garbled output.
fn is_supported_content_encoding(encoding: &str) -> bool {
    matches!(encoding, "" | "identity" | "gzip" | "deflate" | "br")
}

/// Same-origin gate for `/__ts/page-bids`.
///
/// The endpoint is a side-effecting GET: it dispatches real PBS/APS auctions
/// and forwards request-derived signals (IP, UA, geo, consent) to partners.
/// Without a gate, any third-party page could trigger it from a visitor's
/// browser (it cannot read the JSON, but it burns SSP quota and leaks
/// outbound partner calls).
///
/// A request is allowed when:
/// - `Sec-Fetch-Site` is `same-origin` (the tsjs SPA hook fetches a relative
///   URL, so a genuine same-origin navigation always reports this). `same-site`
///   is intentionally rejected: it admits sibling origins under the same
///   registrable domain, which are not trusted to spend SSP quota on the
///   visitor's behalf.
/// - `Sec-Fetch-Site` is absent (legacy client predating Fetch Metadata) **and**
///   the request carries the non-simple `X-TSJS-Page-Bids` header set by the
///   tsjs SPA hook — cross-origin callers cannot attach it without a CORS
///   preflight, which this endpoint never grants.
fn page_bids_request_allowed(req: &Request<EdgeBody>) -> bool {
    match req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
    {
        Some(site) => site == "same-origin",
        None => req.headers().contains_key("x-tsjs-page-bids"),
    }
}

/// Builds the `403 Forbidden` returned when the side-effecting
/// `/__ts/page-bids` endpoint refuses a request — both the CORS preflight
/// (`OPTIONS`) and the GET cross-site gate ([`page_bids_request_allowed`])
/// return this single denial shape.
///
/// The GET handler's [`page_bids_request_allowed`] gate trusts the
/// `X-TSJS-Page-Bids` header precisely because this endpoint never grants a
/// preflight; letting `OPTIONS` fall through to the publisher origin (which may
/// return permissive CORS) would defeat that, allowing a cross-site page to
/// trigger real PBS/APS auctions from a visitor's browser. Every adapter returns
/// this same response for `OPTIONS /__ts/page-bids`.
pub fn page_bids_preflight_denied() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Forbidden"));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

/// Normalizes the client-supplied `path` query parameter before glob matching.
///
/// The SPA hook sends `location.pathname`, but the parameter is
/// client-controlled: strip any query string or fragment and force a leading
/// `/` so slot `page_patterns` always match against a canonical path shape.
fn normalize_page_bids_path(raw: &str) -> String {
    let path = raw.split(['?', '#']).next().unwrap_or("");
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Handle `GET /__ts/page-bids?path=<path>` — server-side auction for SPA navigation.
///
/// Matches creative opportunity slots for the given path, runs a server-side
/// auction (APS + PBS), and returns the slot definitions and winning bids as JSON.
/// Called by the client-side SPA navigation hook after `pushState` / `popstate`.
///
/// `kv` enriches the bid request with server-side EIDs from the EC identity
/// graph. Only the Fastly adapter has a KV identity store, so Axum, Cloudflare,
/// and Spin pass `None`; client cookie EIDs are still resolved and consent-gated
/// on every adapter, so no adapter forwards unconsented EIDs.
///
/// # Errors
///
/// Returns [`TrustedServerError`] if cookie parsing or EC ID generation fails.
pub async fn handle_page_bids(
    settings: &Settings,
    services: &RuntimeServices,
    kv: Option<&KvIdentityGraph>,
    auction: AuctionDispatch<'_>,
    ec_context: &EcContext,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let Some(co_config) = &settings.creative_opportunities else {
        let mut response = Response::new(EdgeBody::from("Creative opportunities not configured"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        return Ok(response);
    };

    // CSRF-style gate: refuse cross-site invocations before any auction work.
    if !page_bids_request_allowed(&req) {
        log::debug!(
            "page-bids: rejecting request (sec-fetch-site={:?}, tsjs header present={})",
            req.headers()
                .get("sec-fetch-site")
                .and_then(|v| v.to_str().ok()),
            req.headers().contains_key("x-tsjs-page-bids")
        );
        return Ok(page_bids_preflight_denied());
    }

    let path_param = req
        .uri()
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(k, _)| k == "path")
                .map(|(_, v)| normalize_page_bids_path(&v))
        })
        .unwrap_or_else(|| "/".to_string());

    let matched_slots: Vec<_> =
        crate::creative_opportunities::match_slots(auction.slots, &path_param)
            .into_iter()
            .cloned()
            .collect();

    let request_info = crate::http_util::RequestInfo::from_request(&req, services.client_info());
    let ec_id = ec_context.ec_value().filter(|_| ec_context.ec_allowed());
    let consent_context = ec_context.consent();
    let geo = ec_context.geo_info().cloned();
    let cookie_jar = handle_request_cookies(&req)?;

    // Same fail-closed jurisdiction-aware gate the publisher navigation path
    // uses — relies on the adapter's geo-aware EC context.
    let consent_allows_auction = consent_allows_server_side_auction(consent_context);

    // Same bot / prefetch guards the publisher path uses — without them this
    // endpoint would fire real SSP auctions on Sec-Purpose=prefetch warm-up
    // navigations and known crawler UA scans, burning partner request quota.
    let is_prefetch = is_prefetch_request(&req);
    let is_bot = is_bot_user_agent(&req);

    let auction_enabled = auction.orchestrator.is_enabled();
    if !auction_enabled {
        log::debug!("page-bids: [auction].enabled is false — skipping auction");
    } else if matched_slots.is_empty() {
        log::debug!(
            "No creative opportunity slots matched path '{}' — skipping auction",
            path_param
        );
    } else if is_bot || is_prefetch {
        log::debug!(
            "page-bids: skipping auction for path '{}' (is_bot={}, is_prefetch={})",
            path_param,
            is_bot,
            is_prefetch
        );
    }

    // The [auction].enabled kill switch and a consent denial disable the entire
    // server-side ad stack. In those states the endpoint must return no slots,
    // so the SPA hook does not assign `ts.adSlots` and call `adInit()` —
    // otherwise the kill switch/consent gate would stop SSP calls but still let
    // the client create/refresh GPT slots. Bot/prefetch requests, by contrast,
    // keep their slot definitions (the placement structure is unchanged) but
    // skip the live auction, matching the existing bot/prefetch behaviour.
    let ad_stack_enabled = auction_enabled && consent_allows_auction;

    let winning_bids = if matched_slots.is_empty() {
        std::collections::HashMap::new()
    } else {
        let observation = AuctionObservationContext::from_parts(
            AuctionSource::SpaNavigation,
            &request_info.host,
            &path_param,
            matched_slots.len(),
            ec_context,
        );
        if ad_stack_enabled && !is_bot && !is_prefetch {
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &matched_slots,
                request_path: &path_param,
            };
            let mut auction_request = build_auction_request(
                &slots_ctx,
                ec_id,
                consent_context,
                &request_info,
                req.headers()
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok()),
            );
            apply_auction_eids_and_device(
                &mut auction_request,
                &AuctionEidTargeting {
                    cookie_jar: cookie_jar.as_ref(),
                    ec_id,
                    kv,
                    partner_registry: auction.registry,
                    ec_context,
                    services,
                    geo: geo.as_ref(),
                    path_label: "Page-bids",
                },
            );
            let timeout_ms = co_config
                .auction_timeout_ms
                .unwrap_or(settings.auction.timeout_ms);
            let auction_context = AuctionContext {
                settings,
                request: &req,
                timeout_ms,
                provider_responses: None,
                services,
            };
            match auction
                .orchestrator
                .run_auction(&auction_request, &auction_context)
                .await
            {
                Ok(result) => {
                    let winning_bids = result.winning_bids.clone();
                    emit_auction_events_best_effort_lazy(services, || {
                        build_auction_events(
                            observation,
                            AuctionTerminalOutcome::Completed {
                                request: &auction_request,
                                result: &result,
                            },
                        )
                    })
                    .await;
                    winning_bids
                }
                Err(e) => {
                    log::warn!("page-bids auction failed: {e:?}");
                    let elapsed_ms = observation.elapsed_ms();
                    emit_auction_events_best_effort_lazy(services, || {
                        build_auction_events(
                            observation,
                            AuctionTerminalOutcome::ExecutionFailed {
                                request: Some(&auction_request),
                                provider_responses: &[],
                                reason: "execution_failed",
                                elapsed_ms,
                            },
                        )
                    })
                    .await;
                    std::collections::HashMap::new()
                }
            }
        } else {
            let skip_reason = if !auction_enabled {
                "auction_disabled"
            } else if !consent_allows_auction {
                "consent_denied"
            } else if is_bot {
                "bot"
            } else if is_prefetch {
                "prefetch"
            } else {
                "not_ad_stack_eligible"
            };
            let elapsed_ms = observation.elapsed_ms();
            emit_auction_events_best_effort_lazy(services, || {
                build_auction_events(
                    observation,
                    AuctionTerminalOutcome::Skipped {
                        reason: skip_reason,
                        elapsed_ms,
                    },
                )
            })
            .await;
            std::collections::HashMap::new()
        }
    };

    let bid_map = build_bid_map(
        &winning_bids,
        co_config.price_granularity,
        settings.debug.inject_adm_for_testing,
    );

    // Gate slots on the ad-stack kill switch / consent: when disabled, return no
    // slots so the SPA hook does not call `adInit()` / create GPT slots.
    let slots_json: Vec<serde_json::Value> = if ad_stack_enabled {
        matched_slots
            .iter()
            .map(|slot| build_slot_json(slot, co_config))
            .collect()
    } else {
        Vec::new()
    };

    let body = serde_json::json!({
        "slots": slots_json,
        "bids": bid_map,
    });

    let json_str = serde_json::to_string(&body).change_context(TrustedServerError::Proxy {
        message: "Failed to serialize page-bids response".to_string(),
    })?;

    let mut response = Response::new(EdgeBody::from(json_str));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );

    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use brotli::enc::writer::CompressorWriter;
    use brotli::Decompressor;
    use flate2::read::GzDecoder;
    use flate2::write::{GzEncoder, ZlibEncoder};

    use super::*;
    use crate::auction::types::{AdFormat, AdSlot, MediaType};
    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::{
        build_services_with_http_client, noop_services, StubHttpClient,
    };
    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};
    use edgezero_core::body::Body as EdgeBody;
    use http::{header, Method, Request as HttpRequest, StatusCode};
    use std::sync::Arc;

    fn gzip_encode(input: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(input)
            .expect("should write gzip test input");
        encoder.finish().expect("should finish gzip encoding")
    }

    fn gzip_decode(input: &[u8]) -> Vec<u8> {
        let mut decoder = GzDecoder::new(input);
        let mut output = Vec::new();
        decoder
            .read_to_end(&mut output)
            .expect("should decode gzip test output");
        output
    }

    fn deflate_encode(input: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(input)
            .expect("should write deflate test input");
        encoder.finish().expect("should finish deflate encoding")
    }

    fn brotli_encode(input: &[u8]) -> Vec<u8> {
        let mut encoder = CompressorWriter::new(Vec::new(), 4096, 5, 22);
        encoder
            .write_all(input)
            .expect("should write brotli test input");
        encoder.into_inner()
    }

    fn brotli_decode(input: &[u8]) -> Vec<u8> {
        let mut decoder = Decompressor::new(input, 4096);
        let mut output = Vec::new();
        decoder
            .read_to_end(&mut output)
            .expect("should decode brotli test output");
        output
    }

    fn make_stream_params(
        settings: &Settings,
        content_encoding: &str,
    ) -> OwnedProcessResponseParams {
        OwnedProcessResponseParams {
            input_compression: Compression::from_content_encoding(content_encoding),
            output_compression: Compression::from_content_encoding(content_encoding),
            origin_host: settings.publisher.origin_host(),
            origin_url: settings.publisher.origin_url.clone(),
            request_host: settings.publisher.domain.clone(),
            request_scheme: "https".to_owned(),
            content_type: "application/json".to_owned(),
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: Default::default(),
        }
    }

    fn make_html_ssat_params(
        settings: &Settings,
        dispatched_auction: Option<DispatchedAuction>,
    ) -> OwnedProcessResponseParams {
        OwnedProcessResponseParams {
            input_compression: Compression::None,
            output_compression: Compression::None,
            origin_host: settings.publisher.origin_host(),
            origin_url: settings.publisher.origin_url.clone(),
            request_host: settings.publisher.domain.clone(),
            request_scheme: "https".to_owned(),
            content_type: "text/html; charset=utf-8".to_owned(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=[];</script>"#.to_owned(),
            ),
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            auction_observation: None,
            auction_request: None,
            dispatched_auction,
            price_granularity: Default::default(),
        }
    }

    fn test_auction_request() -> AuctionRequest {
        AuctionRequest {
            id: "test-auction".to_string(),
            slots: vec![AdSlot {
                id: "atf".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: None,
                targeting: Default::default(),
                bidders: Default::default(),
            }],
            publisher: PublisherInfo {
                domain: "test-publisher.com".to_string(),
                page_url: Some("https://test-publisher.com/article".to_string()),
            },
            user: UserInfo {
                id: None,
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: Default::default(),
        }
    }

    fn build_request(method: Method, uri: &str) -> HttpRequest<EdgeBody> {
        HttpRequest::builder()
            .method(method)
            .uri(uri)
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    #[test]
    fn stream_publisher_body_round_trips_gzip() {
        let settings = create_test_settings();
        let integration_registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let input = b"{\"asset\":\"https://origin.test-publisher.com/path/file.js\"}";
        let compressed = gzip_encode(input);
        let params = make_stream_params(&settings, "gzip");
        let mut output = Vec::new();

        stream_publisher_body(
            EdgeBody::from(compressed),
            &mut output,
            &params,
            &settings,
            &integration_registry,
        )
        .expect("should stream gzip response through rewrite pipeline");

        let decoded = gzip_decode(&output);
        let decoded = String::from_utf8(decoded).expect("should decode rewritten gzip payload");
        assert!(
            decoded.contains("https://test-publisher.com/path/file.js"),
            "should rewrite origin URLs to the request host"
        );
        assert!(
            !decoded.contains("origin.test-publisher.com"),
            "should remove the origin hostname from the rewritten payload"
        );
    }

    #[test]
    fn stream_publisher_body_round_trips_brotli() {
        let settings = create_test_settings();
        let integration_registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let input = b"{\"asset\":\"https://origin.test-publisher.com/path/file.css\"}";
        let compressed = brotli_encode(input);
        let params = make_stream_params(&settings, "br");
        let mut output = Vec::new();

        stream_publisher_body(
            EdgeBody::from(compressed),
            &mut output,
            &params,
            &settings,
            &integration_registry,
        )
        .expect("should stream brotli response through rewrite pipeline");

        let decoded = brotli_decode(&output);
        let decoded = String::from_utf8(decoded).expect("should decode rewritten brotli payload");
        assert!(
            decoded.contains("https://test-publisher.com/path/file.css"),
            "should rewrite origin URLs to the request host"
        );
        assert!(
            !decoded.contains("origin.test-publisher.com"),
            "should remove the origin hostname from the rewritten payload"
        );
    }

    fn ssat_compression_test_settings(enabled: bool) -> Settings {
        let mut settings = Settings::from_toml(&format!(
            r#"{}

            [auction]
            enabled = true

            [creative_opportunities]
            gam_network_id = "12345"

            [[creative_opportunities.slot]]
            id = "article-slot"
            page_patterns = ["/article"]
            formats = [{{ width = 300, height = 250 }}]
            "#,
            crate_test_settings_str()
        ))
        .expect("should parse SSAT compression test settings");
        settings.publisher.ssat_compression_offload_enabled = enabled;
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
    }

    async fn run_ssat_compression_request(
        settings: &Settings,
        adapter_options: PublisherAdapterOptions,
        origin_body: Vec<u8>,
        origin_status: u16,
        origin_content_type: &str,
        origin_content_encoding: Option<&str>,
    ) -> (
        PublisherResponse,
        Arc<StubHttpClient>,
        RuntimeServices,
        AuctionOrchestrator,
    ) {
        let stub = Arc::new(StubHttpClient::new());
        let mut origin_headers = vec![
            (
                header::CONTENT_TYPE.as_str().to_string(),
                origin_content_type.to_string(),
            ),
            (
                header::CONTENT_LENGTH.as_str().to_string(),
                "9999".to_string(),
            ),
            (header::VARY.as_str().to_string(), "Origin".to_string()),
            (
                header::VARY.as_str().to_string(),
                "Accept-Language".to_string(),
            ),
        ];
        if let Some(origin_content_encoding) = origin_content_encoding {
            origin_headers.push((
                header::CONTENT_ENCODING.as_str().to_string(),
                origin_content_encoding.to_string(),
            ));
        }
        stub.push_response_with_headers(origin_status, origin_body, origin_headers);
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let consent = crate::consent::ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..Default::default()
        };
        let mut ec_context = EcContext::new_for_test_with_ip(None, consent, None);
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/article?edition=morning&view=full")
            .header(header::HOST, "publisher.example")
            .header(header::ACCEPT_ENCODING, "gzip, br, identity;q=0")
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build SSAT compression request");

        let publisher_response = handle_publisher_request(
            settings,
            &services,
            None,
            &mut ec_context,
            AuctionDispatch {
                orchestrator: &orchestrator,
                slots: settings.creative_opportunity_slots(),
                registry: None,
            },
            req,
            adapter_options,
        )
        .await
        .expect("should proxy SSAT compression request");

        (publisher_response, stub, services, orchestrator)
    }

    #[test]
    fn ssat_compression_offload_requires_setting_capability_and_ad_stack() {
        let mut settings = create_test_settings();
        let fastly_options = PublisherAdapterOptions {
            edge_cache_header: EdgeCacheHeader::SurrogateControl,
            delivery_compression: DeliveryCompressionCapability::FastlyDynamic,
        };
        let unavailable_options = PublisherAdapterOptions {
            edge_cache_header: EdgeCacheHeader::SMaxageFallback,
            delivery_compression: DeliveryCompressionCapability::Unavailable,
        };

        assert!(
            !should_offload_ssat_compression(&settings, fastly_options, true),
            "disabled setting should retain in-guest compression"
        );
        settings.publisher.ssat_compression_offload_enabled = true;
        assert!(
            !should_offload_ssat_compression(&settings, unavailable_options, true),
            "adapters without Fastly dynamic compression should retain in-guest compression"
        );
        assert!(
            !should_offload_ssat_compression(&settings, fastly_options, false),
            "requests outside the server-side ad stack should retain in-guest compression"
        );
        assert!(
            should_offload_ssat_compression(&settings, fastly_options, true),
            "enabled Fastly server-side ad stack should offload compression"
        );
    }

    #[test]
    fn ensure_vary_accept_encoding_preserves_wildcard() {
        let mut headers = http::HeaderMap::new();
        headers.insert(header::VARY, HeaderValue::from_static("*"));

        ensure_vary_accept_encoding(&mut headers);

        assert_eq!(
            headers
                .get_all(header::VARY)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .collect::<Vec<_>>(),
            vec!["*"],
            "wildcard Vary should not receive a redundant Accept-Encoding field"
        );
    }

    #[tokio::test]
    async fn fastly_ssat_compression_offload_preserves_origin_negotiation_and_emits_identity() {
        let settings = ssat_compression_test_settings(true);
        let html = b"<html><body><p>Origin HTML</p></body></html>";
        let (publisher_response, stub, services, orchestrator) = run_ssat_compression_request(
            &settings,
            PublisherAdapterOptions {
                edge_cache_header: EdgeCacheHeader::SurrogateControl,
                delivery_compression: DeliveryCompressionCapability::FastlyDynamic,
            },
            html.to_vec(),
            200,
            "text/html; charset=utf-8",
            None,
        )
        .await;

        assert_eq!(
            stub.recorded_request_uris(),
            vec!["https://origin.test-publisher.com/article?edition=morning&view=full"],
            "offload must preserve the publisher request path and query"
        );
        let request_headers = stub.recorded_request_headers();
        assert!(
            request_headers[0].iter().any(|(name, value)| {
                name.eq_ignore_ascii_case(header::ACCEPT_ENCODING.as_str()) && value == "gzip, br"
            }),
            "Fastly SSAT offload must retain supported origin negotiation"
        );

        let PublisherResponse::Stream {
            response, params, ..
        } = &publisher_response
        else {
            panic!("processable SSAT HTML should use the stream route");
        };
        assert_eq!(params.input_compression, Compression::None);
        assert_eq!(params.output_compression, Compression::None);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=0"),
            "compression offload must preserve SSAT browser privacy"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "compression offload must not restore shared cacheability"
        );
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_COMPRESS_HINT)
                .and_then(|value| value.to_str().ok()),
            Some("on")
        );
        let vary = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        assert_eq!(
            vary,
            vec!["Origin", "Accept-Language", "Accept-Encoding"],
            "offload must preserve existing Vary fields and add Accept-Encoding once"
        );

        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let response = buffer_publisher_response_async(
            publisher_response,
            &Method::GET,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should buffer identity SSAT output");
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        let body = response
            .into_body()
            .into_bytes()
            .expect("should read identity SSAT output");
        let body = String::from_utf8(body.to_vec()).expect("should decode identity SSAT output");
        assert!(body.contains("Origin HTML"), "should preserve origin HTML");
        assert!(body.contains("tsjs"), "should still process SSAT HTML");
    }

    #[tokio::test]
    async fn fastly_ssat_compression_offload_preserves_non_html_error_compression() {
        let settings = ssat_compression_test_settings(true);
        let json = br#"{"message":"Negotiated JSON"}"#;
        let (publisher_response, stub, services, orchestrator) = run_ssat_compression_request(
            &settings,
            PublisherAdapterOptions {
                edge_cache_header: EdgeCacheHeader::SurrogateControl,
                delivery_compression: DeliveryCompressionCapability::FastlyDynamic,
            },
            gzip_encode(json),
            502,
            "application/json",
            Some("gzip"),
        )
        .await;

        let request_headers = stub.recorded_request_headers();
        assert!(
            request_headers[0].iter().any(|(name, value)| {
                name.eq_ignore_ascii_case(header::ACCEPT_ENCODING.as_str()) && value == "gzip, br"
            }),
            "offload must preserve supported origin negotiation for non-HTML responses"
        );

        let PublisherResponse::Stream {
            response, params, ..
        } = &publisher_response
        else {
            panic!("processable JSON should use the stream route");
        };
        assert_eq!(response.status(), http::StatusCode::BAD_GATEWAY);
        assert_eq!(params.input_compression, Compression::Gzip);
        assert_eq!(params.output_compression, Compression::Gzip);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
        assert!(response.headers().get(HEADER_X_COMPRESS_HINT).is_none());

        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let response = buffer_publisher_response_async(
            publisher_response,
            &Method::GET,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should buffer mirrored non-HTML output");
        let body = response
            .into_body()
            .into_bytes()
            .expect("should read mirrored non-HTML output");
        let decoded = gzip_decode(&body);
        assert!(
            String::from_utf8(decoded)
                .expect("should decode mirrored non-HTML gzip")
                .contains("Negotiated JSON"),
            "non-HTML output must remain valid gzip"
        );
    }

    #[tokio::test]
    async fn fastly_ssat_compression_offload_decodes_supported_origin_encodings() {
        let settings = ssat_compression_test_settings(true);
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let html = b"<html><body><p>Compressed origin HTML</p></body></html>";
        let cases = [
            ("gzip", gzip_encode(html)),
            ("deflate", deflate_encode(html)),
            ("br", brotli_encode(html)),
        ];

        for (origin_content_encoding, origin_body) in cases {
            let (publisher_response, _stub, services, orchestrator) = run_ssat_compression_request(
                &settings,
                PublisherAdapterOptions {
                    edge_cache_header: EdgeCacheHeader::SurrogateControl,
                    delivery_compression: DeliveryCompressionCapability::FastlyDynamic,
                },
                origin_body,
                200,
                "text/html; charset=utf-8",
                Some(origin_content_encoding),
            )
            .await;

            let PublisherResponse::Stream {
                response, params, ..
            } = &publisher_response
            else {
                panic!("supported compressed SSAT HTML should use the stream route");
            };
            assert_eq!(
                params.input_compression,
                Compression::from_content_encoding(origin_content_encoding),
                "should decode the origin's actual content encoding"
            );
            assert_eq!(params.output_compression, Compression::None);
            assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_X_COMPRESS_HINT)
                    .and_then(|value| value.to_str().ok()),
                Some("on")
            );

            let response = buffer_publisher_response_async(
                publisher_response,
                &Method::GET,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("should buffer decoded SSAT output");
            let body = response
                .into_body()
                .into_bytes()
                .expect("should read decoded SSAT output");
            let body =
                String::from_utf8(body.to_vec()).expect("should decode identity SSAT output");
            assert!(
                body.contains("Compressed origin HTML"),
                "should decode {origin_content_encoding} origin HTML"
            );
        }
    }

    #[tokio::test]
    async fn unavailable_delivery_compression_keeps_mirrored_gzip_behavior() {
        let settings = ssat_compression_test_settings(true);
        let html = b"<html><body><p>Portable adapter</p></body></html>";
        let (publisher_response, stub, services, orchestrator) = run_ssat_compression_request(
            &settings,
            PublisherAdapterOptions {
                edge_cache_header: EdgeCacheHeader::SMaxageFallback,
                delivery_compression: DeliveryCompressionCapability::Unavailable,
            },
            gzip_encode(html),
            200,
            "text/html; charset=utf-8",
            Some("gzip"),
        )
        .await;

        let request_headers = stub.recorded_request_headers();
        assert!(
            request_headers[0].iter().any(|(name, value)| {
                name.eq_ignore_ascii_case(header::ACCEPT_ENCODING.as_str()) && value == "gzip, br"
            }),
            "non-Fastly adapters must retain supported client encodings"
        );
        let PublisherResponse::Stream {
            response, params, ..
        } = &publisher_response
        else {
            panic!("processable SSAT HTML should use the stream route");
        };
        assert_eq!(params.input_compression, Compression::Gzip);
        assert_eq!(params.output_compression, Compression::Gzip);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
        assert!(response.headers().get(HEADER_X_COMPRESS_HINT).is_none());
        assert_eq!(
            response
                .headers()
                .get_all(header::VARY)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .collect::<Vec<_>>(),
            vec!["Origin", "Accept-Language"],
            "non-Fastly adapters must not mutate Vary"
        );

        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let response = buffer_publisher_response_async(
            publisher_response,
            &Method::GET,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should buffer mirrored gzip output");
        let body = response
            .into_body()
            .into_bytes()
            .expect("should read mirrored gzip output");
        let decoded = gzip_decode(&body);
        assert!(
            String::from_utf8(decoded)
                .expect("should decode mirrored gzip")
                .contains("Portable adapter"),
            "non-Fastly output must remain valid gzip"
        );
    }

    #[tokio::test]
    async fn disabled_offload_keeps_default_mirrored_compression() {
        let settings = ssat_compression_test_settings(false);
        let html = b"<html><body><p>Default behavior</p></body></html>";
        let (publisher_response, stub, _services, _orchestrator) = run_ssat_compression_request(
            &settings,
            PublisherAdapterOptions {
                edge_cache_header: EdgeCacheHeader::SurrogateControl,
                delivery_compression: DeliveryCompressionCapability::FastlyDynamic,
            },
            gzip_encode(html),
            200,
            "text/html; charset=utf-8",
            Some("gzip"),
        )
        .await;

        let request_headers = stub.recorded_request_headers();
        assert!(request_headers[0].iter().any(|(name, value)| {
            name.eq_ignore_ascii_case(header::ACCEPT_ENCODING.as_str()) && value == "gzip, br"
        }));
        let PublisherResponse::Stream {
            response, params, ..
        } = publisher_response
        else {
            panic!("processable SSAT HTML should use the stream route");
        };
        assert_eq!(params.input_compression, Compression::Gzip);
        assert_eq!(params.output_compression, Compression::Gzip);
        assert!(response.headers().get(HEADER_X_COMPRESS_HINT).is_none());
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
    }

    #[test]
    fn request_ec_uses_cookie_not_header() {
        let settings = create_test_settings();
        let header_ec = format!("{}.HdrId1", "a".repeat(64));
        let cookie_ec = format!("{}.CkId01", "b".repeat(64));
        let req = Request::builder()
            .method(Method::GET)
            .uri("https://test.example.com/page")
            .header("x-ts-ec", &header_ec)
            .header("cookie", format!("ts-ec={cookie_ec}; other=value"))
            .body(EdgeBody::empty())
            .expect("should build test request");

        let ec_context = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert_eq!(
            ec_context.ec_value(),
            Some(cookie_ec.as_str()),
            "should resolve request EC ID from cookie"
        );
        assert!(
            ec_context.cookie_was_present(),
            "should detect cookie was present"
        );
        assert_eq!(
            ec_context.existing_cookie_ec_id(),
            Some(cookie_ec.as_str()),
            "should return cookie EC value for revocation"
        );
    }

    /// Drive `handle_publisher_request` with no creative opportunities — a plain
    /// proxy with no server-side auction. Hides the auction/EC wiring so callers
    /// read like a simple `(settings, services, req)` proxy.
    async fn run_publisher_proxy(
        settings: &Settings,
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> PublisherResponse {
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let mut ec_context =
            EcContext::read_from_request(settings, &req, services).expect("should read EC context");
        handle_publisher_request(
            settings,
            services,
            None,
            &mut ec_context,
            AuctionDispatch {
                orchestrator: &orchestrator,
                slots: &[],
                registry: None,
            },
            req,
            PublisherAdapterOptions {
                edge_cache_header: EdgeCacheHeader::SurrogateControl,
                delivery_compression: DeliveryCompressionCapability::Unavailable,
            },
        )
        .await
        .expect("should proxy publisher request")
    }

    #[tokio::test]
    async fn publisher_request_uses_platform_http_client_with_http_types() {
        let settings = create_test_settings();
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"origin response".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/page")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = match run_publisher_proxy(&settings, &services, req).await {
            PublisherResponse::Buffered(r) => r,
            PublisherResponse::PassThrough { mut response, body } => {
                *response.body_mut() = body;
                response
            }
            PublisherResponse::Stream { response, .. } => response,
        };

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body_string(response), "origin response");
        assert_eq!(
            stub.recorded_backend_names(),
            vec!["stub-backend".to_string()],
            "should proxy through the platform http client"
        );
    }

    #[tokio::test]
    async fn publisher_request_applies_configured_asset_cache_policy() {
        let settings = Settings::from_toml(&format!(
            r#"{}

            [[cache.asset_rules]]
            id = "publisher-fingerprinted-assets"
            enabled = true
            path_globs = ["/assets/**/*.png"]
            requires_hash_in_filename = true
            visibility = "public"
            browser_ttl_seconds = 31536000
            edge_ttl_seconds = 31536000
            immutable = true
        "#,
            crate_test_settings_str()
        ))
        .expect("should parse settings with cache rule");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"png".to_vec(),
            vec![
                (header::CONTENT_TYPE.as_str(), "image/png"),
                (header::CACHE_CONTROL.as_str(), "public, max-age=60"),
            ],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/assets/logo.0123abcd.png")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = match run_publisher_proxy(&settings, &services, req).await {
            PublisherResponse::PassThrough { response, .. } => response,
            PublisherResponse::Buffered(response) | PublisherResponse::Stream { response, .. } => {
                response
            }
        };

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=31536000, immutable"),
            "matched publisher-origin asset should receive normalized immutable policy"
        );
        assert_eq!(
            response
                .headers()
                .get("surrogate-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000"),
            "publisher-origin asset should receive selected runtime edge header"
        );
    }

    #[tokio::test]
    async fn publisher_asset_cache_policy_respects_split_no_store_origin_header() {
        let settings = Settings::from_toml(&format!(
            r#"{}

            [[cache.asset_rules]]
            id = "publisher-fingerprinted-assets"
            enabled = true
            path_globs = ["/assets/**/*.png"]
            requires_hash_in_filename = true
            visibility = "public"
            browser_ttl_seconds = 31536000
            edge_ttl_seconds = 31536000
            immutable = true
        "#,
            crate_test_settings_str()
        ))
        .expect("should parse settings with cache rule");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"png".to_vec(),
            vec![
                (header::CONTENT_TYPE.as_str(), "image/png"),
                (header::CACHE_CONTROL.as_str(), "public, max-age=60"),
                (header::CACHE_CONTROL.as_str(), "no-store"),
            ],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/assets/logo.0123abcd.png")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = match run_publisher_proxy(&settings, &services, req).await {
            PublisherResponse::PassThrough { response, .. } => response,
            PublisherResponse::Buffered(response) | PublisherResponse::Stream { response, .. } => {
                response
            }
        };

        let cache_control_values = response
            .headers()
            .get_all(header::CACHE_CONTROL)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        assert_eq!(
            cache_control_values,
            vec!["public, max-age=60", "no-store"],
            "origin no-store in a later Cache-Control field should prevent normalized upgrade"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "origin no-store response must not receive edge-cache headers"
        );
    }

    #[tokio::test]
    async fn handle_publisher_request_does_not_self_generate_ec() {
        // EC generation is the adapter's real-browser-gated responsibility. This
        // handler must never mint an EC ID on its own: for a navigation from a
        // client the adapter did not pre-generate for (e.g. a non-real browser),
        // `ec_value` must stay `None` so no IP-derived identifier reaches the
        // auction. Consent allows EC creation and a client IP is present here —
        // exactly the conditions under which the old inline call would have
        // generated one.
        let settings = create_test_settings();
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"<html><body>ok</body></html>".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );

        let consent = crate::consent::ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..Default::default()
        };
        let mut ec_context =
            EcContext::new_for_test_with_ip(None, consent, Some("203.0.113.7".to_string()));
        assert!(
            ec_context.ec_allowed(),
            "test precondition: consent must allow EC creation"
        );

        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/article")
            .header(header::HOST, "publisher.example")
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build request");

        let _ = handle_publisher_request(
            &settings,
            &services,
            None,
            &mut ec_context,
            AuctionDispatch {
                orchestrator: &orchestrator,
                slots: &[],
                registry: None,
            },
            req,
            PublisherAdapterOptions {
                edge_cache_header: EdgeCacheHeader::SurrogateControl,
                delivery_compression: DeliveryCompressionCapability::Unavailable,
            },
        )
        .await
        .expect("should proxy publisher request");

        assert_eq!(
            ec_context.ec_value(),
            None,
            "handler must not self-generate an EC ID; generation is the adapter's real-browser-gated responsibility",
        );
    }

    #[test]
    fn response_carries_body_preserves_bodiless_metadata() {
        // A processable GET 200 buffers a body and recomputes Content-Length.
        assert!(
            super::response_carries_body(&Method::GET, StatusCode::OK),
            "a GET 200 publisher response should carry a buffered body"
        );
        // HEAD carries no body; recomputing Content-Length to 0 would mislead
        // clients/caches about the GET representation length.
        assert!(
            !super::response_carries_body(&Method::HEAD, StatusCode::OK),
            "HEAD publisher responses must not get a recomputed Content-Length"
        );
        // Bodiless statuses keep their metadata regardless of method.
        assert!(
            !super::response_carries_body(&Method::GET, StatusCode::NO_CONTENT),
            "204 responses must not get a recomputed Content-Length"
        );
        assert!(
            !super::response_carries_body(&Method::GET, StatusCode::NOT_MODIFIED),
            "304 responses must not get a recomputed Content-Length"
        );
    }

    fn response_body_string(response: http::Response<EdgeBody>) -> String {
        String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("response body should be valid UTF-8")
    }

    #[test]
    fn test_content_type_detection() {
        let test_cases = vec![
            ("text/html", true),
            ("text/html; charset=utf-8", true),
            ("Text/HTML; Charset=utf-8", true),
            ("text/css", true),
            ("Text/CSS", true),
            ("text/javascript", true),
            ("application/javascript", true),
            ("Application/JavaScript", true),
            ("application/json", true),
            ("application/json; charset=utf-8", true),
            ("Application/JSON; Charset=UTF-8", true),
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
    fn server_side_ad_stack_runs_only_when_all_auction_gates_pass() {
        assert!(
            should_run_server_side_ad_stack(true, true, false, false, true, true, true),
            "GET, real navigation, matched slots, and consent should run TS ad stack"
        );

        assert!(
            !should_run_server_side_ad_stack(false, true, false, false, true, true, true),
            "non-GET requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, false, false, false, true, true, true),
            "non-document requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, true, false, true, true, true),
            "prefetch requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, true, true, true, true),
            "bot requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, false, true, true),
            "requests with no matching slots should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, true, false, true),
            "requests without required consent should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, true, true, false),
            "disabled [auction].enabled kill switch should skip TS ad stack and injection"
        );
    }

    #[tokio::test]
    async fn ssat_late_binding_ignores_body_text_inside_script() {
        let settings = create_test_settings();
        let services = noop_services();
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let registry = IntegrationRegistry::empty_for_tests();
        let dispatched = DispatchedAuction::empty_for_test(test_auction_request(), 500);
        let mut params = make_html_ssat_params(&settings, Some(dispatched));
        let mut output = Vec::new();
        let html = r#"<html><head></head><body><script>const marker = "</body>";</script><p>painted</p></body></html>"#;

        stream_publisher_body_async(
            EdgeBody::from(html.as_bytes().to_vec()),
            &mut output,
            &mut params,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should stream HTML with parser-safe late binding");

        let rewritten = String::from_utf8(output).expect("should be utf8");
        assert!(
            rewritten.contains("const marker = \"</body>\";"),
            "script literal should remain script text"
        );
        let script_literal_pos = rewritten
            .find("const marker")
            .expect("should preserve script literal");
        let bids_pos = rewritten
            .find(".bids=JSON.parse")
            .expect("should inject bids script");
        let real_body_close_pos = rewritten.rfind("</body>").expect("should have body close");
        assert!(
            bids_pos > script_literal_pos,
            "script literal must not trigger early bid insertion"
        );
        assert!(
            bids_pos < real_body_close_pos,
            "bids must be injected before the real parser-confirmed body close"
        );
        assert!(
            !rewritten.contains("__TSJS_BIDS_PLACEHOLDER"),
            "placeholder must not leak to the client"
        );
    }

    #[tokio::test]
    async fn ssat_late_binding_appends_bids_at_eof_when_body_close_missing() {
        let settings = create_test_settings();
        let services = noop_services();
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let registry = IntegrationRegistry::empty_for_tests();
        let mut params = make_html_ssat_params(&settings, None);
        let mut output = Vec::new();
        let html = b"<html><head></head><body><p>painted</p>";

        stream_publisher_body_async(
            EdgeBody::from(html.to_vec()),
            &mut output,
            &mut params,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should append EOF fallback bids");

        let rewritten = String::from_utf8(output).expect("should be utf8");
        assert!(
            rewritten.contains(".bids=JSON.parse"),
            "missing body close should append bids at EOF"
        );
        assert!(
            !rewritten.contains("__TSJS_BIDS_PLACEHOLDER"),
            "placeholder must not leak on EOF fallback"
        );
    }

    #[tokio::test]
    async fn ssat_late_binding_prepends_bootstrap_when_body_closes_without_head() {
        let settings = create_test_settings();
        let services = noop_services();
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let registry = IntegrationRegistry::empty_for_tests();
        let mut params = make_html_ssat_params(&settings, None);
        let mut output = Vec::new();
        let html = b"<body><p>painted</p></body>";

        stream_publisher_body_async(
            EdgeBody::from(html.to_vec()),
            &mut output,
            &mut params,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should prepend missing-head bootstrap before body-close bids");

        let rewritten = String::from_utf8(output).expect("should be utf8");
        let slots_pos = rewritten
            .find(".adSlots=")
            .expect("should inject ad slot state");
        let bundle_pos = rewritten
            .find("/static/tsjs=")
            .expect("should inject tsjs script tag");
        let bids_pos = rewritten
            .find(".bids=JSON.parse")
            .expect("should inject bids script");
        let body_close_pos = rewritten.rfind("</body>").expect("should have body close");
        assert!(
            slots_pos < bundle_pos && bundle_pos < bids_pos && bids_pos < body_close_pos,
            "missing-head body-close replacement should preserve executable order before </body>"
        );
        assert!(
            !rewritten.contains("__TSJS_BIDS_PLACEHOLDER"),
            "placeholder must not leak on missing-head body-close replacement"
        );
    }

    #[tokio::test]
    async fn ssat_late_binding_appends_minimal_tail_when_head_and_body_close_missing() {
        let settings = create_test_settings();
        let services = noop_services();
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let registry = IntegrationRegistry::empty_for_tests();
        let mut params = make_html_ssat_params(&settings, None);
        let mut output = Vec::new();
        let html = b"<div>fragment without document scaffolding</div>";

        stream_publisher_body_async(
            EdgeBody::from(html.to_vec()),
            &mut output,
            &mut params,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should append missing-head EOF fallback tail");

        let rewritten = String::from_utf8(output).expect("should be utf8");
        let slots_pos = rewritten
            .find(".adSlots=")
            .expect("should append ad slot state");
        let bundle_pos = rewritten
            .find("/static/tsjs=")
            .expect("should append tsjs script tag");
        let bids_pos = rewritten
            .find(".bids=JSON.parse")
            .expect("should append bids script");
        assert!(
            slots_pos < bundle_pos && bundle_pos < bids_pos,
            "fallback tail should preserve executable order"
        );
        assert!(
            !rewritten.contains("__TSJS_BIDS_PLACEHOLDER"),
            "placeholder must not leak on missing-head fallback"
        );
    }

    #[tokio::test]
    async fn buffered_ssat_postprocessor_path_rejects_processed_output_over_cap() {
        let mut settings = create_test_settings();
        settings.publisher.max_buffered_body_bytes = 96;
        settings
            .integrations
            .insert_config(
                "nextjs",
                &serde_json::json!({
                    "enabled": true,
                    "rewrite_attributes": ["href"],
                }),
            )
            .expect("should update nextjs config");
        let services = noop_services();
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        assert!(
            registry.has_html_post_processors(),
            "nextjs should force the buffered post-processor path"
        );
        let mut params = make_html_ssat_params(&settings, None);
        let mut output = Vec::new();
        let html = b"<html><head></head><body>x</body></html>";
        assert!(
            html.len() < settings.publisher.max_buffered_body_bytes,
            "decoded input should fit so the processed-output cap is exercised"
        );

        let err = stream_publisher_body_async(
            EdgeBody::from(html.to_vec()),
            &mut output,
            &mut params,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect_err("should reject processed output over cap");

        let err = format!("{err:?}");
        assert!(
            err.contains("processed output exceeded"),
            "error should come from the processed-output cap: {err}"
        );
    }

    #[test]
    fn unsupported_encoding_response_is_returned_unmodified() {
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "zstd",
                "example.com"
            ),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn test_publisher_origin_host_extraction() {
        let settings = create_test_settings();
        let origin_host = settings.publisher.origin_host();
        assert_eq!(origin_host, "origin.test-publisher.com");

        let mut settings_with_port = create_test_settings();
        settings_with_port.publisher.origin_url = "origin.test-publisher.com:8080".to_string();
        assert_eq!(
            settings_with_port.publisher.origin_host(),
            "origin.test-publisher.com:8080"
        );
    }

    #[test]
    fn test_invalid_utf8_handling() {
        let invalid_utf8_bytes = vec![0xFF, 0xFE, 0xFD];
        assert!(String::from_utf8(invalid_utf8_bytes.clone()).is_err());
    }

    #[test]
    fn test_utf8_conversion_edge_cases() {
        let test_cases = vec![
            (vec![0xE2, 0x98, 0x83], true),
            (vec![0xF0, 0x9F, 0x98, 0x80], true),
            (vec![0xFF, 0xFE], false),
            (vec![0xC0, 0x80], false),
            (vec![0xED, 0xA0, 0x80], false),
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
    fn route_streams_2xx_html_without_post_processors() {
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "gzip",
                "example.com"
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_streams_mixed_case_html_content_type() {
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "Text/HTML; Charset=utf-8",
                "gzip",
                "example.com"
            ),
            ResponseRoute::Stream,
            "HTML MIME type matching must be case-insensitive",
        );
    }

    #[test]
    fn route_streams_html_with_post_processors() {
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "gzip",
                "example.com"
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_streams_non_html_even_with_post_processors_registered() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "application/json", "gzip", "example.com"),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_buffers_unmodified_on_unsupported_encoding() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "text/html", "zstd", "example.com"),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_passes_through_non_processable_2xx() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "image/png", "", "example.com"),
            ResponseRoute::PassThrough,
        );
    }

    #[test]
    fn route_buffers_non_processable_error_responses() {
        assert_eq!(
            classify_response_route(StatusCode::NOT_FOUND, "image/png", "", "example.com"),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_excludes_204_from_pass_through() {
        assert_eq!(
            classify_response_route(StatusCode::NO_CONTENT, "image/png", "", "example.com"),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_excludes_205_from_pass_through() {
        assert_eq!(
            classify_response_route(StatusCode::RESET_CONTENT, "image/png", "", "example.com"),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_excludes_204_for_processable_content_types() {
        assert_eq!(
            classify_response_route(
                StatusCode::NO_CONTENT,
                "text/html; charset=utf-8",
                "gzip",
                "example.com"
            ),
            ResponseRoute::BufferedUnmodified,
            "204 + HTML must not route to Stream",
        );
        assert_eq!(
            classify_response_route(
                StatusCode::NO_CONTENT,
                "text/html; charset=utf-8",
                "gzip",
                "example.com"
            ),
            ResponseRoute::BufferedUnmodified,
            "204 + HTML + post-processors must not route to Stream",
        );
    }

    #[test]
    fn route_excludes_205_for_processable_content_types() {
        assert_eq!(
            classify_response_route(
                StatusCode::RESET_CONTENT,
                "application/json",
                "",
                "example.com"
            ),
            ResponseRoute::BufferedUnmodified,
            "205 + JSON must not route to Stream",
        );
    }

    #[test]
    fn route_streams_non_2xx_processable_content() {
        assert_eq!(
            classify_response_route(
                StatusCode::NOT_FOUND,
                "text/html; charset=utf-8",
                "gzip",
                "example.com"
            ),
            ResponseRoute::Stream,
        );
        assert_eq!(
            classify_response_route(
                StatusCode::INTERNAL_SERVER_ERROR,
                "application/json",
                "gzip",
                "example.com"
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_streams_non_2xx_html_with_post_processors() {
        assert_eq!(
            classify_response_route(
                StatusCode::NOT_FOUND,
                "text/html; charset=utf-8",
                "gzip",
                "example.com"
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_passes_through_non_processable_even_with_empty_request_host() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "image/png", "", ""),
            ResponseRoute::PassThrough,
        );
    }

    #[test]
    fn route_buffers_processable_content_with_empty_request_host() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "text/html", "gzip", ""),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn pass_through_preserves_body_and_content_length() {
        let image_bytes: Vec<u8> = (0..=255).cycle().take(4096).collect();

        let mut response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CONTENT_LENGTH, image_bytes.len() as u64)
            .body(EdgeBody::from(image_bytes.clone()))
            .expect("should build test response");

        // Simulate PassThrough: take body then reattach
        let body = std::mem::replace(response.body_mut(), EdgeBody::empty());
        // Body is unmodified — Content-Length stays correct
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .expect("should have content-length"),
            "4096",
            "Content-Length should be preserved for pass-through"
        );

        // Reattach and verify body content
        *response.body_mut() = body;
        let (_, final_body) = response.into_parts();
        let output = final_body.into_bytes().unwrap_or_default();
        assert_eq!(
            output, image_bytes,
            "pass-through should preserve body byte-for-byte"
        );
    }

    #[test]
    fn test_content_encoding_detection() {
        let test_encodings = vec!["gzip", "deflate", "br", "identity", ""];

        for encoding in test_encodings {
            let mut req = build_request(Method::GET, "https://test.example.com/page");
            req.headers_mut().insert(
                header::ACCEPT_ENCODING,
                http::HeaderValue::from_static("gzip, deflate, br"),
            );

            if !encoding.is_empty() {
                req.headers_mut().insert(
                    header::CONTENT_ENCODING,
                    http::HeaderValue::from_str(encoding)
                        .expect("content encoding should be valid"),
                );
            }

            let content_encoding = req
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(|h| h.to_str().unwrap_or_default())
                .unwrap_or_default();

            assert_eq!(content_encoding, encoding);
        }
    }

    #[test]
    fn publisher_proxy_does_not_add_accept_encoding_when_absent() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        // No Accept-Encoding header set by the client.

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.headers().get(header::ACCEPT_ENCODING),
            None,
            "publisher proxy should not inject Accept-Encoding when the client sent none"
        );
    }

    #[test]
    fn publisher_proxy_limits_accept_encoding_to_supported_values() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        req.headers_mut().insert(
            header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("gzip, deflate, br, zstd"),
        );

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.headers()
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip, deflate, br"),
            "publisher fallback should only advertise encodings the rewrite pipeline supports"
        );
    }

    #[test]
    fn publisher_proxy_preserves_identity_only_accept_encoding() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        req.headers_mut().insert(
            header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("identity"),
        );

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.headers()
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("identity"),
            "publisher fallback should preserve identity-only clients"
        );
    }

    #[test]
    fn publisher_proxy_respects_supported_client_subset() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        req.headers_mut().insert(
            header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("br, gzip;q=0, zstd"),
        );

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.headers()
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("br"),
            "publisher fallback should only advertise the supported encodings the client accepts"
        );
    }

    #[test]
    fn publisher_proxy_falls_back_to_identity_for_unsupported_client_encodings() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        req.headers_mut().insert(
            header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("zstd"),
        );

        restrict_accept_encoding(&mut req);

        assert_eq!(
            req.headers()
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("identity"),
            "publisher fallback should request identity when the client only accepts unsupported encodings"
        );
    }

    #[test]
    fn tsjs_dynamic_returns_not_found_for_unknown_filename() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=unknown.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn tsjs_dynamic_serves_unified_bundle_for_known_filename() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-unified.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn tsjs_dynamic_uses_immutable_cache_for_matching_hash() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let module_ids = registry.js_module_ids_immediate();
        let hash = trusted_server_js::concatenated_hash(&module_ids);
        let req = build_request(
            Method::GET,
            &format!("https://publisher.example/static/tsjs=tsjs-unified.min.js?v={hash}"),
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=31536000, immutable"),
            "should make matching content-versioned bundle immutable"
        );
        assert_eq!(
            response
                .headers()
                .get("surrogate-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000"),
            "should give Fastly edge cache the same immutable TTL"
        );
        assert_eq!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|value| value.to_str().ok()),
            Some("Accept-Encoding"),
            "should keep encoding in the cache key"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_COMPRESS_HINT)
                .and_then(|value| value.to_str().ok()),
            Some("on"),
            "should keep Fastly delivery compression hint"
        );
    }

    #[test]
    fn tsjs_dynamic_uses_cloudflare_edge_header_when_selected() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let module_ids = registry.js_module_ids_immediate();
        let hash = trusted_server_js::concatenated_hash(&module_ids);
        let req = build_request(
            Method::GET,
            &format!("https://publisher.example/static/tsjs=tsjs-unified.min.js?v={hash}"),
        );

        let response =
            handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::CloudflareCdnCacheControl)
                .expect("should handle tsjs request");

        assert_eq!(
            response
                .headers()
                .get("cloudflare-cdn-cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000"),
            "should render Cloudflare-specific edge cache header"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "Cloudflare responses should not emit Fastly Surrogate-Control"
        );
    }

    #[test]
    fn tsjs_dynamic_keeps_short_cache_for_mismatched_hash() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-unified.min.js?v=not-the-hash",
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .expect("should set cache-control");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            cache_control.contains("max-age=300"),
            "should keep short browser TTL for mismatched hash"
        );
        assert!(
            !cache_control.contains("immutable"),
            "should not make mismatched hash requests immutable"
        );
        assert_eq!(
            response
                .headers()
                .get("surrogate-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "should keep short edge TTL for mismatched hash"
        );
    }

    #[test]
    fn parse_deferred_module_filename_extracts_known_id() {
        assert_eq!(
            parse_deferred_module_filename("tsjs-sourcepoint.min.js"),
            Some("sourcepoint"),
            "should extract sourcepoint from minified filename"
        );
        assert_eq!(
            parse_deferred_module_filename("tsjs-sourcepoint.js"),
            Some("sourcepoint"),
            "should extract sourcepoint from unminified filename"
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
            parse_deferred_module_filename("tsjs-sourcepoint.txt"),
            None,
            "should reject non-js extension"
        );
    }

    #[test]
    fn tsjs_dynamic_does_not_serve_embedded_prebid() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-prebid.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should not serve embedded prebid module"
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
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                }),
            )
            .expect("should update prebid config");
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-prebid.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should return 404 for disabled deferred module"
        );
    }

    #[test]
    fn tsjs_dynamic_returns_not_found_for_arbitrary_module_name() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-evil.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should reject unknown module names"
        );
    }

    #[tokio::test]
    async fn publisher_request_sends_configured_host_header_override() {
        let mut settings = create_test_settings();
        settings.publisher.origin_host_header_override = Some("www.example.com".to_string());
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"origin response".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/page")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build request");

        let _ = run_publisher_proxy(&settings, &services, req).await;

        let recorded_headers = stub.recorded_request_headers();
        let outbound_headers = recorded_headers
            .first()
            .expect("should record one outbound request");
        let outbound_host = outbound_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.as_str());

        assert_eq!(
            outbound_host,
            Some("www.example.com"),
            "should send configured host override to outbound request"
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

        let body = EdgeBody::from(compressed);
        let params = OwnedProcessResponseParams {
            input_compression: Compression::Gzip,
            output_compression: Compression::Gzip,
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/css".to_string(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
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
            input_compression: Compression::None,
            output_compression: Compression::None,
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
        };

        let mut output = Vec::new();
        stream_publisher_body(
            EdgeBody::empty(),
            &mut output,
            &params,
            &settings,
            &registry,
        )
        .expect("should succeed on empty body");

        assert!(
            output.is_empty(),
            "empty origin body should produce empty streaming output. Got: {output:?}"
        );
    }

    #[test]
    fn stream_publisher_body_treats_mixed_case_html_as_html() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let bids_script =
            r#"<script>(window.tsjs=window.tsjs||{}).bids=JSON.parse("{}");</script>"#;
        let state = Arc::new(Mutex::new(Some(bids_script.to_string())));
        let params = OwnedProcessResponseParams {
            input_compression: Compression::None,
            output_compression: Compression::None,
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "Text/HTML; Charset=utf-8".to_string(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=JSON.parse("[]");</script>"#
                    .to_string(),
            ),
            ad_bids_state: state,
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
        };
        let mut output = Vec::new();

        stream_publisher_body(
            EdgeBody::from(b"<html><head></head><body>content</body></html>".to_vec()),
            &mut output,
            &params,
            &settings,
            &registry,
        )
        .expect("should process mixed-case HTML content type");

        let html = String::from_utf8(output).expect("should be valid UTF-8");
        assert!(
            html.contains(".adSlots=JSON.parse"),
            "mixed-case HTML must use the HTML processor and inject ad slots. Got: {html}"
        );
        assert!(
            html.contains(".bids=JSON.parse"),
            "mixed-case HTML must use the HTML processor and inject bids. Got: {html}"
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
            input_compression: Compression::Gzip,
            output_compression: Compression::Gzip,
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html".to_string(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
        };

        let bogus_body = EdgeBody::from(b"<html>not gzip</html>".to_vec());
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

        let mut response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CONTENT_LENGTH, image_bytes.len() as u64)
            .body(EdgeBody::from(image_bytes.clone()))
            .expect("should build test response");

        // Mirror adapter: take body, then reattach.
        let body = std::mem::replace(response.body_mut(), EdgeBody::empty());
        *response.body_mut() = body;

        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .expect("content-length should survive"),
            "2048"
        );
        let (_, final_body) = response.into_parts();
        let round_trip = final_body.into_bytes().unwrap_or_default();
        assert_eq!(
            round_trip, image_bytes,
            "pass-through reattach must preserve bytes exactly"
        );
    }

    /// Streaming dispatch contract: HTML with a registered post-processor still
    /// routes through `Stream`, and the shared processor pipeline still applies
    /// the post-processor rewrite.
    #[test]
    fn streaming_html_with_post_processors_rewrites_body() {
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
            ),
            ResponseRoute::Stream,
            "HTML with post-processors must route to Stream"
        );

        // Feed a small HTML body through the same pipeline the Stream arm uses.
        let html =
            b"<html><body><a href=\"https://origin.example.com/page\">link</a></body></html>";
        let body = EdgeBody::from(html.to_vec());

        let params = OwnedProcessResponseParams {
            input_compression: Compression::None,
            output_compression: Compression::None,
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
        };
        let mut output = Vec::new();
        stream_publisher_body(body, &mut output, &params, &settings, &registry)
            .expect("should process streaming HTML");

        assert!(
            !output.is_empty(),
            "streaming processed output must not be empty"
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
            input_compression: Compression::None,
            output_compression: Compression::None,
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html".to_string(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
        };

        let mut output = Vec::new();
        stream_publisher_body(
            EdgeBody::from(html.to_vec()),
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

    #[cfg(test)]
    mod creative_opportunities_tests {
        use super::super::{
            build_ad_slots_script, build_auction_request, build_bid_map, build_bids_script,
            html_escape_for_script, MatchedSlotsContext,
        };
        use crate::auction::types::{Bid, MediaType};
        use crate::consent::ConsentContext;
        use crate::creative_opportunities::{
            CreativeOpportunitiesConfig, CreativeOpportunityFormat, CreativeOpportunitySlot,
        };
        use crate::http_util::RequestInfo;
        use crate::price_bucket::PriceGranularity;
        use std::collections::HashMap;

        fn make_config() -> CreativeOpportunitiesConfig {
            CreativeOpportunitiesConfig {
                gam_network_id: "21765378893".to_string(),
                auction_timeout_ms: Some(500),
                price_granularity: PriceGranularity::Dense,
                slot: Vec::new(),
            }
        }

        fn make_slot() -> CreativeOpportunitySlot {
            CreativeOpportunitySlot {
                id: "atf_sidebar_ad".to_string(),
                gam_unit_path: Some("/21765378893/publisher/atf-sidebar".to_string()),
                div_id: Some("div-atf-sidebar".to_string()),
                page_patterns: vec!["/20**".to_string()],
                formats: vec![CreativeOpportunityFormat {
                    width: 300,
                    height: 250,
                    media_type: MediaType::Banner,
                }],
                floor_price: Some(0.50),
                targeting: [("pos".to_string(), "atf".to_string())]
                    .into_iter()
                    .collect(),
                providers: Default::default(),
                compiled_patterns: Vec::new(),
            }
        }

        fn make_bid(
            slot_id: &str,
            price: f64,
            bidder: &str,
            ad_id: &str,
            nurl: &str,
            burl: &str,
        ) -> Bid {
            Bid {
                slot_id: slot_id.to_string(),
                price: Some(price),
                currency: "USD".to_string(),
                creative: None,
                adomain: None,
                bidder: bidder.to_string(),
                width: 300,
                height: 250,
                nurl: Some(nurl.to_string()),
                burl: Some(burl.to_string()),
                ad_id: Some(ad_id.to_string()),
                cache_id: None,
                cache_host: None,
                cache_path: None,
                metadata: Default::default(),
            }
        }

        #[test]
        fn ad_slots_script_contains_slot_data() {
            let slots = vec![make_slot()];
            let config = make_config();
            let script = build_ad_slots_script(&slots, &config);
            assert!(
                script.contains("window.tsjs=window.tsjs||{}"),
                "should initialise tsjs namespace"
            );
            assert!(
                script.contains(".adSlots=JSON.parse"),
                "should use JSON.parse for adSlots"
            );
            assert!(script.contains("atf_sidebar_ad"), "should include slot id");
            assert!(!script.contains("adInit"), "must NOT contain adInit");
            assert!(
                !script.contains("__ts_request_id"),
                "must NOT contain request_id"
            );
        }

        #[test]
        fn ad_slots_script_is_xss_safe() {
            let slots = vec![make_slot()];
            let config = make_config();
            let script = build_ad_slots_script(&slots, &config);
            let inner = script
                .trim_start_matches("<script>")
                .trim_end_matches("</script>");
            assert!(!inner.contains('<'), "no unescaped < in script content");
            assert!(!inner.contains('>'), "no unescaped > in script content");
        }

        #[test]
        fn bid_map_includes_nurl_and_burl() {
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "atf_sidebar_ad".to_string(),
                make_bid(
                    "atf_sidebar_ad",
                    1.50,
                    "kargo",
                    "abc123",
                    "https://ssp/win",
                    "https://ssp/bill",
                ),
            );
            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, false);
            let entry = map.get("atf_sidebar_ad").expect("should have bid entry");
            let obj = entry.as_object().expect("should be object");
            assert_eq!(
                obj.get("hb_pb").and_then(|v| v.as_str()),
                Some("1.50"),
                "should bucket price with dense granularity"
            );
            assert_eq!(
                obj.get("hb_bidder").and_then(|v| v.as_str()),
                Some("kargo"),
                "should include bidder"
            );
            assert_eq!(
                obj.get("hb_adid").and_then(|v| v.as_str()),
                Some("abc123"),
                "should fall back to ad_id when no cache_id present"
            );
            assert_eq!(
                obj.get("nurl").and_then(|v| v.as_str()),
                Some("https://ssp/win"),
                "should include nurl"
            );
            assert_eq!(
                obj.get("burl").and_then(|v| v.as_str()),
                Some("https://ssp/bill"),
                "should include burl"
            );
        }

        #[test]
        fn client_bid_map_omits_adm_by_default() {
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.creative = Some("<div>Creative</div>".to_string());
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, false);
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");

            assert!(
                obj.get("adm").is_none(),
                "should omit adm when debug injection is disabled"
            );
            assert!(
                obj.get("debug_bid").is_none(),
                "should omit debug bid when debug injection is disabled"
            );
        }

        #[test]
        fn client_bid_map_includes_adm_when_debug_injection_enabled() {
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.creative = Some("<div>Creative</div>".to_string());
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, true);
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");

            assert_eq!(
                obj.get("adm").and_then(|v| v.as_str()),
                Some("<div>Creative</div>"),
                "should include adm when debug injection is enabled"
            );
        }

        #[test]
        fn client_bid_map_includes_debug_bid_when_debug_injection_enabled() {
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "mocktioneer",
                "bid-ad-id",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.creative = Some("<div>Creative</div>".to_string());
            bid.adomain = Some(vec!["example.com".to_string()]);
            bid.cache_id = Some("cache-uuid".to_string());
            bid.cache_host = Some("cache.example".to_string());
            bid.cache_path = Some("/cache".to_string());
            bid.metadata.insert(
                "raw_field".to_string(),
                serde_json::Value::String("raw-value".to_string()),
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, true);
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");
            let debug_bid = obj
                .get("debug_bid")
                .and_then(|v| v.as_object())
                .expect("should include debug bid when debug injection is enabled");

            assert_eq!(
                debug_bid.get("slot_id").and_then(|v| v.as_str()),
                Some("atf_sidebar_ad"),
                "should expose original slot id"
            );
            assert_eq!(
                debug_bid.get("bidder").and_then(|v| v.as_str()),
                Some("mocktioneer"),
                "should expose original bidder"
            );
            assert_eq!(
                debug_bid.get("ad_id").and_then(|v| v.as_str()),
                Some("bid-ad-id"),
                "should expose original bid ad id"
            );
            assert_eq!(
                debug_bid.get("cache_id").and_then(|v| v.as_str()),
                Some("cache-uuid"),
                "should expose original PBS cache id"
            );
            assert_eq!(
                debug_bid.get("metadata").and_then(|v| v.get("raw_field")),
                Some(&serde_json::Value::String("raw-value".to_string())),
                "should expose provider metadata"
            );
        }

        #[test]
        fn bid_map_uses_cache_id_for_hb_adid_when_present() {
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "atf_sidebar_ad".to_string(),
                Bid {
                    slot_id: "atf_sidebar_ad".to_string(),
                    price: Some(1.50),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "thetradedesk".to_string(),
                    width: 300,
                    height: 250,
                    nurl: None,
                    burl: None,
                    ad_id: Some("bid-impression-id".to_string()),
                    cache_id: Some("f47447a0-b759-4f2f-9887-af458b79b570".to_string()),
                    cache_host: Some("openads.adsrvr.org".to_string()),
                    cache_path: Some("/cache".to_string()),
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, false);
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");
            assert_eq!(
                obj.get("hb_adid").and_then(|v| v.as_str()),
                Some("f47447a0-b759-4f2f-9887-af458b79b570"),
                "should use cache_id for hb_adid, not ad_id"
            );
            assert_eq!(
                obj.get("hb_cache_host").and_then(|v| v.as_str()),
                Some("openads.adsrvr.org"),
                "should emit hb_cache_host"
            );
            assert_eq!(
                obj.get("hb_cache_path").and_then(|v| v.as_str()),
                Some("/cache"),
                "should emit hb_cache_path"
            );
        }

        #[test]
        fn bid_map_falls_back_to_ad_id_when_cache_id_absent() {
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "atf_sidebar_ad".to_string(),
                Bid {
                    slot_id: "atf_sidebar_ad".to_string(),
                    price: Some(0.50),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "amazon-aps".to_string(),
                    width: 300,
                    height: 250,
                    nurl: None,
                    burl: None,
                    ad_id: Some("aps-bid-token".to_string()),
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, false);
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");
            assert_eq!(
                obj.get("hb_adid").and_then(|v| v.as_str()),
                Some("aps-bid-token"),
                "should fall back to ad_id when cache_id absent"
            );
            assert!(
                obj.get("hb_cache_host").is_none(),
                "should not emit hb_cache_host when absent"
            );
            assert!(
                obj.get("hb_cache_path").is_none(),
                "should not emit hb_cache_path when absent"
            );
        }

        #[test]
        fn bid_map_omits_hb_adid_when_both_cache_id_and_ad_id_absent() {
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "atf_sidebar_ad".to_string(),
                Bid {
                    slot_id: "atf_sidebar_ad".to_string(),
                    price: Some(0.50),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "amazon-aps".to_string(),
                    width: 300,
                    height: 250,
                    nurl: None,
                    burl: None,
                    ad_id: None,
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, false);
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");
            assert!(
                obj.get("hb_adid").is_none(),
                "should omit hb_adid when no cache_id and no ad_id"
            );
        }

        #[test]
        fn bid_map_excludes_slot_when_price_is_none() {
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "no-price-slot".to_string(),
                Bid {
                    slot_id: "no-price-slot".to_string(),
                    price: None,
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "kargo".to_string(),
                    width: 300,
                    height: 250,
                    nurl: None,
                    burl: None,
                    ad_id: None,
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, false);
            assert!(
                map.is_empty(),
                "slot with no price should be excluded from bid map"
            );
        }

        #[test]
        fn bids_script_is_xss_safe() {
            let mut map = serde_json::Map::new();
            map.insert("atf".to_string(), serde_json::json!({"hb_pb": "1.00"}));
            let script = build_bids_script(&map);
            let inner = script
                .trim_start_matches("<script>")
                .trim_end_matches("</script>");
            assert!(!inner.contains('<'), "no unescaped < in bids script");
            assert!(!inner.contains('>'), "no unescaped > in bids script");
        }

        #[test]
        fn bids_script_calls_ad_init_without_retry_timer() {
            let mut map = serde_json::Map::new();
            map.insert("atf".to_string(), serde_json::json!({"hb_pb": "1.00"}));

            let script = build_bids_script(&map);

            assert!(
                script.contains("window.tsjs.adInit"),
                "should hand off bids to adInit"
            );
            assert!(
                !script.contains("setTimeout"),
                "should not retry adInit on a timer"
            );
            assert!(
                !script.contains("prevGptSlots"),
                "should not use TS-owned slots as adInit success signal"
            );
        }

        #[test]
        fn auction_request_without_ec_id_omits_user_id_and_uses_non_ec_request_id() {
            let slot = make_slot();
            let slots = [slot];
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &slots,
                request_path: "/2024/01/my-article/",
            };
            let request_info = RequestInfo {
                host: "publisher.example.com".to_string(),
                scheme: "https".to_string(),
            };

            let request = build_auction_request(
                &slots_ctx,
                None,
                &ConsentContext::default(),
                &request_info,
                Some("Mozilla/5.0"),
            );

            assert_eq!(request.user.id, None, "should not forward an EC user id");
            assert!(
                request.id.starts_with("ts-req-"),
                "should use a non-EC request id, got {}",
                request.id
            );
        }

        #[test]
        fn auction_request_with_ec_id_sets_user_id_and_ec_request_id() {
            let slot = make_slot();
            let slots = [slot];
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &slots,
                request_path: "/2024/01/my-article/",
            };
            let request_info = RequestInfo {
                host: "publisher.example.com".to_string(),
                scheme: "https".to_string(),
            };

            let request = build_auction_request(
                &slots_ctx,
                Some("ec-abc"),
                &ConsentContext::default(),
                &request_info,
                Some("Mozilla/5.0"),
            );

            assert_eq!(
                request.user.id.as_deref(),
                Some("ec-abc"),
                "should forward EC id when identity consent allows it"
            );
            assert_eq!(
                request.id, "ts-ec-abc",
                "should preserve existing EC-derived request id when present"
            );
        }

        #[test]
        fn html_escape_encodes_special_chars() {
            assert_eq!(
                html_escape_for_script("text\\with\\backslash"),
                "text\\\\with\\\\backslash",
                "should escape backslashes"
            );
            assert_eq!(
                html_escape_for_script("string\"with\"quotes"),
                "string\\\"with\\\"quotes",
                "should escape quotes"
            );
            assert_eq!(
                html_escape_for_script("simple"),
                "simple",
                "should not change simple text"
            );
            assert_eq!(
                html_escape_for_script("both\\\"mixed"),
                "both\\\\\\\"mixed",
                "should escape both backslashes and quotes"
            );
            assert_eq!(
                html_escape_for_script("<script>alert(1)</script>"),
                "\\u003Cscript\\u003Ealert(1)\\u003C/script\\u003E",
                "should unicode-escape angle brackets to prevent script injection"
            );
            assert_eq!(
                html_escape_for_script("a&b"),
                "a\\u0026b",
                "should unicode-escape ampersand"
            );
            assert_eq!(
                html_escape_for_script("line\u{2028}sep"),
                "line\\u2028sep",
                "should unicode-escape U+2028 line separator"
            );
            assert_eq!(
                html_escape_for_script("para\u{2029}sep"),
                "para\\u2029sep",
                "should unicode-escape U+2029 paragraph separator"
            );
        }
    }

    mod page_bids_no_match_tests {
        use super::super::*;
        use crate::auction::AuctionOrchestrator;
        use crate::creative_opportunities::{CreativeOpportunityFormat, CreativeOpportunitySlot};
        use crate::platform::test_support::noop_services;
        use crate::test_support::tests::crate_test_settings_str;
        use http::Method;

        fn settings_with_co() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n[creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with creative_opportunities")
        }

        fn settings_with_co_auction_disabled() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = false\n\n[creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with creative_opportunities")
        }

        async fn run_page_bids(
            settings: &Settings,
            orchestrator: &AuctionOrchestrator,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
        ) -> serde_json::Value {
            let response = run_page_bids_response(settings, orchestrator, slots, req).await;
            serde_json::from_slice(&response.into_body().into_bytes().unwrap_or_default())
                .expect("should be json")
        }

        /// `run_page_bids` with an EC context whose jurisdiction allows the
        /// server-side auction, so slot-counting tests isolate the variable
        /// under test (bot/prefetch) from the consent gate. The default
        /// request resolves to `Jurisdiction::Unknown`, which fails the
        /// consent gate and now suppresses slots.
        async fn run_page_bids_consent_allowed(
            settings: &Settings,
            orchestrator: &AuctionOrchestrator,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
        ) -> serde_json::Value {
            let ec_context = consent_allowing_ec_context();
            let response =
                run_page_bids_response_with_ec(settings, orchestrator, slots, &ec_context, req)
                    .await;
            serde_json::from_slice(&response.into_body().into_bytes().unwrap_or_default())
                .expect("should be json")
        }

        /// Builds an [`EcContext`] whose consent context permits the server-side
        /// auction (known non-GDPR jurisdiction, no EU TCF signal).
        fn consent_allowing_ec_context() -> EcContext {
            let consent = crate::consent::ConsentContext {
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                ..Default::default()
            };
            EcContext::new_for_test(None, consent)
        }

        fn article_slot() -> Vec<CreativeOpportunitySlot> {
            vec![CreativeOpportunitySlot {
                id: "atf".to_string(),
                gam_unit_path: None,
                div_id: None,
                page_patterns: vec!["/20**".to_string()],
                formats: vec![CreativeOpportunityFormat {
                    width: 300,
                    height: 250,
                    media_type: crate::auction::types::MediaType::Banner,
                }],
                floor_price: Some(0.50),
                targeting: Default::default(),
                providers: Default::default(),
                compiled_patterns: Vec::new(),
            }]
        }

        fn make_page_bids_request(path: &str) -> Request<EdgeBody> {
            let mut req = Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "https://test-publisher.com/_ts/page-bids?path={path}"
                ))
                .body(EdgeBody::empty())
                .expect("should build test request");
            // Pass the same-origin gate the way a browser fetch from the
            // publisher page does.
            set_test_header(&mut req, "sec-fetch-site", "same-origin");
            req
        }

        fn set_test_header(req: &mut Request<EdgeBody>, name: &'static str, value: &'static str) {
            req.headers_mut().insert(
                header::HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }

        async fn run_page_bids_response(
            settings: &Settings,
            orchestrator: &AuctionOrchestrator,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
        ) -> Response<EdgeBody> {
            let ec_context = EcContext::read_from_request(settings, &req, &noop_services())
                .expect("should read EC context");
            run_page_bids_response_with_ec(settings, orchestrator, slots, &ec_context, req).await
        }

        async fn run_page_bids_response_with_ec(
            settings: &Settings,
            orchestrator: &AuctionOrchestrator,
            slots: &[CreativeOpportunitySlot],
            ec_context: &EcContext,
            req: Request<EdgeBody>,
        ) -> Response<EdgeBody> {
            let services = noop_services();
            handle_page_bids(
                settings,
                &services,
                None,
                AuctionDispatch {
                    orchestrator,
                    slots,
                    registry: None,
                },
                ec_context,
                req,
            )
            .await
            .expect("should return ok response")
        }

        #[tokio::test]
        async fn cross_site_fetch_metadata_is_rejected() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let mut req = make_page_bids_request("/2024/01/my-article/");
            set_test_header(&mut req, "sec-fetch-site", "cross-site");

            let response =
                run_page_bids_response(&settings, &orchestrator, &article_slot(), req).await;

            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "cross-site request should be rejected before any auction work"
            );
        }

        #[tokio::test]
        async fn missing_fetch_metadata_without_tsjs_header_is_rejected() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let mut req = make_page_bids_request("/2024/01/my-article/");
            req.headers_mut().remove("sec-fetch-site");

            let response =
                run_page_bids_response(&settings, &orchestrator, &article_slot(), req).await;

            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "request with neither fetch metadata nor tsjs header should be rejected"
            );
        }

        #[tokio::test]
        async fn missing_fetch_metadata_with_tsjs_header_is_allowed() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let mut req = make_page_bids_request("/2024/01/my-article/");
            req.headers_mut().remove("sec-fetch-site");
            set_test_header(&mut req, "x-tsjs-page-bids", "1");

            let response =
                run_page_bids_response(&settings, &orchestrator, &article_slot(), req).await;

            assert_eq!(
                response.status(),
                StatusCode::OK,
                "legacy client carrying the tsjs header should pass the gate"
            );
        }

        #[tokio::test]
        async fn same_site_fetch_metadata_is_rejected() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let mut req = make_page_bids_request("/2024/01/my-article/");
            // `same-site` admits sibling origins under the same registrable
            // domain — not trusted to spend SSP quota.
            set_test_header(&mut req, "sec-fetch-site", "same-site");

            let response =
                run_page_bids_response(&settings, &orchestrator, &article_slot(), req).await;

            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "same-site request should be rejected; only same-origin is trusted"
            );
        }

        #[tokio::test]
        async fn empty_slots_file_returns_empty_slots_and_bids() {
            // Spec §8 kill-switch: creative-opportunities.toml with zero slots disables
            // all server-side auction activity and injection.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let req = make_page_bids_request("/2024/01/my-article/");

            let body = run_page_bids(&settings, &orchestrator, &[], req).await;

            assert_eq!(
                body["slots"]
                    .as_array()
                    .expect("slots should be array")
                    .len(),
                0,
                "empty slots should produce zero injected slots"
            );
            assert_eq!(
                body["bids"]
                    .as_object()
                    .expect("bids should be object")
                    .len(),
                0,
                "empty slots should produce zero bids"
            );
        }

        #[tokio::test]
        async fn bot_user_agent_returns_slots_but_no_bids() {
            // Crawlers should get slot definitions (so HTML structure is unchanged)
            // but the server must not burn SSP request quota running a real auction
            // for them. Same gate the publisher path applies.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let mut req = make_page_bids_request("/2024/01/my-article/");
            set_test_header(
                &mut req,
                "user-agent",
                "Mozilla/5.0 (compatible; Googlebot/2.1)",
            );

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;

            assert_eq!(
                body["slots"]
                    .as_array()
                    .expect("slots should be array")
                    .len(),
                1,
                "bot request should still get slot definitions"
            );
            assert_eq!(
                body["bids"]
                    .as_object()
                    .expect("bids should be object")
                    .len(),
                0,
                "bot request must not run an auction (no SSP cost burned for crawlers)"
            );
        }

        #[tokio::test]
        async fn prefetch_request_returns_slots_but_no_bids() {
            // Navigations triggered by Sec-Purpose=prefetch should not fire real
            // SSP auctions — the user has not yet visited the page.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let mut req = make_page_bids_request("/2024/01/my-article/");
            set_test_header(&mut req, "sec-purpose", "prefetch");

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;

            assert_eq!(
                body["slots"]
                    .as_array()
                    .expect("slots should be array")
                    .len(),
                1,
                "prefetch request should still get slot definitions"
            );
            assert_eq!(
                body["bids"]
                    .as_object()
                    .expect("bids should be object")
                    .len(),
                0,
                "prefetch request must not run an auction"
            );
        }

        #[tokio::test]
        async fn url_not_matching_any_pattern_returns_empty_response() {
            // Slots exist but request path does not match — no auction, no injection.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot(); // slot matches /20** only
            let req = make_page_bids_request("/about"); // does not match

            let body = run_page_bids(&settings, &orchestrator, &slots, req).await;

            assert_eq!(
                body["slots"]
                    .as_array()
                    .expect("slots should be array")
                    .len(),
                0,
                "non-matching URL should produce zero injected slots"
            );
            assert_eq!(
                body["bids"]
                    .as_object()
                    .expect("bids should be object")
                    .len(),
                0,
                "non-matching URL should produce zero bids"
            );
        }

        #[test]
        fn normalize_page_bids_path_strips_query_fragment_and_forces_leading_slash() {
            assert_eq!(
                normalize_page_bids_path("/2024/01/article/"),
                "/2024/01/article/",
                "canonical path should pass through unchanged"
            );
            assert_eq!(
                normalize_page_bids_path("/2024/01/article/?utm_source=x"),
                "/2024/01/article/",
                "query string should be stripped before glob matching"
            );
            assert_eq!(
                normalize_page_bids_path("/2024/01/article/#section"),
                "/2024/01/article/",
                "fragment should be stripped before glob matching"
            );
            assert_eq!(
                normalize_page_bids_path("2024/01/article/"),
                "/2024/01/article/",
                "missing leading slash should be added"
            );
            assert_eq!(
                normalize_page_bids_path(""),
                "/",
                "empty path should normalize to root"
            );
        }

        #[tokio::test]
        async fn disabled_auction_returns_no_slots_or_bids() {
            // [auction].enabled = false is a global kill switch: it must disable
            // the entire server-side ad stack, not just SSP calls. Returning slot
            // definitions would let the SPA hook assign `ts.adSlots` and call
            // `adInit()`, creating/refreshing GPT slots client-side even though
            // the auction is off. Consent is allowed here so the test isolates
            // the kill switch.
            let settings = settings_with_co_auction_disabled();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let req = make_page_bids_request("/2024/01/my-article/");

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;

            assert_eq!(
                body["slots"]
                    .as_array()
                    .expect("slots should be array")
                    .len(),
                0,
                "disabled auction must not return slot definitions (kill switch stops the ad stack)"
            );
            assert_eq!(
                body["bids"]
                    .as_object()
                    .expect("bids should be object")
                    .len(),
                0,
                "disabled auction must not produce bids"
            );
        }

        #[tokio::test]
        async fn consent_denied_returns_no_slots_or_bids() {
            // When consent denies the server-side auction (here: Jurisdiction
            // Unknown fails closed), the endpoint must return no slots so the SPA
            // hook does not create GPT slots client-side — matching the publisher
            // navigation path's `should_run_server_side_ad_stack` gate.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let req = make_page_bids_request("/2024/01/my-article/");

            // run_page_bids uses the default EC context, which resolves to
            // Jurisdiction::Unknown (consent denied).
            let body = run_page_bids(&settings, &orchestrator, &slots, req).await;

            assert_eq!(
                body["slots"]
                    .as_array()
                    .expect("slots should be array")
                    .len(),
                0,
                "consent denial must suppress slot definitions"
            );
            assert_eq!(
                body["bids"]
                    .as_object()
                    .expect("bids should be object")
                    .len(),
                0,
                "consent denial must produce no bids"
            );
        }
    }

    #[test]
    fn bounded_writer_accepts_writes_within_limit() {
        let mut writer = BoundedWriter::new(10);

        writer
            .write_all(b"12345")
            .expect("should accept write within limit");
        writer
            .write_all(b"67890")
            .expect("should accept write up to exact limit");

        assert_eq!(
            writer.into_inner(),
            b"1234567890",
            "should preserve all written bytes"
        );
    }

    #[test]
    fn bounded_writer_rejects_writes_exceeding_limit() {
        let mut writer = BoundedWriter::new(8);

        writer
            .write_all(b"12345")
            .expect("should accept write within limit");
        let err = writer
            .write_all(b"6789")
            .expect_err("should reject write that exceeds the limit");

        assert!(
            err.to_string().contains("maximum buffered size"),
            "should report the buffer cap in the error message"
        );
    }
}
