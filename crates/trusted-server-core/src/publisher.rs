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

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use brotli::enc::writer::CompressorWriter;
use brotli::enc::BrotliEncoderParams;
use brotli::Decompressor;
use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};
use futures::StreamExt as _;
use http::{header, HeaderValue, Method, Request, Response, StatusCode, Uri};

use crate::auction::endpoints::{
    merge_auction_eids, resolve_auction_eids, resolve_client_auction_eids,
};
use crate::auction::orchestrator::{
    AuctionOrchestrator, DispatchAuctionOutcome, DispatchedAuction,
};
use crate::auction::telemetry::{
    AuctionObservationContext, AuctionSource, AuctionTerminalOutcome, build_auction_events,
    emit_auction_events_best_effort_lazy,
};
use crate::auction::types::{
    AuctionContext, AuctionRequest, Bid, DeviceInfo, PublisherInfo, SiteInfo, UserInfo,
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
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::Settings;
use crate::streaming_processor::{
    BodyStreamDecoder, BodyStreamEncoder, Compression, PipelineConfig, StreamProcessor,
    StreamingPipeline, STREAM_CHUNK_SIZE,
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
        let mut resp = serve_static_with_etag(&body, req, "application/javascript; charset=utf-8");
        resp.headers_mut()
            .insert(HEADER_X_COMPRESS_HINT, HeaderValue::from_static("on"));
        return Ok(resp);
    }

    if let Some(module_id) = parse_deferred_module_filename(filename) {
        // Only serve if the deferred module is actually enabled
        let deferred_ids = integration_registry.js_module_ids_deferred();
        if !deferred_ids.contains(&module_id) {
            return Ok(not_found_response());
        }
        if let Some(content) = trusted_server_js::module_bundle(module_id) {
            let mut resp =
                serve_static_with_etag(content, req, "application/javascript; charset=utf-8");
            resp.headers_mut()
                .insert(HEADER_X_COMPRESS_HINT, HeaderValue::from_static("on"));
            return Ok(resp);
        }
    }

    Ok(not_found_response())
}

