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
//! `BootManifestV1` serialization remains a pure helper in Task 8. This
//! production pipeline does not emit it until the coordinated Task 19 switch.
//!
//! **Note on platform coupling:** The handler boundaries use portable HTTP
//! types: [`handle_publisher_request`] and [`stream_publisher_body`] take and
//! return `http::Request`/`http::Response` over `EdgeBody`, and platform I/O is
//! reached through `RuntimeServices` rather than `fastly::*` directly. The
//! streaming processor itself is generic: `process_response_streaming` writes
//! into any [`Write`] (a `Vec<u8>` for buffered routes, a streaming writer for
//! the streaming route). It is not a content-rewriting concern.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use http::{HeaderValue, Method, Request, Response, StatusCode, Uri, header};

use crate::auction::endpoints::{
    merge_auction_eids, resolve_auction_eids, resolve_client_auction_eids,
};
use crate::auction::formats::{
    coordinated_cutover_v1::CanonicalBrowserAuctionProjectionV1, sanitize_publisher_page_url,
};
use crate::auction::orchestrator::{
    AuctionOrchestrator, DispatchAuctionOutcome, DispatchedAuction, OrchestrationResult,
};
use crate::auction::telemetry::{
    AuctionObservationContext, AuctionSource, AuctionTerminalOutcome, build_auction_events,
    emit_auction_events_best_effort_lazy,
};
use crate::auction::types::{
    AdmRenderSourceV1, AuctionContext, AuctionDecisionSetV1, AuctionIdentityGenerator,
    AuctionRequest, AuctionSlotFailureReason, Bid, BidRenderSourceV1, BrowserAuctionBidV1,
    BrowserAuctionProjectionV1, BrowserAuctionSlotV1, CacheFetchPolicyV1, CacheRenderSourceV1,
    DeviceInfo, PublisherInfo, SiteInfo, SlotAuctionDecisionV1, SystemAuctionIdentityGenerator,
    UserInfo, mint_response_unique_base64url_identity,
};
use crate::consent::{consent_allows_server_side_auction, gate_eids_by_consent};
use crate::constants::{COOKIE_TS_EIDS, HEADER_X_COMPRESS_HINT};
use crate::cookies::handle_request_cookies;
use crate::ec::EcContext;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::http_util::{RequestInfo, is_navigation_request, serve_static_with_etag};
use crate::integrations::IntegrationRegistry;
use crate::platform::{GeoInfo, PlatformBackendSpec, PlatformHttpRequest, RuntimeServices};
use crate::price_bucket::{PriceGranularity, price_bucket};
use crate::response_privacy::CDN_CACHE_HEADERS;
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::Settings;
use crate::streaming_processor::{
    BodyStreamDecoder, BodyStreamEncoder, Compression, PipelineConfig, STREAM_CHUNK_SIZE,
    StreamProcessor, StreamingPipeline,
};
use crate::streaming_replacer::create_url_replacer;

const SUPPORTED_ENCODING_VALUES: [&str; 3] = ["gzip", "deflate", "br"];
const DEFAULT_PUBLISHER_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(15);

fn body_as_reader(
    body: EdgeBody,
) -> Result<std::io::Cursor<bytes::Bytes>, Report<TrustedServerError>> {
    let bytes = body.into_bytes().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "streaming body cannot be processed by sync publisher pipeline".to_string(),
        })
    })?;
    Ok(std::io::Cursor::new(bytes))
}

struct BodyChunkSource {
    body: Option<EdgeBody>,
    chunk_size: usize,
    max_bytes: usize,
    bytes_seen: usize,
    once_offset: usize,
}

impl BodyChunkSource {
    fn new(body: EdgeBody, chunk_size: usize) -> Self {
        Self {
            body: Some(body),
            chunk_size,
            max_bytes: usize::MAX,
            bytes_seen: 0,
            once_offset: 0,
        }
    }

    fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    async fn next_chunk(&mut self) -> Result<Option<bytes::Bytes>, Report<TrustedServerError>> {
        // The body is polled in place (never moved out across an await) so a
        // cancelled `next_chunk` future leaves the source resumable instead of
        // silently reporting end-of-stream on the next call.
        let pulled = match &mut self.body {
            None => Ok(None),
            Some(EdgeBody::Once(bytes)) => {
                let end = (self.once_offset + self.chunk_size).min(bytes.len());
                if self.once_offset >= end {
                    Ok(None)
                } else {
                    let chunk = bytes.slice(self.once_offset..end);
                    self.once_offset = end;
                    Ok(Some(chunk))
                }
            }
            Some(EdgeBody::Stream(stream)) => match stream.next().await {
                Some(Ok(chunk)) => Ok(Some(chunk)),
                Some(Err(err)) => Err(Report::new(TrustedServerError::Proxy {
                    message: format!("Failed to read publisher origin body stream: {err}"),
                })),
                None => Ok(None),
            },
        };

        let chunk = match pulled {
            Ok(Some(chunk)) => chunk,
            Ok(None) => {
                self.body = None;
                return Ok(None);
            }
            Err(err) => {
                self.body = None;
                return Err(err);
            }
        };

        self.bytes_seen = self.bytes_seen.checked_add(chunk.len()).ok_or_else(|| {
            Report::new(TrustedServerError::Proxy {
                message: "publisher origin body byte count overflowed".to_string(),
            })
        })?;
        if self.bytes_seen > self.max_bytes {
            return Err(Report::new(TrustedServerError::Proxy {
                message: format!(
                    "publisher origin body exceeded {}-byte streaming limit",
                    self.max_bytes
                ),
            }));
        }

        Ok(Some(chunk))
    }
}

fn process_and_encode_chunk<P: StreamProcessor>(
    processor: &mut P,
    encoder: &mut BodyStreamEncoder,
    chunk: &[u8],
    is_last: bool,
    process_error: &str,
) -> Result<Option<bytes::Bytes>, Report<TrustedServerError>> {
    let processed =
        processor
            .process_chunk(chunk, is_last)
            .change_context(TrustedServerError::Proxy {
                message: process_error.to_string(),
            })?;
    if processed.is_empty() {
        return Ok(None);
    }
    let encoded = encoder.encode_chunk(processed)?;
    if encoded.is_empty() {
        return Ok(None);
    }
    Ok(Some(bytes::Bytes::from(encoded)))
}

// By-value signature so `map_err(publisher_stream_error)` works directly.
#[allow(clippy::needless_pass_by_value)]
fn publisher_stream_error(err: Report<TrustedServerError>) -> std::io::Error {
    std::io::Error::other(format!("{err:?}"))
}

fn not_found_response() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Not Found"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

fn tsjs_not_found_response() -> Response<EdgeBody> {
    let mut response = not_found_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
            if name.trim().eq_ignore_ascii_case("q")
                && let Ok(parsed_qvalue) = value.trim().parse::<f32>()
            {
                qvalue = parsed_qvalue;
            }
        }

        // First match wins per RFC 7231 — duplicate tokens are non-normative,
        // but using first-match is the conventional interpretation.
        matched_qvalue = Some(qvalue);
        break;
    }

    matched_qvalue
}

/// Exact content-addressed TSJS release transport.
///
/// # Errors
///
/// This function never returns an error; the Result type is for API consistency.
pub fn handle_tsjs_dynamic(
    req: &Request<EdgeBody>,
    integration_registry: &IntegrationRegistry,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    const PREFIX: &str = "/static/tsjs=";

    let path = req.uri().path();
    if req.method() != Method::GET || !path.starts_with(PREFIX) {
        return Ok(tsjs_not_found_response());
    }
    let filename = &path[PREFIX.len()..];
    let Some(requested_hash) = req.uri().query().and_then(|query| query.strip_prefix("v=")) else {
        return Ok(tsjs_not_found_response());
    };
    if !valid_lowercase_sha256(requested_hash) {
        return Ok(tsjs_not_found_response());
    }

    if filename == "tsjs-unified.min.js" {
        let creative_ids = crate::tsjs::creative_tsjs_module_ids();
        let module_ids = (trusted_server_js::concatenated_hash(creative_ids) == requested_hash)
            .then_some(creative_ids.to_vec())
            .or_else(|| {
                integration_registry
                    .tsjs_static_transport_selections(false)
                    .into_iter()
                    .map(|selection| integration_registry.tsjs_critical_module_ids(selection))
                    .find(|ids| trusted_server_js::concatenated_hash(ids) == requested_hash)
            });
        let Some(module_ids) = module_ids else {
            return Ok(tsjs_not_found_response());
        };
        let body = trusted_server_js::concatenate_modules(&module_ids);
        let mut resp = serve_static_with_etag(&body, req, "application/javascript; charset=utf-8");
        apply_tsjs_success_headers(&mut resp);
        return Ok(resp);
    }

    if let Some(module_id) = parse_single_module_filename(filename) {
        let render_trace_overlay = crate::trace_cookie::render_trace_overlay_active(req);
        let module_enabled = integration_registry
            .tsjs_static_transport_selections(render_trace_overlay)
            .into_iter()
            .any(|selection| {
                integration_registry
                    .tsjs_deferred_module_ids(selection)
                    .contains(&module_id)
            });
        if module_enabled
            && trusted_server_js::single_module_hash(module_id).as_deref() == Some(requested_hash)
            && let Some(content) = trusted_server_js::module_bundle(module_id)
        {
            let mut resp =
                serve_static_with_etag(content, req, "application/javascript; charset=utf-8");
            apply_tsjs_success_headers(&mut resp);
            return Ok(resp);
        }
    }

    Ok(tsjs_not_found_response())
}

fn valid_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn apply_tsjs_success_headers(response: &mut Response<EdgeBody>) {
    response
        .headers_mut()
        .insert(HEADER_X_COMPRESS_HINT, HeaderValue::from_static("on"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
}

/// Extract a catalogued deferred module ID from its only admitted filename.
///
/// Returns `Some(&'static str)` if the filename matches a known JS module ID,
/// `None` otherwise. The caller must additionally verify that the module is
/// both deferred and enabled via the [`IntegrationRegistry`].
#[must_use]
fn parse_single_module_filename(filename: &str) -> Option<&'static str> {
    let stem = filename
        .strip_prefix("tsjs-")
        .and_then(|s| s.strip_suffix(".min.js"))?;

    trusted_server_js::all_integration_metadata()
        .into_iter()
        .find(|metadata| {
            metadata.id == stem
                && metadata.phase == Some(trusted_server_js::TsjsModulePhase::Deferred)
        })
        .map(|metadata| metadata.id)
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
    suppress_datadome_client_side_tag: bool,
    gpt_diagnostics:
        Option<&'a crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision>,
    render_trace_overlay: bool,
}

struct PublisherBodyProcessor {
    inner: Box<dyn StreamProcessor>,
}

impl PublisherBodyProcessor {
    fn new(
        params: &OwnedProcessResponseParams,
        settings: &Settings,
        integration_registry: &IntegrationRegistry,
    ) -> Result<Self, Report<TrustedServerError>> {
        let is_html = is_html_content_type(&params.content_type);
        let is_rsc_flight =
            content_type_contains_ascii_case_insensitive(&params.content_type, "text/x-component");
        let inner: Box<dyn StreamProcessor> = if is_html {
            Box::new(create_html_stream_processor(HtmlStreamProcessorParams {
                origin_host: &params.origin_host,
                request_host: &params.request_host,
                request_scheme: &params.request_scheme,
                settings,
                integration_registry,
                ad_slots_script: params.ad_slots_script.as_deref().map(str::to_string),
                ad_bids_state: Arc::clone(&params.ad_bids_state),
                suppress_datadome_client_side_tag: params.suppress_datadome_client_side_tag,
                gpt_diagnostics: params.gpt_diagnostics.clone(),
                render_trace_overlay: params.render_trace_overlay,
            })?)
        } else if is_rsc_flight {
            Box::new(RscFlightUrlRewriter::new(
                &params.origin_host,
                &params.origin_url,
                &params.request_host,
                &params.request_scheme,
            ))
        } else {
            Box::new(create_url_replacer(
                &params.origin_host,
                &params.origin_url,
                &params.request_host,
                &params.request_scheme,
            ))
        };

        Ok(Self { inner })
    }
}

impl StreamProcessor for PublisherBodyProcessor {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, std::io::Error> {
        self.inner.process_chunk(chunk, is_last)
    }
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
        "process_response_streaming: content_type={}, content_encoding={}, is_html={}, is_rsc_flight={}",
        params.content_type,
        params.content_encoding,
        is_html,
        is_rsc_flight
    );

    let compression = Compression::from_content_encoding(params.content_encoding);
    let config = PipelineConfig {
        input_compression: compression,
        output_compression: compression,
        chunk_size: 8192,
    };
    // Bound how much decoded gzip output may sit in the heap at once, using the
    // same ceiling the buffered writer enforces on the rewritten output: a gzip
    // bomb is then rejected mid-decode instead of materializing its full
    // expansion first. The bound is per-step, so a large honest body still
    // streams through and only the buffered writer judges the total — otherwise
    // gzip would reject bodies the identity, deflate and brotli paths accept.
    let max_pending_decoded_bytes = params.settings.publisher.max_buffered_body_bytes;

    if is_html {
        let processor = create_html_stream_processor(HtmlStreamProcessorParams {
            origin_host: params.origin_host,
            request_host: params.request_host,
            request_scheme: params.request_scheme,
            settings: params.settings,
            integration_registry: params.integration_registry,
            ad_slots_script: params.ad_slots_script.map(str::to_string),
            ad_bids_state: params.ad_bids_state.clone(),
            suppress_datadome_client_side_tag: params.suppress_datadome_client_side_tag,
            gpt_diagnostics: params.gpt_diagnostics.cloned(),
            render_trace_overlay: params.render_trace_overlay,
        })?;
        StreamingPipeline::new(config, processor)
            .with_max_pending_decoded_bytes(max_pending_decoded_bytes)
            .process(body_as_reader(body)?, output)?;
    } else if is_rsc_flight {
        // RSC Flight responses are length-prefixed (T rows). A naive string replacement will
        // corrupt the stream by changing byte lengths without updating the prefixes.
        let processor = RscFlightUrlRewriter::new(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, processor)
            .with_max_pending_decoded_bytes(max_pending_decoded_bytes)
            .process(body_as_reader(body)?, output)?;
    } else {
        let replacer = create_url_replacer(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, replacer)
            .with_max_pending_decoded_bytes(max_pending_decoded_bytes)
            .process(body_as_reader(body)?, output)?;
    }

    Ok(())
}

async fn process_response_streaming_async<W: Write>(
    body: EdgeBody,
    output: &mut W,
    params: &OwnedProcessResponseParams,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
) -> Result<(), Report<TrustedServerError>> {
    log::debug!(
        "process_response_streaming_async: content_type={}, content_encoding={}",
        params.content_type,
        params.content_encoding
    );

    let compression = Compression::from_content_encoding(&params.content_encoding);
    let mut processor = PublisherBodyProcessor::new(params, settings, integration_registry)?;
    process_body_chunks_async(
        body,
        output,
        &mut processor,
        compression,
        settings.publisher.max_buffered_body_bytes,
    )
    .await
}

/// Pull, decode, process, and encode the next chunk of a no-hold pipeline.
///
/// Returns `Ok(None)` when the source is exhausted; the caller must then emit
/// [`passthrough_finish_segments`]. Shared by the write-sink driver
/// ([`process_body_chunks_async`]) and the lazy publisher body stream so the
/// two no-hold paths cannot drift apart.
async fn passthrough_step<P: StreamProcessor>(
    source: &mut BodyChunkSource,
    decoder: &mut BodyStreamDecoder,
    encoder: &mut BodyStreamEncoder,
    processor: &mut P,
) -> Result<Option<Vec<bytes::Bytes>>, Report<TrustedServerError>> {
    let Some(raw_chunk) = source.next_chunk().await? else {
        return Ok(None);
    };
    let decoded = decoder.decode_chunk(raw_chunk)?;
    if decoded.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut segments = Vec::new();
    if let Some(encoded) = process_and_encode_chunk(
        processor,
        encoder,
        &decoded,
        false,
        "Failed to process chunk",
    )? {
        segments.push(encoded);
    }
    Ok(Some(segments))
}

async fn process_body_chunks_async<W: Write, P: StreamProcessor>(
    body: EdgeBody,
    writer: &mut W,
    processor: &mut P,
    compression: Compression,
    max_body_bytes: usize,
) -> Result<(), Report<TrustedServerError>> {
    let mut decoder = BodyStreamDecoder::new(compression, max_body_bytes);
    let mut encoder = BodyStreamEncoder::new(compression);
    let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE).with_max_bytes(max_body_bytes);

    while let Some(segments) =
        passthrough_step(&mut source, &mut decoder, &mut encoder, processor).await?
    {
        for encoded in segments {
            write_encoded_segment(writer, &encoded)?;
        }
    }

    for encoded in passthrough_finish_segments(processor, &mut decoder, &mut encoder)? {
        write_encoded_segment(writer, &encoded)?;
    }
    writer.flush().change_context(TrustedServerError::Proxy {
        message: "Failed to flush output".to_string(),
    })?;

    Ok(())
}

/// Write one encoded output segment produced by the chunk pipeline.
fn write_encoded_segment<W: Write>(
    writer: &mut W,
    encoded: &[u8],
) -> Result<(), Report<TrustedServerError>> {
    writer
        .write_all(encoded)
        .change_context(TrustedServerError::Proxy {
            message: "Failed to write encoded chunk".to_string(),
        })
}

/// Finalize a no-hold chunk pipeline: drain the decoder tail through the
/// processor, signal end-of-stream to the processor, and emit the encoder
/// trailer. Returns the encoded segments for the caller to emit.
fn passthrough_finish_segments<P: StreamProcessor>(
    processor: &mut P,
    decoder: &mut BodyStreamDecoder,
    encoder: &mut BodyStreamEncoder,
) -> Result<Vec<bytes::Bytes>, Report<TrustedServerError>> {
    let mut segments = Vec::new();
    let decoded_tail = decoder.finish()?;
    if !decoded_tail.is_empty()
        && let Some(encoded) = process_and_encode_chunk(
            processor,
            encoder,
            &decoded_tail,
            false,
            "Failed to process decoded tail",
        )?
    {
        segments.push(encoded);
    }
    if let Some(encoded) = process_and_encode_chunk(
        processor,
        encoder,
        &[],
        true,
        "Failed to finalize processor",
    )? {
        segments.push(encoded);
    }
    let trailer = encoder.finish()?;
    if !trailer.is_empty() {
        segments.push(bytes::Bytes::from(trailer));
    }
    Ok(segments)
}

/// Owns a [`DispatchedAuction`] and logs if it is dropped uncollected.
///
/// The lazy publisher body stream can be dropped at any await point — a
/// client disconnect aborts the transfer mid-body, or the response may never
/// be polled at all. Async telemetry cannot run in `Drop`, so the loss is
/// surfaced in logs; the abandoned-auction telemetry event is only emitted on
/// error paths that can still await (see [`abandon_hold_auction`]).
struct DispatchedAuctionGuard {
    dispatched: Option<DispatchedAuction>,
    /// Stays `true` from dispatch until collection (or telemetry-emitting
    /// abandonment) reaches a terminal result. [`Self::take`] removes the
    /// dispatched auction to hand it to the async collector but deliberately
    /// leaves the guard armed, so a drop *while collection is still pending* —
    /// a client disconnect at the collection await point — still logs the
    /// loss. [`Self::disarm`] clears it only once collection has completed.
    armed: bool,
}

impl DispatchedAuctionGuard {
    fn new(dispatched: DispatchedAuction) -> Self {
        Self {
            dispatched: Some(dispatched),
            armed: true,
        }
    }

    /// Remove the dispatched auction to begin collection. The guard stays armed
    /// until [`Self::disarm`] is called, so a drop before collection reaches a
    /// terminal result is still reported.
    fn take(&mut self) -> Option<DispatchedAuction> {
        self.dispatched.take()
    }

    /// Disarm the drop warning once collection (or telemetry-emitting
    /// abandonment) has reached a terminal result.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DispatchedAuctionGuard {
    fn drop(&mut self) {
        if self.armed {
            log::warn!(
                "Dispatched server-side auction dropped without collection; SSP bid responses discarded (publisher body stream aborted or never polled)"
            );
        }
    }
}

/// Create a unified HTML stream processor.
///
/// Builds the config via [`HtmlProcessorConfig::from_settings`] and then
/// layers the auction-hold streaming fields on top via
/// [`HtmlProcessorConfig::with_ad_state`], so the canonical builder stays the
/// single source of truth: a future field added to `from_settings` is
/// inherited here automatically.
///
/// The returned processor owns its state and borrows none of the arguments.
/// `use<>` states that explicitly: without it, Rust 2024 would have the opaque
/// type capture every input lifetime, forcing callers to keep the settings and
/// registry alive for as long as the processor.
struct HtmlStreamProcessorParams<'a> {
    origin_host: &'a str,
    request_host: &'a str,
    request_scheme: &'a str,
    settings: &'a Settings,
    integration_registry: &'a IntegrationRegistry,
    ad_slots_script: Option<String>,
    ad_bids_state: Arc<Mutex<Option<String>>>,
    suppress_datadome_client_side_tag: bool,
    gpt_diagnostics: Option<crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision>,
    render_trace_overlay: bool,
}

fn create_html_stream_processor(
    params: HtmlStreamProcessorParams<'_>,
) -> Result<impl StreamProcessor + use<>, Report<TrustedServerError>> {
    use crate::html_processor::{HtmlProcessorConfig, create_html_processor};

    let config = HtmlProcessorConfig::from_settings(
        params.settings,
        params.integration_registry,
        params.origin_host,
        params.request_host,
        params.request_scheme,
    )
    .with_ad_state(params.ad_slots_script, params.ad_bids_state)
    .with_gpt_diagnostics(params.gpt_diagnostics)
    .with_render_trace_overlay(params.render_trace_overlay)
    .with_datadome_client_tag_suppression(params.suppress_datadome_client_side_tag);

    Ok(create_html_processor(config))
}

