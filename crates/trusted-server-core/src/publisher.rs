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

use std::io::Write;
use std::sync::{Arc, Mutex};

use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Body, Request, Response};

use crate::auction::endpoints::{
    merge_auction_eids, resolve_auction_eids, resolve_client_auction_eids,
};
use crate::auction::orchestrator::{AuctionOrchestrator, DispatchedAuction};
use crate::auction::types::{
    AuctionContext, AuctionRequest, Bid, DeviceInfo, PublisherInfo, SiteInfo, UserInfo,
};
use crate::backend::BackendConfig;
use crate::compat;
use crate::consent::gate_eids_by_consent;
use crate::constants::{COOKIE_TS_EIDS, HEADER_X_COMPRESS_HINT};
use crate::cookies::handle_request_cookies;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::registry::PartnerRegistry;
use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::http_util::{is_navigation_request, serve_static_with_etag, RequestInfo};
use crate::integrations::IntegrationRegistry;
use crate::platform::RuntimeServices;
use crate::price_bucket::{price_bucket, PriceGranularity};
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::Settings;
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};
use crate::streaming_replacer::create_url_replacer;

const SUPPORTED_ENCODING_VALUES: [&str; 3] = ["gzip", "deflate", "br"];

/// Read buffer size for streaming body processing and brotli internal buffers.
/// Both the `Decompressor` and `CompressorWriter` use this value so all
/// brotli I/O layers operate on consistently-sized chunks.
const STREAM_CHUNK_SIZE: usize = 8192;

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
    let http_req = compat::from_fastly_headers_ref(req);

    if UNIFIED_FILENAMES.contains(&filename) {
        // Serve core + immediate modules (excludes deferred like prebid)
        let module_ids = integration_registry.js_module_ids_immediate();
        let body = trusted_server_js::concatenate_modules(&module_ids);
        let http_resp =
            serve_static_with_etag(&body, &http_req, "application/javascript; charset=utf-8");
        let mut resp = compat::to_fastly_response(http_resp);
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
            let http_resp =
                serve_static_with_etag(content, &http_req, "application/javascript; charset=utf-8");
            let mut resp = compat::to_fastly_response(http_resp);
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

/// Parameters for processing response streaming.
struct ProcessResponseParams<'a> {
    content_encoding: &'a str,
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
    body: Body,
    output: &mut W,
    params: &ProcessResponseParams,
) -> Result<(), Report<TrustedServerError>> {
    let is_html = params.content_type.contains("text/html");
    let is_rsc_flight = params.content_type.contains("text/x-component");
    // lgtm[rust/cleartext-logging]
    // This debug log records content-shape metadata and hostnames only; no secrets are logged.
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
            params.ad_slots_script.map(str::to_string),
            params.ad_bids_state.clone(),
        )?;
        StreamingPipeline::new(config, processor).process(body, output)?;
    } else if is_rsc_flight {
        // RSC Flight responses are length-prefixed (T rows). A naive string replacement will
        // corrupt the stream by changing byte lengths without updating the prefixes.
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
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};

    let config = HtmlProcessorConfig::from_settings(
        settings,
        integration_registry,
        origin_host,
        request_host,
        request_scheme,
    )
    .with_ad_state(ad_slots_script, ad_bids_state);

    Ok(create_html_processor(config))
}

/// Result of publisher request handling, indicating whether the response body
/// should be streamed or has already been buffered.
pub enum PublisherResponse {
    /// Response is fully buffered and ready to send via `send_to_client()`.
    Buffered(Response),
    /// Response headers are ready for a streaming response. EC finalization is
    /// header-only and must run before the adapter commits the headers.
    Stream {
        /// Response with all non-EC headers set but body not yet written.
        response: Response,
        /// Origin body to be piped through the streaming pipeline.
        body: Body,
        /// Parameters for [`process_response_streaming`].
        params: Box<OwnedProcessResponseParams>,
    },
    /// Non-processable `2xx` response (images, fonts, video). The adapter must
    /// reattach the body before sending it to the client.
    PassThrough {
        /// Response with headers set but body not yet written.
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
    _has_post_processors: bool,
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

/// Owned version of [`ProcessResponseParams`] for returning from
/// [`handle_publisher_request`] without lifetime issues.
pub struct OwnedProcessResponseParams {
    pub(crate) content_encoding: String,
    pub(crate) origin_host: String,
    pub(crate) origin_url: String,
    pub(crate) request_host: String,
    pub(crate) request_scheme: String,
    pub(crate) content_type: String,
    pub(crate) ad_slots_script: Option<String>,
    pub(crate) ad_bids_state: Arc<Mutex<Option<String>>>,
    /// In-flight SSP bids dispatched before `pending_origin.wait()`.
    /// The streaming phase collects these and writes bids to `ad_bids_state`
    /// before processing the last body chunk, so `</body>` injection sees live bids.
    pub(crate) dispatched_auction: Option<DispatchedAuction>,
    /// Price granularity used to bucket bids when building `tsjs.bids`.
    pub(crate) price_granularity: PriceGranularity,
}

/// Stream the publisher response body through the processing pipeline.
///
/// Called by the adapter after `stream_to_client()` has committed the response
/// headers. Writes processed chunks directly to `output`.
///
/// # Errors
///
/// Returns an error if processing fails mid-stream. Since headers are already
/// committed, the caller should log the error and drop the `StreamingBody`
/// (client sees a truncated response).
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
        ad_slots_script: params.ad_slots_script.as_deref(),
        ad_bids_state: &params.ad_bids_state,
    };
    process_response_streaming(body, output, &borrowed)
}

