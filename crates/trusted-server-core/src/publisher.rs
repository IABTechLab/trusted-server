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

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
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
    AdmRenderSourceV1, AuctionContext, AuctionIdentityGenerator, AuctionRequest,
    AuctionSlotFailureReason, Bid, BidRenderSourceV1, BrowserAuctionBidV1,
    BrowserAuctionProjectionV1, CacheFetchPolicyV1, CacheRenderSourceV1, DeviceInfo, PublisherInfo,
    SiteInfo, SlotAuctionDecisionV1, UserInfo, mint_response_unique_base64url_identity,
};
use crate::cache_policy::{
    CachePolicy, EdgeCacheHeader, cache_control_headers_are_private_or_no_store,
};
use crate::consent::{consent_allows_server_side_auction, gate_eids_by_consent};
use crate::constants::{COOKIE_TS_EIDS, HEADER_X_COMPRESS_HINT};
use crate::cookies::handle_request_cookies;
use crate::creative_opportunities::{AssemblyMode, CreativeOpportunitiesConfig};
use crate::ec::EcContext;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::html_processor::BodyCloseInjection;
use crate::http_util::{RequestInfo, is_navigation_request, serve_static_with_etag};
use crate::integrations::IntegrationRegistry;
use crate::platform::{
    GeoInfo, PlatformBackendSpec, PlatformHttpRequest, RuntimeServices, VarySpec,
    contains_publisher_esi_directive,
};
use crate::price_bucket::{PriceGranularity, price_bucket};
use crate::response_privacy::{
    apply_inactive_ad_stack_browser_cache_policy, cache_control_forbids_shared_storage,
    enforce_synthesized_html_cache_privacy, enforce_terminal_private_cache_privacy,
};
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
const HEADER_X_TS_TEMPLATE_CACHE: &str = "x-ts-template-cache";
const HEADER_X_TS_ASSEMBLY: &str = "x-ts-assembly";

#[derive(Clone, Copy, PartialEq, Eq)]
enum TemplateCacheResponseState {
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

impl TemplateCacheResponseState {
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

fn set_template_cache_response_state(
    response: &mut Response<EdgeBody>,
    state: TemplateCacheResponseState,
) {
    response.headers_mut().insert(
        HEADER_X_TS_TEMPLATE_CACHE,
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

    if filename == crate::integrations::gpt_diagnostics::GPT_DIAGNOSTICS_BOOTSTRAP_FILENAME {
        let mut response = serve_static_with_etag(
            crate::integrations::gpt_diagnostics::GPT_DIAGNOSTICS_BOOTSTRAP_SOURCE,
            req,
            "application/javascript; charset=utf-8",
        );
        response
            .headers_mut()
            .insert(HEADER_X_COMPRESS_HINT, HeaderValue::from_static("on"));
        return Ok(response);
    }

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
    /// See [`HtmlStreamProcessorParams::csp_nonce_observed`].
    csp_nonce_observed: Option<&'a Arc<AtomicBool>>,
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
                csp_nonce_observed: params.csp_nonce_observed.clone(),
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
            csp_nonce_observed: params.csp_nonce_observed.cloned(),
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
    // A template-cache response is always identity bytes. Decode during the transform instead of
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
    /// Where the transform records a response-bound CSP nonce, when one matters.
    csp_nonce_observed: Option<Arc<AtomicBool>>,
}

/// The diagnostics decision the template may carry.
///
/// Diagnostics is request-scoped — activated by a cookie or query parameter, and
/// documented as an immutable per-request decision — so it must not reach a shared
/// template.
///
/// It does not leak today even without this gate, but only by coincidence:
/// `requires_private_no_store()` is a strict superset of the conditions under which
/// a script is emitted, and that stamp lands before the template cache gate reads response
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

/// Transform-owned stand-in for the seam, emitted at the document's structural body end.
///
/// Deliberately *not* [`AD_ASSEMBLY_SEAM`]. The payload that ends up in the seam is not
/// known until the completed transform has been checked for publisher collisions, and the
/// position is not knowable from the output bytes: a reverse search for `</body>` selects
/// a string literal in `<script>const marker = "</body>";</script>` when the document has
/// no real close, and prefers a `</body>` sequence in trailing comment data over the real
/// closing tag. Only the parser knows which one is structural, so the parser marks the
/// spot and the substitution below fills it in.
///
/// Keeping it distinct from [`AD_ASSEMBLY_SEAM`] is what lets a publisher document that
/// contains the seam bytes still receive correctly positioned bids: that collision
/// revokes the shared reservation without disturbing this placeholder.
pub(crate) const TEMPLATE_SEAM_PLACEHOLDER: &str = "<!--ts-seam-slot-->";

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

/// Whether this mode's completed template receives [`AD_ASSEMBLY_SEAM`].
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

/// What the streaming HTML processor should inject at `</body>`.
///
/// Explicit rather than inferred. The previous shape read
/// `ad_slots_script.is_some()` inside the element handler, which silently coupled
/// two independent decisions: once [`template_ad_slots_script`] stopped emitting a
/// head script under a shared mode, body-close injection stopped with it.
///
/// `Esi` emits [`TEMPLATE_SEAM_PLACEHOLDER`], not the seam itself. What goes into the
/// seam is still decided after the completed transform has been checked for publisher
/// collisions and ESI directives — but *where* it goes has to be decided here, by the
/// parser, because the output bytes cannot distinguish a structural `</body>` from one
/// written inside a script string or a trailing comment.
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
        AssemblyMode::Esi => BodyCloseInjection::Marker(TEMPLATE_SEAM_PLACEHOLDER.to_string()),
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

    // Only a response that can be stored has a consumer for the observation, so the
    // handlers are not registered for ordinary inline traffic.
    let csp_nonce_observed = params
        .shared_template_authorized
        .then_some(params.csp_nonce_observed)
        .flatten();

    let config = config
        .with_ad_state(params.ad_slots_script, params.ad_bids_state)
        .with_gpt_diagnostics(gpt_diagnostics)
        .with_body_close(body_close)
        .with_csp_nonce_observer(csp_nonce_observed)
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
    /// A shared template read from template cache, to be assembled on the way out.
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
    /// `Some` only when `template_cache_bypass_reason` cleared the response, so the key's
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
    /// Set by the transform when the document carries a response-bound CSP nonce.
    ///
    /// `None` wherever no transform runs. Recorded by the HTML parser rather than
    /// rescanned from the output, which cannot tell a `nonce` attribute from the same
    /// word inside a script.
    pub(crate) csp_nonce_observed: Option<Arc<AtomicBool>>,
}

/// Response-authorized template cache insert inputs. The key is built before origin lookup; the
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
            // Authorized template cache transforms are emitted as identity by
            // `process_response_streaming_async`; inline transforms retain the origin
            // coding. This avoids recompressing and immediately decoding a full document.
            let bytes = output.into_inner();
            // Cache taxonomy for this path: C1 is the raw origin/read-through cache,
            // the template cache stores processed reader-neutral HTML, and C3 would be
            // a forbidden cache of the final per-user assembled response.
            // Store first, assemble second — never the reverse. The stored bytes are
            // shared between visitors; the assembled ones carry this visitor's bids.
            // Swapping these two lines would create the forbidden C3 leak.
            // Read before the store: `store_template_if_authorized` *takes* the key so a
            // request cannot store twice, which would leave nothing for assembly to gate
            // on.
            let was_authorized = params.template_cache_key.is_some();
            let shared_bypass_reason = was_authorized
                .then(|| shared_template_bypass_reason(&bytes, params.csp_nonce_observed.as_ref()))
                .flatten();
            if let Some(reason) = shared_bypass_reason {
                log::warn!("template_cache bypass: transformed response {reason}");
                params.template_cache_key.take();
            }
            let mut shared_response_authorized = was_authorized && shared_bypass_reason.is_none();
            // The parser marked the structural body end during the transform; this puts the
            // right payload there. A shared response gets the inert seam every reader will
            // split on, a bypassed one gets this reader's bids directly.
            let bytes = if was_authorized {
                let payload: Cow<'_, str> = if shared_response_authorized {
                    Cow::Borrowed(AD_ASSEMBLY_SEAM)
                } else {
                    Cow::Owned(seam_script_for(&params))
                };
                match replace_seam_placeholder(bytes, payload.as_bytes()) {
                    Ok(bytes) => bytes,
                    Err((bytes, error)) => {
                        // Publisher bytes collided with the transform's own placeholder, so
                        // there is no position TS can claim. Serving the document untouched
                        // costs this page its bids; guessing a position corrupts it and, if
                        // stored, every later reader of it too.
                        log::warn!(
                            "template_cache bypass: {error}; serving the transformed document \
                             without a seam"
                        );
                        params.template_cache_key.take();
                        shared_response_authorized = false;
                        bytes
                    }
                }
            } else {
                bytes
            };
            let bypasses_shared_template = was_authorized && !shared_response_authorized;
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
            if response_carries_a_seam_marker(shared_response_authorized, settings) {
                split_template_at_seam(&bytes).change_context_lazy(|| {
                    crate::error::TrustedServerError::Proxy {
                        message: "refusing to store a template with no usable seam marker"
                            .to_string(),
                    }
                })?;
            }
            let store_outcome = store_template_if_authorized(&mut params, &bytes).await;
            if was_authorized {
                set_template_cache_response_state(
                    &mut response,
                    match (bypasses_shared_template, store_outcome) {
                        (true, _) => TemplateCacheResponseState::BypassResponse,
                        (false, Some(TemplateStoreOutcome::Stored)) => {
                            TemplateCacheResponseState::MissStored
                        }
                        (false, Some(TemplateStoreOutcome::Expired)) => {
                            TemplateCacheResponseState::BypassResponse
                        }
                        (false, Some(TemplateStoreOutcome::Error) | None) => {
                            TemplateCacheResponseState::MissStoreError
                        }
                    },
                );
            }
            let (bytes, assembly_state) = if bypasses_shared_template {
                (bytes, Some(AssemblyResponseState::ByteSeamFallback))
            } else {
                assemble_if_shared(
                    shared_response_authorized,
                    shared_response_authorized,
                    settings,
                    &params,
                    services,
                    bytes,
                )?
            };
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
        csp_nonce_observed: None,
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
    let at = only_occurrence(template, marker)?;
    Ok((&template[..at], &template[at + marker.len()..]))
}

