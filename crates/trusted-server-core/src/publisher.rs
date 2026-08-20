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

use std::borrow::Cow;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use brotli::Decompressor;
use brotli::enc::BrotliEncoderParams;
use brotli::enc::writer::CompressorWriter;
use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use flate2::read::ZlibDecoder;
use flate2::write::{GzEncoder, ZlibEncoder};
use futures::StreamExt as _;
use http::{HeaderValue, Method, Request, Response, StatusCode, Uri, header};

use crate::auction::endpoints::{
    merge_auction_eids, resolve_auction_eids, resolve_client_auction_eids,
};
use crate::auction::formats::sanitize_publisher_page_url;
use crate::auction::orchestrator::{
    AuctionOrchestrator, DispatchAuctionOutcome, DispatchedAuction, ERROR_TYPE_ALL,
    ERROR_TYPE_HTTP_STATUS, ERROR_TYPE_LAUNCH_FAILED, ERROR_TYPE_PARSE_RESPONSE,
    ERROR_TYPE_TIMEOUT, ERROR_TYPE_TRANSPORT,
};
use crate::auction::telemetry::{
    AuctionObservationContext, AuctionSource, AuctionTerminalOutcome, build_auction_events,
    emit_auction_events_best_effort_lazy,
};
use crate::auction::types::{
    AuctionContext, AuctionRequest, Bid, DeviceInfo, PublisherInfo, SiteInfo, UserInfo,
};
use crate::cache_policy::{
    CachePolicy, EdgeCacheHeader, cache_control_headers_are_private_or_no_store,
};
use crate::consent::{consent_allows_server_side_auction, gate_eids_by_consent};
use crate::constants::{COOKIE_TS_EIDS, HEADER_X_COMPRESS_HINT};
use crate::cookies::handle_request_cookies;
use crate::creative_opportunities::{
    AdStackGateInput, AssemblyMode, CreativeOpportunitiesConfig, RuntimeAdStackExpected,
    evaluate_ad_stack_gate,
};
use crate::ec::EcContext;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::html_processor::BodyCloseInjection;
use crate::http_util::{RequestInfo, is_navigation_request, serve_static_with_etag};
use crate::integrations::IntegrationRegistry;
use crate::platform::{
    GeoInfo, PlatformBackendSpec, PlatformHttpRequest, RuntimeServices, VarySpec,
};
use crate::price_bucket::{PriceGranularity, price_bucket};
use crate::response_privacy::{enforce_private_no_store, enforce_synthesized_html_cache_privacy};
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::{
    AUCTION_DEBUG_METADATA_ALLOWLIST, AUCTION_DEBUG_UPSTREAM_METADATA_KEYS,
    AuctionDebugCommentFormat, AuctionDebugCommentOptions, AuctionDebugCommentVerbosity, Settings,
};
use crate::streaming_processor::{
    BodyStreamDecoder, BodyStreamEncoder, Compression, GzipDecodeReader, PipelineConfig,
    STREAM_CHUNK_SIZE, StreamProcessor, StreamingPipeline,
};
use crate::streaming_replacer::create_url_replacer;

const SUPPORTED_ENCODING_VALUES: [&str; 3] = ["gzip", "deflate", "br"];
const DEFAULT_PUBLISHER_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(15);
const HEADER_X_TS_C2_CACHE: &str = "x-ts-c2-cache";
const HEADER_X_TS_ASSEMBLY: &str = "x-ts-assembly";

#[derive(Clone, Copy, PartialEq, Eq)]
enum C2ResponseState {
    Hit,
    MissReserved,
    MissStored,
    MissStoreError,
    BypassRequest,
    BypassResponse,
    Unsupported,
    Invalid,
    BackendError,
}

impl C2ResponseState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::MissReserved => "miss-reserved",
            Self::MissStored => "miss-stored",
            Self::MissStoreError => "miss-store-error",
            Self::BypassRequest => "bypass-request",
            Self::BypassResponse => "bypass-response",
            Self::Unsupported => "unsupported",
            Self::Invalid => "invalid",
            Self::BackendError => "backend-error",
        }
    }
}

fn set_c2_response_state(response: &mut Response<EdgeBody>, state: C2ResponseState) {
    response.headers_mut().insert(
        HEADER_X_TS_C2_CACHE,
        HeaderValue::from_static(state.as_str()),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssemblyResponseState {
    EsiParser,
    ByteSeamFallback,
    ByteSeam,
}

impl AssemblyResponseState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EsiParser => "esi-parser",
            Self::ByteSeamFallback => "byte-seam-fallback",
            Self::ByteSeam => "byte-seam",
        }
    }
}

fn set_assembly_response_state(response: &mut Response<EdgeBody>, state: AssemblyResponseState) {
    response.headers_mut().insert(
        HEADER_X_TS_ASSEMBLY,
        HeaderValue::from_static(state.as_str()),
    );
}

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
    if !req.headers().contains_key(header::ACCEPT_ENCODING) {
        return;
    }
    let Some(current) = req
        .headers()
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .map(|value| value.to_str().ok())
        .collect::<Option<Vec<_>>>()
        .map(|values| values.join(", "))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderEncodingError {
    Malformed,
    NoAcceptableEncoding,
}

fn parse_quality_value(value: &str) -> Option<f32> {
    let value = value.trim();
    let (whole, fraction) = value
        .split_once('.')
        .map_or((value, None), |(whole, fraction)| (whole, Some(fraction)));
    let fraction_is_valid = fraction.is_none_or(|fraction| {
        fraction.len() <= 3 && fraction.bytes().all(|byte| byte.is_ascii_digit())
    });
    if !fraction_is_valid {
        return None;
    }
    match whole {
        "0" => value.parse().ok(),
        "1" if fraction.is_none_or(|fraction| fraction.bytes().all(|byte| byte == b'0')) => {
            Some(1.0)
        }
        _ => None,
    }
}

fn negotiate_reader_compression(
    headers: &edgezero_core::http::HeaderMap,
) -> Result<Compression, ReaderEncodingError> {
    if !headers.contains_key(header::ACCEPT_ENCODING) {
        return Ok(Compression::None);
    }

    let mut qualities = Vec::<(String, f32)>::new();
    for field in headers.get_all(header::ACCEPT_ENCODING) {
        let field = field.to_str().map_err(|_| ReaderEncodingError::Malformed)?;
        for item in field
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let mut parts = item.split(';');
            let token = parts
                .next()
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .ok_or(ReaderEncodingError::Malformed)?
                .to_ascii_lowercase();
            if token != "*" && http::HeaderName::from_bytes(token.as_bytes()).is_err() {
                return Err(ReaderEncodingError::Malformed);
            }
            let mut quality = 1.0;
            let mut saw_quality = false;
            for parameter in parts {
                let (name, value) = parameter
                    .trim()
                    .split_once('=')
                    .ok_or(ReaderEncodingError::Malformed)?;
                if !name.trim().eq_ignore_ascii_case("q") || saw_quality {
                    return Err(ReaderEncodingError::Malformed);
                }
                quality = parse_quality_value(value).ok_or(ReaderEncodingError::Malformed)?;
                saw_quality = true;
            }
            if qualities.iter().any(|(seen, _)| seen == &token) {
                return Err(ReaderEncodingError::Malformed);
            }
            qualities.push((token, quality));
        }
    }

    let explicit = |name: &str| {
        qualities
            .iter()
            .find_map(|(candidate, quality)| (candidate == name).then_some(*quality))
    };
    let wildcard = explicit("*");
    let quality_for = |name: &str| explicit(name).or(wildcard).unwrap_or(0.0);
    // Identity is implicitly acceptable at q=1 unless explicitly excluded, or a
    // wildcard q=0 excludes every unlisted coding.
    let identity_quality =
        explicit("identity").unwrap_or_else(|| if wildcard == Some(0.0) { 0.0 } else { 1.0 });

    let candidates = [
        (Compression::Brotli, quality_for("br")),
        (Compression::Gzip, quality_for("gzip")),
        (Compression::Deflate, quality_for("deflate")),
        (Compression::None, identity_quality),
    ];
    let mut selected = None;
    for (compression, quality) in candidates {
        if quality > 0.0 && selected.is_none_or(|(_, best)| quality > best) {
            selected = Some((compression, quality));
        }
    }
    selected
        .map(|(compression, _)| compression)
        .ok_or(ReaderEncodingError::NoAcceptableEncoding)
}

fn set_response_compression(response: &mut Response<EdgeBody>, compression: Compression) {
    let encoding = match compression {
        Compression::None => None,
        Compression::Gzip => Some("gzip"),
        Compression::Deflate => Some("deflate"),
        Compression::Brotli => Some("br"),
    };
    if let Some(encoding) = encoding {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
    } else {
        response.headers_mut().remove(header::CONTENT_ENCODING);
    }
    let varies_on_encoding = response
        .headers()
        .get_all(header::VARY)
        .iter()
        .any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|name| name.trim().eq_ignore_ascii_case("accept-encoding"))
            })
        });
    if !varies_on_encoding {
        response
            .headers_mut()
            .append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    response.headers_mut().remove(header::CONTENT_LENGTH);
}

fn response_compression(response: &Response<EdgeBody>) -> Compression {
    response
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(Compression::from_content_encoding)
        .unwrap_or(Compression::None)
}

fn encode_complete_body(
    body: Vec<u8>,
    compression: Compression,
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    let mut encoder = BodyStreamEncoder::new(compression);
    let mut encoded = encoder.encode_chunk(body)?;
    encoded.extend_from_slice(&encoder.finish()?);
    Ok(encoded)
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
        // Serve core + immediate modules (excludes deferred like prebid).
        let module_ids = integration_registry.js_module_ids_immediate();
        let body = trusted_server_js::concatenate_modules(&module_ids);
        let hash = trusted_server_js::concatenated_hash(&module_ids);
        return Ok(serve_tsjs_static(req, &body, &hash, edge_header));
    }

    if let Some(module_id) = parse_single_module_filename(filename) {
        // Deferred modules and the conditionally injected diagnostics module
        // are served as content-addressed standalone assets. Delivery remains
        // cookie-independent so the static response can stay publicly cached.
        let deferred_ids = integration_registry.js_module_ids_deferred();
        let diagnostics_standalone = module_id
            == crate::integrations::gpt_diagnostics::GPT_DIAGNOSTICS_INTEGRATION_ID
            && integration_registry.integration_enabled(module_id);
        if !deferred_ids.contains(&module_id) && !diagnostics_standalone {
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
    let mut response = serve_static_with_etag(
        body,
        req,
        "application/javascript; charset=utf-8",
        edge_header,
    );
    if request_version_hash(req).is_some_and(|hash| hash == expected_hash) {
        CachePolicy::public_immutable(Duration::from_secs(31_536_000))
            .apply_to_headers(response.headers_mut(), edge_header);
    }
    response
        .headers_mut()
        .insert(HEADER_X_COMPRESS_HINT, HeaderValue::from_static("on"));
    response
}

fn request_version_hash(req: &Request<EdgeBody>) -> Option<&str> {
    req.uri().query()?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "v").then_some(value)
    })
}

/// Extract a module ID from a deferred-module filename like `tsjs-sourcepoint.min.js`.
///
/// Returns `Some(&'static str)` if the filename matches a known JS module ID,
/// `None` otherwise. The caller must additionally verify that the module is
/// both deferred and enabled via the [`IntegrationRegistry`].
#[must_use]
fn parse_single_module_filename(filename: &str) -> Option<&'static str> {
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
    suppress_datadome_client_side_tag: bool,
    gpt_diagnostics:
        Option<&'a crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision>,
    /// See [`HtmlStreamProcessorParams::shared_template_authorized`].
    shared_template_authorized: bool,
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
                ad_bids_state: Arc::clone(params.ad_bids_state.script_cell()),
                suppress_datadome_client_side_tag: params.suppress_datadome_client_side_tag,
                gpt_diagnostics: params.gpt_diagnostics.clone(),
                shared_template_authorized: params.template_cache_key.is_some(),
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
    output_compression: Compression,
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
        output_compression,
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
            shared_template_authorized: params.shared_template_authorized,
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

    let input_compression = Compression::from_content_encoding(&params.content_encoding);
    // A C2 template is always identity bytes. Decode during the transform instead of
    // recompressing and immediately decoding the entire buffered result afterwards.
    let output_compression = if params.template_cache_key.is_some() {
        Compression::None
    } else {
        input_compression
    };
    let mut processor = PublisherBodyProcessor::new(params, settings, integration_registry)?;
    process_body_chunks_async(
        body,
        output,
        &mut processor,
        input_compression,
        output_compression,
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
    input_compression: Compression,
    output_compression: Compression,
    max_body_bytes: usize,
) -> Result<(), Report<TrustedServerError>> {
    let mut decoder = BodyStreamDecoder::new(input_compression, max_body_bytes);
    let mut encoder = BodyStreamEncoder::new(output_compression);
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

/// Output of a single close-body hold step, split at the auction-collection
/// barrier.
///
/// `ready` is the prefix the caller must emit *before* collecting the auction,
/// so a small page whose `</body>` lands in the first source chunk still
/// streams its document prefix immediately instead of stalling behind the
/// auction. `close_found` signals that `</body` was seen: the caller emits
/// `ready`, then awaits [`hold_collect_close_tail`] to collect the auction and
/// emit the held closing tail.
struct HoldStepSegments {
    ready: Vec<bytes::Bytes>,
    close_found: bool,
}

/// Feed one decoded chunk through the close-body hold and processor.
///
/// Returns the ready prefix for the caller to emit — written to a client stream
/// by [`body_close_hold_loop_stream`], yielded from the lazy body by
/// [`publisher_response_into_streaming_response`]. Both async hold paths share
/// this function so their behavior cannot drift apart.
///
/// This step never awaits auction collection: it processes only the bytes the
/// hold buffer releases as ready and reports whether `</body` was seen. Holding
/// the collection out of this step is what lets callers emit the prefix before
/// the auction resolves. On processing failure the pending auction is abandoned
/// before the error is returned.
async fn hold_step_decoded_chunk<P: StreamProcessor>(
    processor: &mut P,
    encoder: &mut BodyStreamEncoder,
    chunk: &[u8],
    state: &mut AuctionHoldState,
    collect_refs: &AuctionCollectDeps<'_>,
) -> Result<HoldStepSegments, Report<TrustedServerError>> {
    let mut ready = Vec::new();
    let bytes: Cow<'_, [u8]> = match state.hold.as_mut() {
        // Once the hold has been released the chunk streams straight through,
        // borrowed rather than copied.
        None => Cow::Borrowed(chunk),
        Some(hold_buffer) => Cow::Owned(hold_buffer.push(chunk)),
    };
    match process_and_encode_chunk(processor, encoder, &bytes, false, "Failed to process chunk") {
        Ok(Some(encoded)) => ready.push(encoded),
        Ok(None) => {}
        Err(err) => {
            abandon_hold_auction(state, collect_refs.services, "stream_process_error").await;
            return Err(err);
        }
    }
    let close_found = state
        .hold
        .as_ref()
        .is_some_and(BodyCloseHoldBuffer::found_close);
    Ok(HoldStepSegments { ready, close_found })
}

/// Collect the dispatched auction and process the held `</body>` tail.
///
/// Call only after [`hold_step_decoded_chunk`] (or
/// [`hold_finish_ready_segments`]) reports `close_found` and the ready prefix
/// has already been emitted:
/// collecting here — after the prefix streams — is what keeps the auction
/// riding alongside transfer instead of blocking it. Collection runs before the
/// tail is processed so `lol_html` sees live bids at the injection point.
async fn hold_collect_close_tail<P: StreamProcessor>(
    processor: &mut P,
    encoder: &mut BodyStreamEncoder,
    state: &mut AuctionHoldState,
    collect_refs: &AuctionCollectDeps<'_>,
) -> Result<Vec<bytes::Bytes>, Report<TrustedServerError>> {
    let mut segments = Vec::new();
    let dispatched = state
        .dispatched
        .take()
        .expect("should have dispatched auction to collect");
    collect_stream_auction(dispatched, state.telemetry.take(), collect_refs).await;
    // Collection reached a terminal result; disarm only now so a drop while the
    // collect await above was still pending is reported.
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
    Ok(segments)
}

/// Pull and decode the next chunk of the close-body hold pipeline, feeding it
/// through [`hold_step_decoded_chunk`].
///
/// Returns `Ok(None)` when the source is exhausted; the caller must then emit
/// [`hold_finish_ready_segments`] followed by [`hold_finish_tail_segments`]. On
/// read or decode failure the pending auction is
/// abandoned before the error is returned. Shared by the write-sink driver
/// ([`body_close_hold_loop_stream`]) and the lazy publisher body stream so
/// the two hold paths cannot drift apart.
async fn hold_step_next_chunk<P: StreamProcessor>(
    source: &mut BodyChunkSource,
    decoder: &mut BodyStreamDecoder,
    encoder: &mut BodyStreamEncoder,
    processor: &mut P,
    state: &mut AuctionHoldState,
    collect_refs: &AuctionCollectDeps<'_>,
) -> Result<Option<HoldStepSegments>, Report<TrustedServerError>> {
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
        return Ok(Some(HoldStepSegments {
            ready: Vec::new(),
            close_found: false,
        }));
    }
    hold_step_decoded_chunk(processor, encoder, &decoded, state, collect_refs)
        .await
        .map(Some)
}

/// Drain the decoder tail at end of the origin stream, returning the prefix the
/// caller must emit before [`hold_finish_tail_segments`].
///
/// A codec can hold document bytes back until its own finalization — the gzip
/// decoder releases the remainder of the final member at `finish()` — and that
/// remainder may be the whole document for a small page. Returning it ahead of
/// collection keeps the invariant the mid-stream path already has: only the
/// closing `</body>` tail waits for the auction, never renderable content.
///
/// On decoder failure the pending auction is abandoned before the error is
/// returned.
async fn hold_finish_ready_segments<P: StreamProcessor>(
    processor: &mut P,
    decoder: &mut BodyStreamDecoder,
    encoder: &mut BodyStreamEncoder,
    state: &mut AuctionHoldState,
    collect_refs: &AuctionCollectDeps<'_>,
) -> Result<Vec<bytes::Bytes>, Report<TrustedServerError>> {
    let decoded_tail = match decoder.finish() {
        Ok(decoded_tail) => decoded_tail,
        Err(err) => {
            abandon_hold_auction(state, collect_refs.services, "stream_decode_error").await;
            return Err(err);
        }
    };
    if decoded_tail.is_empty() {
        return Ok(Vec::new());
    }
    let step =
        hold_step_decoded_chunk(processor, encoder, &decoded_tail, state, collect_refs).await?;
    Ok(step.ready)
}

/// Finalize the close-body hold pipeline after [`hold_finish_ready_segments`].
///
/// Collects the auction if the close-body tag never streamed, processes the held
/// tail plus the processor's final chunk, and emits the encoder trailer. Returns
/// the encoded segments for the caller to emit.
async fn hold_finish_tail_segments<P: StreamProcessor>(
    processor: &mut P,
    encoder: &mut BodyStreamEncoder,
    state: &mut AuctionHoldState,
    collect_refs: &AuctionCollectDeps<'_>,
) -> Result<Vec<bytes::Bytes>, Report<TrustedServerError>> {
    let mut segments = Vec::new();

    // If the hold is still armed the auction was never collected mid-stream:
    // `</body>` arrived only in the decoder tail, or the document had none at
    // all. Collect now and flush the held remainder before finalizing.
    if state.hold.is_some() {
        segments.extend(hold_collect_close_tail(processor, encoder, state, collect_refs).await?);
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
    /// Whether a shared template was authorized for this response.
    ///
    /// Carried rather than re-derived so both seams see the same answer. See
    /// [`effective_assembly_mode`].
    shared_template_authorized: bool,
}

/// The diagnostics decision the template may carry.
///
/// Diagnostics is request-scoped — activated by a cookie or query parameter, and
/// documented as an immutable per-request decision — so it must not reach a shared
/// template.
///
/// It does not leak today even without this gate, but only by coincidence:
/// `requires_private_no_store()` is a strict superset of the conditions under which
/// a script is emitted, and that stamp lands before the C2 gate reads response
/// headers, so the gate refuses. Two independent conditions that happen to align,
/// with nothing enforcing the relationship. This makes the guarantee explicit;
/// `requires_private_no_store_is_a_superset_of_injection` keeps the coincidence as a
/// backstop if this gate is ever removed.
pub(crate) fn template_gpt_diagnostics(
    mode: AssemblyMode,
    decision: Option<crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision>,
) -> Option<crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision> {
    match mode {
        AssemblyMode::Inline => decision,
        AssemblyMode::Esi => None,
    }
}

/// The marker emitted at the `</body>` seam under [`AssemblyMode::Esi`], reserving the
/// place this reader's slots and bids are spliced into.
///
/// An inert HTML comment, deliberately. Template schema v1 used an executable ESI include
/// tag here, when the `esi` crate resolved it at the edge. That crate was removed from the
/// render path because it truncates any element larger than its 16 KB chunk size, and
/// nothing has parsed ESI since. What remained was a tag that *looked* executable, would
/// have been executed by any ESI-enabled layer in front of us, and renders as text in a
/// browser if assembly is ever skipped. A comment cannot do any of those things: an
/// unassembled template degrades to a page with no ads rather than a page with a visible
/// tag.
///
/// Carries no URL. Every byte here is a byte every reader of the shared template
/// receives, so nothing request-scoped may appear, and keeping a URL out also removes
/// any escaping question at the seam.
pub const AD_ASSEMBLY_SEAM: &str = "<!--ts-ad-seam-->";

/// The mode the operator asked for, before availability is taken into account.
///
/// Spelled once, because the mode has to mean the same thing at the cache key, at the
/// seam, and at both hit finalizers. Every one of those re-derived it from the same
/// `Option` chain, and the finalizers had no way to ask at all — which is why they
/// demanded a seam marker of a mode that emits none.
fn configured_assembly_mode(settings: &Settings) -> AssemblyMode {
    settings
        .creative_opportunities
        .as_ref()
        .map(CreativeOpportunitiesConfig::assembly_mode)
        .unwrap_or_default()
}

/// Whether this mode's `</body>` seam emits [`AD_ASSEMBLY_SEAM`].
///
/// The property that decides whether a template is *expected* to have a hole in it, and
/// therefore whether the absence of one is a defect or the design. Only `Esi` splices per
/// reader.
///
/// Matched exhaustively rather than compared against `Esi`, so a new mode has to state
/// its answer here instead of silently inheriting one.
fn mode_emits_seam_marker(mode: AssemblyMode) -> bool {
    match mode {
        AssemblyMode::Inline => false,
        AssemblyMode::Esi => true,
    }
}

/// The assembly mode this response will actually be delivered under.
///
/// The configured mode says what the operator wants; the cache key says whether it is
/// available. A shared mode with no key means the gate refused this response — the
/// origin set a cookie, declared a `Vary` the key does not cover, returned a non-200,
/// and so on — so there is no shared template to build and nothing downstream will
/// assemble one.
///
/// When that happens the request falls back to [`AssemblyMode::Inline`] **entirely**,
/// at every seam. Falling back at one seam and not another is what produced the failure
/// this function exists to prevent: the `</body>` seam emitted a legacy ESI tag because
/// the mode was `Esi`, while assembly was skipped because there was no key, so the reader
/// received a document with unresolved executable ESI markup in it and no bids at all.
///
/// Bypassing is the *normal* case against a real origin, not an edge case, so this path
/// runs far more often than the shared one.
fn effective_assembly_mode(settings: &Settings, shared_template_authorized: bool) -> AssemblyMode {
    let configured = configured_assembly_mode(settings);
    if matches!(configured, AssemblyMode::Inline) || shared_template_authorized {
        return configured;
    }
    log::debug!(
        "assembly mode {configured:?} is unavailable for this response (no shared template \
         was authorized); falling back to inline"
    );
    AssemblyMode::Inline
}

/// What the `</body>` seam should inject, given the assembly mode.
///
/// Explicit rather than inferred. The previous shape read
/// `ad_slots_script.is_some()` inside the element handler, which silently coupled
/// two independent decisions: once [`template_ad_slots_script`] stopped emitting a
/// head script under a shared mode, body-close injection stopped with it.
///
/// `Esi` emits [`AD_ASSEMBLY_SEAM`], an inert HTML comment marking where this reader's
/// slots and bids are spliced in. Assembly is a byte split on that comment, performed by
/// this crate on both the miss and the hit path; no ESI layer is involved.
pub(crate) fn body_close_injection(
    mode: AssemblyMode,
    head_script_present: bool,
) -> BodyCloseInjection {
    match mode {
        // Per-navigation and never shared, so gating on slot presence is correct.
        AssemblyMode::Inline => {
            if head_script_present {
                BodyCloseInjection::InlineBids
            } else {
                BodyCloseInjection::None
            }
        }
        // Constant across every request that reaches the transform — which is what
        // makes it safe in a shared template.
        AssemblyMode::Esi => BodyCloseInjection::Marker(AD_ASSEMBLY_SEAM.to_string()),
    }
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
    );

    let assembly_mode = effective_assembly_mode(params.settings, params.shared_template_authorized);
    let body_close = body_close_injection(assembly_mode, params.ad_slots_script.is_some());

    let gpt_diagnostics = template_gpt_diagnostics(assembly_mode, params.gpt_diagnostics);

    let config = config
        .with_ad_state(params.ad_slots_script, params.ad_bids_state)
        .with_gpt_diagnostics(gpt_diagnostics)
        .with_body_close(body_close)
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
    /// A shared template read from C2, to be assembled on the way out.
    ///
    /// Distinct from [`Self::Stream`] because the bytes are **already transformed** —
    /// running them through `lol_html` again would inject a second tsjs `<script>` and
    /// re-rewrite already-rewritten URLs. All this needs is the seam split.
    ///
    /// Carried to the finalizer rather than assembled at the read, because assembling
    /// eagerly means buffering: the reader would wait for the auction before the first
    /// byte, which measured ~100x worse TTFB than doing nothing. The finalizer owns the
    /// `Arc`s a `'static` stream needs.
    ///
    /// Spike-only, for the #1009 ESI validation.
    AssembleTemplate {
        /// Response with every header already set. `Content-Length` must stay absent:
        /// the assembled length is unknown until bids resolve.
        response: Response<EdgeBody>,
        /// The cached template, containing exactly one unresolved seam marker.
        template: Vec<u8>,
        /// Auction and injection state, same as [`Self::Stream`].
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

fn response_cache_control_is_private_or_no_store(response: &Response<EdgeBody>) -> bool {
    cache_control_headers_are_private_or_no_store(response.headers())
}

fn apply_publisher_asset_cache_policy(
    settings: &Settings,
    path: &str,
    method: &Method,
    edge_header: EdgeCacheHeader,
    response: &mut Response<EdgeBody>,
) -> Result<(), Report<TrustedServerError>> {
    let is_cacheable_method = *method == Method::GET || *method == Method::HEAD;
    if !is_cacheable_method
        || response_cache_control_is_private_or_no_store(response)
        || response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(is_html_content_type)
    {
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
    /// Where to store the transformed template, or [`None`] to store nothing.
    ///
    /// `Some` only when [`c2_bypass_reason`] cleared the response, so the key's
    /// presence *is* the decision — there is no second place that could disagree with
    /// the gate, and no way to reach the store without having passed it.
    ///
    /// Spike-only, for the #1009 ESI validation.
    pub(crate) template_cache_key: Option<AuthorizedTemplateStore>,
    /// Slot definitions for the `</body>` seam under a shared mode, as JSON.
    ///
    /// Request-scoped, so it travels with the request rather than into the template.
    pub(crate) seam_ad_slots: Option<String>,
    /// Origin policy headers to store with the template and replay on a hit.
    pub(crate) policy_headers: Vec<(String, String)>,
    pub(crate) content_encoding: String,
    pub(crate) origin_host: String,
    pub(crate) origin_url: String,
    pub(crate) request_host: String,
    pub(crate) request_scheme: String,
    pub(crate) content_type: String,
    pub(crate) ad_slots_script: Option<String>,
    pub(crate) ad_bids_state: AdBidsState,
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
}

/// Response-authorized C2 insert inputs. The key is built before origin lookup; the
/// lifetime is only known after the origin proves this representation is shareable.
pub(crate) struct AuthorizedTemplateStore {
    reservation: crate::platform::TemplateCacheReservation,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TemplateStoreOutcome {
    Stored,
    Expired,
    Error,
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
///
/// # Panics
///
/// Panics if the `ad_bids_state` mutex is poisoned, which requires a prior panic while
/// it was held. Every holder is a short, infallible read or write of an `Option<String>`.
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
            // Authorized C2 transforms are emitted as identity by
            // `process_response_streaming_async`; inline transforms retain the origin
            // coding. This avoids recompressing and immediately decoding a full document.
            let bytes = output.into_inner();
            // Store first, assemble second — never the reverse. The stored bytes are
            // shared between visitors; the assembled ones carry this visitor's bids.
            // Swapping these two lines is the C3 leak.
            // Read before the store: `store_template_if_authorized` *takes* the key so a
            // request cannot store twice, which would leave nothing for assembly to gate
            // on.
            let was_authorized = params.template_cache_key.is_some();
            let bytes = if response_carries_a_seam_marker(was_authorized, settings) {
                normalize_fresh_template_seam(bytes)
            } else {
                bytes
            };
            // Validate before the store, not after.
            //
            // `assemble_if_shared` does the same split and would reject a malformed
            // template — but only once it had already been written, so every later
            // reader was served an unusable entry until it expired or was purged. The
            // one request that produced it failed loudly; everyone after it hit a
            // template with no hole in it.
            //
            // The scan runs twice on a miss as a result. A miss is already paying an
            // origin fetch and a full `lol_html` transform, and a wrong entry in a
            // shared cache outlives the request that wrote it.
            if response_carries_a_seam_marker(was_authorized, settings) {
                split_template_at_seam(&bytes).change_context_lazy(|| {
                    crate::error::TrustedServerError::Proxy {
                        message: "refusing to store a template with no usable seam marker"
                            .to_string(),
                    }
                })?;
            }
            // Publisher-authored ESI is outside TS's template schema. If it entered C2,
            // a warm hit would stream it without passing through the cold parser that
            // rejects it. Revoke the reservation before storage and keep this response
            // on the portable byte-seam path.
            let contains_publisher_esi = was_authorized && contains_esi_directive(&bytes);
            if contains_publisher_esi {
                log::warn!(
                    "c2_template_cache bypass: transformed response contains publisher-authored ESI"
                );
                params.template_cache_key.take();
            }
            let store_outcome = store_template_if_authorized(services, &mut params, &bytes).await;
            if was_authorized {
                set_c2_response_state(
                    &mut response,
                    match (contains_publisher_esi, store_outcome) {
                        (true, _) => C2ResponseState::BypassResponse,
                        (false, Some(TemplateStoreOutcome::Stored)) => C2ResponseState::MissStored,
                        (false, Some(TemplateStoreOutcome::Expired)) => {
                            C2ResponseState::BypassResponse
                        }
                        (false, Some(TemplateStoreOutcome::Error) | None) => {
                            C2ResponseState::MissStoreError
                        }
                    },
                );
            }
            let (bytes, assembly_state) = assemble_if_shared(
                was_authorized,
                !contains_publisher_esi,
                settings,
                &params,
                services,
                bytes,
            )?;
            if let Some(state) = assembly_state {
                set_assembly_response_state(&mut response, state);
            }
            let bytes = if was_authorized {
                encode_complete_body(bytes, response_compression(&response))?
            } else {
                bytes
            };
            response.headers_mut().insert(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from(bytes.len() as u64),
            );
            *response.body_mut() = EdgeBody::from(bytes);
            Ok(response)
        }
        PublisherResponse::AssembleTemplate {
            mut response,
            template,
            mut params,
        } => {
            // Buffered adapters have no streaming to preserve, so eager assembly costs
            // them nothing. The streaming finalizer must not do this.
            if let Some(dispatched) = params.dispatched_auction.take() {
                collect_stream_auction(
                    dispatched,
                    AuctionTelemetryCarry {
                        observation: params.auction_observation.take(),
                        auction_request: params.auction_request.take(),
                    },
                    &AuctionCollectDeps {
                        price_granularity: params.price_granularity,
                        ad_bids_state: &params.ad_bids_state,
                        orchestrator,
                        services,
                        settings,
                        request_origin: request_origin(
                            &params.request_scheme,
                            &params.request_host,
                        ),
                    },
                )
                .await;
            }
            let (head, tail) = split_template_at_seam(&template).change_context_lazy(|| {
                crate::error::TrustedServerError::Proxy {
                    message: "cached template has no usable seam marker".to_string(),
                }
            })?;
            let seam = seam_script_for(&params);
            let mut assembled = Vec::with_capacity(head.len() + seam.len() + tail.len());
            assembled.extend_from_slice(head);
            assembled.extend_from_slice(seam.as_bytes());
            assembled.extend_from_slice(tail);
            let assembled = encode_complete_body(assembled, response_compression(&response))?;
            response.headers_mut().insert(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from(assembled.len() as u64),
            );
            *response.body_mut() = EdgeBody::from(assembled);
            Ok(response)
        }
        PublisherResponse::PassThrough { mut response, body } => {
            *response.body_mut() = body;
            Ok(response)
        }
    }
}

/// Splices this visitor's slots and bids into the seam, if the mode assembles.
///
/// Gives the platform assembler the already-buffered cold document. Fastly resolves a
/// synthetic ESI include with the repaired parser; adapters without an assembler and
/// documents the parser rejects use the same validated byte split as the warm path.
///
/// Called *after* [`store_template_if_authorized`], never before: what is stored must be
/// the template every visitor shares.
///
/// # Errors
///
/// Returns an error if the seam marker is missing or repeated.
fn assemble_if_shared(
    was_authorized: bool,
    platform_assembly_allowed: bool,
    settings: &Settings,
    params: &OwnedProcessResponseParams,
    services: &RuntimeServices,
    bytes: Vec<u8>,
) -> Result<(Vec<u8>, Option<AssemblyResponseState>), Report<crate::error::TrustedServerError>> {
    if !response_carries_a_seam_marker(was_authorized, settings) {
        return Ok((bytes, None));
    }

    let (head, tail) = split_template_at_seam(&bytes).change_context_lazy(|| {
        crate::error::TrustedServerError::Proxy {
            message: "shared template has no usable seam marker".to_string(),
        }
    })?;
    let seam = seam_script_for(params);

    let platform_result = platform_assembly_allowed.then(|| {
        services
            .template_assembler()
            .assemble(&bytes, seam.as_bytes())
    });
    match platform_result {
        Some(Ok(assembled))
            if !assembled
                .windows(AD_ASSEMBLY_SEAM.len())
                .any(|window| window == AD_ASSEMBLY_SEAM.as_bytes()) =>
        {
            return Ok((assembled, Some(AssemblyResponseState::EsiParser)));
        }
        Some(Ok(_)) => {
            log::warn!(
                "platform template assembler left the shared seam unresolved; using byte-seam fallback"
            );
        }
        Some(Err(err)) => {
            log::warn!("platform template assembly failed: {err}; using byte-seam fallback");
        }
        None => {}
    }

    let mut out = Vec::with_capacity(head.len() + seam.len() + tail.len());
    out.extend_from_slice(head);
    out.extend_from_slice(seam.as_bytes());
    out.extend_from_slice(tail);
    Ok((out, Some(AssemblyResponseState::ByteSeamFallback)))
}

/// Fingerprint of every configuration input plus the compiled browser bundle.
///
/// This intentionally over-invalidates. Trying to maintain a hand-written list already
/// omitted publisher origin identity and creative-opportunity shaping fields. A digest of
/// the complete typed settings cannot expose secret values and makes future config fields
/// safe by default: a change misses until someone proves it irrelevant, never cross-serves
/// an old template under new behavior.
///
/// # Panics
///
/// Does not panic: serializing the already-deserialized typed settings to a JSON value is
/// infallible for this schema.
fn template_fingerprint(settings: &Settings) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    hasher.update(
        trusted_server_js::concatenated_hash(&trusted_server_js::all_module_ids()).as_bytes(),
    );
    // `serde_json::Value` uses a sorted object map without `preserve_order`, making
    // independently deserialized HashMaps canonical before they are serialized again.
    let canonical = serde_json::to_value(settings)
        .and_then(|value| serde_json::to_vec(&value))
        .expect("serializing typed settings should be infallible");
    hasher.update(canonical);
    hex::encode(hasher.finalize())
}

/// Whether this response's transformed bytes are expected to carry a seam marker.
///
/// Gated on the *authorization*, not on the configured mode alone. A bypassed response
/// fell back to inline and therefore carries no marker; validating or splitting it would
/// fail and turn an ordinary bypass — the common case against a real origin — into a
/// 500. Both conditions, and neither alone: only `Esi` emits a marker, and only an
/// authorized response has one to find.
fn response_carries_a_seam_marker(was_authorized: bool, settings: &Settings) -> bool {
    was_authorized && mode_emits_seam_marker(configured_assembly_mode(settings))
}

/// Whether bytes contain an ESI directive that came from publisher content.
///
/// TS's stored seam is an inert HTML comment, so any `<esi:` sequence is outside the
/// cache schema. The conservative scan also rejects the sequence inside scripts and
/// comments: bypassing an optimization is safer than sharing edge instructions.
fn contains_esi_directive(bytes: &[u8]) -> bool {
    bytes
        .windows(b"<esi:".len())
        .any(|window| window.eq_ignore_ascii_case(b"<esi:"))
}

/// What this request splices into the seam marker: a `<script>`, or nothing at all.
///
/// `seam_ad_slots` is `None` exactly when the ad stack did not run — bot, prefetch,
/// consent-denied, auction kill switch. Emitting a seam anyway, with `[]` slots, still
/// calls `scheduleInitialAdInit`, which schedules `adInit` for precisely the traffic
/// that opted out. Absent is not the same as empty here.
///
/// Shared by the miss path and by **both** hit finalizers. They previously each spelled
/// the decision out, and the two hit paths spelled it `unwrap_or("[]")` — so the gate
/// held on a cache miss and was ignored on every cache hit.
fn seam_script_for(params: &OwnedProcessResponseParams) -> String {
    params
        .seam_ad_slots
        .as_deref()
        .map(|slots| params.ad_bids_state.build_seam_script(slots))
        .unwrap_or_default()
}

/// Builds the injection state a cached template needs on the way out.
///
/// The template carries no auction state — that is what makes it shareable — so the
/// per-reader parts are attached here, from this request.
fn build_template_assembly_params(
    entry: &crate::platform::TemplateEntry,
    settings: &Settings,
    request_host: &str,
    request_scheme: &str,
    price_granularity: PriceGranularity,
    ad_bids_state: AdBidsState,
) -> OwnedProcessResponseParams {
    OwnedProcessResponseParams {
        // Already stored; storing again on a hit would be pointless work.
        template_cache_key: None,
        seam_ad_slots: None,
        policy_headers: Vec::new(),
        content_encoding: entry.metadata.content_encoding.clone(),
        origin_host: String::new(),
        origin_url: settings.publisher.origin_url.clone(),
        request_host: request_host.to_string(),
        request_scheme: request_scheme.to_string(),
        content_type: entry.metadata.content_type.clone(),
        // The template already carries the head seam; re-injecting would duplicate it.
        ad_slots_script: None,
        ad_bids_state,
        auction_observation: None,
        auction_request: None,
        dispatched_auction: None,
        price_granularity,
        gpt_diagnostics: None,
        suppress_datadome_client_side_tag: false,
    }
}

/// Splits a template at its seam marker.
///
/// # Errors
///
/// Returns [`SeamError`] when the marker is absent or appears more than once. Both mean
/// the template is not one this arm produced, and assembling it anyway would serve a page
/// with either no bids or a visible marker in it.
fn split_template_at_seam(template: &[u8]) -> Result<(&[u8], &[u8]), SeamError> {
    let marker = AD_ASSEMBLY_SEAM.as_bytes();
    let mut found = template
        .windows(marker.len())
        .enumerate()
        .filter(|(_, w)| *w == marker)
        .map(|(at, _)| at);
    let at = found.next().ok_or(SeamError::Missing)?;
    if found.next().is_some() {
        return Err(SeamError::Repeated);
    }
    Ok((&template[..at], &template[at + marker.len()..]))
}

/// Make a freshly transformed ESI template contain one unambiguous seam.
///
/// `lol_html` emits the marker at `</body>`. HTML fragments and malformed-but-browser-
/// renderable documents may have no body handler, so append a terminal seam rather than
/// converting a valid origin 200 into a TS 500. If any later processing creates an
/// ambiguous result, neutralize every existing marker and mint a new terminal seam. That
/// avoids guessing which occurrence belongs to this transform.
fn normalize_fresh_template_seam(mut template: Vec<u8>) -> Vec<u8> {
    let marker = AD_ASSEMBLY_SEAM.as_bytes();
    let positions = template
        .windows(marker.len())
        .enumerate()
        .filter_map(|(at, window)| (window == marker).then_some(at))
        .collect::<Vec<_>>();

    if positions.is_empty() {
        log::warn!("c2_template_cache transform emitted no body seam; appending a terminal seam");
        template.extend_from_slice(marker);
        return template;
    }

    if positions.len() > 1 {
        let mut escaped = marker.to_vec();
        // Byte 4 is the first byte inside `<!-- ... -->`; changing it preserves an
        // invisible comment and keeps offsets stable.
        escaped[4] = b'x';
        for at in &positions {
            template[*at..*at + marker.len()].copy_from_slice(&escaped);
        }
        template.extend_from_slice(marker);
        log::warn!(
            "c2_template_cache neutralized {} ambiguous seam markers and appended a terminal seam",
            positions.len()
        );
    }
    template
}

/// Why a template could not be split at its seam.
#[derive(Debug, derive_more::Display)]
pub(crate) enum SeamError {
    /// No marker: the template predates the current transform, or was never templatized.
    #[display("template contains no seam marker")]
    Missing,
    /// More than one marker, which the seam is never supposed to emit.
    #[display("template contains more than one seam marker")]
    Repeated,
}

impl core::error::Error for SeamError {}

/// Builds the response served from a C2 hit.
///
/// Every header is constructed here rather than replayed from the stored entry, so no
/// origin header can reach a second visitor through the cache.
///
/// # Why this sets `private, no-store` itself
///
/// A C2 hit returns **before** the origin fetch, and therefore before the point where
/// the publisher path stamps `private, no-store` and strips validators. Omitting it
/// here does not fall back to a safe default — it emits HTML with no `Cache-Control` at
/// all, which is heuristically cacheable by browsers and intermediaries. That is a
/// shared cache of an assembled per-user response: the C3 the design forbids outright.
///
/// Asserting the absence of `public`/`s-maxage`/`Surrogate-Control` would not have
/// caught it. Nothing was present to forbid.
///
/// # Errors
///
/// Returns an error if the stored metadata cannot be rendered as header values, which
/// would mean a corrupt entry.
///
/// Spike-only, for the #1009 ESI validation.
fn build_cached_template_response(
    entry: &crate::platform::TemplateEntry,
    reader_compression: Compression,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let invalid = |what: &str| TrustedServerError::Proxy {
        message: format!("cached template has an unusable {what}"),
    };
    let mut response = Response::new(EdgeBody::empty());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&entry.metadata.content_type)
            .change_context_lazy(|| invalid("content type"))?,
    );
    // Stored templates are identity bytes; final reader negotiation is applied after
    // assembly rather than replaying the origin's representation.
    if !entry.metadata.content_encoding.is_empty() && entry.metadata.content_encoding != "identity"
    {
        response.headers_mut().insert(
            header::CONTENT_ENCODING,
            HeaderValue::from_str(&entry.metadata.content_encoding)
                .change_context_lazy(|| invalid("content encoding"))?,
        );
    }
    // No `Content-Length`. The assembled length is not known until bids resolve, and on
    // this adapter headers commit before the first body byte — so a length guessed here
    // could not be corrected later.
    response.headers_mut().remove(header::CONTENT_LENGTH);

    // Policy headers are replayed; per-reader and cache-controlling ones are not, which
    // is what the allowlist encodes. Without this a hit silently dropped the origin's
    // Content-Security-Policy and framing protection — a weaker page, served faster.
    for (name, value) in &entry.metadata.policy_headers {
        let name = header::HeaderName::from_bytes(name.as_bytes())
            .change_context_lazy(|| invalid("policy header name"))?;
        if !crate::platform::REPLAYABLE_POLICY_HEADERS.contains(&name.as_str()) {
            return Err(Report::new(invalid("policy header allowlist")));
        }
        let value =
            HeaderValue::from_str(value).change_context_lazy(|| invalid("policy header value"))?;
        response.headers_mut().append(name, value);
    }
    // Last, after replay. Metadata decoding rejects cache-controlling names, and this
    // terminal stamp is defense in depth for direct/test entries and future format bugs.
    enforce_private_no_store(&mut response);
    set_response_compression(&mut response, reader_compression);
    set_c2_response_state(&mut response, C2ResponseState::Hit);
    set_assembly_response_state(&mut response, AssemblyResponseState::ByteSeam);
    Ok(response)
}

/// Writes the transformed template to the shared cache, if the gate authorized it.
///
/// The key's presence is the authorization: it is `Some` only when
/// [`c2_bypass_reason`] cleared the response, so this cannot store something the gate
/// rejected. Takes the key rather than borrowing it, so a second call for the same
/// request stores nothing.
///
/// Failures are logged and swallowed. A cache that cannot be written is a slower
/// service, not a broken one, and the whole point of C2 is that the response is
/// reproducible without it.
///
/// Spike-only, for the #1009 ESI validation.
async fn store_template_if_authorized(
    _services: &RuntimeServices,
    params: &mut OwnedProcessResponseParams,
    bytes: &[u8],
) -> Option<TemplateStoreOutcome> {
    let store = params.template_cache_key.take()?;
    let max_age = store.expires_at.saturating_duration_since(Instant::now());
    if max_age.is_zero() {
        log::debug!(
            "c2_template_cache store skipped: origin freshness expired during transformation"
        );
        return Some(TemplateStoreOutcome::Expired);
    }
    let metadata = crate::platform::TemplateMetadata {
        // `identity`, not the origin's encoding. The caller decoded before storing,
        // because the seam split is textual — so recording the origin's encoding here
        // would make a cache hit declare `Content-Encoding: gzip` over plaintext bytes,
        // which is the same undecodable response one layer along.
        content_encoding: "identity".to_string(),
        policy_headers: params.policy_headers.clone(),
        content_type: params.content_type.clone(),
        schema_version: crate::platform::TEMPLATE_SCHEMA_VERSION,
        body_len: bytes.len() as u64,
    };
    match store.reservation.insert(&metadata, bytes.to_vec(), max_age) {
        Ok(()) => {
            // Reports whether the seam marker made it into the stored bytes. The marker
            // is deliberately invisible from the outside — assembly replaces it before
            // the response is sent, on both the miss and hit paths — so this log line is
            // the only way to confirm the template really has a hole in it rather than
            // per-reader bids baked in.
            log::debug!(
                "c2_template_cache stored {} bytes (seam marker present: {})",
                bytes.len(),
                bytes
                    .windows(AD_ASSEMBLY_SEAM.len())
                    .any(|w| w == AD_ASSEMBLY_SEAM.as_bytes())
            );
            Some(TemplateStoreOutcome::Stored)
        }
        Err(err) => {
            log::warn!("c2_template_cache store failed: {err}");
            Some(TemplateStoreOutcome::Error)
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
/// Panics if the `ad_bids_state` mutex is poisoned, which requires a prior panic while
/// it was held. Every holder is a short, infallible read or write of an `Option<String>`.
pub async fn publisher_response_into_streaming_response(
    publisher_response: PublisherResponse,
    method: &Method,
    settings: Arc<Settings>,
    integration_registry: &IntegrationRegistry,
    orchestrator: Arc<AuctionOrchestrator>,
    services: RuntimeServices,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // A template can only be stored once the transform has produced every byte, and
    // streaming hands bytes to the client as they are produced rather than collecting
    // them. Shared modes therefore take the buffered finalizer, which already
    // materializes the transformed body.
    //
    // Deliberately keyed on the store authorization rather than on the assembly mode:
    // a shared-mode response the gate rejected has nothing to store, so it keeps
    // streaming. `Inline` — the shipped path — never reaches this branch at all, which
    // is the point. The spike cannot regress production latency by construction.
    //
    // The cost is that a C2 *miss* buffers. That is the right trade: misses are already
    // paying an origin fetch and a full transform, and what the spike measures is the
    // hit, where there is no origin fetch to stream from in the first place.
    if matches!(
        &publisher_response,
        PublisherResponse::Stream { params, .. } if params.template_cache_key.is_some()
    ) {
        return buffer_publisher_response_async(
            publisher_response,
            method,
            &settings,
            integration_registry,
            &orchestrator,
            &services,
        )
        .await;
    }

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
        PublisherResponse::AssembleTemplate {
            mut response,
            template,
            mut params,
        } => {
            if !response_carries_body(method, response.status()) {
                make_response_bodiless(&mut response);
                return Ok(response);
            }

            let services = services.clone();
            let settings = Arc::clone(&settings);
            let orchestrator = Arc::clone(&orchestrator);
            let compression = response_compression(&response);
            // Arm the drop warning before constructing the lazy body. A reader can
            // disconnect after receiving the cached article prefix but before the seam
            // is polled, just like on the ordinary streaming path.
            let dispatched_auction = params.dispatched_auction.take().map(|dispatched| {
                let telemetry = AuctionTelemetryCarry {
                    observation: params.auction_observation.take(),
                    auction_request: params.auction_request.take(),
                };
                (DispatchedAuctionGuard::new(dispatched), telemetry)
            });

            // This is the whole point of the variant. The template's head goes out
            // immediately, so the article paints while the auction is still running; the
            // only wait is at the seam, at the very end of the document. Assembling
            // eagerly instead measured ~100x worse TTFB than doing nothing.
            let stream = async_stream::try_stream! {
                let (head, tail) = split_template_at_seam(&template)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                let mut encoder = BodyStreamEncoder::new(compression);
                let encoded_head = encoder
                    .encode_chunk(head.to_vec())
                    .map_err(publisher_stream_error)?;
                if !encoded_head.is_empty() {
                    yield bytes::Bytes::from(encoded_head);
                }

                if let Some((mut guard, telemetry)) = dispatched_auction
                    && let Some(dispatched) = guard.take()
                {
                    collect_stream_auction(
                        dispatched,
                        telemetry,
                        &AuctionCollectDeps {
                            price_granularity: params.price_granularity,
                            ad_bids_state: &params.ad_bids_state,
                            orchestrator: &orchestrator,
                            services: &services,
                            settings: &settings,
                            request_origin: request_origin(
                                &params.request_scheme,
                                &params.request_host,
                            ),
                        },
                    )
                    .await;
                    guard.disarm();
                }

                let seam = seam_script_for(&params);
                if !seam.is_empty() {
                    let encoded_seam = encoder
                        .encode_chunk(seam.into_bytes())
                        .map_err(publisher_stream_error)?;
                    if !encoded_seam.is_empty() {
                        yield bytes::Bytes::from(encoded_seam);
                    }
                }
                let encoded_tail = encoder
                    .encode_chunk(tail.to_vec())
                    .map_err(publisher_stream_error)?;
                if !encoded_tail.is_empty() {
                    yield bytes::Bytes::from(encoded_tail);
                }
                let trailer = encoder.finish().map_err(publisher_stream_error)?;
                if !trailer.is_empty() {
                    yield bytes::Bytes::from(trailer);
                }
            };
            *response.body_mut() = EdgeBody::from_stream::<_, std::io::Error>(stream);
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
                    let collect_refs = AuctionCollectDeps {
                        price_granularity: params.price_granularity,
                        ad_bids_state: &params.ad_bids_state,
                        orchestrator: &orchestrator,
                        services: &services,
                        settings: &settings,
                        request_origin: request_origin(
                            &params.request_scheme,
                            &params.request_host,
                        ),
                    };

                    while let Some(step) = hold_step_next_chunk(
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
                        // Emit the ready prefix before collecting the auction so
                        // the client receives the document up to `</body>` while
                        // the auction rides alongside transfer.
                        for encoded in step.ready {
                            yield encoded;
                        }
                        if step.close_found {
                            for encoded in hold_collect_close_tail(
                                &mut processor,
                                &mut encoder,
                                &mut state,
                                &collect_refs,
                            )
                            .await
                            .map_err(publisher_stream_error)?
                            {
                                yield encoded;
                            }
                        }
                    }

                    // Whatever the decoder released at finalization is emitted
                    // before collection, for the same reason as the mid-stream
                    // prefix above: a small compressed page can surface its
                    // whole document here.
                    for encoded in hold_finish_ready_segments(
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
                    for encoded in hold_finish_tail_segments(
                        &mut processor,
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

/// Returns whether a request can render an HTML document context.
fn is_html_document_request(req: &Request<EdgeBody>) -> bool {
    if let Some(destination) = req
        .headers()
        .get("sec-fetch-dest")
        .and_then(|value| value.to_str().ok())
    {
        return matches!(
            destination.trim().to_ascii_lowercase().as_str(),
            "document" | "embed" | "fencedframe" | "frame" | "iframe" | "object"
        );
    }

    is_navigation_request(req)
}

/// Removes request headers that can produce a bodyless or partial origin response.
fn strip_conditional_and_range_headers(req: &mut Request<EdgeBody>) {
    req.headers_mut().remove(header::IF_NONE_MATCH);
    req.headers_mut().remove(header::IF_MODIFIED_SINCE);
    req.headers_mut().remove(header::RANGE);
    req.headers_mut().remove(header::IF_RANGE);
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
    if suppress_datadome_client_side_tag
        && response_carries_body(method, response.status())
        && is_html_content_type(content_type)
    {
        enforce_synthesized_html_cache_privacy(response);
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
        ad_bids_state: params.ad_bids_state.script_cell(),
        suppress_datadome_client_side_tag: params.suppress_datadome_client_side_tag,
        gpt_diagnostics: params.gpt_diagnostics.as_ref(),
        shared_template_authorized: params.template_cache_key.is_some(),
    };
    let input_compression = Compression::from_content_encoding(&params.content_encoding);
    let output_compression = if params.template_cache_key.is_some() {
        Compression::None
    } else {
        input_compression
    };
    process_response_streaming(body, output, &borrowed, output_compression)
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
    let mut processor = match create_html_stream_processor(HtmlStreamProcessorParams {
        origin_host: &params.origin_host,
        request_host: &params.request_host,
        request_scheme: &params.request_scheme,
        settings,
        integration_registry,
        ad_slots_script: params.ad_slots_script.as_deref().map(str::to_string),
        ad_bids_state: Arc::clone(params.ad_bids_state.script_cell()),
        suppress_datadome_client_side_tag: params.suppress_datadome_client_side_tag,
        gpt_diagnostics: params.gpt_diagnostics.clone(),
        shared_template_authorized: params.template_cache_key.is_some(),
    }) {
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

    let input_compression = Compression::from_content_encoding(&params.content_encoding);
    let output_compression = if params.template_cache_key.is_some() {
        Compression::None
    } else {
        input_compression
    };
    stream_html_with_auction_hold(
        body,
        output,
        &mut processor,
        input_compression,
        output_compression,
        AuctionCollectCtx {
            dispatched,
            telemetry,
            deps: AuctionCollectDeps {
                price_granularity: params.price_granularity,
                ad_bids_state: &params.ad_bids_state,
                orchestrator,
                services,
                settings,
                request_origin: request_origin(&params.request_scheme, &params.request_host),
            },
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

/// Write winning bids from an auction result into the shared `ad_bids_state` lock.
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

/// This request's auction result, in both forms the seams need.
///
/// The inline `</body>` seam injects a rendered `<script>`; the shared-template seam
/// splices the bids into a script it builds itself, and therefore needs them
/// structured. [`write_bids_to_state`] derives both from one bid map under one call, so
/// the two can never describe different auctions.
///
/// # Why the map is carried rather than recovered
///
/// An earlier revision stored only the script and reconstructed the map by un-escaping
/// it. [`html_escape_for_script`] escapes `"` as `\"`, so the extracted text was invalid
/// JSON for every non-empty map; `serde_json::from_str` failed and `unwrap_or_default()`
/// turned the failure into `{}`. Shared modes therefore served **zero bids**, silently,
/// on every request that had any. Every fixture had empty bids, so nothing caught it.
#[derive(Clone, Default)]
pub(crate) struct AdBidsState {
    /// Rendered bids `<script>`. Shared with the HTML processor's `</body>` handler,
    /// which is why this stays an `Option<String>` cell rather than moving inside the
    /// same lock as the map.
    script: Arc<Mutex<Option<String>>>,
    /// The same bids, structured, for the shared-template seam.
    bids: Arc<Mutex<serde_json::Map<String, serde_json::Value>>>,
    /// Optional per-request diagnostics emitted before either bids-script shape.
    debug_prefix: Arc<Mutex<String>>,
}

#[cfg(test)]
impl AdBidsState {
    /// A state whose rendered script is already set, for tests that exercise the inline
    /// `</body>` seam without running an auction.
    fn with_script(script: &str) -> Self {
        let state = Self::default();
        *state.script.lock().expect("should lock bid script") = Some(script.to_string());
        state
    }
}

impl AdBidsState {
    /// The cell the HTML processor reads at the `</body>` seam.
    pub(crate) fn script_cell(&self) -> &Arc<Mutex<Option<String>>> {
        &self.script
    }

    /// Record one auction result, rendering the script from the same map that is
    /// stored, so the two representations cannot drift.
    fn set(&self, bid_map: serde_json::Map<String, serde_json::Value>) {
        let bids_script = build_bids_script(&bid_map);
        *self.script.lock().expect("should lock bid script") = Some(bids_script);
        *self.bids.lock().expect("should lock bid map") = bid_map;
    }

    /// The structured bids the shared-template seam splices into the marker.
    ///
    /// Empty when no auction has been collected, which is the same payload the inline
    /// seam falls back to.
    pub(crate) fn bids(&self) -> serde_json::Map<String, serde_json::Value> {
        self.bids.lock().expect("should lock bid map").clone()
    }

    /// Build the shared-template seam, retaining the same debug prefix as inline.
    fn build_seam_script(&self, slots_json: &str) -> String {
        let seam = build_seam_script(slots_json, &self.bids());
        let prefix = self
            .debug_prefix
            .lock()
            .expect("should lock bid debug prefix");
        if prefix.is_empty() {
            seam
        } else {
            format!("{prefix}\n{seam}")
        }
    }

    /// Put `comment` immediately before the rendered bids script.
    ///
    /// Debug-only, and deliberately confined to the script: the structured map is what
    /// the shared seam serializes, so prefixing an HTML comment onto it would be
    /// meaningless there.
    fn prepend_to_script(&self, comment: &str) {
        let mut state = self
            .script
            .lock()
            .expect("should lock bid script for debug");
        match &mut *state {
            Some(script) => {
                *script = format!("{comment}\n{script}");
            }
            None => {
                // invariant: write_bids_to_state is always called before this and
                // always sets Some(_); this branch is unreachable in production.
                *state = Some(comment.to_string());
            }
        }
        let mut prefix = self
            .debug_prefix
            .lock()
            .expect("should lock bid debug prefix");
        if prefix.is_empty() {
            *prefix = comment.to_string();
        } else {
            *prefix = format!("{comment}\n{prefix}");
        }
    }
}

pub(crate) fn write_bids_to_state(
    winning_bids: &std::collections::HashMap<String, Bid>,
    price_granularity: PriceGranularity,
    ad_bids_state: &AdBidsState,
    settings: &Settings,
    request_origin: &str,
    include_debug_bid: bool,
    auction_id: Option<&str>,
) -> std::collections::HashSet<String> {
    log::debug!(
        "write_bids_to_state: {} winning bid(s): [{}]",
        winning_bids.len(),
        winning_bids.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    let bid_map = build_bid_map_with_auction_id(
        winning_bids,
        price_granularity,
        settings,
        request_origin,
        include_debug_bid,
        auction_id,
    );
    let delivered_winner_slots = bid_map.keys().cloned().collect();
    ad_bids_state.set(bid_map);
    delivered_winner_slots
}

/// Maximum serialized size (in bytes) of a dump embedded in the `ts-debug`
/// comment. A PBS response with many bids can carry megabytes of creative
/// markup; cap it so leaving
/// [`auction_html_comment`](crate::settings::DebugConfig::auction_html_comment)
/// enabled cannot bloat every page render without bound.
const MAX_AUCTION_DEBUG_DUMP_BYTES: usize = 256 * 1024;

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

/// Return a recognized server-owned provider error classification.
///
/// Validates against [`ERROR_TYPE_ALL`] rather than a local literal list so a
/// classification added in the orchestrator cannot drift out of the dump.
fn validated_error_type(
    metadata: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<&str> {
    let value = metadata.get("error_type")?.as_str()?;
    ERROR_TYPE_ALL.contains(&value).then_some(value)
}

/// Return a valid HTTP response status from provider metadata.
fn validated_http_status(
    metadata: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<u64> {
    metadata
        .get("http_status")?
        .as_u64()
        .filter(|status| (100..=599).contains(status))
}

/// Generate public diagnostic wording without copying provider-controlled text.
///
/// Every [`ERROR_TYPE_ALL`] entry must map to wording here; the
/// `redacted_metadata_covers_every_orchestrator_error_type` test fails when a
/// new orchestrator classification is added without one.
fn safe_error_message(error_type: &str, http_status: Option<u64>) -> Option<String> {
    match error_type {
        ERROR_TYPE_PARSE_RESPONSE => Some("Provider response could not be parsed".to_string()),
        ERROR_TYPE_LAUNCH_FAILED => Some("Provider launch failed".to_string()),
        ERROR_TYPE_TRANSPORT => Some("Provider request failed".to_string()),
        ERROR_TYPE_TIMEOUT => Some("Provider request timed out".to_string()),
        ERROR_TYPE_HTTP_STATUS => Some(http_status.map_or_else(
            || "Provider returned an HTTP error".to_string(),
            |status| format!("Provider returned HTTP {status}"),
        )),
        _ => None,
    }
}

/// Reconstruct the configured response metadata from validated values.
fn redacted_metadata_for_dump(
    metadata: &std::collections::HashMap<String, serde_json::Value>,
    options: &AuctionDebugCommentOptions,
) -> serde_json::Map<String, serde_json::Value> {
    let selected = |key: &str| {
        AUCTION_DEBUG_METADATA_ALLOWLIST.contains(&key)
            && options
                .metadata_keys
                .iter()
                .any(|candidate| candidate == key)
    };
    let error_type = validated_error_type(metadata);
    let http_status = validated_http_status(metadata);
    let mut safe = serde_json::Map::new();

    if selected("error_type")
        && let Some(value) = error_type
    {
        safe.insert("error_type".to_string(), serde_json::json!(value));
    }
    if selected("http_status")
        && let Some(value) = http_status
    {
        safe.insert("http_status".to_string(), serde_json::json!(value));
    }
    if selected("message")
        && let Some(value) = error_type.and_then(|kind| safe_error_message(kind, http_status))
    {
        safe.insert("message".to_string(), serde_json::json!(value));
    }

    safe
}

/// Build a JSON view of a single provider response for the `ts-debug` dump.
///
/// `Redacted` reconstructs only schema-validated response metadata, `Upstream`
/// adds six named provider diagnostics, and `Full` copies every metadata value.
fn redact_response_for_dump(
    response: &crate::auction::types::AuctionResponse,
    options: &AuctionDebugCommentOptions,
) -> serde_json::Value {
    let metadata: serde_json::Map<String, serde_json::Value> = match options.verbosity {
        AuctionDebugCommentVerbosity::Redacted => {
            redacted_metadata_for_dump(&response.metadata, options)
        }
        AuctionDebugCommentVerbosity::Upstream => {
            let mut metadata = redacted_metadata_for_dump(&response.metadata, options);
            for key in AUCTION_DEBUG_UPSTREAM_METADATA_KEYS {
                if let Some(value) = response.metadata.get(*key) {
                    metadata.insert((*key).to_string(), value.clone());
                }
            }
            metadata
        }
        AuctionDebugCommentVerbosity::Full => response
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    };
    let bids: Vec<serde_json::Value> = if options.include_bids {
        response
            .bids
            .iter()
            .map(|bid| redact_bid_for_dump(bid, options))
            .collect()
    } else {
        Vec::new()
    };
    serde_json::json!({
        "provider": response.provider,
        "status": response.status,
        "response_time_ms": response.response_time_ms,
        "bids": bids,
        "metadata": metadata,
    })
}

/// Build a JSON view of a single bid. `Redacted` and `Upstream` preview the
/// creative to [`MAX_BID_CREATIVE_DUMP_BYTES`]; `Full` passes it through.
fn redact_bid_for_dump(
    bid: &crate::auction::types::Bid,
    options: &AuctionDebugCommentOptions,
) -> serde_json::Value {
    let mut value = serde_json::to_value(bid).unwrap_or(serde_json::Value::Null);
    if options.verbosity != AuctionDebugCommentVerbosity::Full
        && let Some(creative) = &bid.creative
    {
        value["creative"] =
            serde_json::Value::String(truncate_with_marker(creative, MAX_BID_CREATIVE_DUMP_BYTES));
    }
    value
}

/// Prepend a `<!-- ts-debug: ... -->` HTML comment carrying a view of the
/// auction result — pipeline stats plus, per provider, its status, bids, and
/// metadata, shaped by `options` — onto the shared `ad_bids_state` so it
/// lands directly before the injected bids `<script>`. In
/// [`AuctionDebugCommentVerbosity::Redacted`] (the default), response metadata
/// is reconstructed from validated server-owned fields; provider diagnostics
/// and prebid's `debug` subtree are dropped. Bid-level fields and bounded
/// creative previews remain visible, so this is not a fully anonymized dump.
/// Gated by
/// [`auction_html_comment`](crate::settings::DebugConfig::auction_html_comment);
/// never enable in production.
///
/// `path_label` differentiates the streaming-with-auction-hold path (`stream`)
/// from the buffered path (`buffered`) in the marker so on-page debugging can
/// tell which code path produced the bids.
pub(crate) fn prepend_auction_debug_comment(
    path_label: &str,
    result: &crate::auction::orchestrator::OrchestrationResult,
    ad_bids_state: &AdBidsState,
    options: &AuctionDebugCommentOptions,
) {
    let ssp_count = result.provider_responses.len();
    let mediator_info = match &result.mediator_response {
        Some(r) => format!("ok({}_bids)", r.bids.len()),
        None => "none".to_string(),
    };
    // Bounded, deterministic dump so an operator can see each provider's
    // status, bids, and mode-appropriate metadata without needing log access.
    //
    // SECURITY: `Bid.creative` and provider metadata are attacker/partner-
    // influenced. Two layers protect the DOM:
    //   1. In Redacted mode, `redact_response_for_dump` reconstructs only
    //      schema-validated response metadata and previews each creative, so
    //      untyped provider diagnostics cannot cross that boundary and one
    //      large creative cannot dominate the payload. Bid-level fields
    //      (`Bid.metadata`, `nurl`, `burl`) are NOT yet allowlisted; they pass
    //      through today because the only writer (`integrations/aps.rs`) emits
    //      opaque targeting keys. Tightening this to a fail-closed bid allowlist
    //      is tracked in #925.
    //   2. `render_dump` below neutralises HTML comment terminators and caps the
    //      total serialized size.
    //
    // `serde_json::Map` (no `preserve_order` feature) is `BTreeMap`-backed, so
    // the rendered metadata keys are sorted — the dump is deterministic even
    // though `AuctionResponse.metadata` is a `HashMap`.
    let mut dump = serde_json::Map::new();
    if options.include_provider_responses {
        dump.insert(
            "provider_responses".to_string(),
            serde_json::Value::Array(
                result
                    .provider_responses
                    .iter()
                    .map(|r| redact_response_for_dump(r, options))
                    .collect(),
            ),
        );
    }
    // Only include the mediator response when one actually ran; otherwise the
    // `mediator=none` on the summary line already conveys it.
    if options.include_mediator_response
        && let Some(mediator_response) = &result.mediator_response
    {
        dump.insert(
            "mediator_response".to_string(),
            redact_response_for_dump(mediator_response, options),
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
    let dump = serde_json::Value::Object(dump);
    let serialized = match options.format {
        AuctionDebugCommentFormat::Compact => serde_json::to_string(&dump),
        AuctionDebugCommentFormat::Pretty => serde_json::to_string_pretty(&dump),
    };
    let dump =
        render_dump(serialized.unwrap_or_else(|error| format!("<dump serialize error: {error}>")));
    let debug_comment = format!(
        "<!-- ts-debug: path={path_label} ssp={ssp_count} mediator={mediator_info} winning={} time={}ms\n\
         dump={dump}\n\
         -->",
        result.winning_bids.len(),
        result.total_time_ms,
    );
    ad_bids_state.prepend_to_script(&debug_comment);
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

/// Bundles the auction-collection state passed through the streaming helpers.
struct AuctionCollectCtx<'a> {
    dispatched: DispatchedAuction,
    telemetry: AuctionTelemetryCarry,
    deps: AuctionCollectDeps<'a>,
}

/// Borrowed dependencies of the auction collect step.
///
/// Split from the per-auction state above because `dispatched` and `telemetry`
/// are moved out at collect time while these stay live for the rest of the
/// streaming loop.
struct AuctionCollectDeps<'a> {
    price_granularity: PriceGranularity,
    ad_bids_state: &'a AdBidsState,
    orchestrator: &'a AuctionOrchestrator,
    services: &'a RuntimeServices,
    settings: &'a Settings,
    /// Trusted request origin (`scheme://host`) for absolute inline creative URLs.
    request_origin: String,
}

/// Run the close-body hold loop for HTML bodies, collecting the auction before
/// the raw `</body` tail is processed so `lol_html` sees live bids.
async fn stream_html_with_auction_hold<W: Write, P: StreamProcessor>(
    body: EdgeBody,
    output: &mut W,
    processor: &mut P,
    input_compression: Compression,
    output_compression: Compression,
    ctx: AuctionCollectCtx<'_>,
) -> Result<(), Report<TrustedServerError>> {
    if body.is_stream() {
        let max_body_bytes = ctx.deps.settings.publisher.max_buffered_body_bytes;
        return body_close_hold_loop_stream(
            body,
            output,
            processor,
            input_compression,
            output_compression,
            ctx,
            max_body_bytes,
        )
        .await;
    }

    // Bound the gzip decode budget to the same ceiling the buffered writer
    // enforces, matching the streaming arm above and the no-hold buffered path.
    let max_body_bytes = ctx.deps.settings.publisher.max_buffered_body_bytes;
    let body = body_as_reader(body)?;
    if output_compression == Compression::None {
        return match input_compression {
            Compression::None => body_close_hold_loop(body, output, processor, ctx).await,
            Compression::Gzip => {
                let decoder = GzipDecodeReader::new(body, max_body_bytes);
                body_close_hold_loop(decoder, output, processor, ctx).await
            }
            Compression::Deflate => {
                let decoder = ZlibDecoder::new(body);
                body_close_hold_loop(decoder, output, processor, ctx).await
            }
            Compression::Brotli => {
                let decoder = Decompressor::new(body, STREAM_CHUNK_SIZE);
                body_close_hold_loop(decoder, output, processor, ctx).await
            }
        };
    }

    debug_assert_eq!(input_compression, output_compression);
    match input_compression {
        Compression::None => body_close_hold_loop(body, output, processor, ctx).await,
        Compression::Gzip => {
            // `GzipDecodeReader` decodes concatenated gzip members (RFC 1952)
            // and bounds decoded output, unlike `flate2::read::GzDecoder`, which
            // silently drops every member after the first — dropping trailing
            // markup (potentially including `</body>`) on buffered adapters.
            let decoder = GzipDecodeReader::new(body, max_body_bytes);
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
/// Shares [`hold_step_next_chunk`] and the finish stages with the
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
    input_compression: Compression,
    output_compression: Compression,
    ctx: AuctionCollectCtx<'_>,
    max_body_bytes: usize,
) -> Result<(), Report<TrustedServerError>> {
    let AuctionCollectCtx {
        dispatched,
        telemetry,
        deps: collect_refs,
    } = ctx;
    let mut decoder = BodyStreamDecoder::new(input_compression, max_body_bytes);
    let mut encoder = BodyStreamEncoder::new(output_compression);
    let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE).with_max_bytes(max_body_bytes);
    let mut state = AuctionHoldState::new(DispatchedAuctionGuard::new(dispatched), telemetry);

    while let Some(step) = hold_step_next_chunk(
        &mut source,
        &mut decoder,
        &mut encoder,
        processor,
        &mut state,
        &collect_refs,
    )
    .await?
    {
        // Write the ready prefix before collecting the auction, matching the
        // lazy Fastly stream: only the held `</body>` tail waits on collection.
        for encoded in step.ready {
            write_encoded_segment(writer, &encoded)?;
        }
        if step.close_found {
            for encoded in
                hold_collect_close_tail(processor, &mut encoder, &mut state, &collect_refs).await?
            {
                write_encoded_segment(writer, &encoded)?;
            }
        }
    }

    // Write the decoder-finalized prefix before collection, matching the lazy
    // Fastly stream: only the held `</body>` tail waits on the auction.
    for encoded in hold_finish_ready_segments(
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
    for encoded in
        hold_finish_tail_segments(processor, &mut encoder, &mut state, &collect_refs).await?
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
        deps,
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
                    collect_stream_auction(dispatched, telemetry.take(), &deps).await;

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
                                deps.services,
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
                        collect_stream_auction(dispatched, telemetry.take(), &deps).await;

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
                        deps.services,
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
    let auction_id = telemetry
        .auction_request
        .as_ref()
        .and_then(|_| diagnostics_auction_id(settings));
    let placeholder = mediator_placeholder_request();
    let result = orchestrator
        .collect_dispatched_auction(
            dispatched,
            services,
            &make_collect_context(settings, services, &placeholder),
        )
        .await;
    let delivered_winner_slots = write_bids_to_state(
        &result.winning_bids,
        params.price_granularity,
        &params.ad_bids_state,
        settings,
        &request_origin(&params.request_scheme, &params.request_host),
        settings.debug.inject_adm_for_testing,
        auction_id.as_deref(),
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

// Private orchestration helper called only from `body_close_hold_loop`.
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
        orchestrator,
        services,
        settings,
        request_origin,
    } = deps;
    let auction_id = telemetry
        .auction_request
        .as_ref()
        .and_then(|_| diagnostics_auction_id(settings));
    log::info!("body_close_hold_loop: collecting dispatched auction before held body tail");
    let placeholder = mediator_placeholder_request();
    let collect_ctx = make_collect_context(settings, services, &placeholder);
    let result = orchestrator
        .collect_dispatched_auction(dispatched, services, &collect_ctx)
        .await;
    log::info!(
        "body_close_hold_loop: collect complete - {} winning bid(s)",
        result.winning_bids.len()
    );
    let delivered_winner_slots = write_bids_to_state(
        &result.winning_bids,
        *price_granularity,
        ad_bids_state,
        settings,
        request_origin,
        settings.debug.inject_adm_for_testing,
        auction_id.as_deref(),
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
        prepend_auction_debug_comment(
            "stream",
            &result,
            ad_bids_state,
            &settings.debug.auction_html_comment_options,
        );
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
    edge_header: EdgeCacheHeader,
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

    // Adapter fallbacks prepare this before EC/cookie handling. Keep this
    // idempotent call as a direct-handler safety net and for focused tests.
    let gpt_diagnostics =
        crate::integrations::gpt_diagnostics::prepare_request(settings, &mut req)?;

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

    let request_path = req.uri().path().to_string();
    let is_get = req.method() == http::Method::GET;

    let is_prefetch = is_prefetch_request(&req);
    let is_bot = is_bot_user_agent(&req);

    let creative_opportunities = settings.creative_opportunities.as_ref();
    let ad_templates_enabled = creative_opportunities.is_some_and(|co_config| co_config.enabled);
    let ad_templates_disabled = creative_opportunities.is_some_and(|co_config| !co_config.enabled);
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

    let should_run_ad_stack = evaluate_ad_stack_gate(AdStackGateInput {
        method_get: is_get,
        navigation: is_navigation,
        prefetch: is_prefetch,
        bot: is_bot,
        matched_slots: !matched_slots.is_empty(),
        consent_allows_auction: Some(consent_allows_auction),
        auction_enabled: auction.orchestrator.is_enabled(),
    })
    .expected
        == RuntimeAdStackExpected::Yes;
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

    let ad_bids_state = AdBidsState::default();

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
    let assembly_mode = configured_assembly_mode(settings);

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
                request_path: &request_path,
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

    // Recorded before the request is consumed by the origin send: the C2 gate
    // below needs it, and an authorized response must never become a shared
    // template.
    let request_had_authorization = req.headers().contains_key(header::AUTHORIZATION);
    let request_had_cookie = req.headers().contains_key(header::COOKIE);
    // Whether carrying a cookie is itself disqualifying. Computed once and used for both
    // the lookup and the store, so the two cannot drift apart.
    //
    // The conservative default disqualifies every cookie-bearing request, which is very
    // nearly a disable switch — TS sets its own identity cookie, so essentially every
    // repeat visitor carries one. An operator who knows their origin ignores cookies can
    // say so; the `Vary: Cookie` drift guard still refuses the response if the origin
    // ever contradicts them.
    let cookie_disqualifies = request_had_cookie
        && !settings
            .creative_opportunities
            .as_ref()
            .is_some_and(CreativeOpportunitiesConfig::origin_is_cookie_independent);
    let suppress_datadome_client_side_tag = req
        .extensions()
        .get::<crate::integrations::datadome::DataDomeClientTagSuppressed>()
        .is_some();
    // Tag suppression is request-scoped (for example, an IP exclusion), while a C2
    // template is shared across readers. A shared template can represent neither the
    // suppressed nor unsuppressed variant safely for the other population.
    let datadome_suppression_requires_origin = suppress_datadome_client_side_tag;
    let datadome_suppression_requires_full_body =
        suppress_datadome_client_side_tag && is_html_document_request(&req);
    let request_requires_origin = request_bypasses_c2(req.headers())
        || gpt_diagnostics.requires_private_no_store()
        || datadome_suppression_requires_origin;
    let reader_compression = negotiate_reader_compression(req.headers());
    let reader_supports_assembly = reader_compression.is_ok();
    // A failed negotiation bypasses C2 below, so this value is used only on an
    // admitted path. Keeping an identity fallback avoids making that relationship
    // a panic-prone invariant in the public request handler.
    let reader_compression = reader_compression.unwrap_or(Compression::None);

    if should_run_ad_stack || datadome_suppression_requires_full_body {
        // HTML document contexts whose output may be synthesized must not
        // receive a cached 304 or partial 206. Non-document subresources contain
        // no executable injected tag, so retain their validators and ranges.
        strip_conditional_and_range_headers(&mut req);
    }

    let method_is_cacheable = req.method() == Method::GET;
    let request_can_use_shared_template = method_is_cacheable
        && matches!(assembly_mode, AssemblyMode::Esi)
        && !request_host.is_empty()
        && !request_had_authorization
        && !cookie_disqualifies
        && !request_requires_origin
        && reader_supports_assembly;

    // Only advertise encodings the rewrite pipeline can decode and re-encode. The
    // template is normalized to identity after the origin responds, then encoded for
    // this reader after assembly; the origin offer itself remains reader-compatible so
    // a response-gate bypass can still fall back to inline losslessly.
    if !matches!(assembly_mode, AssemblyMode::Esi) || reader_supports_assembly {
        restrict_accept_encoding(&mut req);
    } else {
        log::debug!("c2_template_cache bypass: reader accepts no representation TS can assemble");
    }
    // Strip the internal `fastly-ssl` scheme signal before forwarding to the
    // origin. On the EdgeZero path the entry point re-injects this header from
    // trusted Fastly TLS metadata so in-process scheme detection works; the
    // legacy path never sets it. Either way it is an internal edge signal that
    // must not leak to publisher backends.
    req.headers_mut().remove("fastly-ssl");
    // The C2 key is built here, before the request is consumed, because every field
    // is request-derived and this is the last point where the request is in hand.
    //
    // Building it pre-fetch is what makes a lookup possible at all: a key that needed
    // the origin's response could only ever authorize a store, never satisfy a read.
    //
    // Configured Vary headers are read exactly as forwarded: absence, empty fields,
    // repeated values and non-UTF8 bytes remain distinct.
    // GET only, and this is the single point that enforces it: the key governs both the
    // lookup and the store, so a `None` here excludes non-GET from each.
    //
    // Without it, `handle_publisher_request` — the `*`-method fallback route — answers a
    // POST to a path whose GET is cached with the cached page, and the origin never sees
    // the mutating request. No error, no log, the action silently swallowed. A POST is
    // not entitled to a GET's representation.
    if !method_is_cacheable && !matches!(assembly_mode, AssemblyMode::Inline) {
        log::debug!(
            "c2_template_cache bypass: method {} is not eligible for a shared template",
            req.method()
        );
    }
    if request_requires_origin && matches!(assembly_mode, AssemblyMode::Esi) {
        log::debug!(
            "c2_template_cache bypass: request cache semantics or diagnostics require origin"
        );
    }
    let template_cache_key =
        request_can_use_shared_template.then(|| crate::platform::TemplateCacheKey {
            url: target_uri.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            origin_identity: format!("{}\0{}", settings.publisher.origin_url, origin_host_header),
            assembly_mode,
            vary_values: settings
                .creative_opportunities
                .as_ref()
                .map(CreativeOpportunitiesConfig::template_cache_vary)
                .unwrap_or_else(|| VarySpec::new([]))
                .values_from(req.headers()),
            template_fingerprint: template_fingerprint(settings),
            schema_version: crate::platform::TEMPLATE_SCHEMA_VERSION,
        });
    let mut c2_response_state =
        matches!(assembly_mode, AssemblyMode::Esi).then_some(C2ResponseState::BypassRequest);
    *req.uri_mut() = target_uri;
    req.headers_mut().insert(
        header::HOST,
        HeaderValue::from_str(&origin_host_header).change_context(TrustedServerError::Proxy {
            message: "invalid publisher origin host header".to_string(),
        })?,
    );

    // The slots the head seam deliberately withheld, routed to the `</body>` seam so they
    // travel per request instead of into a template shared between readers.
    let seam_ad_slots = seam_ad_slots_json(
        assembly_mode,
        should_run_ad_stack,
        settings,
        &matched_slots,
        &request_path,
    );

    // C2 lookup, before the origin fetch — the whole point is to skip it.
    //
    // The gate that authorized the store was response-derived, so it cannot re-run
    // here and does not need to: a template in the cache already passed it. What must
    // re-run are the *request*-derived disqualifications, because they are properties
    // of this request rather than of the stored bytes. An authenticated request must
    // not be served a shared template even if that template is perfectly cacheable.
    let mut template_cache_reservation = None;
    if let Some(key) = template_cache_key.as_ref() {
        match services.template_cache().lookup_or_reserve(key).await {
            Ok(crate::platform::TemplateCacheLookup::Hit(entry)) => {
                log::debug!("c2_template_cache hit: {} bytes", entry.body.len());

                // The **strict** check — exactly one marker — and it runs here, before a
                // single response header is constructed.
                //
                // This used to test only that a marker existed somewhere, and left the
                // exactly-one check to `split_template_at_seam` inside the finalizer.
                // By then a 200 with its headers had already been committed and, on the
                // streaming adapter, the document head was already on the wire; the
                // failure could only truncate the response mid-body. Failing here falls
                // back to the origin instead, which is a slower correct page.
                //
                // `schema_version` should make either failure unreachable, so reaching
                // it means the transform changed without the version moving.
                //
                // Asked of the mode, not of every template. The key covers
                // `assembly_mode`, so a hit was stored by this same mode.
                let seam_check = mode_emits_seam_marker(assembly_mode)
                    .then(|| split_template_at_seam(&entry.body).err())
                    .flatten();
                if let Some(err) = seam_check {
                    log::error!(
                        "c2_template_cache hit is unusable ({err}); treating as a miss. \
                         The transform changed without TEMPLATE_SCHEMA_VERSION moving."
                    );
                    if let Err(purge_err) = services.template_cache().purge_url(key).await {
                        log::warn!(
                            "c2_template_cache could not purge unusable URL variants: {purge_err}"
                        );
                    }
                    c2_response_state = Some(C2ResponseState::Invalid);
                } else {
                    // Deliberately *not* assembled here. The auction is still in flight,
                    // and awaiting it now would hold the first byte until it resolves —
                    // measured at ~100x worse TTFB than doing nothing. The finalizer
                    // streams the template up to the seam, waits there, and writes the
                    // bids into the gap.
                    //
                    // Headers are constructed rather than replayed, so no origin header
                    // can reach a second reader through the cache.
                    let response = build_cached_template_response(&entry, reader_compression)?;
                    let mut params = build_template_assembly_params(
                        &entry,
                        settings,
                        request_host,
                        request_scheme,
                        price_granularity,
                        ad_bids_state.clone(),
                    );
                    params.seam_ad_slots = seam_ad_slots.clone();
                    params.dispatched_auction = dispatched_auction.take();
                    params.auction_observation = auction_observation.take();
                    params.auction_request = auction_request_for_telemetry.clone();
                    return Ok(PublisherResponse::AssembleTemplate {
                        response,
                        template: entry.body,
                        params: Box::new(params),
                    });
                }
            }
            Ok(crate::platform::TemplateCacheLookup::Reserved(reservation)) => {
                log::debug!("c2_template_cache cold miss: insert reservation acquired");
                template_cache_reservation = Some(reservation);
                c2_response_state = Some(C2ResponseState::MissReserved);
            }
            Ok(crate::platform::TemplateCacheLookup::Unsupported) => {
                log::debug!("c2_template_cache bypass: platform has no shared cache");
                c2_response_state = Some(C2ResponseState::Unsupported);
            }
            Ok(crate::platform::TemplateCacheLookup::Invalid(miss)) => {
                log::warn!(
                    "c2_template_cache invalid entry: {miss}; purging and falling back inline"
                );
                if let Err(purge_err) = services.template_cache().purge_url(key).await {
                    log::warn!(
                        "c2_template_cache could not purge invalid URL variants: {purge_err}"
                    );
                }
                c2_response_state = Some(C2ResponseState::Invalid);
            }
            Err(err) => {
                log::warn!("c2_template_cache backend failure: {err}; falling back inline");
                c2_response_state = Some(C2ResponseState::BackendError);
            }
        }
    }

    // SSP requests are already racing through the platform HTTP client, so
    // origin TTFB tracks origin latency rather than the auction timeout.
    //
    // Streaming is gated on the capability (unlike the asset-proxy path, which
    // sets the flag unconditionally and tolerates buffered fallback): adapters
    // without streaming support may reject the flag outright rather than
    // silently buffering, which would fail every publisher fetch.
    let request_method = req.method().clone();
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

        let response = Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(header::CACHE_CONTROL, "private, no-store")
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(EdgeBody::from(
                "Publisher origin returned an invalid conditional response",
            ))
            .change_context(TrustedServerError::Proxy {
                message: "failed to build unexpected origin 304 response".to_string(),
            })?;
        return Ok(PublisherResponse::Buffered(response));
    }

    crate::integrations::gpt_diagnostics::finalize_response(&gpt_diagnostics, &mut response);

    let c2_cache_policy = C2CachePolicy::from_settings(settings);
    let gate_content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut template_cache_key = template_cache_reservation.and_then(|reservation| {
        match c2_cache_ttl(
            assembly_mode,
            request_had_authorization,
            cookie_disqualifies,
            response.status(),
            &gate_content_type,
            response.headers(),
            &c2_cache_policy,
        ) {
            Err(reason) => {
                log::debug!("c2_template_cache bypass: {reason}");
                None
            }
            Ok(ttl) => {
                log::debug!("c2_template_cache eligible for {}s", ttl.as_secs());
                let Some(expires_at) = Instant::now().checked_add(ttl) else {
                    log::warn!("c2_template_cache bypass: origin freshness cannot be represented");
                    return None;
                };
                Some(AuthorizedTemplateStore {
                    reservation,
                    expires_at,
                })
            }
        }
    });
    let policy_headers = if template_cache_key.is_some() {
        match replayable_policy_headers(response.headers()) {
            Ok(headers) => headers,
            Err(reason) => {
                // The eligibility gate checks this same input immediately above. Keep
                // the second read fail-closed in case future code mutates the response
                // between authorization and metadata capture.
                log::warn!(
                    "c2_template_cache bypass: policy metadata changed after validation ({reason})"
                );
                template_cache_key = None;
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    if c2_response_state == Some(C2ResponseState::MissReserved) && template_cache_key.is_none() {
        c2_response_state = Some(C2ResponseState::BypassResponse);
    }
    if let Some(state) = c2_response_state {
        set_c2_response_state(&mut response, state);
    }

    // Both seams resolve the mode the same way, from the gate's verdict rather than
    // from configuration. Deciding them independently is what produced a document with
    // unresolved executable ESI markup and no bids: the body seam saw `Esi` and emitted
    // a marker, the head seam emitted no `adSlots`, and nothing assembled either.
    let assembly_mode = effective_assembly_mode(settings, template_cache_key.is_some());

    let ad_slots_script = template_ad_slots_script(
        assembly_mode,
        should_run_ad_stack,
        settings,
        &matched_slots,
        &request_path,
    );

    // §4.7: HTML with synthesized per-navigation auction state must not be
    // stored or validated as an origin representation. Strip both browser and
    // surrogate validators/cache directives before returning it.
    //
    // The shared-template cache gate: it does not build the key, it authorizes storing
    // the one built pre-fetch. Everything it checks is response-derived, which is
    // exactly why it cannot run at lookup time — and why it does not need to. Anything
    // already in the cache passed this gate on the way in.
    //
    // A surviving key is the store authorization. Under `Inline` there is no key to
    // survive.
    //
    // **Evaluated before TS stamps its own `private, no-store` below, and that ordering
    // is load-bearing.** The gate asks whether the *origin* declared the response
    // shareable. Run it after the stamp and it reads TS's own header instead, concludes
    // `OriginNotShareable`, and refuses to cache — on every page where the ad stack
    // runs, which is every page that matters. Local testing caught exactly that; no unit
    // test did, because their fixtures leave the auction disabled and never reach the
    // stamp.
    // Gate on `should_run_ad_stack` rather than content-type alone: when no slot
    // matched, the feature is disabled, or this is not an ad-eligible navigation,
    // no per-user `tsjs.adSlots`/`tsjs.bids` are injected. Applies regardless of
    // the auction *outcome* (empty bids still inject per-user slot state). The
    // separate EC-cookie cache net in the adapter's `finalize_response` keeps
    // first-visit identity responses private.
    let origin_content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    // `template_cache_key` is `Some` only for a response the gate authorized, which is
    // exactly a response that will be assembled. Those must be private regardless of
    // `should_run_ad_stack`: a bot, prefetch, kill-switched or consent-denied request can
    // assemble an empty-bids document and would otherwise keep the origin's public
    // caching directives, letting a downstream cache serve it to a later eligible reader.
    let assembled_response_must_be_private = template_cache_key.is_some();
    if is_html_content_type(origin_content_type) {
        if should_run_ad_stack || assembled_response_must_be_private {
            enforce_synthesized_html_cache_privacy(&mut response);
        } else {
            // Server-side ad templates are inactive for this response, so the
            // document carries no per-navigation auction state and stays
            // cacheable — unless the origin already marked it private.
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
        .trim()
        .to_ascii_lowercase();
    let route = classify_response_route(status, &content_type, &content_encoding, request_host);
    if template_cache_key.is_some() {
        set_response_compression(&mut response, reader_compression);
    }

    apply_publisher_asset_cache_policy(
        settings,
        &request_path,
        &request_method,
        edge_header,
        &mut response,
    )?;

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
                    template_cache_key,
                    seam_ad_slots,
                    policy_headers,
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
        request_info.scheme, publisher_domain, slots_ctx.request_path
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

/// Mint the browser-visible auction correlation token for GPT diagnostics.
///
/// The token is freshly generated per auction and carries no user identity.
/// [`AuctionRequest::id`] must never be used here: for a consented visitor it is
/// `ts-{ec_id}`, so publishing it in `window.tsjs.bids` would hand the `HttpOnly`
/// EC identifier to any script on the page, and — being stable per visitor — it
/// could not distinguish one auction from the next either.
///
/// Returns `None` unless the GPT diagnostics integration is enabled, since
/// nothing else consumes the value.
fn diagnostics_auction_id(settings: &Settings) -> Option<String> {
    crate::integrations::gpt_diagnostics::is_enabled(settings)
        .then(|| format!("ts-auc-{}", uuid::Uuid::new_v4().simple()))
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

/// Maximum length Google Ad Manager accepts for a key-value targeting value.
/// Longer values are rejected by GAM, so the key never reaches the creative and
/// the `hb_adid` render handshake cannot complete.
const GAM_TARGETING_VALUE_MAX_LEN: usize = 40;

/// Treat an empty identifier as absent.
///
/// `Option::or` considers `Some("")` present, which would let a blank
/// `cacheId`/`adid` win the `hb_adid` precedence below and emit an empty
/// targeting value — falsey on the page, so GPT skips the key and the render
/// bridge has nothing to match, exactly the failure the fallback chain closes.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// Build a price-bucketed bid map from winning bids.
///
/// Returns a JSON object map of slot ID → bid metadata including the bucketed
/// CPM (`hb_pb`), bidder (`hb_bidder`), and optional ad ID, nurl, and burl.
#[cfg(test)]
pub(crate) fn build_bid_map(
    winning_bids: &std::collections::HashMap<String, Bid>,
    granularity: crate::price_bucket::PriceGranularity,
    settings: &Settings,
    request_origin: &str,
    include_debug_bid: bool,
) -> serde_json::Map<String, serde_json::Value> {
    build_bid_map_with_auction_id(
        winning_bids,
        granularity,
        settings,
        request_origin,
        include_debug_bid,
        None,
    )
}

pub(crate) fn build_bid_map_with_auction_id(
    winning_bids: &std::collections::HashMap<String, Bid>,
    granularity: crate::price_bucket::PriceGranularity,
    settings: &Settings,
    request_origin: &str,
    include_debug_bid: bool,
    auction_id: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    // Inline creatives render in a foreign origin (PUC's srcdoc under GAM), so
    // their proxy/click URLs must be absolute against the origin the visitor is
    // actually on — scheme, host, and port. Fall back to the configured publisher
    // domain only when the request origin is unknown (e.g. an empty host on a
    // non-navigation path), where no inline render is expected anyway.
    let base_origin = if request_origin.is_empty() {
        format!("https://{}", settings.publisher.domain)
    } else {
        request_origin.to_owned()
    };
    winning_bids
        .iter()
        .filter_map(|(slot_id, bid)| {
            bid.price.and_then(|cpm| {
                let bucket = price_bucket(cpm, granularity);
                let mut obj = serde_json::Map::new();
                obj.insert("hb_pb".to_string(), serde_json::Value::String(bucket));
                obj.insert(
                    "hb_bidder".to_string(),
                    serde_json::Value::String(bid.bidder.clone()),
                );
                if let Some(auction_id) = auction_id {
                    obj.insert(
                        "hb_auction_id".to_string(),
                        serde_json::Value::String(auction_id.to_owned()),
                    );
                }
                // Winning creative dimensions — the bridge sizes the inline
                // render from these, falling back to the first configured slot
                // format only when absent, which mis-sizes a multi-size slot.
                // Omit a zero dimension (missing OpenRTB w/h parse to 0) so the
                // bridge falls back rather than sizing the frame to 0.
                if bid.width > 0 {
                    obj.insert("w".to_string(), serde_json::Value::from(bid.width));
                }
                if bid.height > 0 {
                    obj.insert("h".to_string(), serde_json::Value::from(bid.height));
                }
                // hb_adid: use the PBS Cache UUID when present — the Prebid
                // Universal Creative uses this as the cache lookup key. Fall back
                // to the selected typed-renderer bid ID, then `adid`, then the
                // OpenRTB bid ID. The latter is the last resort: it is unique per
                // bid instance rather than a creative, but GAM echoes it verbatim
                // so the render bridge can find the exact winning bid.
                let renderer_bid_id = bid.renderer.as_ref().and_then(|renderer| {
                    renderer
                        .as_aps()
                        .map(|renderer| renderer.bid_id.as_str())
                });
                let hb_adid = non_empty(bid.cache_id.as_deref())
                    .or_else(|| renderer_bid_id.and_then(|id| non_empty(Some(id))))
                    .or_else(|| non_empty(bid.ad_id.as_deref()))
                    .or_else(|| non_empty(bid.bid_id.as_deref()));
                if let Some(id) = hb_adid {
                    // GAM drops an over-long targeting value, so the creative
                    // echoes nothing and the bridge's equality check never
                    // matches. Log rather than truncate: a truncated ID is no
                    // longer unique per bid.
                    if id.len() > GAM_TARGETING_VALUE_MAX_LEN {
                        log::warn!(
                            "hb_adid for slot '{slot_id}' is {} characters, over GAM's \
                             {GAM_TARGETING_VALUE_MAX_LEN}-character targeting value limit — \
                             GAM may drop the key and the creative will not render",
                            id.len()
                        );
                    }
                    obj.insert(
                        "hb_adid".to_string(),
                        serde_json::Value::String(id.to_string()),
                    );
                }

                // Win/billing notification URLs, fired verbatim by the bridge.
                // Per OpenRTB these are the canonical carriers of
                // `${AUCTION_PRICE}`, so expand it from the same winning CPM used
                // for the creative below — an unexpanded macro would report an
                // unresolved clearing price to the SSP, and some reject such
                // notifications outright.
                if let Some(ref nurl) = bid.nurl {
                    let nurl = crate::creative::expand_auction_price_macro(nurl, cpm);
                    obj.insert("nurl".to_string(), serde_json::Value::String(nurl));
                }
                if let Some(ref burl) = bid.burl {
                    let burl = crate::creative::expand_auction_price_macro(burl, cpm);
                    obj.insert("burl".to_string(), serde_json::Value::String(burl));
                }
                // Always include the winning creative so the pbRender bridge can
                // render it locally when GAM serves the Prebid Universal Creative
                // — no PBS Cache round trip.
                //
                // Optionally sanitize dangerous markup, then optionally rewrite
                // URLs to first-party proxies — the same opt-in creative-processing
                // policy as the `/auction` path (see `auction::formats`), except
                // for the inline render context. This `adm` is rendered by the
                // Prebid Universal Creative inside GAM's iframe (`f.srcdoc = d.ad`),
                // a foreign origin where root-relative `/first-party/…` URLs resolve
                // against GAM and 404. The inline rewriter therefore emits
                // absolute first-party URLs and omits the tsjs bundle injection.
                let has_renderer = if let Some(ref renderer) = bid.renderer {
                    let renderer = match serde_json::to_value(renderer) {
                        Ok(renderer) => renderer,
                        Err(error) => {
                            log::warn!(
                                "Skipping winning bid for slot '{}' because its typed renderer could not be serialized: {error}",
                                slot_id
                            );
                            return None;
                        }
                    };
                    obj.insert("renderer".to_string(), renderer);
                    true
                } else {
                    false
                };
                // Every `Some(raw)` — including an explicit empty string, which
                // PBS can return — counts as a supplied creative and goes
                // through processing, so an empty `adm` cannot masquerade as
                // "absent" and re-enable the raw cache fallback below.
                // Processing may reject the creative outright (empty output):
                // sanitization can strip everything, parsing can fail, or the
                // size cap can trip.
                if let Some(ref raw_creative) = bid.creative {
                    // Resolve ${AUCTION_PRICE} from the exact winning CPM BEFORE
                    // sanitizing, rewriting, and signing — URL rewriting would
                    // otherwise encode the literal macro into the signed proxy/click
                    // URL, and signing would lock that wrong value.
                    let priced = crate::creative::expand_auction_price_macro(raw_creative, cpm);
                    let adm = crate::creative::process_inline_auction_creative(
                        settings,
                        &base_origin,
                        &priced,
                    );
                    if adm.trim().is_empty() {
                        // Rejected. The PBS Cache coordinates are deliberately
                        // NOT emitted as a fallback: the Universal Creative
                        // fetches the cached bid's ORIGINAL adm, which would
                        // hand the client an unprocessed copy of the very
                        // markup processing just refused.
                        if !has_renderer {
                            log::warn!(
                                "Skipping winning bid for slot '{}' because creative processing rejected its only render source",
                                slot_id
                            );
                            return None;
                        }
                        log::warn!(
                            "Creative for slot '{}' from '{}' rejected by processing; rendering through its typed renderer without a cache fallback",
                            slot_id,
                            bid.bidder
                        );
                    } else {
                        obj.insert("adm".to_string(), serde_json::Value::String(adm));
                    }
                } else {
                    // No creative of the bid's own, so the cache is the sole
                    // render source and its coordinates are safe to emit. The
                    // Prebid Universal Creative constructs:
                    //   https://<hb_cache_host><hb_cache_path>?uuid=<hb_adid>
                    //
                    // Gated on a non-blank `cache_id`: PBS reports the cache
                    // `url` and `cacheId` independently, and hb_adid falls back
                    // to a non-cache identifier (`adid`, then the bid id).
                    // Emitting the coordinates without a cache UUID would point
                    // the Universal Creative at `?uuid=<non-cache-id>` — a
                    // guaranteed cache miss. The gate matches the `non_empty`
                    // chain used for hb_adid above so a blank `cacheId` cannot
                    // pass here while losing the hb_adid precedence.
                    if non_empty(bid.cache_id.as_deref()).is_some() {
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
                    }
                }
                // Verbose per-bid debug blob only under the testing flag; also
                // doubles as the client-side gate for the direct GAM-replace path.
                // Deliberately mirrors the bidder-supplied `creative`/`nurl`/`burl`
                // verbatim, macros unexpanded: this blob is diagnostic — nothing
                // renders or fires from it — and showing what the bidder actually
                // sent is the point.
                if include_debug_bid {
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
                Some((slot_id.clone(), serde_json::Value::Object(obj)))
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
    // adInit() defines GPT slots on the publisher's `-container` wrappers, which
    // mutates those ad-slot subtrees. Calling it synchronously here (this script
    // runs at body-parse time) lands those mutations inside React's hydration
    // window and trips a #418 hydration mismatch. The deferral — gate on window
    // `load`, then a double `requestAnimationFrame`, pinned to navigation
    // generation 0 so a faster SPA navigation cancels it — lives in the GPT
    // bundle module as `tsjs.scheduleInitialAdInit`
    // (crates/trusted-server-js/lib/src/integrations/gpt/index.ts), where the
    // lifecycle is executable under Vitest (schedule_initial_ad_init.test.ts)
    // and the navigation-generation guard is shared with the SPA auction hook;
    // gpt_bootstrap.js installs a minimal head-injected fallback so a failed
    // bundle load still initializes initial ads.
    //
    // The deferral is deliberately unconditional — every publisher, every
    // page — even though only hydrating React publishers exhibit the #418
    // failure. Uniform behavior keeps one code path to reason about and
    // avoids a framework-detection or config surface that must be kept
    // truthful per publisher; the cost is that non-React pages also move the
    // initial request from parse time to window load. The agreed follow-up
    // (branch 958-adinit-hydration-chunk-gate, spec in docs/superpowers/
    // specs/2026-07-24-adinit-hydration-gate-design.md) narrows the gate to
    // the Next.js hydration chunks with `load` as the can't-hang fallback,
    // which recovers most of that latency without a new config surface.
    //
    // The bids payload is handed to the scheduler instead of being assigned
    // here: an SPA navigation that committed while this document was still
    // streaming has already replaced `tsjs.bids`, and an unconditional
    // assignment would clobber the live route's bids with the stale SSR
    // payload. Only when no scheduler exists at all (GPT integration active
    // without its head bootstrap — not an expected deployment) does the script
    // fall back to a plain assignment, where no SPA hook exists to race with.
    format!(
        "<script>(function(){{\
var t=window.tsjs=window.tsjs||{{}};\
var b=JSON.parse(\"{}\");\
var s=t.scheduleInitialAdInit;\
if(typeof s===\"function\")s(b);\
else t.bids=b;\
}})();</script>",
        escaped
    )
}

/// Build the `</body>` seam script for a shared-template mode.
///
/// Carries **both** request-scoped pieces: the slot definitions and the bids. Under a
/// shared mode the head seam emits no `tsjs.adSlots`, because slot *presence* is
/// request-gated and baking it into a template shared between readers would decide for
/// all of them. Sending only bids — which is what this did — left `tsjs.adSlots` at its
/// `[]` default, so `adInit` defined no slots and the page rendered correctly with **no
/// TS ads at all**. A review caught it; nothing here or in the harness would have.
///
/// Both are handed to the **scheduler**, not assigned here. `scheduleInitialAdInit`
/// drops the whole payload once a SPA navigation has committed, and slots assigned on
/// the line before the call would already have overwritten that navigation's slots by
/// the time the guard ran — the guard covered the bids and `adInit`, and the assignment
/// it was there to protect happened in front of it. The `else` arm keeps the direct
/// assignment for the one case with no scheduler to race against: the GPT integration
/// active without its head bootstrap, which is not an expected deployment.
///
/// Slots are applied before bids inside the scheduler, because the scheduler may fire
/// `adInit` and `adInit` reads `ts.adSlots`.
pub(crate) fn build_seam_script(
    slots_json: &str,
    bid_map: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let bids = serde_json::to_string(bid_map)
        .expect("serde_json::to_string of Map<String,Value> should be infallible");
    format!(
        "<script>(function(){{\
var t=window.tsjs=window.tsjs||{{}};\
var a=JSON.parse(\"{}\");\
var b=JSON.parse(\"{}\");\
var s=t.scheduleInitialAdInit;\
if(typeof s===\"function\")s(b,a);\
else{{t.adSlots=a;t.bids=b;}}\
}})();</script>",
        html_escape_for_script(slots_json),
        html_escape_for_script(&bids)
    )
}

/// The slot definitions a shared-mode seam must carry, as JSON.
///
/// Mirrors [`template_ad_slots_script`]'s gating: same `should_run_ad_stack` condition,
/// same slot set. The difference is only *where* it is delivered — the seam, per
/// request, rather than the head, into a shared template.
pub(crate) fn seam_ad_slots_json(
    mode: AssemblyMode,
    should_run_ad_stack: bool,
    settings: &Settings,
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    request_path: &str,
) -> Option<String> {
    if matches!(mode, AssemblyMode::Inline) || !should_run_ad_stack {
        return None;
    }
    let co_config = settings.creative_opportunities.as_ref()?;
    let section = co_config.section_for_path(request_path);
    let slots: Vec<serde_json::Value> = matched_slots
        .iter()
        .filter_map(|slot| build_slot_json(slot, co_config, &section))
        .collect();
    Some(
        serde_json::to_string(&slots)
            .expect("serde_json::to_string of Vec<Value> should be infallible"),
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
/// `formats`, and `targeting`. Returns `None` when the slot's dynamic GAM unit
/// path exceeds its rendering limit.
pub(crate) fn build_slot_json(
    slot: &crate::creative_opportunities::CreativeOpportunitySlot,
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
    section: &str,
) -> Option<serde_json::Value> {
    let gam_path = slot.render_gam_unit_path(&co_config.gam_network_id, section)?;
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
    Some(serde_json::json!({
        "id": slot.id,
        "gam_unit_path": gam_path,
        "div_id": div_id,
        "formats": formats,
        "targeting": targeting,
    }))
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

/// Why a response must not enter the shared transformed-template cache (C2).
///
/// `cache::core` is not an HTTP cache: it stores whatever bytes it is handed and
/// rejects nothing on its own. Every safety condition is the caller's to enforce,
/// so they are enumerated here rather than left implicit.
///
/// Spike-only, for the #1009 ESI validation.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
pub(crate) enum C2BypassReason {
    /// Not a shared-template mode; there is no C2 object to write.
    #[display("assembly mode is inline")]
    InlineMode,
    /// The origin set a cookie. Caching this would replay one visitor's cookie
    /// to the next — and the cookie-privacy net downgrades *our* response, which
    /// happens after the cache has already stored the origin's.
    #[display("origin response carries Set-Cookie")]
    OriginSetCookie,
    /// The origin declared the response non-shareable, or supplied a CDN-specific
    /// policy C2 does not interpret and therefore cannot safely override.
    #[display("origin cache policy is not eligible for C2 sharing")]
    OriginNotShareable,
    /// Cache directives or HTTP dates were malformed. Failing closed prevents a
    /// parser disagreement from extending a representation's lifetime.
    #[display("origin cache policy is malformed")]
    MalformedCachePolicy,
    /// Core Cache has no HTTP heuristic freshness. C2 therefore requires an explicit,
    /// still-positive origin lifetime rather than inventing one.
    #[display("origin response has no positive shared freshness")]
    NoPositiveFreshness,
    /// The request was authenticated. #1009 describes a Basic-Auth-gated
    /// deployment, so an authorized response entering a shared cache is a live
    /// concern rather than a hypothetical one.
    #[display("request carried Authorization")]
    AuthorizedRequest,
    /// Not a 200. This is also what covers a `DataDome` block, which replaces the
    /// document with a `403` (`integrations/datadome/protection.rs:778`).
    #[display("status was not 200 OK")]
    NonOkStatus,
    /// Not HTML, so there is no template to transform.
    #[display("content type is not text/html")]
    NotHtml,
    /// A single-valued representation header was absent, repeated or malformed.
    #[display("origin representation headers are ambiguous or malformed")]
    MalformedRepresentationHeaders,
    /// The body cannot be decoded by the transform. Authorizing it would rewrite
    /// `Content-Encoding` while returning the untouched bytes on the fallback route.
    #[display("origin content encoding is not supported by the template transform")]
    UnsupportedContentEncoding,
    /// The request carried a `Cookie`, which TS forwards to origin unchanged — there
    /// is no `Cookie` strip on the publisher path. Cookie-personalized HTML is
    /// therefore cross-servable unless the origin declares `Vary: Cookie` or marks
    /// those responses private, and a response can be personalized without carrying
    /// `Set-Cookie` itself when the session was established earlier. Named in §4 of
    /// the design doc; disqualifying until the origin's `Vary` is verified to cover
    /// it.
    #[display("request carried Cookie and the origin's Vary does not cover it")]
    CookieForwarded,
    /// The origin varies on a header the cache key does not cover.
    ///
    /// The key is built *before* the fetch from a configured [`VarySpec`], because a
    /// lookup cannot know what the origin varies on until it has responded. That makes
    /// the configured list capable of going stale. This is the guard: once the origin's
    /// `Vary` is finally known, a template whose key missed one of its headers must not
    /// be stored, because a request differing only in that header would read it.
    ///
    /// Carries the uncovered header names rather than a bare flag, so a stale config is
    /// identifiable from the log line instead of requiring a bisect.
    #[display("origin varies on {_0}, which the cache key does not cover")]
    VaryNotCovered(VaryGap),
    /// `Vary: *` — the origin says no request key can select this representation.
    ///
    /// `VarySpec::uncovered_by` filters the wildcard out on the grounds that "the
    /// eligibility gate handles it". It did not: nothing rejected it, so a `Vary: *`
    /// response was shareable. A review found the gap between the comment and the code.
    #[display("origin sent Vary: *, so no request key can select this response")]
    VaryWildcard,
    /// Cookie-selected HTML contradicts the reader-neutral-template contract even if
    /// an operator accidentally lists `cookie` in the configured Vary key.
    #[display("origin sent Vary: Cookie, contradicting cookie independence")]
    VaryCookie,
    /// A response-bound CSP nonce cannot safely be replayed with a shared document.
    #[display("origin CSP contains a response-bound nonce")]
    CspNonce,
    /// A policy header selected for replay could not be represented losslessly.
    #[display("origin policy header is malformed")]
    MalformedPolicyHeader,
}

fn request_bypasses_c2(headers: &edgezero_core::http::HeaderMap) -> bool {
    const CONDITIONAL_OR_PARTIAL: &[&str] = &[
        "range",
        "if-range",
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
    ];
    if CONDITIONAL_OR_PARTIAL
        .iter()
        .any(|name| headers.contains_key(*name))
    {
        return true;
    }

    for value in headers.get_all(header::CACHE_CONTROL) {
        let Ok(value) = value.to_str() else {
            return true;
        };
        for directive in value.split(',').map(str::trim) {
            let (name, argument) = directive
                .split_once('=')
                .map_or((directive, None), |(name, argument)| (name, Some(argument)));
            match name.trim().to_ascii_lowercase().as_str() {
                "no-cache" | "no-store" => return true,
                // A browser reload's `max-age=0` requires a newly assembled response,
                // not a second origin fetch for its reader-neutral template. The hit
                // still runs this reader's auction and is stamped private/no-store.
                // Positive or malformed constraints remain unprovable because C2 does
                // not expose object age/remaining freshness at this layer.
                "max-age"
                    if argument.and_then(|argument| parse_delta_seconds(argument).ok())
                        != Some(0) =>
                {
                    return true;
                }
                "min-fresh" => return true,
                _ => {}
            }
        }
    }

    headers.get_all(header::PRAGMA).iter().any(|value| {
        value.to_str().map_or(true, |value| {
            value.split(',').any(|directive| {
                directive
                    .split_once('=')
                    .map_or(directive, |(name, _)| name)
                    .trim()
                    .eq_ignore_ascii_case("no-cache")
            })
        })
    })
}

/// The header names an origin's `Vary` named that the cache key did not cover.
///
/// A newtype rather than a bare `Vec<String>` so [`C2BypassReason`] stays `Display`-able
/// as one line, and so the empty case is unrepresentable at the call site — an empty gap
/// is not a bypass, it is a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VaryGap(Vec<String>);

impl core::fmt::Display for VaryGap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0.join(", "))
    }
}

/// Operator policy applied after the origin authorizes shared freshness.
#[derive(Debug, Clone)]
struct C2CachePolicy {
    key_vary: VarySpec,
    max_age: Duration,
}

impl C2CachePolicy {
    fn from_settings(settings: &Settings) -> Self {
        settings.creative_opportunities.as_ref().map_or_else(
            || Self {
                key_vary: VarySpec::new([]),
                max_age: Duration::from_secs(60),
            },
            |config| Self {
                key_vary: config.template_cache_vary(),
                max_age: config.template_cache_max_age(),
            },
        )
    }

    #[cfg(test)]
    fn for_test(key_vary: &VarySpec, max_age: Duration) -> Self {
        Self {
            key_vary: key_vary.clone(),
            max_age,
        }
    }
}

/// Whether a response may be written to the shared transformed-template cache.
///
/// Returns [`None`] when it is safe to cache, or the first disqualifying reason.
/// Leak vectors are checked before mere ineligibility so the reported reason is
/// the most serious one that applies.
///
/// See `docs/superpowers/specs/2026-08-08-esi-cacheable-root-validation-design.md`
/// §6.6 for why C1, C2 and a final assembled-response cache are distinct, and why
/// the third must never exist.
#[cfg(test)]
pub(crate) fn c2_bypass_reason(
    mode: AssemblyMode,
    request_had_authorization: bool,
    cookie_disqualifies: bool,
    status: StatusCode,
    content_type: &str,
    response_headers: &edgezero_core::http::HeaderMap,
    key_vary: &VarySpec,
) -> Option<C2BypassReason> {
    let policy = C2CachePolicy::for_test(key_vary, Duration::from_secs(60));
    c2_cache_ttl(
        mode,
        request_had_authorization,
        cookie_disqualifies,
        status,
        content_type,
        response_headers,
        &policy,
    )
    .err()
}

fn single_header_value(
    headers: &edgezero_core::http::HeaderMap,
    name: header::HeaderName,
) -> Result<Option<&str>, C2BypassReason> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(C2BypassReason::MalformedCachePolicy);
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| C2BypassReason::MalformedCachePolicy)
}

fn single_representation_header_value(
    headers: &edgezero_core::http::HeaderMap,
    name: header::HeaderName,
) -> Result<Option<&str>, C2BypassReason> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(C2BypassReason::MalformedRepresentationHeaders);
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| C2BypassReason::MalformedRepresentationHeaders)
}

fn parse_delta_seconds(value: &str) -> Result<u64, C2BypassReason> {
    let value = value.trim();
    let quoted_at_start = value.starts_with('"');
    let quoted_at_end = value.ends_with('"');
    let digits = match (quoted_at_start, quoted_at_end) {
        (true, true) if value.len() >= 2 => &value[1..value.len() - 1],
        (false, false) => value,
        _ => return Err(C2BypassReason::MalformedCachePolicy),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(C2BypassReason::MalformedCachePolicy);
    }
    digits
        .parse::<u64>()
        .map_err(|_| C2BypassReason::MalformedCachePolicy)
}

/// Parse the Fastly-specific freshness policy used by C2's hosting platform.
///
/// Fastly documents `max-age`, `stale-while-revalidate`, and `stale-if-error` for
/// `Surrogate-Control`. C2 uses only `max-age` as fresh lifetime; the stale windows
/// are validated so malformed policy cannot hide beside a valid max age, but Core
/// Cache assembly does not serve stale templates under either extension.
///
/// Unknown directives fail closed rather than inheriting semantics from another CDN.
fn surrogate_control_freshness(
    headers: &edgezero_core::http::HeaderMap,
) -> Result<Option<Duration>, C2BypassReason> {
    let mut saw_header = false;
    let mut max_age = None;
    let mut stale_while_revalidate = None;
    let mut stale_if_error = None;

    for value in headers.get_all("surrogate-control") {
        saw_header = true;
        let value = value
            .to_str()
            .map_err(|_| C2BypassReason::MalformedCachePolicy)?;
        if value.trim().is_empty() {
            return Err(C2BypassReason::MalformedCachePolicy);
        }
        for directive in value.split(',') {
            let directive = directive.trim();
            if directive.is_empty() {
                return Err(C2BypassReason::MalformedCachePolicy);
            }
            let (name, value) = directive
                .split_once('=')
                .map_or((directive, None), |(name, value)| (name, Some(value)));
            let name = name.trim().to_ascii_lowercase();
            match name.as_str() {
                "private" | "no-store" | "no-cache" => {
                    return Err(C2BypassReason::OriginNotShareable);
                }
                "max-age" => {
                    let parsed =
                        parse_delta_seconds(value.ok_or(C2BypassReason::MalformedCachePolicy)?)?;
                    if max_age.replace(parsed).is_some() {
                        return Err(C2BypassReason::MalformedCachePolicy);
                    }
                }
                "stale-while-revalidate" => {
                    let parsed =
                        parse_delta_seconds(value.ok_or(C2BypassReason::MalformedCachePolicy)?)?;
                    if stale_while_revalidate.replace(parsed).is_some() {
                        return Err(C2BypassReason::MalformedCachePolicy);
                    }
                }
                "stale-if-error" => {
                    let parsed =
                        parse_delta_seconds(value.ok_or(C2BypassReason::MalformedCachePolicy)?)?;
                    if stale_if_error.replace(parsed).is_some() {
                        return Err(C2BypassReason::MalformedCachePolicy);
                    }
                }
                _ => return Err(C2BypassReason::MalformedCachePolicy),
            }
        }
    }

    if !saw_header {
        return Ok(None);
    }
    let max_age = max_age.ok_or(C2BypassReason::NoPositiveFreshness)?;
    if max_age == 0 {
        return Err(C2BypassReason::NoPositiveFreshness);
    }
    Ok(Some(Duration::from_secs(max_age)))
}

fn origin_shared_ttl(
    headers: &edgezero_core::http::HeaderMap,
    max_age: Duration,
) -> Result<Duration, C2BypassReason> {
    origin_shared_ttl_at(headers, SystemTime::now(), max_age)
}

fn origin_shared_ttl_at(
    headers: &edgezero_core::http::HeaderMap,
    now: SystemTime,
    template_cache_max_age: Duration,
) -> Result<Duration, C2BypassReason> {
    let mut max_age = None;
    let mut shared_max_age = None;

    for value in headers.get_all(header::CACHE_CONTROL) {
        let value = value
            .to_str()
            .map_err(|_| C2BypassReason::MalformedCachePolicy)?;
        for directive in value.split(',').map(str::trim).filter(|v| !v.is_empty()) {
            let (name, value) = directive
                .split_once('=')
                .map_or((directive, None), |(name, value)| (name, Some(value)));
            match name.trim().to_ascii_lowercase().as_str() {
                "private" | "no-store" | "no-cache" => {
                    return Err(C2BypassReason::OriginNotShareable);
                }
                "max-age" => {
                    let parsed =
                        parse_delta_seconds(value.ok_or(C2BypassReason::MalformedCachePolicy)?)?;
                    if max_age.replace(parsed).is_some() {
                        return Err(C2BypassReason::MalformedCachePolicy);
                    }
                }
                "s-maxage" => {
                    let parsed =
                        parse_delta_seconds(value.ok_or(C2BypassReason::MalformedCachePolicy)?)?;
                    if shared_max_age.replace(parsed).is_some() {
                        return Err(C2BypassReason::MalformedCachePolicy);
                    }
                }
                _ => {}
            }
        }
    }

    let date = single_header_value(headers, header::DATE)?
        .map(httpdate::parse_http_date)
        .transpose()
        .map_err(|_| C2BypassReason::MalformedCachePolicy)?;
    let standard_freshness = match shared_max_age.or(max_age) {
        Some(seconds) => Some(Duration::from_secs(seconds)),
        None => single_header_value(headers, header::EXPIRES)?
            .map(|value| {
                let expires = httpdate::parse_http_date(value)
                    .map_err(|_| C2BypassReason::MalformedCachePolicy)?;
                expires
                    .duration_since(date.unwrap_or(now))
                    .map_err(|_| C2BypassReason::NoPositiveFreshness)
            })
            .transpose()?,
    };
    // Fastly gives Surrogate-Control precedence for its edge cache. Standard
    // restrictive directives were still parsed above and remain hard refusals.
    let freshness = surrogate_control_freshness(headers)?
        .or(standard_freshness)
        .ok_or(C2BypassReason::NoPositiveFreshness)?;

    let age = single_header_value(headers, header::AGE)?
        .map(parse_delta_seconds)
        .transpose()?
        .unwrap_or(0);
    // `Age` may be absent even when an upstream cache emitted an old `Date`.
    // RFC 9111's corrected age is at least the apparent age; ignoring it would
    // grant an already-expired representation a new C2 lifetime.
    let apparent_age = date
        .and_then(|date| now.duration_since(date).ok())
        .unwrap_or_default();
    let current_age = Duration::from_secs(age).max(apparent_age);
    let remaining = freshness
        .checked_sub(current_age)
        .filter(|duration| !duration.is_zero())
        .ok_or(C2BypassReason::NoPositiveFreshness)?;
    let capped = remaining.min(template_cache_max_age);
    if capped.is_zero() {
        return Err(C2BypassReason::NoPositiveFreshness);
    }
    Ok(capped)
}

fn replayable_policy_headers(
    headers: &edgezero_core::http::HeaderMap,
) -> Result<Vec<(String, String)>, C2BypassReason> {
    let mut captured = Vec::new();
    for name in crate::platform::REPLAYABLE_POLICY_HEADERS {
        for value in headers.get_all(*name) {
            let value = value
                .to_str()
                .map_err(|_| C2BypassReason::MalformedPolicyHeader)?;
            if (*name == "content-security-policy"
                || *name == "content-security-policy-report-only")
                && value.to_ascii_lowercase().contains("'nonce-")
            {
                return Err(C2BypassReason::CspNonce);
            }
            captured.push(((*name).to_string(), value.to_string()));
        }
    }
    Ok(captured)
}

fn c2_cache_ttl(
    mode: AssemblyMode,
    request_had_authorization: bool,
    cookie_disqualifies: bool,
    status: StatusCode,
    content_type: &str,
    response_headers: &edgezero_core::http::HeaderMap,
    policy: &C2CachePolicy,
) -> Result<Duration, C2BypassReason> {
    if matches!(mode, AssemblyMode::Inline) {
        return Err(C2BypassReason::InlineMode);
    }
    if request_had_authorization {
        return Err(C2BypassReason::AuthorizedRequest);
    }
    if cookie_disqualifies {
        return Err(C2BypassReason::CookieForwarded);
    }
    if response_headers.contains_key(header::SET_COOKIE) {
        return Err(C2BypassReason::OriginSetCookie);
    }
    // Core Cache has no HTTP semantics. Fastly's documented Surrogate-Control subset
    // is parsed by `origin_shared_ttl`; every other vendor-specific policy remains a
    // bypass rather than guessing that unrelated CDNs share its grammar or precedence.
    if crate::response_privacy::CDN_CACHE_HEADERS
        .iter()
        .filter(|name| **name != "surrogate-control")
        .any(|name| response_headers.contains_key(*name))
    {
        return Err(C2BypassReason::OriginNotShareable);
    }
    // Checked here, among the leak vectors, because storing under a key that does not
    // cover the origin's Vary is cross-serving rather than mere ineligibility: a request
    // differing only in the uncovered header would read this template.
    let mut vary_values = Vec::new();
    for value in response_headers.get_all(header::VARY) {
        let value = value
            .to_str()
            .map_err(|_| C2BypassReason::MalformedCachePolicy)?;
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if name == "*" {
                return Err(C2BypassReason::VaryWildcard);
            }
            if name.eq_ignore_ascii_case(header::COOKIE.as_str()) {
                return Err(C2BypassReason::VaryCookie);
            }
            header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| C2BypassReason::MalformedCachePolicy)?;
        }
        vary_values.push(value);
    }
    let uncovered = policy.key_vary.uncovered_by(vary_values);
    if !uncovered.is_empty() {
        return Err(C2BypassReason::VaryNotCovered(VaryGap(uncovered)));
    }
    if status != StatusCode::OK {
        return Err(C2BypassReason::NonOkStatus);
    }
    let declared_content_type =
        single_representation_header_value(response_headers, header::CONTENT_TYPE)?;
    if declared_content_type.is_some_and(|declared| declared != content_type)
        || !is_html_content_type(content_type)
    {
        return Err(C2BypassReason::NotHtml);
    }
    let content_encoding =
        single_representation_header_value(response_headers, header::CONTENT_ENCODING)?
            .map_or_else(
                || "identity".to_string(),
                |value| value.trim().to_ascii_lowercase(),
            );
    if content_encoding.is_empty() {
        return Err(C2BypassReason::MalformedRepresentationHeaders);
    }
    if !is_supported_content_encoding(&content_encoding) {
        return Err(C2BypassReason::UnsupportedContentEncoding);
    }
    replayable_policy_headers(response_headers)?;
    origin_shared_ttl(response_headers, policy.max_age)
}

/// What the `<head>` seam injects, given the assembly mode.
///
/// Under [`AssemblyMode::Inline`] the response is per-navigation and not shared,
/// so emitting `tsjs.adSlots` only when the ad stack runs is correct.
///
/// Under [`AssemblyMode::Esi`] the document is a
/// **shared template**, and `should_run_ad_stack` is request-dependent — it folds
/// in consent, bot classification, prefetch status and the auction kill switch.
/// Emitting conditionally there would freeze the first-filling request's decision
/// for every later reader of the cached object: a consent-denied fill would serve
/// a no-ads template to consenting users, and a consenting fill would serve ad
/// markup to someone who refused.
///
/// So ESI returns [`None`] **unconditionally**, and `adSlots` moves to the
/// per-request body seam alongside the bids. The head is not a template hole.
///
/// See `docs/superpowers/specs/2026-08-08-esi-cacheable-root-validation-design.md`
/// §6.7.
pub(crate) fn template_ad_slots_script(
    mode: AssemblyMode,
    should_run_ad_stack: bool,
    settings: &Settings,
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    request_path: &str,
) -> Option<String> {
    match mode {
        AssemblyMode::Esi => None,
        AssemblyMode::Inline => {
            if !should_run_ad_stack {
                return None;
            }
            settings
                .creative_opportunities
                .as_ref()
                .map(|co_config| build_ad_slots_script(matched_slots, co_config, request_path))
        }
    }
}

/// Build the `tsjs.adSlots` `<script>` tag from matched slots.
///
/// Property names match what the client-side TSJS bundle expects:
/// `gam_unit_path`, `div_id`, `formats`, and `targeting`.
pub(crate) fn build_ad_slots_script(
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
    request_path: &str,
) -> String {
    // `{section}` derives from the same raw path `page_patterns` matched
    // against; derive it once for every slot on this request.
    let section = co_config.section_for_path(request_path);
    let slots: Vec<serde_json::Value> = matched_slots
        .iter()
        .filter_map(|slot| build_slot_json(slot, co_config, &section))
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

/// Canonical URL path of the SPA re-auction endpoint.
///
/// Lives in the internal `/_ts/` namespace shared by every other Trusted
/// Server route. Adapters register this path; the tsjs SPA hook fetches it.
pub const PAGE_BIDS_PATH: &str = "/_ts/page-bids";

/// Deprecated double-underscore alias of [`PAGE_BIDS_PATH`].
///
/// The endpoint originally shipped as `/__ts/page-bids`, the only internal path
/// using a `__` prefix. Renaming it is atomic on the server, but a browser runs
/// whichever tsjs bundle it was already served: pages loaded before the rename —
/// and cached bundles — keep requesting this path, and on a SPA that path is what
/// delivers ads for in-session navigations. Adapters route it to the same handler
/// so those clients keep working.
///
/// The alias is bidirectional in practice: the current tsjs bundle requests
/// [`PAGE_BIDS_PATH`] first and falls back here when that path does not serve
/// page-bids on a deployment. That covers a server rolled back to before the
/// rename, and an operator `[[handlers]]` auth regex broad enough to cover
/// `/_ts` (which would answer the canonical path with `401`). Both are
/// transitional — an affected operator must narrow the regex before the alias
/// is removed.
///
/// Removal is tracked by IABTechLab/trusted-server#970: drop this const, its
/// four adapter registrations, and the client fallback once access logs show no
/// remaining traffic on the legacy path.
pub const PAGE_BIDS_LEGACY_PATH: &str = "/__ts/page-bids";

/// `X-TSJS-Page-Bids` value the current tsjs bundle sends when it retries
/// [`PAGE_BIDS_LEGACY_PATH`] because [`PAGE_BIDS_PATH`] was unusable.
///
/// Separates the two populations on the deprecated alias: pre-rename bundles
/// (which age out by themselves) from current bundles falling back (which do
/// not, because the cause is deployment configuration). See the logging in
/// [`handle_page_bids`].
pub const PAGE_BIDS_FALLBACK_MARKER: &str = "fallback";

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
/// this same response for `OPTIONS /_ts/page-bids` and for its deprecated
/// `/__ts/page-bids` alias.
pub fn page_bids_preflight_denied() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Forbidden"));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

/// Builds the `400 Bad Request` returned for an unrecognized `format`.
///
/// `private, no-store` like every other response from this endpoint, so an error
/// cannot be cached and replayed.
fn page_bids_unknown_format() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Unknown format"));
    *response.status_mut() = StatusCode::BAD_REQUEST;
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
/// How the page-bids endpoint serializes its answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PageBidsFormat {
    /// `application/json`. What the SPA navigation hook consumes.
    #[default]
    Json,
}

impl PageBidsFormat {
    /// Parse the `format` query parameter.
    ///
    /// # Errors
    ///
    /// Returns the offending value if it names no known format. Unknown values are
    /// rejected rather than defaulting so callers cannot silently negotiate a response
    /// representation the endpoint no longer supports.
    fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw {
            None | Some("json") => Ok(Self::Json),
            Some(other) => Err(other.to_string()),
        }
    }
}

fn normalize_page_bids_path(raw: &str) -> String {
    let path = raw.split(['?', '#']).next().unwrap_or("");
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
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

    // Deprecation signal for the transition alias. Evaluated after the
    // cross-site gate, so the count reflects genuine SPA clients still running a
    // pre-rename tsjs bundle rather than anything a third-party page can
    // inflate — and before the not-configured 404 below, so deployments with no
    // creative opportunities still report their alias traffic instead of
    // reading as zero. The log line and the response marker are the only in-app
    // signals that `PAGE_BIDS_LEGACY_PATH` is still in use — the removal
    // precondition in IABTechLab/trusted-server#970 is "no remaining traffic on
    // the legacy path", which is otherwise only answerable from edge access
    // logs.
    //
    // Two different clients land here, and they age out differently, so the log
    // separates them by the `X-TSJS-Page-Bids` value. A pre-rename bundle sends
    // `1` and disappears on its own as caches turn over. The current bundle
    // sends `fallback` and does *not* — it only reaches the alias when the
    // canonical path is unusable on this deployment (an operator `[[handlers]]`
    // regex covering `/_ts`, or a server rolled back past the rename), which
    // persists until that is fixed. Treating the two as one number would make
    // #970 wait forever on a config problem. The value is client-supplied, so
    // it is a diagnostic hint only; the gate above does not trust it.
    let is_legacy_alias = req.uri().path() == PAGE_BIDS_LEGACY_PATH;
    if is_legacy_alias {
        let is_client_fallback = req
            .headers()
            .get("x-tsjs-page-bids")
            .and_then(|value| value.to_str().ok())
            == Some(PAGE_BIDS_FALLBACK_MARKER);
        if is_client_fallback {
            log::warn!(
                "page-bids: served deprecated alias {PAGE_BIDS_LEGACY_PATH} to a current \
                 tsjs bundle that could not use {PAGE_BIDS_PATH} — check `[[handlers]]` for \
                 a pattern covering `/_ts`; see IABTechLab/trusted-server#970"
            );
        } else {
            log::info!(
                "page-bids: served deprecated alias {PAGE_BIDS_LEGACY_PATH} \
                 (pre-rename tsjs bundle); see IABTechLab/trusted-server#970"
            );
        }
    }

    let Some(co_config) = &settings.creative_opportunities else {
        let mut response = Response::new(EdgeBody::from("Creative opportunities not configured"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        mark_deprecated_alias(&mut response, is_legacy_alias);
        return Ok(response);
    };

    // Trusted request origin for absolute inline creative URLs — derived from the
    // origin the visitor is actually on (scheme, host, port), not the configured
    // publisher domain, which cannot carry a port and may differ by subdomain.
    let request_info = RequestInfo::from_request(&req, services.client_info());
    let page_bids_request_origin = request_origin(&request_info.scheme, &request_info.host);

    let path_param = req
        .uri()
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(k, _)| k == "path")
                .map(|(_, v)| normalize_page_bids_path(&v))
        })
        .unwrap_or_else(|| "/".to_string());

    let format = match PageBidsFormat::parse(
        req.uri()
            .query()
            .and_then(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .find(|(k, _)| k == "format")
                    .map(|(_, v)| v.into_owned())
            })
            .as_deref(),
    ) {
        Ok(format) => format,
        Err(unknown) => {
            log::warn!("page-bids: rejecting unknown format `{unknown}`");
            return Ok(page_bids_unknown_format());
        }
    };

    let matched_slots = match_renderable_slots(auction.slots, co_config, &path_param);

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

    // The dedicated template switch, [auction].enabled, and a consent denial
    // disable the entire server-side ad stack. In those states the endpoint must
    // return no slots, so the SPA hook does not assign `ts.adSlots` and call
    // `adInit()` — otherwise the gate would stop SSP calls but still let the
    // client create/refresh GPT slots client-side. Bot/prefetch requests, by
    // contrast, keep their slot definitions (the placement structure is
    // unchanged) but skip the live auction, matching the existing behavior.
    let ad_stack_enabled = ad_templates_enabled && auction_enabled && consent_allows_auction;

    let (winning_bids, prebuilt_bid_map) = if matched_slots.is_empty() {
        (std::collections::HashMap::new(), None)
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
                request_path: &path_param,
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
                    let winning_bids = result.winning_bids.clone();
                    let auction_id = diagnostics_auction_id(settings);
                    let bid_map = build_bid_map_with_auction_id(
                        &winning_bids,
                        co_config.price_granularity,
                        settings,
                        &page_bids_request_origin,
                        settings.debug.inject_adm_for_testing,
                        auction_id.as_deref(),
                    );
                    let delivered_winner_slots = bid_map.keys().cloned().collect();
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
                    (winning_bids, Some(bid_map))
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
                    (std::collections::HashMap::new(), None)
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
            (std::collections::HashMap::new(), None)
        }
    };

    let bid_map = prebuilt_bid_map.unwrap_or_else(|| {
        build_bid_map_with_auction_id(
            &winning_bids,
            co_config.price_granularity,
            settings,
            &page_bids_request_origin,
            settings.debug.inject_adm_for_testing,
            None,
        )
    });

    // Gate slots on the ad-stack kill switch / consent: when disabled, return no
    // slots so the SPA hook does not call `adInit()` / create GPT slots.
    let slots_json: Vec<serde_json::Value> = if ad_stack_enabled {
        let section = co_config.section_for_path(&path_param);
        matched_slots
            .iter()
            .filter_map(|slot| build_slot_json(slot, co_config, &section))
            .collect()
    } else {
        Vec::new()
    };

    debug_assert_eq!(format, PageBidsFormat::Json);
    let body = serde_json::json!({
        "slots": slots_json,
        "bids": bid_map,
    });
    let body = serde_json::to_string(&body).change_context(TrustedServerError::Proxy {
        message: "Failed to serialize page-bids response".to_string(),
    })?;

    let mut response = Response::new(EdgeBody::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    mark_deprecated_alias(&mut response, is_legacy_alias);

    Ok(response)
}

/// Marks a response served through [`PAGE_BIDS_LEGACY_PATH`] as deprecated.
///
/// Attaches the RFC 9745 `deprecation` link relation pointing at the removal
/// issue, so CDN and edge log pipelines can measure remaining legacy traffic
/// from any vantage point — the removal precondition in
/// IABTechLab/trusted-server#970 is otherwise answerable only from application
/// logs. RFC 9745's companion `Deprecation` field is deliberately omitted: it
/// carries a date, and there is no source of truth here for when the alias was
/// deprecated.
///
/// No-op for the canonical path.
fn mark_deprecated_alias(response: &mut Response<EdgeBody>, is_legacy_alias: bool) {
    if !is_legacy_alias {
        return;
    }

    response.headers_mut().insert(
        header::LINK,
        HeaderValue::from_static(
            "<https://github.com/IABTechLab/trusted-server/issues/970>; rel=\"deprecation\"",
        ),
    );
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
    use crate::auction::orchestrator::OrchestrationResult;
    use crate::auction::types::AuctionResponse;
    use crate::auction::types::{AdFormat, AdSlot, MediaType};
    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::{
        NoopSecretStore, StubHttpClient, build_services_with_http_client,
        build_services_with_secret_http_client_and_client_ip, noop_services,
        noop_services_with_telemetry_sink,
    };
    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};
    use edgezero_core::body::Body as EdgeBody;
    use http::{Method, Request as HttpRequest, StatusCode, header};
    use std::sync::Arc;

    fn make_test_bid_with_creative(creative: &str) -> Bid {
        Bid {
            slot_id: "slot".to_string(),
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

    /// Build the ts-debug comment for a one-bid auction whose creative is
    /// `creative`, so tests can assert on the rendered dump.
    fn dump_comment_for_creative_with_options(
        creative: &str,
        options: &AuctionDebugCommentOptions,
    ) -> String {
        let mut bid = make_test_bid_with_creative(creative);
        bid.slot_id = "ad-header-0".to_string();
        let result = OrchestrationResult {
            provider_responses: vec![
                AuctionResponse::no_bid("prebid", 665),
                AuctionResponse::success("aps", vec![bid], 42),
            ],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            total_time_ms: 665,
            metadata: std::collections::HashMap::new(),
        };
        let state = AdBidsState::with_script("BIDS_SCRIPT");
        prepend_auction_debug_comment("stream", &result, &state, options);
        let comment = state
            .script_cell()
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");
        drop(state);
        comment
    }

    fn dump_comment_for_creative(creative: &str) -> String {
        dump_comment_for_creative_with_options(creative, &AuctionDebugCommentOptions::default())
    }

    fn dump_comment_for_metadata_with_options(
        metadata: std::collections::HashMap<String, serde_json::Value>,
        options: &AuctionDebugCommentOptions,
    ) -> String {
        let mut response = AuctionResponse::error("prebid", 12);
        response.metadata = metadata;
        let result = OrchestrationResult {
            provider_responses: vec![response],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            total_time_ms: 12,
            metadata: std::collections::HashMap::new(),
        };
        let state = AdBidsState::with_script("BIDS_SCRIPT");
        prepend_auction_debug_comment("stream", &result, &state, options);
        let comment = state
            .script_cell()
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");
        drop(state);
        comment
    }

    fn dump_from_comment(comment: &str) -> (&str, serde_json::Value) {
        let (_, after_dump) = comment
            .split_once("dump=")
            .expect("should contain dump marker");
        let (dump, _) = after_dump
            .rsplit_once("\n-->")
            .expect("should contain comment terminator");
        let value: serde_json::Value =
            serde_json::from_str(dump).expect("should contain valid untruncated JSON");
        (dump, value)
    }

    fn response_metadata_from_comment(comment: &str) -> serde_json::Value {
        let (_, dump) = dump_from_comment(comment);
        dump["provider_responses"][0]["metadata"].clone()
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
    fn auction_debug_comment_reaches_the_shared_template_seam() {
        let result = OrchestrationResult {
            provider_responses: vec![AuctionResponse::no_bid("prebid", 12)],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            total_time_ms: 12,
            metadata: std::collections::HashMap::new(),
        };
        let state = AdBidsState::with_script("BIDS_SCRIPT");
        prepend_auction_debug_comment(
            "stream",
            &result,
            &state,
            &AuctionDebugCommentOptions::default(),
        );

        let seam = state.build_seam_script("[]");

        assert!(
            seam.contains("<!-- ts-debug:"),
            "debug state must reach ESI"
        );
        assert!(
            seam.find("<!-- ts-debug:") < seam.find("<script>"),
            "the diagnostic comment should precede the executable seam: {seam}"
        );
    }

    #[test]
    fn auction_debug_comment_pretty_formats_outer_json_without_changing_value() {
        let compact_comment = dump_comment_for_creative("<div>plain</div>");
        let pretty_options = AuctionDebugCommentOptions {
            format: AuctionDebugCommentFormat::Pretty,
            ..AuctionDebugCommentOptions::default()
        };
        let pretty_comment =
            dump_comment_for_creative_with_options("<div>plain</div>", &pretty_options);

        assert!(
            compact_comment.contains("dump={\"provider_responses\":"),
            "default output should remain compact: {compact_comment}"
        );
        assert!(
            pretty_comment.contains("dump={\n  \"provider_responses\":"),
            "pretty output should indent the outer dump: {pretty_comment}"
        );

        let (_, compact_dump) = dump_from_comment(&compact_comment);
        let (_, pretty_dump) = dump_from_comment(&pretty_comment);
        assert_eq!(
            pretty_dump, compact_dump,
            "formatting should not change the dump value"
        );
    }

    #[test]
    fn auction_debug_comment_pretty_preserves_nested_json_as_string() {
        let metadata = std::collections::HashMap::from([(
            "debug".to_string(),
            serde_json::json!({
                "httpcalls": {
                    "openx": [{ "requestbody": "{\"id\":\"request-1\"}" }]
                }
            }),
        )]);
        let options = AuctionDebugCommentOptions {
            verbosity: AuctionDebugCommentVerbosity::Full,
            format: AuctionDebugCommentFormat::Pretty,
            ..AuctionDebugCommentOptions::default()
        };

        let comment = dump_comment_for_metadata_with_options(metadata, &options);
        let response_metadata = response_metadata_from_comment(&comment);
        let request_body = &response_metadata["debug"]["httpcalls"]["openx"][0]["requestbody"];
        assert_eq!(request_body, "{\"id\":\"request-1\"}");
        assert!(
            request_body.is_string(),
            "nested request body should remain a string"
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
            total_time_ms: 12,
            metadata: std::collections::HashMap::new(),
        };
        let state = AdBidsState::with_script("BIDS_SCRIPT");
        prepend_auction_debug_comment(
            "stream",
            &result,
            &state,
            &AuctionDebugCommentOptions::default(),
        );
        let comment = state
            .script_cell()
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
    fn default_options_apply_safe_response_metadata_schema() {
        let metadata = std::collections::HashMap::from([
            ("error_type".to_string(), serde_json::json!("http_status")),
            ("http_status".to_string(), serde_json::json!(422)),
            (
                "message".to_string(),
                serde_json::json!("raw-message-example-user-123"),
            ),
            (
                "errors".to_string(),
                serde_json::json!(["errors-example-user-123"]),
            ),
            (
                "warnings".to_string(),
                serde_json::json!(["warnings-example-user-123"]),
            ),
            (
                "responsetimemillis".to_string(),
                serde_json::json!({"timing-example-user-123": 12}),
            ),
            (
                "bidstatus".to_string(),
                serde_json::json!([{"bidder": "bidstatus-example-user-123"}]),
            ),
            (
                "upstream_message".to_string(),
                serde_json::json!("upstream-example-user-123"),
            ),
            (
                "upstream_message_truncated".to_string(),
                serde_json::json!("truncated-example-user-123"),
            ),
            (
                "debug".to_string(),
                serde_json::json!({"resolvedrequest": {"user": {"id": "debug-example-user-123"}}}),
            ),
        ]);

        let comment = dump_comment_for_metadata_with_options(
            metadata,
            &AuctionDebugCommentOptions::default(),
        );

        assert_eq!(
            response_metadata_from_comment(&comment),
            serde_json::json!({
                "error_type": "http_status",
                "http_status": 422,
                "message": "Provider returned HTTP 422",
            })
        );
    }

    #[test]
    fn configured_metadata_subset_only_includes_selected_safe_keys() {
        let metadata = std::collections::HashMap::from([
            ("error_type".to_string(), serde_json::json!("http_status")),
            ("http_status".to_string(), serde_json::json!(418)),
            (
                "errors".to_string(),
                serde_json::json!(["errors-example-user-123"]),
            ),
            (
                "debug".to_string(),
                serde_json::json!({"identity": "debug-example-user-123"}),
            ),
        ]);
        let options = AuctionDebugCommentOptions {
            metadata_keys: vec![
                "http_status".to_string(),
                "errors".to_string(),
                "debug".to_string(),
            ],
            ..AuctionDebugCommentOptions::default()
        };

        let comment = dump_comment_for_metadata_with_options(metadata, &options);

        assert_eq!(
            response_metadata_from_comment(&comment),
            serde_json::json!({"http_status": 418})
        );
    }

    #[test]
    fn redacted_mode_rejects_wrong_types_and_unknown_error_classifications() {
        let invalid_cases = [
            std::collections::HashMap::from([(
                "error_type".to_string(),
                serde_json::json!({"identity": "example-user-123"}),
            )]),
            std::collections::HashMap::from([
                (
                    "error_type".to_string(),
                    serde_json::json!("provider_supplied_unknown"),
                ),
                ("message".to_string(), serde_json::json!("example-user-123")),
            ]),
            std::collections::HashMap::from([(
                "http_status".to_string(),
                serde_json::json!("200 example-user-123"),
            )]),
            std::collections::HashMap::from([("http_status".to_string(), serde_json::json!(99))]),
            std::collections::HashMap::from([("http_status".to_string(), serde_json::json!(600))]),
            std::collections::HashMap::from([(
                "http_status".to_string(),
                serde_json::json!(200.5),
            )]),
            std::collections::HashMap::from([(
                "message".to_string(),
                serde_json::json!({"identity": "example-user-123"}),
            )]),
        ];
        for metadata in invalid_cases {
            let comment = dump_comment_for_metadata_with_options(
                metadata,
                &AuctionDebugCommentOptions::default(),
            );
            assert_eq!(
                response_metadata_from_comment(&comment),
                serde_json::json!({})
            );
            assert!(!comment.contains("example-user-123"));
            assert!(!comment.contains("provider_supplied_unknown"));
        }

        for status in [100_u64, 599] {
            let metadata = std::collections::HashMap::from([(
                "http_status".to_string(),
                serde_json::json!(status),
            )]);
            let comment = dump_comment_for_metadata_with_options(
                metadata,
                &AuctionDebugCommentOptions::default(),
            );
            assert_eq!(
                response_metadata_from_comment(&comment),
                serde_json::json!({"http_status": status})
            );
        }
    }

    #[test]
    fn redacted_mode_generates_fixed_safe_messages() {
        let cases = [
            (
                "parse_response",
                None,
                "Provider response could not be parsed",
            ),
            ("launch_failed", None, "Provider launch failed"),
            ("transport", None, "Provider request failed"),
            ("timeout", None, "Provider request timed out"),
            ("http_status", Some(418_u64), "Provider returned HTTP 418"),
            ("http_status", None, "Provider returned an HTTP error"),
        ];
        for (error_type, status, expected) in cases {
            let mut metadata = std::collections::HashMap::from([
                ("error_type".to_string(), serde_json::json!(error_type)),
                (
                    "message".to_string(),
                    serde_json::json!("raw-example-user-123"),
                ),
            ]);
            if let Some(status) = status {
                metadata.insert("http_status".to_string(), serde_json::json!(status));
            }
            let comment = dump_comment_for_metadata_with_options(
                metadata,
                &AuctionDebugCommentOptions::default(),
            );
            let rendered = response_metadata_from_comment(&comment);
            assert_eq!(rendered["message"], serde_json::json!(expected));
            assert!(!comment.contains("raw-example-user-123"));
        }
    }

    #[test]
    fn redacted_metadata_covers_every_orchestrator_error_type() {
        // Drift guard: adding a classification to ERROR_TYPE_ALL without wiring
        // wording into safe_error_message would make it vanish from redacted
        // dumps through the catch-all match arm.
        for error_type in ERROR_TYPE_ALL {
            let metadata = std::collections::HashMap::from([(
                "error_type".to_string(),
                serde_json::json!(error_type),
            )]);
            assert_eq!(
                validated_error_type(&metadata),
                Some(*error_type),
                "{error_type} should be a recognized classification"
            );
            assert!(
                safe_error_message(error_type, None).is_some(),
                "{error_type} should map to safe diagnostic wording"
            );
        }
    }

    #[test]
    fn metadata_keys_empty_yields_empty_safe_metadata_in_redacted() {
        let options = AuctionDebugCommentOptions {
            metadata_keys: vec![],
            ..AuctionDebugCommentOptions::default()
        };
        let comment = dump_comment_for_creative_with_options("<div>x</div>", &options);
        assert!(
            comment.contains("\"metadata\":{}"),
            "empty metadata_keys should yield an empty metadata object: {comment}"
        );
    }

    #[test]
    fn metadata_keys_attack_vector_debug_key_never_surfaces_in_redacted_mode() {
        // Configuring "debug" in metadata_keys must have zero effect in Redacted
        // mode — the allowlist intersection is the actual security boundary, not
        // the config value. This is the load-bearing test for this whole design.
        let response = AuctionResponse::error("prebid", 12).with_metadata(
            "debug",
            serde_json::json!({"resolvedrequest": {"user": {"id": "EC-ID-abc123"}}}),
        );
        let result = OrchestrationResult {
            provider_responses: vec![response],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            total_time_ms: 12,
            metadata: std::collections::HashMap::new(),
        };
        let options = AuctionDebugCommentOptions {
            metadata_keys: vec!["debug".to_string()],
            ..AuctionDebugCommentOptions::default()
        };
        let state = AdBidsState::with_script("BIDS_SCRIPT");
        prepend_auction_debug_comment("stream", &result, &state, &options);
        let comment = state
            .script_cell()
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");
        assert!(
            !comment.contains("EC-ID-abc123"),
            "debug key must never surface in Redacted mode even if configured: {comment}"
        );
    }

    #[test]
    fn upstream_mode_includes_provider_diagnostics_but_not_debug_subtree() {
        let metadata = std::collections::HashMap::from([
            ("error_type".to_string(), serde_json::json!("timeout")),
            (
                "errors".to_string(),
                serde_json::json!(["errors-example-user-123"]),
            ),
            (
                "warnings".to_string(),
                serde_json::json!(["warnings-example-user-123"]),
            ),
            (
                "responsetimemillis".to_string(),
                serde_json::json!({"example-bidder": 12}),
            ),
            (
                "bidstatus".to_string(),
                serde_json::json!([{"bidder": "example-bidder", "status": "timeout"}]),
            ),
            (
                "upstream_message".to_string(),
                serde_json::json!("upstream-example-user-123"),
            ),
            (
                "upstream_message_truncated".to_string(),
                serde_json::json!(true),
            ),
            (
                "debug".to_string(),
                serde_json::json!({"resolvedrequest": {"user": {"id": "debug-example-user-123"}}}),
            ),
        ]);
        let options = AuctionDebugCommentOptions {
            verbosity: AuctionDebugCommentVerbosity::Upstream,
            metadata_keys: vec!["error_type".to_string()],
            ..AuctionDebugCommentOptions::default()
        };

        let comment = dump_comment_for_metadata_with_options(metadata, &options);
        let rendered = response_metadata_from_comment(&comment);

        for key in [
            "error_type",
            "errors",
            "warnings",
            "responsetimemillis",
            "bidstatus",
            "upstream_message",
            "upstream_message_truncated",
        ] {
            assert!(
                rendered.get(key).is_some(),
                "should include {key}: {comment}"
            );
        }
        assert!(rendered.get("debug").is_none());
        assert!(!comment.contains("debug-example-user-123"));
    }

    #[test]
    fn verbosity_upstream_still_truncates_creative() {
        let big_creative = "u".repeat(MAX_BID_CREATIVE_DUMP_BYTES * 2);
        let options = AuctionDebugCommentOptions {
            verbosity: AuctionDebugCommentVerbosity::Upstream,
            ..AuctionDebugCommentOptions::default()
        };

        let comment = dump_comment_for_creative_with_options(&big_creative, &options);

        assert!(comment.contains("(truncated"));
        assert!(!comment.contains(&big_creative));
    }

    #[test]
    fn verbosity_full_includes_raw_debug_subtree_when_present() {
        let response = AuctionResponse::error("prebid", 12).with_metadata(
            "debug",
            serde_json::json!({"httpcalls": {"aps": [{"status": 200}]}}),
        );
        let result = OrchestrationResult {
            provider_responses: vec![response],
            mediator_response: None,
            winning_bids: std::collections::HashMap::new(),
            total_time_ms: 12,
            metadata: std::collections::HashMap::new(),
        };
        let options = AuctionDebugCommentOptions {
            verbosity: AuctionDebugCommentVerbosity::Full,
            ..AuctionDebugCommentOptions::default()
        };
        let state = AdBidsState::with_script("BIDS_SCRIPT");
        prepend_auction_debug_comment("stream", &result, &state, &options);
        let comment = state
            .script_cell()
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");
        assert!(
            comment.contains("httpcalls"),
            "Full verbosity should surface the raw debug subtree: {comment}"
        );
    }

    #[test]
    fn verbosity_full_skips_creative_truncation() {
        let big_creative = "y".repeat(MAX_BID_CREATIVE_DUMP_BYTES * 2);
        let options = AuctionDebugCommentOptions {
            verbosity: AuctionDebugCommentVerbosity::Full,
            ..AuctionDebugCommentOptions::default()
        };
        let comment = dump_comment_for_creative_with_options(&big_creative, &options);
        assert!(
            comment.contains(&big_creative),
            "Full verbosity should not truncate the creative preview"
        );
    }

    #[test]
    fn verbosity_full_still_hits_overall_byte_cap() {
        let huge_creative = "z".repeat(MAX_AUCTION_DEBUG_DUMP_BYTES * 2);
        for format in [
            AuctionDebugCommentFormat::Compact,
            AuctionDebugCommentFormat::Pretty,
        ] {
            let options = AuctionDebugCommentOptions {
                verbosity: AuctionDebugCommentVerbosity::Full,
                format,
                ..AuctionDebugCommentOptions::default()
            };
            let comment = dump_comment_for_creative_with_options(&huge_creative, &options);
            assert!(
                comment.contains("(truncated"),
                "even Full verbosity must respect the total dump byte cap for {format:?}: {}",
                &comment[..comment.len().min(200)]
            );
        }
    }

    #[test]
    fn include_provider_responses_false_omits_section_entirely() {
        let options = AuctionDebugCommentOptions {
            include_provider_responses: false,
            ..AuctionDebugCommentOptions::default()
        };
        let comment = dump_comment_for_creative_with_options("<div>x</div>", &options);
        assert!(!comment.contains("provider_responses"));
    }

    #[test]
    fn include_mediator_response_false_omits_even_when_mediator_ran() {
        let response = AuctionResponse::success("aps", vec![], 10);
        let mediator = AuctionResponse::success("mediator", vec![], 5);
        let result = OrchestrationResult {
            provider_responses: vec![response],
            mediator_response: Some(mediator),
            winning_bids: std::collections::HashMap::new(),
            total_time_ms: 10,
            metadata: std::collections::HashMap::new(),
        };
        let options = AuctionDebugCommentOptions {
            include_mediator_response: false,
            ..AuctionDebugCommentOptions::default()
        };
        let state = AdBidsState::with_script("BIDS_SCRIPT");
        prepend_auction_debug_comment("stream", &result, &state, &options);
        let comment = state
            .script_cell()
            .lock()
            .expect("should lock state")
            .clone()
            .expect("should have comment");
        assert!(!comment.contains("mediator_response"));
    }

    #[test]
    fn include_bids_false_yields_empty_bids_array_not_omitted_response() {
        let options = AuctionDebugCommentOptions {
            include_bids: false,
            ..AuctionDebugCommentOptions::default()
        };
        let comment = dump_comment_for_creative_with_options("<div>x</div>", &options);
        assert!(comment.contains("\"bids\":[]"));
        // The provider entry itself (status/provider name) must still be present.
        assert!(comment.contains("\"provider\":\"aps\""));
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
        for verbosity in [
            AuctionDebugCommentVerbosity::Redacted,
            AuctionDebugCommentVerbosity::Full,
        ] {
            for format in [
                AuctionDebugCommentFormat::Compact,
                AuctionDebugCommentFormat::Pretty,
            ] {
                let options = AuctionDebugCommentOptions {
                    verbosity,
                    format,
                    ..AuctionDebugCommentOptions::default()
                };
                for creative in [
                    "<div>evil-->break</div>",
                    "--!><img src=x onerror=alert(1)>",
                    "<!--><img src=x onerror=alert(1)>",
                    "<!--!><img src=x onerror=alert(1)>",
                    "----!><img src=x onerror=alert(1)>",
                ] {
                    let comment = dump_comment_for_creative_with_options(creative, &options);
                    assert_eq!(
                        comment.matches("-->").count(),
                        1,
                        "exactly one terminator must survive for {verbosity:?}, {format:?}, {creative:?}: {comment}"
                    );
                    assert!(
                        !comment.contains("--!>"),
                        "nested terminator must not survive for {verbosity:?}, {format:?}, {creative:?}: {comment}"
                    );
                }
            }
        }
    }

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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: content_encoding.to_owned(),
            origin_host: settings.publisher.origin_host(),
            origin_url: settings.publisher.origin_url.clone(),
            request_host: settings.publisher.domain.clone(),
            request_scheme: "https".to_owned(),
            content_type: "application/json".to_owned(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: Default::default(),
            gpt_diagnostics: None,
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
    fn stream_publisher_body_injects_active_diagnostics_for_materialized_html() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
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
            html.contains("__tsjs_gpt_diagnostics_active"),
            "should inject the activation flag"
        );
        assert!(
            html.contains("tsjs-gpt_diagnostics.min.js"),
            "should inject the standalone diagnostics module"
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
            EdgeCacheHeader::SMaxageFallback,
        )
        .await
        .expect("should proxy publisher request")
    }

    mod rendered_template_identity_tests {
        //! The gate the plan's Task 3 Step 2 actually asks for.
        //!
        //! Every other test in this area exercises the decision functions with
        //! hand-built inputs. That is how three HIGH review findings sat in covered,
        //! passing code: the decisions were right and nothing checked what the
        //! composition of them *renders*.
        //!
        //! These tests render whole documents through `create_html_processor`,
        //! composing the same three decisions `create_html_stream_processor` uses,
        //! and compare bytes. A future request-dependent injection added at either
        //! seam fails here even if every decision function is left untouched.

        use super::template_neutrality_tests::{settings_with_slots, slot};
        use super::*;
        use crate::creative_opportunities::AssemblyMode;
        use crate::html_processor::{HtmlProcessorConfig, create_html_processor};
        use crate::integrations::IntegrationRegistry;
        use crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision;

        const DOCUMENT: &[u8] =
            b"<html><head><title>t</title></head><body><p>content</p></body></html>";

        /// One request's worth of variation. Everything here is request-scoped and
        /// must not reach a shared template.
        #[derive(Debug, Clone, Copy)]
        struct RequestShape {
            /// Folds in consent, bot classification, prefetch and the kill switch.
            ad_stack_ran: bool,
            /// Cookie- or query-activated.
            diagnostics_active: bool,
            /// A resolved auction, present only when one was dispatched.
            bids_available: bool,
        }

        /// Build the config exactly as `create_html_stream_processor` does, so a
        /// drift between a decision and its use is caught rather than hidden.
        fn render(mode: AssemblyMode, shape: RequestShape) -> String {
            let settings = settings_with_slots();
            let slots = [slot()];

            let ad_slots_script =
                template_ad_slots_script(mode, shape.ad_stack_ran, &settings, &slots, "/");
            let body_close = body_close_injection(mode, ad_slots_script.is_some());
            let gpt_diagnostics = template_gpt_diagnostics(
                mode,
                shape
                    .diagnostics_active
                    .then(GptDiagnosticsRequestDecision::active_for_tests),
            );

            let ad_bids_state =
                std::sync::Arc::new(std::sync::Mutex::new(shape.bids_available.then(|| {
                    r#"<script>(window.tsjs=window.tsjs||{}).bids={"atf":1};</script>"#.to_string()
                })));

            let config = HtmlProcessorConfig {
                origin_host: "origin.example.com".to_string(),
                request_host: "example.com".to_string(),
                request_scheme: "https".to_string(),
                integrations: IntegrationRegistry::empty_for_tests(),
                ad_slots_script,
                ad_bids_state,
                max_buffered_body_bytes: 16 * 1024 * 1024,
                gpt_diagnostics,
                body_close,
                suppress_datadome_client_side_tag: false,
            };

            let mut processor = create_html_processor(config);
            let out = processor
                .process_chunk(DOCUMENT, true)
                .expect("should process the document");
            String::from_utf8(out).expect("output should be utf8")
        }

        fn every_shape() -> Vec<RequestShape> {
            let mut shapes = Vec::new();
            for ad_stack_ran in [false, true] {
                for diagnostics_active in [false, true] {
                    for bids_available in [false, true] {
                        shapes.push(RequestShape {
                            ad_stack_ran,
                            diagnostics_active,
                            bids_available,
                        });
                    }
                }
            }
            shapes
        }

        #[test]
        fn shared_template_ad_seam_is_readable_and_versioned() {
            let BodyCloseInjection::Marker(marker) = body_close_injection(AssemblyMode::Esi, false)
            else {
                panic!("ESI mode should emit a shared-template marker");
            };

            assert_eq!(
                (crate::platform::TEMPLATE_SCHEMA_VERSION, marker.as_str(),),
                (4, "<!--ts-ad-seam-->"),
                "the readable seam and its cache schema must move together"
            );
        }

        #[test]
        fn shared_modes_render_byte_identical_documents_for_every_request_shape() {
            let mode = AssemblyMode::Esi;
            let shapes = every_shape();
            let baseline = render(mode, shapes[0]);

            for shape in &shapes[1..] {
                let rendered = render(mode, *shape);
                assert_eq!(
                    rendered, baseline,
                    "{mode:?}: rendered template differs for {shape:?}. A shared \
                     template that varies by request freezes the first-filling \
                     request's decision for every later reader."
                );
            }
        }

        #[test]
        fn shared_mode_templates_contain_no_request_scoped_markers() {
            // Byte-identity alone would be satisfied by rendering the same wrong
            // thing every time, so also assert the specific things that must be
            // absent.
            let mode = AssemblyMode::Esi;
            let rendered = render(
                mode,
                RequestShape {
                    ad_stack_ran: true,
                    diagnostics_active: true,
                    bids_available: true,
                },
            );
            for forbidden in [
                ".adSlots",
                ".bids=",
                "__tsjs_gpt_diagnostics_active",
                "history.replaceState",
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "{mode:?}: template contains request-scoped `{forbidden}`:\n{rendered}"
                );
            }
        }

        #[test]
        fn inline_still_varies_by_request_as_it_must() {
            // The shared-mode assertions would also pass if rendering were broken
            // everywhere. Inline responses are per-navigation and never shared, so
            // they *should* differ — this proves the test can tell the difference.
            let with_ads = render(
                AssemblyMode::Inline,
                RequestShape {
                    ad_stack_ran: true,
                    diagnostics_active: false,
                    bids_available: true,
                },
            );
            let without = render(
                AssemblyMode::Inline,
                RequestShape {
                    ad_stack_ran: false,
                    diagnostics_active: false,
                    bids_available: false,
                },
            );
            assert_ne!(
                with_ads, without,
                "inline must still vary by request; if it does not, this harness is \
                 not rendering what it claims to"
            );
            assert!(
                with_ads.contains(".adSlots"),
                "inline with a matched slot should carry adSlots"
            );
        }
    }

    mod template_fingerprint_tests {
        use super::*;

        /// Base settings with one integration's config replaced.
        ///
        /// Edits the parsed `[integrations]` map rather than appending TOML, so the two
        /// fixtures differ in exactly the field under test — the base settings already
        /// declare `[integrations.prebid]`, and a second table would not parse.
        fn settings_with_prebid(enabled: bool, timeout_ms: u32) -> Settings {
            let mut settings = create_test_settings();
            settings.integrations.insert(
                "prebid".to_string(),
                serde_json::json!({
                    "enabled": enabled,
                    "server_url": "https://prebid.example.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example.com/prebid/bundle.js",
                    "timeout": timeout_ms,
                }),
            );
            settings
        }

        #[test]
        fn disabling_an_integration_changes_the_fingerprint() {
            // The fingerprint was `concatenated_hash(all_module_ids())` — every module
            // compiled into the binary, so a constant for that binary. Turning an
            // integration off changed the injected `<script>` set and left the cache key
            // untouched, and every reader kept getting the template built while it was on.
            assert_ne!(
                template_fingerprint(&settings_with_prebid(true, 1000)),
                template_fingerprint(&settings_with_prebid(false, 1000)),
                "the enabled integration set must select a different template"
            );
        }

        #[test]
        fn reconfiguring_an_integration_changes_the_fingerprint() {
            // Config reaches the template directly: the prebid head insert carries the
            // account ID, timeout and bidder list into bytes shared between readers.
            assert_ne!(
                template_fingerprint(&settings_with_prebid(true, 1000)),
                template_fingerprint(&settings_with_prebid(true, 2500)),
                "an integration's configuration must select a different template"
            );
        }

        #[test]
        fn the_fingerprint_is_stable_across_calls_for_one_configuration() {
            // `IntegrationSettings` derefs to a `HashMap`, whose iteration order varies.
            // An unsorted digest would differ between two requests to the same binary and
            // the cache would never hit — a fix that quietly disables the feature.
            let settings = settings_with_prebid(true, 1000);
            let first = template_fingerprint(&settings);

            for _ in 0..16 {
                assert_eq!(
                    template_fingerprint(&settings),
                    first,
                    "the same configuration must always fingerprint identically"
                );
            }
            // A second, independently parsed `Settings` builds a fresh `HashMap` with a
            // different iteration order, which is what actually exercises the sort.
            assert_eq!(
                template_fingerprint(&settings_with_prebid(true, 1000)),
                first,
                "two equal configurations must fingerprint identically"
            );
        }

        #[test]
        fn creative_and_origin_configuration_change_the_fingerprint() {
            let base = super::template_neutrality_tests::settings_with_slots();

            let mut creative = base.clone();
            creative
                .creative_opportunities
                .as_mut()
                .expect("fixture should configure creative opportunities")
                .gam_network_id = "different-network".to_string();
            assert_ne!(template_fingerprint(&base), template_fingerprint(&creative));

            let mut origin = base.clone();
            origin.publisher.origin_host_header_override = Some("tenant.example.com".to_string());
            assert_ne!(template_fingerprint(&base), template_fingerprint(&origin));
        }
    }

    mod seam_script_tests {
        use super::*;

        fn seam() -> String {
            build_seam_script(
                r#"[{"id":"atf","div_id":"atf"}]"#,
                &serde_json::Map::from_iter([(
                    "atf".to_string(),
                    serde_json::json!({"hb_pb": "1.50"}),
                )]),
            )
        }

        #[test]
        fn no_slot_assignment_precedes_the_navigation_generation_guard() {
            // `scheduleInitialAdInit` drops the whole SSR payload once a navigation has
            // committed. The seam used to assign `t.adSlots` on the line *before* calling
            // it, which is outside that guard — so a committed SPA navigation kept its own
            // bids and silently lost its slots to the stale SSR document's.
            let script = seam();
            let guard = script
                .find("t.scheduleInitialAdInit")
                .expect("the seam should consult the scheduler");

            assert!(
                !script[..guard].contains("t.adSlots="),
                "no slot assignment may run ahead of the navigation-generation guard: \
                 {script}"
            );
        }

        #[test]
        fn the_slots_are_handed_to_the_scheduler_so_the_guard_can_apply_them() {
            // Removing the unguarded assignment is only half the fix: the slots still have
            // to arrive, or a shared-mode page defines no slots at all.
            let script = seam();

            assert!(
                script.contains("s(b,a)"),
                "the scheduler should receive bids and slots together: {script}"
            );
            assert!(
                script.contains("atf"),
                "the slot definitions should reach the page: {script}"
            );
        }

        #[test]
        fn a_page_with_no_scheduler_still_gets_its_slots() {
            // The GPT integration active without its head bootstrap. There is no SPA hook
            // to race with there, so the direct assignment is the correct fallback — and
            // dropping it while moving the assignment would have served no ads at all.
            let script = seam();
            let fallback = script
                .find("else")
                .expect("the seam should carry a no-scheduler fallback");

            assert!(
                script[fallback..].contains("t.adSlots=a"),
                "the fallback should still assign slots: {script}"
            );
            assert!(
                script[fallback..].contains("t.bids=b"),
                "the fallback should still assign bids: {script}"
            );
        }
    }

    mod page_bids_format_tests {
        //! Page-bids is a JSON API. The old executable fragment was part of the removed
        //! parser-based spike and must not remain as an accidental public surface.

        use super::*;

        #[test]
        fn an_absent_format_is_json_so_existing_clients_are_unaffected() {
            assert_eq!(PageBidsFormat::parse(None), Ok(PageBidsFormat::Json));
            assert_eq!(
                PageBidsFormat::parse(Some("json")),
                Ok(PageBidsFormat::Json)
            );
        }

        #[test]
        fn the_removed_fragment_format_is_rejected() {
            assert_eq!(
                PageBidsFormat::parse(Some("fragment")),
                Err("fragment".to_string())
            );
        }

        #[test]
        fn an_unknown_format_is_rejected_rather_than_defaulting_to_json() {
            assert_eq!(
                PageBidsFormat::parse(Some("scrpit")),
                Err("scrpit".to_string())
            );
            assert_eq!(PageBidsFormat::parse(Some("")), Err(String::new()));
        }

        #[test]
        fn an_unknown_format_response_is_not_storable() {
            // Every response from this endpoint is per-user. An error is no exception:
            // a cached 400 would be replayed to clients asking correctly.
            let response = page_bids_unknown_format();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|v| v.to_str().ok()),
                Some("private, no-store")
            );
        }
    }

    mod c2_store_authorization_tests {
        //! The store is authorized by the key's presence and nothing else. These cover
        //! the two ways that could silently break: storing without authorization, and
        //! storing twice for one request.

        use super::*;
        use crate::platform::ClientInfo;
        use crate::platform::test_support::{
            NoopConfigStore, NoopGeo, NoopSecretStore, StubBackend,
        };

        /// Records what was stored, so the assertions are about behaviour rather than
        /// about a call not returning an error.
        #[derive(Default)]
        struct RecordingCache {
            stored: Arc<Mutex<Vec<(String, usize)>>>,
        }

        struct RecordingReservation {
            stored: Arc<Mutex<Vec<(String, usize)>>>,
            url: String,
        }

        impl crate::platform::PlatformTemplateCacheReservation for RecordingReservation {
            fn insert(
                self: Box<Self>,
                _metadata: &crate::platform::TemplateMetadata,
                body: Vec<u8>,
                _max_age: Duration,
            ) -> Result<(), crate::platform::TemplateCacheError> {
                self.stored
                    .lock()
                    .expect("should lock recorded stores")
                    .push((self.url, body.len()));
                Ok(())
            }

            fn cancel(self: Box<Self>) -> Result<(), crate::platform::TemplateCacheError> {
                Ok(())
            }
        }

        #[async_trait::async_trait(?Send)]
        impl crate::platform::PlatformTemplateCache for RecordingCache {
            async fn get(
                &self,
                _key: &crate::platform::TemplateCacheKey,
            ) -> Result<crate::platform::TemplateEntry, crate::platform::TemplateCacheMiss>
            {
                Err(crate::platform::TemplateCacheMiss::NotFound)
            }

            async fn put(
                &self,
                key: &crate::platform::TemplateCacheKey,
                _metadata: &crate::platform::TemplateMetadata,
                body: Vec<u8>,
                _max_age: Duration,
            ) -> Result<(), crate::platform::TemplateCacheError> {
                self.stored
                    .lock()
                    .expect("should lock recorded stores")
                    .push((key.url.clone(), body.len()));
                Ok(())
            }

            async fn purge_url(
                &self,
                _key: &crate::platform::TemplateCacheKey,
            ) -> Result<(), crate::platform::TemplateCacheError> {
                Ok(())
            }

            async fn purge_all(&self) -> Result<(), crate::platform::TemplateCacheError> {
                Ok(())
            }
        }

        impl RecordingCache {
            fn recorded(&self) -> Vec<(String, usize)> {
                self.stored
                    .lock()
                    .expect("should lock recorded stores")
                    .clone()
            }
        }

        fn key() -> crate::platform::TemplateCacheKey {
            crate::platform::TemplateCacheKey {
                url: "https://example.com/page".to_string(),
                request_host: "example.com".to_string(),
                request_scheme: "https".to_string(),
                origin_identity: "https://origin.example.com\0origin.example.com".to_string(),
                assembly_mode: AssemblyMode::Esi,
                vary_values: vec![],
                template_fingerprint: "fp".to_string(),
                schema_version: crate::platform::TEMPLATE_SCHEMA_VERSION,
            }
        }

        fn authorization(cache: &RecordingCache) -> AuthorizedTemplateStore {
            AuthorizedTemplateStore {
                reservation: crate::platform::TemplateCacheReservation::new(Box::new(
                    RecordingReservation {
                        stored: Arc::clone(&cache.stored),
                        url: key().url,
                    },
                )),
                expires_at: Instant::now() + Duration::from_secs(30),
            }
        }

        fn services_with(cache: Arc<RecordingCache>) -> RuntimeServices {
            RuntimeServices::builder()
                .config_store(Arc::new(NoopConfigStore))
                .secret_store(Arc::new(NoopSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .backend(Arc::new(StubBackend))
                .geo(Arc::new(NoopGeo))
                .http_client(Arc::new(StubHttpClient::new()))
                .client_info(ClientInfo::default())
                .template_cache(cache)
                .build()
        }

        #[tokio::test]
        async fn an_unauthorized_response_stores_nothing() {
            // `None` is what the gate leaves behind on every bypass, and it is also the
            // default for Inline. If this ever stored, every bypass reason would be
            // decorative.
            let cache = Arc::new(RecordingCache::default());
            let settings = create_test_settings();
            let mut params = make_stream_params(&settings, "identity");
            params.template_cache_key = None;

            let _ = store_template_if_authorized(
                &services_with(Arc::clone(&cache)),
                &mut params,
                b"body",
            )
            .await;

            assert!(
                cache.recorded().is_empty(),
                "a response the gate rejected must not reach the cache"
            );
        }

        #[tokio::test]
        async fn an_authorized_response_stores_the_transformed_bytes() {
            let cache = Arc::new(RecordingCache::default());
            let settings = create_test_settings();
            let mut params = make_stream_params(&settings, "identity");
            params.template_cache_key = Some(authorization(&cache));

            let _ = store_template_if_authorized(
                &services_with(Arc::clone(&cache)),
                &mut params,
                b"<html>transformed</html>",
            )
            .await;

            assert_eq!(
                cache.recorded(),
                vec![("https://example.com/page".to_string(), 24)],
                "the authorized response should store its transformed bytes"
            );
        }

        #[tokio::test]
        async fn authorization_is_consumed_so_one_request_stores_once() {
            // The finalizers are layered, and a future change could plausibly call this
            // from both. Taking the key makes a double store impossible rather than
            // merely unlikely.
            let cache = Arc::new(RecordingCache::default());
            let settings = create_test_settings();
            let mut params = make_stream_params(&settings, "identity");
            params.template_cache_key = Some(authorization(&cache));
            let services = services_with(Arc::clone(&cache));

            let _ = store_template_if_authorized(&services, &mut params, b"first").await;
            let _ = store_template_if_authorized(&services, &mut params, b"second").await;

            assert_eq!(
                cache.recorded().len(),
                1,
                "authorization must be single-use"
            );
        }

        #[tokio::test]
        async fn origin_freshness_keeps_ticking_while_the_template_is_built() {
            let cache = Arc::new(RecordingCache::default());
            let settings = create_test_settings();
            let mut params = make_stream_params(&settings, "identity");
            let mut expired = authorization(&cache);
            expired.expires_at = Instant::now();
            params.template_cache_key = Some(expired);

            let outcome = store_template_if_authorized(
                &services_with(Arc::clone(&cache)),
                &mut params,
                b"body",
            )
            .await;

            assert_eq!(outcome, Some(TemplateStoreOutcome::Expired));
            assert!(
                cache.recorded().is_empty(),
                "C2 must not invent a new TTL after origin freshness elapsed"
            );
        }
    }

    mod c2_end_to_end_tests {
        //! The chain, end to end: a second request for the same URL must be served from
        //! the cache without touching the origin. Everything else in this file tests a
        //! link; this tests that they connect.

        use super::*;
        use crate::platform::ClientInfo;
        use crate::platform::test_support::{
            NoopConfigStore, NoopGeo, NoopSecretStore, StubBackend, StubHttpClient,
        };
        use crate::test_support::tests::crate_test_settings_str;
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        /// A working cache, unlike the recorder above — this one has to actually return
        /// what it stored, or a hit proves nothing.
        #[derive(Default)]
        struct MemoryTemplateCache {
            entries: Arc<Mutex<HashMap<String, crate::platform::TemplateEntry>>>,
            /// Set only after a reservation has committed its reader-neutral bytes.
            insert_completed: Arc<AtomicBool>,
            /// Every key `get` was called with, so a test can assert what was *asked
            /// for* rather than only what came back. A lookup that names a schema
            /// version this binary cannot assemble is the bug; whether the entry
            /// happened to survive the later marker check is not the same question.
            lookups: Arc<Mutex<Vec<crate::platform::TemplateCacheKey>>>,
            /// Every key `put` was called with, so a test can re-key an entry exactly
            /// instead of reconstructing what the request derived.
            stored_keys: Arc<Mutex<Vec<crate::platform::TemplateCacheKey>>>,
            /// Per-entry freshness handed from publisher policy to the platform cache.
            stored_max_ages: Arc<Mutex<Vec<Duration>>>,
            /// Force the lookup transaction to fail, for the fail-open + telemetry
            /// contract. A backend outage must never become a publisher outage.
            fail_lookup: AtomicBool,
        }

        struct MemoryTemplateReservation {
            entries: Arc<Mutex<HashMap<String, crate::platform::TemplateEntry>>>,
            insert_completed: Arc<AtomicBool>,
            stored_keys: Arc<Mutex<Vec<crate::platform::TemplateCacheKey>>>,
            stored_max_ages: Arc<Mutex<Vec<Duration>>>,
            key: crate::platform::TemplateCacheKey,
        }

        #[derive(Default)]
        struct RecordingTemplateAssembler {
            calls: AtomicUsize,
            fail: AtomicBool,
            expected_store_completion: Mutex<Option<Arc<AtomicBool>>>,
        }

        impl RecordingTemplateAssembler {
            fn require_store_before_call(&self, completed: Arc<AtomicBool>) {
                *self
                    .expected_store_completion
                    .lock()
                    .expect("should lock expected store completion") = Some(completed);
            }
        }

        impl crate::platform::PlatformTemplateAssembler for RecordingTemplateAssembler {
            fn assemble(
                &self,
                template: &[u8],
                fragment: &[u8],
            ) -> Result<Vec<u8>, crate::platform::TemplateAssemblyError> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if self
                    .expected_store_completion
                    .lock()
                    .expect("should lock expected store completion")
                    .as_ref()
                    .is_some_and(|completed| !completed.load(Ordering::Relaxed))
                {
                    return Err(crate::platform::TemplateAssemblyError::Failed {
                        message: "assembler ran before cache insertion completed".to_string(),
                    });
                }
                if self.fail.load(Ordering::Relaxed) {
                    return Err(crate::platform::TemplateAssemblyError::Failed {
                        message: "injected assembly failure".to_string(),
                    });
                }
                let (head, tail) = split_template_at_seam(template).map_err(|error| {
                    crate::platform::TemplateAssemblyError::Failed {
                        message: error.to_string(),
                    }
                })?;
                let mut assembled = Vec::with_capacity(head.len() + fragment.len() + tail.len());
                assembled.extend_from_slice(head);
                assembled.extend_from_slice(fragment);
                assembled.extend_from_slice(tail);
                Ok(assembled)
            }
        }

        impl crate::platform::PlatformTemplateCacheReservation for MemoryTemplateReservation {
            fn insert(
                self: Box<Self>,
                metadata: &crate::platform::TemplateMetadata,
                body: Vec<u8>,
                max_age: Duration,
            ) -> Result<(), crate::platform::TemplateCacheError> {
                self.stored_keys
                    .lock()
                    .expect("should lock stored keys")
                    .push(self.key.clone());
                self.stored_max_ages
                    .lock()
                    .expect("should lock stored max ages")
                    .push(max_age);
                self.entries.lock().expect("should lock entries").insert(
                    self.key.to_cache_key(),
                    crate::platform::TemplateEntry {
                        metadata: metadata.clone(),
                        body,
                    },
                );
                self.insert_completed.store(true, Ordering::Relaxed);
                Ok(())
            }

            fn cancel(self: Box<Self>) -> Result<(), crate::platform::TemplateCacheError> {
                Ok(())
            }
        }

        #[async_trait::async_trait(?Send)]
        impl crate::platform::PlatformTemplateCache for MemoryTemplateCache {
            async fn lookup_or_reserve(
                &self,
                key: &crate::platform::TemplateCacheKey,
            ) -> Result<crate::platform::TemplateCacheLookup, crate::platform::TemplateCacheError>
            {
                self.lookups
                    .lock()
                    .expect("should lock lookups")
                    .push(key.clone());
                if self.fail_lookup.load(Ordering::Relaxed) {
                    return Err(crate::platform::TemplateCacheError::Backend {
                        message: "injected lookup failure".to_string(),
                    });
                }
                if let Some(entry) = self
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .get(&key.to_cache_key())
                    .cloned()
                {
                    return Ok(crate::platform::TemplateCacheLookup::Hit(entry));
                }
                Ok(crate::platform::TemplateCacheLookup::Reserved(
                    crate::platform::TemplateCacheReservation::new(Box::new(
                        MemoryTemplateReservation {
                            entries: Arc::clone(&self.entries),
                            insert_completed: Arc::clone(&self.insert_completed),
                            stored_keys: Arc::clone(&self.stored_keys),
                            stored_max_ages: Arc::clone(&self.stored_max_ages),
                            key: key.clone(),
                        },
                    )),
                ))
            }

            async fn get(
                &self,
                key: &crate::platform::TemplateCacheKey,
            ) -> Result<crate::platform::TemplateEntry, crate::platform::TemplateCacheMiss>
            {
                self.lookups
                    .lock()
                    .expect("should lock lookups")
                    .push(key.clone());
                self.entries
                    .lock()
                    .expect("should lock entries")
                    .get(&key.to_cache_key())
                    .cloned()
                    .ok_or(crate::platform::TemplateCacheMiss::NotFound)
            }

            async fn put(
                &self,
                key: &crate::platform::TemplateCacheKey,
                metadata: &crate::platform::TemplateMetadata,
                body: Vec<u8>,
                max_age: Duration,
            ) -> Result<(), crate::platform::TemplateCacheError> {
                self.stored_keys
                    .lock()
                    .expect("should lock stored keys")
                    .push(key.clone());
                self.stored_max_ages
                    .lock()
                    .expect("should lock stored max ages")
                    .push(max_age);
                self.entries.lock().expect("should lock entries").insert(
                    key.to_cache_key(),
                    crate::platform::TemplateEntry {
                        metadata: metadata.clone(),
                        body,
                    },
                );
                self.insert_completed.store(true, Ordering::Relaxed);
                Ok(())
            }

            async fn purge_url(
                &self,
                key: &crate::platform::TemplateCacheKey,
            ) -> Result<(), crate::platform::TemplateCacheError> {
                self.entries
                    .lock()
                    .expect("should lock entries")
                    .remove(&key.to_cache_key());
                Ok(())
            }

            async fn purge_all(&self) -> Result<(), crate::platform::TemplateCacheError> {
                self.entries.lock().expect("should lock entries").clear();
                Ok(())
            }
        }

        /// Settings with the ad stack **live**, not merely configured.
        ///
        /// `[auction] enabled = true` and a slot matching the request path are both
        /// required, because `should_run_ad_stack` folds them together and half this
        /// path only executes when it is true. An earlier version of this helper left
        /// the auction disabled, which made every test here exercise the branch where
        /// TS never stamps its own `private, no-store` — and so missed that the C2 gate
        /// was reading that stamp and refusing to cache every page that runs ads.
        fn settings_with_mode(mode: &str) -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n\
                 [creative_opportunities]\ngam_network_id = \"99999\"\n\
                 assembly_mode = \"{mode}\"\n\n\
                 [[creative_opportunities.slot]]\n\
                 id = \"test-slot\"\n\
                 page_patterns = [\"/article\"]\n\
                 formats = [{{ width = 728, height = 90 }}]\n",
                crate_test_settings_str()
            );
            let mut settings =
                Settings::from_toml(&toml).expect("should parse settings with an assembly mode");
            // Mirrors `create_test_settings`; the integration registry refuses to build
            // without it.
            settings.proxy.allowed_domains =
                vec!["*.example".to_string(), "*.example.com".to_string()];
            settings
        }

        fn settings_with_mode_and_template_cache_max_age(mode: &str, seconds: u32) -> Settings {
            let mut settings = settings_with_mode(mode);
            settings
                .creative_opportunities
                .as_mut()
                .expect("should configure creative opportunities")
                .template_cache_max_age_seconds = Some(seconds);
            settings
        }

        fn services(
            http_client: Arc<StubHttpClient>,
            cache: Arc<MemoryTemplateCache>,
        ) -> RuntimeServices {
            RuntimeServices::builder()
                .config_store(Arc::new(NoopConfigStore))
                .secret_store(Arc::new(NoopSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .backend(Arc::new(StubBackend))
                .http_client(http_client)
                .geo(Arc::new(NoopGeo))
                .client_info(ClientInfo::default())
                .template_cache(cache)
                .build()
        }

        fn services_with_assembler(
            http_client: Arc<StubHttpClient>,
            cache: Arc<MemoryTemplateCache>,
            assembler: Arc<RecordingTemplateAssembler>,
        ) -> RuntimeServices {
            services(http_client, cache).with_template_assembler(assembler)
        }

        /// Shareable HTML: no `Set-Cookie`, no `Vary`, a public `Cache-Control`. Every
        /// condition the gate checks is satisfied, so a bypass here would be a bug in
        /// the wiring rather than in the fixture.
        fn queue_shareable_html(stub: &StubHttpClient) {
            stub.push_response_with_headers(
                200,
                b"<html><head></head><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                ],
            );
        }

        fn navigation_request() -> Request<EdgeBody> {
            HttpRequest::builder()
                .method(Method::GET)
                .uri("https://ts.example.com/article")
                .header(header::HOST, "ts.example.com")
                .header("sec-fetch-dest", "document")
                .header("sec-fetch-mode", "navigate")
                .body(EdgeBody::empty())
                .expect("should build navigation request")
        }

        /// A navigation that opts out of the server-side ad stack.
        ///
        /// A prefetch is the cheapest of the four opt-outs to build (the others are bot
        /// classification, denied consent, and the auction kill switch); all four reach
        /// the seam the same way, as `seam_ad_slots == None`.
        fn prefetch_navigation_request() -> Request<EdgeBody> {
            let mut request = navigation_request();
            request.headers_mut().insert(
                "sec-purpose",
                HeaderValue::from_static("prefetch;prerender"),
            );
            request
        }

        /// A navigation carrying an **active** diagnostics decision, the way the real one
        /// arrives: in the request extensions, set from a per-reader cookie or query
        /// parameter.
        fn diagnostics_navigation_request() -> Request<EdgeBody> {
            let mut request = navigation_request();
            request.extensions_mut().insert(
                crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision::active_for_tests(),
            );
            request
        }

        /// A slot matching `/article`.
        ///
        /// Passed through `AuctionDispatch`, not read from settings — and that is the
        /// distinction that matters. `should_run_ad_stack` requires a *matched* slot, so
        /// a config slot with no dispatch slot leaves the ad stack off and skips every
        /// branch that only runs when it is on.
        fn article_slot() -> crate::creative_opportunities::CreativeOpportunitySlot {
            crate::creative_opportunities::CreativeOpportunitySlot {
                id: "test-slot".to_string(),
                gam_unit_path: None,
                div_id: Some("test-slot".to_string()),
                page_patterns: vec!["/article".to_string()],
                formats: vec![crate::creative_opportunities::CreativeOpportunityFormat {
                    width: 728,
                    height: 90,
                    media_type: MediaType::Banner,
                }],
                floor_price: None,
                targeting: Default::default(),
                providers: Default::default(),
                compiled_patterns: Vec::new(),
                compiled_unit: None,
            }
        }

        /// Name of the bidding test double, matched by `[auction].providers`.
        const STUB_BIDDER: &str = "stub-bidder";

        /// The CPM the stub bids. Chosen so its price bucket (`"3.50"`) is a distinctive
        /// string that cannot appear in the fixture page by accident.
        const STUB_BID_CPM: f64 = 3.5;

        /// An auction provider that returns one genuine winning bid.
        ///
        /// Every other fixture in this file leaves the orchestrator with no providers, so
        /// every auction resolves to an empty bid map. That is exactly why a defect that
        /// discarded *non-empty* maps survived: no test ever produced one.
        struct WinningBidProvider;

        #[async_trait::async_trait(?Send)]
        impl crate::auction::provider::AuctionProvider for WinningBidProvider {
            fn provider_name(&self) -> &'static str {
                STUB_BIDDER
            }

            async fn request_bids(
                &self,
                _request: &AuctionRequest,
                context: &crate::auction::types::AuctionContext<'_>,
            ) -> Result<crate::auction::provider::ProviderRequestOutcome, Report<TrustedServerError>>
            {
                let request = PlatformHttpRequest::new(
                    HttpRequest::builder()
                        .method(Method::POST)
                        .uri("https://bidder.example.com/openrtb2/auction")
                        .body(EdgeBody::empty())
                        .expect("should build stub bid request"),
                    "stub-bidder-backend",
                );
                context
                    .services
                    .http_client()
                    .send_async(request)
                    .await
                    .change_context(TrustedServerError::Auction {
                        message: "stub bid launch failed".to_string(),
                    })
                    .map(crate::auction::provider::ProviderRequestOutcome::pending)
            }

            async fn parse_response(
                &self,
                _response: crate::platform::PlatformResponse,
                response_time_ms: u64,
            ) -> Result<crate::auction::types::AuctionResponse, Report<TrustedServerError>>
            {
                Ok(crate::auction::types::AuctionResponse::success(
                    STUB_BIDDER,
                    vec![Bid {
                        slot_id: "test-slot".to_string(),
                        price: Some(STUB_BID_CPM),
                        currency: "USD".to_string(),
                        creative: None,
                        adomain: None,
                        bidder: STUB_BIDDER.to_string(),
                        width: 728,
                        height: 90,
                        nurl: None,
                        burl: None,
                        bid_id: None,
                        creative_id: None,
                        renderer: None,
                        ad_id: Some("stub-creative-1".to_string()),
                        cache_id: None,
                        cache_host: None,
                        cache_path: None,
                        metadata: Default::default(),
                    }],
                    response_time_ms,
                ))
            }

            fn timeout_ms(&self) -> u32 {
                2000
            }

            fn backend_name(
                &self,
                _services: &RuntimeServices,
                _timeout_ms: u32,
            ) -> Option<String> {
                Some("stub-bidder-backend".to_string())
            }
        }

        /// [`settings_with_mode`], with an auction provider that actually bids.
        fn settings_with_bidder(mode: &str) -> Settings {
            let mut settings = settings_with_mode(mode);
            settings.auction.providers = vec![STUB_BIDDER.to_string()];
            settings
        }

        /// Queue the bidder's canned response.
        ///
        /// Must be pushed **before** the origin HTML: the stub client serves one shared
        /// FIFO queue, and the auction is dispatched before the origin fetch.
        fn queue_bid_response(stub: &StubHttpClient) {
            stub.push_response(200, b"{}".to_vec());
        }

        /// Which finalizer a request is driven through.
        ///
        /// Both exist and both own an `AssembleTemplate` arm with its own seam call, so a
        /// test that exercised one of them proves nothing about the other.
        #[derive(Clone, Copy)]
        enum Finalizer {
            /// Fastly's path: headers commit first and the seam is spliced inside the
            /// body stream.
            Streaming,
            /// Every buffered adapter's path: the seam is spliced eagerly.
            Buffered,
        }

        /// Runs the full request, **including the finalizer**.
        ///
        /// The finalizer is not optional here: the store happens once the transform has
        /// produced every byte, so a test that stopped at `handle_publisher_request`
        /// would never populate the cache and a hit could not be proven.
        async fn run(
            settings: &Arc<Settings>,
            services: &RuntimeServices,
            request: Request<EdgeBody>,
        ) -> Response<EdgeBody> {
            run_with_orchestrator(
                settings,
                services,
                request,
                AuctionOrchestrator::new(settings.auction.clone()),
                Finalizer::Streaming,
            )
            .await
        }

        /// [`run`], through the chosen finalizer.
        async fn run_via(
            settings: &Arc<Settings>,
            services: &RuntimeServices,
            request: Request<EdgeBody>,
            finalizer: Finalizer,
        ) -> Response<EdgeBody> {
            run_with_orchestrator(
                settings,
                services,
                request,
                AuctionOrchestrator::new(settings.auction.clone()),
                finalizer,
            )
            .await
        }

        /// [`run`], with a provider registered so the auction returns a real bid.
        async fn run_bidding(
            settings: &Arc<Settings>,
            services: &RuntimeServices,
            request: Request<EdgeBody>,
        ) -> Response<EdgeBody> {
            let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            orchestrator.register_provider(Arc::new(WinningBidProvider));
            run_with_orchestrator(
                settings,
                services,
                request,
                orchestrator,
                Finalizer::Streaming,
            )
            .await
        }

        async fn run_with_orchestrator(
            settings: &Arc<Settings>,
            services: &RuntimeServices,
            request: Request<EdgeBody>,
            orchestrator: AuctionOrchestrator,
            finalizer: Finalizer,
        ) -> Response<EdgeBody> {
            let orchestrator = Arc::new(orchestrator);
            let registry =
                IntegrationRegistry::new(settings).expect("should create integration registry");
            let consent = crate::consent::ConsentContext {
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                ..Default::default()
            };
            let mut ec_context = EcContext::new_for_test(None, consent);
            let publisher_response = handle_publisher_request(
                settings,
                services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &[article_slot()],
                    registry: None,
                },
                request,
                EdgeCacheHeader::SMaxageFallback,
            )
            .await
            .expect("should proxy publisher request");

            match finalizer {
                Finalizer::Streaming => publisher_response_into_streaming_response(
                    publisher_response,
                    &Method::GET,
                    Arc::clone(settings),
                    &registry,
                    orchestrator,
                    services.clone(),
                )
                .await
                .expect("should finalize publisher response"),
                Finalizer::Buffered => buffer_publisher_response_async(
                    publisher_response,
                    &Method::GET,
                    settings,
                    &registry,
                    &orchestrator,
                    services,
                )
                .await
                .expect("should finalize publisher response"),
            }
        }

        /// Collects a response body whether it was buffered or streamed.
        ///
        /// Both occur here by design: a response the gate authorized is buffered so it
        /// can be stored, and one it refused keeps streaming exactly like `inline`. A
        /// helper that assumed either would silently only ever test one of them.
        async fn body_of(response: Response<EdgeBody>) -> Vec<u8> {
            match response.into_body() {
                EdgeBody::Stream(mut stream) => {
                    let mut out = Vec::new();
                    while let Some(chunk) = stream.next().await {
                        out.extend_from_slice(&chunk.expect("stream chunk should read"));
                    }
                    out
                }
                other => other
                    .into_bytes()
                    .expect("a non-stream body has its bytes in hand")
                    .to_vec(),
            }
        }

        #[tokio::test]
        async fn a_second_request_is_served_from_the_cache_without_touching_the_origin() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));

            // Only one origin response is queued. If the second request reached the
            // origin it would find the queue empty, so this fixture is itself part of
            // the assertion.
            queue_shareable_html(&stub);

            let cold = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                cold.headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("miss-stored"),
                "a successful cold fill must be distinguishable from a bypass"
            );
            let first = body_of(cold).await;
            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the cold request must fetch the origin"
            );

            let warm = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                warm.headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("hit")
            );
            let second = body_of(warm).await;
            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the warm request must not fetch the origin — that saving is the point"
            );
            assert_eq!(
                second, first,
                "the cached template must be byte-identical to what was stored"
            );
        }

        #[tokio::test]
        async fn only_the_cold_miss_uses_the_platform_assembler() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let assembler = Arc::new(RecordingTemplateAssembler::default());
            assembler.require_store_before_call(Arc::clone(&cache.insert_completed));
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services_with_assembler(
                Arc::clone(&stub),
                Arc::clone(&cache),
                Arc::clone(&assembler),
            );
            queue_shareable_html(&stub);

            let cold = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                header_of(&cold, header::HeaderName::from_static("x-ts-assembly")),
                Some("esi-parser")
            );
            let cold_body = body_of(cold).await;
            assert_eq!(assembler.calls.load(Ordering::Relaxed), 1);

            let warm = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                header_of(&warm, header::HeaderName::from_static("x-ts-assembly")),
                Some("byte-seam")
            );
            let warm_body = body_of(warm).await;

            assert_eq!(
                assembler.calls.load(Ordering::Relaxed),
                1,
                "a warm hit must preserve the streaming byte-seam path"
            );
            assert_eq!(cold_body, warm_body);
        }

        #[tokio::test]
        async fn a_platform_assembly_failure_falls_back_to_a_complete_byte_seam() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let assembler = Arc::new(RecordingTemplateAssembler::default());
            assembler.fail.store(true, Ordering::Relaxed);
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services_with_assembler(
                Arc::clone(&stub),
                Arc::clone(&cache),
                Arc::clone(&assembler),
            );
            queue_shareable_html(&stub);

            let response = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                header_of(&response, header::HeaderName::from_static("x-ts-assembly")),
                Some("byte-seam-fallback")
            );
            let document = String::from_utf8(body_of(response).await)
                .expect("served document should be UTF-8");

            assert!(document.contains("origin"));
            assert!(document.contains("scheduleInitialAdInit"));
            assert!(!document.contains(AD_ASSEMBLY_SEAM));
            assert_eq!(assembler.calls.load(Ordering::Relaxed), 1);
        }

        #[tokio::test]
        async fn publisher_esi_is_never_stored_or_executed() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let assembler = Arc::new(RecordingTemplateAssembler::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services_with_assembler(
                Arc::clone(&stub),
                Arc::clone(&cache),
                Arc::clone(&assembler),
            );
            stub.push_response_with_headers(
                200,
                b"<html><head></head><body><ESI:remove>publisher</ESI:remove></body></html>"
                    .to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                ],
            );

            let response = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                header_of(&response, header::HeaderName::from_static("x-ts-c2-cache")),
                Some("bypass-response")
            );
            assert_eq!(
                header_of(&response, header::HeaderName::from_static("x-ts-assembly")),
                Some("byte-seam-fallback")
            );
            let document = String::from_utf8(body_of(response).await)
                .expect("served document should be UTF-8");

            assert!(document.to_ascii_lowercase().contains("<esi:remove>"));
            assert!(document.contains("scheduleInitialAdInit"));
            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "publisher-authored ESI must never enter the shared template cache"
            );
            assert_eq!(
                assembler.calls.load(Ordering::Relaxed),
                0,
                "publisher-authored ESI must never reach the platform parser"
            );
        }

        #[tokio::test]
        async fn ineligible_requests_and_responses_never_use_the_platform_assembler() {
            let inline_stub = Arc::new(StubHttpClient::new());
            let inline_cache = Arc::new(MemoryTemplateCache::default());
            let inline_assembler = Arc::new(RecordingTemplateAssembler::default());
            let inline_settings = Arc::new(settings_with_mode("inline"));
            let inline_services = services_with_assembler(
                Arc::clone(&inline_stub),
                Arc::clone(&inline_cache),
                Arc::clone(&inline_assembler),
            );
            queue_shareable_html(&inline_stub);

            let _ =
                body_of(run(&inline_settings, &inline_services, navigation_request()).await).await;
            assert_eq!(inline_assembler.calls.load(Ordering::Relaxed), 0);

            let private_stub = Arc::new(StubHttpClient::new());
            let private_cache = Arc::new(MemoryTemplateCache::default());
            let private_assembler = Arc::new(RecordingTemplateAssembler::default());
            let esi_settings = Arc::new(settings_with_mode("esi"));
            let private_services = services_with_assembler(
                Arc::clone(&private_stub),
                Arc::clone(&private_cache),
                Arc::clone(&private_assembler),
            );
            private_stub.push_response_with_headers(
                200,
                b"<html><head></head><body>private origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                    ("set-cookie", "reader=personalized; Path=/"),
                ],
            );

            let _ =
                body_of(run(&esi_settings, &private_services, navigation_request()).await).await;
            assert_eq!(private_assembler.calls.load(Ordering::Relaxed), 0);

            let authenticated_stub = Arc::new(StubHttpClient::new());
            let authenticated_cache = Arc::new(MemoryTemplateCache::default());
            let authenticated_assembler = Arc::new(RecordingTemplateAssembler::default());
            let authenticated_services = services_with_assembler(
                Arc::clone(&authenticated_stub),
                Arc::clone(&authenticated_cache),
                Arc::clone(&authenticated_assembler),
            );
            queue_shareable_html(&authenticated_stub);
            let mut authenticated_request = navigation_request();
            authenticated_request.headers_mut().insert(
                header::AUTHORIZATION,
                HeaderValue::from_static("Bearer example-token"),
            );

            let _ = body_of(
                run(
                    &esi_settings,
                    &authenticated_services,
                    authenticated_request,
                )
                .await,
            )
            .await;
            assert_eq!(authenticated_assembler.calls.load(Ordering::Relaxed), 0);

            let failed_cache_stub = Arc::new(StubHttpClient::new());
            let failed_cache = Arc::new(MemoryTemplateCache::default());
            failed_cache.fail_lookup.store(true, Ordering::Relaxed);
            let failed_cache_assembler = Arc::new(RecordingTemplateAssembler::default());
            let failed_cache_services = services_with_assembler(
                Arc::clone(&failed_cache_stub),
                Arc::clone(&failed_cache),
                Arc::clone(&failed_cache_assembler),
            );
            queue_shareable_html(&failed_cache_stub);

            let _ = body_of(run(&esi_settings, &failed_cache_services, navigation_request()).await)
                .await;
            assert_eq!(failed_cache_assembler.calls.load(Ordering::Relaxed), 0);
        }

        #[tokio::test]
        async fn fastly_surrogate_control_still_allows_a_cold_fill_and_warm_hit() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode_and_template_cache_max_age("esi", 1_200));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));

            // This is the policy observed on the publisher that exposed the bug. Fastly
            // gives the surrogate lifetime edge precedence; the configured safety
            // ceiling allows the origin's twenty-minute edge freshness.
            stub.push_response_with_headers(
                200,
                b"<html><head></head><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=60"),
                    (
                        "surrogate-control",
                        "max-age=1200, stale-while-revalidate=21600, \
                         stale-if-error=604800",
                    ),
                ],
            );

            let cold = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                cold.headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("miss-stored")
            );
            let _ = body_of(cold).await;
            let stored_max_age = cache
                .stored_max_ages
                .lock()
                .expect("should lock stored max ages")[0];
            assert!(
                stored_max_age <= Duration::from_secs(1_200)
                    && stored_max_age > Duration::from_secs(1_195),
                "the real store path should receive the configured twenty-minute ceiling, got {stored_max_age:?}"
            );

            let warm = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                warm.headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("hit")
            );
            let _ = body_of(warm).await;
            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the warm request must reuse the template authorized by Surrogate-Control"
            );
        }

        #[tokio::test]
        async fn one_identity_template_is_negotiated_per_reader_on_miss_and_hit() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            stub.push_response_with_headers(
                200,
                gzip_encode(b"<html><head></head><body>origin</body></html>"),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("content-encoding", "gzip"),
                    ("cache-control", "public, max-age=300"),
                    ("vary", "accept-encoding"),
                ],
            );

            let mut gzip_request = navigation_request();
            gzip_request
                .headers_mut()
                .insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
            let cold = run(&settings, &services, gzip_request).await;
            assert_eq!(
                cold.headers()
                    .get(header::CONTENT_ENCODING)
                    .and_then(|value| value.to_str().ok()),
                Some("gzip")
            );
            assert!(
                cold.headers().get_all(header::VARY).iter().any(|value| {
                    value.to_str().is_ok_and(|value| {
                        value
                            .split(',')
                            .any(|name| name.trim().eq_ignore_ascii_case("accept-encoding"))
                    })
                }),
                "the encoded cold response must declare reader negotiation"
            );
            let cold_decoded = gzip_decode(&body_of(cold).await);

            let warm_identity = run(&settings, &services, navigation_request()).await;
            assert!(
                warm_identity
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .is_none(),
                "a reader that did not advertise compression must receive identity"
            );
            let warm_identity_body = body_of(warm_identity).await;

            let mut warm_gzip_request = navigation_request();
            warm_gzip_request
                .headers_mut()
                .insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
            let warm_gzip = run(&settings, &services, warm_gzip_request).await;
            assert_eq!(
                warm_gzip
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .and_then(|value| value.to_str().ok()),
                Some("gzip")
            );
            assert_eq!(
                warm_gzip
                    .headers()
                    .get(header::VARY)
                    .and_then(|value| value.to_str().ok()),
                Some("Accept-Encoding")
            );
            let warm_gzip_decoded = gzip_decode(&body_of(warm_gzip).await);

            let mut warm_brotli_request = navigation_request();
            warm_brotli_request.headers_mut().insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("br, gzip;q=0.5"),
            );
            let warm_brotli = run(&settings, &services, warm_brotli_request).await;
            assert_eq!(
                warm_brotli
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .and_then(|value| value.to_str().ok()),
                Some("br")
            );
            let warm_brotli_decoded = brotli_decode(&body_of(warm_brotli).await);

            assert_eq!(stub.recorded_request_uris().len(), 1);
            assert_eq!(cold_decoded, warm_identity_body);
            assert_eq!(warm_gzip_decoded, warm_identity_body);
            assert_eq!(warm_brotli_decoded, warm_identity_body);
        }

        #[tokio::test]
        async fn a_reader_that_refuses_every_supported_encoding_bypasses_c2_losslessly() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            let mut request = navigation_request();
            request.headers_mut().insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("zstd, identity;q=0"),
            );

            let _ = body_of(run(&settings, &services, request).await).await;

            let forwarded = stub.recorded_request_headers();
            assert!(
                forwarded[0].iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("accept-encoding") && value == "zstd, identity;q=0"
                }),
                "an unsupported offer must reach origin unchanged so TS can pass through the \
                 representation instead of sending forbidden identity"
            );
            assert!(
                cache
                    .lookups
                    .lock()
                    .expect("should lock lookups")
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn an_unsupported_origin_encoding_bypasses_c2_without_relabeling_the_body() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            let wire_body = b"opaque-zstd-wire-bytes".to_vec();
            stub.push_response_with_headers(
                200,
                wire_body.clone(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("content-encoding", "zstd"),
                    ("cache-control", "public, max-age=300"),
                ],
            );

            let response = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .and_then(|value| value.to_str().ok()),
                Some("zstd"),
                "the pass-through bytes must keep the origin's representation label"
            );
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("bypass-response")
            );
            assert_eq!(body_of(response).await, wire_body);
            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn revalidation_partial_and_conditional_requests_bypass_a_warm_template() {
            for (name, value) in [
                (header::CACHE_CONTROL, "no-cache"),
                (header::PRAGMA, "no-cache"),
                (header::RANGE, "bytes=0-31"),
                (header::IF_NONE_MATCH, "\"reader-validator\""),
            ] {
                let stub = Arc::new(StubHttpClient::new());
                let cache = Arc::new(MemoryTemplateCache::default());
                let settings = Arc::new(settings_with_mode("esi"));
                let services = services(Arc::clone(&stub), Arc::clone(&cache));
                queue_shareable_html(&stub);
                queue_shareable_html(&stub);

                let _ = body_of(run(&settings, &services, navigation_request()).await).await;
                let mut revalidating = navigation_request();
                revalidating
                    .headers_mut()
                    .insert(name.clone(), HeaderValue::from_static(value));
                let _ = body_of(run(&settings, &services, revalidating).await).await;

                assert_eq!(
                    stub.recorded_request_uris().len(),
                    2,
                    "{name}: {value} must reach the origin even when C2 is warm"
                );
            }
        }

        fn header_of(response: &Response<EdgeBody>, name: header::HeaderName) -> Option<&str> {
            response.headers().get(name).and_then(|v| v.to_str().ok())
        }

        /// The bid payload the seam actually embedded, decoded from the served page.
        ///
        /// Deliberately **not** written as the inverse of [`html_escape_for_script`]: the
        /// escaper emits `\"`, `\\` and `\uXXXX`, which are all JSON string escapes, so
        /// re-quoting the payload and handing it to `serde_json`'s own string parser
        /// decodes it without this test agreeing with the encoder about anything.
        ///
        /// Panics rather than returning an error, because a served page with no decodable
        /// bid payload is the failure under test and its diagnostics belong at the
        /// assertion.
        fn seam_bids(document: &str) -> serde_json::Map<String, serde_json::Value> {
            const OPEN: &str = "var b=JSON.parse(\"";
            // The seam's fixed shape. A bare `")` could occur inside the payload; this
            // nine-byte terminator cannot.
            const CLOSE: &str = "\");var s=";
            let start = document
                .find(OPEN)
                .unwrap_or_else(|| panic!("the seam must carry a bids payload: {document}"));
            let rest = &document[start + OPEN.len()..];
            let end = rest
                .find(CLOSE)
                .unwrap_or_else(|| panic!("the bids payload must terminate: {document}"));
            let unescaped: String = serde_json::from_str(&format!("\"{}\"", &rest[..end]))
                .expect("the embedded payload should be a valid JSON string literal");
            serde_json::from_str(&unescaped).expect("the embedded payload should be a JSON object")
        }

        #[test]
        fn repeated_fresh_markers_are_all_neutralized_before_a_new_terminal_seam_is_added() {
            let input = format!("prefix{AD_ASSEMBLY_SEAM}middle{AD_ASSEMBLY_SEAM}publisher-tail");

            let normalized = normalize_fresh_template_seam(input.into_bytes());

            assert_eq!(
                normalized
                    .windows(AD_ASSEMBLY_SEAM.len())
                    .filter(|window| *window == AD_ASSEMBLY_SEAM.as_bytes())
                    .count(),
                1
            );
            assert!(
                normalized.ends_with(AD_ASSEMBLY_SEAM.as_bytes()),
                "normalization must mint its own unambiguous terminal seam"
            );
        }

        #[test]
        fn parser_validation_does_not_change_the_cached_schema() {
            assert_eq!(crate::platform::TEMPLATE_SCHEMA_VERSION, 4);
            assert_eq!(AD_ASSEMBLY_SEAM, "<!--ts-ad-seam-->");
            assert!(!contains_esi_directive(AD_ASSEMBLY_SEAM.as_bytes()));
        }

        /// Shareable HTML that already contains the seam marker.
        ///
        /// The marker is reserved, but publisher content can still contain it. Fresh
        /// normalization must disambiguate that content from the transform-owned seam.
        fn queue_html_that_collides_with_the_marker(stub: &StubHttpClient) {
            stub.push_response_with_headers(
                200,
                format!(
                    "<html><head></head><body>origin{AD_ASSEMBLY_SEAM}</body></html>\
                     {AD_ASSEMBLY_SEAM}"
                )
                .into_bytes(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                ],
            );
        }

        #[tokio::test]
        async fn an_origin_marker_collision_is_normalized_before_store() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_html_that_collides_with_the_marker(&stub);

            let cold = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("cold document should be UTF-8");
            let warm = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("warm document should be UTF-8");

            assert!(
                cold.contains("origin") && cold.contains("window.tsjs"),
                "a reserved-comment collision must not turn a valid origin 200 into a 500"
            );
            assert!(!cold.contains(AD_ASSEMBLY_SEAM));
            assert!(!warm.contains(AD_ASSEMBLY_SEAM));
            assert_eq!(stub.recorded_request_uris().len(), 1);
            let entries = cache.entries.lock().expect("should lock entries");
            let template = &entries
                .values()
                .next()
                .expect("template should be stored")
                .body;
            assert_eq!(
                template
                    .windows(AD_ASSEMBLY_SEAM.len())
                    .filter(|window| *window == AD_ASSEMBLY_SEAM.as_bytes())
                    .count(),
                1,
                "the stored template must retain exactly the transform-owned seam"
            );
            let seam_at = template
                .windows(AD_ASSEMBLY_SEAM.len())
                .position(|window| window == AD_ASSEMBLY_SEAM.as_bytes())
                .expect("stored template should contain its seam");
            let body_close_at = template
                .windows(b"</body>".len())
                .position(|window| window.eq_ignore_ascii_case(b"</body>"))
                .expect("fixture should retain its body close");
            assert!(
                seam_at < body_close_at,
                "publisher content after </body> must not be mistaken for TS's seam"
            );
        }

        #[tokio::test]
        async fn html_without_an_explicit_body_gets_a_terminal_seam_instead_of_a_500() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            stub.push_response_with_headers(
                200,
                b"<div>origin fragment</div>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                ],
            );

            let cold = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("cold fragment should be UTF-8");
            let warm = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("warm fragment should be UTF-8");

            for document in [&cold, &warm] {
                assert!(document.contains("origin fragment"));
                assert!(!document.contains(AD_ASSEMBLY_SEAM));
            }
            assert_eq!(stub.recorded_request_uris().len(), 1);
            assert!(
                !cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "the recovered template should remain cacheable"
            );
        }

        #[tokio::test]
        async fn an_unsplittable_cached_template_is_a_miss_before_any_header_commits() {
            // The hit path used to check only that a marker existed *somewhere* and
            // leave the exactly-one check to the finalizer. By then the 200 and its
            // headers were committed and, on the streaming adapter, the document head
            // was already on the wire — so the only available failure was a truncated
            // response. Falling back to the origin is a slower correct page.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            // Fill the cache legitimately, then corrupt the stored body in place so the
            // entry is found under exactly the key the next request derives.
            let _ = body_of(run(&settings, &services, navigation_request()).await).await;
            {
                let stored_key = cache
                    .stored_keys
                    .lock()
                    .expect("should lock stored keys")
                    .first()
                    .cloned()
                    .expect("the cold request should have stored a template");
                let mut entries = cache.entries.lock().expect("should lock entries");
                let entry = entries
                    .get_mut(&stored_key.to_cache_key())
                    .expect("the stored template should be readable");
                entry.body = format!(
                    "<html><head></head><body>origin{AD_ASSEMBLY_SEAM}\
                     {AD_ASSEMBLY_SEAM}</body></html>"
                )
                .into_bytes();
            }

            // The fall-back must be able to reach the origin.
            queue_shareable_html(&stub);
            let response = run(&settings, &services, navigation_request()).await;
            let status = response.status();
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("invalid"),
                "an invalid-object recovery must be observable without exposing its key"
            );
            let served = String::from_utf8(body_of(response).await)
                .expect("served document should be UTF-8");

            assert_eq!(
                status,
                StatusCode::OK,
                "the reader should get a complete page, not a committed-then-broken one"
            );
            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "an unusable entry must be treated as a miss and refetched"
            );
            assert!(
                !served.contains(AD_ASSEMBLY_SEAM),
                "no marker may survive into the served page: {served}"
            );
            assert!(
                served.contains("window.tsjs"),
                "and the fallback must still deliver the seam: {served}"
            );
            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "an unusable fresh object must be purged instead of forcing inline fallback \
                 until its TTL expires"
            );
        }

        /// The schema version whose templates carried an executable ESI tag at the seam.
        ///
        /// Pinned as a literal rather than derived from [`TEMPLATE_SCHEMA_VERSION`]: the
        /// point is that the current version is *not* this one, and a derived value
        /// would move with it and assert nothing.
        const ESI_INCLUDE_SCHEMA_VERSION: u32 = 1;

        /// The exact schema-v1 marker retained only as a cache-compatibility fixture.
        const LEGACY_ESI_INCLUDE: &str = "<esi:include src=\"/_ts/page-bids?format=fragment\"/>";

        #[tokio::test]
        async fn a_template_written_under_the_previous_schema_version_is_never_read() {
            // v1 put an executable ESI include at the seam. v2 puts an inert comment
            // there and hands slots to the scheduler, so a v1 entry has no marker this
            // binary can find. `schema_version` is the only thing keeping the two apart
            // — nothing purges C2 on deploy.
            assert_ne!(
                crate::platform::TEMPLATE_SCHEMA_VERSION,
                ESI_INCLUDE_SCHEMA_VERSION,
                "the seam marker changed shape, so the schema version must have moved \
                 off the value under which the old marker was stored"
            );

            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            // Fill the cache the ordinary way, then plant a predecessor's entry
            // alongside it: same request, previous schema version, previous marker.
            // Re-keyed from the key the store actually used, so the fixture cannot
            // drift from what the request derives.
            let _ = body_of(run(&settings, &services, navigation_request()).await).await;
            {
                let stored_key = cache
                    .stored_keys
                    .lock()
                    .expect("should lock stored keys")
                    .first()
                    .cloned()
                    .expect("the cold request should have stored a template");
                let mut entries = cache.entries.lock().expect("should lock entries");
                let mut predecessor = entries
                    .get(&stored_key.to_cache_key())
                    .cloned()
                    .expect("the stored template should be readable");
                predecessor.body =
                    format!("<html><head></head><body>origin{LEGACY_ESI_INCLUDE}</body></html>")
                        .into_bytes();
                predecessor.metadata.schema_version = ESI_INCLUDE_SCHEMA_VERSION;
                let mut old_key = stored_key;
                old_key.schema_version = ESI_INCLUDE_SCHEMA_VERSION;
                entries.insert(old_key.to_cache_key(), predecessor);
            }

            let served = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("served document should be UTF-8");

            assert!(
                cache
                    .lookups
                    .lock()
                    .expect("should lock lookups")
                    .iter()
                    .all(|key| key.schema_version != ESI_INCLUDE_SCHEMA_VERSION),
                "no lookup may name a schema version whose templates this binary cannot \
                 assemble"
            );
            assert!(
                !served.contains("<esi:"),
                "a predecessor's template must never reach a reader: {served}"
            );
        }

        #[tokio::test]
        async fn a_hit_for_opted_out_traffic_emits_no_seam_on_either_finalizer() {
            // The miss path already honours this: `assemble_if_shared` emits nothing when
            // the ad stack did not run. Both hit finalizers used `unwrap_or("[]")`
            // instead, so a prefetch, a bot, a consent-denied reader, or every request
            // under a disabled auction received a seam that still called
            // `scheduleInitialAdInit` — scheduling `adInit` for exactly the traffic that
            // opted out — as long as the template was already cached.
            for finalizer in [Finalizer::Streaming, Finalizer::Buffered] {
                let stub = Arc::new(StubHttpClient::new());
                let cache = Arc::new(MemoryTemplateCache::default());
                let settings = Arc::new(settings_with_mode("esi"));
                let services = services(Arc::clone(&stub), Arc::clone(&cache));
                queue_shareable_html(&stub);

                // A normal navigation fills the cache; only then is a hit available for
                // the opted-out request, which is the whole point — the gate held on the
                // miss and was ignored on the hit.
                let cold = String::from_utf8(
                    body_of(run_via(&settings, &services, navigation_request(), finalizer).await)
                        .await,
                )
                .expect("cold document should be UTF-8");
                assert!(
                    cold.contains("scheduleInitialAdInit"),
                    "the fixture must produce a seam for ordinary traffic, or this test \
                     passes for the wrong reason: {cold}"
                );

                let served = String::from_utf8(
                    body_of(
                        run_via(
                            &settings,
                            &services,
                            prefetch_navigation_request(),
                            finalizer,
                        )
                        .await,
                    )
                    .await,
                )
                .expect("served document should be UTF-8");

                assert_eq!(
                    stub.recorded_request_uris().len(),
                    1,
                    "the opted-out request must be a cache hit, or the hit finalizer is \
                     never reached"
                );
                assert!(
                    !served.contains("scheduleInitialAdInit"),
                    "a reader whose ad stack did not run must not have `adInit` \
                     scheduled: {served}"
                );
                assert!(
                    !served.contains("t.adSlots="),
                    "no slot assignment may reach opted-out traffic: {served}"
                );
                assert!(
                    !served.contains(AD_ASSEMBLY_SEAM),
                    "the marker must still be consumed even when nothing replaces it: \
                     {served}"
                );
                assert!(
                    served.contains("origin"),
                    "the page itself must still be served: {served}"
                );
            }
        }

        #[tokio::test]
        async fn a_winning_bid_reaches_the_page_on_both_the_miss_and_the_hit_path() {
            // The fixture every other test here lacks. With no registered provider the
            // auction always resolves to `{}`, so a seam that discarded every non-empty
            // bid map looked identical to one that worked — which is precisely how the
            // shared modes shipped serving zero bids.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_bidder("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));

            // Bid first, page second: one FIFO queue, and the auction is dispatched
            // before the origin fetch.
            queue_bid_response(&stub);
            queue_shareable_html(&stub);

            let cold = String::from_utf8(
                body_of(run_bidding(&settings, &services, navigation_request()).await).await,
            )
            .expect("cold document should be UTF-8");
            let cold_bids = seam_bids(&cold);
            assert_eq!(
                cold_bids
                    .get("test-slot")
                    .and_then(|bid| bid.get("hb_pb"))
                    .and_then(serde_json::Value::as_str),
                Some("3.50"),
                "the miss path must deliver the winning bid's bucketed price, not an \
                 empty map: {cold}"
            );
            assert_eq!(
                cold_bids
                    .get("test-slot")
                    .and_then(|bid| bid.get("hb_bidder"))
                    .and_then(serde_json::Value::as_str),
                Some(STUB_BIDDER),
                "the miss path must name the winning bidder: {cold}"
            );

            // The warm request needs only the bid response: a hit skips the origin
            // entirely, so an unused page fixture here would mask a fetch.
            queue_bid_response(&stub);
            let warm = String::from_utf8(
                body_of(run_bidding(&settings, &services, navigation_request()).await).await,
            )
            .expect("warm document should be UTF-8");
            assert_eq!(
                stub.recorded_request_uris()
                    .iter()
                    .filter(|uri| uri.contains("/article"))
                    .count(),
                1,
                "the warm request must be served from the template cache"
            );
            let warm_bids = seam_bids(&warm);
            assert_eq!(
                warm_bids
                    .get("test-slot")
                    .and_then(|bid| bid.get("hb_pb"))
                    .and_then(serde_json::Value::as_str),
                Some("3.50"),
                "the hit path must deliver this reader's winning bid, not an empty \
                 map: {warm}"
            );
            assert_eq!(
                warm_bids, cold_bids,
                "both paths must splice the same bids for the same auction"
            );
        }

        #[tokio::test]
        async fn max_age_zero_reload_reuses_a_warm_template_and_runs_a_fresh_auction() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_bidder("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));

            queue_bid_response(&stub);
            queue_shareable_html(&stub);
            let cold = run_bidding(&settings, &services, navigation_request()).await;
            assert_eq!(
                cold.headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("miss-stored")
            );
            let _ = body_of(cold).await;

            // Keep an origin response available so the old bypass behavior completes
            // normally and fails on the observable cache state rather than a fixture
            // underflow. A correct warm reload leaves this response untouched.
            queue_bid_response(&stub);
            queue_shareable_html(&stub);
            let mut reload = navigation_request();
            reload
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=0"));
            let warm = run_bidding(&settings, &services, reload).await;
            assert_eq!(
                warm.headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("hit"),
                "a browser reload must re-run per-reader assembly without refetching the \
                 reader-neutral template"
            );
            assert_eq!(
                warm.headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, no-store"),
                "reusing C2 must not make the assembled response browser-cacheable"
            );
            let warm = String::from_utf8(body_of(warm).await)
                .expect("warm reload document should be UTF-8");

            let requests = stub.recorded_request_uris();
            assert_eq!(
                requests
                    .iter()
                    .filter(|uri| uri.contains("/article"))
                    .count(),
                1,
                "the reload must reuse the warm article template"
            );
            assert_eq!(
                stub.recorded_backend_names()
                    .iter()
                    .filter(|backend| backend.as_str() == "stub-bidder-backend")
                    .count(),
                2,
                "each navigation must dispatch its own auction even when C2 hits"
            );
            assert_eq!(
                seam_bids(&warm)
                    .get("test-slot")
                    .and_then(|bid| bid.get("hb_pb"))
                    .and_then(serde_json::Value::as_str),
                Some("3.50"),
                "the reload's auction result must reach the assembled seam"
            );
        }

        #[tokio::test]
        async fn the_marker_never_reaches_the_browser_on_either_path() {
            // The user-visible failure this whole chain exists to avoid: a page that
            // returns 200, parses fine, renders no ads, and reports no error, because
            // the legacy executable ESI tag was served literally.
            //
            // Both paths are checked. The miss path assembles after the transform; the
            // hit path assembles after reading the cache. They are separate call sites
            // and only one of them running would still look like success on the other.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let cold = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("cold response should be utf-8");
            let warm = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("warm response should be utf-8");

            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the second request must be a hit, or this only tests one path"
            );
            for (label, document) in [("miss", &cold), ("hit", &warm)] {
                assert!(
                    !document.contains(AD_ASSEMBLY_SEAM),
                    "{label}: an unresolved marker reached the browser: {document}"
                );
                assert!(
                    document.contains("window.tsjs"),
                    "{label}: assembly must leave a bids script behind: {document}"
                );
            }
        }

        #[tokio::test]
        async fn the_cached_template_holds_the_marker_and_never_the_bids() {
            // The ordering the C3 prohibition depends on: store before assembling. If
            // these were swapped, the cache would hold one visitor's bids and serve them
            // to the next — and every test above would still pass, because the served
            // page would look correct.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let _ = run(&settings, &services, navigation_request()).await;

            let entries = cache.entries.lock().expect("should lock entries");
            let stored = entries
                .values()
                .next()
                .expect("a template should be stored");
            let template = core::str::from_utf8(&stored.body).expect("template should be utf-8");

            assert!(
                template.contains(AD_ASSEMBLY_SEAM),
                "the cached template must still carry the unresolved marker: {template}"
            );
            assert!(
                !template.contains("window.tsjs"),
                "the cached template must not carry a bids script: {template}"
            );
        }

        #[tokio::test]
        async fn a_transform_that_overruns_its_buffer_stores_nothing() {
            // The 16 MB cap in production, shrunk here. A partial template in C2 is the
            // worst outcome available: it would be served to every subsequent visitor as
            // a truncated document, indefinitely, with no error after the first request.
            //
            // Safe by construction — the cap error propagates before the store runs — but
            // "by construction" is exactly the kind of claim that stops being true after
            // an unrelated refactor moves one line.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let mut raw = settings_with_mode("esi");
            raw.publisher.max_buffered_body_bytes = 8;
            let settings = Arc::new(raw);
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let consent = crate::consent::ConsentContext {
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                ..Default::default()
            };
            let mut ec_context = EcContext::new_for_test(None, consent);
            let publisher_response = handle_publisher_request(
                &settings,
                &services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &[article_slot()],
                    registry: None,
                },
                navigation_request(),
                EdgeCacheHeader::SMaxageFallback,
            )
            .await
            .expect("the request itself should succeed; the cap trips during streaming");

            let result = publisher_response_into_streaming_response(
                publisher_response,
                &Method::GET,
                Arc::clone(&settings),
                &registry,
                orchestrator,
                services.clone(),
            )
            .await;

            assert!(
                result.is_err(),
                "overrunning the buffer must fail rather than truncate"
            );
            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "a failed transform must leave nothing in the shared cache"
            );
        }

        #[tokio::test]
        async fn a_cache_hit_is_never_shared_cacheable() {
            // Asserted positively, because the obvious negative check does not work.
            // Forbidding `public`, `s-maxage` and `Surrogate-Control` passes trivially
            // on a response that carries no `Cache-Control` at all — and *that* is the
            // real failure mode here, since a C2 hit returns before the point where the
            // publisher path stamps the response private. HTML with no `Cache-Control`
            // is heuristically cacheable, so "nothing to forbid" is not safety.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let _cold = run(&settings, &services, navigation_request()).await;
            let warm = run(&settings, &services, navigation_request()).await;

            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the second request must be a hit, or this asserts nothing"
            );
            assert_eq!(
                header_of(&warm, header::CACHE_CONTROL),
                Some("private, no-store"),
                "an assembled response is per-user even when its template is not"
            );

            // Validators would let a client revalidate into a shared copy, and the CDN
            // directives would instruct an intermediary to store one outright. The
            // origin fixture sends a `public, max-age=300` that must not survive.
            for stripped in [header::ETAG, header::LAST_MODIFIED, header::EXPIRES] {
                assert_eq!(
                    header_of(&warm, stripped.clone()),
                    None,
                    "{stripped} must not survive onto an assembled response"
                );
            }
        }

        #[tokio::test]
        async fn a_returning_visitor_gets_the_same_privacy_headers() {
            // The case with no backstop. A first-visit response sets an EC cookie, so
            // the adapter's cookie-privacy net force-privatizes it regardless of what
            // this path does. A returning visitor sets no cookie, so that net never
            // fires and this path is the only thing standing between an assembled
            // per-user document and a shared cache.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let _cold = run(&settings, &services, navigation_request()).await;
            let warm = run(&settings, &services, navigation_request()).await;

            assert!(
                header_of(&warm, header::SET_COOKIE).is_none(),
                "no cookie here means no privacy net, which is the point of this test"
            );
            assert_eq!(
                header_of(&warm, header::CACHE_CONTROL),
                Some("private, no-store")
            );
        }

        /// Geo that reports a fixed, recognizable location.
        ///
        /// A distinct value per synthetic user, so a geo leak into the template shows up
        /// as a substring rather than requiring inference.
        struct StubGeo(&'static str);

        impl crate::platform::PlatformGeo for StubGeo {
            fn lookup(
                &self,
                _client_ip: Option<std::net::IpAddr>,
            ) -> Result<Option<GeoInfo>, Report<crate::platform::PlatformError>> {
                Ok(Some(GeoInfo {
                    city: self.0.to_string(),
                    country: self.0.to_string(),
                    continent: self.0.to_string(),
                    latitude: 1.0,
                    longitude: 2.0,
                    metro_code: 3,
                    region: Some(self.0.to_string()),
                    asn: Some(4),
                }))
            }
        }

        /// One synthetic user: an identity, a consent posture, and a location.
        struct SyntheticUser {
            ec_id: &'static str,
            jurisdiction: crate::consent::jurisdiction::Jurisdiction,
            geo_marker: &'static str,
        }

        /// Runs one synthetic user against a fresh cache and returns the stored template.
        ///
        /// Fresh cache per user deliberately: the point is to compare what each *would*
        /// store, so sharing a cache would let the first user's entry answer for the
        /// second and the comparison would prove nothing.
        async fn stored_template_for(user: &SyntheticUser) -> Vec<u8> {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = RuntimeServices::builder()
                .config_store(Arc::new(NoopConfigStore))
                .secret_store(Arc::new(NoopSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .backend(Arc::new(StubBackend))
                .http_client(Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>)
                .geo(Arc::new(StubGeo(user.geo_marker)))
                .client_info(ClientInfo::default())
                .template_cache(
                    Arc::clone(&cache) as Arc<dyn crate::platform::PlatformTemplateCache>
                )
                .build();
            queue_shareable_html(&stub);

            let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let consent = crate::consent::ConsentContext {
                jurisdiction: user.jurisdiction.clone(),
                ..Default::default()
            };
            let mut ec_context = EcContext::new_for_test(Some(user.ec_id.to_string()), consent);
            let publisher_response = handle_publisher_request(
                &settings,
                &services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &[article_slot()],
                    registry: None,
                },
                navigation_request(),
                EdgeCacheHeader::SMaxageFallback,
            )
            .await
            .expect("should proxy publisher request");
            let _ = publisher_response_into_streaming_response(
                publisher_response,
                &Method::GET,
                Arc::clone(&settings),
                &registry,
                orchestrator,
                services.clone(),
            )
            .await
            .expect("should finalize publisher response");

            let entries = cache.entries.lock().expect("should lock entries");
            entries
                .values()
                .next()
                .expect("a template should have been stored")
                .body
                .clone()
        }

        #[tokio::test]
        async fn two_users_differing_in_identity_consent_and_geo_store_the_same_template() {
            // The gate the whole design rests on. The template is shared between
            // visitors, so anything request-scoped that reaches it is one visitor's data
            // served to the next. Byte-identity is the assertion because it does not
            // depend on guessing which field might leak.
            let alice = SyntheticUser {
                ec_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.alice1",
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                geo_marker: "AliceCity",
            };
            let bob = SyntheticUser {
                ec_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.bobbb1",
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::Gdpr,
                geo_marker: "BobCity",
            };

            let alice_template = stored_template_for(&alice).await;
            let bob_template = stored_template_for(&bob).await;

            assert_eq!(
                alice_template, bob_template,
                "two users differing in identity, consent and geo must produce the same \
                 shared template"
            );

            // Belt and braces: byte-identity would also hold if *both* templates leaked
            // the same wrong thing, so name the values that must be absent.
            let template = String::from_utf8(alice_template).expect("template should be utf-8");
            for forbidden in [
                alice.ec_id,
                bob.ec_id,
                alice.geo_marker,
                bob.geo_marker,
                "adSlots",
                "window.tsjs",
            ] {
                assert!(
                    !template.contains(forbidden),
                    "`{forbidden}` must not appear in a shared template: {template}"
                );
            }
        }

        #[tokio::test]
        async fn inline_mode_never_reads_or_writes_the_cache() {
            // The shipped path. If this ever cached, per-user ad state would be shared
            // between visitors — the exact failure the whole design exists to avoid.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("inline"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let _ = run(&settings, &services, navigation_request()).await;
            let _ = run(&settings, &services, navigation_request()).await;

            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "inline must fetch the origin every time"
            );
            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "inline must never write a shared template"
            );
        }

        /// [`settings_with_mode`], with one integration configured a stated way.
        ///
        /// Edits the parsed `[integrations]` map rather than appending TOML, so two
        /// fixtures differ in exactly the field under test.
        fn settings_with_prebid_timeout(mode: &str, timeout_ms: u32) -> Settings {
            let mut settings = settings_with_mode(mode);
            settings.integrations.insert(
                "prebid".to_string(),
                serde_json::json!({
                    "enabled": true,
                    "server_url": "https://prebid.example.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example.com/prebid/bundle.js",
                    "timeout": timeout_ms,
                }),
            );
            settings
        }

        /// Every key the cache was asked to store, rendered.
        fn stored_cache_keys(cache: &MemoryTemplateCache) -> Vec<String> {
            cache
                .stored_keys
                .lock()
                .expect("should lock stored keys")
                .iter()
                .map(crate::platform::TemplateCacheKey::to_cache_key)
                .collect()
        }

        /// Every key the cache was asked to read, rendered.
        fn looked_up_cache_keys(cache: &MemoryTemplateCache) -> Vec<String> {
            cache
                .lookups
                .lock()
                .expect("should lock lookups")
                .iter()
                .map(crate::platform::TemplateCacheKey::to_cache_key)
                .collect()
        }

        #[tokio::test]
        async fn two_integration_configurations_never_share_a_template() {
            // `template_fingerprint` folds the complete typed config, and its own
            // tests call it directly — so reverting the *call site* back to the
            // bundle-only hash, a constant for a given binary, left the entire suite
            // green while every configuration silently shared one template.
            //
            // This drives the real request path and asserts on the keys the cache was
            // actually handed, which is the only place that mutation is visible.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            let first = Arc::new(settings_with_prebid_timeout("esi", 1000));
            let second = Arc::new(settings_with_prebid_timeout("esi", 2500));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let _ = run(&first, &services, navigation_request()).await;
            let _ = run(&second, &services, navigation_request()).await;

            let stored = stored_cache_keys(&cache);
            assert_eq!(
                stored.len(),
                2,
                "each configuration must store its own template, got {stored:?}"
            );
            assert_ne!(
                stored[0], stored[1],
                "two `[integrations]` configurations must key different templates; one key \
                 serves the first configuration's injected markup to the second"
            );
            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "the second configuration must have missed and fetched the origin rather \
                 than reading the first's template"
            );
        }

        #[tokio::test]
        async fn one_integration_configuration_keys_one_template() {
            // The converse, and the failure mode a fingerprint fix can introduce:
            // over-invalidating is as total as under-invalidating. A fingerprint that
            // moves between two equal configurations is a cache that never hits, which
            // the spike would report as "no measurable benefit" rather than as a bug.
            //
            // The two `Settings` are parsed independently, so their `[integrations]`
            // maps iterate in different orders — which is what exercises the sort.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            let first = Arc::new(settings_with_prebid_timeout("esi", 1000));
            let second = Arc::new(settings_with_prebid_timeout("esi", 1000));
            queue_shareable_html(&stub);

            let _ = run(&first, &services, navigation_request()).await;
            let _ = run(&second, &services, navigation_request()).await;

            let looked_up = looked_up_cache_keys(&cache);
            assert_eq!(
                looked_up.len(),
                2,
                "both requests must consult the cache, got {looked_up:?}"
            );
            assert_eq!(
                looked_up[0], looked_up[1],
                "equal configurations must name one key, or the cache never hits"
            );
            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the second request must be served from the first's template"
            );
        }

        fn cookie_navigation_request() -> Request<EdgeBody> {
            HttpRequest::builder()
                .method(Method::GET)
                .uri("https://ts.example.com/article")
                .header(header::HOST, "ts.example.com")
                .header("sec-fetch-dest", "document")
                .header("sec-fetch-mode", "navigate")
                .header(header::COOKIE, "ts-ec=abc123")
                .body(EdgeBody::empty())
                .expect("should build cookie-bearing request")
        }

        #[tokio::test]
        async fn by_default_a_cookie_bearing_request_uses_no_shared_cache() {
            // The shipped default, and the reason the cache is nearly inert on real
            // traffic: TS sets its own identity cookie, so essentially every repeat
            // visitor arrives carrying one and is excluded in both directions.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let _ = run(&settings, &services, cookie_navigation_request()).await;
            let _ = run(&settings, &services, cookie_navigation_request()).await;

            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "both requests must reach the origin"
            );
            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "and neither may store a template"
            );
        }

        #[tokio::test]
        async fn a_declared_cookie_independent_origin_lets_repeat_visitors_share() {
            // The opt-in. Without it the spike can only ever measure first-ever page
            // views, which is not the population the issue cares about.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let mut raw = settings_with_mode("esi");
            raw.creative_opportunities
                .as_mut()
                .expect("fixture configures creative opportunities")
                .origin_is_cookie_independent = Some(true);
            let settings = Arc::new(raw);
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let _ = run(&settings, &services, cookie_navigation_request()).await;
            let _ = run(&settings, &services, cookie_navigation_request()).await;

            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the second cookie-bearing request should be served from the cache"
            );
        }

        #[tokio::test]
        async fn an_active_diagnostics_request_never_stores_a_template() {
            // An independent review reintroduced a diagnostics leak scoped to
            // A request-private diagnostics mutation could otherwise leak through a
            // shared template. `requires_private_no_store()` is a
            // strict superset of the condition under which diagnostics markup is
            // emitted, and that stamp lands *before* the C2 gate reads response headers,
            // so such a request never stores a template at all.
            //
            // That is a coincidence between two independent conditions, and the whole
            // protection rests on it. This pins the consequence directly, so the
            // relationship is checked rather than merely reasoned about.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let _ = run(&settings, &services, diagnostics_navigation_request()).await;

            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "a reader running diagnostics must not contribute to a shared cache"
            );
        }

        #[tokio::test]
        async fn active_diagnostics_bypass_an_already_warm_template() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let _ = body_of(run(&settings, &services, navigation_request()).await).await;
            let response = run(&settings, &services, diagnostics_navigation_request()).await;
            let document = String::from_utf8(body_of(response).await)
                .expect("diagnostics document should be UTF-8");

            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "request-private diagnostics must reach the origin even when an ordinary \
                 shared template is warm"
            );
            assert_eq!(
                cache.lookups.lock().expect("should lock lookups").len(),
                1,
                "the diagnostics request must not consult C2 at all"
            );
            assert!(
                document.contains("__tsjs_gpt_diagnostics_active"),
                "origin fallback must retain the request-private diagnostics bootstrap"
            );
        }

        #[tokio::test]
        async fn datadome_suppressed_request_bypasses_a_warm_shared_template() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let mut raw = settings_with_mode("esi");
            raw.integrations
                .insert_config(
                    "datadome",
                    &serde_json::json!({
                        "enabled": true,
                        "client_side_key": "test-client-key",
                    }),
                )
                .expect("should configure DataDome integration");
            let settings = Arc::new(raw);
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let ordinary = run(&settings, &services, navigation_request()).await;
            let ordinary_document = String::from_utf8(body_of(ordinary).await)
                .expect("ordinary document should be UTF-8");
            assert!(
                ordinary_document.contains("/integrations/datadome/tags.js"),
                "the warm template fixture must contain the ordinary DataDome tag"
            );

            let mut suppressed_request = navigation_request();
            suppressed_request
                .headers_mut()
                .insert("sec-fetch-dest", HeaderValue::from_static("script"));
            suppressed_request
                .extensions_mut()
                .insert(crate::integrations::datadome::DataDomeClientTagSuppressed);
            let suppressed = run(&settings, &services, suppressed_request).await;
            assert_eq!(
                suppressed
                    .headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("bypass-request"),
                "request-scoped tag suppression must not read a shared template"
            );
            let suppressed_document = String::from_utf8(body_of(suppressed).await)
                .expect("suppressed document should be UTF-8");

            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "the suppressed navigation must reach the origin even when C2 is warm"
            );
            assert_eq!(
                cache.lookups.lock().expect("should lock lookups").len(),
                1,
                "the suppressed request must bypass C2 before lookup"
            );
            assert!(
                !suppressed_document.contains("/integrations/datadome/tags.js"),
                "the request-scoped suppression decision must survive origin processing"
            );
        }

        #[tokio::test]
        async fn real_diagnostics_query_bypasses_c2_and_keeps_its_private_bootstrap() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let mut raw = settings_with_mode("esi");
            raw.integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
                .expect("should configure diagnostics");
            let settings = Arc::new(raw);
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let mut request = navigation_request();
            *request.uri_mut() = "https://ts.example.com/article?ts_console=1"
                .parse()
                .expect("should parse diagnostics URI");
            let response = run(&settings, &services, request).await;
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("bypass-request")
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, no-store")
            );
            assert!(response.headers().contains_key(header::SET_COOKIE));
            let document = String::from_utf8(body_of(response).await)
                .expect("diagnostics document should be UTF-8");
            assert!(document.contains("__tsjs_gpt_diagnostics_active"));
            assert!(document.contains("tsjs-gpt_diagnostics.min.js"));
            assert!(
                cache
                    .lookups
                    .lock()
                    .expect("should lock lookups")
                    .is_empty(),
                "the real query activation must bypass lookup before origin work"
            );
        }

        #[tokio::test]
        async fn real_diagnostics_cookie_bypasses_a_warm_cookie_independent_template() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let mut raw = settings_with_mode("esi");
            raw.creative_opportunities
                .as_mut()
                .expect("fixture configures creative opportunities")
                .origin_is_cookie_independent = Some(true);
            raw.integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
                .expect("should configure diagnostics");
            let settings = Arc::new(raw);
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let _ = body_of(run(&settings, &services, navigation_request()).await).await;
            let mut diagnostics = navigation_request();
            diagnostics.headers_mut().insert(
                header::COOKIE,
                HeaderValue::from_static("__Host-ts-console=1"),
            );
            let response = run(&settings, &services, diagnostics).await;
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("bypass-request")
            );
            let document = String::from_utf8(body_of(response).await)
                .expect("diagnostics document should be UTF-8");

            assert_eq!(stub.recorded_request_uris().len(), 2);
            assert_eq!(
                cache.lookups.lock().expect("should lock lookups").len(),
                1,
                "the diagnostics cookie must bypass the otherwise-eligible warm lookup"
            );
            assert!(document.contains("__tsjs_gpt_diagnostics_active"));
        }

        #[tokio::test]
        async fn a_cache_backend_failure_falls_back_to_origin_and_is_observable() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            cache.fail_lookup.store(true, Ordering::Relaxed);
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let response = run(&settings, &services, navigation_request()).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_X_TS_C2_CACHE)
                    .and_then(|value| value.to_str().ok()),
                Some("backend-error")
            );
            let document = String::from_utf8(body_of(response).await)
                .expect("fallback document should be UTF-8");
            assert!(document.contains("origin"));
            assert_eq!(stub.recorded_request_uris().len(), 1);
        }

        #[tokio::test]
        async fn a_cache_hit_streams_rather_than_buffering() {
            // The property that makes the cache worth having, and the one that regressed
            // silently. Buffered assembly held the first byte until the auction resolved
            // — measured at ~100x worse TTFB than shipping nothing at all, because the
            // inline path already streams and waits only at `</body>`.
            //
            // Asserted on the body *shape* rather than on timing: a timing test would be
            // flaky, and `EdgeBody::Stream` is the structural fact that produces the
            // timing.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            // Warm the cache, then take the hit.
            let _ = run(&settings, &services, navigation_request()).await;
            let warm = run(&settings, &services, navigation_request()).await;

            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "the second request must be a hit, or this asserts nothing"
            );
            assert!(
                warm.headers().get(header::CONTENT_LENGTH).is_none(),
                "a streamed assembly has no length until bids resolve, and headers \
                 commit before the first byte"
            );

            // The invariant that actually matters, and the one a body-shape assertion
            // misses: a stream that awaited the auction before its first yield would
            // still be an `EdgeBody::Stream` and would still hold the first byte.
            //
            // So pull exactly one chunk and prove the auction has not been collected
            // yet — `ad_bids_state` is only written by the collector. No timing
            // involved, so nothing to be flaky about.
            let EdgeBody::Stream(mut stream) = warm.into_body() else {
                panic!("a cache hit must stream, not buffer");
            };
            let first = stream
                .next()
                .await
                .expect("the stream should yield a first chunk")
                .expect("the first chunk should read");

            assert!(
                first.starts_with(b"<!doctype html>") || first.starts_with(b"<html"),
                "the first chunk must be the document head, not the bids: {:?}",
                String::from_utf8_lossy(&first[..first.len().min(60)])
            );
            assert!(
                !first.windows(11).any(|w| w == b"window.tsjs"),
                "the first chunk must precede the bids script"
            );
        }

        #[tokio::test]
        async fn a_bypassed_response_falls_back_to_inline_at_every_seam() {
            // Observed against a real origin: a page arrived with a raw
            // executable ESI markup in it and no bids at all. The `</body>` seam emitted
            // the marker because the mode was `Esi`, while assembly was skipped because
            // the gate had refused a key — a fallback at one seam and not the other.
            //
            // Bypassing is the *normal* case against a real origin, so this path runs
            // far more often than the shared one. It has to produce a working page.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));

            // A `Set-Cookie` from the origin disqualifies the response, exactly as a
            // real origin does.
            stub.push_response_with_headers(
                200,
                b"<html><head></head><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                    ("set-cookie", "sess=1"),
                ],
            );

            let document = String::from_utf8(
                body_of(run(&settings, &services, navigation_request()).await).await,
            )
            .expect("response should be utf-8");

            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty(),
                "the gate refused, so nothing may be stored"
            );
            assert!(
                !document.contains(AD_ASSEMBLY_SEAM),
                "a marker must never be emitted when nothing will resolve it: {document}"
            );
            assert!(
                document.contains("window.tsjs"),
                "and the reader must still get their bids: {document}"
            );
        }

        #[tokio::test]
        async fn the_seam_carries_slot_definitions_or_the_page_serves_no_ads() {
            // Shared modes suppress the head slot script, so the seam is the only place
            // `tsjs.adSlots` can come from. Sending only bids left it at its `[]` default,
            // `adInit` defined nothing, and the page rendered perfectly with zero TS ads —
            // green tests, healthy-looking page, no revenue.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let miss = body_of(run(&settings, &services, navigation_request()).await).await;
            let hit = body_of(run(&settings, &services, navigation_request()).await).await;

            for (label, body) in [("miss", miss), ("hit", hit)] {
                let text = String::from_utf8(body).expect("utf-8");
                assert!(
                    text.contains("var a=JSON.parse(") && text.contains("s(b,a)"),
                    "{label}: the seam must carry slot definitions and hand them to the \
                     scheduler: {text}"
                );
                assert!(
                    text.contains("test-slot"),
                    "{label}: the slot definitions must be populated, not `[]`"
                );
            }
        }

        #[tokio::test]
        async fn an_assembled_response_is_private_even_when_the_ad_stack_is_off() {
            // The private stamp was gated on `should_run_ad_stack`. A bot, prefetch,
            // kill-switched or consent-denied request can still assemble an empty-bids
            // document, and would have kept the origin's public caching directives — so a
            // downstream cache could serve that to a later eligible reader.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);

            let bot = HttpRequest::builder()
                .method(Method::GET)
                .uri("https://ts.example.com/article")
                .header(header::HOST, "ts.example.com")
                .header("sec-fetch-dest", "document")
                .header("sec-fetch-mode", "navigate")
                .header(
                    header::USER_AGENT,
                    "Googlebot/2.1 (+http://www.google.com/bot.html)",
                )
                .body(EdgeBody::empty())
                .expect("should build bot request");
            let response = run(&settings, &services, bot).await;

            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|v| v.to_str().ok()),
                Some("private, no-store"),
                "an assembled response must be private whatever the ad stack decided"
            );
        }

        #[tokio::test]
        async fn a_cache_hit_keeps_the_origin_security_headers() {
            // Reconstructing headers keeps origin `Set-Cookie` and caching directives out
            // of a shared cache, and also silently dropped Content-Security-Policy —
            // a weaker page, served faster. Policy headers are per-URL, so they belong
            // with the template.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            stub.push_response_with_headers(
                200,
                b"<html><head></head><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                    ("content-security-policy", "default-src 'self'"),
                    ("x-frame-options", "SAMEORIGIN"),
                ],
            );

            let _ = run(&settings, &services, navigation_request()).await;
            let warm = run(&settings, &services, navigation_request()).await;

            assert_eq!(
                warm.headers()
                    .get(header::CONTENT_SECURITY_POLICY)
                    .and_then(|v| v.to_str().ok()),
                Some("default-src 'self'"),
                "a hit must not drop the origin's CSP"
            );
            assert_eq!(
                warm.headers()
                    .get("x-frame-options")
                    .and_then(|v| v.to_str().ok()),
                Some("SAMEORIGIN")
            );
        }

        #[tokio::test]
        async fn a_cache_hit_preserves_every_repeated_policy_header_in_order() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            stub.push_response_with_headers(
                200,
                b"<html><head></head><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                    ("content-security-policy", "default-src 'self'"),
                    ("content-security-policy", "script-src 'self'"),
                    ("link", "</one.js>; rel=preload; as=script"),
                    ("link", "</two.css>; rel=preload; as=style"),
                    ("cross-origin-opener-policy", "same-origin"),
                    ("cross-origin-embedder-policy", "require-corp"),
                ],
            );

            let _ = run(&settings, &services, navigation_request()).await;
            let warm = run(&settings, &services, navigation_request()).await;

            let values = |name: &'static str| {
                warm.headers()
                    .get_all(name)
                    .iter()
                    .map(|value| value.to_str().expect("policy header should be text"))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                values("content-security-policy"),
                ["default-src 'self'", "script-src 'self'"]
            );
            assert_eq!(
                values("link"),
                [
                    "</one.js>; rel=preload; as=script",
                    "</two.css>; rel=preload; as=style"
                ]
            );
            assert_eq!(values("cross-origin-opener-policy"), ["same-origin"]);
            assert_eq!(values("cross-origin-embedder-policy"), ["require-corp"]);
        }

        #[tokio::test]
        async fn nonce_bearing_csp_bypasses_the_shared_template_cache() {
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            for _ in 0..2 {
                stub.push_response_with_headers(
                    200,
                    b"<html><head><script nonce=reader-nonce></script></head><body>origin</body></html>"
                        .to_vec(),
                    vec![
                        ("content-type", "text/html; charset=utf-8"),
                        ("cache-control", "public, max-age=300"),
                        (
                            "content-security-policy",
                            "default-src 'self'; script-src 'nonce-reader-nonce'",
                        ),
                    ],
                );
            }

            let _ = body_of(run(&settings, &services, navigation_request()).await).await;
            let _ = body_of(run(&settings, &services, navigation_request()).await).await;

            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "a response-bound CSP nonce and its HTML must never be reused from C2"
            );
            assert!(
                cache
                    .entries
                    .lock()
                    .expect("should lock entries")
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn a_post_is_never_answered_from_a_cached_get() {
            // `handle_publisher_request` is the `*`-method fallback route, so a publisher
            // path that renders a page on GET and accepts a form or webhook on POST reaches
            // here for both. Serving the cached GET to the POST swallows the mutating
            // request entirely: the origin never sees it, the caller gets 200 and a page,
            // and nothing anywhere reports a problem.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            // Warm the cache with a GET.
            let _ = run(&settings, &services, navigation_request()).await;
            assert_eq!(stub.recorded_request_uris().len(), 1);

            let post = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://ts.example.com/article")
                .header(header::HOST, "ts.example.com")
                .body(EdgeBody::from("field=value"))
                .expect("should build post request");
            let _ = run(&settings, &services, post).await;

            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "the POST must reach the origin rather than being answered from the \
                 cached GET"
            );
            assert_eq!(
                cache.entries.lock().expect("should lock entries").len(),
                1,
                "and it must not store a template of its own"
            );
        }

        #[tokio::test]
        async fn an_authenticated_request_is_not_served_a_shared_template() {
            // The stored template is perfectly cacheable; this request is not entitled
            // to it. The store gate cannot express that, because it is a property of
            // the reader rather than of the bytes — which is why the lookup re-checks.
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let settings = Arc::new(settings_with_mode("esi"));
            let services = services(Arc::clone(&stub), Arc::clone(&cache));
            queue_shareable_html(&stub);
            queue_shareable_html(&stub);

            let _ = run(&settings, &services, navigation_request()).await;
            assert_eq!(
                cache.entries.lock().expect("should lock entries").len(),
                1,
                "the cold request should have populated the cache"
            );

            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let consent = crate::consent::ConsentContext {
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                ..Default::default()
            };
            let mut ec_context = EcContext::new_for_test(None, consent);
            let authenticated = HttpRequest::builder()
                .method(Method::GET)
                .uri("https://ts.example.com/article")
                .header(header::HOST, "ts.example.com")
                .header("sec-fetch-dest", "document")
                .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
                .body(EdgeBody::empty())
                .expect("should build authenticated request");
            let _ = handle_publisher_request(
                &settings,
                &services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &[article_slot()],
                    registry: None,
                },
                authenticated,
                EdgeCacheHeader::SMaxageFallback,
            )
            .await
            .expect("should proxy publisher request");

            assert_eq!(
                stub.recorded_request_uris().len(),
                2,
                "an authenticated request must reach the origin rather than read a \
                 shared template"
            );
        }
    }

    mod c2_gate_tests {
        //! `cache::core` stores whatever it is handed and rejects nothing, so every
        //! one of these conditions is the caller's to enforce. Each is a leak vector
        //! or an eligibility rule, not a preference.

        use super::*;
        use crate::creative_opportunities::AssemblyMode;
        use edgezero_core::http::HeaderName;

        fn headers(pairs: &[(HeaderName, &str)]) -> edgezero_core::http::HeaderMap {
            let mut map = edgezero_core::http::HeaderMap::new();
            for (name, value) in pairs {
                map.insert(
                    name.clone(),
                    HeaderValue::from_str(value).expect("should build header value"),
                );
            }
            map
        }

        fn shareable() -> edgezero_core::http::HeaderMap {
            headers(&[(header::CACHE_CONTROL, "max-age=60")])
        }

        /// The shipped default: no operator has stated what the origin varies on, so the
        /// key covers nothing. Responses without a `Vary` are unaffected; any `Vary` at
        /// all disqualifies.
        fn nothing_covered() -> VarySpec {
            VarySpec::new([])
        }

        #[test]
        fn an_unconfigured_deployment_never_caches_a_varying_response() {
            // The fail-closed default. An operator who has not stated the origin's Vary
            // must not acquire a shared cache by omission — and a real origin varies on
            // something, so this is the common path, not an edge case.
            // Deliberately not `Accept-Encoding`: the shared path normalizes supported
            // content codings to one identity template, so that header is covered
            // whatever the operator configured. Using it here would test the
            // structural-coverage carve-out rather than the drift guard.
            let mut varying = shareable();
            varying.insert(header::VARY, HeaderValue::from_static("rsc"));

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::VaryNotCovered(VaryGap(vec![
                    "rsc".to_string()
                ]))),
                "an unstated Vary must disqualify rather than silently under-key"
            );
        }

        #[test]
        fn an_origin_that_varies_on_cookie_is_refused_even_when_declared_independent() {
            // The backstop that makes `origin_is_cookie_independent` safe to offer. The
            // operator asserts their origin ignores cookies; if the origin then says
            // otherwise, the assertion loses. Without this, a wrong assertion would
            // silently cross-serve personalized HTML.
            let mut varying = shareable();
            varying.insert(header::VARY, HeaderValue::from_static("Cookie"));

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    // The operator's assertion has already been applied here: this is
                    // `false` precisely because they declared independence.
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &VarySpec::new(["cookie".to_string()]),
                ),
                Some(C2BypassReason::VaryCookie),
                "the origin's declaration must override both cookie independence and an \
                 accidentally configured per-cookie key"
            );
        }

        #[test]
        fn a_private_directive_on_a_second_cache_control_line_is_refused() {
            // `HeaderMap::get` returns the first value only. An origin that sends
            // `Cache-Control: public, max-age=300` and then `Cache-Control: private` on a
            // separate line means exactly what one comma-joined line would mean, but the
            // second line was invisible — so a response the origin marked private was
            // written to a cache shared between readers. The `Vary` reads a few lines up
            // already use `get_all` for the same reason.
            let mut split = edgezero_core::http::HeaderMap::new();
            split.append(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300"),
            );
            split.append(header::CACHE_CONTROL, HeaderValue::from_static("private"));

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &split,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::OriginNotShareable),
                "a directive on any Cache-Control line must disqualify the response"
            );
        }

        #[test]
        fn a_no_store_directive_on_a_second_cache_control_line_is_refused() {
            // Same defect, the other directive that matters — `no-store` is the one an
            // origin uses for a response that must not be written down anywhere.
            let mut split = edgezero_core::http::HeaderMap::new();
            split.append(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=60"),
            );
            split.append(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &split,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::OriginNotShareable)
            );
        }

        #[test]
        fn cdn_specific_cache_policy_cannot_be_overridden_by_public_cache_control() {
            for name in crate::response_privacy::CDN_CACHE_HEADERS {
                let mut split = shareable();
                split.insert(
                    header::HeaderName::from_static(name),
                    HeaderValue::from_static("no-store"),
                );

                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &split,
                        &nothing_covered(),
                    ),
                    Some(C2BypassReason::OriginNotShareable),
                    "C2 must fail closed on the CDN-specific policy header {name}"
                );
            }
        }

        #[test]
        fn unsupported_vendor_freshness_does_not_authorize_c2() {
            for name in crate::response_privacy::CDN_CACHE_HEADERS
                .iter()
                .filter(|name| **name != "surrogate-control")
            {
                let mut split = shareable();
                split.insert(
                    header::HeaderName::from_static(name),
                    HeaderValue::from_static("max-age=60"),
                );

                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &split,
                        &nothing_covered(),
                    ),
                    Some(C2BypassReason::OriginNotShareable),
                    "the Fastly exception must not authorize the vendor policy {name}"
                );
            }
        }

        #[test]
        fn observed_fastly_surrogate_policy_uses_edge_freshness_capped_by_configuration() {
            let publisher_headers = headers(&[
                (header::CACHE_CONTROL, "public, max-age=60"),
                (
                    header::HeaderName::from_static("surrogate-control"),
                    "max-age=1200, stale-while-revalidate=21600, stale-if-error=604800",
                ),
            ]);

            assert_eq!(
                c2_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &publisher_headers,
                    &C2CachePolicy::for_test(&nothing_covered(), Duration::from_secs(300),),
                ),
                Ok(Duration::from_secs(300)),
                "Fastly's edge freshness should take precedence over the shorter browser \
                 lifetime, while the configured safety ceiling remains authoritative"
            );
        }

        #[test]
        fn fastly_surrogate_freshness_takes_precedence_over_standard_freshness() {
            for (cache_control, surrogate_control, expected) in [
                ("public, max-age=300", "max-age=30", 30),
                ("public, max-age=30", "max-age=300", 300),
            ] {
                let publisher_headers = headers(&[
                    (header::CACHE_CONTROL, cache_control),
                    (
                        header::HeaderName::from_static("surrogate-control"),
                        surrogate_control,
                    ),
                ]);

                assert_eq!(
                    c2_cache_ttl(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &publisher_headers,
                        &C2CachePolicy::for_test(&nothing_covered(), Duration::from_secs(600),),
                    ),
                    Ok(Duration::from_secs(expected))
                );
            }
        }

        #[test]
        fn surrogate_stale_windows_do_not_extend_fresh_reuse() {
            let publisher_headers = headers(&[
                (header::CACHE_CONTROL, "public, max-age=300"),
                (
                    header::HeaderName::from_static("surrogate-control"),
                    "max-age=20, stale-while-revalidate=600, stale-if-error=1200",
                ),
                (header::AGE, "10"),
            ]);

            assert_eq!(
                c2_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &publisher_headers,
                    &C2CachePolicy::for_test(&nothing_covered(), Duration::from_secs(600),),
                ),
                Ok(Duration::from_secs(10)),
                "stale windows are validated metadata, not fresh C2 lifetime"
            );
        }

        #[test]
        fn ambiguous_or_unsupported_surrogate_policy_fails_closed() {
            for (policy, expected) in [
                ("max-age", C2BypassReason::MalformedCachePolicy),
                (
                    "max-age=30, max-age=60",
                    C2BypassReason::MalformedCachePolicy,
                ),
                ("max-age=tomorrow", C2BypassReason::MalformedCachePolicy),
                ("max-age=30, public", C2BypassReason::MalformedCachePolicy),
                ("stale-if-error=60", C2BypassReason::NoPositiveFreshness),
                ("max-age=0", C2BypassReason::NoPositiveFreshness),
                ("max-age=30,", C2BypassReason::MalformedCachePolicy),
            ] {
                let mut publisher_headers = shareable();
                publisher_headers.insert(
                    header::HeaderName::from_static("surrogate-control"),
                    HeaderValue::from_str(policy).expect("should build Surrogate-Control"),
                );

                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &publisher_headers,
                        &nothing_covered(),
                    ),
                    Some(expected),
                    "`{policy}` must fail closed"
                );
            }
        }

        #[test]
        fn restrictive_surrogate_policy_is_never_overridden_by_standard_freshness() {
            for directive in ["private", "no-store", "no-cache"] {
                let mut publisher_headers = shareable();
                publisher_headers.insert(
                    header::HeaderName::from_static("surrogate-control"),
                    HeaderValue::from_str(directive).expect("should build Surrogate-Control"),
                );

                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &publisher_headers,
                        &nothing_covered(),
                    ),
                    Some(C2BypassReason::OriginNotShareable),
                    "`{directive}` must remain authoritative"
                );
            }
        }

        #[test]
        fn restrictive_standard_policy_is_never_overridden_by_surrogate_freshness() {
            for directive in ["private", "no-store", "no-cache"] {
                let publisher_headers = headers(&[
                    (
                        header::CACHE_CONTROL,
                        &format!("public, max-age=60, {directive}"),
                    ),
                    (
                        header::HeaderName::from_static("surrogate-control"),
                        "max-age=1200",
                    ),
                ]);

                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &publisher_headers,
                        &nothing_covered(),
                    ),
                    Some(C2BypassReason::OriginNotShareable),
                    "standard `{directive}` must refuse C2 even with positive edge freshness"
                );
            }
        }

        #[test]
        fn surrogate_control_can_authorize_fastly_edge_freshness_without_browser_freshness() {
            let publisher_headers = headers(&[(
                header::HeaderName::from_static("surrogate-control"),
                "max-age=1200",
            )]);

            assert_eq!(
                c2_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &publisher_headers,
                    &C2CachePolicy::for_test(&nothing_covered(), Duration::from_secs(300),),
                ),
                Ok(Duration::from_secs(300)),
                "Fastly edge freshness should not require browser freshness"
            );
        }

        #[test]
        fn repeated_cache_control_lines_without_a_disqualifier_still_cache() {
            // The other direction: reading every value must not turn an ordinary
            // multi-line `Cache-Control` into a bypass, or the fix would disable the
            // cache instead of tightening it.
            let mut split = edgezero_core::http::HeaderMap::new();
            split.append(header::CACHE_CONTROL, HeaderValue::from_static("public"));
            split.append(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=60"),
            );

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &split,
                    &nothing_covered(),
                ),
                None
            );
        }

        #[test]
        fn origin_freshness_is_positive_age_adjusted_and_capped() {
            let fresh_headers = headers(&[(header::CACHE_CONTROL, "public, max-age=300")]);
            assert_eq!(
                c2_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &fresh_headers,
                    &C2CachePolicy::for_test(&nothing_covered(), Duration::from_secs(60),),
                ),
                Ok(Duration::from_secs(60))
            );

            let aged = headers(&[
                (header::CACHE_CONTROL, "s-maxage=50, max-age=300"),
                (header::AGE, "35"),
            ]);
            assert_eq!(
                c2_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &aged,
                    &C2CachePolicy::for_test(&nothing_covered(), Duration::from_secs(60),),
                ),
                Ok(Duration::from_secs(15))
            );

            let old_date_without_age = headers(&[
                (header::CACHE_CONTROL, "public, max-age=60"),
                (header::DATE, "Wed, 12 Aug 2026 08:00:00 GMT"),
            ]);
            let one_minute_later = httpdate::parse_http_date("Wed, 12 Aug 2026 08:01:00 GMT")
                .expect("should parse fixture time");
            assert_eq!(
                origin_shared_ttl_at(
                    &old_date_without_age,
                    one_minute_later,
                    Duration::from_secs(60),
                ),
                Err(C2BypassReason::NoPositiveFreshness),
                "an old Date is apparent age even when an upstream omitted Age"
            );
        }

        #[test]
        fn zero_exhausted_missing_and_malformed_freshness_are_refused() {
            for (map, expected) in [
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=0")]),
                    C2BypassReason::NoPositiveFreshness,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=60"), (header::AGE, "60")]),
                    C2BypassReason::NoPositiveFreshness,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "public")]),
                    C2BypassReason::NoPositiveFreshness,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=tomorrow")]),
                    C2BypassReason::MalformedCachePolicy,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=\"60")]),
                    C2BypassReason::MalformedCachePolicy,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=+60")]),
                    C2BypassReason::MalformedCachePolicy,
                ),
            ] {
                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &map,
                        &nothing_covered(),
                    ),
                    Some(expected)
                );
            }
        }

        #[test]
        fn expires_can_authorize_but_never_extend_an_expired_response() {
            let now = httpdate::parse_http_date("Wed, 12 Aug 2026 08:00:00 GMT")
                .expect("should parse fixture time");
            let fresh = headers(&[
                (header::DATE, "Wed, 12 Aug 2026 08:00:00 GMT"),
                (header::EXPIRES, "Wed, 12 Aug 2026 08:00:30 GMT"),
            ]);
            assert_eq!(
                origin_shared_ttl_at(&fresh, now, Duration::from_secs(60)),
                Ok(Duration::from_secs(30))
            );

            let expired = headers(&[
                (header::DATE, "Wed, 12 Aug 2026 08:01:00 GMT"),
                (header::EXPIRES, "Wed, 12 Aug 2026 08:00:30 GMT"),
            ]);
            assert_eq!(
                origin_shared_ttl_at(&expired, now, Duration::from_secs(60)),
                Err(C2BypassReason::NoPositiveFreshness)
            );
        }

        #[test]
        fn request_semantics_bypass_c2_except_for_a_max_age_zero_reload() {
            for (name, value) in [
                (header::CACHE_CONTROL, "no-cache"),
                (header::CACHE_CONTROL, "max-age=30"),
                (header::CACHE_CONTROL, "max-age=\"0"),
                (header::CACHE_CONTROL, "min-fresh=10"),
                (header::CACHE_CONTROL, "no-store"),
                (header::PRAGMA, "no-cache"),
                (header::PRAGMA, "legacy-extension, no-cache"),
                (header::RANGE, "bytes=0-99"),
                (header::IF_NONE_MATCH, "\"etag\""),
                (header::IF_MODIFIED_SINCE, "Wed, 12 Aug 2026 08:00:00 GMT"),
            ] {
                let map = headers(&[(name.clone(), value)]);
                assert!(request_bypasses_c2(&map), "{name}: {value} must bypass");
            }
            assert!(
                !request_bypasses_c2(&headers(&[(header::CACHE_CONTROL, "max-age=0")])),
                "a browser reload may reuse C2 because the assembled response and auction \
                 are still rebuilt for this reader"
            );
            assert!(!request_bypasses_c2(&headers(&[(
                header::CACHE_CONTROL,
                "public"
            )])));
        }

        #[test]
        fn a_wildcard_vary_is_refused() {
            // `VarySpec::uncovered_by` filters `*` out, with a comment saying the
            // eligibility gate handles it. It did not — nothing rejected the wildcard, so
            // a response the origin said no key can select was shareable.
            let mut varying = shareable();
            varying.insert(header::VARY, HeaderValue::from_static("*"));

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::VaryWildcard)
            );
        }

        #[test]
        fn a_fully_covered_vary_is_cacheable() {
            let mut varying = shareable();
            varying.insert(
                header::VARY,
                HeaderValue::from_static("rsc, Accept-Encoding"),
            );

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &VarySpec::new(["rsc".to_string(), "accept-encoding".to_string()]),
                ),
                None,
                "a key covering everything the origin varies on is safe to store"
            );
        }

        #[test]
        fn config_drift_names_the_missing_header() {
            // The failure this guards: the origin adds a header to its Vary, nobody
            // updates config, and requests differing only in that header start sharing a
            // template. The reason must name it, or diagnosing means a bisect.
            let mut varying = shareable();
            varying.insert(
                header::VARY,
                HeaderValue::from_static("rsc, next-router-prefetch"),
            );

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &VarySpec::new(["rsc".to_string()]),
                ),
                Some(C2BypassReason::VaryNotCovered(VaryGap(vec![
                    "next-router-prefetch".to_string()
                ]))),
                "the uncovered header must be named"
            );
        }

        #[test]
        fn a_vary_split_across_repeated_headers_is_still_checked() {
            // Vary is a list header, so an origin may send it once or many times. Reading
            // only the first would let the rest through unkeyed.
            let mut varying = shareable();
            varying.append(header::VARY, HeaderValue::from_static("rsc"));
            varying.append(header::VARY, HeaderValue::from_static("cookie"));

            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &VarySpec::new(["rsc".to_string()]),
                ),
                Some(C2BypassReason::VaryCookie),
                "a repeated Vary header must not hide names behind the first value"
            );
        }

        #[test]
        fn a_plain_shareable_html_200_is_cacheable() {
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &shareable(),
                    &nothing_covered(),
                ),
                None,
                "ESI shareable HTML 200 should be eligible"
            );
        }

        #[test]
        fn inline_mode_never_writes_a_template() {
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Inline,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &shareable(),
                    &nothing_covered(),
                ),
                Some(C2BypassReason::InlineMode),
                "inline has no shared template to write"
            );
        }

        #[test]
        fn an_authorized_request_is_never_cached() {
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    true,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &shareable(),
                    &nothing_covered(),
                ),
                Some(C2BypassReason::AuthorizedRequest),
                "an authenticated response must not enter a shared cache"
            );
        }

        #[test]
        fn a_forwarded_request_cookie_disqualifies_even_without_set_cookie() {
            // The dangerous case: session established on an earlier request, so this
            // response carries no Set-Cookie, has no Cache-Control at all, is a 200,
            // and is HTML — yet is personalized because TS forwarded the Cookie to
            // origin unchanged. Every other condition reports it cacheable.
            let no_cache_control = edgezero_core::http::HeaderMap::new();
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    true,
                    StatusCode::OK,
                    "text/html",
                    &no_cache_control,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::CookieForwarded),
                "cookie-personalized HTML must not become a shared template"
            );
        }

        #[test]
        fn an_origin_set_cookie_is_never_cached() {
            let with_cookie = headers(&[
                (header::CACHE_CONTROL, "max-age=60"),
                (header::SET_COOKIE, "sid=abc; Path=/"),
            ]);
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &with_cookie,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::OriginSetCookie),
                "caching this would replay one visitor's cookie to the next"
            );
        }

        #[test]
        fn non_shareable_cache_control_is_refused_case_insensitively() {
            for directive in [
                "private",
                "no-store",
                "no-cache",
                "Private, max-age=60",
                "NO-STORE",
                "public, No-Cache",
            ] {
                let map = headers(&[(header::CACHE_CONTROL, directive)]);
                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &map,
                        &nothing_covered(),
                    ),
                    Some(C2BypassReason::OriginNotShareable),
                    "`{directive}` should disqualify the response"
                );
            }
        }

        #[test]
        fn a_datadome_block_is_refused_by_the_status_check() {
            // DataDome replaces the document with a 403
            // (`integrations/datadome/protection.rs:778`). There is no separate
            // marker to detect, and none is needed.
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::FORBIDDEN,
                    "text/html",
                    &shareable(),
                    &nothing_covered(),
                ),
                Some(C2BypassReason::NonOkStatus),
                "a blocked document must not become the shared template"
            );
        }

        #[test]
        fn non_html_is_refused() {
            for content_type in ["text/x-component", "application/json", ""] {
                assert_eq!(
                    c2_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        content_type,
                        &shareable(),
                        &nothing_covered(),
                    ),
                    Some(C2BypassReason::NotHtml),
                    "`{content_type}` has no HTML template to transform"
                );
            }
        }

        #[test]
        fn unsupported_content_encoding_is_refused_before_representation_headers_change() {
            let map = headers(&[
                (header::CACHE_CONTROL, "public, max-age=60"),
                (header::CONTENT_ENCODING, "zstd"),
            ]);
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &map,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::UnsupportedContentEncoding)
            );

            let mut repeated = headers(&[(header::CACHE_CONTROL, "public, max-age=60")]);
            repeated.append(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
            repeated.append(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &repeated,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::MalformedRepresentationHeaders)
            );
        }

        #[test]
        fn leak_vectors_are_reported_before_mere_ineligibility() {
            // A response that fails several conditions should name the most serious
            // one, so an operator reading the log sees the security reason rather
            // than a content-type quibble.
            let map = headers(&[
                (header::CACHE_CONTROL, "private"),
                (header::SET_COOKIE, "sid=abc"),
            ]);
            assert_eq!(
                c2_bypass_reason(
                    AssemblyMode::Esi,
                    true,
                    false,
                    StatusCode::FORBIDDEN,
                    "application/json",
                    &map,
                    &nothing_covered(),
                ),
                Some(C2BypassReason::AuthorizedRequest),
                "authorization is the most serious disqualifier and should win"
            );
        }
    }

    mod template_neutrality_tests {
        //! The gate for #1009's shared-template design.
        //!
        //! An "absence of per-user values" scan is not sufficient here: the bug
        //! that nearly shipped was a *conditionally present* element whose own
        //! content was per-URL. These tests assert byte-identity across requests
        //! that differ only in the gating decision.

        use super::*;
        use crate::creative_opportunities::{
            AssemblyMode, CreativeOpportunityFormat, CreativeOpportunitySlot,
        };

        pub(super) fn slot() -> CreativeOpportunitySlot {
            CreativeOpportunitySlot {
                id: "atf".to_string(),
                gam_unit_path: Some("/99999/example/home".to_string()),
                div_id: Some("ad-atf".to_string()),
                page_patterns: vec!["/**".to_string()],
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

        pub(super) fn settings_with_slots() -> Settings {
            let mut settings = crate::test_support::tests::create_test_settings();
            // Construct the section rather than mutating it if present: the shared
            // fixture does not carry `[creative_opportunities]`, and an `if let
            // Some(..)` here would silently no-op and make the inline assertion
            // below vacuous.
            settings.creative_opportunities = Some(CreativeOpportunitiesConfig {
                enabled: true,
                gam_network_id: "99999".to_string(),
                auction_timeout_ms: Some(500),
                price_granularity: Default::default(),
                section_root: None,
                assembly_mode: None,
                template_cache_vary: None,
                template_cache_max_age_seconds: None,
                origin_is_cookie_independent: None,
                section_segment: None,
                slot: vec![slot()],
            });
            settings
        }

        #[test]
        fn shared_modes_emit_no_head_script_regardless_of_the_gating_decision() {
            let settings = settings_with_slots();
            let slots = [slot()];
            let mode = AssemblyMode::Esi;
            let ran = template_ad_slots_script(mode, true, &settings, &slots, "/");
            let did_not_run = template_ad_slots_script(mode, false, &settings, &slots, "/");

            assert_eq!(
                ran, did_not_run,
                "{mode:?}: the template must be byte-identical whether or not the ad \
                 stack ran; a cached object cannot carry one request's consent, bot, \
                 prefetch or kill-switch decision"
            );
            assert_eq!(
                ran, None,
                "{mode:?}: adSlots belongs in the per-request seam, not the template"
            );
        }

        #[test]
        fn inline_mode_keeps_its_request_dependent_behaviour() {
            // Inline responses are per-navigation and never shared, so gating is
            // correct there. This guards against "fixing" the shared-mode bug by
            // breaking the shipped path.
            let settings = settings_with_slots();
            let slots = [slot()];

            assert!(
                template_ad_slots_script(AssemblyMode::Inline, true, &settings, &slots, "/")
                    .is_some(),
                "inline should emit adSlots when the ad stack runs"
            );
            assert_eq!(
                template_ad_slots_script(AssemblyMode::Inline, false, &settings, &slots, "/"),
                None,
                "inline should emit nothing when the ad stack does not run"
            );
        }

        #[test]
        fn shared_modes_are_neutral_across_differing_slot_matches() {
            // Slot matching folds in the request path. Under a shared mode even
            // that must not reach the template.
            let settings = settings_with_slots();

            let matched = template_ad_slots_script(
                AssemblyMode::Esi,
                true,
                &settings,
                &[slot()],
                "/news/article",
            );
            let unmatched =
                template_ad_slots_script(AssemblyMode::Esi, true, &settings, &[], "/other");

            assert_eq!(
                matched, unmatched,
                "the template must not vary with slot matching under a shared mode"
            );
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

        fn settings_with_disabled_ad_templates() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n\
                 [creative_opportunities]\nenabled = false\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with disabled ad templates")
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

        fn queue_html_response_with_cache_control(
            stub: &StubHttpClient,
            cache_control: &'static str,
        ) {
            stub.push_response_with_headers(
                200,
                b"<html><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", cache_control),
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
                EdgeCacheHeader::SMaxageFallback,
            )
            .await
            .expect("should proxy publisher request")
        }

        fn response_head(response: PublisherResponse) -> http::response::Parts {
            match response {
                PublisherResponse::Buffered(response)
                | PublisherResponse::Stream { response, .. }
                | PublisherResponse::AssembleTemplate { response, .. }
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
            queue_html_response_with_cache_control(&stub, "public, max-age=300");
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
            queue_html_response_with_cache_control(&stub, "public, max-age=300");
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
        async fn disabled_ad_templates_use_short_browser_cache_policy() {
            // Arrange
            let settings = settings_with_disabled_ad_templates();
            let stub = Arc::new(StubHttpClient::new());
            queue_html_response_with_cache_control(&stub, "public, max-age=300");
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let slots = [article_slot()];

            // Act
            let response = run_with_slots(
                &settings,
                &services,
                &slots,
                conditional_navigation_request(),
            )
            .await;
            let response_head = response_head(response);

            // Assert
            assert_eq!(
                stub.recorded_cache_bypass_flags(),
                vec![false],
                "disabled server-side ad templates should not bypass the origin cache"
            );
            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("max-age=60"),
                "disabled server-side ad templates should use the short browser cache policy"
            );
            for (header_name, expected) in [
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
                    "disabled server-side ad templates should preserve {header_name}"
                );
            }
        }

        #[tokio::test]
        async fn navigation_without_matched_slots_preserves_private_origin_cache_policy() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();

            for cache_control in ["private, max-age=0", "No-Store"] {
                // Arrange
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_cache_control(&stub, cache_control);
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                // Act
                let response =
                    run_with_slots(&settings, &services, &[], conditional_navigation_request())
                        .await;
                let response_head = response_head(response);

                // Assert
                assert_eq!(
                    response_head
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some(cache_control),
                    "origin {cache_control} policy should not be weakened"
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
                    PublisherResponse::PassThrough { .. }
                    | PublisherResponse::Stream { .. }
                    | PublisherResponse::AssembleTemplate { .. } => {
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
                PublisherResponse::PassThrough { .. }
                | PublisherResponse::Stream { .. }
                | PublisherResponse::AssembleTemplate { .. } => {
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
    async fn publisher_asset_cache_policy_applies_to_non_html_response() {
        let settings = Settings::from_toml(&format!(
            r#"{}

            [[cache.asset_rules]]
            id = "publisher-fingerprinted-assets"
            enabled = true
            path_globs = ["/assets/**/*.png"]
            fingerprint_style = "hex"
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
        let request = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/assets/logo.0123abcd.png")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = run_publisher_proxy(&settings, &services, request).await;
        let PublisherResponse::PassThrough { response, .. } = response else {
            panic!("should pass through non-HTML asset response");
        };

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=31536000, s-maxage=31536000, immutable"),
            "matched publisher asset should receive immutable browser and edge policy"
        );
        assert_eq!(
            response
                .headers()
                .get("surrogate-control")
                .and_then(|value| value.to_str().ok()),
            None,
            "S-maxage fallback should keep the edge TTL in Cache-Control"
        );
    }

    #[tokio::test]
    async fn publisher_asset_policy_response_with_cookie_is_private_after_finalization() {
        let settings = Settings::from_toml(&format!(
            r#"{}

            [[cache.asset_rules]]
            id = "publisher-fingerprinted-assets"
            enabled = true
            path_globs = ["/assets/**/*.png"]
            fingerprint_style = "hex"
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
                (header::SET_COOKIE.as_str(), "viewer=example; Path=/"),
            ],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let request = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/assets/logo.0123abcd.png")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = run_publisher_proxy(&settings, &services, request).await;
        let PublisherResponse::PassThrough { mut response, .. } = response else {
            panic!("should pass through non-HTML asset response");
        };
        crate::response_privacy::apply_response_headers_with_cache_privacy(
            &settings,
            &mut response,
        );

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=0"),
            "publisher asset with Set-Cookie must become private after finalization"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "publisher asset with Set-Cookie must not retain a shared-cache header"
        );
    }

    #[tokio::test]
    async fn publisher_asset_cache_policy_skips_html_response() {
        let settings = Settings::from_toml(&format!(
            r#"{}

            [[cache.asset_rules]]
            id = "broad-publisher-path"
            enabled = true
            path_glob = "/news/*.html"
            visibility = "public"
            browser_ttl_seconds = 31536000
            edge_ttl_seconds = 31536000
            immutable = true
            fingerprint_style = "hex"
        "#,
            crate_test_settings_str()
        ))
        .expect("should parse settings with cache rule");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><body>news</body></html>".to_vec(),
            vec![
                (header::CONTENT_TYPE.as_str(), "text/html; charset=utf-8"),
                (header::CACHE_CONTROL.as_str(), "public, max-age=60"),
            ],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let request = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/news/story.0123abcd.html")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = run_publisher_proxy(&settings, &services, request).await;
        let response = match response {
            PublisherResponse::Stream { response, .. } | PublisherResponse::Buffered(response) => {
                response
            }
            PublisherResponse::PassThrough { .. } | PublisherResponse::AssembleTemplate { .. } => {
                panic!("should classify HTML response for processing")
            }
        };

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("max-age=60"),
            "asset policy must not apply shared asset caching to publisher HTML"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "HTML response must not receive a shared-cache header"
        );
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
            PublisherResponse::Stream { response, .. }
            | PublisherResponse::AssembleTemplate { response, .. } => response,
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
    async fn suppressed_navigation_removes_conditional_and_range_headers() {
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
            .header("sec-fetch-dest", "document")
            .header(header::IF_NONE_MATCH, "\"cached-page\"")
            .header(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2015 07:28:00 GMT")
            .header(header::RANGE, "bytes=0-18")
            .header(header::IF_RANGE, "\"cached-page\"")
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
        for header_name in [
            header::IF_NONE_MATCH,
            header::IF_MODIFIED_SINCE,
            header::RANGE,
            header::IF_RANGE,
        ] {
            assert!(
                headers
                    .iter()
                    .all(|(name, _)| !name.eq_ignore_ascii_case(header_name.as_str())),
                "suppressed navigations must not forward {header_name}"
            );
        }
    }

    #[tokio::test]
    async fn suppressed_iframe_removes_conditional_and_range_headers() {
        let settings = create_test_settings();
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><body>frame</body></html>".to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let mut req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/frame")
            .header(header::HOST, "publisher.example")
            .header("sec-fetch-dest", "iframe")
            .header(header::IF_NONE_MATCH, "\"cached-frame\"")
            .header(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2015 07:28:00 GMT")
            .header(header::RANGE, "bytes=0-18")
            .header(header::IF_RANGE, "\"cached-frame\"")
            .body(EdgeBody::empty())
            .expect("should build conditional iframe request");
        req.extensions_mut()
            .insert(crate::integrations::datadome::DataDomeClientTagSuppressed);

        let _response = run_publisher_proxy(&settings, &services, req).await;

        let headers = stub
            .recorded_request_headers()
            .into_iter()
            .next()
            .expect("should record one outbound request");
        for header_name in [
            header::IF_NONE_MATCH,
            header::IF_MODIFIED_SINCE,
            header::RANGE,
            header::IF_RANGE,
        ] {
            assert!(
                headers
                    .iter()
                    .all(|(name, _)| !name.eq_ignore_ascii_case(header_name.as_str())),
                "suppressed iframe documents must not forward {header_name}"
            );
        }
    }

    #[tokio::test]
    async fn suppressed_subresource_preserves_conditional_and_range_headers() {
        let settings = create_test_settings();
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"video".to_vec(),
            vec![("content-type", "video/mp4")],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let mut req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/video.mp4")
            .header(header::HOST, "publisher.example")
            .header("sec-fetch-dest", "video")
            .header(header::IF_NONE_MATCH, "\"cached-video\"")
            .header(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2015 07:28:00 GMT")
            .header(header::RANGE, "bytes=0-18")
            .header(header::IF_RANGE, "\"cached-video\"")
            .body(EdgeBody::empty())
            .expect("should build conditional subresource request");
        req.extensions_mut()
            .insert(crate::integrations::datadome::DataDomeClientTagSuppressed);

        let _response = run_publisher_proxy(&settings, &services, req).await;

        let headers = stub
            .recorded_request_headers()
            .into_iter()
            .next()
            .expect("should record one outbound request");
        for (header_name, expected) in [
            (header::IF_NONE_MATCH, "\"cached-video\""),
            (header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2015 07:28:00 GMT"),
            (header::RANGE, "bytes=0-18"),
            (header::IF_RANGE, "\"cached-video\""),
        ] {
            assert_eq!(
                headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(header_name.as_str()))
                    .map(|(_, value)| value.as_str()),
                Some(expected),
                "suppressed subresources should preserve {header_name}"
            );
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
            EdgeCacheHeader::SMaxageFallback,
        )
        .await
        .expect("should proxy publisher request");

        assert_eq!(
            ec_context.ec_value(),
            None,
            "handler must not self-generate an EC ID; generation is the adapter's real-browser-gated responsibility",
        );
    }

    #[tokio::test]
    async fn datadome_filter_marker_survives_into_publisher_html_pipeline() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "datadome",
                &serde_json::json!({
                    "enabled": true,
                    "enable_protection": true,
                    "protection_excluded_ip_cidrs": ["192.0.2.0/24"],
                    "client_side_key": "test-client-key",
                }),
            )
            .expect("should configure DataDome integration");
        let registry = IntegrationRegistry::new(&settings)
            .expect("should create integration registry with DataDome");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><head></head><body>content</body></html>".to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
        let services = build_services_with_secret_http_client_and_client_ip(
            NoopSecretStore,
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            Some("192.0.2.10".parse().expect("should parse client IP")),
        );
        let mut req = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/page")
            .header(header::HOST, "publisher.example")
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build request");

        let filter_outcome = registry
            .filter_request(crate::integrations::RequestFilterRegistryInput {
                settings: &settings,
                services: &services,
                req: &mut req,
                geo_info: None,
            })
            .await
            .expect("should run DataDome filter");
        assert!(matches!(
            filter_outcome,
            crate::integrations::RequestFilterRegistryOutcome::Continue(_)
        ));
        let publisher_response = run_publisher_proxy(&settings, &services, req).await;
        let response = buffer_publisher_response_async(
            publisher_response,
            &Method::GET,
            &settings,
            &registry,
            &AuctionOrchestrator::new(settings.auction.clone()),
            &services,
        )
        .await
        .expect("should buffer publisher response");
        let html = response_body_string(response);

        assert!(!html.contains("window.ddjskey"));
        assert!(!html.contains("/integrations/datadome/tags.js"));
        assert_eq!(
            stub.recorded_backend_names().len(),
            1,
            "only the publisher origin should be called"
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
            .header("cloudflare-cdn-cache-control", "max-age=600")
            .header("cdn-cache-control", "max-age=600")
            .header(header::ETAG, "\"origin-tag\"")
            .header(header::LAST_MODIFIED, "Wed, 21 Oct 2015 07:28:00 GMT")
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
            Some("private, no-store"),
            "suppressed HTML should be private and non-storable"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "suppressed HTML should not retain Surrogate-Control"
        );
        assert!(
            response.headers().get("fastly-surrogate-control").is_none(),
            "suppressed HTML should not retain Fastly-Surrogate-Control"
        );
        assert!(
            response
                .headers()
                .get("cloudflare-cdn-cache-control")
                .is_none(),
            "suppressed HTML should not retain Cloudflare-CDN-Cache-Control"
        );
        assert!(
            response.headers().get("cdn-cache-control").is_none(),
            "suppressed HTML should not retain CDN-Cache-Control"
        );
        for header_name in [header::ETAG, header::LAST_MODIFIED] {
            assert!(
                !response.headers().contains_key(&header_name),
                "suppressed HTML should not retain {header_name}"
            );
        }

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
            no_store_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, no-store"),
            "suppressed HTML should use the exact synthesized-HTML policy"
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
        let ad_bids_state = AdBidsState::default();
        let ctx = AuctionCollectCtx {
            dispatched,
            telemetry: AuctionTelemetryCarry {
                observation: None,
                auction_request: None,
            },
            deps: AuctionCollectDeps {
                price_granularity: PriceGranularity::default(),
                ad_bids_state: &ad_bids_state,
                orchestrator: &orchestrator,
                services: &services,
                settings: &settings,
                request_origin: String::new(),
            },
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

    #[tokio::test]
    async fn hold_step_yields_ready_prefix_before_collecting_auction() {
        // A small page whose `</body>` lands in the first source chunk must
        // still stream its document prefix immediately. `hold_step_decoded_chunk`
        // reports the ready prefix and `close_found` without collecting; only
        // `hold_collect_close_tail` awaits collection. Regression guard for the
        // #849 FCP objective: the prefix must become ready while collection
        // remains pending.
        let settings = create_test_settings();
        let services = noop_services();
        let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        let ad_bids_state = AdBidsState::default();
        let mut state = AuctionHoldState::new(
            DispatchedAuctionGuard::new(DispatchedAuction::empty_for_test(
                test_auction_request(),
                500,
            )),
            AuctionTelemetryCarry {
                observation: None,
                auction_request: None,
            },
        );
        let collect_refs = AuctionCollectDeps {
            price_granularity: PriceGranularity::default(),
            ad_bids_state: &ad_bids_state,
            orchestrator: &orchestrator,
            services: &services,
            settings: &settings,
            request_origin: String::new(),
        };
        // Passthrough processor: the ordering contract is about collection, not
        // HTML rewriting, so keep the emitted bytes verbatim.
        let mut processor = RecordingProcessor {
            read_count: Arc::new(AtomicUsize::new(0)),
            body_close_processed_at: Arc::new(AtomicUsize::new(0)),
        };
        let mut encoder = BodyStreamEncoder::new(Compression::None);

        let step = hold_step_decoded_chunk(
            &mut processor,
            &mut encoder,
            b"<html><body>painted</body></html>",
            &mut state,
            &collect_refs,
        )
        .await
        .expect("hold step should succeed");

        assert!(
            step.close_found,
            "</body> in the first chunk must be detected"
        );
        let ready: Vec<u8> = step.ready.iter().flat_map(|b| b.to_vec()).collect();
        assert_eq!(
            std::str::from_utf8(&ready).expect("ready prefix should be utf8"),
            "<html><body>painted",
            "the prefix up to </body> must be ready before collection"
        );
        assert!(
            ad_bids_state
                .script_cell()
                .lock()
                .expect("should lock bid state")
                .is_none(),
            "auction must not be collected while the ready prefix is emitted"
        );

        let tail = hold_collect_close_tail(&mut processor, &mut encoder, &mut state, &collect_refs)
            .await
            .expect("collect should succeed");
        let tail_bytes: Vec<u8> = tail.iter().flat_map(|b| b.to_vec()).collect();
        assert_eq!(
            std::str::from_utf8(&tail_bytes).expect("held tail should be utf8"),
            "</body></html>",
            "the held close tail must be emitted after collection"
        );
        assert!(
            ad_bids_state
                .script_cell()
                .lock()
                .expect("should lock bid state")
                .is_some(),
            "collection must run when the held tail is emitted"
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
    fn esi_reader_encoding_negotiation_honours_quality_identity_and_repeated_fields() {
        let headers = |values: &[&str]| {
            let mut headers = edgezero_core::http::HeaderMap::new();
            for value in values {
                headers.append(
                    header::ACCEPT_ENCODING,
                    HeaderValue::from_str(value).expect("should build accept-encoding"),
                );
            }
            headers
        };

        assert_eq!(
            negotiate_reader_compression(&headers(&[])),
            Ok(Compression::None)
        );
        assert_eq!(
            negotiate_reader_compression(&headers(&["gzip;q=0.8", "br;q=0.4, identity;q=0.1"])),
            Ok(Compression::Gzip)
        );
        assert_eq!(
            negotiate_reader_compression(&headers(&["gzip, br"])),
            Ok(Compression::Brotli),
            "server preference breaks an equal-quality tie"
        );
        assert_eq!(
            negotiate_reader_compression(&headers(&["gzip;q=0.5"])),
            Ok(Compression::None),
            "implicit identity has q=1"
        );
        assert_eq!(
            negotiate_reader_compression(&headers(&["zstd, identity;q=0"])),
            Err(ReaderEncodingError::NoAcceptableEncoding)
        );
        assert_eq!(
            negotiate_reader_compression(&headers(&["gzip;q=invalid"])),
            Err(ReaderEncodingError::Malformed)
        );
        for malformed in [
            "gzip;q=1e-1",
            "gzip;q=0.1234",
            "gzip;q=1.001",
            "not a coding;q=1",
        ] {
            assert_eq!(
                negotiate_reader_compression(&headers(&[malformed])),
                Err(ReaderEncodingError::Malformed),
                "{malformed} is not valid Accept-Encoding syntax"
            );
        }
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

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
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

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn tsjs_dynamic_serves_diagnostics_standalone_without_cookie_variance() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let mut req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-gpt_diagnostics.min.js",
        );
        req.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-console=1"),
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        assert!(
            !response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("private") || value.contains("no-store")),
            "standalone module should remain cookie-independent and publicly cacheable"
        );
    }

    #[test]
    fn parse_single_module_filename_extracts_known_id() {
        assert_eq!(
            parse_single_module_filename("tsjs-sourcepoint.min.js"),
            Some("sourcepoint"),
            "should extract sourcepoint from minified filename"
        );
        assert_eq!(
            parse_single_module_filename("tsjs-sourcepoint.js"),
            Some("sourcepoint"),
            "should extract sourcepoint from unminified filename"
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
            Some("core"),
            "should accept any known module ID (deferred check happens in caller)"
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
    fn tsjs_dynamic_serves_prebid_shim_when_enabled() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let req = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-prebid.min.js",
        );

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should serve the deferred prebid shim module when prebid is enabled"
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

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
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

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should reject unknown module names"
        );
    }

    #[test]
    fn tsjs_dynamic_uses_immutable_cache_for_matching_hash() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let module_ids = registry.js_module_ids_immediate();
        let hash = trusted_server_js::concatenated_hash(&module_ids);
        let request = build_request(
            Method::GET,
            &format!("https://publisher.example/static/tsjs=tsjs-unified.min.js?v={hash}"),
        );

        let response = handle_tsjs_dynamic(&request, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=31536000, immutable"),
            "matching content-versioned bundle should be immutable"
        );
        assert_eq!(
            response
                .headers()
                .get("surrogate-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000"),
            "matching content-versioned bundle should set the Fastly edge TTL"
        );
    }

    #[test]
    fn tsjs_dynamic_uses_cloudflare_edge_header_when_selected() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let module_ids = registry.js_module_ids_immediate();
        let hash = trusted_server_js::concatenated_hash(&module_ids);
        let request = build_request(
            Method::GET,
            &format!("https://publisher.example/static/tsjs=tsjs-unified.min.js?v={hash}"),
        );

        let response = handle_tsjs_dynamic(
            &request,
            &registry,
            EdgeCacheHeader::CloudflareCdnCacheControl,
        )
        .expect("should handle tsjs request");

        assert_eq!(
            response
                .headers()
                .get("cloudflare-cdn-cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000"),
            "Cloudflare requests should use its edge cache header"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "Cloudflare requests should not emit Fastly's edge cache header"
        );
    }

    #[test]
    fn tsjs_dynamic_keeps_short_cache_for_mismatched_hash() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let request = build_request(
            Method::GET,
            "https://publisher.example/static/tsjs=tsjs-unified.min.js?v=not-the-hash",
        );

        let response = handle_tsjs_dynamic(&request, &registry, EdgeCacheHeader::SurrogateControl)
            .expect("should handle tsjs request");
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .expect("should set cache-control");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            cache_control.contains("max-age=300") && !cache_control.contains("immutable"),
            "mismatched hash should retain the short, mutable cache policy"
        );
        assert_eq!(
            response
                .headers()
                .get("surrogate-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "mismatched hash should retain the short Fastly edge TTL"
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: "gzip".to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/css".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
                content_encoding: String::new(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/css".to_string(),
                ad_slots_script: None,
                ad_bids_state: AdBidsState::default(),
                auction_observation: None,
                auction_request: None,
                dispatched_auction: None,
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
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
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
                content_encoding: "gzip".to_string(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/css".to_string(),
                ad_slots_script: None,
                ad_bids_state: AdBidsState::default(),
                auction_observation: None,
                auction_request: None,
                dispatched_auction: None,
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
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
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
                content_encoding: "deflate".to_string(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/css".to_string(),
                ad_slots_script: None,
                ad_bids_state: AdBidsState::default(),
                auction_observation: None,
                auction_request: None,
                dispatched_auction: None,
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
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
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
                content_encoding: "br".to_string(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/css".to_string(),
                ad_slots_script: None,
                ad_bids_state: AdBidsState::default(),
                auction_observation: None,
                auction_request: None,
                dispatched_auction: None,
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
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
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
                content_encoding: "br".to_string(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/css".to_string(),
                ad_slots_script: None,
                ad_bids_state: AdBidsState::default(),
                auction_observation: None,
                auction_request: None,
                dispatched_auction: None,
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: content_encoding.to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/css".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
    fn stream_publisher_body_async_processes_stream_with_auction_hold() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let state = AdBidsState::default();
            let mut params = OwnedProcessResponseParams {
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
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
                gpt_diagnostics: None,
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
                html.contains(".adSlots=JSON.parse"),
                "should still inject ad slots. Got: {html}"
            );
            assert!(
                html.contains("var b=JSON.parse("),
                "should collect auction and inject bids before body close. Got: {html}"
            );
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
            let state = AdBidsState::default();
            let mut params = OwnedProcessResponseParams {
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
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
                suppress_datadome_client_side_tag: false,
            };
            // The `</body>` that triggers bid injection lives in the SECOND gzip
            // member. `flate2::read::GzDecoder` decodes only the first member, so
            // this buffered `Body::Once` body (the non-stream auction arm) proves
            // the multi-member decoder now reads every member.
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
                html.contains("var b=JSON.parse("),
                "should inject bids before the </body> carried in the second member. Got: {html}"
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
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
                content_encoding: String::new(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/css".to_string(),
                ad_slots_script: None,
                ad_bids_state: AdBidsState::default(),
                auction_observation: None,
                auction_request: Some(test_auction_request()),
                dispatched_auction: Some(DispatchedAuction::empty_for_test(
                    test_auction_request(),
                    10,
                )),
                price_granularity: crate::price_bucket::PriceGranularity::default(),
                gpt_diagnostics: None,
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: content_encoding.to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/css".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
        streaming_finalize_response_with_settings(params, body, create_test_settings())
    }

    fn streaming_finalize_response_with_settings(
        params: OwnedProcessResponseParams,
        body: EdgeBody,
        settings: Settings,
    ) -> EdgeBody {
        let settings = Arc::new(settings);
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
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
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: dispatched_auction.as_ref().map(|_| test_auction_request()),
            dispatched_auction,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
        }
    }

    #[test]
    fn streaming_finalize_emits_gam_attribution_head_before_origin_eof() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "gpt",
                &serde_json::json!({
                    "enabled": true,
                    "gam_attribution_enabled": true
                }),
            )
            .expect("should insert GPT config");

        let body = streaming_finalize_response_with_settings(
            html_stream_params("", None),
            origin_chunk_then_pending(bytes::Bytes::from_static(
                b"<html><head></head><body><p>origin remains pending</p>",
            )),
            settings,
        );
        let html = String::from_utf8(first_lazy_body_chunk(body).to_vec())
            .expect("should emit UTF-8 HTML");

        assert!(
            html.contains("__tsjs_gam_attribution_enabled=true"),
            "first rewritten head chunk should carry the primary activation flag: {html}"
        );
        assert!(
            html.contains("data-ts-gam-attribution=\"true\""),
            "first rewritten head chunk should authorize the bundle fallback: {html}"
        );
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
    fn streaming_finalize_auction_hold_emits_prefix_before_origin_eof() {
        // The auction-hold path must stream the document prefix (up to the held
        // `</body>` tail) before the origin finishes and before the auction is
        // collected — otherwise the hold reintroduces the FCP regression. The
        // origin sends the head/body prefix (no `</body>`) then stays Pending.
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
            "auction-hold path must stream the prefix before EOF. Got: {html}"
        );
        assert!(
            html.contains(".adSlots=JSON.parse"),
            "prefix must carry the injected (rewritten) head before EOF. Got: {html}"
        );
        assert!(
            !html.contains("var b=JSON.parse("),
            "bids inject only at </body> after collection, which the first poll must not wait for. Got: {html}"
        );
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
                template_cache_key: None,
                seam_ad_slots: None,
                policy_headers: Vec::new(),
                content_encoding: String::new(),
                origin_host: "origin.example.com".to_string(),
                origin_url: "https://origin.example.com".to_string(),
                request_host: "proxy.example.com".to_string(),
                request_scheme: "https".to_string(),
                content_type: "text/html; charset=utf-8".to_string(),
                ad_slots_script: None,
                ad_bids_state: AdBidsState::default(),
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
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
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: Some(test_auction_request()),
            dispatched_auction: Some(DispatchedAuction::empty_for_test(
                test_auction_request(),
                10,
            )),
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
            html.contains("var b=JSON.parse("),
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
        let state = AdBidsState::with_script(bids_script);
        let params = OwnedProcessResponseParams {
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: "gzip".to_string(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
            template_cache_key: None,
            seam_ad_slots: None,
            policy_headers: Vec::new(),
            content_encoding: String::new(),
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "proxy.example.com".to_string(),
            request_scheme: "https".to_string(),
            content_type: "text/html".to_string(),
            ad_slots_script: None,
            ad_bids_state: AdBidsState::default(),
            auction_observation: None,
            auction_request: None,
            dispatched_auction: None,
            price_granularity: crate::price_bucket::PriceGranularity::default(),
            gpt_diagnostics: None,
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
        use super::super::{
            AdBidsState, MatchedSlotsContext, build_ad_slots_script, build_auction_request,
            build_bid_map, build_bids_script, diagnostics_auction_id, html_escape_for_script,
            write_bids_to_state,
        };
        use crate::auction::types::{ApsRendererV1, ApsTagType, Bid, BidRenderer, MediaType};
        use crate::consent::ConsentContext;
        use crate::creative_opportunities::{
            CreativeOpportunitiesConfig, CreativeOpportunityFormat, CreativeOpportunitySlot,
        };
        use crate::http_util::RequestInfo;
        use crate::price_bucket::PriceGranularity;
        use crate::settings::Settings;
        use std::collections::HashMap;

        // Default settings are enough for the creative boundary: the sanitize
        // pass needs no config, and `rewrite_creative_html` only signs URLs it
        // actually rewrites (none of these fixtures carry proxyable URLs).
        fn test_settings() -> Settings {
            Settings::default()
        }

        fn make_config() -> CreativeOpportunitiesConfig {
            CreativeOpportunitiesConfig {
                enabled: true,
                gam_network_id: "21765378893".to_string(),
                auction_timeout_ms: Some(500),
                price_granularity: PriceGranularity::Dense,
                section_root: None,
                assembly_mode: None,
                template_cache_vary: None,
                template_cache_max_age_seconds: None,
                origin_is_cookie_independent: None,
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
                bid_id: None,
                creative_id: None,
                renderer: None,
                ad_id: Some(ad_id.to_string()),
                cache_id: None,
                cache_host: None,
                cache_path: None,
                metadata: Default::default(),
            }
        }

        #[test]
        fn ad_slots_script_contains_slot_data() {
            let mut slot = make_slot();
            slot.targeting
                .insert("ts".to_string(), "operator-value".to_string());
            let slots = vec![slot];
            let config = make_config();
            let script = build_ad_slots_script(&slots, &config, "/");
            let slot_json = crate::publisher::build_slot_json(&slots[0], &config, "example")
                .expect("should build slot JSON");
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
            assert_eq!(
                slot_json["targeting"]["ts"], "operator-value",
                "should forward operator-provided ts targeting verbatim"
            );
        }

        #[test]
        fn ad_slots_script_is_xss_safe() {
            let slots = vec![make_slot()];
            let config = make_config();
            let script = build_ad_slots_script(&slots, &config, "/");
            let inner = script
                .trim_start_matches("<script>")
                .trim_end_matches("</script>");
            assert!(!inner.contains('<'), "no unescaped < in script content");
            assert!(!inner.contains('>'), "no unescaped > in script content");
        }

        #[test]
        fn ad_slots_script_omits_only_over_limit_dynamic_slot() {
            let mut over_limit = make_slot();
            over_limit.id = "over_limit_dynamic".to_string();
            over_limit.gam_unit_path = Some("/{section}/{section}".to_string());
            over_limit
                .compile_unit_template()
                .expect("template should compile");
            let mut valid_static = make_slot();
            valid_static.id = "valid_static_sibling".to_string();
            valid_static.gam_unit_path = Some("/12345/example/static".to_string());
            let slots = vec![over_limit, valid_static];
            let config = make_config();
            let request_path = format!("/{}", "a".repeat(60));

            let script = build_ad_slots_script(&slots, &config, &request_path);

            assert!(
                !script.contains("over_limit_dynamic"),
                "should omit the over-limit dynamic slot"
            );
            assert!(
                script.contains("valid_static_sibling"),
                "should retain the valid static sibling"
            );
        }

        #[test]
        fn build_slot_json_renders_section_from_request_path() {
            let mut config = make_config();
            config.gam_network_id = "99999".to_string();
            config.section_root = Some("homepage".to_string());
            let mut slot = make_slot();
            slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
            slot.compile_unit_template()
                .expect("template should compile");

            let news_section = config.section_for_path("/news/article-123");
            let news = crate::publisher::build_slot_json(&slot, &config, &news_section)
                .expect("should render slot");
            assert_eq!(
                news["gam_unit_path"], "/99999/example/news",
                "section should derive from the first path segment"
            );

            let home_section = config.section_for_path("/");
            let home = crate::publisher::build_slot_json(&slot, &config, &home_section)
                .expect("should render slot");
            assert_eq!(
                home["gam_unit_path"], "/99999/example/homepage",
                "root path should use section_root"
            );
        }

        #[test]
        fn build_slot_json_honours_configured_section_segment() {
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

            let news_section = config.section_for_path("/en/news/article-123");
            let news = crate::publisher::build_slot_json(&slot, &config, &news_section)
                .expect("should render slot");
            assert_eq!(
                news["gam_unit_path"], "/99999/example/news",
                "section should derive from the configured segment index"
            );

            let locale_root_section = config.section_for_path("/en");
            let locale_root =
                crate::publisher::build_slot_json(&slot, &config, &locale_root_section)
                    .expect("should render slot");
            assert_eq!(
                locale_root["gam_unit_path"], "/99999/example/homepage",
                "a path with no segment at the configured index should use section_root"
            );
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
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
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

        /// Guards the browser-visible token every auction path shares: it must
        /// be fresh per auction and absent unless diagnostics can consume it.
        #[test]
        fn diagnostics_auction_id_is_fresh_and_gated() {
            let mut settings = test_settings();
            assert_eq!(
                diagnostics_auction_id(&settings),
                None,
                "no token should be minted without the diagnostics integration"
            );

            settings
                .integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
                .expect("should enable diagnostics");
            let first =
                diagnostics_auction_id(&settings).expect("enabled diagnostics should mint a token");
            let second =
                diagnostics_auction_id(&settings).expect("enabled diagnostics should mint a token");

            assert!(
                first.starts_with("ts-auc-"),
                "token should use the diagnostics prefix, got `{first}`"
            );
            assert_ne!(first, second, "each auction should mint its own token");
        }

        #[test]
        fn initial_document_bids_script_includes_auction_id_only_for_winning_bids() {
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
            let mut auction_request = build_auction_request(
                &slots_ctx,
                None,
                &ConsentContext::default(),
                &request_info,
                "publisher.example.com",
                Some("Mozilla/5.0"),
            );
            auction_request.id = "initial-auction-example-123".to_string();
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "atf_sidebar_ad".to_string(),
                make_bid(
                    "atf_sidebar_ad",
                    1.50,
                    "example_bidder",
                    "abc123",
                    "https://example.com/win",
                    "https://example.com/bill",
                ),
            );

            let state = AdBidsState::default();
            write_bids_to_state(
                &winning_bids,
                PriceGranularity::Dense,
                &state,
                &test_settings(),
                "",
                false,
                Some(&auction_request.id),
            );
            let script = state
                .script_cell()
                .lock()
                .expect("should lock initial bid state")
                .clone()
                .expect("should generate initial-document bids script");
            let bid_json = script
                .strip_prefix(
                    "<script>(function(){var t=window.tsjs=window.tsjs||{};var b=JSON.parse(\"",
                )
                .and_then(|script| {
                    script.strip_suffix(
                        "\");var s=t.scheduleInitialAdInit;if(typeof s===\"function\")s(b);else t.bids=b;})();</script>",
                    )
                })
                .expect("should emit the initial-document tsjs.bids script shape");
            let bid_json: String = serde_json::from_str(&format!("\"{bid_json}\""))
                .expect("should decode initial-document JSON.parse input");
            let bids: serde_json::Value = serde_json::from_str(&bid_json)
                .expect("should serialize initial-document bids as JSON");

            assert_eq!(
                bids["atf_sidebar_ad"]["hb_auction_id"], auction_request.id,
                "initial-document bids should expose the current request ID only on the winner"
            );

            write_bids_to_state(
                &HashMap::new(),
                PriceGranularity::Dense,
                &state,
                &test_settings(),
                "",
                false,
                Some(&auction_request.id),
            );
            let empty_script = state
                .script_cell()
                .lock()
                .expect("should lock empty initial bid state")
                .clone()
                .expect("should generate empty initial-document bids script");
            let empty_bid_json = empty_script
                .strip_prefix(
                    "<script>(function(){var t=window.tsjs=window.tsjs||{};var b=JSON.parse(\"",
                )
                .and_then(|script| {
                    script.strip_suffix(
                        "\");var s=t.scheduleInitialAdInit;if(typeof s===\"function\")s(b);else t.bids=b;})();</script>",
                    )
                })
                .expect("should emit the empty initial-document tsjs.bids script shape");
            let empty_bid_json: String = serde_json::from_str(&format!("\"{empty_bid_json}\""))
                .expect("should decode empty initial-document JSON.parse input");
            let empty_bids: serde_json::Value = serde_json::from_str(&empty_bid_json)
                .expect("should serialize empty initial-document bids as JSON");
            assert!(
                empty_bids
                    .as_object()
                    .expect("initial-document bids should be an object")
                    .is_empty(),
                "initial-document bids should not fabricate metadata without a winner"
            );
        }

        #[test]
        fn bid_map_omits_zero_creative_dimensions() {
            // Missing OpenRTB w/h parse to 0. Emitting w:0/h:0 would make the
            // bridge (which nullish-coalesces) size the frame to 0 instead of
            // falling back to the slot format, so a zero dimension must be omitted.
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.width = 0;
            bid.height = 0;
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");
            assert!(obj.get("w").is_none(), "should omit zero width");
            assert!(obj.get("h").is_none(), "should omit zero height");
        }

        #[test]
        fn bid_map_includes_winning_creative_dimensions() {
            // The bridge sizes the inline render from these dimensions; without
            // them it falls back to the first configured slot format, which
            // mis-sizes a multi-size slot whose winner is not the first format.
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.width = 300;
            bid.height = 600;
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");
            assert_eq!(
                obj.get("w").and_then(serde_json::Value::as_u64),
                Some(300),
                "should include winning creative width"
            );
            assert_eq!(
                obj.get("h").and_then(serde_json::Value::as_u64),
                Some(600),
                "should include winning creative height"
            );
        }

        #[test]
        fn client_bid_map_includes_adm_and_omits_debug_bid_by_default() {
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

            // Production path (include_debug_bid = false): the creative is always
            // included so the bridge can render it locally, but the verbose
            // debug_bid blob is not.
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
            let obj = map
                .get("atf_sidebar_ad")
                .expect("should have bid entry")
                .as_object()
                .expect("should be object");

            assert_eq!(
                obj.get("adm").and_then(|v| v.as_str()),
                Some("<div>Creative</div>"),
                "should include creative markup for local rendering by default"
            );
            assert!(
                obj.get("debug_bid").is_none(),
                "should omit the debug_bid blob when debug injection is disabled"
            );
        }

        #[test]
        fn build_bid_map_sanitizes_hostile_adm() {
            // The inline-adm path must run the same opt-in creative-processing
            // boundary as the `/auction` path (sanitize → rewrite) before the
            // creative reaches window.tsjs.bids, so with sanitization enabled
            // hostile executable markup never lands in the client-facing `adm`
            // for the Prebid Universal Creative to run.
            let mut settings = test_settings();
            settings.auction.sanitize_creatives = true;
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.creative = Some(
                "<div onclick=\"steal()\"><script>alert(1)</script>\
                 <a href=\"javascript:evil()\">x</a></div>"
                    .to_string(),
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, &settings, "", false);
            let adm = map
                .get("atf_sidebar_ad")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("adm"))
                .and_then(|v| v.as_str())
                .expect("should include a sanitized adm");

            assert!(
                !adm.contains("<script"),
                "should strip <script> elements from the inline adm"
            );
            assert!(
                !adm.contains("alert(1)"),
                "should strip inline script bodies from the inline adm"
            );
            assert!(
                !adm.contains("onclick"),
                "should strip on* event-handler attributes from the inline adm"
            );
            assert!(
                !adm.contains("javascript:"),
                "should strip javascript: URIs from the inline adm"
            );
        }

        #[test]
        fn build_bid_map_can_skip_rewriting_while_sanitizing() {
            let mut settings = test_settings();
            settings.auction.sanitize_creatives = true;
            settings.auction.rewrite_creatives = false;
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.creative = Some(
                "<div onclick=\"steal()\"><script>marker</script>\
                 <a href=\"https://click.example/landing\">x</a>\
                 <img src=\"https://cdn.example/ad.png\"></div>"
                    .to_string(),
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &settings,
                "https://publisher.example",
                false,
            );
            let adm = map
                .get("atf_sidebar_ad")
                .and_then(|value| value.as_object())
                .and_then(|object| object.get("adm"))
                .and_then(|value| value.as_str())
                .expect("should include a sanitized adm");

            assert!(
                adm.contains(r#"href="https://click.example/landing""#),
                "should keep accepted click URLs direct: {adm}"
            );
            assert!(
                adm.contains(r#"src="https://cdn.example/ad.png""#),
                "should keep accepted resource URLs direct: {adm}"
            );
            assert!(
                !adm.contains("/first-party/"),
                "should skip first-party URL rewriting: {adm}"
            );
            assert!(
                !adm.contains("data-tsclick"),
                "should skip click-guard attributes: {adm}"
            );
            assert!(
                !adm.contains("marker") && !adm.contains("onclick"),
                "should still sanitize executable markup: {adm}"
            );
        }

        #[test]
        fn build_bid_map_omits_oversized_adm() {
            // Creatives larger than the 1 MiB cap are rejected (empty result)
            // in every processing mode, so the bid is omitted rather than
            // recording a blank winner or shipping an unbounded creative to the
            // client. Runs with default settings to cover the shipped
            // configuration.
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.creative = Some(format!("<div>{}</div>", "a".repeat(1024 * 1024 + 1)));
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
            assert!(
                !map.contains_key("atf_sidebar_ad"),
                "should omit the bid when the creative exceeds the 1 MiB cap"
            );
        }

        // A supplied creative that processing rejects must not fall back to the
        // PBS Cache coordinates: the GPT bridge fetches the cached bid's ORIGINAL
        // adm, which would undo sanitization and the size cap entirely.
        fn cached_bid_with_creative(creative: &str) -> Bid {
            Bid {
                slot_id: "atf_sidebar_ad".to_string(),
                price: Some(1.50),
                currency: "USD".to_string(),
                creative: Some(creative.to_string()),
                adomain: None,
                bidder: "prebid".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                ad_id: Some("bid-impression-id".to_string()),
                bid_id: Some("openrtb-bid-id".to_string()),
                creative_id: None,
                // No typed renderer: these cases assert what happens when the
                // supplied markup is the bid's only render source.
                renderer: None,
                cache_id: Some("cache-uuid".to_string()),
                cache_host: Some("prebid-cache.example.com".to_string()),
                cache_path: Some("/cache".to_string()),
                metadata: Default::default(),
            }
        }

        // These fixtures carry cache coordinates but no typed renderer, so a
        // rejected creative leaves the bid with no render source at all and it
        // is dropped outright — which subsumes the property under test: the
        // cache coordinates never reach the client, so the cached (unprocessed)
        // copy of the markup cannot be fetched in place of what was refused.
        fn assert_no_render_source(settings: &Settings, creative: String, case: &str) {
            let mut winning_bids = HashMap::new();
            let mut bid = cached_bid_with_creative("");
            bid.creative = Some(creative);
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, settings, "", false);

            match map.get("atf_sidebar_ad").and_then(|v| v.as_object()) {
                None => {}
                Some(obj) => {
                    assert!(
                        obj.get("adm").is_none(),
                        "{case}: rejected creative should not emit adm"
                    );
                    assert!(
                        obj.get("hb_cache_host").is_none(),
                        "{case}: rejected creative should suppress hb_cache_host"
                    );
                    assert!(
                        obj.get("hb_cache_path").is_none(),
                        "{case}: rejected creative should suppress hb_cache_path"
                    );
                }
            }
        }

        #[test]
        fn build_bid_map_suppresses_cache_fallback_for_rejected_creatives() {
            let mut sanitizing = test_settings();
            sanitizing.auction.sanitize_creatives = true;

            // Script-only creative: sanitization strips everything.
            assert_no_render_source(
                &sanitizing,
                "<script>document.write('ad')</script>".to_string(),
                "script-only",
            );
            // Oversized creative: rejected by the cap in every mode.
            assert_no_render_source(
                &test_settings(),
                format!("<div>{}</div>", "a".repeat(1024 * 1024 + 1)),
                "oversized",
            );
            // An explicit empty `adm` is a supplied creative, not an absent one:
            // classifying it as absent would re-enable the raw cache fallback.
            assert_no_render_source(&test_settings(), String::new(), "explicit-empty");
        }

        #[test]
        fn build_bid_map_keeps_cache_fallback_for_absent_creatives() {
            // A bid with no supplied creative is the legitimate PBS Cache case:
            // the coordinates are the only render source.
            let mut winning_bids = HashMap::new();
            let mut bid = cached_bid_with_creative("");
            bid.creative = None;
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
            let obj = map
                .get("atf_sidebar_ad")
                .and_then(|v| v.as_object())
                .expect("should have a bid entry");

            assert_eq!(
                obj.get("hb_cache_host").and_then(|v| v.as_str()),
                Some("prebid-cache.example.com"),
                "absent creative should keep hb_cache_host"
            );
            assert_eq!(
                obj.get("hb_cache_path").and_then(|v| v.as_str()),
                Some("/cache"),
                "absent creative should keep hb_cache_path"
            );
        }

        #[test]
        fn build_bid_map_rewrites_inline_adm_to_absolute_first_party_urls() {
            // The inline `adm` is rendered by the Prebid Universal Creative inside
            // GAM's iframe (`f.srcdoc = d.ad`), a foreign origin. Proxied URLs must
            // therefore be emitted **absolute** against the publisher domain — a
            // root-relative `/first-party/proxy` would resolve against GAM and 404.
            // The tsjs bundle must NOT be injected into that foreign-origin iframe.
            let mut settings = test_settings();
            settings.auction.rewrite_creatives = true;
            settings.publisher.domain = "example.com".to_string();

            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "examplessp",
                "abc123",
                "https://ssp.example.com/win",
                "https://ssp.example.com/bill",
            );
            bid.creative = Some(
                "<html><body><img src=\"https://cdn.example.com/pixel.png\"></body></html>"
                    .to_string(),
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, &settings, "", false);
            let adm = map
                .get("atf_sidebar_ad")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("adm"))
                .and_then(|v| v.as_str())
                .expect("should include a rewritten adm");

            assert!(
                adm.contains("https://example.com/first-party/proxy?tsurl="),
                "should emit an absolute first-party proxy URL for the foreign-origin render context, got: {adm}"
            );
            assert!(
                !adm.contains("src=\"/first-party/proxy"),
                "should not emit a root-relative proxy URL that 404s under GAM's origin, got: {adm}"
            );
            assert!(
                !adm.contains("https://cdn.example.com/pixel.png"),
                "should proxy the original absolute CDN URL, got: {adm}"
            );
            assert!(
                !adm.contains("/static/tsjs="),
                "should not inject the tsjs bundle into a foreign-origin creative iframe, got: {adm}"
            );
        }

        #[test]
        fn build_bid_map_uses_request_origin_for_inline_urls() {
            // The inline adm's absolute first-party URLs must resolve against the
            // origin the visitor is on (here an HTTP dev host with a port), not the
            // configured publisher domain.
            let mut settings = test_settings();
            settings.auction.rewrite_creatives = true;
            settings.publisher.domain = "example.com".to_string();

            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "examplessp",
                "abc123",
                "https://ssp.example.com/win",
                "https://ssp.example.com/bill",
            );
            bid.creative = Some(
                "<html><body><img src=\"https://cdn.example.com/pixel.png\"></body></html>"
                    .to_string(),
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &settings,
                "http://localhost:7676",
                false,
            );
            let adm = map
                .get("atf_sidebar_ad")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("adm"))
                .and_then(|v| v.as_str())
                .expect("should include a rewritten adm");

            assert!(
                adm.contains("http://localhost:7676/first-party/proxy?tsurl="),
                "should emit URLs against the request origin, got: {adm}"
            );
            assert!(
                !adm.contains("https://example.com/first-party/proxy"),
                "must not fall back to the configured publisher domain, got: {adm}"
            );
        }

        #[test]
        fn build_bid_map_expands_auction_price_macro_before_rewrite() {
            // ${AUCTION_PRICE} must be resolved to the clearing price before the
            // creative is rewritten and signed. Otherwise URL rewriting encodes the
            // literal macro (`%24%7BAUCTION_PRICE%7D`) into the signed proxy/click
            // URL, so trackers receive an encoded macro instead of the price and the
            // signature locks the wrong value.
            let mut settings = test_settings();
            settings.publisher.domain = "example.com".to_string();

            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "examplessp",
                "abc123",
                "https://ssp.example.com/win",
                "https://ssp.example.com/bill",
            );
            bid.creative = Some(
                "<html><body>\
                 <a href=\"https://ads.example.com/click?p=${AUCTION_PRICE}\">go</a>\
                 </body></html>"
                    .to_string(),
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, &settings, "", false);
            let adm = map
                .get("atf_sidebar_ad")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("adm"))
                .and_then(|v| v.as_str())
                .expect("should include a rewritten adm");

            assert!(
                !adm.to_uppercase().contains("AUCTION_PRICE"),
                "no literal or encoded ${{AUCTION_PRICE}} macro should survive: {adm}"
            );
            assert!(
                adm.contains("p=1.5"),
                "the exact winning CPM should be substituted into the signed URL: {adm}"
            );
        }

        #[test]
        fn build_bid_map_expands_auction_price_macro_in_notification_urls() {
            // Per OpenRTB the win/billing notices are the primary carriers of
            // ${AUCTION_PRICE}, and the bridge fires them verbatim. An unexpanded
            // macro would report an unresolved clearing price to the SSP, and
            // would disagree with the price already substituted into the adm.
            let mut winning_bids = HashMap::new();
            let bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "examplessp",
                "abc123",
                "https://ssp.example.com/win?p=${AUCTION_PRICE}",
                "https://ssp.example.com/bill?p=${AUCTION_PRICE}",
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
            let obj = map
                .get("atf_sidebar_ad")
                .and_then(|v| v.as_object())
                .expect("should have bid entry");

            for field in ["nurl", "burl"] {
                let url = obj
                    .get(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("should include {field}"));
                assert!(
                    !url.to_uppercase().contains("AUCTION_PRICE"),
                    "no literal or encoded ${{AUCTION_PRICE}} macro should survive in {field}: {url}"
                );
                assert!(
                    url.ends_with("?p=1.5"),
                    "the exact winning CPM should be substituted into {field}: {url}"
                );
            }
        }

        #[test]
        fn build_bids_script_escapes_line_separators_in_adm() {
            // U+2028/U+2029 are valid JSON string content but terminate inline
            // <script> statements; they survive the sanitize boundary as ordinary
            // text, so build_bids_script must unicode-escape them.
            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "s",
                1.50,
                "kargo",
                "abc123",
                "https://ssp/win",
                "https://ssp/bill",
            );
            bid.creative = Some("<div>a\u{2028}b\u{2029}c</div>".to_string());
            winning_bids.insert("s".to_string(), bid);

            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
            let script = build_bids_script(&map);
            assert!(
                !script.contains('\u{2028}') && !script.contains('\u{2029}'),
                "should unicode-escape both U+2028 and U+2029 in the adm"
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

            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                true,
            );
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
                    bid_id: None,
                    creative_id: None,
                    renderer: None,
                    ad_id: Some("bid-impression-id".to_string()),
                    cache_id: Some("f47447a0-b759-4f2f-9887-af458b79b570".to_string()),
                    cache_host: Some("openads.adsrvr.org".to_string()),
                    cache_path: Some("/cache".to_string()),
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
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
                    bid_id: None,
                    ad_id: Some("aps-bid-token".to_string()),
                    creative_id: None,
                    renderer: None,
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
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
        fn bid_map_exposes_aps_renderer_and_selected_bid_id() {
            // Sanitization is opt-in, so enable it: the script-only creative
            // below is what drives this bid onto the renderer path. Left at the
            // default it would survive processing as an ordinary creative.
            let mut settings = test_settings();
            settings.auction.sanitize_creatives = true;
            let mut bid = make_bid("atf_sidebar_ad", 1.50, "aps", "fallback-ad", "", "");
            bid.bid_id = Some("selected-bid".to_string());
            bid.creative = Some("<script>reject()</script>".to_string());
            bid.nurl = None;
            bid.burl = None;
            bid.renderer = Some(BidRenderer::Aps(ApsRendererV1 {
                version: 1,
                account_id: "example-account".to_string(),
                bid_id: "selected-bid".to_string(),
                creative_id: None,
                tag_type: ApsTagType::Iframe,
                creative_url: "https://creative.example/render".to_string(),
                aax_response: "fictional-base64</script>".to_string(),
                width: 300,
                height: 250,
            }));
            let winning_bids = HashMap::from([("atf_sidebar_ad".to_string(), bid)]);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, &settings, "", false);
            let obj = map["atf_sidebar_ad"]
                .as_object()
                .expect("should include APS bid");

            assert_eq!(obj["hb_bidder"], "aps");
            assert_eq!(obj["hb_adid"], "selected-bid");
            assert_eq!(obj["renderer"]["type"], "aps");
            assert_eq!(obj["renderer"]["bidId"], "selected-bid");
            assert!(obj.get("adm").is_none());

            let script = build_bids_script(&map);
            assert!(!script.contains("</script></script>"));
            assert!(script.contains("\\u003C/script\\u003E"));
        }

        #[test]
        fn bid_map_omits_creative_rejected_by_processing_without_renderer() {
            // Sanitization is opt-in, so enable it: script-only markup is what
            // makes processing reject this bid's only render source.
            let mut settings = test_settings();
            settings.auction.sanitize_creatives = true;
            let mut bid = make_bid("atf_sidebar_ad", 1.50, "kargo", "fallback-ad", "", "");
            bid.creative = Some("<script>reject()</script>".to_string());
            let winning_bids = HashMap::from([("atf_sidebar_ad".to_string(), bid)]);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, &settings, "", false);

            assert!(
                !map.contains_key("atf_sidebar_ad"),
                "should omit a bid whose only creative was rejected"
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
                    bid_id: None,
                    creative_id: None,
                    renderer: None,
                    ad_id: None,
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
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
                    bid_id: None,
                    creative_id: None,
                    renderer: None,
                    ad_id: None,
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: Default::default(),
                },
            );
            let map = build_bid_map(
                &winning_bids,
                PriceGranularity::Dense,
                &test_settings(),
                "",
                false,
            );
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
        fn bids_script_schedules_ad_init_without_retry_timer() {
            let mut map = serde_json::Map::new();
            map.insert("atf".to_string(), serde_json::json!({"hb_pb": "1.00"}));

            let script = build_bids_script(&map);

            assert!(
                script.contains("t.scheduleInitialAdInit"),
                "should hand off bids to the deferred adInit scheduler"
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
        fn bids_script_defers_ad_init_until_after_hydration() {
            let mut map = serde_json::Map::new();
            map.insert("atf".to_string(), serde_json::json!({"hb_pb": "1.00"}));

            let script = build_bids_script(&map);

            // adInit() mutates ad-slot subtrees (GPT defineSlot on the
            // `-container` wrapper). Running it synchronously at body-parse time
            // lands those mutations inside React's hydration window and trips a
            // #418 hydration mismatch. The deferral lifecycle (window `load`,
            // double `requestAnimationFrame`, generation-0 pinning via
            // `tsjs.navGeneration`) lives in the GPT bundle module (with a
            // head-injected fallback in gpt_bootstrap.js) where it is executable
            // under Vitest (schedule_initial_ad_init.test.ts); this inline
            // script must only delegate to that scheduler.
            assert!(
                script.contains("var s=t.scheduleInitialAdInit"),
                "should delegate deferral to the installed scheduler"
            );
            // The bids payload is handed to the scheduler (which applies it only
            // while the page is still on navigation generation 0) instead of
            // being assigned unconditionally, so a faster SPA navigation's live
            // bids cannot be clobbered by the stale SSR payload.
            assert!(
                script.contains("if(typeof s===\"function\")s(b)"),
                "should pass the SSR bids payload to the scheduler"
            );
            assert!(
                script.contains("else t.bids=b"),
                "should fall back to a plain bids assignment without a scheduler"
            );
            assert!(
                !script.contains(".bids=JSON.parse"),
                "should not assign the SSR payload unconditionally"
            );
            // The one hydration-unsafe thing this script could do is invoke
            // adInit synchronously at body-parse time — it must not.
            assert!(
                !script.contains("adInit()"),
                "should not invoke adInit synchronously at parse time"
            );
            assert!(
                !script.contains("setTimeout"),
                "should not retry adInit on a timer"
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
                "publisher.example.com",
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
        fn auction_request_uses_configured_publisher_domain_not_edge_host() {
            // On the SSAT proxy path the browser addresses the trusted-server
            // edge host, but the auction must advertise the configured
            // publisher domain to SSPs — otherwise injected creatives and the
            // brand-safety pixel leak the edge/staging host.
            let slot = make_slot();
            let slots = [slot];
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &slots,
                request_path: "/2024/01/my-article/?edition=fictional",
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
        fn auction_request_preserves_configured_publisher_domain_with_query() {
            // On the SSAT proxy path the browser addresses the trusted-server
            // edge host, but the auction must advertise the configured
            // publisher domain to SSPs — otherwise injected creatives and the
            // brand-safety pixel leak the edge/staging host.
            let slot = make_slot();
            let slots = [slot];
            let slots_ctx = MatchedSlotsContext {
                matched_slots: &slots,
                request_path: "/2024/01/my-article/?edition=fictional",
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
                "page_url should remove client query data"
            );
            assert_eq!(
                site.page, "https://www.example.com/2024/01/my-article/",
                "site.page should remove client query data"
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
        use super::build_services_with_http_client;
        use crate::auction::AuctionOrchestrator;
        use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
        use crate::auction::types::{AuctionRequest, AuctionResponse, Bid};
        use crate::creative_opportunities::{CreativeOpportunityFormat, CreativeOpportunitySlot};
        use crate::platform::test_support::{StubHttpClient, noop_services};
        use crate::platform::{PlatformHttpRequest, PlatformResponse};
        use crate::test_support::tests::crate_test_settings_str;
        use error_stack::{Report, ResultExt};
        use http::Method;
        use std::sync::{Arc, Mutex};

        const AUCTION_ID_TEST_PROVIDER: &str = "auction_id_test_provider";
        const AUCTION_ID_TEST_BACKEND: &str = "auction-id-test-backend";

        struct AuctionIdTestProvider {
            captured_request: Arc<Mutex<Option<AuctionRequest>>>,
            winning_bid: bool,
        }

        #[async_trait::async_trait(?Send)]
        impl AuctionProvider for AuctionIdTestProvider {
            fn provider_name(&self) -> &'static str {
                AUCTION_ID_TEST_PROVIDER
            }

            async fn request_bids(
                &self,
                request: &AuctionRequest,
                context: &AuctionContext<'_>,
            ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
                *self
                    .captured_request
                    .lock()
                    .expect("should lock captured auction request") = Some(request.clone());
                let request = PlatformHttpRequest::new(
                    Request::builder()
                        .method(Method::POST)
                        .uri("https://bidder.example.test/bids")
                        .body(EdgeBody::empty())
                        .expect("should build test bidder request"),
                    AUCTION_ID_TEST_BACKEND,
                );
                context
                    .services
                    .http_client()
                    .send_async(request)
                    .await
                    .change_context(TrustedServerError::Auction {
                        message: "test bidder launch failed".to_string(),
                    })
                    .map(ProviderRequestOutcome::pending)
            }

            async fn parse_response(
                &self,
                _response: PlatformResponse,
                response_time_ms: u64,
            ) -> Result<AuctionResponse, Report<TrustedServerError>> {
                let bids = if self.winning_bid {
                    vec![Bid {
                        slot_id: "atf".to_string(),
                        price: Some(1.50),
                        currency: "USD".to_string(),
                        creative: None,
                        adomain: None,
                        bidder: AUCTION_ID_TEST_PROVIDER.to_string(),
                        width: 300,
                        height: 250,
                        nurl: None,
                        burl: None,
                        bid_id: None,
                        creative_id: None,
                        renderer: None,
                        ad_id: Some("winner-123".to_string()),
                        cache_id: None,
                        cache_host: None,
                        cache_path: None,
                        metadata: Default::default(),
                    }]
                } else {
                    Vec::new()
                };
                Ok(AuctionResponse::success(
                    AUCTION_ID_TEST_PROVIDER,
                    bids,
                    response_time_ms,
                ))
            }

            fn timeout_ms(&self) -> u32 {
                100
            }

            fn backend_name(
                &self,
                _services: &RuntimeServices,
                _timeout_ms: u32,
            ) -> Option<String> {
                Some(AUCTION_ID_TEST_BACKEND.to_string())
            }
        }

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
            Settings::from_toml(&toml).expect("should parse settings with disabled templates")
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
            make_page_bids_request_on(PAGE_BIDS_PATH, path)
        }

        /// Builds a page-bids request against an explicit endpoint path, so the
        /// canonical route and its deprecated alias can be compared directly.
        fn make_page_bids_request_on(endpoint: &str, path: &str) -> Request<EdgeBody> {
            let mut req = Request::builder()
                .method(Method::GET)
                .uri(format!("https://test-publisher.com{endpoint}?path={path}"))
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

        fn auction_id_test_orchestrator(
            settings: &Settings,
            captured_request: Arc<Mutex<Option<AuctionRequest>>>,
            winning_bid: bool,
        ) -> AuctionOrchestrator {
            let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            orchestrator.register_provider(Arc::new(AuctionIdTestProvider {
                captured_request,
                winning_bid,
            }));
            orchestrator
        }

        #[tokio::test]
        async fn page_bids_response_includes_auction_id_only_for_winning_bids() {
            let mut settings = settings_with_co();
            settings.auction.providers = vec![AUCTION_ID_TEST_PROVIDER.to_string()];
            settings
                .integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
                .expect("should enable diagnostics");
            let slots = article_slot();
            let winning_stub = Arc::new(StubHttpClient::new());
            winning_stub.push_response(200, b"winner".to_vec());
            let winning_services = build_services_with_http_client(
                Arc::clone(&winning_stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let winning_request = Arc::new(Mutex::new(None));
            let winning_orchestrator =
                auction_id_test_orchestrator(&settings, Arc::clone(&winning_request), true);
            let ec_context = EcContext::new_for_test(
                Some("page-auction-example-123".to_string()),
                crate::consent::ConsentContext {
                    jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                    ..Default::default()
                },
            );

            let winning_response = handle_page_bids(
                &settings,
                &winning_services,
                None,
                AuctionDispatch {
                    orchestrator: &winning_orchestrator,
                    slots: &slots,
                    registry: None,
                },
                &ec_context,
                make_page_bids_request("/2024/01/my-article/"),
            )
            .await
            .expect("should return winning page-bids response");
            let winning_body: serde_json::Value = serde_json::from_slice(
                &winning_response
                    .into_body()
                    .into_bytes()
                    .expect("should read winning page-bids response body"),
            )
            .expect("should serialize winning page-bids response as JSON");
            let auction_request = winning_request
                .lock()
                .expect("should lock captured winning request")
                .clone()
                .expect("should dispatch a winning auction request");

            assert_eq!(
                auction_request.id, "ts-page-auction-example-123",
                "test EC ID should produce a deterministic auction request ID"
            );
            let winning_auction_id = winning_body["bids"]["atf"]["hb_auction_id"]
                .as_str()
                .expect("page-bids should expose an auction ID on the winner")
                .to_string();
            assert!(
                winning_auction_id.starts_with("ts-auc-"),
                "page-bids should expose a freshly minted diagnostics token, got `{winning_auction_id}`"
            );
            assert_ne!(
                winning_auction_id, auction_request.id,
                "browser-visible auction ID must not be the EC-derived request ID"
            );
            assert!(
                !winning_auction_id.contains("page-auction-example-123"),
                "browser-visible auction ID must not embed the EC ID"
            );

            let no_winner_stub = Arc::new(StubHttpClient::new());
            no_winner_stub.push_response(200, b"no-bid".to_vec());
            let no_winner_services = build_services_with_http_client(
                Arc::clone(&no_winner_stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let no_winner_orchestrator =
                auction_id_test_orchestrator(&settings, Arc::new(Mutex::new(None)), false);
            let no_winner_response = handle_page_bids(
                &settings,
                &no_winner_services,
                None,
                AuctionDispatch {
                    orchestrator: &no_winner_orchestrator,
                    slots: &slots,
                    registry: None,
                },
                &ec_context,
                make_page_bids_request("/2024/01/my-article/"),
            )
            .await
            .expect("should return no-winner page-bids response");
            let no_winner_body: serde_json::Value = serde_json::from_slice(
                &no_winner_response
                    .into_body()
                    .into_bytes()
                    .expect("should read no-winner page-bids response body"),
            )
            .expect("should serialize no-winner page-bids response as JSON");

            assert!(
                no_winner_body["bids"]
                    .as_object()
                    .expect("page-bids should return a bids object")
                    .is_empty(),
                "page-bids should not fabricate auction metadata without a winner"
            );
        }

        /// The browser-visible auction ID is minted per auction and only for
        /// deployments that run the diagnostics integration, so it can neither
        /// carry EC identity across auctions nor reach pages that ignore it.
        #[tokio::test]
        async fn page_bids_auction_id_is_per_auction_and_gated_on_diagnostics() {
            async fn winning_auction_id(settings: &Settings) -> Option<String> {
                let slots = article_slot();
                let stub = Arc::new(StubHttpClient::new());
                stub.push_response(200, b"winner".to_vec());
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );
                let orchestrator =
                    auction_id_test_orchestrator(settings, Arc::new(Mutex::new(None)), true);
                let ec_context = EcContext::new_for_test(
                    Some("page-auction-example-123".to_string()),
                    crate::consent::ConsentContext {
                        jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                        ..Default::default()
                    },
                );
                let response = handle_page_bids(
                    settings,
                    &services,
                    None,
                    AuctionDispatch {
                        orchestrator: &orchestrator,
                        slots: &slots,
                        registry: None,
                    },
                    &ec_context,
                    make_page_bids_request("/2024/01/my-article/"),
                )
                .await
                .expect("should return page-bids response");
                let body: serde_json::Value = serde_json::from_slice(
                    &response
                        .into_body()
                        .into_bytes()
                        .expect("should read page-bids response body"),
                )
                .expect("should serialize page-bids response as JSON");
                body["bids"]["atf"]["hb_auction_id"]
                    .as_str()
                    .map(str::to_string)
            }

            let mut settings = settings_with_co();
            settings.auction.providers = vec![AUCTION_ID_TEST_PROVIDER.to_string()];
            settings
                .integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
                .expect("should enable diagnostics");

            let first = winning_auction_id(&settings)
                .await
                .expect("first auction should expose a diagnostics token");
            let second = winning_auction_id(&settings)
                .await
                .expect("second auction should expose a diagnostics token");
            assert_ne!(
                first, second,
                "each auction for the same visitor should mint its own token"
            );

            settings
                .integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": false }))
                .expect("should disable diagnostics");
            assert_eq!(
                winning_auction_id(&settings).await,
                None,
                "no auction metadata should reach the page without the diagnostics integration"
            );
        }

        /// The deprecated `/__ts/page-bids` alias must be handled identically to
        /// the canonical path — same status, same JSON body.
        ///
        /// The alias exists so pre-rename tsjs bundles keep getting ads on SPA
        /// navigations. If the handler ever varied its output by request path
        /// (slot matching reads the `path` *query parameter*, not the endpoint
        /// path), those clients would silently get different results from the
        /// ones on the canonical route.
        #[tokio::test]
        async fn deprecated_alias_response_matches_canonical_path() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());

            let canonical = run_page_bids_response(
                &settings,
                &orchestrator,
                &article_slot(),
                make_page_bids_request_on(PAGE_BIDS_PATH, "/2024/01/my-article/"),
            )
            .await;
            let alias = run_page_bids_response(
                &settings,
                &orchestrator,
                &article_slot(),
                make_page_bids_request_on(PAGE_BIDS_LEGACY_PATH, "/2024/01/my-article/"),
            )
            .await;

            assert_eq!(
                canonical.status(),
                alias.status(),
                "alias must return the same status as the canonical path"
            );
            assert_eq!(
                canonical.into_body().into_bytes(),
                alias.into_body().into_bytes(),
                "alias must return the same body as the canonical path"
            );
        }

        /// Traffic on the deprecated alias must be measurable from edge access
        /// logs, not just application logs: the removal precondition in
        /// IABTechLab/trusted-server#970 is "no remaining traffic on the legacy
        /// path", and operators who cannot read app logs need a response-side
        /// marker to count.
        #[tokio::test]
        async fn deprecated_alias_response_is_marked_deprecated() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());

            let canonical = run_page_bids_response(
                &settings,
                &orchestrator,
                &article_slot(),
                make_page_bids_request_on(PAGE_BIDS_PATH, "/2024/01/my-article/"),
            )
            .await;
            let alias = run_page_bids_response(
                &settings,
                &orchestrator,
                &article_slot(),
                make_page_bids_request_on(PAGE_BIDS_LEGACY_PATH, "/2024/01/my-article/"),
            )
            .await;

            assert_eq!(
                alias
                    .headers()
                    .get(header::LINK)
                    .and_then(|value| value.to_str().ok()),
                Some(
                    "<https://github.com/IABTechLab/trusted-server/issues/970>; rel=\"deprecation\""
                ),
                "alias response should carry the RFC 9745 deprecation link relation"
            );
            assert!(
                !canonical.headers().contains_key(header::LINK),
                "canonical path should not be marked deprecated"
            );
        }

        /// A deployment without creative opportunities answers page-bids with a
        /// 404, but its alias traffic still has to be counted — otherwise a
        /// silent legacy signal on such a config reads as "no remaining
        /// traffic" when evaluating IABTechLab/trusted-server#970.
        #[tokio::test]
        async fn deprecated_alias_is_marked_without_creative_opportunities() {
            let settings = settings_without_co();
            assert!(
                settings.creative_opportunities.is_none(),
                "test settings should have no creative opportunities configured"
            );
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());

            let response = run_page_bids_response(
                &settings,
                &orchestrator,
                &[],
                make_page_bids_request_on(PAGE_BIDS_LEGACY_PATH, "/2024/01/my-article/"),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "should 404 when creative opportunities are not configured"
            );
            assert!(
                response.headers().contains_key(header::LINK),
                "alias 404 should still be marked deprecated so it is countable"
            );
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
            let returned_slots = body["slots"].as_array().expect("slots should be array");

            assert_eq!(
                returned_slots.len(),
                1,
                "should omit only the over-limit dynamic slot"
            );
            assert_eq!(
                returned_slots[0]["id"], "valid_static_sibling",
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
        async fn disabled_server_side_ad_templates_return_no_slots_or_bids() {
            // The dedicated template switch must suppress publisher/page-bids
            // delivery without using the global auction switch.
            let settings = settings_with_co_templates_disabled();
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
                "disabled server-side ad templates must not return slot definitions"
            );
            assert_eq!(
                body["bids"]
                    .as_object()
                    .expect("bids should be object")
                    .len(),
                0,
                "disabled server-side ad templates must not produce bids"
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
                EdgeCacheHeader::SMaxageFallback,
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
                EdgeCacheHeader::SMaxageFallback,
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