/// Stream publisher body with a `</body` tail hold for live bid injection.
///
/// Drives the origin body through the HTML pipeline one chunk at a time, using a
/// small buffer that holds the first raw `</body` tail. When the origin body is
/// exhausted (`read` returns `Ok(0)`):
///
/// 1. [`collect_dispatched_auction`](AuctionOrchestrator::collect_dispatched_auction)
///    is awaited with the remaining deadline.
/// 2. Winning bids are written to `ad_bids_state`.
/// 3. The held tail is fed through the pipeline so `lol_html` fires its
///    `</body>` handler with bids now in state.
///
/// For non-HTML content types the auction is collected before any body bytes
/// are written (no `</body>` to inject).  If `params.dispatched_auction` is
/// `None` the function falls back to the synchronous
/// [`stream_publisher_body`] path.
///
/// # Errors
///
/// Returns an error if processing fails mid-stream. Headers are already
/// committed at that point; the caller logs and drops the `StreamingBody`.
pub async fn stream_publisher_body_async<W: Write>(
    body: Body,
    output: &mut W,
    params: &mut OwnedProcessResponseParams,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
) -> Result<(), Report<TrustedServerError>> {
    let Some(dispatched) = params.dispatched_auction.take() else {
        // No auction — use the existing sync pipeline unchanged.
        return stream_publisher_body(body, output, params, settings, integration_registry);
    };

    let is_html = params.content_type.contains("text/html");

    if !is_html {
        // Non-HTML: collect auction first, then stream.  There is no </body>
        // to hold, so delaying the entire body until collection is acceptable.
        let placeholder = Request::get(crate::auction::types::MEDIATOR_PLACEHOLDER_URL);
        let result = orchestrator
            .collect_dispatched_auction(
                dispatched,
                services,
                &make_collect_context(settings, services, &placeholder),
            )
            .await;
        write_bids_to_state(
            &result.winning_bids,
            params.price_granularity,
            &params.ad_bids_state,
            settings.debug.inject_adm_for_testing,
        );
        return stream_publisher_body(body, output, params, settings, integration_registry);
    }

    // HTML: build the processor once and drive it chunk by chunk.
    // One-behind buffer: stream chunk N-1 immediately; hold chunk N until origin
    // EOF, then await auction and process chunk N (which contains </body>).
    let mut processor = create_html_stream_processor(
        &params.origin_host,
        &params.request_host,
        &params.request_scheme,
        settings,
        integration_registry,
        params.ad_slots_script.as_deref().map(str::to_string),
        params.ad_bids_state.clone(),
    )?;

    let compression = Compression::from_content_encoding(&params.content_encoding);
    stream_html_with_auction_hold(
        body,
        output,
        &mut processor,
        compression,
        AuctionCollectCtx {
            dispatched,
            price_granularity: params.price_granularity,
            ad_bids_state: &params.ad_bids_state,
            orchestrator,
            services,
            settings,
        },
    )
    .await
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
    placeholder: &'a Request,
) -> AuctionContext<'a> {
    debug_assert_eq!(
        placeholder.get_url_str(),
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
pub(crate) fn is_bot_user_agent(req: &Request) -> bool {
    let ua = req.get_header_str("user-agent").unwrap_or("");
    BOT_USER_AGENT_FRAGMENTS
        .iter()
        .any(|frag| ua.contains(frag))
}

/// Returns true when the request advertises itself as a prefetch via either
/// the standard `Sec-Purpose` or the legacy `Purpose` header.
pub(crate) fn is_prefetch_request(req: &Request) -> bool {
    req.get_header_str("sec-purpose")
        .is_some_and(|v| v.contains("prefetch"))
        || req
            .get_header_str("purpose")
            .is_some_and(|v| v.contains("prefetch"))
}

/// Returns true only when the publisher request should run the full
/// server-side ad stack: auction dispatch plus initial ad-slot injection.
pub(crate) fn should_run_server_side_ad_stack(
    is_get: bool,
    is_navigation: bool,
    is_prefetch: bool,
    is_bot: bool,
    has_matched_slots: bool,
    consent_allows_auction: bool,
) -> bool {
    is_get
        && is_navigation
        && !is_prefetch
        && !is_bot
        && has_matched_slots
        && consent_allows_auction
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

/// Bundles the auction-collection dependencies passed through the streaming helpers.
struct AuctionCollectCtx<'a> {
    dispatched: DispatchedAuction,
    price_granularity: PriceGranularity,
    ad_bids_state: &'a Arc<Mutex<Option<String>>>,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    settings: &'a Settings,
}

/// Run the close-body hold loop for HTML bodies, collecting the auction before
/// the raw `</body` tail is processed so `lol_html` sees live bids.
async fn stream_html_with_auction_hold<W: Write, P: StreamProcessor>(
    body: Body,
    output: &mut W,
    processor: &mut P,
    compression: Compression,
    ctx: AuctionCollectCtx<'_>,
) -> Result<(), Report<TrustedServerError>> {
    use brotli::enc::writer::CompressorWriter;
    use brotli::enc::BrotliEncoderParams;
    use brotli::Decompressor;
    use flate2::read::{GzDecoder, ZlibDecoder};
    use flate2::write::{GzEncoder, ZlibEncoder};

    match compression {
        Compression::None => body_close_hold_loop(body, output, processor, ctx).await,
        Compression::Gzip => {
            let decoder = GzDecoder::new(body);
            let mut encoder = GzEncoder::new(&mut *output, flate2::Compression::default());
            body_close_hold_loop(decoder, &mut encoder, processor, ctx).await?;
            encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize gzip encoder".to_string(),
            })?;
            Ok(())
        }
        Compression::Deflate => {
            let decoder = ZlibDecoder::new(body);
            let mut encoder = ZlibEncoder::new(&mut *output, flate2::Compression::default());
            body_close_hold_loop(decoder, &mut encoder, processor, ctx).await?;
            encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize deflate encoder".to_string(),
            })?;
            Ok(())
        }
        Compression::Brotli => {
            let decoder = Decompressor::new(body, STREAM_CHUNK_SIZE);
            let params = BrotliEncoderParams {
                quality: 4,
                lgwin: 22,
                ..Default::default()
            };
            let mut encoder =
                CompressorWriter::with_params(&mut *output, STREAM_CHUNK_SIZE, &params);
            body_close_hold_loop(decoder, &mut encoder, processor, ctx).await?;
            let _ = encoder.into_inner();
            Ok(())
        }
    }
}