/// Extract a module ID from a deferred-module filename like `tsjs-sourcepoint.min.js`.
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
            Box::new(create_html_stream_processor(
                &params.origin_host,
                &params.request_host,
                &params.request_scheme,
                settings,
                integration_registry,
                params.ad_slots_script.as_deref().map(str::to_string),
                Arc::clone(&params.ad_bids_state),
            )?)
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
        StreamingPipeline::new(config, processor).process(body_as_reader(body)?, output)?;
    } else if is_rsc_flight {
        // RSC Flight responses are length-prefixed (T rows). A naive string replacement will
        // corrupt the stream by changing byte lengths without updating the prefixes.
        let processor = RscFlightUrlRewriter::new(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, processor).process(body_as_reader(body)?, output)?;
    } else {
        let replacer = create_url_replacer(
            params.origin_host,
            params.origin_url,
            params.request_host,
            params.request_scheme,
        );
        StreamingPipeline::new(config, replacer).process(body_as_reader(body)?, output)?;
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

/// Mutable auction-hold state threaded through the streaming hold pipeline.
struct AuctionHoldState {
    hold: Option<BodyCloseHoldBuffer>,
    dispatched: DispatchedAuctionGuard,
    telemetry: AuctionTelemetryCarry,
}

impl AuctionHoldState {
    fn new(dispatched: DispatchedAuctionGuard, telemetry: AuctionTelemetryCarry) -> Self {
        Self {
            hold: Some(BodyCloseHoldBuffer::new()),
            dispatched,
            telemetry,
        }
    }
}

/// Abandon the in-flight auction (if still pending) with the given telemetry
/// reason. No-op once the auction has been collected or already abandoned.
async fn abandon_hold_auction(
    state: &mut AuctionHoldState,
    services: &RuntimeServices,
    reason: &'static str,
) {
    if let Some(dispatched) = state.dispatched.take() {
        emit_abandoned_auction(
            services,
            state.telemetry.observation.take(),
            dispatched,
            reason,
        )
        .await;
        // Abandonment with telemetry is a terminal result, so the drop warning
        // is no longer warranted. (A drop *during* the emit above still fires
        // it, since the guard stays armed until here.)
        state.dispatched.disarm();
    }
}

/// Feed one decoded chunk through the close-body hold and processor.
///
/// Returns the encoded output segments for the caller to emit — written to a
/// client stream by [`body_close_hold_loop_stream`], yielded from the lazy
/// body by [`publisher_response_into_streaming_response`]. Both async hold
/// paths share this function so their behavior cannot drift apart.
///
/// When the raw `</body` prefix is found the dispatched auction is collected
/// before the held tail is processed, so `lol_html` sees live bids. On
/// processing failure the pending auction is abandoned before the error is
/// returned.
async fn hold_step_decoded_chunk<P: StreamProcessor>(
    processor: &mut P,
    encoder: &mut BodyStreamEncoder,
    chunk: &[u8],
    state: &mut AuctionHoldState,
    collect_refs: &AuctionHoldCollectRefs<'_>,
) -> Result<Vec<bytes::Bytes>, Report<TrustedServerError>> {
    let mut segments = Vec::new();
    if let Some(hold_buffer) = state.hold.as_mut() {
        let ready = hold_buffer.push(chunk);
        match process_and_encode_chunk(processor, encoder, &ready, false, "Failed to process chunk")
        {
            Ok(Some(encoded)) => segments.push(encoded),
            Ok(None) => {}
            Err(err) => {
                abandon_hold_auction(state, collect_refs.services, "stream_process_error").await;
                return Err(err);
            }
        }

        if state
            .hold
            .as_ref()
            .is_some_and(BodyCloseHoldBuffer::found_close)
        {
            let dispatched = state
                .dispatched
                .take()
                .expect("should have dispatched auction to collect");
            collect_stream_auction(
                dispatched,
                state.telemetry.take(),
                collect_refs.price_granularity,
                collect_refs.ad_bids_state,
                collect_refs.orchestrator,
                collect_refs.services,
                collect_refs.settings,
            )
            .await;
            // Collection reached a terminal result; disarm only now so a drop
            // while the collect await above was still pending is reported.
            state.dispatched.disarm();

            let held = state
                .hold
                .take()
                .expect("should have close-body hold buffer")
                .finish();
            if let Some(encoded) = process_and_encode_chunk(
                processor,
                encoder,
                &held,
                false,
                "Failed to process held body close",
            )? {
                segments.push(encoded);
            }
        }
    } else {
        match process_and_encode_chunk(processor, encoder, chunk, false, "Failed to process chunk")
        {
            Ok(Some(encoded)) => segments.push(encoded),
            Ok(None) => {}
            Err(err) => {
                abandon_hold_auction(state, collect_refs.services, "stream_process_error").await;
                return Err(err);
            }
        }
    }
    Ok(segments)
}

/// Pull and decode the next chunk of the close-body hold pipeline, feeding it
/// through [`hold_step_decoded_chunk`].
///
/// Returns `Ok(None)` when the source is exhausted; the caller must then emit
/// [`hold_finish_segments`]. On read or decode failure the pending auction is
/// abandoned before the error is returned. Shared by the write-sink driver
/// ([`body_close_hold_loop_stream`]) and the lazy publisher body stream so
/// the two hold paths cannot drift apart.
async fn hold_step_next_chunk<P: StreamProcessor>(
    source: &mut BodyChunkSource,
    decoder: &mut BodyStreamDecoder,
    encoder: &mut BodyStreamEncoder,
    processor: &mut P,
    state: &mut AuctionHoldState,
    collect_refs: &AuctionHoldCollectRefs<'_>,
) -> Result<Option<Vec<bytes::Bytes>>, Report<TrustedServerError>> {
    let raw_chunk = match source.next_chunk().await {
        Ok(Some(chunk)) => chunk,
        Ok(None) => return Ok(None),
        Err(err) => {
            abandon_hold_auction(state, collect_refs.services, "stream_read_error").await;
            return Err(err);
        }
    };
    let decoded = match decoder.decode_chunk(raw_chunk) {
        Ok(decoded) => decoded,
        Err(err) => {
            abandon_hold_auction(state, collect_refs.services, "stream_decode_error").await;
            return Err(err);
        }
    };
    if decoded.is_empty() {
        return Ok(Some(Vec::new()));
    }
    hold_step_decoded_chunk(processor, encoder, &decoded, state, collect_refs)
        .await
        .map(Some)
}

/// Finalize the close-body hold pipeline at end of the origin stream.
///
/// Drains the decoder tail through the hold (or straight through when the
/// hold was already released mid-stream), collects the auction if the
/// close-body tag never streamed, processes the held tail plus the
/// processor's final chunk, and emits the encoder trailer. Returns the
/// encoded segments for the caller to emit. On decoder failure the pending
/// auction is abandoned before the error is returned.
async fn hold_finish_segments<P: StreamProcessor>(
    processor: &mut P,
    decoder: &mut BodyStreamDecoder,
    encoder: &mut BodyStreamEncoder,
    state: &mut AuctionHoldState,
    collect_refs: &AuctionHoldCollectRefs<'_>,
) -> Result<Vec<bytes::Bytes>, Report<TrustedServerError>> {
    let mut segments = Vec::new();

    let decoded_tail = match decoder.finish() {
        Ok(decoded_tail) => decoded_tail,
        Err(err) => {
            abandon_hold_auction(state, collect_refs.services, "stream_decode_error").await;
            return Err(err);
        }
    };
    if !decoded_tail.is_empty() {
        segments.extend(
            hold_step_decoded_chunk(processor, encoder, &decoded_tail, state, collect_refs).await?,
        );
    }

    if let Some(hold) = state.hold.take() {
        let dispatched = state
            .dispatched
            .take()
            .expect("should have dispatched auction to collect");
        collect_stream_auction(
            dispatched,
            state.telemetry.take(),
            collect_refs.price_granularity,
            collect_refs.ad_bids_state,
            collect_refs.orchestrator,
            collect_refs.services,
            collect_refs.settings,
        )
        .await;
        // Collection reached a terminal result; disarm only now so a drop while
        // the collect await above was still pending is reported.
        state.dispatched.disarm();

        let held = hold.finish();
        if let Some(encoded) = process_and_encode_chunk(
            processor,
            encoder,
            &held,
            false,
            "Failed to process held body close",
        )? {
            segments.push(encoded);
        }
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
fn create_html_stream_processor(
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    ad_slots_script: Option<String>,
    ad_bids_state: Arc<Mutex<Option<String>>>,
) -> Result<impl StreamProcessor + use<>, Report<TrustedServerError>> {
    use crate::html_processor::{HtmlProcessorConfig, create_html_processor};

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
            // 205, 304) must stay bodiless, so drop the body while preserving
            // metadata such as `Content-Length`, matching the streaming
            // finalizer.
            if !response_carries_body(method, response.status()) {
                *response.body_mut() = EdgeBody::empty();
            }
            Ok(response)
        }
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
            // `EdgeBody::Stream` to the client — so drop the body while
            // preserving metadata such as `Content-Length`.
            if !response_carries_body(method, response.status()) {
                *response.body_mut() = EdgeBody::empty();
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
            params,
        } => {
            if !response_carries_body(method, response.status()) {
                if params.dispatched_auction.is_some() {
                    // A bodiless response (HEAD navigation, 204/304) has no
                    // `</body>` to inject bids into, so the dispatched SSP
                    // requests are wasted — surface it for quota observability,
                    // matching the buffered finalizer.
                    log::warn!(
                        "Server-side auction dispatched but response is bodiless (method: {}, status: {}); in-flight SSP bid requests will not be collected",
                        method,
                        response.status(),
                    );
                }
                return Ok(response);
            }

            response.headers_mut().remove(header::CONTENT_LENGTH);
            let mut params = *params;
            let mut processor =
                match PublisherBodyProcessor::new(&params, &settings, integration_registry) {
                    Ok(processor) => processor,
                    Err(err) => {
                        // Parity with the buffered finalizer: a processor
                        // construction failure abandons the dispatched auction
                        // with telemetry instead of dropping the in-flight SSP
                        // responses silently.
                        if let Some(dispatched) = params.dispatched_auction.take() {
                            emit_abandoned_auction(
                                &services,
                                params.auction_observation.take(),
                                dispatched,
                                "processor_init_error",
                            )
                            .await;
                        }
                        return Err(err);
                    }
                };
            // The guard is created before the lazy stream so an auction whose
            // response body is dropped unpolled still logs the loss.
            let dispatched_auction = params.dispatched_auction.take().map(|dispatched| {
                let telemetry = AuctionTelemetryCarry {
                    observation: params.auction_observation.take(),
                    auction_request: params.auction_request.take(),
                };
                (DispatchedAuctionGuard::new(dispatched), telemetry)
            });
            let stream = async_stream::try_stream! {
                let compression = Compression::from_content_encoding(&params.content_encoding);
                let max_body_bytes = settings.publisher.max_buffered_body_bytes;
                let mut decoder = BodyStreamDecoder::new(compression, max_body_bytes);
                let mut encoder = BodyStreamEncoder::new(compression);
                let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE)
                    .with_max_bytes(max_body_bytes);

                // HTML rides the close-body hold so bids land before `</body>`;
                // non-HTML has no injection point, so its auction is collected
                // before any byte streams (matching the buffered finalizer).
                let mut hold_auction = None;
                if let Some((mut guard, telemetry)) = dispatched_auction {
                    if is_html_content_type(&params.content_type) {
                        hold_auction = Some((guard, telemetry));
                    } else if let Some(dispatched) = guard.take() {
                        collect_non_html_auction(
                            dispatched,
                            telemetry,
                            &params,
                            &orchestrator,
                            &services,
                            &settings,
                        )
                        .await;
                        // Collection reached a terminal result; disarm only now
                        // so a drop while the collect await above was still
                        // pending is reported.
                        guard.disarm();
                    }
                }

                if let Some((guard, telemetry)) = hold_auction {
                    let mut state = AuctionHoldState::new(guard, telemetry);
                    let collect_refs = AuctionHoldCollectRefs {
                        price_granularity: params.price_granularity,
                        ad_bids_state: &params.ad_bids_state,
                        orchestrator: &orchestrator,
                        services: &services,
                        settings: &settings,
                    };

                    while let Some(segments) = hold_step_next_chunk(
                        &mut source,
                        &mut decoder,
                        &mut encoder,
                        &mut processor,
                        &mut state,
                        &collect_refs,
                    )
                    .await
                    .map_err(publisher_stream_error)?
                    {
                        for encoded in segments {
                            yield encoded;
                        }
                    }

                    for encoded in hold_finish_segments(
                        &mut processor,
                        &mut decoder,
                        &mut encoder,
                        &mut state,
                        &collect_refs,
                    )
                    .await
                    .map_err(publisher_stream_error)?
                    {
                        yield encoded;
                    }
                } else {
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

    let is_html = is_html_content_type(&params.content_type);

    if !is_html {
        // Non-HTML: collect auction first, then stream.  There is no </body>
        // to hold, so delaying the entire body until collection is acceptable.
        collect_non_html_auction(
            dispatched,
            telemetry,
            params,
            orchestrator,
            services,
            settings,
        )
        .await;
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
        return stream_publisher_body(body, output, params, settings, integration_registry);
    }

    // HTML: build the processor once and drive it chunk by chunk.
    // One-behind buffer: stream chunk N-1 immediately; hold chunk N until origin
    // EOF, then await auction and process chunk N (which contains </body>).
    let mut processor = match create_html_stream_processor(
        &params.origin_host,
        &params.request_host,
        &params.request_scheme,
        settings,
        integration_registry,
        params.ad_slots_script.as_deref().map(str::to_string),
        params.ad_bids_state.clone(),
    ) {
        Ok(processor) => processor,
        Err(err) => {
            emit_abandoned_auction(
                services,
                telemetry.observation,
                dispatched,
                "processor_init_error",
            )
            .await;
            return Err(err);
        }
    };

    let compression = Compression::from_content_encoding(&params.content_encoding);
    stream_html_with_auction_hold(
        body,
        output,
        &mut processor,
        compression,
        AuctionCollectCtx {
            dispatched,
            telemetry,
            price_granularity: params.price_granularity,
            ad_bids_state: &params.ad_bids_state,
            orchestrator,
            services,
            settings,
        },
    )
    .await
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
    dispatched: DispatchedAuction,
    telemetry: AuctionTelemetryCarry,
    price_granularity: PriceGranularity,
    ad_bids_state: &'a Arc<Mutex<Option<String>>>,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    settings: &'a Settings,
}

struct AuctionHoldCollectRefs<'a> {
    price_granularity: PriceGranularity,
    ad_bids_state: &'a Arc<Mutex<Option<String>>>,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    settings: &'a Settings,
}

/// Run the close-body hold loop for HTML bodies, collecting the auction before
/// the raw `</body` tail is processed so `lol_html` sees live bids.
async fn stream_html_with_auction_hold<W: Write, P: StreamProcessor>(
    body: EdgeBody,
    output: &mut W,
    processor: &mut P,
    compression: Compression,
    ctx: AuctionCollectCtx<'_>,
) -> Result<(), Report<TrustedServerError>> {
    if body.is_stream() {
        let max_body_bytes = ctx.settings.publisher.max_buffered_body_bytes;
        return body_close_hold_loop_stream(
            body,
            output,
            processor,
            compression,
            ctx,
            max_body_bytes,
        )
        .await;
    }

    let body = body_as_reader(body)?;
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

/// Async-pull variant of [`body_close_hold_loop`] for live origin streams.
///
/// Shares [`hold_step_next_chunk`] and [`hold_finish_segments`] with the
/// lazy streaming body built by [`publisher_response_into_streaming_response`],
/// so the two async hold paths cannot drift apart.
///
/// No production caller reaches this today: it is only entered through
/// [`buffer_publisher_response_async`], and the buffered adapters (Axum,
/// Cloudflare, Spin) never produce `Body::Stream` because the publisher fetch
/// is gated on `supports_streaming_responses()`. It is groundwork for those
/// adapters' streaming cutover; Fastly uses the lazy stream instead.
async fn body_close_hold_loop_stream<W: Write, P: StreamProcessor>(
    body: EdgeBody,
    writer: &mut W,
    processor: &mut P,
    compression: Compression,
    ctx: AuctionCollectCtx<'_>,
    max_body_bytes: usize,
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
    let mut decoder = BodyStreamDecoder::new(compression, max_body_bytes);
    let mut encoder = BodyStreamEncoder::new(compression);
    let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE).with_max_bytes(max_body_bytes);
    let mut state = AuctionHoldState::new(DispatchedAuctionGuard::new(dispatched), telemetry);
    let collect_refs = AuctionHoldCollectRefs {
        price_granularity,
        ad_bids_state,
        orchestrator,
        services,
        settings,
    };

    while let Some(segments) = hold_step_next_chunk(
        &mut source,
        &mut decoder,
        &mut encoder,
        processor,
        &mut state,
        &collect_refs,
    )
    .await?
    {
        for encoded in segments {
            write_encoded_segment(writer, &encoded)?;
        }
    }

    for encoded in hold_finish_segments(
        processor,
        &mut decoder,
        &mut encoder,
        &mut state,
        &collect_refs,
    )
    .await?
    {
        write_encoded_segment(writer, &encoded)?;
    }
    writer.flush().change_context(TrustedServerError::Proxy {
        message: "Failed to flush output".to_string(),
    })?;
    Ok(())
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
        mut telemetry,
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
                        telemetry.take(),
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
                    if let Err(err) = write_processed_chunk(
                        writer,
                        processor,
                        &ready,
                        false,
                        "Failed to process chunk",
                        "Failed to write chunk",
                    ) {
                        if let Some(dispatched) = dispatched.take() {
                            emit_abandoned_auction(
                                services,
                                telemetry.observation.take(),
                                dispatched,
                                "stream_process_error",
                            )
                            .await;
                        }
                        return Err(err);
                    }

                    if hold_buffer.found_close() {
                        let dispatched = dispatched
                            .take()
                            .expect("should have dispatched auction to collect");
                        collect_stream_auction(
                            dispatched,
                            telemetry.take(),
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
                if let Some(dispatched) = dispatched.take() {
                    emit_abandoned_auction(
                        services,
                        telemetry.observation.take(),
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
pub async fn handle_publisher_request(
    settings: &Settings,
    services: &RuntimeServices,
    kv: Option<&KvIdentityGraph>,
    ec_context: &mut EcContext,
    auction: AuctionDispatch<'_>,
    mut req: Request<EdgeBody>,
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

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
    let mut platform_request = PlatformHttpRequest::new(req, backend_name);
    if services.http_client().supports_streaming_responses() {
        platform_request = platform_request.with_stream_response();
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
    // revalidation; `Surrogate-Control` removal handles the Fastly shared cache.
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
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=0"),
        );
        response.headers_mut().remove("surrogate-control");
        response.headers_mut().remove("fastly-surrogate-control");
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
    use std::future::Future as _;
    use std::io::{self, Read as _, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use brotli::Decompressor;
    use brotli::enc::writer::CompressorWriter;
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;

    use super::*;
    use crate::auction::types::{AdFormat, AdSlot, MediaType};
    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::{
        StubHttpClient, build_services_with_http_client, noop_services,
    };
    use crate::test_support::tests::create_test_settings;
    use edgezero_core::body::Body as EdgeBody;
    use http::{Method, Request as HttpRequest, StatusCode, header};
    use std::sync::Arc;

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
            telemetry: AuctionTelemetryCarry {
                observation: None,
                auction_request: None,
            },
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
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-unified.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
        assert_eq!(response.status(), StatusCode::OK);
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

        let response = handle_tsjs_dynamic(&req, &registry).expect("should handle tsjs request");
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
    fn stream_publisher_body_async_processes_stream_with_auction_hold() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let state = Arc::new(Mutex::new(None));
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
                auction_request: Some(test_auction_request()),
                dispatched_auction: Some(DispatchedAuction::empty_for_test(
                    test_auction_request(),
                    10,
                )),
                price_granularity: crate::price_bucket::PriceGranularity::default(),
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
                html.contains(".adSlots=JSON.parse"),
                "should still inject ad slots. Got: {html}"
            );
            assert!(
                html.contains(".bids=JSON.parse"),
                "should collect auction and inject bids before body close. Got: {html}"
            );
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

    #[test]
    fn publisher_response_streaming_finalize_drops_bodiless_buffered_stream_body() {
        // Fastly requests the origin body as a stream before classification, so
        // a buffered-unmodified response can hold an `EdgeBody::Stream`. The
        // adapter streams any `EdgeBody::Stream` to the client, so bodiless
        // responses must be normalized to carry no body while keeping metadata.
        let settings = Arc::new(create_test_settings());
        let registry = Arc::new(
            IntegrationRegistry::new(&settings).expect("should create integration registry"),
        );
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));

        let cases = [
            (Method::HEAD, StatusCode::OK),
            (Method::GET, StatusCode::NO_CONTENT),
            (Method::GET, StatusCode::RESET_CONTENT),
            (Method::GET, StatusCode::NOT_MODIFIED),
        ];

        for (method, status) in cases {
            let response = Response::builder()
                .status(status)
                .header(header::CONTENT_LENGTH, "42")
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
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok()),
                Some("42"),
                "bodiless {method} {status} must preserve the origin Content-Length"
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
    fn publisher_response_streaming_finalize_holds_auction_and_keeps_gzip_tail() {
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
        // decoder's 32 KiB internal output buffer: the close-body tag then
        // surfaces (and releases the auction hold) mid-stream, while the
        // trailing markup only surfaces at decoder finalization. This guards
        // against the EOF decoded tail being dropped once the hold is gone.
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
            html.contains(".bids=JSON.parse"),
            "should collect the held auction and inject bids. Got tail: {}",
            &html[html.len().saturating_sub(200)..]
        );
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
    fn stream_publisher_body_treats_mixed_case_html_as_html() {
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
            MatchedSlotsContext, build_ad_slots_script, build_auction_request, build_bid_map,
            build_bids_script, html_escape_for_script,
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