/// Offset of the one and only occurrence of `needle`.
///
/// # Errors
///
/// Returns [`SeamError::Missing`] when `needle` is absent and [`SeamError::Repeated`]
/// when it appears more than once — which, for a transform-owned marker, means publisher
/// bytes collided with it and no occurrence can be claimed as ours.
fn only_occurrence(haystack: &[u8], needle: &[u8]) -> Result<usize, SeamError> {
    let mut found = haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(at, _)| at);
    let at = found.next().ok_or(SeamError::Missing)?;
    if found.next().is_some() {
        return Err(SeamError::Repeated);
    }
    Ok(at)
}

/// Why the completed transform must not be stored as a shared template, if it must not.
///
/// Every one of these is invisible before the transform runs. Publisher-authored seam
/// bytes and ESI survive it, and a CSP nonce the origin delivered in a `<meta>` policy or
/// on its own elements never appears in a response header, which is all the eligibility
/// gate gets to inspect.
fn shared_template_bypass_reason(
    bytes: &[u8],
    csp_nonce_observed: Option<&Arc<AtomicBool>>,
) -> Option<&'static str> {
    if bytes
        .windows(AD_ASSEMBLY_SEAM.len())
        .any(|window| window == AD_ASSEMBLY_SEAM.as_bytes())
    {
        return Some("contains publisher-authored seam bytes");
    }
    if contains_publisher_esi_directive(bytes) {
        return Some("contains publisher-authored ESI");
    }
    if csp_nonce_observed.is_some_and(|observed| observed.load(Ordering::SeqCst)) {
        return Some("delivers a response-bound CSP nonce in its own markup");
    }
    None
}

/// Substitute `payload` for the transform-owned [`TEMPLATE_SEAM_PLACEHOLDER`].
///
/// The placeholder was written by the HTML parser at the document's structural body end,
/// or appended at document end when the document has no body close at all — so this
/// carries the parser's answer rather than re-deriving it from bytes that cannot express
/// it.
///
/// # Errors
///
/// Returns the untouched document together with the reason when the placeholder is absent
/// or appears more than once. Both mean publisher bytes collided with it, and inserting at
/// a guessed position would corrupt the document and the template stored from it.
fn replace_seam_placeholder(
    mut document: Vec<u8>,
    payload: &[u8],
) -> Result<Vec<u8>, (Vec<u8>, SeamError)> {
    let placeholder = TEMPLATE_SEAM_PLACEHOLDER.as_bytes();
    match only_occurrence(&document, placeholder) {
        Ok(at) => {
            document.splice(at..at + placeholder.len(), payload.iter().copied());
            Ok(document)
        }
        Err(error) => Err((document, error)),
    }
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

/// Builds the response served from a template cache hit.
///
/// Every header is constructed here rather than replayed from the stored entry, so no
/// origin header can reach a second visitor through the cache.
///
/// # Why this sets `private, no-store` itself
///
/// A template cache hit returns **before** the origin fetch, and therefore before the point where
/// the publisher path stamps `private, no-store` and strips validators. Omitting it
/// here does not fall back to a safe default — it emits HTML with no `Cache-Control` at
/// all, which is heuristically cacheable by browsers and intermediaries. That is a
/// forbidden C3 cache of a final per-user assembled response.
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
    // Metadata decoding accepts only identity templates. Reader encoding is selected
    // after assembly rather than replaying the origin's representation.
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
    enforce_synthesized_html_cache_privacy(&mut response);
    set_response_compression(&mut response, reader_compression);
    set_template_cache_response_state(&mut response, TemplateCacheResponseState::Hit);
    set_assembly_response_state(&mut response, AssemblyResponseState::ByteSeam);
    Ok(response)
}

/// Writes the transformed template to the shared cache, if the gate authorized it.
///
/// The key's presence is the authorization: it is `Some` only when
/// `template_cache_bypass_reason` cleared the response, so this cannot store something the gate
/// rejected. Takes the key rather than borrowing it, so a second call for the same
/// request stores nothing.
///
/// Failures are logged and swallowed. A cache that cannot be written is a slower
/// service, not a broken one, and the whole point of the template cache is that the response is
/// reproducible without it.
///
/// Spike-only, for the #1009 ESI validation.
async fn store_template_if_authorized(
    params: &mut OwnedProcessResponseParams,
    bytes: &[u8],
) -> Option<TemplateStoreOutcome> {
    let store = params.template_cache_key.take()?;
    let max_age = store.expires_at.saturating_duration_since(Instant::now());
    if max_age.is_zero() {
        log::debug!("template_cache store skipped: origin freshness expired during transformation");
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
                "template_cache stored {} bytes (seam marker present: {})",
                bytes.len(),
                bytes
                    .windows(AD_ASSEMBLY_SEAM.len())
                    .any(|w| w == AD_ASSEMBLY_SEAM.as_bytes())
            );
            Some(TemplateStoreOutcome::Stored)
        }
        Err(err) => {
            log::warn!("template_cache store failed: {err}");
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
    // The cost is that a template cache *miss* buffers. That is the right trade: misses are already
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
        csp_nonce_observed: params.csp_nonce_observed.as_ref(),
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
        csp_nonce_observed: params.csp_nonce_observed.clone(),
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
        transport_timeout_ms: 0,
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
    /// Dedicated `[creative_opportunities].enabled` switch.
    ad_templates_enabled: bool,
    /// Global `[auction].enabled` gate used by publisher/page-bids flows.
    auction_enabled: bool,
}

/// Returns whether request-scoped signals permit an ad-eligible navigation.
fn is_server_side_ad_eligible_navigation(
    is_get: bool,
    is_navigation: bool,
    is_prefetch: bool,
    is_bot: bool,
    consent_allows_auction: bool,
) -> bool {
    is_get && is_navigation && !is_prefetch && !is_bot && consent_allows_auction
}