/// Result of publisher request handling, indicating whether the response body
/// should be streamed or has already been buffered.
pub enum PublisherResponse {
    /// Response returned unmodified, ready to send via `send_to_client()`.
    ///
    /// On streaming adapters the unmodified body may still be a live
    /// [`EdgeBody::Stream`] (the origin fetch requested streaming before the
    /// response was classified); it passes through to the client untouched.
    Buffered(Response<EdgeBody>),
    /// Response headers are ready for a streaming response. Covers processable
    /// content on any status (2xx or non-2xx — e.g., branded 404/500 HTML and
    /// error JSON still get URL rewriting) where the encoding is supported.
    /// Post-processors run inside the streaming processor, so processable HTML
    /// is streamed regardless of whether any are registered.
    ///
    /// Adapters with platform streaming support preserve `body` as
    /// [`EdgeBody::Stream`] and attach a lazy processed stream via
    /// [`publisher_response_into_streaming_response`]. Buffered adapters use
    /// [`buffer_publisher_response_async`] and are bounded by
    /// `settings.publisher.max_buffered_body_bytes`.
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
    /// `Content-Length` is preserved — the body is unmodified. Streaming
    /// adapters reattach the origin body directly so non-processable 2xx bodies
    /// can pass through without materializing in WASM memory.
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
    /// Whether to omit Trusted Server's automatic `DataDome` client-side tag.
    pub(crate) suppress_datadome_client_side_tag: bool,
    /// Request-scoped conditional diagnostics delivery decision.
    pub(crate) gpt_diagnostics:
        Option<crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision>,
    /// Server-owned request-scoped render-trace overlay decision.
    pub(crate) render_trace_overlay: bool,
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
/// Buffered adapters (Axum, Cloudflare, Spin, and non-streaming fallbacks) call
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
        PublisherResponse::Buffered(mut response) => {
            // A buffered-unmodified response can carry an origin body (a stream
            // on streaming-capable adapters). A bodiless response (HEAD, 204,
            // 205, 304) must stay bodiless, so drop the body and correct its
            // framing (204/205 Content-Length; HEAD/304 preserved), matching
            // the streaming finalizer.
            if !response_carries_body(method, response.status()) {
                make_response_bodiless(&mut response);
            }
            Ok(response)
        }
        PublisherResponse::Stream {
            mut response,
            body,
            mut params,
        } => {
            if !response_carries_body(method, response.status()) {
                if let Some(dispatched) = params.dispatched_auction.take() {
                    // A bodiless response (HEAD navigation, 304) has no
                    // `</body>` to inject bids into, so the dispatched SSP
                    // requests are wasted. Emit a terminal abandonment event so
                    // the SSP work and quota consumption stay observable instead
                    // of vanishing, matching the streaming finalizer.
                    log::warn!(
                        "Server-side auction dispatched but response is bodiless (method: {}, status: {}); in-flight SSP bid requests will not be collected",
                        method,
                        response.status(),
                    );
                    emit_abandoned_auction(
                        services,
                        params.auction_observation.take(),
                        dispatched,
                        "bodiless_response",
                    )
                    .await;
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

/// Convert a [`PublisherResponse`] into a response that preserves streaming
/// bodies where possible.
///
/// Buffered adapters should keep using [`buffer_publisher_response_async`].
/// Fastly uses this helper before the entry point commits headers, allowing the
/// response body to be pulled lazily by `stream_to_client()`.
///
/// # Errors
///
/// Returns an error if processor construction fails before the streaming body
/// is created; a dispatched auction is abandoned with `processor_init_error`
/// telemetry first, matching the buffered finalizer.
///
/// # Panics
///
/// Panics if an internal streaming auction guard is missing after this helper
/// has committed to collecting it. That state indicates a violated internal
/// ownership invariant.
pub async fn publisher_response_into_streaming_response(
    publisher_response: PublisherResponse,
    method: &Method,
    settings: Arc<Settings>,
    integration_registry: &IntegrationRegistry,
    orchestrator: Arc<AuctionOrchestrator>,
    services: RuntimeServices,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    match publisher_response {
        PublisherResponse::Buffered(mut response) => {
            // Fastly requests the origin body as a stream before the response is
            // classified, so a buffered-unmodified response can still hold an
            // `EdgeBody::Stream`. A bodiless response (HEAD, 204, 205, 304) must
            // stay bodiless — `send_edgezero_response` streams any
            // `EdgeBody::Stream` to the client — so drop the body and correct
            // its framing (204/205 Content-Length; HEAD/304 preserved).
            if !response_carries_body(method, response.status()) {
                make_response_bodiless(&mut response);
            }
            Ok(response)
        }
        PublisherResponse::PassThrough { mut response, body } => {
            if response_carries_body(method, response.status()) {
                *response.body_mut() = body;
            }
            Ok(response)
        }
        PublisherResponse::Stream {
            mut response,
            body,
            mut params,
        } => {
            if !response_carries_body(method, response.status()) {
                if let Some(dispatched) = params.dispatched_auction.take() {
                    // A bodiless response (HEAD navigation, 304) has no
                    // `</body>` to inject bids into, so the dispatched SSP
                    // requests are wasted. Emit a terminal abandonment event so
                    // the SSP work and quota consumption stay observable instead
                    // of vanishing, matching the processor-init-error path and
                    // the buffered finalizer.
                    log::warn!(
                        "Server-side auction dispatched but response is bodiless (method: {}, status: {}); in-flight SSP bid requests will not be collected",
                        method,
                        response.status(),
                    );
                    emit_abandoned_auction(
                        &services,
                        params.auction_observation.take(),
                        dispatched,
                        "bodiless_response",
                    )
                    .await;
                }
                return Ok(response);
            }

            response.headers_mut().remove(header::CONTENT_LENGTH);
            let mut params = *params;
            if let Some(dispatched) = params.dispatched_auction.take() {
                let telemetry = AuctionTelemetryCarry {
                    observation: params.auction_observation.take(),
                    auction_request: params.auction_request.take(),
                };
                let mut guard = DispatchedAuctionGuard::new(dispatched);
                let dispatched = guard
                    .take()
                    .expect("should have dispatched auction to collect before HTML");
                if is_html_content_type(&params.content_type) {
                    let collect_refs = AuctionCollectDeps {
                        price_granularity: params.price_granularity,
                        ad_bids_state: &params.ad_bids_state,
                        browser_slots_json: params.ad_slots_script.as_deref(),
                        orchestrator: &orchestrator,
                        services: &services,
                        settings: &settings,
                        request_origin: request_origin(
                            &params.request_scheme,
                            &params.request_host,
                        ),
                    };
                    collect_stream_auction(dispatched, telemetry, &collect_refs).await;
                } else {
                    collect_non_html_auction(
                        dispatched,
                        telemetry,
                        &params,
                        &orchestrator,
                        &services,
                        &settings,
                    )
                    .await;
                }
                guard.disarm();
            }
            let mut processor =
                PublisherBodyProcessor::new(&params, &settings, integration_registry)?;
            let stream = async_stream::try_stream! {
                let compression = Compression::from_content_encoding(&params.content_encoding);
                let max_body_bytes = settings.publisher.max_buffered_body_bytes;
                let mut decoder = BodyStreamDecoder::new(compression, max_body_bytes);
                let mut encoder = BodyStreamEncoder::new(compression);
                let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE)
                    .with_max_bytes(max_body_bytes);

                while let Some(segments) = passthrough_step(
                    &mut source,
                    &mut decoder,
                    &mut encoder,
                    &mut processor,
                )
                .await
                .map_err(publisher_stream_error)?
                {
                    for encoded in segments {
                        yield encoded;
                    }
                }
                for encoded in
                    passthrough_finish_segments(&mut processor, &mut decoder, &mut encoder)
                        .map_err(publisher_stream_error)?
                {
                    yield encoded;
                }
            };
            *response.body_mut() = EdgeBody::from_stream::<_, std::io::Error>(stream);
            Ok(response)
        }
    }
}

/// Returns `true` when a buffered publisher response should carry a body and a
/// recomputed `Content-Length`.
///
/// `HEAD` responses and bodiless statuses (204, 205, 304) carry no body;
/// rewriting their `Content-Length` to the (empty) buffered length — or
/// streaming an origin body for them at all — would mislead clients and caches
/// and violate HTTP framing, so the origin metadata is preserved and the body
/// is dropped instead.
fn response_carries_body(method: &Method, status: StatusCode) -> bool {
    *method != Method::HEAD
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::RESET_CONTENT
        && status != StatusCode::NOT_MODIFIED
}

/// Prevent shared caches from replaying tag-suppressed HTML to other clients.
fn apply_datadome_client_tag_cache_privacy(
    response: &mut Response<EdgeBody>,
    method: &Method,
    suppress_datadome_client_side_tag: bool,
    content_type: &str,
) {
    if !suppress_datadome_client_side_tag
        || !response_carries_body(method, response.status())
        || !is_html_content_type(content_type)
    {
        return;
    }

    let already_uncacheable = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .map(str::to_ascii_lowercase)
        .is_some_and(|value| value.contains("private") || value.contains("no-store"));
    if !already_uncacheable {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=0"),
        );
    }
    for header_name in CDN_CACHE_HEADERS {
        response.headers_mut().remove(*header_name);
    }
}

/// Drop a bodiless response's body and correct its framing headers.
///
/// The response keeps no body, and its `Content-Length` is corrected where the
/// origin's value would be invalid for the now-empty message:
/// - **204 No Content**: RFC 9110 §8.6 forbids `Content-Length`; remove it.
/// - **205 Reset Content**: the reset response carries nothing, so a nonzero
///   origin length is wrong (RFC 9110 §15.4.6); normalize it to `0`.
/// - **HEAD and 304 Not Modified**: `Content-Length` legitimately advertises
///   the `GET` representation length, so it is preserved untouched.
///
/// The normalized 204/205 responses also drop chunked-framing metadata the
/// adapters would otherwise copy verbatim: RFC 9112 §6.1 forbids
/// `Transfer-Encoding` on a 204, and keeping it on a 205 alongside the
/// `Content-Length: 0` inserted above would advertise two conflicting framings.
/// `Trailer` describes fields that only a chunked body can carry, so it goes
/// with it. `HEAD` and 304 keep both for the same reason they keep
/// `Content-Length`: the fields describe the `GET` representation, not this
/// message.
fn make_response_bodiless(response: &mut Response<EdgeBody>) {
    *response.body_mut() = EdgeBody::empty();
    match response.status() {
        StatusCode::NO_CONTENT => {
            response.headers_mut().remove(header::CONTENT_LENGTH);
            remove_chunked_framing_headers(response.headers_mut());
        }
        StatusCode::RESET_CONTENT => {
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(0_u64));
            remove_chunked_framing_headers(response.headers_mut());
        }
        _ => {}
    }
}

/// Remove the framing fields that only apply to a message with a chunked body.
fn remove_chunked_framing_headers(headers: &mut header::HeaderMap) {
    headers.remove(header::TRANSFER_ENCODING);
    headers.remove(header::TRAILER);
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
        suppress_datadome_client_side_tag: params.suppress_datadome_client_side_tag,
        gpt_diagnostics: params.gpt_diagnostics.as_ref(),
        render_trace_overlay: params.render_trace_overlay,
    };
    process_response_streaming(body, output, &borrowed)
}

/// Stream a publisher body after collecting any dispatched auction.
///
/// HTML requires the exact initial auction projection before the first body
/// byte reaches the processor because the immutable boot value is emitted at
/// `<head>`. Non-HTML responses use the same collect-before-body ordering. If
/// `params.dispatched_auction` is `None`, the function uses the ordinary
/// streaming pipeline directly.
///
/// # Errors
///
/// Returns an error if processing fails mid-stream. Headers are already
/// committed at that point; the caller logs and drops the `StreamingBody`.
pub async fn stream_publisher_body_async<W: Write>(
    body: EdgeBody,
    output: &mut W,
    params: &mut OwnedProcessResponseParams,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
) -> Result<(), Report<TrustedServerError>> {
    let Some(dispatched) = params.dispatched_auction.take() else {
        if body.is_stream() {
            return process_response_streaming_async(
                body,
                output,
                params,
                settings,
                integration_registry,
            )
            .await;
        }

        // No auction and already-buffered body — keep the existing sync pipeline.
        return stream_publisher_body(body, output, params, settings, integration_registry);
    };
    let telemetry = AuctionTelemetryCarry {
        observation: params.auction_observation.take(),
        auction_request: params.auction_request.take(),
    };

    if is_html_content_type(&params.content_type) {
        let collect_refs = AuctionCollectDeps {
            price_granularity: params.price_granularity,
            ad_bids_state: &params.ad_bids_state,
            browser_slots_json: params.ad_slots_script.as_deref(),
            orchestrator,
            services,
            settings,
            request_origin: request_origin(&params.request_scheme, &params.request_host),
        };
        collect_stream_auction(dispatched, telemetry, &collect_refs).await;
    } else {
        collect_non_html_auction(
            dispatched,
            telemetry,
            params,
            orchestrator,
            services,
            settings,
        )
        .await;
    }

    if body.is_stream() {
        return process_response_streaming_async(
            body,
            output,
            params,
            settings,
            integration_registry,
        )
        .await;
    }
    stream_publisher_body(body, output, params, settings, integration_registry)
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

#[derive(Debug, Clone, Copy)]
struct ServerSideAdStackConfig {
    ad_templates_enabled: bool,
    auction_enabled: bool,
}

/// Returns true only when the publisher request should run the full
/// server-side ad stack: auction dispatch plus initial ad-slot injection.
fn should_run_server_side_ad_stack(
    is_get: bool,
    is_navigation: bool,
    is_prefetch: bool,
    is_bot: bool,
    has_matched_slots: bool,
    consent_allows_auction: bool,
    config: ServerSideAdStackConfig,
) -> bool {
    is_get
        && is_navigation
        && !is_prefetch
        && !is_bot
        && config.ad_templates_enabled
        && has_matched_slots
        && consent_allows_auction
        && config.auction_enabled
}

fn mint_browser_auction_id(
    identity_generator: &dyn AuctionIdentityGenerator,
) -> Result<String, Report<TrustedServerError>> {
    mint_response_unique_base64url_identity(identity_generator, &mut HashSet::new(), "a1_", 9, 8)
        .ok_or_else(|| {
            Report::new(TrustedServerError::Auction {
                message: "Failed to mint browser auction identity".to_string(),
            })
        })
}

/// Build the request origin (`scheme://host`, where `host` includes any port)
/// used to emit absolute first-party URLs in inline creatives. Returns an empty
/// string when the scheme or host is unknown, so callers fall back to the
/// configured publisher domain.
fn request_origin(scheme: &str, host: &str) -> String {
    if scheme.is_empty() || host.is_empty() {
        String::new()
    } else {
        format!("{scheme}://{host}")
    }
}

/// Write the one exact initial browser projection into the pre-core boot state.
pub(crate) fn write_projection_to_state(
    result: &OrchestrationResult,
    price_granularity: PriceGranularity,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
    settings: &Settings,
    request_origin: &str,
    browser_slots_json: Option<&str>,
) -> HashSet<String> {
    let browser_auction_id = match mint_browser_auction_id(&SystemAuctionIdentityGenerator) {
        Ok(auction_id) => auction_id,
        Err(error) => {
            log::error!("initial browser auction identity failed closed: {error:?}");
            *ad_bids_state.lock().expect("should lock projection state") = Some(
                r#"{"version":1,"auction":{"version":1,"auctionId":"a1_unavailable","results":[]},"slots":[],"bids":[]}"#
                    .to_string(),
            );
            return HashSet::new();
        }
    };
    let projected = coordinated_cutover_v1::build_browser_auction_projection_v1(
        result,
        price_granularity,
        settings,
        request_origin,
        None,
        Some(&browser_auction_id),
        &SystemAuctionIdentityGenerator,
    )
    .and_then(|projection| {
        let slots = browser_slots_json.map_or_else(
            || {
                if projection.projection.auction.results.is_empty() {
                    Ok(Vec::new())
                } else {
                    Err(Report::new(TrustedServerError::Auction {
                        message: "Initial browser slot projection is unavailable".to_string(),
                    }))
                }
            },
            |json| {
                serde_json::from_str::<Vec<BrowserAuctionSlotV1>>(json).map_err(|error| {
                    Report::new(TrustedServerError::Auction {
                        message: format!("Invalid browser slot projection: {error}"),
                    })
                })
            },
        )?;
        coordinated_cutover_v1::attach_browser_slots_v1(projection, slots, request_origin)
    });
    let (json, delivered_winner_slots) = match projected {
        Ok(CanonicalBrowserAuctionProjectionV1 {
            projection, json, ..
        }) => {
            let delivered = projection
                .auction
                .results
                .iter()
                .filter_map(|decision| match decision {
                    SlotAuctionDecisionV1::Winner { slot, .. } => Some(slot.clone()),
                    _ => None,
                })
                .collect();
            (
                String::from_utf8(json).expect("should serialize projection as UTF-8"),
                delivered,
            )
        }
        Err(error) => {
            log::error!("initial browser auction projection failed closed: {error:?}");
            let projection = BrowserAuctionProjectionV1 {
                version: 1,
                auction: AuctionDecisionSetV1 {
                    version: 1,
                    auction_id: browser_auction_id,
                    results: result
                        .decision_set
                        .results
                        .iter()
                        .map(|decision| match decision {
                            SlotAuctionDecisionV1::Winner { slot, .. } => {
                                SlotAuctionDecisionV1::Failed {
                                    slot: slot.clone(),
                                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                                }
                            }
                            decision => decision.clone(),
                        })
                        .collect(),
                },
                slots: Vec::new(),
                bids: Vec::new(),
            };
            (
                serde_json::to_string(&projection)
                    .expect("should serialize fail-closed projection as UTF-8"),
                HashSet::new(),
            )
        }
    };
    *ad_bids_state.lock().expect("should lock projection state") = Some(json);
    delivered_winner_slots
}

/// Maximum serialized size (in bytes) of a dump embedded in the `ts-debug`
/// comment. A PBS response with many bids can carry megabytes of creative
/// markup; cap it so leaving
/// [`auction_html_comment`](crate::settings::DebugConfig::auction_html_comment)
/// enabled cannot bloat every page render without bound.
const MAX_AUCTION_DEBUG_DUMP_BYTES: usize = 256 * 1024;

/// Provider-metadata keys safe to surface in the on-page `ts-debug` dump.
///
/// Fail-closed allowlist: any key not listed — notably `debug`, which carries
/// the resolved `OpenRTB` request (EC ID, `user.ext.eids`, the TC consent string,
/// `device.ip`, and `device.geo`) plus per-bidder `httpcalls` — is dropped so a
/// visitor's identity graph cannot reach the client-readable DOM even when
/// `[integration.prebid].debug` is also enabled. Full debug detail remains
/// available server-side via `log::trace!`.
const DEBUG_DUMP_METADATA_ALLOWLIST: &[&str] = &[
    "drop_reasons",
    "error_type",
    "status",
    "message",
    "responsetimemillis",
    "errors",
    "warnings",
    "bidstatus",
];

/// Per-bid creative preview length (in bytes) in the `ts-debug` dump. Mirrors
/// the 512-byte upstream-body preview the prebid provider logs on an HTTP error
/// (`integrations/prebid.rs`): enough to identify a creative without copying
/// megabytes of `adm` markup into every page render. The full creative still
/// renders via the injected bids `<script>`.
const MAX_BID_CREATIVE_DUMP_BYTES: usize = 512;

/// Truncate `value` to at most `max` bytes on a UTF-8 char boundary, appending
/// a `…(truncated N bytes)` marker when truncation occurred.
fn truncate_with_marker(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let end = value.floor_char_boundary(max);
    format!("{}…(truncated {} bytes)", &value[..end], value.len() - end)
}

/// Build a redacted JSON view of a single provider response for the `ts-debug`
/// dump: only [`DEBUG_DUMP_METADATA_ALLOWLIST`] metadata keys survive, and each
/// bid's creative is previewed to [`MAX_BID_CREATIVE_DUMP_BYTES`].
fn redact_response_for_dump(
    response: &crate::auction::types::AuctionResponse,
) -> serde_json::Value {
    let metadata: serde_json::Map<String, serde_json::Value> = response
        .metadata
        .iter()
        .filter(|(key, _)| DEBUG_DUMP_METADATA_ALLOWLIST.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let bids: Vec<serde_json::Value> = response.bids.iter().map(redact_bid_for_dump).collect();
    serde_json::json!({
        "provider": response.provider,
        "status": response.status,
        "response_time_ms": response.response_time_ms,
        "bids": bids,
        "metadata": metadata,
    })
}

/// Build a redacted JSON view of a single bid: every field except `creative`,
/// which is previewed to [`MAX_BID_CREATIVE_DUMP_BYTES`].
fn redact_bid_for_dump(bid: &crate::auction::types::Bid) -> serde_json::Value {
    let mut value = serde_json::to_value(bid).unwrap_or(serde_json::Value::Null);
    if let Some(creative) = &bid.creative {
        value["creative"] =
            serde_json::Value::String(truncate_with_marker(creative, MAX_BID_CREATIVE_DUMP_BYTES));
    }
    value
}

/// Prepend a `<!-- ts-debug: ... -->` HTML comment carrying a redacted view of
/// the auction result — pipeline stats plus, per provider, its status, bids
/// (each creative previewed to [`MAX_BID_CREATIVE_DUMP_BYTES`]), and allowlisted
/// metadata — onto the shared `ad_bids_state` so it lands directly before the
/// injected bids `<script>`. Identity-bearing metadata (notably prebid's `debug`
/// subtree) is dropped; see [`DEBUG_DUMP_METADATA_ALLOWLIST`]. Gated by
/// [`auction_html_comment`](crate::settings::DebugConfig::auction_html_comment);
/// never enable in production.
///
/// `path_label` differentiates the streaming-with-auction-hold path (`stream`)
/// from the buffered path (`buffered`) in the marker so on-page debugging can
/// tell which code path produced the bids.
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
    // Redacted, bounded, deterministic dump so an operator can see each
    // provider's status, bids, and safe metadata without needing log access.
    //
    // SECURITY: `Bid.creative` and provider metadata are attacker/partner-
    // influenced. Two layers protect the DOM:
    //   1. `redact_response_for_dump` drops all non-allowlisted *response-level*
    //      metadata (notably the identity-bearing `debug` subtree) and previews
    //      each creative, so the visitor's identity graph never enters the
    //      comment and one large creative cannot dominate the payload. Bid-level
    //      fields (`Bid.metadata`, `nurl`, `burl`) are NOT yet allowlisted; they
    //      pass through today because the only writer (`integrations/aps.rs`)
    //      emits opaque targeting keys. Tightening this to a fail-closed bid
    //      allowlist is tracked in #925.
    //   2. `render_dump` below neutralises HTML comment terminators and caps the
    //      total serialized size.
    //
    // `serde_json::Map` (no `preserve_order` feature) is `BTreeMap`-backed, so
    // the rendered metadata keys are sorted — the dump is deterministic even
    // though `AuctionResponse.metadata` is a `HashMap`.
    let mut dump = serde_json::Map::new();
    dump.insert(
        "provider_responses".to_string(),
        serde_json::Value::Array(
            result
                .provider_responses
                .iter()
                .map(redact_response_for_dump)
                .collect(),
        ),
    );
    // Only include the mediator response when one actually ran; otherwise the
    // `mediator=none` on the summary line already conveys it.
    if let Some(mediator_response) = &result.mediator_response {
        dump.insert(
            "mediator_response".to_string(),
            redact_response_for_dump(mediator_response),
        );
    }
    // A single `replace("--", …)` is deliberately NOT used — because
    // `str::replace` is non-overlapping, it re-forms a live `-->` / `--!>` at
    // the junction of an odd dash-run (`--->` -> `- -->`, `----->` -> `- -- -->`),
    // reintroducing exactly the terminator we are trying to remove. The two
    // targeted replacements below cannot re-form either sequence. Applied to the
    // serialize-error fallback too, so nothing reaches the DOM un-neutralised.
    let render_dump = |json: String| -> String {
        let neutralised = json.replace("-->", "-- >").replace("--!>", "-- !>");
        if neutralised.len() > MAX_AUCTION_DEBUG_DUMP_BYTES {
            let end = neutralised.floor_char_boundary(MAX_AUCTION_DEBUG_DUMP_BYTES);
            format!(
                "{}…(truncated {} bytes)",
                &neutralised[..end],
                neutralised.len() - end
            )
        } else {
            neutralised
        }
    };
    // Single serialize → single neutralise → single total-budget cap.
    let dump = render_dump(
        serde_json::to_string(&serde_json::Value::Object(dump))
            .unwrap_or_else(|e| format!("<dump serialize error: {e}>")),
    );
    let debug_comment = format!(
        "<!-- ts-debug: path={path_label} ssp={ssp_count} mediator={mediator_info} winning={} time={}ms\n\
         dump={dump}\n\
         -->",
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

/// Borrowed dependencies of the auction collect step.
///
/// Split from the per-auction state above because `dispatched` and `telemetry`
/// are moved out at collect time while these stay live for the rest of the
/// streaming loop.
struct AuctionCollectDeps<'a> {
    price_granularity: PriceGranularity,
    ad_bids_state: &'a Arc<Mutex<Option<String>>>,
    browser_slots_json: Option<&'a str>,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    settings: &'a Settings,
    /// Trusted request origin (`scheme://host`) for absolute inline creative URLs.
    request_origin: String,
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

/// Collect a dispatched auction before a non-HTML body streams: there is no
/// `</body>` to inject into, so bids are written to state up front and the
/// auction telemetry completes immediately.
async fn collect_non_html_auction(
    dispatched: DispatchedAuction,
    telemetry: AuctionTelemetryCarry,
    params: &OwnedProcessResponseParams,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
    settings: &Settings,
) {
    let placeholder = mediator_placeholder_request();
    let result = orchestrator
        .collect_dispatched_auction(
            dispatched,
            services,
            &make_collect_context(settings, services, &placeholder),
        )
        .await;
    let delivered_winner_slots = write_projection_to_state(
        &result,
        params.price_granularity,
        &params.ad_bids_state,
        settings,
        &request_origin(&params.request_scheme, &params.request_host),
        params.ad_slots_script.as_deref(),
    );
    if let (Some(observation), Some(auction_request)) =
        (telemetry.observation, telemetry.auction_request.as_ref())
    {
        emit_auction_events_best_effort_lazy(services, || {
            build_auction_events(
                observation,
                AuctionTerminalOutcome::Completed {
                    request: auction_request,
                    result: &result,
                    delivered_winner_slots: Some(&delivered_winner_slots),
                },
            )
        })
        .await;
    }
}

// Private orchestration helper used by the streaming response paths.
// `dispatched` and `telemetry` are moved per collect, so they stay by value
// while the rest of the context is borrowed.
async fn collect_stream_auction(
    dispatched: DispatchedAuction,
    telemetry: AuctionTelemetryCarry,
    deps: &AuctionCollectDeps<'_>,
) {
    let AuctionCollectDeps {
        price_granularity,
        ad_bids_state,
        browser_slots_json,
        orchestrator,
        services,
        settings,
        request_origin,
    } = deps;
    log::info!("streaming response: collecting dispatched auction");
    let placeholder = mediator_placeholder_request();
    let collect_ctx = make_collect_context(settings, services, &placeholder);
    let result = orchestrator
        .collect_dispatched_auction(dispatched, services, &collect_ctx)
        .await;
    log::info!(
        "streaming response: collect complete - {} winning bid(s)",
        result.winning_bids.len()
    );
    let delivered_winner_slots = write_projection_to_state(
        &result,
        *price_granularity,
        ad_bids_state,
        settings,
        request_origin,
        *browser_slots_json,
    );
    if let (Some(observation), Some(auction_request)) =
        (telemetry.observation, telemetry.auction_request.as_ref())
    {
        emit_auction_events_best_effort_lazy(services, || {
            build_auction_events(
                observation,
                AuctionTerminalOutcome::Completed {
                    request: auction_request,
                    result: &result,
                    delivered_winner_slots: Some(&delivered_winner_slots),
                },
            )
        })
        .await;
    }

    if settings.debug.auction_html_comment {
        prepend_auction_debug_comment("stream", &result, ad_bids_state);
    }
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
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

    // Adapter fallbacks prepare this before EC/cookie handling. Keep this
    // idempotent call as a direct-handler safety net and for focused tests.
    let gpt_diagnostics =
        crate::integrations::gpt_diagnostics::prepare_request(settings, &mut req)?;
    let render_trace_overlay = crate::trace_cookie::render_trace_overlay_active(&req);

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
            discriminator: None,
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

    let request_path_and_query = origin_path_and_query.to_string();
    let request_path = req.uri().path().to_string();
    let is_get = req.method() == http::Method::GET;

    let is_prefetch = is_prefetch_request(&req);
    let is_bot = is_bot_user_agent(&req);

    let creative_opportunities = settings.creative_opportunities.as_ref();
    let ad_templates_enabled = creative_opportunities.is_some_and(|config| config.enabled);
    let ad_templates_disabled = creative_opportunities.is_some_and(|config| !config.enabled);
    let matched_slots = if is_get && ad_templates_enabled {
        settings
            .creative_opportunities
            .as_ref()
            .map_or_else(Vec::new, |co_config| {
                match_renderable_slots(auction.slots, co_config, &request_path)
            })
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
        ServerSideAdStackConfig {
            ad_templates_enabled,
            auction_enabled: auction.orchestrator.is_enabled(),
        },
    );
    let should_run_auction = should_run_ad_stack;
    // Diagnostic: shows which gate suppresses the server-side auction. Pair with
    // the `EC context: ... jurisdiction=...` line from EC-context construction
    // when `consent_allows_auction=false`.
    log::debug!(
        "server-side ad-stack gate: is_get={is_get} is_navigation={is_navigation} \
         is_prefetch={is_prefetch} is_bot={is_bot} ad_templates_enabled={ad_templates_enabled} \
         matched_slots={} consent_allows_auction={consent_allows_auction} \
         orchestrator_enabled={} -> should_run_auction={should_run_auction}",
        matched_slots.len(),
        auction.orchestrator.is_enabled(),
    );

    if ad_templates_disabled {
        log::debug!("Server-side ad templates are disabled by configuration");
    } else if matched_slots.is_empty() && settings.creative_opportunities.is_some() {
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
        // Telemetry attribution must use the same publisher identity as the
        // outbound bid request. On the navigation path `request_host` is the
        // trusted-server edge host, so using it here would attribute navigation
        // rows to the edge/staging domain while `/auction` rows (built from
        // `AuctionRequest::publisher.domain`) use the configured domain.
        let observation = AuctionObservationContext::from_parts(
            AuctionSource::InitialNavigation,
            &settings.publisher.domain,
            &request_path,
            matched_slots.len(),
            ec_context,
        );

        if should_run_auction {
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &matched_slots,
                request_path_and_query: &request_path_and_query,
            };
            let mut auction_request = build_auction_request(
                &slots_ctx,
                ec_id,
                &consent_context,
                &request_info,
                &settings.publisher.domain,
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
            let skip_reason = if ad_templates_disabled {
                "ad_templates_disabled"
            } else if !auction.orchestrator.is_enabled() {
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

    if should_run_ad_stack {
        req.headers_mut().remove(header::IF_NONE_MATCH);
        req.headers_mut().remove(header::IF_MODIFIED_SINCE);
        req.headers_mut().remove(header::RANGE);
        req.headers_mut().remove(header::IF_RANGE);
    }

    // Only advertise encodings the rewrite pipeline can decode and re-encode.
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
    //
    // Streaming is gated on the capability (unlike the asset-proxy path, which
    // sets the flag unconditionally and tolerates buffered fallback): adapters
    // without streaming support may reject the flag outright rather than
    // silently buffering, which would fail every publisher fetch.
    let request_method = req.method().clone();
    let suppress_datadome_client_side_tag = req
        .extensions()
        .get::<crate::integrations::datadome::DataDomeClientTagSuppressed>()
        .is_some();
    if suppress_datadome_client_side_tag {
        req.headers_mut().remove(header::IF_NONE_MATCH);
        req.headers_mut().remove(header::IF_MODIFIED_SINCE);
    }
    let mut platform_request = PlatformHttpRequest::new(req, backend_name);
    if services.http_client().supports_streaming_responses() {
        platform_request = platform_request.with_stream_response();
    }
    if should_run_ad_stack {
        platform_request = platform_request.with_cache_bypass();
    }

    let mut response = match services.http_client().send(platform_request).await {
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

    if should_run_ad_stack && response.status() == StatusCode::NOT_MODIFIED {
        if let Some(dispatched) = dispatched_auction.take() {
            emit_abandoned_auction(
                services,
                auction_observation.take(),
                dispatched,
                "unexpected_origin_304",
            )
            .await;
        }

        let mut response = Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(header::CACHE_CONTROL, "private, no-store")
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(EdgeBody::from(
                "Publisher origin returned an invalid conditional response",
            ))
            .change_context(TrustedServerError::Proxy {
                message: "failed to build unexpected origin 304 response".to_string(),
            })?;
        crate::integrations::gpt_diagnostics::finalize_response(&gpt_diagnostics, &mut response);
        return Ok(PublisherResponse::Buffered(response));
    }

    crate::integrations::gpt_diagnostics::finalize_response(&gpt_diagnostics, &mut response);

    let ad_slots_script = if should_run_ad_stack {
        settings
            .creative_opportunities
            .as_ref()
            .map(|co_config| build_browser_slots_json(&matched_slots, co_config, &request_path))
    } else {
        None
    };

    // §4.7: HTML with synthesized per-navigation auction state must not be
    // stored or validated as an origin representation. Strip both browser and
    // surrogate validators/cache directives before returning it.
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
    if is_html_content_type(origin_content_type) {
        if should_run_ad_stack {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
            response.headers_mut().remove(header::ETAG);
            response.headers_mut().remove(header::LAST_MODIFIED);
            // Every CDN-targeted cache directive, not just the browser-facing
            // `Cache-Control` above: an origin emitting any of these would otherwise
            // instruct an intermediary to store a synthesized per-navigation
            // document. `Surrogate-Control` and `Fastly-Surrogate-Control` cover
            // Fastly; `CDN-Cache-Control` is the standard targeted field (RFC 9213)
            // and `Cloudflare-CDN-Cache-Control` is the Cloudflare-specific field
            // that overrides it there, so both are needed to close the gap on the
            // Cloudflare adapter.
            for directive in CDN_CACHE_HEADERS {
                response.headers_mut().remove(*directive);
            }
        } else {
            let origin_cache_control = response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                .map(str::to_ascii_lowercase);
            if !origin_cache_control
                .as_deref()
                .is_some_and(|value| value.contains("private") || value.contains("no-store"))
            {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("max-age=60"),
                );
            }
        }
    }

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

            apply_datadome_client_tag_cache_privacy(
                &mut response,
                &request_method,
                suppress_datadome_client_side_tag,
                &content_type,
            );
            let body = std::mem::replace(response.body_mut(), EdgeBody::empty());
            response.headers_mut().remove(header::CONTENT_LENGTH);

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
                    suppress_datadome_client_side_tag,
                    auction_observation,
                    auction_request: auction_request_for_telemetry,
                    dispatched_auction,
                    price_granularity,
                    gpt_diagnostics: Some(gpt_diagnostics),
                    render_trace_overlay,
                }),
            })
        }
    }
}