const BODY_CLOSE_PREFIX: &[u8] = b"</body";

struct BodyCloseHoldBuffer {
    buffered: Vec<u8>,
    found_close: bool,
}

impl BodyCloseHoldBuffer {
    fn new() -> Self {
        Self {
            buffered: Vec::new(),
            found_close: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffered.extend_from_slice(chunk);

        if self.found_close {
            return Vec::new();
        }

        if let Some(pos) = find_ascii_case_insensitive(&self.buffered, BODY_CLOSE_PREFIX) {
            self.found_close = true;
            return self.buffered.drain(..pos).collect();
        }

        let keep_len = BODY_CLOSE_PREFIX.len().saturating_sub(1);
        if self.buffered.len() <= keep_len {
            return Vec::new();
        }

        let split_at = self.buffered.len() - keep_len;
        self.buffered.drain(..split_at).collect()
    }

    fn found_close(&self) -> bool {
        self.found_close
    }

    fn finish(self) -> Vec<u8> {
        self.buffered
    }
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

/// Core close-body hold loop.
///
/// Streams processed output until the first case-insensitive `</body` prefix is
/// seen, then collects the auction, writes bids, and processes the held tail
/// before reading post-body chunks. If no close-body tag is found, collection
/// happens at EOF before finalization.
async fn body_close_hold_loop<R: std::io::Read, W: Write, P: StreamProcessor>(
    mut reader: R,
    writer: &mut W,
    processor: &mut P,
    ctx: AuctionCollectCtx<'_>,
) -> Result<(), Report<TrustedServerError>> {
    let AuctionCollectCtx {
        dispatched,
        price_granularity,
        ad_bids_state,
        orchestrator,
        services,
        settings,
    } = ctx;
    let mut buffer = vec![0u8; STREAM_CHUNK_SIZE];
    let mut hold = Some(BodyCloseHoldBuffer::new());
    let mut dispatched = Some(dispatched);

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                if let Some(hold) = hold.take() {
                    let dispatched = dispatched
                        .take()
                        .expect("should have dispatched auction to collect");
                    collect_stream_auction(
                        dispatched,
                        price_granularity,
                        ad_bids_state,
                        orchestrator,
                        services,
                        settings,
                    )
                    .await;

                    let held = hold.finish();
                    write_processed_chunk(
                        writer,
                        processor,
                        &held,
                        false,
                        "Failed to process held body close",
                        "Failed to write held body close",
                    )?;
                }
                // Signal EOF to lol_html (fires end() which flushes remaining state).
                let final_out = processor.process_chunk(&[], true).change_context(
                    TrustedServerError::Proxy {
                        message: "Failed to finalize processor".to_string(),
                    },
                )?;
                if !final_out.is_empty() {
                    writer
                        .write_all(&final_out)
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to write finalized output".to_string(),
                        })?;
                }
                break;
            }
            Ok(n) => {
                if let Some(hold_buffer) = hold.as_mut() {
                    let ready = hold_buffer.push(&buffer[..n]);
                    write_processed_chunk(
                        writer,
                        processor,
                        &ready,
                        false,
                        "Failed to process chunk",
                        "Failed to write chunk",
                    )?;

                    if hold_buffer.found_close() {
                        let dispatched = dispatched
                            .take()
                            .expect("should have dispatched auction to collect");
                        collect_stream_auction(
                            dispatched,
                            price_granularity,
                            ad_bids_state,
                            orchestrator,
                            services,
                            settings,
                        )
                        .await;

                        let held = hold
                            .take()
                            .expect("should have close-body hold buffer")
                            .finish();
                        write_processed_chunk(
                            writer,
                            processor,
                            &held,
                            false,
                            "Failed to process held body close",
                            "Failed to write held body close",
                        )?;
                    }
                } else {
                    write_processed_chunk(
                        writer,
                        processor,
                        &buffer[..n],
                        false,
                        "Failed to process chunk",
                        "Failed to write chunk",
                    )?;
                }
            }
            Err(e) => {
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

async fn collect_stream_auction(
    dispatched: DispatchedAuction,
    price_granularity: PriceGranularity,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
    settings: &Settings,
) {
    log::info!("body_close_hold_loop: collecting dispatched auction before held body tail");
    let placeholder = Request::get(crate::auction::types::MEDIATOR_PLACEHOLDER_URL);
    let collect_ctx = make_collect_context(settings, services, &placeholder);
    let result = orchestrator
        .collect_dispatched_auction(dispatched, services, &collect_ctx)
        .await;
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

fn write_processed_chunk<W: Write, P: StreamProcessor>(
    writer: &mut W,
    processor: &mut P,
    chunk: &[u8],
    is_last: bool,
    process_error: &str,
    write_error: &str,
) -> Result<(), Report<TrustedServerError>> {
    if chunk.is_empty() && !is_last {
        return Ok(());
    }

    let out =
        processor
            .process_chunk(chunk, is_last)
            .change_context(TrustedServerError::Proxy {
                message: process_error.to_string(),
            })?;
    if !out.is_empty() {
        writer
            .write_all(&out)
            .change_context(TrustedServerError::Proxy {
                message: write_error.to_string(),
            })?;
    }

    Ok(())
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
///
/// # Panics
///
/// Panics if `should_run_auction` is `true` but `settings.creative_opportunities` is `None`.
/// This is a logic invariant: `should_run_auction` is only set when creative opportunities
/// are configured, so this state is unreachable in practice.
pub async fn handle_publisher_request(
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    services: &RuntimeServices,
    kv: Option<&KvIdentityGraph>,
    ec_context: &mut EcContext,
    auction: AuctionDispatch<'_>,
    mut req: Request,
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

    // Prebid.js requests are not intercepted here anymore. The HTML processor removes
    // publisher-supplied Prebid scripts; the unified TSJS bundle includes Prebid.js when enabled.

    let http_req = compat::from_fastly_headers_ref(&req);

    // Extract request host and scheme (uses Host header and TLS detection after edge sanitization)
    let request_info = RequestInfo::from_request(&http_req, &services.client_info);
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

    let is_navigation = is_navigation_request(&http_req);

    // Generate a new EC ID only for document navigations. Subresource
    // requests (fonts, images, CSS) may lack consent signals such as the
    // Sec-GPC header, so we skip generation to avoid setting identity
    // cookies when the user's consent preference is unknown.
    if is_navigation {
        if let Err(err) = ec_context.generate_if_needed(settings, kv) {
            log::warn!("EC generation failed: {err:?}");
        }
    } else {
        log::debug!(
            "EC generation skipped: non-document request (path={})",
            req.get_path(),
        );
    }

    let ec_allowed = ec_context.ec_allowed();
    log::debug!(
        "Proxy EC ID: {:?}, ec_allowed: {ec_allowed}",
        ec_context.ec_value(),
    );

    let consent_context = ec_context.consent().clone();
    let ec_id = ec_context.ec_value().filter(|_| ec_allowed);
    let cookie_jar = handle_request_cookies(&http_req)?;
    let geo = ec_context.geo_info().cloned();

    let backend_name = BackendConfig::from_url(
        &settings.publisher.origin_url,
        settings.proxy.certificate_check,
    )?;
    let origin_host = settings.publisher.origin_host();

    // lgtm[rust/cleartext-logging]
    // This debug log records backend routing metadata only; `Settings` secrets remain redacted.
    log::debug!(
        "Proxying to dynamic backend: {} (from {})",
        backend_name,
        settings.publisher.origin_url
    );

    let request_path = req.get_path().to_string();
    let is_get = req.get_method() == fastly::http::Method::GET;

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

    // Non-GDPR regions (US, etc.) have no TCF string — auction is freely allowed.
    // GDPR regions require TCF Purpose 1 (storage/access) before firing.
    let consent_allows_auction = !consent_context.gdpr_applies
        || consent_context
            .tcf
            .as_ref()
            .is_some_and(|tcf| tcf.has_purpose_consent(1));

    let should_run_ad_stack = should_run_server_side_ad_stack(
        is_get,
        is_navigation,
        is_prefetch,
        is_bot,
        !matched_slots.is_empty(),
        consent_allows_auction,
    );
    let should_run_auction = should_run_ad_stack;

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
    let dispatched_auction = if should_run_auction {
        let slots_ctx = MatchedSlotsContext {
            matched_slots: &matched_slots,
            request_path: &request_path,
        };
        let mut auction_request = build_auction_request(
            &slots_ctx,
            ec_id,
            &consent_context,
            &request_info,
            req.get_header_str("user-agent"),
        );
        let ts_eids_value = cookie_jar
            .as_ref()
            .and_then(|j| j.get(COOKIE_TS_EIDS))
            .map(|c| c.value().to_owned());
        let client_eids = if ec_id.is_some() {
            resolve_client_auction_eids(None, ts_eids_value.as_deref())
        } else {
            None
        };
        let kv_eids = resolve_auction_eids(kv, auction.registry, ec_context);
        let merged_eids = merge_auction_eids(client_eids, kv_eids);
        let had_eids = merged_eids.as_ref().is_some_and(|v| !v.is_empty());
        auction_request.user.eids =
            gate_eids_by_consent(merged_eids, auction_request.user.consent.as_ref());
        if had_eids && auction_request.user.eids.is_none() {
            log::warn!("Server-side auction EIDs stripped by TCF consent gating");
        }
        let client_ip = services.client_info.client_ip.map(|ip| ip.to_string());
        if client_ip.is_some() || geo.is_some() {
            let device = auction_request.device.get_or_insert(DeviceInfo {
                user_agent: None,
                ip: None,
                geo: None,
            });
            device.ip = client_ip;
            device.geo = geo.clone();
        }
        let auction_context = AuctionContext {
            settings,
            request: &req,
            timeout_ms: auction_timeout_ms,
            provider_responses: None,
            services,
        };
        auction
            .orchestrator
            .dispatch_auction(&auction_request, &auction_context)
    } else {
        None
    };
    log::info!(
        "dispatch_auction: {}",
        if dispatched_auction.is_some() {
            "Some — auction running async"
        } else {
            "None — falling back to sync or skipped"
        }
    );

    // Only advertise encodings the rewrite pipeline can decode and re-encode.
    restrict_accept_encoding(&mut req);
    req.set_header("host", &origin_host);

    // Dispatch origin — SSP requests are already racing in Fastly's native layer.
    // TTFB ≈ origin latency instead of TTFB ≈ auction timeout.
    let pending_origin =
        req.send_async(&backend_name)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to dispatch async origin request".to_string(),
            })?;

    // Now yield for origin.
    let mut response = pending_origin
        .wait()
        .change_context(TrustedServerError::Proxy {
            message: "Failed to await origin response".to_string(),
        })?;

    log::debug!("Response headers:");
    for (name, value) in response.get_headers() {
        log::debug!("  {}: {:?}", name, value);
    }

    let ad_slots_script = if should_run_ad_stack {
        settings
            .creative_opportunities
            .as_ref()
            .map(|co_config| build_ad_slots_script(&matched_slots, co_config))
    } else {
        None
    };

    // §4.7: assembled HTML responses must never be shared-cached — per-user bid data
    // travels inline. Apply regardless of slot match or auction outcome (§8).
    let origin_content_type = response
        .get_header(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if origin_content_type.contains("text/html") {
        response.set_header(header::CACHE_CONTROL, "private, max-age=0");
        response.remove_header("surrogate-control");
        response.remove_header("fastly-surrogate-control");
    }

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
            // Unsupported or unprocessable responses must bypass rewriting
            // entirely rather than entering the pipeline as identity bytes.
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
                content_type,
                content_encoding,
                request_host,
                origin_host,
            );

            let body = response.take_body();
            response.remove_header(header::CONTENT_LENGTH);

            Ok(PublisherResponse::Stream {
                response,
                body,
                params: Box::new(OwnedProcessResponseParams {
                    content_encoding,
                    origin_host,
                    origin_url: settings.publisher.origin_url.clone(),
                    request_host: request_host.to_string(),
                    request_scheme: request_scheme.to_string(),
                    content_type,
                    ad_slots_script: ad_slots_script.clone(),
                    ad_bids_state: ad_bids_state.clone(),
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
        .map(|slot| {
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
        })
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
    content_type.contains("text/")
        || content_type.contains("application/javascript")
        || content_type.contains("application/json")
}

/// Whether the `Content-Encoding` is one the streaming pipeline can handle.
///
/// Unsupported encodings (e.g. `zstd` from a misbehaving origin) bypass the
/// rewrite pipeline entirely and are returned unchanged. Processing such bodies
/// as identity-encoded would produce garbled output.
fn is_supported_content_encoding(encoding: &str) -> bool {
    matches!(encoding, "" | "identity" | "gzip" | "deflate" | "br")
}

/// Handle `GET /__ts/page-bids?path=<path>` — server-side auction for SPA navigation.
///
/// Matches creative opportunity slots for the given path, runs a server-side
/// auction (APS + PBS), and returns the slot definitions and winning bids as JSON.
/// Called by the client-side SPA navigation hook after `pushState` / `popstate`.
///
/// # Errors
///
/// Returns [`TrustedServerError`] if cookie parsing or EC ID generation fails.
pub async fn handle_page_bids(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
    kv: Option<&KvIdentityGraph>,
    registry: Option<&PartnerRegistry>,
    slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let Some(co_config) = &settings.creative_opportunities else {
        return Ok(Response::from_status(StatusCode::NOT_FOUND)
            .with_body_text_plain("Creative opportunities not configured"));
    };

    let path_param = req
        .get_url()
        .query_pairs()
        .find(|(k, _)| k == "path")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| "/".to_string());

    let matched_slots: Vec<_> = crate::creative_opportunities::match_slots(slots, &path_param)
        .into_iter()
        .cloned()
        .collect();

    let http_req = compat::from_fastly_headers_ref(&req);
    let request_info =
        crate::http_util::RequestInfo::from_request(&http_req, &services.client_info);
    let ec_ctx =
        EcContext::read_from_request(settings, &req).change_context(TrustedServerError::Proxy {
            message: "page-bids: failed to read EC context".to_string(),
        })?;
    let ec_id = ec_ctx.ec_value().filter(|_| ec_ctx.ec_allowed());
    let consent_context = ec_ctx.consent().clone();
    let geo = ec_ctx.geo_info().cloned();
    let cookie_jar = handle_request_cookies(&http_req)?;

    let consent_allows_auction = !consent_context.gdpr_applies
        || consent_context
            .tcf
            .as_ref()
            .is_some_and(|tcf| tcf.has_purpose_consent(1));

    // Same bot / prefetch guards the publisher path uses — without them this
    // endpoint would fire real SSP auctions on Sec-Purpose=prefetch warm-up
    // navigations and known crawler UA scans, burning partner request quota.
    let is_prefetch = is_prefetch_request(&req);
    let is_bot = is_bot_user_agent(&req);

    if matched_slots.is_empty() {
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

    let winning_bids =
        if !matched_slots.is_empty() && consent_allows_auction && !is_bot && !is_prefetch {
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &matched_slots,
                request_path: &path_param,
            };
            let mut auction_request = build_auction_request(
                &slots_ctx,
                ec_id,
                &consent_context,
                &request_info,
                req.get_header_str("user-agent"),
            );
            let ts_eids_value = cookie_jar
                .as_ref()
                .and_then(|j| j.get(COOKIE_TS_EIDS))
                .map(|c| c.value().to_owned());
            let client_eids = if ec_id.is_some() {
                resolve_client_auction_eids(None, ts_eids_value.as_deref())
            } else {
                None
            };
            let kv_eids = resolve_auction_eids(kv, registry, &ec_ctx);
            let merged_eids = merge_auction_eids(client_eids, kv_eids);
            let had_eids = merged_eids.as_ref().is_some_and(|v| !v.is_empty());
            auction_request.user.eids =
                gate_eids_by_consent(merged_eids, auction_request.user.consent.as_ref());
            if had_eids && auction_request.user.eids.is_none() {
                log::warn!("Page-bids auction EIDs stripped by TCF consent gating");
            }
            let client_ip = services.client_info.client_ip.map(|ip| ip.to_string());
            if client_ip.is_some() || geo.is_some() {
                let device = auction_request.device.get_or_insert(DeviceInfo {
                    user_agent: None,
                    ip: None,
                    geo: None,
                });
                device.ip = client_ip;
                device.geo = geo.clone();
            }
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
            match orchestrator
                .run_auction(&auction_request, &auction_context)
                .await
            {
                Ok(result) => result.winning_bids,
                Err(e) => {
                    log::warn!("page-bids auction failed: {e:?}");
                    std::collections::HashMap::new()
                }
            }
        } else {
            std::collections::HashMap::new()
        };

    let bid_map = build_bid_map(
        &winning_bids,
        co_config.price_granularity,
        settings.debug.inject_adm_for_testing,
    );

    let slots_json: Vec<serde_json::Value> = matched_slots
        .iter()
        .map(|slot| {
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
        })
        .collect();

    let body = serde_json::json!({
        "slots": slots_json,
        "bids": bid_map,
    });

    let json_str = serde_json::to_string(&body).change_context(TrustedServerError::Proxy {
        message: "Failed to serialize page-bids response".to_string(),
    })?;

    let mut response = Response::from_status(StatusCode::OK);
    response.set_header(header::CONTENT_TYPE, "application/json");
    response.set_header(header::CACHE_CONTROL, "private, no-store");
    response.set_body(json_str);

    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read as _, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use brotli::enc::writer::CompressorWriter;
    use brotli::Decompressor;
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;

    use super::*;
    use crate::auction::types::{AdFormat, AdSlot, MediaType};
    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::{header, Method, StatusCode};

    struct ChunkedReader {
        chunks: std::collections::VecDeque<Vec<u8>>,
        read_count: Arc<AtomicUsize>,
    }

    impl ChunkedReader {
        fn new(chunks: &[&[u8]], read_count: Arc<AtomicUsize>) -> Self {
            Self {
                chunks: chunks.iter().map(|chunk| chunk.to_vec()).collect(),
                read_count,
            }
        }
    }

    impl io::Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            self.read_count.fetch_add(1, Ordering::SeqCst);
            let len = chunk.len().min(buf.len());
            buf[..len].copy_from_slice(&chunk[..len]);
            Ok(len)
        }
    }

    struct RecordingProcessor {
        read_count: Arc<AtomicUsize>,
        body_close_processed_at: Arc<AtomicUsize>,
    }

    impl StreamProcessor for RecordingProcessor {
        fn process_chunk(&mut self, chunk: &[u8], _is_last: bool) -> Result<Vec<u8>, io::Error> {
            if find_ascii_case_insensitive(chunk, BODY_CLOSE_PREFIX).is_some() {
                self.body_close_processed_at
                    .store(self.read_count.load(Ordering::SeqCst), Ordering::SeqCst);
            }
            Ok(chunk.to_vec())
        }
    }

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
            content_encoding: content_encoding.to_owned(),
            origin_host: settings.publisher.origin_host(),
            origin_url: settings.publisher.origin_url.clone(),
            request_host: settings.publisher.domain.clone(),
            request_scheme: "https".to_owned(),
            content_type: "application/json".to_owned(),
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            dispatched_auction: None,
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

    #[test]
    fn test_content_type_detection() {
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
            should_run_server_side_ad_stack(true, true, false, false, true, true),
            "GET, real navigation, matched slots, and consent should run TS ad stack"
        );

        assert!(
            !should_run_server_side_ad_stack(false, true, false, false, true, true),
            "non-GET requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, false, false, false, true, true),
            "non-document requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, true, false, true, true),
            "prefetch requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, true, true, true),
            "bot requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, false, true),
            "requests with no matching slots should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, true, false),
            "requests without required consent should skip TS ad stack and injection"
        );
    }

    #[tokio::test]
    async fn body_close_hold_loop_processes_close_tail_before_reading_post_body_chunks() {
        let settings = create_test_settings();
        let services = noop_services();
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let dispatched = DispatchedAuction::empty_for_test(test_auction_request(), 500);
        let read_count = Arc::new(AtomicUsize::new(0));
        let body_close_processed_at = Arc::new(AtomicUsize::new(0));
        let reader = ChunkedReader::new(
            &[
                b"<html><body>painted</body>",
                b"<script>late()</script>",
                b"</html>",
            ],
            Arc::clone(&read_count),
        );
        let mut processor = RecordingProcessor {
            read_count: Arc::clone(&read_count),
            body_close_processed_at: Arc::clone(&body_close_processed_at),
        };
        let ad_bids_state = Arc::new(Mutex::new(None));
        let ctx = AuctionCollectCtx {
            dispatched,
            price_granularity: PriceGranularity::default(),
            ad_bids_state: &ad_bids_state,
            orchestrator: &orchestrator,
            services: &services,
            settings: &settings,
        };
        let mut output = Vec::new();

        body_close_hold_loop(reader, &mut output, &mut processor, ctx)
            .await
            .expect("should stream body with auction hold");

        assert_eq!(
            body_close_processed_at.load(Ordering::SeqCst),
            1,
            "close-body tail should be processed as soon as it is found, before later chunks are read"
        );
        assert_eq!(
            std::str::from_utf8(&output).expect("should be utf8"),
            "<html><body>painted</body><script>late()</script></html>",
            "post-body chunks should still stream in order"
        );
    }

    #[test]
    fn body_close_hold_buffer_holds_close_body_tail_in_single_chunk() {
        let mut hold = BodyCloseHoldBuffer::new();

        let ready = hold.push(b"<html><body>painted</body></html>");
        let held = hold.finish();

        assert_eq!(
            std::str::from_utf8(&ready).expect("should be utf8"),
            "<html><body>painted",
            "content before </body> should stream before auction collection"
        );
        assert_eq!(
            std::str::from_utf8(&held).expect("should be utf8"),
            "</body></html>",
            "the close-body tag and trailing bytes should be held"
        );
    }

    #[test]
    fn body_close_hold_buffer_holds_close_body_tail_across_chunks() {
        let mut hold = BodyCloseHoldBuffer::new();

        let first = hold.push(b"<html><body>painted</bo");
        let second = hold.push(b"dy></html>");
        let held = hold.finish();

        let streamed = [first, second].concat();
        assert_eq!(
            std::str::from_utf8(&streamed).expect("should be utf8"),
            "<html><body>painted",
            "split </body> bytes must not leak before auction collection"
        );
        assert_eq!(
            std::str::from_utf8(&held).expect("should be utf8"),
            "</body></html>",
            "split close-body tag should be held intact"
        );
    }

    #[test]
    fn unsupported_encoding_response_is_returned_unmodified() {
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
                "example.com",
                false,
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_streams_html_with_post_processors() {
        assert_eq!(
            classify_response_route(
                StatusCode::OK,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                true,
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_streams_non_html_even_with_post_processors_registered() {
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
        assert_eq!(
            classify_response_route(StatusCode::OK, "text/html", "zstd", "example.com", false,),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_passes_through_non_processable_2xx() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "image/png", "", "example.com", false,),
            ResponseRoute::PassThrough,
        );
    }

    #[test]
    fn route_buffers_non_processable_error_responses() {
        assert_eq!(
            classify_response_route(StatusCode::NOT_FOUND, "image/png", "", "example.com", false,),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn route_excludes_204_from_pass_through() {
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
                "example.com",
                false,
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
    fn route_streams_non_2xx_html_with_post_processors() {
        assert_eq!(
            classify_response_route(
                StatusCode::NOT_FOUND,
                "text/html; charset=utf-8",
                "gzip",
                "example.com",
                true,
            ),
            ResponseRoute::Stream,
        );
    }

    #[test]
    fn route_passes_through_non_processable_even_with_empty_request_host() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "image/png", "", "", false,),
            ResponseRoute::PassThrough,
        );
    }

    #[test]
    fn route_buffers_processable_content_with_empty_request_host() {
        assert_eq!(
            classify_response_route(StatusCode::OK, "text/html", "gzip", "", false,),
            ResponseRoute::BufferedUnmodified,
        );
    }

    #[test]
    fn pass_through_preserves_body_and_content_length() {
        let image_bytes: Vec<u8> = (0..=255).cycle().take(4096).collect();

        let mut response = Response::from_status(StatusCode::OK);
        response.set_header("content-type", "image/png");
        response.set_header("content-length", image_bytes.len().to_string());
        response.set_body(Body::from(image_bytes.clone()));

        let body = response.take_body();
        assert_eq!(
            response
                .get_header_str("content-length")
                .expect("should have content-length"),
            "4096",
            "Content-Length should be preserved for pass-through"
        );

        response.set_body(body);
        let output = response.into_body().into_bytes();
        assert_eq!(
            output, image_bytes,
            "pass-through should preserve body byte-for-byte"
        );
    }

    #[test]
    fn test_content_encoding_detection() {
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
    fn stream_publisher_body_round_trips_gzip() {
        let settings = create_test_settings();
        let integration_registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let input = b"{\"asset\":\"https://origin.test-publisher.com/path/file.js\"}";
        let compressed = gzip_encode(input);
        let params = make_stream_params(&settings, "gzip");
        let mut output = Vec::new();

        stream_publisher_body(
            Body::from(compressed),
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
            Body::from(compressed),
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

    #[test]
    fn request_ec_uses_cookie_not_header() {
        let settings = create_test_settings();
        let header_ec = format!("{}.HdrId1", "a".repeat(64));
        let cookie_ec = format!("{}.CkId01", "b".repeat(64));
        let mut req = Request::new(Method::GET, "https://test.example.com/page");
        req.set_header("x-ts-ec", &header_ec);
        req.set_header("cookie", format!("ts-ec={cookie_ec}; other=value"));

        let ec_context =
            EcContext::read_from_request(&settings, &req).expect("should read EC context");

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
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
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
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
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
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
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
                registry.has_html_post_processors(),
            ),
            ResponseRoute::Stream,
            "HTML with post-processors must route to Stream"
        );

        // Feed a small HTML body through the same pipeline the Stream arm uses.
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
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
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
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html".to_string(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(Mutex::new(None)),
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
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
        use fastly::http::Method;
        use fastly::Request;

        fn settings_with_co() -> Settings {
            let toml = format!(
                "{}\n[creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with creative_opportunities")
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

        fn make_page_bids_request(path: &str) -> Request {
            Request::new(
                Method::GET,
                format!("https://test-publisher.com/_ts/page-bids?path={path}"),
            )
        }

        #[tokio::test]
        async fn empty_slots_file_returns_empty_slots_and_bids() {
            // Spec §8 kill-switch: creative-opportunities.toml with zero slots disables
            // all server-side auction activity and injection.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let req = make_page_bids_request("/2024/01/my-article/");

            let response =
                handle_page_bids(&settings, &orchestrator, &services, None, None, &[], req)
                    .await
                    .expect("should return ok response");

            let body: serde_json::Value =
                serde_json::from_slice(&response.into_body_bytes()).expect("should be json");

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
            let services = noop_services();
            let slots = article_slot();
            let mut req = make_page_bids_request("/2024/01/my-article/");
            req.set_header("user-agent", "Mozilla/5.0 (compatible; Googlebot/2.1)");

            let response =
                handle_page_bids(&settings, &orchestrator, &services, None, None, &slots, req)
                    .await
                    .expect("should return ok response");

            let body: serde_json::Value =
                serde_json::from_slice(&response.into_body_bytes()).expect("should be json");

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
            let services = noop_services();
            let slots = article_slot();
            let mut req = make_page_bids_request("/2024/01/my-article/");
            req.set_header("sec-purpose", "prefetch");

            let response =
                handle_page_bids(&settings, &orchestrator, &services, None, None, &slots, req)
                    .await
                    .expect("should return ok response");

            let body: serde_json::Value =
                serde_json::from_slice(&response.into_body_bytes()).expect("should be json");

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
            let services = noop_services();
            let slots = article_slot(); // slot matches /20** only
            let req = make_page_bids_request("/about"); // does not match

            let response =
                handle_page_bids(&settings, &orchestrator, &services, None, None, &slots, req)
                    .await
                    .expect("should return ok response");

            let body: serde_json::Value =
                serde_json::from_slice(&response.into_body_bytes()).expect("should be json");

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
    }
}