/// Returns true only when the publisher should inject and run server-side ad templates.
///
/// This includes auction dispatch plus initial ad-slot injection.
fn should_run_server_side_ad_stack(
    is_get: bool,
    is_navigation: bool,
    is_prefetch: bool,
    is_bot: bool,
    has_matched_slots: bool,
    consent_allows_auction: bool,
    config: ServerSideAdStackConfig,
) -> bool {
    is_server_side_ad_eligible_navigation(
        is_get,
        is_navigation,
        is_prefetch,
        is_bot,
        consent_allows_auction,
    ) && config.ad_templates_enabled
        && has_matched_slots
        && config.auction_enabled
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
        creative_opportunities.map_or_else(Vec::new, |co_config| {
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
                transport_timeout_ms: auction_timeout_ms,
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
                    fatal_admission_error,
                    metadata,
                    elapsed_ms,
                } => {
                    if let Some(error) = fatal_admission_error {
                        log::warn!(
                            "Auction admission failed before publisher dispatch; continuing without bids: {error:?}"
                        );
                    }
                    if !metadata.is_empty() {
                        log::info!("Auction dispatch failure metadata: {metadata:?}");
                    }
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

    // Recorded before the request is consumed by the origin send: the template cache gate
    // below needs it, and an authorized response must never become a shared
    // template.
    //
    // A credential this edge already terminated is exempt. Basic auth runs as
    // middleware ahead of routing, so reaching here on a gated path means the same
    // handler already validated the request; every reader that can look the template
    // up has satisfied that same credential. Without the marker the credential is
    // pass-through to the origin and still disqualifies. See
    // [`crate::auth::EdgeTerminatedAuthorization`].
    let authorization_value_count = req.headers().get_all(header::AUTHORIZATION).iter().count();
    let request_had_authorization = match authorization_value_count {
        0 => false,
        1 => req
            .extensions()
            .get::<crate::auth::EdgeTerminatedAuthorization>()
            .is_none(),
        _ => true,
    };
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
    // Tag suppression is request-scoped (for example, an IP exclusion), while a template cache
    // template is shared across readers. A shared template can represent neither the
    // suppressed nor unsuppressed variant safely for the other population.
    let datadome_suppression_requires_origin = suppress_datadome_client_side_tag;
    let datadome_suppression_requires_full_body =
        suppress_datadome_client_side_tag && is_html_document_request(&req);
    let request_requires_origin = request_bypasses_template_cache(req.headers())
        || gpt_diagnostics.requires_private_no_store()
        || datadome_suppression_requires_origin;
    let reader_compression = negotiate_reader_compression(req.headers());
    let reader_supports_assembly = reader_compression.is_ok();
    // A failed negotiation bypasses template cache below, so this value is used only on an
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

    // Only advertise encodings the rewrite pipeline can decode and re-encode. This
    // remains unconditional when template cache negotiation fails: that request bypasses shared
    // assembly, but its origin response still needs to be processable for TSJS injection.
    restrict_accept_encoding(&mut req);
    if matches!(assembly_mode, AssemblyMode::Esi) && !reader_supports_assembly {
        log::debug!("template_cache bypass: reader accepts no representation TS can assemble");
    }
    // Strip the internal `fastly-ssl` scheme signal before forwarding to the
    // origin. On the EdgeZero path the entry point re-injects this header from
    // trusted Fastly TLS metadata so in-process scheme detection works; the
    // legacy path never sets it. Either way it is an internal edge signal that
    // must not leak to publisher backends.
    req.headers_mut().remove("fastly-ssl");
    // The template cache key is built here, before the request is consumed, because every field
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
            "template_cache bypass: method {} is not eligible for a shared template",
            req.method()
        );
    }
    if request_requires_origin && matches!(assembly_mode, AssemblyMode::Esi) {
        log::debug!("template_cache bypass: request cache semantics or diagnostics require origin");
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
    let mut template_cache_response_state = matches!(assembly_mode, AssemblyMode::Esi)
        .then_some(TemplateCacheResponseState::BypassRequest);
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

    // Template-cache lookup happens before the origin fetch — the whole point is to skip it.
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
                log::debug!("template_cache hit: {} bytes", entry.body.len());

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
                        "template_cache hit is unusable ({err}); treating as a miss. \
                         The transform changed without TEMPLATE_SCHEMA_VERSION moving."
                    );
                    if let Err(purge_err) = services.template_cache().purge_url(key).await {
                        log::warn!(
                            "template_cache could not purge unusable URL variants: {purge_err}"
                        );
                    }
                    template_cache_response_state = Some(TemplateCacheResponseState::Invalid);
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
                log::debug!("template_cache cold miss: insert reservation acquired");
                template_cache_reservation = Some(reservation);
                template_cache_response_state = Some(TemplateCacheResponseState::MissReserved);
            }
            Ok(crate::platform::TemplateCacheLookup::Unsupported) => {
                log::debug!("template_cache bypass: platform has no shared cache");
                template_cache_response_state = Some(TemplateCacheResponseState::Unsupported);
            }
            Ok(crate::platform::TemplateCacheLookup::Invalid(miss)) => {
                log::warn!("template_cache invalid entry: {miss}; purging and falling back inline");
                if let Err(purge_err) = services.template_cache().purge_url(key).await {
                    log::warn!("template_cache could not purge invalid URL variants: {purge_err}");
                }
                template_cache_response_state = Some(TemplateCacheResponseState::Invalid);
            }
            Err(err) => {
                log::warn!("template_cache backend failure: {err}; falling back inline");
                template_cache_response_state = Some(TemplateCacheResponseState::BackendError);
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

        let mut response = Response::builder()
            .status(StatusCode::BAD_GATEWAY)
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

    let template_cache_policy = TemplateCachePolicy::from_settings(settings);
    let gate_content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut template_cache_key = template_cache_reservation.and_then(|reservation| {
        match template_cache_ttl(
            assembly_mode,
            request_had_authorization,
            cookie_disqualifies,
            response.status(),
            &gate_content_type,
            response.headers(),
            &template_cache_policy,
        ) {
            Err(reason) => {
                log::debug!("template_cache bypass: {reason}");
                None
            }
            Ok(ttl) => {
                log::debug!("template_cache eligible for {}s", ttl.as_secs());
                let Some(expires_at) = Instant::now().checked_add(ttl) else {
                    log::warn!("template_cache bypass: origin freshness cannot be represented");
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
                    "template_cache bypass: policy metadata changed after validation ({reason})"
                );
                template_cache_key = None;
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    if template_cache_response_state == Some(TemplateCacheResponseState::MissReserved)
        && template_cache_key.is_none()
    {
        template_cache_response_state = Some(TemplateCacheResponseState::BypassResponse);
    }
    if let Some(state) = template_cache_response_state {
        set_template_cache_response_state(&mut response, state);
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
    let is_not_modified = response.status() == StatusCode::NOT_MODIFIED;
    if is_html_content_type(origin_content_type) || is_not_modified {
        if should_run_ad_stack || assembled_response_must_be_private {
            enforce_synthesized_html_cache_privacy(&mut response);
        } else if is_server_side_ad_eligible_navigation(
            is_get,
            is_navigation,
            is_prefetch,
            is_bot,
            consent_allows_auction,
        ) && (response.status() == StatusCode::OK || is_not_modified)
        {
            // Issue #1007 caps browser caching for structurally inactive
            // server-side ad templates. The cap also applies to 304 responses
            // so revalidation cannot restore the origin freshness policy.
            // Request-scoped skips retain the origin policy because the same URL
            // can otherwise render templates.
            if !cache_control_forbids_shared_storage(response.headers()) {
                apply_inactive_ad_stack_browser_cache_policy(&mut response);
            }
        }
    }

    crate::integrations::gpt_diagnostics::finalize_response(&gpt_diagnostics, &mut response);

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|h| h.to_str().unwrap_or_default())
        .unwrap_or_default()
        .to_string();

    apply_datadome_client_tag_cache_privacy(
        &mut response,
        &request_method,
        suppress_datadome_client_side_tag,
        &content_type,
    );

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

            let body = std::mem::replace(response.body_mut(), EdgeBody::empty());
            response.headers_mut().remove(header::CONTENT_LENGTH);

            Ok(PublisherResponse::Stream {
                response,
                body,
                params: Box::new(OwnedProcessResponseParams {
                    // The transform writes here; the post-transform gate reads it. Always
                    // present on the live path so no future caller has to remember to
                    // supply it — the handlers themselves stay gated on authorization.
                    csp_nonce_observed: Some(Arc::new(AtomicBool::new(false))),
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

#[allow(
    dead_code,
    reason = "pure coordinated-cutover projection is wired to entry points in Task 19"
)]
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
            let Some(upstream_bid_id) = bid.bid_id.clone() else {
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
                    auction_id: result.decision_set.auction_id.clone(),
                    results: projected_results,
                },
                bids: projected_bids,
            },
            request_origin,
        )
    }
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

/// Prospective hard-cutover mark emitted at the bids/projection boundary.
///
/// Task 19 inserts this already-tested fragment into the production boot path in
/// the same atomic switch that installs the matching first-display mark.
#[allow(
    dead_code,
    reason = "Task 16 prepares this fragment for the atomic Task 19 production switch"
)]
pub(crate) fn build_bids_script_performance_mark() -> &'static str {
    "(function(){try{window.performance.mark(\"tsjs:bids-script\");}catch(_){}})();"
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

/// Why a response must not enter the shared transformed-template cache (template cache).
///
/// `cache::core` is not an HTTP cache: it stores whatever bytes it is handed and
/// rejects nothing on its own. Every safety condition is the caller's to enforce,
/// so they are enumerated here rather than left implicit.
///
/// Spike-only, for the #1009 ESI validation.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
pub(crate) enum TemplateCacheBypassReason {
    /// Not a shared-template mode; there is no template cache object to write.
    #[display("assembly mode is inline")]
    InlineMode,
    /// The origin set a cookie. Caching this would replay one visitor's cookie
    /// to the next — and the cookie-privacy net downgrades *our* response, which
    /// happens after the cache has already stored the origin's.
    #[display("origin response carries Set-Cookie")]
    OriginSetCookie,
    /// The origin declared the response non-shareable, or supplied CDN-specific
    /// policy directives the template-cache path does not interpret and therefore cannot safely override.
    #[display("origin cache policy is not eligible for template cache sharing")]
    OriginNotShareable,
    /// Cache directives or HTTP dates were malformed. Failing closed prevents a
    /// parser disagreement from extending a representation's lifetime.
    #[display("origin cache policy is malformed")]
    MalformedCachePolicy,
    /// Core Cache has no HTTP heuristic freshness. Template cache therefore requires an explicit,
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

fn request_bypasses_template_cache(headers: &edgezero_core::http::HeaderMap) -> bool {
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
                // Positive or malformed constraints remain unprovable because template cache does
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
/// A newtype rather than a bare `Vec<String>` so [`TemplateCacheBypassReason`] stays `Display`-able
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
struct TemplateCachePolicy {
    key_vary: VarySpec,
    max_age: Duration,
}

impl TemplateCachePolicy {
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
/// See `docs/superpowers/archive/2026-08-08-esi-cacheable-root-validation-design.md`
/// §6.6 for why the C1 raw-origin/read-through cache, the reader-neutral template cache, and
/// the forbidden C3 final assembled-response cache are distinct.
#[cfg(test)]
pub(crate) fn template_cache_bypass_reason(
    mode: AssemblyMode,
    request_had_authorization: bool,
    cookie_disqualifies: bool,
    status: StatusCode,
    content_type: &str,
    response_headers: &edgezero_core::http::HeaderMap,
    key_vary: &VarySpec,
) -> Option<TemplateCacheBypassReason> {
    let policy = TemplateCachePolicy::for_test(key_vary, Duration::from_secs(60));
    template_cache_ttl(
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
) -> Result<Option<&str>, TemplateCacheBypassReason> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(TemplateCacheBypassReason::MalformedCachePolicy);
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)
}

fn single_representation_header_value(
    headers: &edgezero_core::http::HeaderMap,
    name: header::HeaderName,
) -> Result<Option<&str>, TemplateCacheBypassReason> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(TemplateCacheBypassReason::MalformedRepresentationHeaders);
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| TemplateCacheBypassReason::MalformedRepresentationHeaders)
}

fn parse_delta_seconds(value: &str) -> Result<u64, TemplateCacheBypassReason> {
    let value = value.trim();
    let quoted_at_start = value.starts_with('"');
    let quoted_at_end = value.ends_with('"');
    let digits = match (quoted_at_start, quoted_at_end) {
        (true, true) if value.len() >= 2 => &value[1..value.len() - 1],
        (false, false) => value,
        _ => return Err(TemplateCacheBypassReason::MalformedCachePolicy),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TemplateCacheBypassReason::MalformedCachePolicy);
    }
    digits
        .parse::<u64>()
        .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)
}