/// Bundle of the per-request creative-opportunity inputs that travel together.
///
/// Extracted so `build_auction_request` stays under the project's
/// 7-argument cap (`matched_slots` + `request_path_and_query` live for the same
/// request scope and are passed together everywhere).
pub(crate) struct MatchedSlotsContext<'a> {
    pub matched_slots: &'a [crate::creative_opportunities::CreativeOpportunitySlot],
    pub request_path_and_query: &'a str,
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
    publisher_domain: &str,
    user_agent: Option<&str>,
) -> AuctionRequest {
    let slots = slots_ctx
        .matched_slots
        .iter()
        .map(crate::creative_opportunities::CreativeOpportunitySlot::to_ad_slot)
        .collect();
    // Advertise the configured publisher domain (not the incoming edge `Host`)
    // so SSPs, injected creatives, and brand-safety pixels see the publisher's
    // own origin. On the SSAT proxy path `request_info.host` is the trusted
    // server edge host, which must not leak into the bid request.
    let page_candidate = format!(
        "{}://{}{}",
        request_info.scheme, publisher_domain, slots_ctx.request_path_and_query
    );
    let page_url = sanitize_publisher_page_url(Some(&page_candidate), publisher_domain);
    let ec_id = ec_id.filter(|id| !id.is_empty());
    let request_id = ec_id.map_or_else(
        || format!("ts-req-{}", uuid::Uuid::new_v4().simple()),
        |id| format!("ts-{id}"),
    );
    AuctionRequest {
        id: request_id,
        slots,
        publisher: PublisherInfo {
            domain: publisher_domain.to_owned(),
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
            domain: publisher_domain.to_owned(),
            page: page_url,
        }),
        context: std::collections::HashMap::new(),
    }
}

pub(crate) mod coordinated_cutover_v1 {
    use super::*;

    const RENDERER_RESERVATION_BYTES: usize = 16;
    const RENDERER_RESERVATION_COLLISION_RETRIES: usize = 8;

    fn cache_policy_base(policy: &CacheFetchPolicyV1) -> Option<url::Url> {
        if policy.version != 1 {
            return None;
        }
        let canonical =
            crate::auction::formats::coordinated_cutover_v1::canonicalize_cache_fetch_policy_v1(
                &policy.base_url,
            )
            .ok()?;
        url::Url::parse(&canonical.base_url).ok()
    }

    fn cache_source_from_legacy_bid(
        bid: &Bid,
        policy: Option<&CacheFetchPolicyV1>,
    ) -> Option<BidRenderSourceV1> {
        let policy = policy?;
        let mut base = cache_policy_base(policy)?;
        let cache_id = bid.cache_id.as_deref()?;
        if bid.cache_host.as_deref() != base.host_str()
            || bid.cache_path.as_deref() != Some(base.path())
        {
            return None;
        }
        base.query_pairs_mut().append_pair("uuid", cache_id);
        Some(BidRenderSourceV1::Cache(CacheRenderSourceV1 {
            version: 1,
            cache_id: cache_id.to_string(),
            fetch_url: base.to_string(),
            width: bid.width,
            height: bid.height,
        }))
    }

    fn typed_source_matches_cache_policy(
        source: &BidRenderSourceV1,
        policy: Option<&CacheFetchPolicyV1>,
    ) -> bool {
        let BidRenderSourceV1::Cache(source) = source else {
            return true;
        };
        let Some(mut expected) = policy.and_then(cache_policy_base) else {
            return false;
        };
        expected
            .query_pairs_mut()
            .append_pair("uuid", &source.cache_id);
        source.fetch_url == expected.as_str()
    }

    fn project_render_source(
        bid: &Bid,
        settings: &Settings,
        request_origin: &str,
        cache_policy: Option<&CacheFetchPolicyV1>,
    ) -> Option<BidRenderSourceV1> {
        match (&bid.renderer, &bid.creative, &bid.cache_id) {
            (Some(source), None, None)
                if bid.cache_host.is_none()
                    && bid.cache_path.is_none()
                    && typed_source_matches_cache_policy(source, cache_policy) =>
            {
                Some(source.clone())
            }
            (None, Some(raw_creative), None) => {
                let priced = crate::creative::expand_auction_price_macro(
                    raw_creative,
                    bid.price
                        .filter(|price| price.is_finite() && *price >= 0.0)?,
                );
                let adm = crate::creative::process_inline_auction_creative(
                    settings,
                    request_origin,
                    &priced,
                );
                (!adm.is_empty()).then_some(BidRenderSourceV1::Adm(AdmRenderSourceV1 {
                    version: 1,
                    adm,
                    width: bid.width,
                    height: bid.height,
                }))
            }
            (None, None, Some(_)) => cache_source_from_legacy_bid(bid, cache_policy),
            _ => None,
        }
    }