/// Parse the Fastly-specific freshness policy used by template cache's hosting platform.
///
/// Fastly documents `max-age`, `stale-while-revalidate`, and `stale-if-error` for
/// `Surrogate-Control`. Template cache uses only `max-age` as fresh lifetime; the stale windows
/// are validated so malformed policy cannot hide beside a valid max age, but Core
/// Cache assembly does not serve stale templates under either extension.
///
/// Unknown directives fail closed rather than inheriting semantics from another CDN.
fn surrogate_control_freshness(
    headers: &edgezero_core::http::HeaderMap,
) -> Result<Option<Duration>, TemplateCacheBypassReason> {
    let mut saw_header = false;
    let mut max_age = None;
    let mut stale_while_revalidate = None;
    let mut stale_if_error = None;

    for value in headers.get_all("surrogate-control") {
        saw_header = true;
        let value = value
            .to_str()
            .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)?;
        if value.trim().is_empty() {
            return Err(TemplateCacheBypassReason::MalformedCachePolicy);
        }
        for directive in value.split(',') {
            let directive = directive.trim();
            if directive.is_empty() {
                return Err(TemplateCacheBypassReason::MalformedCachePolicy);
            }
            let (name, value) = directive
                .split_once('=')
                .map_or((directive, None), |(name, value)| (name, Some(value)));
            let name = name.trim().to_ascii_lowercase();
            match name.as_str() {
                "private" | "no-store" | "no-cache" => {
                    return Err(TemplateCacheBypassReason::OriginNotShareable);
                }
                "max-age" => {
                    let parsed = parse_delta_seconds(
                        value.ok_or(TemplateCacheBypassReason::MalformedCachePolicy)?,
                    )?;
                    if max_age.replace(parsed).is_some() {
                        return Err(TemplateCacheBypassReason::MalformedCachePolicy);
                    }
                }
                "stale-while-revalidate" => {
                    let parsed = parse_delta_seconds(
                        value.ok_or(TemplateCacheBypassReason::MalformedCachePolicy)?,
                    )?;
                    if stale_while_revalidate.replace(parsed).is_some() {
                        return Err(TemplateCacheBypassReason::MalformedCachePolicy);
                    }
                }
                "stale-if-error" => {
                    let parsed = parse_delta_seconds(
                        value.ok_or(TemplateCacheBypassReason::MalformedCachePolicy)?,
                    )?;
                    if stale_if_error.replace(parsed).is_some() {
                        return Err(TemplateCacheBypassReason::MalformedCachePolicy);
                    }
                }
                _ => return Err(TemplateCacheBypassReason::MalformedCachePolicy),
            }
        }
    }

    if !saw_header {
        return Ok(None);
    }
    let max_age = max_age.ok_or(TemplateCacheBypassReason::NoPositiveFreshness)?;
    if max_age == 0 {
        return Err(TemplateCacheBypassReason::NoPositiveFreshness);
    }
    Ok(Some(Duration::from_secs(max_age)))
}

fn origin_shared_ttl(
    headers: &edgezero_core::http::HeaderMap,
    max_age: Duration,
) -> Result<Duration, TemplateCacheBypassReason> {
    origin_shared_ttl_at(headers, SystemTime::now(), max_age)
}

fn origin_shared_ttl_at(
    headers: &edgezero_core::http::HeaderMap,
    now: SystemTime,
    template_cache_max_age: Duration,
) -> Result<Duration, TemplateCacheBypassReason> {
    let mut max_age = None;
    let mut shared_max_age = None;

    for value in headers.get_all(header::CACHE_CONTROL) {
        let value = value
            .to_str()
            .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)?;
        for directive in value.split(',').map(str::trim).filter(|v| !v.is_empty()) {
            let (name, value) = directive
                .split_once('=')
                .map_or((directive, None), |(name, value)| (name, Some(value)));
            match name.trim().to_ascii_lowercase().as_str() {
                "private" | "no-store" | "no-cache" => {
                    return Err(TemplateCacheBypassReason::OriginNotShareable);
                }
                "max-age" => {
                    let parsed = parse_delta_seconds(
                        value.ok_or(TemplateCacheBypassReason::MalformedCachePolicy)?,
                    )?;
                    if max_age.replace(parsed).is_some() {
                        return Err(TemplateCacheBypassReason::MalformedCachePolicy);
                    }
                }
                "s-maxage" => {
                    let parsed = parse_delta_seconds(
                        value.ok_or(TemplateCacheBypassReason::MalformedCachePolicy)?,
                    )?;
                    if shared_max_age.replace(parsed).is_some() {
                        return Err(TemplateCacheBypassReason::MalformedCachePolicy);
                    }
                }
                _ => {}
            }
        }
    }

    let date = single_header_value(headers, header::DATE)?
        .map(httpdate::parse_http_date)
        .transpose()
        .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)?;
    let standard_freshness = match shared_max_age.or(max_age) {
        Some(seconds) => Some(Duration::from_secs(seconds)),
        None => single_header_value(headers, header::EXPIRES)?
            .map(|value| {
                let expires = httpdate::parse_http_date(value)
                    .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)?;
                expires
                    .duration_since(date.unwrap_or(now))
                    .map_err(|_| TemplateCacheBypassReason::NoPositiveFreshness)
            })
            .transpose()?,
    };
    // Fastly gives Surrogate-Control precedence for its edge cache. Standard
    // restrictive directives were still parsed above and remain hard refusals.
    let freshness = surrogate_control_freshness(headers)?
        .or(standard_freshness)
        .ok_or(TemplateCacheBypassReason::NoPositiveFreshness)?;

    let age = single_header_value(headers, header::AGE)?
        .map(parse_delta_seconds)
        .transpose()?
        .unwrap_or(0);
    // `Age` may be absent even when an upstream cache emitted an old `Date`.
    // RFC 9111's corrected age is at least the apparent age; ignoring it would
    // grant an already-expired representation a new template cache lifetime.
    let apparent_age = date
        .and_then(|date| now.duration_since(date).ok())
        .unwrap_or_default();
    let current_age = Duration::from_secs(age).max(apparent_age);
    let remaining = freshness
        .checked_sub(current_age)
        .filter(|duration| !duration.is_zero())
        .ok_or(TemplateCacheBypassReason::NoPositiveFreshness)?;
    let capped = remaining.min(template_cache_max_age);
    if capped.is_zero() {
        return Err(TemplateCacheBypassReason::NoPositiveFreshness);
    }
    Ok(capped)
}

fn replayable_policy_headers(
    headers: &edgezero_core::http::HeaderMap,
) -> Result<Vec<(String, String)>, TemplateCacheBypassReason> {
    let mut captured = Vec::new();
    for name in crate::platform::REPLAYABLE_POLICY_HEADERS {
        for value in headers.get_all(*name) {
            let value = value
                .to_str()
                .map_err(|_| TemplateCacheBypassReason::MalformedPolicyHeader)?;
            if (*name == "content-security-policy"
                || *name == "content-security-policy-report-only")
                && value.to_ascii_lowercase().contains("'nonce-")
            {
                return Err(TemplateCacheBypassReason::CspNonce);
            }
            captured.push(((*name).to_string(), value.to_string()));
        }
    }
    Ok(captured)
}

fn template_cache_ttl(
    mode: AssemblyMode,
    request_had_authorization: bool,
    cookie_disqualifies: bool,
    status: StatusCode,
    content_type: &str,
    response_headers: &edgezero_core::http::HeaderMap,
    policy: &TemplateCachePolicy,
) -> Result<Duration, TemplateCacheBypassReason> {
    if matches!(mode, AssemblyMode::Inline) {
        return Err(TemplateCacheBypassReason::InlineMode);
    }
    if request_had_authorization {
        return Err(TemplateCacheBypassReason::AuthorizedRequest);
    }
    if cookie_disqualifies {
        return Err(TemplateCacheBypassReason::CookieForwarded);
    }
    if response_headers.contains_key(header::SET_COOKIE) {
        return Err(TemplateCacheBypassReason::OriginSetCookie);
    }
    // Core Cache has no HTTP semantics. Fastly's documented Surrogate-Control subset
    // is parsed by `origin_shared_ttl`; every other vendor-specific policy remains a
    // bypass rather than guessing that unrelated CDNs share its grammar or precedence.
    if crate::response_privacy::CDN_CACHE_HEADERS
        .iter()
        .filter(|name| **name != "surrogate-control")
        .any(|name| response_headers.contains_key(*name))
    {
        return Err(TemplateCacheBypassReason::OriginNotShareable);
    }
    // Checked here, among the leak vectors, because storing under a key that does not
    // cover the origin's Vary is cross-serving rather than mere ineligibility: a request
    // differing only in the uncovered header would read this template.
    let mut vary_values = Vec::new();
    for value in response_headers.get_all(header::VARY) {
        let value = value
            .to_str()
            .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)?;
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if name == "*" {
                return Err(TemplateCacheBypassReason::VaryWildcard);
            }
            if name.eq_ignore_ascii_case(header::COOKIE.as_str()) {
                return Err(TemplateCacheBypassReason::VaryCookie);
            }
            header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| TemplateCacheBypassReason::MalformedCachePolicy)?;
        }
        vary_values.push(value);
    }
    let uncovered = policy.key_vary.uncovered_by(vary_values);
    if !uncovered.is_empty() {
        return Err(TemplateCacheBypassReason::VaryNotCovered(VaryGap(
            uncovered,
        )));
    }
    if status != StatusCode::OK {
        return Err(TemplateCacheBypassReason::NonOkStatus);
    }
    let declared_content_type =
        single_representation_header_value(response_headers, header::CONTENT_TYPE)?;
    if declared_content_type.is_some_and(|declared| declared != content_type)
        || !is_html_content_type(content_type)
    {
        return Err(TemplateCacheBypassReason::NotHtml);
    }
    let content_encoding =
        single_representation_header_value(response_headers, header::CONTENT_ENCODING)?
            .map_or_else(
                || "identity".to_string(),
                |value| value.trim().to_ascii_lowercase(),
            );
    if content_encoding.is_empty() {
        return Err(TemplateCacheBypassReason::MalformedRepresentationHeaders);
    }
    if !is_supported_content_encoding(&content_encoding) {
        return Err(TemplateCacheBypassReason::UnsupportedContentEncoding);
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
/// See `docs/superpowers/archive/2026-08-08-esi-cacheable-root-validation-design.md`
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
#[must_use]
pub fn page_bids_preflight_denied() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Forbidden"));
    *response.status_mut() = StatusCode::FORBIDDEN;
    enforce_terminal_private_cache_privacy(&mut response);
    response
}