    fn project_targeting(
        bid: &Bid,
        cpm: f64,
        granularity: PriceGranularity,
    ) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("hb_bidder".to_string(), bid.bidder.clone()),
            ("hb_pb".to_string(), price_bucket(cpm, granularity)),
        ])
    }

    /// Build the exact immutable browser projection without publishing it.
    pub(crate) fn build_browser_auction_projection_v1(
        result: &OrchestrationResult,
        granularity: PriceGranularity,
        settings: &Settings,
        request_origin: &str,
        cache_policy: Option<&CacheFetchPolicyV1>,
        browser_auction_id: Option<&str>,
        identity_generator: &dyn AuctionIdentityGenerator,
    ) -> Result<CanonicalBrowserAuctionProjectionV1, Report<TrustedServerError>> {
        let mut reservation_ids = HashSet::new();
        let mut projected_results = Vec::with_capacity(result.decision_set.results.len());
        let mut projected_bids = Vec::new();

        for decision in &result.decision_set.results {
            let SlotAuctionDecisionV1::Winner { slot, candidate_id } = decision else {
                projected_results.push(decision.clone());
                continue;
            };
            let Some(bid) = result.winning_bids.get(slot).filter(|bid| {
                bid.slot_id == *slot && bid.candidate_id.as_deref() == Some(candidate_id.as_str())
            }) else {
                projected_results.push(SlotAuctionDecisionV1::Failed {
                    slot: slot.clone(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                });
                continue;
            };
            let Some(cpm) = bid.price.filter(|price| price.is_finite() && *price >= 0.0) else {
                projected_results.push(SlotAuctionDecisionV1::Failed {
                    slot: slot.clone(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                });
                continue;
            };
            let Some(provider) = bid.candidate_provider.clone() else {
                projected_results.push(SlotAuctionDecisionV1::Failed {
                    slot: slot.clone(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                });
                continue;
            };
            let Some(upstream_bid_id) = bid.bid_id.clone().filter(|id| !id.is_empty()) else {
                projected_results.push(SlotAuctionDecisionV1::Failed {
                    slot: slot.clone(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                });
                continue;
            };
            let Some(render_source) =
                project_render_source(bid, settings, request_origin, cache_policy)
            else {
                projected_results.push(SlotAuctionDecisionV1::Failed {
                    slot: slot.clone(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                });
                continue;
            };
            let Some(renderer_reservation_id) = mint_response_unique_base64url_identity(
                identity_generator,
                &mut reservation_ids,
                "r1_",
                RENDERER_RESERVATION_BYTES,
                RENDERER_RESERVATION_COLLISION_RETRIES,
            ) else {
                projected_results.push(SlotAuctionDecisionV1::Failed {
                    slot: slot.clone(),
                    reason: AuctionSlotFailureReason::IdentityGenerationFailed,
                });
                continue;
            };

            projected_results.push(decision.clone());
            projected_bids.push(BrowserAuctionBidV1 {
                candidate_id: candidate_id.clone(),
                slot: slot.clone(),
                provider,
                upstream_bid_id,
                cpm,
                currency: bid.currency.clone(),
                targeting: project_targeting(bid, cpm, granularity),
                renderer_reservation_id,
                render_source,
            });
        }

        crate::auction::formats::coordinated_cutover_v1::canonicalize_browser_auction_projection_v1(
            BrowserAuctionProjectionV1 {
                version: 1,
                auction: crate::auction::types::AuctionDecisionSetV1 {
                    version: 1,
                    auction_id: browser_auction_id
                        .unwrap_or(&result.decision_set.auction_id)
                        .to_string(),
                    results: projected_results,
                },
                slots: Vec::new(),
                bids: projected_bids,
            },
            request_origin,
        )
    }

    /// Attach exact ordered GAM slot definitions to an already-canonical auction projection.
    pub(crate) fn attach_browser_slots_v1(
        mut canonical: CanonicalBrowserAuctionProjectionV1,
        slots: Vec<BrowserAuctionSlotV1>,
        request_origin: &str,
    ) -> Result<CanonicalBrowserAuctionProjectionV1, Report<TrustedServerError>> {
        canonical.projection.slots = slots;
        crate::auction::formats::coordinated_cutover_v1::canonicalize_browser_auction_projection_v1(
            canonical.projection,
            request_origin,
        )
    }
}

/// Build the exact ordered GAM placement records carried by the browser projection.
pub(crate) fn build_browser_slots_v1(
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
    request_path: &str,
) -> Vec<BrowserAuctionSlotV1> {
    let section = co_config.section_for_path(request_path);
    matched_slots
        .iter()
        .filter_map(|slot| {
            let gam_unit_path = slot.render_gam_unit_path(&co_config.gam_network_id, &section)?;
            Some(BrowserAuctionSlotV1 {
                slot: slot.id.clone(),
                gam_unit_path,
                div_id: slot.resolved_div_id().to_string(),
                formats: slot
                    .formats
                    .iter()
                    .map(|format| [format.width, format.height])
                    .collect(),
                targeting: slot
                    .targeting
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            })
        })
        .collect()
}

/// Serialize the placement transport held until the initial auction projection is ready.
pub(crate) fn build_browser_slots_json(
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
    request_path: &str,
) -> String {
    serde_json::to_string(&build_browser_slots_v1(
        matched_slots,
        co_config,
        request_path,
    ))
    .expect("should serialize browser slot projection")
}

/// Match creative-opportunity slots and omit dynamic GAM paths that cannot be
/// rendered for this request before they can enter an auction.
fn match_renderable_slots(
    slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
    request_path: &str,
) -> Vec<crate::creative_opportunities::CreativeOpportunitySlot> {
    let section = co_config.section_for_path(request_path);
    crate::creative_opportunities::match_slots(slots, request_path)
        .into_iter()
        .filter_map(|slot| {
            if slot
                .render_gam_unit_path(&co_config.gam_network_id, &section)
                .is_none()
            {
                log::warn!(
                    "Omitting slot `{}`: dynamic gam_unit_path exceeds the render limit for path `{}`",
                    slot.id,
                    request_path
                );
                return None;
            }
            Some(slot.clone())
        })
        .collect()
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

/// Canonical URL path of the SPA re-auction endpoint.
///
/// Lives in the internal `/_ts/` namespace shared by every other Trusted
/// Server route. Adapters register this path; the tsjs SPA hook fetches it.
pub const PAGE_BIDS_PATH: &str = "/_ts/page-bids";

/// Same-origin gate for `/_ts/page-bids`.
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
/// `/_ts/page-bids` endpoint refuses a request — both the CORS preflight
/// (`OPTIONS`) and the GET cross-site gate ([`page_bids_request_allowed`])
/// return this single denial shape.
///
/// The GET handler's [`page_bids_request_allowed`] gate trusts the
/// `X-TSJS-Page-Bids` header precisely because this endpoint never grants a
/// preflight; letting `OPTIONS` fall through to the publisher origin (which may
/// return permissive CORS) would defeat that, allowing a cross-site page to
/// trigger real PBS/APS auctions from a visitor's browser. Every adapter returns
/// this same response for `OPTIONS /_ts/page-bids`.
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
/// The SPA hook sends `location.pathname + location.search`, but the parameter
/// is client-controlled: strip any query string or fragment and force a leading
/// `/` so slot `page_patterns` always match against a canonical path shape.
fn normalize_page_bids_path(raw: &str) -> String {
    let path = raw.split(['?', '#']).next().unwrap_or("");
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Normalize the page path and query retained in the upstream auction context.
///
/// Fragments never reach an HTTP server and are discarded if a client includes
/// one explicitly. Unlike [`normalize_page_bids_path`], the query is preserved
/// for the `OpenRTB` `site.page` URL and is not used for slot matching.
fn normalize_page_bids_path_and_query(raw: &str) -> String {
    let path_and_query = raw.split('#').next().unwrap_or("");
    if path_and_query.starts_with('/') {
        path_and_query.to_string()
    } else {
        format!("/{path_and_query}")
    }
}

fn page_bids_terminal_projection_v1(
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    browser_slots: Vec<BrowserAuctionSlotV1>,
    reason: Option<AuctionSlotFailureReason>,
    request_origin: &str,
) -> Result<CanonicalBrowserAuctionProjectionV1, Report<TrustedServerError>> {
    let auction_id = mint_browser_auction_id(&SystemAuctionIdentityGenerator)?;
    let results = reason.map_or_else(Vec::new, |reason| {
        matched_slots
            .iter()
            .map(|slot| SlotAuctionDecisionV1::Failed {
                slot: slot.id.clone(),
                reason,
            })
            .collect()
    });
    crate::auction::formats::coordinated_cutover_v1::canonicalize_browser_auction_projection_v1(
        BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id,
                results,
            },
            slots: browser_slots,
            bids: Vec::new(),
        },
        request_origin,
    )
}

/// Handle `GET /_ts/page-bids?path=<path>` — server-side auction for SPA navigation.
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
    // CSRF-style gate: refuse cross-site invocations before any other work —
    // including the not-configured 404 below, which would otherwise tell a
    // cross-site caller whether this deployment has creative opportunities.
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

    let Some(co_config) = &settings.creative_opportunities else {
        let mut response = Response::new(EdgeBody::from("Creative opportunities not configured"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        return Ok(response);
    };

    // Trusted request origin for absolute inline creative URLs — derived from the
    // origin the visitor is actually on (scheme, host, port), not the configured
    // publisher domain, which cannot carry a port and may differ by subdomain.
    let request_info = RequestInfo::from_request(&req, services.client_info());
    let request_scheme = if request_info.scheme.is_empty() {
        req.uri().scheme_str().unwrap_or("https")
    } else {
        &request_info.scheme
    };
    let request_host = if request_info.host.is_empty() {
        req.uri()
            .authority()
            .map(http::uri::Authority::as_str)
            .unwrap_or(&settings.publisher.domain)
    } else {
        &request_info.host
    };
    let page_bids_request_origin = request_origin(request_scheme, request_host);

    let requested_page = req
        .uri()
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == "path")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_else(|| "/".to_string());
    let path_param = normalize_page_bids_path(&requested_page);
    let page_path_and_query = normalize_page_bids_path_and_query(&requested_page);

    let matched_slots = if co_config.enabled {
        match_renderable_slots(auction.slots, co_config, &path_param)
    } else {
        Vec::new()
    };
    let browser_slots = build_browser_slots_v1(&matched_slots, co_config, &path_param);

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
    let ad_templates_enabled = co_config.enabled;
    if !ad_templates_enabled {
        log::debug!("page-bids: [creative_opportunities].enabled is false — skipping templates");
    } else if !auction_enabled {
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

    // The template switch, [auction].enabled, and a consent denial disable the
    // entire server-side ad stack. In those states the endpoint returns no slots,
    // so the coordinated runtime cannot create or refresh GPT placements.
    // Bot/prefetch requests, by contrast,
    // keep their slot definitions (the placement structure is unchanged) but
    // skip the live auction, matching the existing bot/prefetch behaviour.
    let ad_stack_enabled = ad_templates_enabled && auction_enabled && consent_allows_auction;

    let projection = if matched_slots.is_empty() {
        page_bids_terminal_projection_v1(
            &matched_slots,
            browser_slots.clone(),
            None,
            &page_bids_request_origin,
        )?
    } else {
        // Same publisher identity as the outbound bid request — see the
        // matching note on the initial-navigation observation above.
        let observation = AuctionObservationContext::from_parts(
            AuctionSource::SpaNavigation,
            &settings.publisher.domain,
            &path_param,
            matched_slots.len(),
            ec_context,
        );
        if ad_stack_enabled && !is_bot && !is_prefetch {
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &matched_slots,
                request_path_and_query: &page_path_and_query,
            };
            let mut auction_request = build_auction_request(
                &slots_ctx,
                ec_id,
                consent_context,
                &request_info,
                &settings.publisher.domain,
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
                    let browser_auction_id =
                        mint_browser_auction_id(&SystemAuctionIdentityGenerator)?;
                    let projection = coordinated_cutover_v1::build_browser_auction_projection_v1(
                        &result,
                        co_config.price_granularity,
                        settings,
                        &page_bids_request_origin,
                        None,
                        Some(&browser_auction_id),
                        &SystemAuctionIdentityGenerator,
                    )?;
                    let projection = coordinated_cutover_v1::attach_browser_slots_v1(
                        projection,
                        browser_slots.clone(),
                        &page_bids_request_origin,
                    )?;
                    let delivered_winner_slots = projection
                        .projection
                        .auction
                        .results
                        .iter()
                        .filter_map(|decision| match decision {
                            SlotAuctionDecisionV1::Winner { slot, .. } => Some(slot.clone()),
                            _ => None,
                        })
                        .collect();
                    emit_auction_events_best_effort_lazy(services, || {
                        build_auction_events(
                            observation,
                            AuctionTerminalOutcome::Completed {
                                request: &auction_request,
                                result: &result,
                                delivered_winner_slots: Some(&delivered_winner_slots),
                            },
                        )
                    })
                    .await;
                    projection
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
                    page_bids_terminal_projection_v1(
                        &matched_slots,
                        browser_slots.clone(),
                        Some(AuctionSlotFailureReason::InternalError),
                        &page_bids_request_origin,
                    )?
                }
            }
        } else {
            let skip_reason = if !ad_templates_enabled {
                "ad_templates_disabled"
            } else if !auction_enabled {
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
            let terminal_reason = if !auction_enabled {
                AuctionSlotFailureReason::AuctionDisabled
            } else if !consent_allows_auction {
                AuctionSlotFailureReason::ConsentDenied
            } else {
                AuctionSlotFailureReason::SlotNotEligible
            };
            page_bids_terminal_projection_v1(
                &matched_slots,
                browser_slots,
                Some(terminal_reason),
                &page_bids_request_origin,
            )?
        }
    };

    let mut response = Response::new(EdgeBody::from(projection.json));
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
    use std::future::Future as _;
    use std::io::{self, Read as _, Write as _};

    use brotli::Decompressor;
    use brotli::enc::writer::CompressorWriter;
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;

    use super::*;
    use crate::auction::orchestrator::OrchestrationResult;
    use crate::auction::types::{AdFormat, AdSlot, MediaType};
    use crate::auction::types::{AuctionDecisionSetV1, AuctionDropReason, AuctionResponse};
    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::{
        StubHttpClient, build_services_with_http_client, noop_services,
        noop_services_with_telemetry_sink,
    };
    use crate::test_support::tests::create_test_settings;
    use edgezero_core::body::Body as EdgeBody;
    use http::{Method, Request as HttpRequest, StatusCode, header};
    use std::sync::Arc;

    fn make_test_bid_with_creative(creative: &str) -> Bid {
        Bid {
            slot_id: "slot".to_string(),
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: Some(creative.to_string()),
            adomain: None,
            bidder: "seat".to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: None,
            ad_id: None,
            creative_id: None,
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: Default::default(),
        }
    }

    mod coordinated_cutover_projection_tests {
        use std::collections::{HashMap, VecDeque};
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::*;
        use crate::auction::types::{
            AdmRenderSourceV1, ApsRendererV1, ApsTagType, AuctionIdentityGenerator,
            AuctionSlotFailureReason, BidRenderSourceV1, CacheFetchPolicyV1, CacheRenderSourceV1,
            SlotAuctionDecisionV1,
        };
        use crate::price_bucket::PriceGranularity;
        use base64::Engine as _;

        struct ScriptedIdentityGenerator {
            draws: Mutex<VecDeque<Vec<u8>>>,
            count: AtomicUsize,
        }

        impl ScriptedIdentityGenerator {
            fn new(draws: impl IntoIterator<Item = Vec<u8>>) -> Self {
                Self {
                    draws: Mutex::new(draws.into_iter().collect()),
                    count: AtomicUsize::new(0),
                }
            }
        }

        impl AuctionIdentityGenerator for ScriptedIdentityGenerator {
            fn fill(&self, destination: &mut [u8]) -> Result<(), ()> {
                self.count.fetch_add(1, Ordering::SeqCst);
                let draw = self
                    .draws
                    .lock()
                    .expect("should lock scripted draws")
                    .pop_front()
                    .ok_or(())?;
                if draw.len() != destination.len() {
                    return Err(());
                }
                destination.copy_from_slice(&draw);
                Ok(())
            }
        }

        fn tagged_adm_bid(slot: &str, candidate_id: &str, cpm: f64) -> Bid {
            Bid {
                slot_id: slot.to_string(),
                candidate_id: Some(candidate_id.to_string()),
                candidate_provider: Some("prebid".to_string()),
                renderer_reservation_id: None,
                price: Some(cpm),
                currency: "USD".to_string(),
                creative: None,
                adomain: None,
                bidder: "example_bidder".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                bid_id: Some(format!("upstream-{slot}")),
                ad_id: None,
                creative_id: None,
                renderer: Some(BidRenderSourceV1::Adm(AdmRenderSourceV1 {
                    version: 1,
                    adm: format!("<div>{slot}</div>"),
                    width: 300,
                    height: 250,
                })),
                cache_id: None,
                cache_host: None,
                cache_path: None,
                metadata: HashMap::new(),
            }
        }

        fn tagged_aps_bid(slot: &str, candidate_id: &str, cpm: f64) -> Bid {
            let envelope =
                include_str!("../../trusted-server-js/lib/test/fixtures/aps-renderer-v1.json");
            let mut bid = tagged_adm_bid(slot, candidate_id, cpm);
            bid.candidate_provider = Some("aps".to_string());
            bid.bidder = "aps".to_string();
            bid.bid_id = Some("fictional-selected-bid-id".to_string());
            bid.renderer = Some(BidRenderSourceV1::Aps(ApsRendererV1 {
                version: 1,
                account_id: "example-account-id".to_string(),
                bid_id: "fictional-selected-bid-id".to_string(),
                creative_id: None,
                tag_type: ApsTagType::Iframe,
                creative_url: "https://creative.example/render".to_string(),
                aax_response: base64::engine::general_purpose::STANDARD.encode(envelope),
                width: 300,
                height: 250,
            }));
            bid
        }

        fn result_with_winners(bids: Vec<Bid>) -> OrchestrationResult {
            let results = bids
                .iter()
                .map(|bid| SlotAuctionDecisionV1::Winner {
                    slot: bid.slot_id.clone(),
                    candidate_id: bid
                        .candidate_id
                        .clone()
                        .expect("test winner should have candidate id"),
                })
                .collect();
            OrchestrationResult {
                provider_responses: Vec::new(),
                mediator_response: None,
                winning_bids: bids
                    .into_iter()
                    .map(|bid| (bid.slot_id.clone(), bid))
                    .collect(),
                decision_set: AuctionDecisionSetV1 {
                    version: 1,
                    auction_id: "auction-projection".to_string(),
                    results,
                },
                total_time_ms: 1,
                metadata: HashMap::new(),
            }
        }

        #[test]
        fn projection_preserves_tagged_source_and_uses_one_reservation_on_both_wires() {
            let source = BidRenderSourceV1::Adm(AdmRenderSourceV1 {
                version: 1,
                adm: "<div>slot-1</div>".to_string(),
                width: 300,
                height: 250,
            });
            let result = result_with_winners(vec![tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 2.75)]);
            let generator = ScriptedIdentityGenerator::new([vec![7; 16]]);

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                None,
                &generator,
            )
            .expect("valid winner should project");

            let bid = &canonical.projection.bids[0];
            assert_eq!(bid.candidate_id, "AAAAAAAAAAAA");
            assert_eq!(bid.cpm, 2.75);
            assert_eq!(bid.render_source, source);
            assert_eq!(bid.renderer_reservation_id, "r1_BwcHBwcHBwcHBwcHBwcHBw");
            assert!(
                !serde_json::to_value(&bid.render_source)
                    .expect("render source should serialize")
                    .to_string()
                    .contains("2.75"),
                "selected CPM must not enter the render capability"
            );

            let direct: serde_json::Value = serde_json::from_slice(
                &crate::auction::formats::coordinated_cutover_v1::serialize_trusted_server_auction_response_v1(
                    &canonical,
                )
                .expect("direct wire should serialize"),
            )
            .expect("direct wire should be JSON");
            assert_eq!(
                direct["seatbid"][0]["bid"][0]["id"],
                bid.renderer_reservation_id
            );
        }

        #[test]
        fn initial_html_state_uses_the_exact_projection_json_without_a_legacy_script() {
            let result = result_with_winners(vec![tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 2.75)]);
            let state: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let slots = serde_json::to_string(&vec![BrowserAuctionSlotV1 {
                slot: "slot-1".to_string(),
                gam_unit_path: "/123/slot-1".to_string(),
                div_id: "div-slot-1".to_string(),
                formats: vec![[300, 250]],
                targeting: BTreeMap::from([("pos".to_string(), "atf".to_string())]),
            }])
            .expect("slot projection should serialize");

            let delivered = write_projection_to_state(
                &result,
                PriceGranularity::Dense,
                &state,
                &Settings::default(),
                "https://publisher.example",
                Some(&slots),
            );

            assert_eq!(delivered, HashSet::from(["slot-1".to_owned()]));
            let stored = state
                .lock()
                .expect("should lock projection state")
                .clone()
                .expect("should store projection JSON");
            let projection: serde_json::Value =
                serde_json::from_str(&stored).expect("should store the exact projection shape");
            let browser_auction_id = projection["auction"]["auctionId"]
                .as_str()
                .expect("browser projection should carry an auction identity");
            assert!(browser_auction_id.starts_with("a1_"));
            assert_ne!(
                browser_auction_id, result.decision_set.auction_id,
                "browser-visible identity must not expose an EC-derived upstream request id"
            );
            assert_eq!(projection["slots"][0]["slot"], "slot-1");
            assert_eq!(projection["slots"][0]["gamUnitPath"], "/123/slot-1");
            assert_eq!(projection["bids"].as_array().map(Vec::len), Some(1));
            assert!(!stored.contains("<script"));
            assert!(!stored.contains(".bids"));
            assert!(!stored.contains("adSlots"));
        }

        #[test]
        fn reservation_collision_exhaustion_fails_only_the_affected_winner() {
            let repeated = vec![9; 16];
            let generator = ScriptedIdentityGenerator::new(
                std::iter::once(repeated.clone()).chain(std::iter::repeat_n(repeated, 9)),
            );
            let result = result_with_winners(vec![
                tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 2.0),
                tagged_adm_bid("slot-2", "BBBBBBBBBBBB", 1.0),
            ]);

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                None,
                &generator,
            )
            .expect("collision exhaustion should remain a per-slot decision");

            assert_eq!(generator.count.load(Ordering::SeqCst), 10);
            assert_eq!(canonical.projection.bids.len(), 1);
            assert!(matches!(
                &canonical.projection.auction.results[0],
                SlotAuctionDecisionV1::Winner { slot, .. } if slot == "slot-1"
            ));
            assert_eq!(
                canonical.projection.auction.results[1],
                SlotAuctionDecisionV1::Failed {
                    slot: "slot-2".to_string(),
                    reason: AuctionSlotFailureReason::IdentityGenerationFailed,
                }
            );
        }

        #[test]
        fn aps_projection_preserves_the_validated_descriptor_without_cpm() {
            let result = result_with_winners(vec![tagged_aps_bid("slot-1", "AAAAAAAAAAAA", 4.25)]);
            let source = result.winning_bids["slot-1"]
                .renderer
                .clone()
                .expect("APS source should exist");
            let generator = ScriptedIdentityGenerator::new([vec![3; 16]]);

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                None,
                &generator,
            )
            .expect("valid APS winner should project");

            assert_eq!(canonical.projection.bids[0].render_source, source);
            assert_eq!(canonical.projection.bids[0].cpm, 4.25);
            assert!(
                !serde_json::to_string(&canonical.projection.bids[0].render_source)
                    .expect("APS source should serialize")
                    .contains("4.25")
            );
        }

        #[test]
        fn cache_projection_uses_only_the_frozen_policy_and_preserves_the_uuid() {
            let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
            bid.renderer = None;
            bid.cache_id = Some("f47447a0-b759-4f2f-9887-af458b79b570".to_string());
            bid.cache_host = Some("cache.example".to_string());
            bid.cache_path = Some("/pbc/v1/cache".to_string());
            let result = result_with_winners(vec![bid]);
            let policy = CacheFetchPolicyV1 {
                version: 1,
                base_url: "https://cache.example/pbc/v1/cache".to_string(),
            };
            let generator = ScriptedIdentityGenerator::new([vec![4; 16]]);

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                Some(&policy),
                None,
                &generator,
            )
            .expect("valid cache winner should project");

            assert_eq!(
                canonical.projection.bids[0].render_source,
                BidRenderSourceV1::Cache(CacheRenderSourceV1 {
                    version: 1,
                    cache_id: "f47447a0-b759-4f2f-9887-af458b79b570".to_string(),
                    fetch_url: "https://cache.example/pbc/v1/cache?uuid=f47447a0-b759-4f2f-9887-af458b79b570".to_string(),
                    width: 300,
                    height: 250,
                })
            );

            let without_policy = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                None,
                &ScriptedIdentityGenerator::new([]),
            )
            .expect("missing policy should remain an explicit winner failure");
            assert!(without_policy.projection.bids.is_empty());
            assert_eq!(
                without_policy.projection.auction.results[0],
                SlotAuctionDecisionV1::Failed {
                    slot: "slot-1".to_string(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                }
            );
        }

        #[test]
        fn projection_rejects_an_adm_with_a_coexisting_cache_pointer() {
            let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
            bid.renderer = None;
            bid.creative = Some("<div>creative</div>".to_string());
            bid.cache_id = Some("f47447a0-b759-4f2f-9887-af458b79b570".to_string());
            bid.cache_host = Some("cache.example".to_string());
            bid.cache_path = Some("/pbc/v1/cache".to_string());
            let result = result_with_winners(vec![bid]);
            let policy = CacheFetchPolicyV1 {
                version: 1,
                base_url: "https://cache.example/pbc/v1/cache".to_string(),
            };

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                Some(&policy),
                None,
                &ScriptedIdentityGenerator::new([vec![8; 16]]),
            )
            .expect("ambiguous source should remain an explicit winner failure");

            assert!(canonical.projection.bids.is_empty());
            assert_eq!(
                canonical.projection.auction.results[0],
                SlotAuctionDecisionV1::Failed {
                    slot: "slot-1".to_string(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                }
            );
        }

        #[test]
        fn invalid_targeting_is_rejected_without_truncation() {
            let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
            bid.bidder = "x".repeat(41);
            let result = result_with_winners(vec![bid]);
            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                None,
                &ScriptedIdentityGenerator::new([vec![5; 16]]),
            )
            .expect("invalid winner targeting should remain an explicit slot result");

            assert!(canonical.projection.bids.is_empty());
            assert_eq!(
                canonical.projection.auction.results[0],
                SlotAuctionDecisionV1::Failed {
                    slot: "slot-1".to_string(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                }
            );
            assert!(
                !String::from_utf8(canonical.json)
                    .expect("canonical projection should be UTF-8")
                    .contains(&"x".repeat(40))
            );
        }

        #[test]
        fn blank_upstream_bid_id_fails_only_the_affected_winner() {
            let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
            bid.bid_id = Some(String::new());
            let result = result_with_winners(vec![bid]);

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                None,
                &ScriptedIdentityGenerator::new([]),
            )
            .expect("blank upstream identity should remain an explicit slot failure");

            assert!(canonical.projection.bids.is_empty());
            assert_eq!(
                canonical.projection.auction.results[0],
                SlotAuctionDecisionV1::Failed {
                    slot: "slot-1".to_string(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                }
            );
        }

        #[test]
        fn unavailable_reservation_randomness_is_identity_generation_failed() {
            let result = result_with_winners(vec![tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5)]);
            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                None,
                &ScriptedIdentityGenerator::new([]),
            )
            .expect("CSPRNG failure should remain a per-slot decision");

            assert!(canonical.projection.bids.is_empty());
            assert_eq!(
                canonical.projection.auction.results[0],
                SlotAuctionDecisionV1::Failed {
                    slot: "slot-1".to_string(),
                    reason: AuctionSlotFailureReason::IdentityGenerationFailed,
                }
            );
        }
    }

    /// Build the ts-debug comment for a one-bid auction whose creative is
    /// `creative`, so tests can assert on the rendered dump.
    fn dump_comment_for_creative(creative: &str) -> String {
        let mut bid = make_test_bid_with_creative(creative);
        bid.slot_id = "ad-header-0".to_string();
        let result = OrchestrationResult {
            provider_responses: vec![
                AuctionResponse::no_bid("prebid", 665),
                AuctionResponse::success("aps", vec![bid], 42),
            ],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
            total_time_ms: 665,
            metadata: std::collections::HashMap::new(),
        };
        let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));
        prepend_auction_debug_comment("stream", &result, &state);
        let comment = state
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");
        drop(state);
        comment
    }

    #[test]
    fn auction_debug_comment_dumps_provider_status() {
        let comment = dump_comment_for_creative("<div>plain</div>");
        // Compact (non-pretty) JSON: `"status":"nobid"` with no spaces.
        assert!(
            comment.contains("\"status\":\"nobid\""),
            "should surface the no-bid provider status: {comment}"
        );
        assert!(
            comment.contains("dump={\"provider_responses\":"),
            "should dump the provider_responses payload: {comment}"
        );
        // No mediator ran, so it is omitted (mediator=none already says so).
        assert!(
            !comment.contains("mediator_response"),
            "should omit mediator_response when no mediator ran: {comment}"
        );
    }

    #[test]
    fn auction_debug_comment_projects_typed_drop_reasons() {
        let response = AuctionResponse::no_bid("aps", 12)
            .with_drop_reason(AuctionDropReason::DuplicateUpstreamBidId);
        let result = OrchestrationResult {
            provider_responses: vec![response],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
            total_time_ms: 12,
            metadata: std::collections::HashMap::new(),
        };
        let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));

        prepend_auction_debug_comment("stream", &result, &state);

        let comment = state
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");
        assert!(
            comment.contains("\"drop_reasons\":{\"duplicate_upstream_bid_id\":1}"),
            "typed fixed reason/count metadata should remain visible: {comment}"
        );
    }

    #[test]
    fn auction_debug_comment_never_leaks_provider_debug_metadata() {
        // A provider response whose `debug` metadata mirrors the shape prebid
        // stores verbatim when `[integration.prebid].debug` is on: the resolved
        // OpenRTB request carrying the visitor's identity graph. The dump must
        // drop it — only allowlisted keys may reach the DOM.
        let response = AuctionResponse::error("prebid", 12)
            .with_metadata(
                "debug",
                serde_json::json!({
                    "resolvedrequest": {
                        "user": {
                            "id": "EC-ID-abc123",
                            "consent": "CPtc-TCSTRING-xyz",
                            "ext": { "eids": [{ "source": "example.com",
                                                "uids": [{ "id": "EID-USER-999" }] }] }
                        },
                        "device": { "ip": "203.0.113.77",
                                    "geo": { "lat": 37.7749, "lon": -122.4194 } }
                    }
                }),
            )
            // An allowlisted key must still survive.
            .with_metadata("error_type", serde_json::json!("http_status"));
        let result = OrchestrationResult {
            provider_responses: vec![response],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
            total_time_ms: 12,
            metadata: std::collections::HashMap::new(),
        };
        let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));
        prepend_auction_debug_comment("stream", &result, &state);
        let comment = state
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");

        for needle in [
            "EC-ID-abc123",
            "EID-USER-999",
            "CPtc-TCSTRING-xyz",
            "203.0.113.77",
            "37.7749",
            "resolvedrequest",
        ] {
            assert!(
                !comment.contains(needle),
                "identity/debug value {needle:?} must not reach the page HTML: {comment}"
            );
        }
        assert!(
            comment.contains("\"error_type\":\"http_status\""),
            "allowlisted metadata must still surface: {comment}"
        );
    }

    #[test]
    fn auction_debug_comment_truncates_oversized_creative() {
        // A creative larger than the per-bid preview cap must be truncated with a
        // marker rather than copied verbatim into the page.
        let oversized = "x".repeat(MAX_BID_CREATIVE_DUMP_BYTES * 4);
        let comment = dump_comment_for_creative(&oversized);
        assert!(
            comment.contains("(truncated"),
            "oversized creative should carry a truncation marker: {}",
            &comment[..comment.len().min(200)]
        );
        assert!(
            !comment.contains(&oversized),
            "the full oversized creative must not appear in the comment"
        );
    }

    #[test]
    fn auction_debug_comment_neutralises_every_comment_terminator_vector() {
        // Each vector reaches HTML5 comment-end state via a distinct tokenizer
        // path. A single `replace("--", …)` would re-form a terminator on the
        // odd-dash-run cases; the targeted two-replace must leave the comment's
        // own trailing `-->` as the only surviving terminator and drop `--!>`.
        for creative in [
            "<div>evil-->break</div>",
            "--!><img src=x onerror=alert(1)>",
            "<!--><img src=x onerror=alert(1)>",
            "<!--!><img src=x onerror=alert(1)>",
            "----!><img src=x onerror=alert(1)>",
        ] {
            let comment = dump_comment_for_creative(creative);
            assert_eq!(
                comment.matches("-->").count(),
                1,
                "exactly one `-->` (the terminator) must survive for {creative:?}: {comment}"
            );
            assert!(
                !comment.contains("--!>"),
                "the `--!>` nested terminator must not survive for {creative:?}: {comment}"
            );
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

    fn deflate_encode(input: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(input)
            .expect("should write deflate test input");
        encoder.finish().expect("should finish deflate encoding")
    }

    fn deflate_decode(input: &[u8]) -> Vec<u8> {
        let mut decoder = flate2::read::ZlibDecoder::new(input);
        let mut output = Vec::new();
        decoder
            .read_to_end(&mut output)
            .expect("should decode deflate test output");
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
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: Default::default(),
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
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
    fn ts_console_stream_publisher_body_injects_active_diagnostics_for_materialized_html() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
        settings
            .integrations
            .insert_config("gpt", &serde_json::json!({}))
            .expect("should enable the GPT event provider");
        let integration_registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let mut request = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/article?ts_console=1")
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build activation request");
        let decision =
            crate::integrations::gpt_diagnostics::prepare_request(&settings, &mut request)
                .expect("should prepare diagnostics request");
        let mut params = make_stream_params(&settings, "");
        params.content_type = "text/html".to_owned();
        params.gpt_diagnostics = Some(decision);
        let mut output = Vec::new();

        stream_publisher_body(
            EdgeBody::from("<html><head><title>Example</title></head><body></body></html>"),
            &mut output,
            &params,
            &settings,
            &integration_registry,
        )
        .expect("should process materialized HTML");

        let html = String::from_utf8(output).expect("should produce UTF-8 HTML");
        assert!(
            !html.contains("__tsjs_gpt_diagnostics_active"),
            "should not inject the removed activation flag"
        );
        assert!(
            html.contains(r#""gpt":{"active":true}"#),
            "should activate diagnostics through immutable boot data"
        );
        assert!(html.contains("tsjs-unified.min.js?v="));
        assert!(!html.contains("tsjs-gpt_diagnostics.min.js"));
        assert_eq!(html.matches("history.replaceState").count(), 1);
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
        )
        .await
        .expect("should proxy publisher request")
    }

    struct TsConsolePipelineResult {
        response: Response<EdgeBody>,
        origin_uri: String,
        outbound_cookie: Option<String>,
    }

    async fn run_ts_console_pipeline(
        method: Method,
        destination: &str,
        uri: &str,
        cookie: Option<&str>,
    ) -> TsConsolePipelineResult {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
        settings
            .integrations
            .insert_config("gpt", &serde_json::json!({}))
            .expect("should enable the GPT event provider");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><head><script>publisher()</script></head><body>origin</body></html>".to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let mut request = Request::builder()
            .method(method.clone())
            .uri(uri)
            .header(header::HOST, "publisher.example")
            .header("sec-fetch-dest", destination);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let request = request
            .body(EdgeBody::empty())
            .expect("should build diagnostics pipeline request");
        let publisher_response = run_publisher_proxy(&settings, &services, request).await;
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let response = buffer_publisher_response_async(
            publisher_response,
            &method,
            &settings,
            &registry,
            &orchestrator,
            &services,
        )
        .await
        .expect("should buffer diagnostics pipeline response");
        let outbound_cookie = stub.recorded_request_headers().first().and_then(|headers| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(header::COOKIE.as_str()))
                .map(|(_, value)| value.clone())
        });
        TsConsolePipelineResult {
            response,
            origin_uri: stub
                .recorded_request_uris()
                .into_iter()
                .next()
                .expect("should forward one origin request"),
            outbound_cookie,
        }
    }

    mod ssat_cache_policy_tests {
        use super::*;
        use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
        use crate::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink};
        use crate::creative_opportunities::{CreativeOpportunityFormat, CreativeOpportunitySlot};
        use crate::platform::test_support::{
            NoopConfigStore, NoopGeo, NoopSecretStore, StubBackend,
        };
        use crate::platform::{
            ClientInfo, PlatformError, PlatformHttpClient, PlatformPendingRequest,
            PlatformResponse, PlatformSelectResult,
        };
        use crate::test_support::tests::crate_test_settings_str;

        const ORIGIN_ETAG: &str = "\"origin-tag\"";
        const ORIGIN_LAST_MODIFIED: &str = "Wed, 21 Oct 2015 07:28:00 GMT";
        const UNEXPECTED_304_PROVIDER: &str = "example_navigation_bidder";
        const UNEXPECTED_304_BACKEND: &str = "example-navigation-bidder-backend";

        struct DispatchingTestProvider;

        struct RangeAwareHttpClient {
            stub: StubHttpClient,
        }

        impl RangeAwareHttpClient {
            fn new() -> Self {
                Self {
                    stub: StubHttpClient::new(),
                }
            }
        }

        #[async_trait::async_trait(?Send)]
        impl PlatformHttpClient for RangeAwareHttpClient {
            async fn send(
                &self,
                request: PlatformHttpRequest,
            ) -> Result<PlatformResponse, Report<PlatformError>> {
                if request.request.headers().contains_key(header::RANGE) {
                    self.stub.push_response_with_headers(
                        206,
                        b"<html><body>partial".to_vec(),
                        vec![
                            ("content-type", "text/html; charset=utf-8"),
                            ("content-range", "bytes 0-18/39"),
                        ],
                    );
                } else {
                    self.stub.push_response_with_headers(
                        200,
                        b"<html><body>origin</body></html>".to_vec(),
                        vec![("content-type", "text/html; charset=utf-8")],
                    );
                }
                self.stub.send(request).await
            }

            async fn send_async(
                &self,
                request: PlatformHttpRequest,
            ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
                self.stub.send_async(request).await
            }

            async fn select(
                &self,
                pending_requests: Vec<PlatformPendingRequest>,
            ) -> Result<PlatformSelectResult, Report<PlatformError>> {
                self.stub.select(pending_requests).await
            }
        }

        #[async_trait::async_trait(?Send)]
        impl AuctionProvider for DispatchingTestProvider {
            fn provider_name(&self) -> &'static str {
                UNEXPECTED_304_PROVIDER
            }

            async fn request_bids(
                &self,
                _request: &AuctionRequest,
                context: &AuctionContext<'_>,
            ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
                let request = PlatformHttpRequest::new(
                    HttpRequest::builder()
                        .method(Method::POST)
                        .uri("https://bidder.example.com/navigation-bids")
                        .body(EdgeBody::empty())
                        .expect("should build test provider request"),
                    UNEXPECTED_304_BACKEND,
                );
                context
                    .services
                    .http_client()
                    .send_async(request)
                    .await
                    .change_context(TrustedServerError::Auction {
                        message: "test provider launch failed".to_string(),
                    })
                    .map(ProviderRequestOutcome::pending)
            }

            async fn parse_response(
                &self,
                _response: PlatformResponse,
                _response_time_ms: u64,
            ) -> Result<AuctionResponse, Report<TrustedServerError>> {
                panic!("parse_response must not run for an unexpected origin 304");
            }

            fn timeout_ms(&self) -> u32 {
                100
            }

            fn backend_name(
                &self,
                _services: &RuntimeServices,
                _timeout_ms: u32,
            ) -> Option<String> {
                Some(UNEXPECTED_304_BACKEND.to_string())
            }
        }

        #[derive(Default)]
        struct RecordingTelemetrySink {
            batches: Mutex<Vec<AuctionEventBatch>>,
        }

        #[async_trait::async_trait(?Send)]
        impl AuctionTelemetrySink for RecordingTelemetrySink {
            async fn emit_auction_events(
                &self,
                _services: &RuntimeServices,
                batch: AuctionEventBatch,
            ) -> Result<(), Report<TrustedServerError>> {
                self.batches
                    .lock()
                    .expect("should lock telemetry batches")
                    .push(batch);
                Ok(())
            }
        }

        fn settings_with_enabled_auction_and_creative_opportunities() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n\
                 [creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml)
                .expect("should parse settings with auction and creative opportunities enabled")
        }

        fn settings_with_dispatching_provider() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\nproviders = [\"{UNEXPECTED_304_PROVIDER}\"]\n\n\
                 [creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml)
                .expect("should parse settings with the dispatching test provider")
        }

        fn services_with_telemetry(
            http_client: Arc<dyn crate::platform::PlatformHttpClient>,
            telemetry_sink: Arc<RecordingTelemetrySink>,
        ) -> RuntimeServices {
            let telemetry_sink: Arc<dyn AuctionTelemetrySink> = telemetry_sink;
            RuntimeServices::builder()
                .config_store(Arc::new(NoopConfigStore))
                .secret_store(Arc::new(NoopSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .backend(Arc::new(StubBackend))
                .http_client(http_client)
                .geo(Arc::new(NoopGeo))
                .auction_telemetry_sink(telemetry_sink)
                .client_info(ClientInfo::default())
                .build()
        }

        fn article_slot() -> CreativeOpportunitySlot {
            CreativeOpportunitySlot {
                id: "article-slot".to_string(),
                gam_unit_path: None,
                div_id: None,
                page_patterns: vec!["/article".to_string()],
                formats: vec![CreativeOpportunityFormat {
                    width: 300,
                    height: 250,
                    media_type: MediaType::Banner,
                }],
                floor_price: None,
                targeting: Default::default(),
                providers: Default::default(),
                compiled_patterns: Vec::new(),
                compiled_unit: None,
            }
        }

        fn conditional_navigation_request() -> Request<EdgeBody> {
            HttpRequest::builder()
                .method(Method::GET)
                .uri("https://ts.example.com/article")
                .header(header::HOST, "ts.example.com")
                .header("sec-fetch-dest", "document")
                .header(header::IF_NONE_MATCH, ORIGIN_ETAG)
                .header(header::IF_MODIFIED_SINCE, ORIGIN_LAST_MODIFIED)
                .body(EdgeBody::empty())
                .expect("should build conditional navigation request")
        }

        fn queue_cacheable_html_response(stub: &StubHttpClient) {
            stub.push_response_with_headers(
                200,
                b"<html><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                    ("etag", ORIGIN_ETAG),
                    ("last-modified", ORIGIN_LAST_MODIFIED),
                    ("surrogate-control", "max-age=300"),
                    ("fastly-surrogate-control", "max-age=300"),
                    ("cdn-cache-control", "max-age=300"),
                    ("cloudflare-cdn-cache-control", "max-age=300"),
                ],
            );
        }

        async fn run_with_slots(
            settings: &Settings,
            services: &RuntimeServices,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
        ) -> PublisherResponse {
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            run_with_orchestrator(settings, services, &orchestrator, slots, req).await
        }

        async fn run_with_orchestrator(
            settings: &Settings,
            services: &RuntimeServices,
            orchestrator: &AuctionOrchestrator,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
        ) -> PublisherResponse {
            let consent = crate::consent::ConsentContext {
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                ..Default::default()
            };
            let mut ec_context = EcContext::new_for_test(None, consent);

            handle_publisher_request(
                settings,
                services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator,
                    slots,
                    registry: None,
                },
                req,
            )
            .await
            .expect("should proxy publisher request")
        }

        fn response_head(response: PublisherResponse) -> http::response::Parts {
            match response {
                PublisherResponse::Buffered(response)
                | PublisherResponse::Stream { response, .. }
                | PublisherResponse::PassThrough { response, .. } => response.into_parts().0,
            }
        }

        fn recorded_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
            headers
                .iter()
                .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }

        #[tokio::test]
        async fn eligible_navigation_bypasses_cache_and_returns_non_storable_html() {
            // Arrange
            let settings = settings_with_enabled_auction_and_creative_opportunities();
            let stub = Arc::new(StubHttpClient::new());
            queue_cacheable_html_response(&stub);
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let slots = [article_slot()];
            let req = conditional_navigation_request();

            // Act
            let response = run_with_slots(&settings, &services, &slots, req).await;
            let response_head = response_head(response);

            // Assert
            assert_eq!(
                stub.recorded_cache_bypass_flags(),
                vec![true],
                "eligible publisher navigation should bypass the platform cache"
            );
            let recorded_requests = stub.recorded_request_headers();
            let outbound_headers = recorded_requests
                .first()
                .expect("should record the outbound publisher request");
            assert_eq!(
                recorded_header(outbound_headers, header::IF_NONE_MATCH.as_str()),
                None,
                "eligible publisher request should not forward If-None-Match"
            );
            assert_eq!(
                recorded_header(outbound_headers, header::IF_MODIFIED_SINCE.as_str()),
                None,
                "eligible publisher request should not forward If-Modified-Since"
            );
            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, no-store"),
                "eligible HTML response should be private and non-storable"
            );
            for header_name in [
                header::ETAG,
                header::LAST_MODIFIED,
                header::HeaderName::from_static("surrogate-control"),
                header::HeaderName::from_static("fastly-surrogate-control"),
                header::HeaderName::from_static("cdn-cache-control"),
                header::HeaderName::from_static("cloudflare-cdn-cache-control"),
            ] {
                assert!(
                    !response_head.headers.contains_key(&header_name),
                    "eligible HTML response should remove {header_name}"
                );
            }
        }

        #[tokio::test]
        async fn eligible_range_navigation_fetches_complete_html() {
            // Arrange
            let settings = settings_with_enabled_auction_and_creative_opportunities();
            let http_client = Arc::new(RangeAwareHttpClient::new());
            let services = build_services_with_http_client(
                Arc::clone(&http_client) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let slots = [article_slot()];
            let mut req = conditional_navigation_request();
            req.headers_mut()
                .insert(header::RANGE, HeaderValue::from_static("bytes=0-18"));
            req.headers_mut()
                .insert(header::IF_RANGE, HeaderValue::from_static(ORIGIN_ETAG));

            // Act
            let response = run_with_slots(&settings, &services, &slots, req).await;
            let response_head = response_head(response);

            // Assert
            assert_eq!(
                response_head.status,
                StatusCode::OK,
                "eligible range navigation should fetch the complete origin document"
            );
            let recorded_requests = http_client.stub.recorded_request_headers();
            let outbound_headers = recorded_requests
                .first()
                .expect("should record the outbound publisher request");
            for header_name in [header::RANGE, header::IF_RANGE] {
                assert_eq!(
                    recorded_header(outbound_headers, header_name.as_str()),
                    None,
                    "eligible publisher request should not forward {header_name}"
                );
            }
        }

        #[tokio::test]
        async fn navigation_without_matched_slots_uses_short_browser_cache_policy() {
            // Arrange
            let settings = settings_with_enabled_auction_and_creative_opportunities();
            let stub = Arc::new(StubHttpClient::new());
            queue_cacheable_html_response(&stub);
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let mut req = conditional_navigation_request();
            req.headers_mut()
                .insert(header::RANGE, HeaderValue::from_static("bytes=0-18"));
            req.headers_mut()
                .insert(header::IF_RANGE, HeaderValue::from_static(ORIGIN_ETAG));

            // Act
            let response = run_with_slots(&settings, &services, &[], req).await;
            let response_head = response_head(response);

            // Assert
            assert_eq!(
                stub.recorded_cache_bypass_flags(),
                vec![false],
                "publisher navigation without matched slots should use the default cache mode"
            );
            let recorded_requests = stub.recorded_request_headers();
            let outbound_headers = recorded_requests
                .first()
                .expect("should record the outbound publisher request");
            assert_eq!(
                recorded_header(outbound_headers, header::IF_NONE_MATCH.as_str()),
                Some(ORIGIN_ETAG),
                "publisher request without matched slots should preserve If-None-Match"
            );
            assert_eq!(
                recorded_header(outbound_headers, header::IF_MODIFIED_SINCE.as_str()),
                Some(ORIGIN_LAST_MODIFIED),
                "publisher request without matched slots should preserve If-Modified-Since"
            );
            assert_eq!(
                recorded_header(outbound_headers, header::RANGE.as_str()),
                Some("bytes=0-18"),
                "publisher request without matched slots should preserve Range"
            );
            assert_eq!(
                recorded_header(outbound_headers, header::IF_RANGE.as_str()),
                Some(ORIGIN_ETAG),
                "publisher request without matched slots should preserve If-Range"
            );

            for (header_name, expected) in [
                (header::CACHE_CONTROL, "max-age=60"),
                (header::ETAG, ORIGIN_ETAG),
                (header::LAST_MODIFIED, ORIGIN_LAST_MODIFIED),
                (
                    header::HeaderName::from_static("surrogate-control"),
                    "max-age=300",
                ),
                (
                    header::HeaderName::from_static("fastly-surrogate-control"),
                    "max-age=300",
                ),
                (
                    header::HeaderName::from_static("cdn-cache-control"),
                    "max-age=300",
                ),
                (
                    header::HeaderName::from_static("cloudflare-cdn-cache-control"),
                    "max-age=300",
                ),
            ] {
                assert_eq!(
                    response_head
                        .headers
                        .get(&header_name)
                        .and_then(|value| value.to_str().ok()),
                    Some(expected),
                    "publisher response without matched slots should preserve {header_name}"
                );
            }
        }

        #[tokio::test]
        async fn eligible_navigation_rejects_unexpected_origin_304() {
            for content_type in [None, Some("text/html; charset=utf-8")] {
                // Arrange
                let settings = settings_with_dispatching_provider();
                let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());
                orchestrator.register_provider(Arc::new(DispatchingTestProvider));
                let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
                let stub = Arc::new(StubHttpClient::new());

                // `send_async` consumes the first response before the publisher
                // origin request consumes the second response.
                stub.push_response(200, b"unused provider response".to_vec());
                let mut origin_headers = vec![
                    ("cache-control", "public, max-age=300"),
                    ("etag", ORIGIN_ETAG),
                    ("last-modified", ORIGIN_LAST_MODIFIED),
                    ("surrogate-control", "max-age=300"),
                    ("fastly-surrogate-control", "max-age=300"),
                ];
                if let Some(content_type) = content_type {
                    origin_headers.push(("content-type", content_type));
                }
                stub.push_response_with_headers(304, Vec::new(), origin_headers);
                let services = services_with_telemetry(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
                    Arc::clone(&telemetry_sink),
                );
                let slots = [article_slot()];

                // Act
                let response = run_with_orchestrator(
                    &settings,
                    &services,
                    &orchestrator,
                    &slots,
                    conditional_navigation_request(),
                )
                .await;

                // Assert
                let response = match response {
                    PublisherResponse::Buffered(response) => response,
                    PublisherResponse::PassThrough { .. } | PublisherResponse::Stream { .. } => {
                        panic!("unexpected origin 304 should return a buffered response")
                    }
                };
                assert_eq!(
                    response.status(),
                    StatusCode::BAD_GATEWAY,
                    "eligible origin 304 should fail closed with or without Content-Type"
                );
                assert_eq!(
                    response
                        .headers()
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some("private, no-store"),
                    "eligible origin 304 should return an explicitly non-storable response"
                );
                for header_name in [
                    header::ETAG,
                    header::LAST_MODIFIED,
                    header::HeaderName::from_static("surrogate-control"),
                    header::HeaderName::from_static("fastly-surrogate-control"),
                ] {
                    assert!(
                        !response.headers().contains_key(&header_name),
                        "eligible origin 304 should not forward {header_name}"
                    );
                }

                let batches = telemetry_sink
                    .batches
                    .lock()
                    .expect("should lock telemetry batches");
                let summary_rows: Vec<_> = batches
                    .iter()
                    .flat_map(AuctionEventBatch::rows)
                    .filter(|row| row.event_kind == "summary")
                    .collect();
                assert_eq!(
                    summary_rows.len(),
                    1,
                    "unexpected origin 304 should emit exactly one summary row"
                );
                assert_eq!(
                    summary_rows[0].terminal_status.as_deref(),
                    Some("abandoned"),
                    "unexpected origin 304 should abandon the dispatched auction"
                );
                assert_eq!(
                    summary_rows[0].terminal_reason.as_deref(),
                    Some("unexpected_origin_304"),
                    "unexpected origin 304 should use the bounded telemetry reason"
                );
            }
        }

        #[tokio::test]
        async fn ts_console_finalizes_session_on_replaced_origin_304_response() {
            let mut settings = settings_with_enabled_auction_and_creative_opportunities();
            settings
                .integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
                .expect("should enable diagnostics");
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response_with_headers(
                304,
                Vec::new(),
                vec![
                    ("cache-control", "public, max-age=300"),
                    ("etag", ORIGIN_ETAG),
                ],
            );
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let slots = [article_slot()];
            let mut req = conditional_navigation_request();
            *req.uri_mut() = "https://ts.example.com/article?keep=1&ts_console=1"
                .parse()
                .expect("should parse activation URI");

            let response = run_with_slots(&settings, &services, &slots, req).await;
            let response = match response {
                PublisherResponse::Buffered(response) => response,
                PublisherResponse::PassThrough { .. } | PublisherResponse::Stream { .. } => {
                    panic!("unexpected origin 304 should return a buffered response")
                }
            };

            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                response.headers()[header::SET_COOKIE],
                "__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax"
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "private, no-store"
            );
            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://origin.test-publisher.com/article?keep=1"]
            );
        }

        #[tokio::test]
        async fn noneligible_origin_304_preserves_conditional_response_metadata() {
            // Arrange
            let settings = settings_with_enabled_auction_and_creative_opportunities();
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response_with_headers(
                304,
                Vec::new(),
                vec![
                    ("cache-control", "public, max-age=300"),
                    ("etag", ORIGIN_ETAG),
                    ("last-modified", ORIGIN_LAST_MODIFIED),
                    ("surrogate-control", "max-age=300"),
                    ("fastly-surrogate-control", "max-age=300"),
                ],
            );
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );

            // Act
            let response =
                run_with_slots(&settings, &services, &[], conditional_navigation_request()).await;

            // Assert
            let response = match response {
                PublisherResponse::Buffered(response) => response,
                PublisherResponse::PassThrough { .. } | PublisherResponse::Stream { .. } => {
                    panic!("noneligible origin 304 should remain buffered")
                }
            };
            assert_eq!(
                response.status(),
                StatusCode::NOT_MODIFIED,
                "noneligible origin 304 should preserve its status"
            );
            for (header_name, expected) in [
                (header::CACHE_CONTROL, "public, max-age=300"),
                (header::ETAG, ORIGIN_ETAG),
                (header::LAST_MODIFIED, ORIGIN_LAST_MODIFIED),
                (
                    header::HeaderName::from_static("surrogate-control"),
                    "max-age=300",
                ),
                (
                    header::HeaderName::from_static("fastly-surrogate-control"),
                    "max-age=300",
                ),
            ] {
                assert_eq!(
                    response
                        .headers()
                        .get(&header_name)
                        .and_then(|value| value.to_str().ok()),
                    Some(expected),
                    "noneligible origin 304 should preserve {header_name}"
                );
            }
            assert_eq!(
                stub.recorded_cache_bypass_flags(),
                vec![false],
                "noneligible publisher navigation should use the default cache mode"
            );
            let recorded_requests = stub.recorded_request_headers();
            let outbound_headers = recorded_requests
                .first()
                .expect("should record the outbound publisher request");
            assert_eq!(
                recorded_header(outbound_headers, header::IF_NONE_MATCH.as_str()),
                Some(ORIGIN_ETAG),
                "noneligible publisher request should preserve If-None-Match"
            );
            assert_eq!(
                recorded_header(outbound_headers, header::IF_MODIFIED_SINCE.as_str()),
                Some(ORIGIN_LAST_MODIFIED),
                "noneligible publisher request should preserve If-Modified-Since"
            );
        }
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
    async fn ts_console_publisher_pipeline_strips_reserved_input_and_finalizes_session() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><head></head><body>origin</body></html>".to_vec(),
            vec![
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "public, max-age=300"),
                ("surrogate-control", "max-age=300"),
            ],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/article?keep=%2F&ts_console=true")
            .header(header::HOST, "publisher.example")
            .header("sec-fetch-dest", "document")
            .header(
                header::COOKIE,
                "other=value; __Host-ts-console=1; second=two",
            )
            .body(EdgeBody::empty())
            .expect("should build diagnostics navigation");

        let response = run_publisher_proxy(&settings, &services, req).await;
        let headers = match response {
            PublisherResponse::Buffered(response)
            | PublisherResponse::PassThrough { response, .. }
            | PublisherResponse::Stream { response, .. } => response.into_parts().0.headers,
        };

        let origin_uri = stub
            .recorded_request_uris()
            .into_iter()
            .next()
            .expect("should forward one publisher request");
        assert!(origin_uri.contains("keep=%2F"));
        assert!(!origin_uri.contains("ts_console"));
        let outbound_headers = stub.recorded_request_headers();
        let outbound_cookies = outbound_headers
            .first()
            .expect("should record publisher request headers")
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(header::COOKIE.as_str()))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(outbound_cookies, vec!["other=value; second=two"]);
        assert_eq!(
            headers[header::SET_COOKIE],
            "__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert!(!headers.contains_key("surrogate-control"));
    }

    #[tokio::test]
    async fn ts_console_publisher_pipeline_duplicate_and_invalid_fail_closed_but_clean_url() {
        for uri in [
            "https://publisher.example/article?keep=a%2Fb&ts_console=1&ts_console=true",
            "https://publisher.example/article?ts_console=True&keep=a%2Fb",
        ] {
            let result = run_ts_console_pipeline(
                Method::GET,
                "document",
                uri,
                Some("__Host-ts-console=1; publisher=value"),
            )
            .await;
            let body = response_body_string(result.response);

            assert_eq!(
                result.origin_uri,
                "https://origin.test-publisher.com/article?keep=a%2Fb"
            );
            assert_eq!(result.outbound_cookie.as_deref(), Some("publisher=value"));
            assert!(body.contains(r#""gpt":{"active":false}"#));
            assert!(!body.contains("tsjs-gpt_diagnostics.min.js"));
            assert!(!body.contains("tsjs-gpt_diagnostics-bootstrap.min.js"));
            assert_eq!(body.matches("history.replaceState").count(), 1);
        }
    }

    #[tokio::test]
    async fn ts_console_publisher_pipeline_cookie_session_and_disable_are_exact() {
        let active = run_ts_console_pipeline(
            Method::GET,
            "document",
            "https://publisher.example/article?keep=%2F",
            Some("publisher=value; __Host-ts-console=1"),
        )
        .await;
        let active_body = response_body_string(active.response);
        assert_eq!(active.outbound_cookie.as_deref(), Some("publisher=value"));
        assert!(active_body.contains(r#""gpt":{"active":true}"#));
        assert!(active_body.contains("tsjs-unified.min.js?v="));
        assert!(!active_body.contains("tsjs-gpt_diagnostics.min.js"));
        assert!(!active_body.contains("tsjs-gpt_diagnostics-bootstrap.min.js"));
        assert!(!active_body.contains("history.replaceState"));

        let disabled = run_ts_console_pipeline(
            Method::GET,
            "document",
            "https://publisher.example/article?ts_console=false&keep=%2F",
            Some("publisher=value; __Host-ts-console=1"),
        )
        .await;
        assert_eq!(
            disabled.response.headers()[header::SET_COOKIE],
            "__Host-ts-console=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"
        );
        assert_eq!(
            disabled.origin_uri,
            "https://origin.test-publisher.com/article?keep=%2F"
        );
        let disabled_body = response_body_string(disabled.response);
        assert!(disabled_body.contains(r#""gpt":{"active":false}"#));
        assert!(!disabled_body.contains("tsjs-gpt_diagnostics.min.js"));
        assert!(!disabled_body.contains("tsjs-gpt_diagnostics-bootstrap.min.js"));
        assert_eq!(disabled_body.matches("history.replaceState").count(), 1);
    }

    #[tokio::test]
    async fn render_trace_cookie_populates_the_immutable_html_boot() {
        let result = run_ts_console_pipeline(
            Method::GET,
            "document",
            "https://publisher.example/article",
            Some("publisher=value; ts-trace=1"),
        )
        .await;
        let body = response_body_string(result.response);

        assert!(
            body.contains(r#""renderTraceOverlay":true"#),
            "the exact server-owned trace cookie must populate DiagnosticsBootV1: {body}"
        );
        assert_eq!(body.matches(r#""renderTraceOverlay""#).count(), 1);
    }

    #[tokio::test]
    async fn ts_console_publisher_pipeline_method_and_document_ineligibility_stay_inert() {
        for (method, destination) in [(Method::POST, "document"), (Method::GET, "script")] {
            let result = run_ts_console_pipeline(
                method,
                destination,
                "https://publisher.example/article?keep=%2F&ts_console=1",
                Some("publisher=value; __Host-ts-console=1"),
            )
            .await;
            assert_eq!(
                result.origin_uri,
                "https://origin.test-publisher.com/article?keep=%2F"
            );
            assert_eq!(result.outbound_cookie.as_deref(), Some("publisher=value"));
            assert!(!result.response.headers().contains_key(header::SET_COOKIE));
            let body = response_body_string(result.response);
            assert!(body.contains(r#""gpt":{"active":false}"#));
            assert!(!body.contains("tsjs-gpt_diagnostics.min.js"));
            assert!(!body.contains("tsjs-gpt_diagnostics-bootstrap.min.js"));
            assert!(!body.contains("history.replaceState"));
        }
    }

    #[tokio::test]
    async fn publisher_origin_fetch_leaves_stream_response_disabled_when_unsupported() {
        let settings = create_test_settings();
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><body>origin</body></html>".to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
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

        assert_eq!(
            stub.recorded_stream_response_flags(),
            vec![false],
            "publisher origin fetch must not request streams when the platform does not support them"
        );
    }

    #[tokio::test]
    async fn publisher_origin_fetch_sets_stream_response_when_supported() {
        let settings = create_test_settings();
        let stub = Arc::new(StubHttpClient::new());
        stub.set_streaming_responses_supported(true);
        stub.push_response_with_headers(
            200,
            b"<html><body>origin</body></html>".to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
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

        assert_eq!(
            stub.recorded_stream_response_flags(),
            vec![true],
            "publisher origin fetch should request streams when the platform supports them"
        );
    }

    #[tokio::test]
    async fn suppressed_datadome_request_drops_origin_validators() {
        let settings = create_test_settings();
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><body>origin</body></html>".to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let mut req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/page")
            .header(header::HOST, "publisher.example")
            .header(header::IF_NONE_MATCH, "\"cached-page\"")
            .header(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2015 07:28:00 GMT")
            .body(EdgeBody::empty())
            .expect("should build conditional request");
        req.extensions_mut()
            .insert(crate::integrations::datadome::DataDomeClientTagSuppressed);

        let _response = run_publisher_proxy(&settings, &services, req).await;

        let headers = stub
            .recorded_request_headers()
            .into_iter()
            .next()
            .expect("should record one outbound request");
        assert!(
            headers.iter().all(|(name, _)| {
                !name.eq_ignore_ascii_case(header::IF_NONE_MATCH.as_str())
                    && !name.eq_ignore_ascii_case(header::IF_MODIFIED_SINCE.as_str())
            }),
            "tag-suppressed origin requests must not revalidate a shared representation"
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
    fn suppressed_datadome_tag_reaches_publisher_html_pipeline() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "datadome",
                &serde_json::json!({
                    "enabled": true,
                    "client_side_key": "test-client-key",
                }),
            )
            .expect("should configure DataDome integration");
        let registry = IntegrationRegistry::new(&settings)
            .expect("should create integration registry with DataDome");
        let mut params = make_stream_params(&settings, "identity");
        params.content_type = "text/html; charset=utf-8".to_string();
        params.suppress_datadome_client_side_tag = true;
        let mut output = Vec::new();

        stream_publisher_body(
            EdgeBody::from(b"<html><head></head><body>content</body></html>".to_vec()),
            &mut output,
            &params,
            &settings,
            &registry,
        )
        .expect("should process suppressed HTML");

        let html = String::from_utf8(output).expect("should produce UTF-8 HTML");
        assert!(
            !html.contains("window.ddjskey"),
            "publisher processing should omit the DataDome client configuration"
        );
        assert!(
            !html.contains("/integrations/datadome/tags.js"),
            "publisher processing should omit the DataDome client tag URL"
        );
    }

    #[test]
    fn suppressed_datadome_html_is_private_and_not_shared_cached() {
        let mut response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "public, max-age=600")
            .header("surrogate-control", "max-age=600")
            .header("fastly-surrogate-control", "max-age=600")
            .header("cdn-cache-control", "max-age=600")
            .header("cloudflare-cdn-cache-control", "max-age=600")
            .body(EdgeBody::empty())
            .expect("should build cacheable HTML response");

        super::apply_datadome_client_tag_cache_privacy(
            &mut response,
            &Method::GET,
            true,
            "text/html; charset=utf-8",
        );

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=0"),
            "suppressed HTML should be private"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "suppressed HTML should not retain Surrogate-Control"
        );
        assert!(
            response.headers().get("fastly-surrogate-control").is_none(),
            "suppressed HTML should not retain Fastly-Surrogate-Control"
        );
        assert!(response.headers().get("cdn-cache-control").is_none());
        assert!(
            response
                .headers()
                .get("cloudflare-cdn-cache-control")
                .is_none()
        );

        let mut no_store_response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "no-store")
            .body(EdgeBody::empty())
            .expect("should build no-store HTML response");
        super::apply_datadome_client_tag_cache_privacy(
            &mut no_store_response,
            &Method::GET,
            true,
            "text/html; charset=utf-8",
        );
        assert_eq!(
            no_store_response.headers()[header::CACHE_CONTROL],
            "no-store"
        );
    }

    #[test]
    fn datadome_cache_privacy_does_not_change_non_html_or_unsuppressed_responses() {
        let mut response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "public, max-age=600")
            .header("surrogate-control", "max-age=600")
            .body(EdgeBody::empty())
            .expect("should build cacheable response");

        super::apply_datadome_client_tag_cache_privacy(
            &mut response,
            &Method::GET,
            false,
            "text/html; charset=utf-8",
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=600"),
            "unsuppressed HTML should retain its existing cache policy"
        );

        super::apply_datadome_client_tag_cache_privacy(
            &mut response,
            &Method::GET,
            true,
            "text/css",
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=600"),
            "non-HTML should retain its existing cache policy"
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
            !super::response_carries_body(&Method::GET, StatusCode::RESET_CONTENT),
            "205 responses must not get a recomputed Content-Length"
        );
        assert!(
            !super::response_carries_body(&Method::GET, StatusCode::NOT_MODIFIED),
            "304 responses must not get a recomputed Content-Length"
        );
    }

    #[test]
    fn dispatched_auction_guard_stays_armed_until_collection_completes() {
        // `take()` hands the dispatched auction to the async collector, but the
        // guard must stay armed across the collection await so a drop while
        // collection is still pending (a client disconnect at the await point)
        // still logs the loss. Only `disarm()` — called once collection reaches
        // a terminal result — clears the warning.
        let mut guard = DispatchedAuctionGuard::new(DispatchedAuction::empty_for_test(
            test_auction_request(),
            10,
        ));
        assert!(guard.armed, "a freshly dispatched guard should be armed");

        let _dispatched = guard
            .take()
            .expect("guard should yield the dispatched auction for collection");
        assert!(
            guard.armed,
            "guard must stay armed across the collection await so a drop mid-collection is reported"
        );

        guard.disarm();
        assert!(
            !guard.armed,
            "guard must disarm once collection reaches a terminal result"
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
        let enabled = ServerSideAdStackConfig {
            ad_templates_enabled: true,
            auction_enabled: true,
        };
        assert!(
            should_run_server_side_ad_stack(true, true, false, false, true, true, enabled),
            "GET, real navigation, matched slots, and consent should run TS ad stack"
        );

        assert!(
            !should_run_server_side_ad_stack(false, true, false, false, true, true, enabled),
            "non-GET requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, false, false, false, true, true, enabled),
            "non-document requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, true, false, true, true, enabled),
            "prefetch requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, true, true, true, enabled),
            "bot requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, false, true, enabled),
            "requests with no matching slots should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, true, false, enabled),
            "requests without required consent should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(
                true,
                true,
                false,
                false,
                true,
                true,
                ServerSideAdStackConfig {
                    ad_templates_enabled: true,
                    auction_enabled: false,
                },
            ),
            "disabled [auction].enabled kill switch should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(
                true,
                true,
                false,
                false,
                true,
                true,
                ServerSideAdStackConfig {
                    ad_templates_enabled: false,
                    auction_enabled: true,
                },
            ),
            "disabled [creative_opportunities].enabled switch should skip TS ad stack"
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

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn tsjs_dynamic_serves_unified_bundle_for_known_filename() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let selection = registry
            .tsjs_static_transport_selections(false)
            .pop()
            .expect("should expose one transport selection");
        let module_ids = registry.tsjs_critical_module_ids(selection);
        let src = crate::tsjs::tsjs_script_src(&module_ids);
        let req = build_request(Method::GET, &format!("https://publisher.example{src}"));

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static(
                "application/javascript; charset=utf-8"
            ))
        );
        assert_eq!(
            response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert!(response.headers().contains_key(header::ETAG));
    }

    #[test]
    fn tsjs_dynamic_serves_the_exact_rewritten_creative_bundle() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let src = crate::tsjs::tsjs_script_src(&["render_runtime", "creative"]);
        let req = build_request(Method::GET, &format!("https://publisher.example{src}"));

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "rewritten creative HTML must reference a transport-admitted artifact"
        );
    }

    #[test]
    fn tsjs_dynamic_preserves_strong_etag_conditional_304() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let selection = registry
            .tsjs_static_transport_selections(false)
            .pop()
            .expect("should expose one transport selection");
        let ids = registry.tsjs_critical_module_ids(selection);
        let src = crate::tsjs::tsjs_script_src(&ids);
        let first = build_request(Method::GET, &format!("https://publisher.example{src}"));
        let first_response =
            handle_tsjs_dynamic(&first, &registry).expect("should serve current release");
        let etag = first_response
            .headers()
            .get(header::ETAG)
            .cloned()
            .expect("should emit an ETag");
        let mut conditional =
            build_request(Method::GET, &format!("https://publisher.example{src}"));
        conditional
            .headers_mut()
            .insert(header::IF_NONE_MATCH, etag.clone());

        let response = handle_tsjs_dynamic(&conditional, &registry)
            .expect("should handle conditional request");

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(header::ETAG), Some(&etag));
        assert_eq!(
            response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
    }

    #[test]
    fn tsjs_dynamic_rejects_noncanonical_transport_locally_without_fallthrough() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let selection = registry
            .tsjs_static_transport_selections(false)
            .pop()
            .expect("should expose one transport selection");
        let ids = registry.tsjs_critical_module_ids(selection);
        let hash = trusted_server_js::concatenated_hash(&ids);
        let cases = [
            (Method::HEAD, format!("tsjs-unified.min.js?v={hash}")),
            (Method::OPTIONS, format!("tsjs-unified.min.js?v={hash}")),
            (Method::POST, format!("tsjs-unified.min.js?v={hash}")),
            (Method::GET, "tsjs-unified.min.js".to_owned()),
            (Method::GET, format!("tsjs-unified.js?v={hash}")),
            (Method::GET, format!("tsjs-unified.min.js?v={hash}&x=1")),
            (Method::GET, format!("tsjs-unified.min.js?x=1&v={hash}")),
            (Method::GET, "tsjs-unified.min.js?v=stale".to_owned()),
            (
                Method::GET,
                format!("tsjs-unified.min.js?v={}", "0".repeat(64)),
            ),
            (
                Method::GET,
                format!("tsjs-unified.min.js?v={}", hash.to_uppercase()),
            ),
            (
                Method::GET,
                format!("tsjs-unknown.min.js?v={}", "0".repeat(64)),
            ),
            (
                Method::GET,
                format!(
                    "tsjs-creative.min.js?v={}",
                    trusted_server_js::single_module_hash("creative")
                        .expect("should hash critical creative")
                ),
            ),
        ];

        for (method, suffix) in cases {
            let req = build_request(
                method,
                &format!("https://publisher.example/static/tsjs={suffix}"),
            );
            let response = handle_tsjs_dynamic(&req, &registry).expect("should reject locally");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "case {suffix}");
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL),
                Some(&HeaderValue::from_static("no-store")),
                "case {suffix}"
            );
            assert!(
                !response.headers().contains_key(header::LOCATION),
                "case {suffix}"
            );
        }
    }

    #[test]
    fn tsjs_dynamic_rejects_critical_diagnostics_as_a_standalone_alias() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let mut req = build_request(
            Method::GET,
            &format!(
                "https://publisher.example/static/tsjs=tsjs-gpt_diagnostics.min.js?v={}",
                trusted_server_js::single_module_hash("gpt_diagnostics")
                    .expect("should hash diagnostics")
            ),
        );
        req.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-console=1"),
        );

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[test]
    fn ts_console_legacy_cleanup_asset_is_not_a_tsjs_release_route() {
        let settings = create_test_settings();
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let req = Request::builder()
            .uri("https://publisher.example/static/tsjs=tsjs-gpt_diagnostics-bootstrap.min.js")
            .body(EdgeBody::empty())
            .expect("should build cleanup asset request");

        let response = handle_tsjs_dynamic(&req, &registry).expect("should serve cleanup asset");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[test]
    fn parse_single_module_filename_extracts_known_id() {
        assert_eq!(
            parse_single_module_filename("tsjs-sourcepoint_lifecycle.min.js"),
            Some("sourcepoint_lifecycle"),
            "should extract a catalogued deferred module from the exact filename"
        );
        assert_eq!(
            parse_single_module_filename("tsjs-sourcepoint_lifecycle.js"),
            None,
            "should reject unminified aliases"
        );
    }

    #[test]
    fn parse_single_module_filename_rejects_unknown_ids() {
        assert_eq!(
            parse_single_module_filename("tsjs-evil.min.js"),
            None,
            "should reject unknown module names"
        );
        assert_eq!(
            parse_single_module_filename("tsjs-core.min.js"),
            None,
            "should reject reserved core as a standalone module"
        );
        assert_eq!(
            parse_single_module_filename("prebid.min.js"),
            None,
            "should reject without tsjs- prefix"
        );
        assert_eq!(
            parse_single_module_filename("tsjs-sourcepoint.txt"),
            None,
            "should reject non-js extension"
        );
    }

    #[test]
    fn tsjs_dynamic_serves_one_enabled_deferred_module_with_exact_hash() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("osano", &serde_json::json!({ "enabled": true }))
            .expect("should enable Osano lifecycle");
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let selection = registry
            .tsjs_static_transport_selections(false)
            .pop()
            .expect("should expose one transport selection");
        let deferred = registry.tsjs_deferred_module_ids(selection);
        let module_id = deferred
            .iter()
            .find(|module_id| **module_id != "diagnostics_presentation")
            .expect("should enable one non-diagnostics deferred module");
        let src = crate::tsjs::tsjs_single_module_script_src(module_id)
            .expect("should name an enabled deferred module");
        let req = build_request(Method::GET, &format!("https://publisher.example{src}"));

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should serve an enabled deferred catalog module"
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

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
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

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
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
            content_encoding: "gzip".to_string(),
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
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
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
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
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
    fn stream_publisher_body_rejects_stream_body_in_sync_path() {
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
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
        };
        let body = EdgeBody::from_stream(futures::stream::iter(vec![Ok::<_, io::Error>(
            bytes::Bytes::from_static(b"<html><body>live</body></html>"),
        )]));
        let mut output = Vec::new();

        let err = stream_publisher_body(body, &mut output, &params, &settings, &registry)
            .expect_err("should reject stream body in sync path");

        assert!(
            format!("{err:?}").contains("streaming body"),
            "should explain that Body::Stream is not supported by the sync path: {err:?}"
        );
    }

    #[test]
    fn body_chunk_source_yields_once_body_in_chunks() {
        futures::executor::block_on(async {
            let body = EdgeBody::from_bytes(bytes::Bytes::from_static(b"abcdef"));
            let mut source = BodyChunkSource::new(body, 3).with_max_bytes(16);

            assert_eq!(
                source.next_chunk().await.expect("should read").as_deref(),
                Some(&b"abc"[..]),
                "should yield the first chunk"
            );
            assert_eq!(
                source.next_chunk().await.expect("should read").as_deref(),
                Some(&b"def"[..]),
                "should yield the second chunk"
            );
            assert!(
                source.next_chunk().await.expect("should read").is_none(),
                "should end after buffered bytes are exhausted"
            );
        });
    }

    #[test]
    fn body_chunk_source_preserves_stream_chunks() {
        futures::executor::block_on(async {
            let body = EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::from_static(b"first"),
                bytes::Bytes::from_static(b"second"),
            ]));
            let mut source = BodyChunkSource::new(body, 3).with_max_bytes(16);

            assert_eq!(
                source.next_chunk().await.expect("should read").as_deref(),
                Some(&b"first"[..]),
                "stream chunks should pass through without re-chunking"
            );
            assert_eq!(
                source.next_chunk().await.expect("should read").as_deref(),
                Some(&b"second"[..]),
                "stream chunks should preserve upstream boundaries"
            );
            assert!(
                source.next_chunk().await.expect("should read").is_none(),
                "should end after stream is exhausted"
            );
        });
    }

    #[test]
    fn body_chunk_source_enforces_cumulative_raw_cap() {
        futures::executor::block_on(async {
            let body = EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::from_static(b"1234"),
                bytes::Bytes::from_static(b"5678"),
            ]));
            let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE).with_max_bytes(6);

            assert!(
                source
                    .next_chunk()
                    .await
                    .expect("first chunk should pass")
                    .is_some(),
                "first chunk should stay under cap"
            );
            let err = source
                .next_chunk()
                .await
                .expect_err("second chunk should exceed cap");

            assert!(
                format!("{err:?}").contains("publisher origin body exceeded"),
                "should report cumulative cap: {err:?}"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_processes_stream_without_auction() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                content_encoding: String::new(),
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
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            let body = EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::from_static(b"body{background:url('https://origin.example.com/"),
                bytes::Bytes::from_static(b"asset.png')}"),
            ]));
            let mut output = Vec::new();

            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("stream body should process on async path");

            let css = String::from_utf8(output).expect("should be valid UTF-8");
            assert!(
                css.contains("proxy.example.com"),
                "should rewrite origin host while streaming. Got: {css}"
            );
            assert!(
                !css.contains("origin.example.com"),
                "should not leave origin host after rewrite. Got: {css}"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_processes_gzip_stream_without_auction() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                content_encoding: "gzip".to_string(),
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
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            let compressed =
                gzip_encode(b"body{background:url('https://origin.example.com/asset.png')}");
            let split_at = compressed.len() / 2;
            let body = EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::copy_from_slice(&compressed[..split_at]),
                bytes::Bytes::copy_from_slice(&compressed[split_at..]),
            ]));
            let mut output = Vec::new();

            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("gzip stream body should process on async path");

            let css = String::from_utf8(gzip_decode(&output)).expect("should be valid UTF-8");
            assert!(
                css.contains("proxy.example.com"),
                "should rewrite origin host while streaming gzip. Got: {css}"
            );
            assert!(
                !css.contains("origin.example.com"),
                "should not leave origin host after gzip rewrite. Got: {css}"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_processes_deflate_stream_without_auction() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                content_encoding: "deflate".to_string(),
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
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            let compressed =
                deflate_encode(b"body{background:url('https://origin.example.com/asset.png')}");
            let split_at = compressed.len() / 2;
            let body = EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::copy_from_slice(&compressed[..split_at]),
                bytes::Bytes::copy_from_slice(&compressed[split_at..]),
            ]));
            let mut output = Vec::new();

            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("deflate stream body should process on async path");

            let css = String::from_utf8(deflate_decode(&output)).expect("should be valid UTF-8");
            assert!(
                css.contains("proxy.example.com"),
                "should rewrite origin host while streaming deflate. Got: {css}"
            );
            assert!(
                !css.contains("origin.example.com"),
                "should not leave origin host after deflate rewrite. Got: {css}"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_processes_brotli_stream_without_auction() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                content_encoding: "br".to_string(),
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
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            let compressed =
                brotli_encode(b"body{background:url('https://origin.example.com/asset.png')}");
            let split_at = compressed.len() / 2;
            let body = EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::copy_from_slice(&compressed[..split_at]),
                bytes::Bytes::copy_from_slice(&compressed[split_at..]),
            ]));
            let mut output = Vec::new();

            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("brotli stream body should process on async path");

            let css = String::from_utf8(brotli_decode(&output)).expect("should be valid UTF-8");
            assert!(
                css.contains("proxy.example.com"),
                "should rewrite origin host while streaming brotli. Got: {css}"
            );
            assert!(
                !css.contains("origin.example.com"),
                "should not leave origin host after brotli rewrite. Got: {css}"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_rejects_truncated_brotli_stream() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                content_encoding: "br".to_string(),
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
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            let compressed =
                brotli_encode(b"body{background:url('https://origin.example.com/asset.png')}");
            let truncated = &compressed[..compressed.len() - 3];
            let body =
                EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::copy_from_slice(
                    truncated,
                )]));
            let mut output = Vec::new();

            let err = stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect_err("truncated brotli stream must fail instead of truncating silently");

            assert!(
                format!("{err:?}").contains("brotli"),
                "should surface the brotli finalization failure: {err:?}"
            );
        });
    }

    fn non_html_stream_params(content_encoding: &str) -> OwnedProcessResponseParams {
        OwnedProcessResponseParams {
            content_encoding: content_encoding.to_string(),
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
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
        }
    }

    #[test]
    fn stream_publisher_body_async_rejects_truncated_gzip_stream() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = non_html_stream_params("gzip");
            let compressed =
                gzip_encode(b"body{background:url('https://origin.example.com/asset.png')}");
            let truncated = &compressed[..compressed.len() - 3];
            let body =
                EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::copy_from_slice(
                    truncated,
                )]));
            let mut output = Vec::new();

            let err = stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect_err("truncated gzip stream must fail instead of truncating silently");

            assert!(
                format!("{err:?}").contains("gzip"),
                "should surface the gzip finalization failure: {err:?}"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_rejects_truncated_deflate_stream() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = non_html_stream_params("deflate");
            let compressed =
                deflate_encode(b"body{background:url('https://origin.example.com/asset.png')}");
            // Cut into the deflate data itself, not just the adler32 trailer.
            let truncated = &compressed[..compressed.len() / 2];
            let body =
                EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::copy_from_slice(
                    truncated,
                )]));
            let mut output = Vec::new();

            let err = stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect_err("truncated deflate stream must fail instead of truncating silently");

            assert!(
                format!("{err:?}").contains("deflate"),
                "should surface the deflate finalization failure: {err:?}"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_enforces_decoded_byte_cap() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            // Raw compressed input stays tiny (well under the cap); only the
            // decoded expansion exceeds it — the decompression-bomb case the
            // raw-byte cap alone cannot catch.
            settings.publisher.max_buffered_body_bytes = 1024;
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = non_html_stream_params("gzip");
            let compressed = gzip_encode(&vec![b'a'; 64 * 1024]);
            assert!(
                compressed.len() < 1024,
                "test precondition: compressed input must stay under the raw cap"
            );
            let body =
                EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::from(compressed)]));
            let mut output = Vec::new();

            let err = stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect_err("decoded expansion past the cap must fail");

            assert!(
                format!("{err:?}").contains("decoded size exceeded"),
                "should report the cumulative decoded cap: {err:?}"
            );
        });
    }

    #[test]
    fn body_chunk_source_resumes_after_cancelled_poll() {
        futures::executor::block_on(async {
            let mut pending_once = true;
            let mut yielded = false;
            let stream = futures::stream::poll_fn(move |cx| {
                if pending_once {
                    pending_once = false;
                    cx.waker().wake_by_ref();
                    return std::task::Poll::Pending;
                }
                if yielded {
                    return std::task::Poll::Ready(None);
                }
                yielded = true;
                std::task::Poll::Ready(Some(Ok::<_, io::Error>(bytes::Bytes::from_static(
                    b"chunk",
                ))))
            });
            let body = EdgeBody::from_stream(stream);
            let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE);

            {
                // Poll the pull future once (Pending), then drop it —
                // simulating a cancelled await (select/timeout wrapper).
                let mut pull = Box::pin(source.next_chunk());
                let waker = futures::task::noop_waker();
                let mut context = std::task::Context::from_waker(&waker);
                assert!(
                    pull.as_mut().poll(&mut context).is_pending(),
                    "first poll should be pending"
                );
            }

            let chunk = source
                .next_chunk()
                .await
                .expect("should read after cancelled poll");
            assert_eq!(
                chunk.as_deref(),
                Some(&b"chunk"[..]),
                "cancelled pull must not lose the origin stream"
            );
        });
    }

    #[test]
    fn stream_publisher_body_async_collects_exact_projection_before_head_boot() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let state = Arc::new(Mutex::new(None));
            let auction_request = test_auction_request();
            let mut params = OwnedProcessResponseParams {
                content_encoding: String::new(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/html; charset=utf-8".to_string(),
                ad_slots_script: Some(
                    r#"<script>(window.tsjs=window.tsjs||{}).adSlots=JSON.parse("[]");</script>"#
                        .to_string(),
                ),
                ad_bids_state: state,
                auction_observation: None,
                auction_request: Some(auction_request.clone()),
                dispatched_auction: Some(DispatchedAuction::empty_for_test(
                    auction_request.clone(),
                    10,
                )),
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            let body = EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::from_static(b"<html><head></head><body>hello"),
                bytes::Bytes::from_static(b"</body></html>"),
            ]));
            let mut output = Vec::new();

            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("stream body with auction should process on async path");

            let html = String::from_utf8(output).expect("should be valid UTF-8");
            assert!(
                html.contains("hello"),
                "should preserve streamed HTML content. Got: {html}"
            );
            assert!(
                html.contains(r#""auctionId":"a1_"#),
                "the immutable pre-core boot must contain the collected auction projection with a browser-only auction id. Got: {html}"
            );
            assert!(
                !html.contains(&format!(r#""auctionId":"{}""#, auction_request.id)),
                "the immutable pre-core boot must not expose the upstream auction id. Got: {html}"
            );
            assert!(
                !html.contains(r#""auctionId":"initial""#),
                "the safe empty projection must not replace a dispatched auction. Got: {html}"
            );
            assert!(!html.contains(".adSlots"));
            assert!(!html.contains(".bids="));
        });
    }

    #[test]
    fn stream_publisher_body_async_auction_hold_decodes_multi_member_gzip_buffered() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let state = Arc::new(Mutex::new(None));
            let mut params = OwnedProcessResponseParams {
                content_encoding: "gzip".to_string(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/html; charset=utf-8".to_string(),
                ad_slots_script: Some(
                    r#"<script>(window.tsjs=window.tsjs||{}).adSlots=JSON.parse("[]");</script>"#
                        .to_string(),
                ),
                ad_bids_state: state,
                auction_observation: None,
                auction_request: Some(test_auction_request()),
                dispatched_auction: Some(DispatchedAuction::empty_for_test(
                    test_auction_request(),
                    10,
                )),
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            // The document tail lives in the SECOND gzip member. This buffered
            // `Body::Once` body proves the hard-cutover pipeline preserves every
            // gzip member after collecting the projection before `<head>`.
            let mut compressed = gzip_encode(b"<html><head></head><body>hello");
            compressed.extend(gzip_encode(b"</body></html>"));
            let body = EdgeBody::from(compressed);
            let mut output = Vec::new();

            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("buffered multi-member gzip auction body should process");

            let html = String::from_utf8(gzip_decode(&output)).expect("should be valid UTF-8");
            assert!(
                html.contains("hello"),
                "should decode the first gzip member. Got: {html}"
            );
            assert!(
                html.contains("</body></html>"),
                "should decode the second gzip member that a single-member decoder drops. Got: {html}"
            );
            assert!(
                html.contains(r#""auctionId":"a1_"#),
                "should emit the collected projection with a browser-only auction id before the head bundle. Got: {html}"
            );
            assert!(
                !html.contains(r#""auctionId":"test-auction""#),
                "should not expose the upstream auction id to the browser. Got: {html}"
            );
            assert!(!html.contains(".bids="));
        });
    }

    #[test]
    fn stream_publisher_body_async_processes_non_html_stream_after_auction_collect() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                content_encoding: String::new(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/css".to_string(),
                ad_slots_script: None,
                ad_bids_state: Arc::new(Mutex::new(None)),
                auction_observation: None,
                auction_request: Some(test_auction_request()),
                dispatched_auction: Some(DispatchedAuction::empty_for_test(
                    test_auction_request(),
                    10,
                )),
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            };
            let body = EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::from_static(
                b"body{background:url('https://origin.example.com/asset.png')}",
            )]));
            let mut output = Vec::new();

            stream_publisher_body_async(
                body,
                &mut output,
                &mut params,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("non-html stream body should process after auction collection");

            let css = String::from_utf8(output).expect("should be valid UTF-8");
            assert!(
                css.contains("proxy.example.com"),
                "should rewrite non-html stream after auction collection. Got: {css}"
            );
            assert!(
                !css.contains("origin.example.com"),
                "should not leave origin host after rewrite. Got: {css}"
            );
        });
    }

    fn drain_streaming_finalize_body(content_encoding: &str, body: EdgeBody) -> Vec<u8> {
        let settings = Arc::new(create_test_settings());
        let registry = Arc::new(
            IntegrationRegistry::new(&settings).expect("should create integration registry"),
        );
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));
        let services = noop_services();
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/css")
            .body(EdgeBody::empty())
            .expect("should build response");
        let params = OwnedProcessResponseParams {
            content_encoding: content_encoding.to_string(),
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
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
        };
        let publisher_response = PublisherResponse::Stream {
            response,
            body,
            params: Box::new(params),
        };

        let response = futures::executor::block_on(publisher_response_into_streaming_response(
            publisher_response,
            &Method::GET,
            Arc::clone(&settings),
            registry.as_ref(),
            orchestrator,
            services,
        ))
        .expect("should build streaming response");

        assert!(
            matches!(response.body(), EdgeBody::Stream(_)),
            "streaming finalize should keep a lazy Body::Stream"
        );

        futures::executor::block_on(
            response
                .into_body()
                .into_bytes_bounded(settings.publisher.max_buffered_body_bytes),
        )
        .expect("streaming body should drain")
        .to_vec()
    }

    #[test]
    fn publisher_response_streaming_finalize_keeps_stream_body_lazy() {
        let body_bytes = drain_streaming_finalize_body(
            "",
            EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::from_static(
                b"body{background:url('https://origin.example.com/asset.png')}",
            )])),
        );
        let css = String::from_utf8(body_bytes).expect("should be valid UTF-8");
        assert!(
            css.contains("proxy.example.com"),
            "streaming response body should still run publisher rewriting. Got: {css}"
        );
        assert!(
            !css.contains("origin.example.com"),
            "streaming response body should not leave origin URLs unrewritten. Got: {css}"
        );
    }

    /// An origin body stream that yields `chunk` once and then stays `Pending`
    /// forever without ever signalling EOF — modelling an origin that has sent
    /// the document head but not yet finished the response.
    fn origin_chunk_then_pending(chunk: bytes::Bytes) -> EdgeBody {
        let mut sent = false;
        EdgeBody::from_stream(futures::stream::poll_fn(move |_cx| {
            if sent {
                return std::task::Poll::Pending;
            }
            sent = true;
            std::task::Poll::Ready(Some(Ok::<_, io::Error>(chunk.clone())))
        }))
    }

    /// Poll a lazy `Body::Stream` exactly once and return the first emitted
    /// chunk. Panics if the first poll is `Pending` (nothing emitted before the
    /// origin would need its next chunk) or the stream ends.
    fn first_lazy_body_chunk(body: EdgeBody) -> bytes::Bytes {
        let mut stream = body
            .into_stream()
            .expect("streaming finalize should keep a lazy Body::Stream");
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let next = futures::StreamExt::next(&mut stream);
        let mut next = std::pin::pin!(next);
        match std::future::Future::poll(next.as_mut(), &mut cx) {
            std::task::Poll::Ready(Some(Ok(bytes))) => bytes,
            std::task::Poll::Ready(other) => {
                panic!("first poll must emit a chunk, got Ready({other:?})")
            }
            std::task::Poll::Pending => {
                panic!("first poll must emit rewritten content before origin EOF, got Pending")
            }
        }
    }

    fn streaming_finalize_response(params: OwnedProcessResponseParams, body: EdgeBody) -> EdgeBody {
        let settings = Arc::new(create_test_settings());
        let registry = Arc::new(
            IntegrationRegistry::new(&settings).expect("should create integration registry"),
        );
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));
        let services = noop_services();
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, &params.content_type)
            .body(EdgeBody::empty())
            .expect("should build response");
        let publisher_response = PublisherResponse::Stream {
            response,
            body,
            params: Box::new(params),
        };
        futures::executor::block_on(publisher_response_into_streaming_response(
            publisher_response,
            &Method::GET,
            Arc::clone(&settings),
            registry.as_ref(),
            orchestrator,
            services,
        ))
        .expect("should build streaming response")
        .into_body()
    }

    fn html_stream_params(
        content_encoding: &str,
        dispatched_auction: Option<DispatchedAuction>,
    ) -> OwnedProcessResponseParams {
        OwnedProcessResponseParams {
            content_encoding: content_encoding.to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=JSON.parse("[]");</script>"#
                    .to_string(),
            ),
            ad_bids_state: Arc::new(Mutex::new(None)),
            auction_observation: None,
            auction_request: dispatched_auction.as_ref().map(|_| test_auction_request()),
            dispatched_auction,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
        }
    }

    #[test]
    fn streaming_finalize_emits_compressed_html_before_origin_eof() {
        // The FCP regression from #849: the lazy body must emit its first
        // rewritten, browser-decodable chunk as soon as the origin delivers one
        // block — not buffer the whole (compressed) response until EOF. The
        // origin here sends one gzip member and then stays Pending; the first
        // poll must still yield gzip bytes that decode to the streamed content.
        // The page exceeds the deflate decoder's internal output buffer so the
        // first decoded block flushes downstream before the (never-arriving)
        // EOF, exactly as a real page does.
        let mut page = b"<html><head></head><body>".to_vec();
        for i in 0..4000 {
            page.extend(format!("<p>hello world paragraph {i}</p>").bytes());
        }
        let params = html_stream_params("gzip", None);
        let body = streaming_finalize_response(
            params,
            origin_chunk_then_pending(bytes::Bytes::from(gzip_encode(&page))),
        );

        let first = first_lazy_body_chunk(body);
        assert!(
            !first.is_empty(),
            "first emitted gzip chunk must not be empty"
        );

        // Decode the flushed (not finished) gzip prefix the way a browser would
        // decode bytes received so far while the response is still open.
        let mut decoder = flate2::write::MultiGzDecoder::new(Vec::new());
        decoder
            .write_all(&first)
            .expect("first chunk must be valid gzip");
        decoder.flush().expect("should flush gzip decoder");
        let decoded = String::from_utf8(decoder.get_ref().clone()).expect("should be valid UTF-8");
        assert!(
            decoded.contains("hello world"),
            "first poll must emit decodable streamed content before EOF. Got: {decoded}"
        );
    }

    #[test]
    fn streaming_finalize_collects_exact_projection_before_lazy_html_head() {
        // Fastly must complete the dispatched auction before it constructs the
        // HTML processor. The origin can still stream lazily after that barrier,
        // but its first `<head>` must carry the exact projection, never the safe
        // empty placeholder or a legacy body-close transport.
        let page = b"<html><head></head><body><p>hello</p><p>more streamed content here</p>";
        let params = html_stream_params(
            "",
            Some(DispatchedAuction::empty_for_test(
                test_auction_request(),
                10,
            )),
        );
        let body = streaming_finalize_response(
            params,
            origin_chunk_then_pending(bytes::Bytes::from(&page[..])),
        );

        let first = first_lazy_body_chunk(body);
        let html = String::from_utf8(first.to_vec()).expect("should be valid UTF-8");
        assert!(
            html.contains("hello"),
            "the origin body should remain lazy after the pre-head collect. Got: {html}"
        );
        assert!(
            html.contains(r#""auctionId":"a1_"#),
            "the first head chunk must contain the exact collected projection with a browser-only auction id. Got: {html}"
        );
        assert!(
            !html.contains(r#""auctionId":"test-auction""#),
            "the first head chunk must not expose the upstream auction id. Got: {html}"
        );
        assert!(
            !html.contains(r#""auctionId":"initial""#),
            "a dispatched auction must not boot with the safe empty projection. Got: {html}"
        );
        assert!(!html.contains(".adSlots"));
        assert!(!html.contains(".bids="));
    }

    #[test]
    fn streaming_finalize_auction_hold_emits_small_compressed_page_before_collection() {
        // A small gzip page arrives as one origin chunk whose whole expansion
        // fits inside `flate2`'s internal decode buffer, and its `</body>` lands
        // in that same chunk. Both stages that could withhold it must not: the
        // decoder sync-flushes per source chunk instead of holding output until
        // its own finalization, and the finish path emits the decoder tail
        // before awaiting collection. Otherwise the client holds committed
        // headers and an empty body until the auction resolves.
        let page = b"<html><head></head><body><p>small compressed page</p></body></html>";
        let params = html_stream_params(
            "gzip",
            Some(DispatchedAuction::empty_for_test(
                test_auction_request(),
                500,
            )),
        );
        let body = streaming_finalize_response(
            params,
            origin_chunk_then_pending(bytes::Bytes::from(gzip_encode(page))),
        );

        let first = first_lazy_body_chunk(body);
        // Decode the flushed (not finished) prefix the way a browser decodes the
        // bytes received while the response is still open.
        let mut decoder = flate2::write::MultiGzDecoder::new(Vec::new());
        decoder
            .write_all(&first)
            .expect("first chunk must be valid gzip");
        decoder.flush().expect("should flush gzip decoder");
        let decoded = String::from_utf8(decoder.get_ref().clone()).expect("should be valid UTF-8");

        assert!(
            decoded.contains("small compressed page"),
            "first poll must emit the decoded document prefix of a small gzip page. Got: {decoded}"
        );
        assert!(
            !decoded.contains("var b=JSON.parse("),
            "bids inject only at </body> after collection, which the first poll must not wait for. Got: {decoded}"
        );
    }

    // (method, status, expected Content-Length, expected Transfer-Encoding)
    // after bodiless normalization. 204 forbids Content-Length (removed); 205
    // must advertise a zero-length body; HEAD and 304 legitimately advertise the
    // GET representation length. Chunked framing is stripped from 204 (forbidden
    // by RFC 9112 §6.1) and 205 (would conflict with the inserted
    // `Content-Length: 0`), but preserved for HEAD and 304, whose message length
    // is fixed by the header section regardless of framing fields
    // (RFC 9112 §6.3, rule 1).
    const BODILESS_FRAMING_CASES: [(Method, StatusCode, Option<&str>, Option<&str>); 4] = [
        (Method::HEAD, StatusCode::OK, Some("42"), Some("chunked")),
        (Method::GET, StatusCode::NO_CONTENT, None, None),
        (Method::GET, StatusCode::RESET_CONTENT, Some("0"), None),
        (
            Method::GET,
            StatusCode::NOT_MODIFIED,
            Some("42"),
            Some("chunked"),
        ),
    ];

    /// Assert the framing fields left on a normalized bodiless response.
    fn assert_bodiless_framing(
        response: &Response<EdgeBody>,
        method: &Method,
        status: StatusCode,
        expected_length: Option<&str>,
        expected_transfer_encoding: Option<&str>,
    ) {
        let header_value = |name: header::HeaderName| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        assert_eq!(
            header_value(header::CONTENT_LENGTH).as_deref(),
            expected_length,
            "bodiless {method} {status} must carry the corrected Content-Length"
        );
        assert_eq!(
            header_value(header::TRANSFER_ENCODING).as_deref(),
            expected_transfer_encoding,
            "bodiless {method} {status} must carry the corrected Transfer-Encoding"
        );
        assert_eq!(
            header_value(header::TRAILER).as_deref(),
            // `Trailer` only describes fields a chunked body can carry, so it
            // survives exactly where chunked framing does.
            expected_transfer_encoding.map(|_| "expires"),
            "bodiless {method} {status} must drop Trailer with its chunked framing"
        );
    }

    #[test]
    fn publisher_response_streaming_finalize_normalizes_bodiless_framing() {
        // Fastly requests the origin body as a stream before classification, so
        // a buffered-unmodified response can hold an `EdgeBody::Stream`. The
        // adapter streams any `EdgeBody::Stream` to the client, so bodiless
        // responses must carry no body and correct framing per status.
        let settings = Arc::new(create_test_settings());
        let registry = Arc::new(
            IntegrationRegistry::new(&settings).expect("should create integration registry"),
        );
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));

        for (method, status, expected_length, expected_transfer_encoding) in BODILESS_FRAMING_CASES
        {
            let response = Response::builder()
                .status(status)
                .header(header::CONTENT_LENGTH, "42")
                .header(header::TRANSFER_ENCODING, "chunked")
                .header(header::TRAILER, "expires")
                .body(EdgeBody::stream(futures::stream::iter(vec![
                    bytes::Bytes::from_static(b"origin body bytes that must not reach the client"),
                ])))
                .expect("should build response");
            let publisher_response = PublisherResponse::Buffered(response);

            let response = futures::executor::block_on(publisher_response_into_streaming_response(
                publisher_response,
                &method,
                Arc::clone(&settings),
                registry.as_ref(),
                Arc::clone(&orchestrator),
                noop_services(),
            ))
            .expect("should finalize buffered response");

            assert!(
                !matches!(response.body(), EdgeBody::Stream(_)),
                "bodiless {method} {status} must not carry a streaming body"
            );
            assert_bodiless_framing(
                &response,
                &method,
                status,
                expected_length,
                expected_transfer_encoding,
            );

            let drained = futures::executor::block_on(
                response
                    .into_body()
                    .into_bytes_bounded(settings.publisher.max_buffered_body_bytes),
            )
            .expect("body should drain")
            .to_vec();
            assert!(
                drained.is_empty(),
                "bodiless {method} {status} must deliver zero body bytes, got {} bytes",
                drained.len()
            );
        }
    }

    #[test]
    fn buffer_publisher_response_normalizes_bodiless_framing() {
        // The buffered finalizer (Axum/Cloudflare/Spin) must correct bodiless
        // framing identically to the streaming finalizer.
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let services = noop_services();

        for (method, status, expected_length, expected_transfer_encoding) in BODILESS_FRAMING_CASES
        {
            let response = Response::builder()
                .status(status)
                .header(header::CONTENT_LENGTH, "42")
                .header(header::TRANSFER_ENCODING, "chunked")
                .header(header::TRAILER, "expires")
                .body(EdgeBody::from(
                    b"origin body bytes that must not reach the client".to_vec(),
                ))
                .expect("should build response");
            let publisher_response = PublisherResponse::Buffered(response);

            let response = futures::executor::block_on(buffer_publisher_response_async(
                publisher_response,
                &method,
                &settings,
                &registry,
                &orchestrator,
                &services,
            ))
            .expect("should finalize buffered response");

            assert_bodiless_framing(
                &response,
                &method,
                status,
                expected_length,
                expected_transfer_encoding,
            );

            let drained = futures::executor::block_on(
                response
                    .into_body()
                    .into_bytes_bounded(settings.publisher.max_buffered_body_bytes),
            )
            .expect("body should drain")
            .to_vec();
            assert!(
                drained.is_empty(),
                "bodiless {method} {status} must deliver zero body bytes, got {} bytes",
                drained.len()
            );
        }
    }

    #[derive(Default)]
    struct RecordingTelemetrySink {
        batches: Mutex<Vec<crate::auction::telemetry::AuctionEventBatch>>,
    }

    #[async_trait::async_trait(?Send)]
    impl crate::auction::telemetry::AuctionTelemetrySink for RecordingTelemetrySink {
        async fn emit_auction_events(
            &self,
            _services: &RuntimeServices,
            batch: crate::auction::telemetry::AuctionEventBatch,
        ) -> Result<(), Report<TrustedServerError>> {
            self.batches
                .lock()
                .expect("should lock telemetry batches")
                .push(batch);
            Ok(())
        }
    }

    #[test]
    fn finalizers_emit_abandoned_auction_for_bodiless_dispatched_response() {
        // A conditional navigation can dispatch an auction and receive a
        // processable HTML 304 that classification routes to Stream. The
        // bodiless response has no `</body>` to inject into, so the dispatched
        // auction is never collected — but both finalizers must still emit a
        // terminal abandonment event so the SSP work and quota consumption stay
        // observable instead of vanishing silently.
        let settings = Arc::new(create_test_settings());
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));

        let make_params = || {
            let ec_context =
                EcContext::new_for_test(None, crate::consent::types::ConsentContext::default());
            OwnedProcessResponseParams {
                content_encoding: String::new(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/html; charset=utf-8".to_string(),
                ad_slots_script: None,
                ad_bids_state: Arc::new(Mutex::new(None)),
                auction_observation: Some(AuctionObservationContext::from_parts(
                    AuctionSource::SpaNavigation,
                    "proxy.example.com",
                    "/article",
                    1,
                    &ec_context,
                )),
                auction_request: Some(test_auction_request()),
                dispatched_auction: Some(DispatchedAuction::empty_for_test(
                    test_auction_request(),
                    10,
                )),
                price_granularity: PriceGranularity::default(),
                gpt_diagnostics: None,
                render_trace_overlay: false,
                suppress_datadome_client_side_tag: false,
            }
        };
        let make_stream_response = || PublisherResponse::Stream {
            response: Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(EdgeBody::empty())
                .expect("should build 304 response"),
            body: EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::from_static(
                b"<html></html>",
            )])),
            params: Box::new(make_params()),
        };

        fn assert_bodiless_abandoned(sink: &RecordingTelemetrySink) {
            let batches = sink.batches.lock().expect("should lock telemetry batches");
            assert_eq!(
                batches.len(),
                1,
                "bodiless dispatched response should emit exactly one terminal batch"
            );
            let reasons: Vec<String> = batches[0]
                .rows()
                .iter()
                .filter_map(|row| row.terminal_reason.clone())
                .collect();
            assert!(
                reasons.iter().any(|reason| reason == "bodiless_response"),
                "abandonment must carry the bodiless_response reason, got {reasons:?}"
            );
        }

        // Streaming finalizer (Fastly).
        let streaming_sink = Arc::new(RecordingTelemetrySink::default());
        let response = futures::executor::block_on(publisher_response_into_streaming_response(
            make_stream_response(),
            &Method::GET,
            Arc::clone(&settings),
            &registry,
            Arc::clone(&orchestrator),
            noop_services_with_telemetry_sink(Arc::clone(&streaming_sink) as _),
        ))
        .expect("streaming finalize should succeed");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_bodiless_abandoned(&streaming_sink);

        // Buffered finalizer (Axum/Cloudflare/Spin).
        let buffered_sink = Arc::new(RecordingTelemetrySink::default());
        let buffered_services = noop_services_with_telemetry_sink(Arc::clone(&buffered_sink) as _);
        let response = futures::executor::block_on(buffer_publisher_response_async(
            make_stream_response(),
            &Method::GET,
            settings.as_ref(),
            &registry,
            orchestrator.as_ref(),
            &buffered_services,
        ))
        .expect("buffered finalize should succeed");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_bodiless_abandoned(&buffered_sink);
    }

    #[test]
    fn publisher_response_streaming_finalize_processes_gzip_stream() {
        let compressed =
            gzip_encode(b"body{background:url('https://origin.example.com/asset.png')}");
        let split_at = compressed.len() / 2;
        let output = drain_streaming_finalize_body(
            "gzip",
            EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::copy_from_slice(&compressed[..split_at]),
                bytes::Bytes::copy_from_slice(&compressed[split_at..]),
            ])),
        );

        let css = String::from_utf8(gzip_decode(&output)).expect("should be valid UTF-8");
        assert!(
            css.contains("proxy.example.com"),
            "streaming response finalize should rewrite gzip body. Got: {css}"
        );
        assert!(
            !css.contains("origin.example.com"),
            "streaming response finalize should not leave gzip origin URLs. Got: {css}"
        );
    }

    #[test]
    fn publisher_response_streaming_finalize_processes_deflate_stream() {
        let compressed =
            deflate_encode(b"body{background:url('https://origin.example.com/asset.png')}");
        let split_at = compressed.len() / 2;
        let output = drain_streaming_finalize_body(
            "deflate",
            EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::copy_from_slice(&compressed[..split_at]),
                bytes::Bytes::copy_from_slice(&compressed[split_at..]),
            ])),
        );

        let css = String::from_utf8(deflate_decode(&output)).expect("should be valid UTF-8");
        assert!(
            css.contains("proxy.example.com"),
            "streaming response finalize should rewrite deflate body. Got: {css}"
        );
        assert!(
            !css.contains("origin.example.com"),
            "streaming response finalize should not leave deflate origin URLs. Got: {css}"
        );
    }

    #[test]
    fn publisher_response_streaming_finalize_processes_brotli_stream() {
        let compressed =
            brotli_encode(b"body{background:url('https://origin.example.com/asset.png')}");
        let split_at = compressed.len() / 2;
        let output = drain_streaming_finalize_body(
            "br",
            EdgeBody::stream(futures::stream::iter(vec![
                bytes::Bytes::copy_from_slice(&compressed[..split_at]),
                bytes::Bytes::copy_from_slice(&compressed[split_at..]),
            ])),
        );

        let css = String::from_utf8(brotli_decode(&output)).expect("should be valid UTF-8");
        assert!(
            css.contains("proxy.example.com"),
            "streaming response finalize should rewrite brotli body. Got: {css}"
        );
        assert!(
            !css.contains("origin.example.com"),
            "streaming response finalize should not leave brotli origin URLs. Got: {css}"
        );
    }

    #[test]
    fn publisher_response_streaming_finalize_boots_projection_and_keeps_gzip_tail() {
        let settings = Arc::new(create_test_settings());
        let registry = Arc::new(
            IntegrationRegistry::new(&settings).expect("should create integration registry"),
        );
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));
        let services = noop_services();
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(EdgeBody::empty())
            .expect("should build response");
        // The trailing content after `</body>` must exceed the flate2 write
        // decoder's 32 KiB internal output buffer. This guards against the EOF
        // decoded tail being dropped after the pre-head projection barrier.
        let trailing_comment = format!("<!-- {} -->", "trailing-content ".repeat(3 * 1024));
        let page = format!("<html><head></head><body>hello</body>{trailing_comment}</html>");
        let compressed = gzip_encode(page.as_bytes());
        let chunks: Vec<bytes::Bytes> = compressed
            .chunks(STREAM_CHUNK_SIZE)
            .map(bytes::Bytes::copy_from_slice)
            .collect();
        let params = OwnedProcessResponseParams {
            content_encoding: "gzip".to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=JSON.parse("[]");</script>"#
                    .to_string(),
            ),
            ad_bids_state: Arc::new(Mutex::new(None)),
            auction_observation: None,
            auction_request: Some(test_auction_request()),
            dispatched_auction: Some(DispatchedAuction::empty_for_test(
                test_auction_request(),
                10,
            )),
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
        };
        let publisher_response = PublisherResponse::Stream {
            response,
            body: EdgeBody::stream(futures::stream::iter(chunks)),
            params: Box::new(params),
        };

        let response = futures::executor::block_on(publisher_response_into_streaming_response(
            publisher_response,
            &Method::GET,
            Arc::clone(&settings),
            registry.as_ref(),
            orchestrator,
            services,
        ))
        .expect("should build streaming response");

        let output = futures::executor::block_on(
            response
                .into_body()
                .into_bytes_bounded(settings.publisher.max_buffered_body_bytes),
        )
        .expect("streaming body should drain")
        .to_vec();

        let html = String::from_utf8(gzip_decode(&output)).expect("should be valid UTF-8");
        assert!(
            html.contains(r#""auctionId":"a1_"#),
            "should collect the exact projection with a browser-only auction id before the compressed head. Got head: {}",
            &html[..html.len().min(500)]
        );
        assert!(
            !html.contains(r#""auctionId":"test-auction""#),
            "should not expose the upstream auction id in the compressed head. Got head: {}",
            &html[..html.len().min(500)]
        );
        assert!(!html.contains(".bids="));
        assert!(
            html.contains("trailing-content"),
            "should preserve content after the close-body tag"
        );
        assert!(
            html.trim_end().ends_with("</html>"),
            "should not drop the decoded tail once the auction hold is released. Got tail: {}",
            &html[html.len().saturating_sub(200)..]
        );
    }

    #[test]
    fn stream_publisher_body_treats_mixed_case_html_as_hard_cutover_html() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let bids_script =
            r#"<script>(window.tsjs=window.tsjs||{}).bids=JSON.parse("{}");</script>"#;
        let state = Arc::new(Mutex::new(Some(bids_script.to_string())));
        let params = OwnedProcessResponseParams {
            content_encoding: String::new(),
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
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
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
            html.contains(r#""auctionId":"initial","results":[]},"slots":[],"bids":[]"#),
            "mixed-case HTML must use the HTML processor and inject the canonical boot projection. Got: {html}"
        );
        assert!(
            !html.contains(".adSlots=JSON.parse") && !html.contains(".bids=JSON.parse"),
            "mixed-case HTML must not restore legacy TSJS data globals. Got: {html}"
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
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
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
            content_encoding: String::new(),
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
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
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
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
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
        use super::super::{MatchedSlotsContext, build_auction_request, build_browser_slots_v1};
        use crate::auction::types::MediaType;
        use crate::consent::ConsentContext;
        use crate::creative_opportunities::{
            CreativeOpportunitiesConfig, CreativeOpportunityFormat, CreativeOpportunitySlot,
        };
        use crate::http_util::RequestInfo;
        use crate::price_bucket::PriceGranularity;

        fn make_config() -> CreativeOpportunitiesConfig {
            CreativeOpportunitiesConfig {
                enabled: true,
                gam_network_id: "21765378893".to_string(),
                auction_timeout_ms: Some(500),
                price_granularity: PriceGranularity::Dense,
                section_root: None,
                section_segment: None,
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
                compiled_unit: None,
            }
        }

        #[test]
        fn browser_slots_render_section_from_request_path() {
            let mut config = make_config();
            config.gam_network_id = "99999".to_string();
            config.section_root = Some("homepage".to_string());
            let mut slot = make_slot();
            slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
            slot.compile_unit_template()
                .expect("template should compile");

            let news =
                build_browser_slots_v1(std::slice::from_ref(&slot), &config, "/news/article-123");
            assert_eq!(
                news[0].gam_unit_path, "/99999/example/news",
                "section should derive from the first path segment"
            );

            let home = build_browser_slots_v1(std::slice::from_ref(&slot), &config, "/");
            assert_eq!(
                home[0].gam_unit_path, "/99999/example/homepage",
                "root path should use section_root"
            );
        }

        #[test]
        fn browser_slots_honour_configured_section_segment() {
            // Locale-prefixed publisher: `/en/news/article` must resolve to the
            // `news` unit, not `en`.
            let mut config = make_config();
            config.gam_network_id = "99999".to_string();
            config.section_root = Some("homepage".to_string());
            config.section_segment = Some(1);
            let mut slot = make_slot();
            slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
            slot.compile_unit_template()
                .expect("template should compile");

            let news = build_browser_slots_v1(
                std::slice::from_ref(&slot),
                &config,
                "/en/news/article-123",
            );
            assert_eq!(
                news[0].gam_unit_path, "/99999/example/news",
                "section should derive from the configured segment index"
            );

            let locale_root = build_browser_slots_v1(std::slice::from_ref(&slot), &config, "/en");
            assert_eq!(
                locale_root[0].gam_unit_path, "/99999/example/homepage",
                "a path with no segment at the configured index should use section_root"
            );
        }

        #[test]
        fn auction_request_without_ec_id_omits_user_id_and_uses_non_ec_request_id() {
            let slot = make_slot();
            let slots = [slot];
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &slots,
                request_path_and_query: "/2024/01/my-article/?edition=fictional",
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
                "publisher.example.com",
                Some("Mozilla/5.0"),
            );

            assert_eq!(request.user.id, None, "should not forward an EC user id");
            assert!(
                request.id.starts_with("ts-req-"),
                "should use a non-EC request id, got {}",
                request.id
            );
            assert_eq!(
                request.publisher.page_url.as_deref(),
                Some("https://publisher.example.com/2024/01/my-article/"),
                "should preserve the page path but strip client query data for auction providers"
            );
        }

        #[test]
        fn auction_request_uses_configured_publisher_domain_not_edge_host() {
            // On the SSAT proxy path the browser addresses the trusted-server
            // edge host, but the auction must advertise the configured
            // publisher domain to SSPs — otherwise injected creatives and the
            // brand-safety pixel leak the edge/staging host.
            let slot = make_slot();
            let slots = [slot];
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &slots,
                request_path_and_query: "/2024/01/my-article/?edition=fictional",
            };
            let request_info = RequestInfo {
                host: "ts.example.com".to_string(),
                scheme: "https".to_string(),
            };

            let request = build_auction_request(
                &slots_ctx,
                None,
                &ConsentContext::default(),
                &request_info,
                "www.example.com",
                Some("Mozilla/5.0"),
            );

            assert_eq!(
                request.publisher.domain, "www.example.com",
                "publisher.domain should be the configured publisher domain, not the edge host"
            );
            let site = request.site.expect("should populate site metadata");
            assert_eq!(
                site.domain, "www.example.com",
                "site.domain should be the configured publisher domain, not the edge host"
            );
            assert_eq!(
                request.publisher.page_url.as_deref(),
                Some("https://www.example.com/2024/01/my-article/"),
                "page_url should use configured publisher identity without client query data"
            );
            assert_eq!(
                site.page, "https://www.example.com/2024/01/my-article/",
                "site.page should use configured publisher identity without client query data"
            );
        }

        #[test]
        fn auction_request_with_ec_id_sets_user_id_and_ec_request_id() {
            let slot = make_slot();
            let slots = [slot];
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &slots,
                request_path_and_query: "/2024/01/my-article/",
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
                "publisher.example.com",
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

        /// Settings for a deployment that has no `[creative_opportunities]`
        /// section, so page-bids answers `404`.
        fn settings_without_co() -> Settings {
            let toml = format!("{}\n[auction]\nenabled = true\n", crate_test_settings_str());
            Settings::from_toml(&toml)
                .expect("should parse settings without creative_opportunities")
        }

        fn settings_with_co_auction_disabled() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = false\n\n[creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with creative_opportunities")
        }

        fn settings_with_co_templates_disabled() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n[creative_opportunities]\nenabled = false\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml)
                .expect("should parse settings with server-side ad templates disabled")
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
                compiled_unit: None,
            }]
        }

        fn make_page_bids_request(path: &str) -> Request<EdgeBody> {
            let mut req = Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "https://test-publisher.com{PAGE_BIDS_PATH}?path={path}"
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

        /// The cross-site gate runs before the not-configured 404, so a
        /// cross-site caller cannot probe whether a deployment has creative
        /// opportunities configured.
        #[tokio::test]
        async fn cross_site_request_is_denied_before_configuration_is_revealed() {
            let settings = settings_without_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let mut req = Request::builder()
                .method(Method::GET)
                .uri(format!("https://test-publisher.com{PAGE_BIDS_PATH}?path=/"))
                .body(EdgeBody::empty())
                .expect("should build test request");
            set_test_header(&mut req, "sec-fetch-site", "cross-site");

            let response = run_page_bids_response(&settings, &orchestrator, &[], req).await;

            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "cross-site request should be denied, not answered with the 404"
            );
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
        async fn empty_slots_file_returns_an_exact_empty_projection() {
            // Spec §8 kill-switch: creative-opportunities.toml with zero slots disables
            // all server-side auction activity and injection.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let req = make_page_bids_request("/2024/01/my-article/");

            let body = run_page_bids(&settings, &orchestrator, &[], req).await;

            assert_eq!(body["version"], 1);
            assert_eq!(body["auction"]["version"], 1);
            assert_eq!(body["auction"]["results"], serde_json::json!([]));
            assert_eq!(
                body["bids"].as_array().expect("bids should be array").len(),
                0,
                "empty slots should produce zero bids"
            );
            assert_eq!(body["slots"], serde_json::json!([]));
        }

        #[tokio::test]
        async fn bot_user_agent_returns_a_terminal_projection_without_bids() {
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
                body["auction"]["results"][0],
                serde_json::json!({
                    "slot": "atf",
                    "outcome": "failed",
                    "reason": "slot_not_eligible"
                })
            );
            assert_eq!(
                body["bids"].as_array().expect("bids should be array").len(),
                0,
                "bot request must not run an auction (no SSP cost burned for crawlers)"
            );
        }

        #[tokio::test]
        async fn prefetch_request_returns_a_terminal_projection_without_bids() {
            // Navigations triggered by Sec-Purpose=prefetch should not fire real
            // SSP auctions — the user has not yet visited the page.
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let mut req = make_page_bids_request("/2024/01/my-article/");
            set_test_header(&mut req, "sec-purpose", "prefetch");

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;

            assert_eq!(body["auction"]["results"][0]["reason"], "slot_not_eligible");
            assert_eq!(
                body["bids"].as_array().expect("bids should be array").len(),
                0,
                "prefetch request must not run an auction"
            );
        }

        #[tokio::test]
        async fn page_bids_omits_only_over_limit_dynamic_slot() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let mut over_limit = article_slot()
                .into_iter()
                .next()
                .expect("should build over-limit slot");
            over_limit.id = "over_limit_dynamic".to_string();
            over_limit.page_patterns = vec!["/*".to_string()];
            over_limit.gam_unit_path = Some("/{section}/{section}".to_string());
            over_limit
                .compile_unit_template()
                .expect("template should compile");
            let mut valid_static = article_slot()
                .into_iter()
                .next()
                .expect("should build valid static slot");
            valid_static.id = "valid_static_sibling".to_string();
            valid_static.page_patterns = vec!["/*".to_string()];
            valid_static.gam_unit_path = Some("/12345/example/static".to_string());
            let slots = vec![over_limit, valid_static];
            let request_path = format!("/{}", "a".repeat(60));
            let mut req = make_page_bids_request(&request_path);
            set_test_header(&mut req, "sec-purpose", "prefetch");

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;
            let returned_slots = body["auction"]["results"]
                .as_array()
                .expect("results should be array");

            assert_eq!(
                returned_slots.len(),
                1,
                "should omit only the over-limit dynamic slot"
            );
            assert_eq!(
                returned_slots[0]["slot"], "valid_static_sibling",
                "should retain the valid static sibling"
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

            assert_eq!(body["auction"]["results"], serde_json::json!([]));
            assert_eq!(
                body["bids"].as_array().expect("bids should be array").len(),
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

        #[test]
        fn normalize_page_bids_path_and_query_preserves_query_but_drops_fragment() {
            assert_eq!(
                normalize_page_bids_path_and_query("/2024/01/article/?edition=fictional#section"),
                "/2024/01/article/?edition=fictional",
                "query should reach auction providers without a browser-only fragment"
            );
            assert_eq!(
                normalize_page_bids_path_and_query("2024/01/article/?edition=fictional"),
                "/2024/01/article/?edition=fictional",
                "missing leading slash should be added"
            );
        }

        #[tokio::test]
        async fn disabled_auction_returns_exact_failed_decisions() {
            // [auction].enabled = false is a global kill switch: it must disable
            // the entire server-side ad stack, not just SSP calls. Returning slot
            // definitions would let the hard-cutover browser runtime create or
            // refresh GPT slots even though the auction is off. Consent is
            // allowed here so the test isolates the kill switch.
            let settings = settings_with_co_auction_disabled();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let req = make_page_bids_request("/2024/01/my-article/");

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;

            assert_eq!(body["auction"]["results"][0]["reason"], "auction_disabled");
            assert_eq!(
                body["bids"].as_array().expect("bids should be array").len(),
                0,
                "disabled auction must not produce bids"
            );
        }

        #[tokio::test]
        async fn disabled_server_side_ad_templates_return_an_empty_projection() {
            let settings = settings_with_co_templates_disabled();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let req = make_page_bids_request("/2024/01/my-article/");

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;

            assert_eq!(body["auction"]["results"], serde_json::json!([]));
            assert_eq!(body["slots"], serde_json::json!([]));
            assert_eq!(body["bids"], serde_json::json!([]));
        }

        #[tokio::test]
        async fn consent_denied_returns_exact_failed_decisions() {
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

            assert_eq!(body["auction"]["results"][0]["reason"], "consent_denied");
            assert_eq!(
                body["bids"].as_array().expect("bids should be array").len(),
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

    /// Handler-level coverage that both navigation paths take the publisher
    /// identity from configuration rather than the incoming edge `Host` header.
    ///
    /// The `build_auction_request` unit test above cannot catch a call site
    /// regressing to `request_info.host`, because both sources are `&str`. These
    /// tests drive the real handlers with a divergent edge host and assert on
    /// the auction request the orchestrator dispatched and on the telemetry rows
    /// the handler emitted.
    mod navigation_publisher_domain_tests {
        use super::*;
        use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
        use crate::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink};
        use crate::auction::types::AuctionRequest;
        use crate::auction::{AuctionContext, AuctionOrchestrator};
        use crate::creative_opportunities::{CreativeOpportunityFormat, CreativeOpportunitySlot};
        use crate::platform::test_support::{
            NoopConfigStore, NoopGeo, NoopSecretStore, StubBackend,
        };
        use crate::platform::{ClientInfo, PlatformResponse};
        use crate::test_support::tests::crate_test_settings_str;
        use std::sync::Mutex;

        /// Trusted-server edge host the browser addresses on the SSAT proxy
        /// path — deliberately different from the configured publisher domain.
        const EDGE_HOST: &str = "ts.example.com";

        /// `[publisher] domain` from [`crate_test_settings_str`].
        const CONFIGURED_DOMAIN: &str = "test-publisher.com";

        const CAPTURING_PROVIDER: &str = "request_capturing_provider";

        /// Records the [`AuctionRequest`] the orchestrator dispatched, then
        /// fails its launch so no real transport handle is needed.
        struct RequestCapturingProvider {
            captured: Arc<Mutex<Option<AuctionRequest>>>,
        }

        #[async_trait::async_trait(?Send)]
        impl AuctionProvider for RequestCapturingProvider {
            fn provider_name(&self) -> &'static str {
                CAPTURING_PROVIDER
            }

            async fn request_bids(
                &self,
                request: &AuctionRequest,
                _context: &AuctionContext<'_>,
            ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
                *self.captured.lock().expect("should lock captured request") =
                    Some(request.clone());
                Err(Report::new(TrustedServerError::Auction {
                    message: "capture only".to_string(),
                }))
            }

            async fn parse_response(
                &self,
                _response: PlatformResponse,
                _response_time_ms: u64,
            ) -> Result<AuctionResponse, Report<TrustedServerError>> {
                panic!("parse_response must not run when the launch fails");
            }

            fn timeout_ms(&self) -> u32 {
                100
            }

            fn backend_name(
                &self,
                _services: &RuntimeServices,
                _timeout_ms: u32,
            ) -> Option<String> {
                Some("capture-backend".to_string())
            }
        }

        #[derive(Default)]
        struct RecordingTelemetrySink {
            batches: Mutex<Vec<AuctionEventBatch>>,
        }

        #[async_trait::async_trait(?Send)]
        impl AuctionTelemetrySink for RecordingTelemetrySink {
            async fn emit_auction_events(
                &self,
                _services: &RuntimeServices,
                batch: AuctionEventBatch,
            ) -> Result<(), Report<TrustedServerError>> {
                self.batches
                    .lock()
                    .expect("should lock telemetry batches")
                    .push(batch);
                Ok(())
            }
        }

        fn settings_with_capturing_provider() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\nproviders = [\"{CAPTURING_PROVIDER}\"]\n\n\
                 [creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with a capturing provider")
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
                    media_type: MediaType::Banner,
                }],
                floor_price: Some(0.50),
                targeting: Default::default(),
                providers: Default::default(),
                compiled_patterns: Vec::new(),
                compiled_unit: None,
            }]
        }

        fn slots_with_over_limit_dynamic_sibling() -> Vec<CreativeOpportunitySlot> {
            let mut over_limit = article_slot()
                .into_iter()
                .next()
                .expect("should build over-limit slot");
            over_limit.id = "over_limit_dynamic".to_string();
            over_limit.page_patterns = vec!["/*".to_string()];
            over_limit.gam_unit_path = Some("/{section}/{section}".to_string());

            let mut valid_static = article_slot()
                .into_iter()
                .next()
                .expect("should build valid static slot");
            valid_static.id = "valid_static_sibling".to_string();
            valid_static.page_patterns = vec!["/*".to_string()];
            valid_static.gam_unit_path = Some("/12345/example/static".to_string());

            vec![over_limit, valid_static]
        }

        fn assert_only_renderable_slot_was_auctioned(
            captured: &Arc<Mutex<Option<AuctionRequest>>>,
        ) {
            let request = captured
                .lock()
                .expect("should lock captured request")
                .clone()
                .expect("should dispatch an auction request");
            let slot_ids: Vec<_> = request.slots.iter().map(|slot| slot.id.as_str()).collect();
            assert_eq!(slot_ids, vec!["valid_static_sibling"]);
        }

        /// [`EcContext`] whose consent context permits the server-side auction.
        fn consent_allowing_ec_context() -> EcContext {
            let consent = crate::consent::ConsentContext {
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                ..Default::default()
            };
            EcContext::new_for_test(None, consent)
        }

        fn services_with(
            http_client: Arc<dyn crate::platform::PlatformHttpClient>,
            telemetry_sink: Arc<RecordingTelemetrySink>,
        ) -> RuntimeServices {
            let telemetry_sink: Arc<dyn AuctionTelemetrySink> = telemetry_sink;
            RuntimeServices::builder()
                .config_store(Arc::new(NoopConfigStore))
                .secret_store(Arc::new(NoopSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .backend(Arc::new(StubBackend))
                .http_client(http_client)
                .geo(Arc::new(NoopGeo))
                .auction_telemetry_sink(telemetry_sink)
                .client_info(ClientInfo::default())
                .build()
        }

        fn orchestrator_capturing_request(
            settings: &Settings,
            captured: &Arc<Mutex<Option<AuctionRequest>>>,
        ) -> AuctionOrchestrator {
            let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            orchestrator.register_provider(Arc::new(RequestCapturingProvider {
                captured: Arc::clone(captured),
            }));
            orchestrator
        }

        /// Assert the dispatched request and every emitted telemetry row carry
        /// the configured publisher domain rather than the edge host.
        fn assert_configured_domain(
            captured: &Arc<Mutex<Option<AuctionRequest>>>,
            telemetry_sink: &RecordingTelemetrySink,
        ) {
            let request = captured
                .lock()
                .expect("should lock captured request")
                .clone()
                .expect("should dispatch an auction request");
            assert_eq!(
                request.publisher.domain, CONFIGURED_DOMAIN,
                "publisher.domain should be the configured publisher domain, not the edge host"
            );
            let site = request.site.expect("should populate site metadata");
            assert_eq!(
                site.domain, CONFIGURED_DOMAIN,
                "site.domain should be the configured publisher domain, not the edge host"
            );
            // Only the host is asserted: the two `AuctionRequest` builders still
            // disagree on where the scheme comes from (edge-detected here,
            // canonical `https` in `convert_tsjs_to_auction_request`), which is
            // tracked separately.
            let page_url = request
                .publisher
                .page_url
                .as_deref()
                .expect("should populate page_url");
            let page_url_host = url::Url::parse(page_url)
                .expect("should build a parseable page_url")
                .host_str()
                .map(str::to_owned)
                .expect("should populate a page_url host");
            assert_eq!(
                page_url_host, CONFIGURED_DOMAIN,
                "page_url host should be the configured publisher domain, not the edge host"
            );
            assert_eq!(
                site.page, page_url,
                "site.page should mirror page_url, so it carries the configured publisher domain too"
            );

            let batches = telemetry_sink
                .batches
                .lock()
                .expect("should lock telemetry batches");
            let rows: Vec<_> = batches.iter().flat_map(AuctionEventBatch::rows).collect();
            assert!(!rows.is_empty(), "should emit at least one telemetry row");
            for row in rows {
                assert_eq!(
                    row.publisher_domain, CONFIGURED_DOMAIN,
                    "telemetry rows should be attributed to the configured publisher domain, not the edge host"
                );
            }
        }

        #[tokio::test]
        async fn initial_navigation_advertises_configured_publisher_domain() {
            let settings = settings_with_capturing_provider();
            let captured = Arc::new(Mutex::new(None));
            let orchestrator = orchestrator_capturing_request(&settings, &captured);
            let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"<html><head></head><body>ok</body></html>".to_vec());
            let services = services_with(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
                Arc::clone(&telemetry_sink),
            );
            let mut ec_context = consent_allowing_ec_context();
            let req = HttpRequest::builder()
                .method(Method::GET)
                .uri(format!("https://{EDGE_HOST}/2024/01/my-article/"))
                .header(header::HOST, EDGE_HOST)
                .header("sec-fetch-dest", "document")
                .body(EdgeBody::empty())
                .expect("should build test request");

            let _ = handle_publisher_request(
                &settings,
                &services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &article_slot(),
                    registry: None,
                },
                req,
            )
            .await
            .expect("should proxy publisher request");

            assert_configured_domain(&captured, &telemetry_sink);
        }

        #[tokio::test]
        async fn page_bids_advertises_configured_publisher_domain() {
            let settings = settings_with_capturing_provider();
            let captured = Arc::new(Mutex::new(None));
            let orchestrator = orchestrator_capturing_request(&settings, &captured);
            let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
            let services = services_with(
                Arc::new(crate::platform::test_support::NoopHttpClient),
                Arc::clone(&telemetry_sink),
            );
            let ec_context = consent_allowing_ec_context();
            let mut req = HttpRequest::builder()
                .method(Method::GET)
                .uri(format!(
                    "https://{EDGE_HOST}/_ts/page-bids?path=/2024/01/my-article/"
                ))
                .header(header::HOST, EDGE_HOST)
                .body(EdgeBody::empty())
                .expect("should build test request");
            req.headers_mut().insert(
                header::HeaderName::from_static("sec-fetch-site"),
                HeaderValue::from_static("same-origin"),
            );

            let _ = handle_page_bids(
                &settings,
                &services,
                None,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &article_slot(),
                    registry: None,
                },
                &ec_context,
                req,
            )
            .await
            .expect("should return ok response");

            assert_configured_domain(&captured, &telemetry_sink);
        }

        #[tokio::test]
        async fn initial_navigation_auctions_only_renderable_slots() {
            let settings = settings_with_capturing_provider();
            let captured = Arc::new(Mutex::new(None));
            let orchestrator = orchestrator_capturing_request(&settings, &captured);
            let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"<html><head></head><body>ok</body></html>".to_vec());
            let services = services_with(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
                telemetry_sink,
            );
            let mut ec_context = consent_allowing_ec_context();
            let request_path = format!("/{}", "a".repeat(60));
            let req = HttpRequest::builder()
                .method(Method::GET)
                .uri(format!("https://{EDGE_HOST}{request_path}"))
                .header(header::HOST, EDGE_HOST)
                .header("sec-fetch-dest", "document")
                .body(EdgeBody::empty())
                .expect("should build test request");
            let slots = slots_with_over_limit_dynamic_sibling();

            let _ = handle_publisher_request(
                &settings,
                &services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &slots,
                    registry: None,
                },
                req,
            )
            .await
            .expect("should proxy publisher request");

            assert_only_renderable_slot_was_auctioned(&captured);
        }

        #[tokio::test]
        async fn page_bids_auctions_only_renderable_slots() {
            let settings = settings_with_capturing_provider();
            let captured = Arc::new(Mutex::new(None));
            let orchestrator = orchestrator_capturing_request(&settings, &captured);
            let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
            let services = services_with(
                Arc::new(crate::platform::test_support::NoopHttpClient),
                telemetry_sink,
            );
            let ec_context = consent_allowing_ec_context();
            let request_path = format!("/{}", "a".repeat(60));
            let mut req = HttpRequest::builder()
                .method(Method::GET)
                .uri(format!(
                    "https://{EDGE_HOST}/_ts/page-bids?path={request_path}"
                ))
                .header(header::HOST, EDGE_HOST)
                .body(EdgeBody::empty())
                .expect("should build test request");
            req.headers_mut().insert(
                header::HeaderName::from_static("sec-fetch-site"),
                HeaderValue::from_static("same-origin"),
            );
            let slots = slots_with_over_limit_dynamic_sibling();

            let _ = handle_page_bids(
                &settings,
                &services,
                None,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &slots,
                    registry: None,
                },
                &ec_context,
                req,
            )
            .await
            .expect("should return ok response");

            assert_only_renderable_slot_was_auctioned(&captured);
        }
    }
}