/// Builds the `400 Bad Request` returned for an unrecognized `format`.
///
/// `private, no-store` like every other response from this endpoint, so an error
/// cannot be cached and replayed.
fn page_bids_unknown_format() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Unknown format"));
    *response.status_mut() = StatusCode::BAD_REQUEST;
    enforce_terminal_private_cache_privacy(&mut response);
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

    let matched_slots = if co_config.enabled {
        match_renderable_slots(auction.slots, co_config, &path_param)
    } else {
        Vec::new()
    };
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
                transport_timeout_ms: timeout_ms,
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
    enforce_terminal_private_cache_privacy(&mut response);
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
    use crate::auction::types::{AdFormat, AdSlot, MediaType};
    use crate::auction::types::{AuctionDecisionSetV1, AuctionDropReason, AuctionResponse};
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
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: Some(creative.to_string()),
            adomain: None,
            bidder: "seat".to_string(),
            returned_seat: None,
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
        fn unavailable_reservation_randomness_is_identity_generation_failed() {
            let result = result_with_winners(vec![tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5)]);
            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
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
            csp_nonce_observed: None,
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
    fn ts_console_stream_publisher_body_injects_active_diagnostics_for_materialized_html() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
        let integration_registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
            html.contains("tsjs-gpt_diagnostics.min.js"),
            "should inject the standalone diagnostics module"
        );
    }

    #[test]
    fn stream_publisher_body_round_trips_gzip() {
        let settings = create_test_settings();
        let integration_registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
        let integration_registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
        const UNEXPECTED_304_PROVIDER: &str = "example-navigation-bidder";
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
            fn provider_name(&self) -> &str {
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

        fn settings_with_disabled_auction() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = false\n\n\
                 [creative_opportunities]\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with disabled auction")
        }

        fn settings_without_creative_opportunities() -> Settings {
            Settings::from_toml(&crate_test_settings_str())
                .expect("should parse settings without creative opportunities")
        }
        fn settings_with_dispatching_provider() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n[auction.providers.{UNEXPECTED_304_PROVIDER}]\nprotocol = \"openrtb-2.6\"\nendpoint = \"https://unexpected.example/openrtb2/auction\"\nrouting = \"all_eligible\"\n\n\
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
            queue_html_response_with_cache_control(stub, "public, max-age=300");
        }

        fn queue_html_response_with_cache_control(
            stub: &StubHttpClient,
            cache_control: &'static str,
        ) {
            queue_html_response_with_status_and_cache_control(stub, 200, cache_control);
        }

        fn queue_html_response_with_status_and_cache_control(
            stub: &StubHttpClient,
            status: u16,
            cache_control: &'static str,
        ) {
            stub.push_response_with_headers(
                status,
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

        fn non_regulated_consent() -> crate::consent::ConsentContext {
            crate::consent::ConsentContext {
                jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
                ..Default::default()
            }
        }

        async fn run_with_slots(
            settings: &Settings,
            services: &RuntimeServices,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
        ) -> PublisherResponse {
            run_with_slots_and_consent(settings, services, slots, req, non_regulated_consent())
                .await
        }

        async fn run_with_slots_and_consent(
            settings: &Settings,
            services: &RuntimeServices,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
            consent: crate::consent::ConsentContext,
        ) -> PublisherResponse {
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            run_with_orchestrator_and_consent(
                settings,
                services,
                &orchestrator,
                slots,
                req,
                consent,
            )
            .await
        }

        async fn run_with_orchestrator(
            settings: &Settings,
            services: &RuntimeServices,
            orchestrator: &AuctionOrchestrator,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
        ) -> PublisherResponse {
            run_with_orchestrator_and_consent(
                settings,
                services,
                orchestrator,
                slots,
                req,
                non_regulated_consent(),
            )
            .await
        }

        async fn run_with_orchestrator_and_consent(
            settings: &Settings,
            services: &RuntimeServices,
            orchestrator: &AuctionOrchestrator,
            slots: &[CreativeOpportunitySlot],
            req: Request<EdgeBody>,
            consent: crate::consent::ConsentContext,
        ) -> PublisherResponse {
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
        async fn signer_admission_failure_continues_publisher_origin_without_provider_io() {
            let mut settings = settings_with_dispatching_provider();
            settings
                .request_signing
                .as_mut()
                .expect("should configure request signing stores")
                .enabled = true;
            let plan = Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile signed navigation auction"),
            );
            let orchestrator = crate::auction::build_orchestrator_with_plan(plan, &settings)
                .expect("should build signed plan-backed orchestrator");
            let stub = Arc::new(StubHttpClient::new());
            queue_cacheable_html_response(&stub);
            let services = services_with_telemetry(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
                Arc::new(RecordingTelemetrySink::default()),
            );
            let slots = [article_slot()];

            let response = run_with_orchestrator(
                &settings,
                &services,
                &orchestrator,
                &slots,
                conditional_navigation_request(),
            )
            .await;

            assert_eq!(response_head(response).status, StatusCode::OK);
            assert_eq!(
                stub.recorded_backend_names(),
                vec!["stub-backend".to_string()],
                "signer admission failure should skip provider I/O and still fetch the publisher origin"
            );
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
        async fn navigation_without_matched_slots_preserves_origin_cache_policy() {
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
                (header::CACHE_CONTROL, "private, max-age=60"),
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
            let mut settings = settings_with_disabled_ad_templates();
            settings.proxy.allowed_domains =
                vec!["*.example".to_string(), "*.example.com".to_string()];
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
            let registry =
                IntegrationRegistry::new(&settings).expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let response = buffer_publisher_response_async(
                response,
                &Method::GET,
                &settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("should buffer disabled-template response");
            let (response_head, body) = response.into_parts();
            let body = String::from_utf8(
                body.into_bytes()
                    .expect("should return an in-memory publisher body")
                    .to_vec(),
            )
            .expect("should return UTF-8 publisher HTML");

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
                Some("private, max-age=60"),
                "disabled server-side ad templates should use the private browser cache policy"
            );
            assert!(
                !body.contains(".adSlots=JSON.parse"),
                "disabled server-side ad templates should not inject ad-slot state"
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
        async fn disabled_auction_uses_private_browser_cache_policy() {
            // Arrange
            let settings = settings_with_disabled_auction();
            let stub = Arc::new(StubHttpClient::new());
            queue_html_response_with_cache_control(&stub, "no-cache");
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );

            // Act
            let response = run_with_slots(
                &settings,
                &services,
                &[article_slot()],
                conditional_navigation_request(),
            )
            .await;
            let response_head = response_head(response);

            // Assert
            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, max-age=60"),
                "disabled auction should use the private browser cache policy"
            );
        }

        #[tokio::test]
        async fn navigation_without_matched_slots_replaces_origin_cache_policy() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();

            for cache_control in ["no-cache", "max-age=0", "must-revalidate", "s-maxage=0"] {
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
                    Some("private, max-age=60"),
                    "inactive server-side ad templates should replace origin {cache_control} policy"
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
                    "inactive server-side ad templates should preserve private origin {cache_control} policy"
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
                        "inactive server-side ad templates should preserve {header_name}"
                    );
                }
            }
        }

        #[tokio::test]
        async fn request_scoped_ad_stack_suppression_preserves_origin_cache_policy() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();
            let slots = [article_slot()];
            let mut bot_request = conditional_navigation_request();
            bot_request.headers_mut().insert(
                "user-agent",
                HeaderValue::from_static("Mozilla/5.0 (compatible; Googlebot/2.1)"),
            );
            let mut prefetch_request = conditional_navigation_request();
            prefetch_request
                .headers_mut()
                .insert("sec-purpose", HeaderValue::from_static("prefetch"));

            for (skip_reason, request, consent) in [
                ("bot", bot_request, non_regulated_consent()),
                ("prefetch", prefetch_request, non_regulated_consent()),
                (
                    "consent denied",
                    conditional_navigation_request(),
                    crate::consent::ConsentContext {
                        jurisdiction: crate::consent::jurisdiction::Jurisdiction::Gdpr,
                        ..Default::default()
                    },
                ),
            ] {
                // Arrange
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_cache_control(&stub, "no-cache");
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                // Act
                let response =
                    run_with_slots_and_consent(&settings, &services, &slots, request, consent)
                        .await;
                let response_head = response_head(response);

                // Assert
                assert_eq!(
                    response_head
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some("no-cache"),
                    "{skip_reason} should retain the origin cache policy"
                );
            }
        }

        #[tokio::test]
        async fn absent_creative_opportunities_use_short_browser_cache_policy() {
            // Arrange
            let settings = settings_without_creative_opportunities();
            let stub = Arc::new(StubHttpClient::new());
            queue_html_response_with_cache_control(&stub, "no-cache");
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );

            // Act
            let response =
                run_with_slots(&settings, &services, &[], conditional_navigation_request()).await;
            let response_head = response_head(response);

            // Assert
            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, max-age=60"),
                "absent creative opportunities should use the private inactive-stack cache policy"
            );
        }

        #[tokio::test]
        async fn inactive_ad_stack_preserves_non_ok_response_cache_policy() {
            let settings = settings_with_disabled_ad_templates();

            for status in [206, 404, 500, 503] {
                // Arrange
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_status_and_cache_control(&stub, status, "no-cache");
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                // Act
                let response = run_with_slots(
                    &settings,
                    &services,
                    &[article_slot()],
                    conditional_navigation_request(),
                )
                .await;
                let response_head = response_head(response);

                // Assert
                assert_eq!(
                    response_head
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some("no-cache"),
                    "inactive server-side ad templates should preserve origin policy on {status}"
                );
            }
        }

        #[tokio::test]
        async fn inactive_ad_stack_preserves_gpt_diagnostics_cache_privacy() {
            // Arrange
            let mut settings = settings_with_disabled_ad_templates();
            settings
                .integrations
                .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
                .expect("should enable GPT diagnostics");
            let stub = Arc::new(StubHttpClient::new());
            queue_html_response_with_cache_control(&stub, "no-cache");
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let request = HttpRequest::builder()
                .method(Method::GET)
                .uri("https://ts.example.com/article?ts_console=1")
                .header(header::HOST, "ts.example.com")
                .header("sec-fetch-dest", "document")
                .body(EdgeBody::empty())
                .expect("should build GPT diagnostics request");

            // Act
            let response = run_with_slots(&settings, &services, &[article_slot()], request).await;
            let response_head = response_head(response);

            // Assert
            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, no-store"),
                "active GPT diagnostics should retain cache privacy when server-side ad templates are inactive"
            );
        }

        #[tokio::test]
        async fn inactive_ad_stack_preserves_non_get_and_non_document_cache_policy() {
            let settings = settings_with_disabled_ad_templates();

            for request in [
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("https://ts.example.com/article")
                    .header(header::HOST, "ts.example.com")
                    .header("sec-fetch-dest", "document")
                    .body(EdgeBody::empty())
                    .expect("should build non-GET document request"),
                HttpRequest::builder()
                    .method(Method::GET)
                    .uri("https://ts.example.com/article")
                    .header(header::HOST, "ts.example.com")
                    .header("sec-fetch-dest", "empty")
                    .body(EdgeBody::empty())
                    .expect("should build non-document request"),
            ] {
                // Arrange
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_cache_control(&stub, "no-cache");
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                // Act
                let response =
                    run_with_slots(&settings, &services, &[article_slot()], request).await;
                let response_head = response_head(response);

                // Assert
                assert_eq!(
                    response_head
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some("no-cache"),
                    "inactive server-side ad templates should preserve non-document request policy"
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
                assert!(
                    response
                        .extensions()
                        .get::<crate::response_privacy::TerminalPrivateResponse>()
                        .is_some(),
                    "invalid origin 304 response should remain private after late response effects"
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
                PublisherResponse::PassThrough { .. }
                | PublisherResponse::Stream { .. }
                | PublisherResponse::AssembleTemplate { .. } => {
                    panic!("noneligible origin 304 should remain buffered")
                }
            };
            assert_eq!(
                response.status(),
                StatusCode::NOT_MODIFIED,
                "inactive server-side ad templates should preserve the 304 status"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, max-age=60"),
                "inactive server-side ad templates should apply the browser policy on revalidation"
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
            ] {
                assert_eq!(
                    response
                        .headers()
                        .get(&header_name)
                        .and_then(|value| value.to_str().ok()),
                    Some(expected),
                    "inactive server-side ad templates should preserve {header_name} on revalidation"
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

        #[tokio::test]
        async fn inactive_ad_stack_304_preserves_private_origin_cache_policy() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();

            for cache_control in ["private, max-age=0", "No-Store"] {
                // Arrange
                let stub = Arc::new(StubHttpClient::new());
                stub.push_response_with_headers(
                    304,
                    Vec::new(),
                    vec![("cache-control", cache_control), ("etag", ORIGIN_ETAG)],
                );
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
                    "inactive server-side ad templates should preserve origin {cache_control} policy on revalidation"
                );
            }
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
            Some("public, max-age=60"),
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
            assert!(!body.contains("tsjs-gpt_diagnostics.min.js"));
            assert_eq!(
                body.matches("tsjs-gpt_diagnostics-bootstrap.min.js")
                    .count(),
                1
            );
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
        assert!(active_body.contains("tsjs-gpt_diagnostics.min.js"));
        assert!(!active_body.contains("tsjs-gpt_diagnostics-bootstrap.min.js"));

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
        assert!(!disabled_body.contains("tsjs-gpt_diagnostics.min.js"));
        assert_eq!(
            disabled_body
                .matches("tsjs-gpt_diagnostics-bootstrap.min.js")
                .count(),
            1
        );
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
            assert!(!body.contains("tsjs-gpt_diagnostics.min.js"));
            assert!(!body.contains("tsjs-gpt_diagnostics-bootstrap.min.js"));
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
                    "server_side_key_secret_name": "server-side-key",
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

    #[test]
    fn server_side_ad_stack_runs_only_when_all_auction_gates_pass() {
        let enabled_config = ServerSideAdStackConfig {
            ad_templates_enabled: true,
            auction_enabled: true,
        };
        assert!(
            should_run_server_side_ad_stack(true, true, false, false, true, true, enabled_config,),
            "GET, real navigation, enabled templates, matched slots, and consent should run TS ad stack"
        );

        assert!(
            !should_run_server_side_ad_stack(false, true, false, false, true, true, enabled_config,),
            "non-GET requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, false, false, false, true, true, enabled_config,),
            "non-document requests should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, true, false, true, true, enabled_config,),
            "prefetch requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, true, true, true, enabled_config,),
            "bot requests should skip TS ad stack and injection"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, false, true, enabled_config,),
            "requests with no matching slots should skip TS ad stack"
        );
        assert!(
            !should_run_server_side_ad_stack(true, true, false, false, true, false, enabled_config,),
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
            "disabled [creative_opportunities].enabled switch should skip TS ad stack and injection"
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
    fn ts_console_dynamic_serves_the_non_authoritative_cleanup_asset() {
        let settings = create_test_settings();
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let req = Request::builder()
            .uri("https://publisher.example/static/tsjs=tsjs-gpt_diagnostics-bootstrap.min.js")
            .body(EdgeBody::empty())
            .expect("should build cleanup asset request");

        let response = handle_tsjs_dynamic(&req, &registry).expect("should serve cleanup asset");
        let source = response_body_string(response);

        assert!(source.contains("history.replaceState"));
        assert!(source.contains("ts_console"));
        assert!(!source.contains("sessionStorage"));
        assert!(!source.contains("__tsjs_gpt_diagnostics_active"));
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                }),
            )
            .expect("should update prebid config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");

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
            csp_nonce_observed: None,
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");

        let params = OwnedProcessResponseParams {
            csp_nonce_observed: None,
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
        let params = OwnedProcessResponseParams {
            csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let state = AdBidsState::default();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let state = AdBidsState::default();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            let registry = IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry");
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let services = noop_services();
            let mut params = OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry"),
        );
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));
        let services = noop_services();
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/css")
            .body(EdgeBody::empty())
            .expect("should build response");
        let params = OwnedProcessResponseParams {
            csp_nonce_observed: None,
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
            IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry"),
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
            csp_nonce_observed: None,
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
            IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry"),
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
        let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));

        let make_params = || {
            let ec_context =
                EcContext::new_for_test(None, crate::consent::types::ConsentContext::default());
            OwnedProcessResponseParams {
                csp_nonce_observed: None,
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
            IntegrationRegistry::with_plan(
                &settings,
                Arc::new(
                    crate::auction::compile_auction_plan(&settings)
                        .expect("should compile auction plan"),
                ),
            )
            .expect("should create integration registry"),
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
            csp_nonce_observed: None,
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");
        let bids_script =
            r#"<script>(window.tsjs=window.tsjs||{}).bids=JSON.parse("{}");</script>"#;
        let state = AdBidsState::with_script(bids_script);
        let params = OwnedProcessResponseParams {
            csp_nonce_observed: None,
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");

        // Claim gzip encoding but feed non-gzip bytes. The GzDecoder will
        // error as soon as it tries to read the gzip header.
        let params = OwnedProcessResponseParams {
            csp_nonce_observed: None,
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

        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");

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
            csp_nonce_observed: None,
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
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create integration registry");

        // Small, single-fragment RSC script — placeholder path (not fallback).
        let html = br#"<html><body><script>self.__next_f.push([1,"1:{\"link\":\"https://origin.example.com/page\"}"])</script></body></html>"#;
        let params = OwnedProcessResponseParams {
            csp_nonce_observed: None,
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
            MatchedSlotsContext, build_ad_slots_script, build_auction_request, build_bid_map,
            build_bids_script, build_bids_script_performance_mark, html_escape_for_script,
        };
        use crate::auction::types::{ApsRendererV1, ApsTagType, Bid, BidRenderSourceV1, MediaType};
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
                candidate_id: None,
                candidate_provider: None,
                renderer_reservation_id: None,
                price: Some(price),
                currency: "USD".to_string(),
                creative: None,
                adomain: None,
                bidder: bidder.to_string(),
                returned_seat: None,
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
        fn bid_map_exposes_aps_renderer_and_selected_bid_id_without_debug_adm() {
            let mut bid = make_bid("atf_sidebar_ad", 1.50, "aps", "fallback-ad", "", "");
            bid.bid_id = Some("selected-bid".to_string());
            bid.renderer = Some(BidRenderSourceV1::Aps(ApsRendererV1 {
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
            bid.nurl = None;
            bid.burl = None;
            let winning_bids = HashMap::from([("atf_sidebar_ad".to_string(), bid)]);

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
                candidate_id: None,
                candidate_provider: None,
                renderer_reservation_id: None,
                price: Some(1.50),
                currency: "USD".to_string(),
                creative: Some(creative.to_string()),
                adomain: None,
                bidder: "prebid".to_string(),
                returned_seat: None,
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
                    candidate_id: None,
                    candidate_provider: None,
                    renderer_reservation_id: None,
                    price: Some(1.50),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "thetradedesk".to_string(),
                    returned_seat: None,
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
                    candidate_id: None,
                    candidate_provider: None,
                    renderer_reservation_id: None,
                    price: Some(0.50),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "amazon-aps".to_string(),
                    returned_seat: None,
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
            bid.renderer = Some(BidRenderSourceV1::Aps(ApsRendererV1 {
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
        fn bid_map_falls_back_to_bid_id_when_cache_id_and_ad_id_absent() {
            // Real shape for bidders that return neither a Prebid Cache UUID nor
            // `adid` in the OpenRTB response, but always carry `id` (the bid's own
            // identifier) per spec. Without this fallback the bid reaches the page
            // with no hb_adid, so no targeting key is set and the render bridge
            // never receives a matching `Prebid Request`.
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "atf_sidebar_ad".to_string(),
                Bid {
                    slot_id: "atf_sidebar_ad".to_string(),
                    candidate_id: None,
                    candidate_provider: None,
                    renderer_reservation_id: None,
                    price: Some(1.00),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "example-bidder".to_string(),
                    width: 300,
                    height: 250,
                    nurl: None,
                    burl: None,
                    bid_id: Some("019f7e2a-b45b-70b0-a2d1-b651c430700b".to_string()),
                    ad_id: None,
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
                Some("019f7e2a-b45b-70b0-a2d1-b651c430700b"),
                "should fall back to bid_id when cache_id and ad_id are both absent"
            );
        }

        #[test]
        fn bid_map_omits_hb_adid_when_both_cache_id_and_ad_id_absent() {
            let mut winning_bids = HashMap::new();
            winning_bids.insert(
                "atf_sidebar_ad".to_string(),
                Bid {
                    slot_id: "atf_sidebar_ad".to_string(),
                    candidate_id: None,
                    candidate_provider: None,
                    renderer_reservation_id: None,
                    price: Some(0.50),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "amazon-aps".to_string(),
                    returned_seat: None,
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
                    candidate_id: None,
                    candidate_provider: None,
                    renderer_reservation_id: None,
                    price: None,
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "kargo".to_string(),
                    returned_seat: None,
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
        fn bids_script_performance_mark_is_exact_and_not_yet_wired() {
            let fragment = build_bids_script_performance_mark();
            assert_eq!(
                fragment,
                "(function(){try{window.performance.mark(\"tsjs:bids-script\");}catch(_){}})();"
            );
            assert!(!fragment.contains("__tsjsPerf"));
            assert!(
                !build_bids_script(&serde_json::Map::new()).contains("tsjs:bids-script"),
                "Task 19 owns the coordinated production insertion"
            );
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
            fn provider_name(&self) -> &str {
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
                        returned_seat: None,
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

        #[tokio::test]
        async fn page_bids_format_absent_or_json_returns_json() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            for path_and_format in ["/2024/article", "/2024/article&format=json"] {
                let response = run_page_bids_response(
                    &settings,
                    &orchestrator,
                    &[],
                    make_page_bids_request(path_and_format),
                )
                .await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "should accept page-bids format in `{path_and_format}`"
                );
                assert_eq!(
                    response.headers().get(header::CONTENT_TYPE),
                    Some(&HeaderValue::from_static("application/json")),
                    "should return JSON for `{path_and_format}`"
                );
                assert!(
                    response
                        .extensions()
                        .get::<crate::response_privacy::TerminalPrivateResponse>()
                        .is_some(),
                    "successful per-user page-bids JSON should remain terminal-private"
                );
            }
        }

        #[tokio::test]
        async fn page_bids_format_rejects_removed_unknown_and_empty_values() {
            let settings = settings_with_co();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            for format in ["fragment", "scrpit", ""] {
                let response = run_page_bids_response(
                    &settings,
                    &orchestrator,
                    &[],
                    make_page_bids_request(&format!("/2024/article&format={format}")),
                )
                .await;
                assert_eq!(
                    response.status(),
                    StatusCode::BAD_REQUEST,
                    "should reject page-bids format `{format}`"
                );
            }
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
            settings.auction.providers =
                crate::auction::AuctionConfig::legacy_provider_map(&[AUCTION_ID_TEST_PROVIDER]);
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
            settings.auction.providers =
                crate::auction::AuctionConfig::legacy_provider_map(&[AUCTION_ID_TEST_PROVIDER]);
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

        const CAPTURING_PROVIDER: &str = "request-capturing-provider";

        /// Records the [`AuctionRequest`] the orchestrator dispatched, then
        /// fails its launch so no real transport handle is needed.
        struct RequestCapturingProvider {
            captured: Arc<Mutex<Option<AuctionRequest>>>,
        }

        #[async_trait::async_trait(?Send)]
        impl AuctionProvider for RequestCapturingProvider {
            fn provider_name(&self) -> &str {
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
                "{}\n[auction]\nenabled = true\n\n[auction.providers.{CAPTURING_PROVIDER}]\nprotocol = \"openrtb-2.6\"\nendpoint = \"https://capture.example/openrtb2/auction\"\nrouting = \"all_eligible\"\n\n\
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
