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
    AuctionOrchestrator, DispatchedAuction, ERROR_TYPE_ALL, ERROR_TYPE_HTTP_STATUS,
    ERROR_TYPE_LAUNCH_FAILED, ERROR_TYPE_PARSE_RESPONSE, ERROR_TYPE_TIMEOUT, ERROR_TYPE_TRANSPORT,
    OrchestrationResult,
};
use crate::auction::telemetry::{
    AuctionObservationContext, AuctionSource, AuctionTerminalOutcome, build_auction_events,
    emit_auction_events_best_effort_lazy,
};
use crate::auction::types::{
    AdmRenderSourceV1, AuctionContext, AuctionDecisionSetV1, AuctionDropReasons,
    AuctionIdentityGenerator, AuctionRequest, AuctionSlotFailureReason, BaselinePbsCacheSourceV1,
    Bid, BidRenderSourceV1, BrowserAuctionBidV1, BrowserAuctionProjectionV1, BrowserAuctionSlotV1,
    DeviceInfo, PublisherInfo, SiteInfo, SlotAuctionDecisionV1, SystemAuctionIdentityGenerator,
    UserInfo, mint_response_unique_base64url_identity,
};
use crate::cache_policy::{EdgeCacheHeader, cache_control_headers_are_private_or_no_store};
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
use crate::integrations::aps::ApsConfig;
use crate::platform::{
    GeoInfo, PlatformBackendSpec, PlatformHttpRequest, RuntimeServices, VarySpec,
    contains_publisher_esi_directive,
};
use crate::price_bucket::{PriceGranularity, price_bucket};
use crate::response_privacy::{
    enforce_synthesized_html_cache_privacy, enforce_terminal_private_cache_privacy,
};
use crate::rsc_flight::RscFlightUrlRewriter;
use crate::settings::{
    AUCTION_DEBUG_METADATA_ALLOWLIST, AUCTION_DEBUG_UPSTREAM_METADATA_KEYS,
    AuctionDebugCommentFormat, AuctionDebugCommentOptions, AuctionDebugCommentVerbosity, Settings,
};
use crate::streaming_processor::{
    BodyStreamDecoder, BodyStreamEncoder, Compression, PipelineConfig, STREAM_CHUNK_SIZE,
    StreamProcessor, StreamingPipeline,
};
use crate::streaming_replacer::create_url_replacer;

const SUPPORTED_ENCODING_VALUES: [&str; 3] = ["gzip", "deflate", "br"];
const DEFAULT_PUBLISHER_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(15);
const HEADER_X_TS_TEMPLATE_CACHE: &str = "x-ts-template-cache";
const HEADER_X_TS_ASSEMBLY: &str = "x-ts-assembly";
const APS_PUBLISHER_FRAME_POLICY: &str = "frame-ancestors 'self'";

fn append_aps_publisher_frame_policy(response: &mut Response<EdgeBody>, aps_enabled: bool) {
    if aps_enabled {
        response.headers_mut().append(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(APS_PUBLISHER_FRAME_POLICY),
        );
    }
}

fn ensure_aps_publisher_frame_policy(response: &mut Response<EdgeBody>, aps_enabled: bool) {
    if !aps_enabled {
        return;
    }
    let already_present = response
        .headers()
        .get_all(header::CONTENT_SECURITY_POLICY)
        .iter()
        .any(|value| value.as_bytes() == APS_PUBLISHER_FRAME_POLICY.as_bytes());
    if !already_present {
        append_aps_publisher_frame_policy(response, true);
    }
}

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
    if fraction.is_some_and(|fraction| {
        fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    }) {
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

/// Exact content-addressed TSJS release transport.
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

    let path = req.uri().path();
    if (req.method() != Method::GET && req.method() != Method::HEAD) || !path.starts_with(PREFIX) {
        return Ok(tsjs_not_found_response());
    }
    let filename = &path[PREFIX.len()..];

    if filename == "tsjs-first-display.min.js" {
        let Some((mask, requested_hash)) = req
            .uri()
            .query()
            .and_then(parse_first_display_artifact_query)
        else {
            return Ok(tsjs_not_found_response());
        };
        let Some(artifact) = integration_registry.tsjs_first_display_artifact(mask, requested_hash)
        else {
            return Ok(tsjs_not_found_response());
        };
        let mut resp = serve_precomputed_tsjs_artifact(artifact, req);
        strip_tsjs_head_body(&mut resp, req.method());
        apply_tsjs_success_headers(&mut resp);
        return Ok(resp);
    }

    let Some(requested_hash) = req.uri().query().and_then(parse_tsjs_hash_query) else {
        return Ok(tsjs_not_found_response());
    };

    if filename == "tsjs-unified.min.js" {
        let Some(artifact) = integration_registry.tsjs_static_artifact(requested_hash) else {
            return Ok(tsjs_not_found_response());
        };
        let mut resp = serve_precomputed_tsjs_artifact(artifact, req);
        strip_tsjs_head_body(&mut resp, req.method());
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
            let mut resp = serve_static_with_etag(
                content,
                req,
                "application/javascript; charset=utf-8",
                edge_header,
            );
            strip_tsjs_head_body(&mut resp, req.method());
            apply_tsjs_success_headers(&mut resp);
            return Ok(resp);
        }
    }

    Ok(tsjs_not_found_response())
}

fn strip_tsjs_head_body(response: &mut Response<EdgeBody>, method: &Method) {
    if method == Method::HEAD {
        *response.body_mut() = EdgeBody::empty();
    }
}

fn valid_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_tsjs_hash_query(query: &str) -> Option<&str> {
    let hash = query.strip_prefix("v=")?;
    valid_lowercase_sha256(hash).then_some(hash)
}

fn parse_first_display_artifact_query(query: &str) -> Option<(u16, &str)> {
    let (mask, hash) = query.strip_prefix("m=")?.split_once("&v=")?;
    if mask.len() != 4
        || !mask
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !valid_lowercase_sha256(hash)
    {
        return None;
    }
    let mask = u16::from_str_radix(mask, 16).ok()?;
    (mask & 1 == 1).then_some((mask, hash))
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

fn serve_precomputed_tsjs_artifact(
    artifact: &crate::tsjs::TsjsStaticArtifactV1,
    req: &Request<EdgeBody>,
) -> Response<EdgeBody> {
    let etag = format!("\"sha256-{}\"", artifact.hash());
    let not_modified = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag);
    let builder = Response::builder()
        .header(
            header::CACHE_CONTROL,
            "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
        )
        .header("surrogate-control", "max-age=300")
        .header(header::ETAG, etag)
        .header(header::VARY, "Accept-Encoding");
    if not_modified {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(EdgeBody::empty())
            .expect("should build 304 TSJS response");
    }

    builder
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )
        .body(EdgeBody::from_bytes(artifact.body().clone()))
        .expect("should build TSJS response")
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
                render_trace_overlay: params.render_trace_overlay,
                assembly_mode: effective_assembly_mode(
                    settings,
                    params.template_cache_key.is_some(),
                ),
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
            render_trace_overlay: params.render_trace_overlay,
            assembly_mode: effective_assembly_mode(
                params.settings,
                params.shared_template_authorized,
            ),
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

/// The single inert marker stored in a reader-neutral C2 template.
pub const AD_ASSEMBLY_SEAM: &str = "<!--ts-ad-seam-->";

/// Transform-owned stand-in placed at the parser-authenticated body seam.
///
/// The completed document replaces this with either [`AD_ASSEMBLY_SEAM`] or
/// the reader's inline boot fragment only after publisher collisions, ESI, and
/// response-bound CSP nonces have been checked.
pub(crate) const TEMPLATE_SEAM_PLACEHOLDER: &str = "<!--ts-seam-slot-->";

fn configured_assembly_mode(settings: &Settings) -> AssemblyMode {
    settings
        .creative_opportunities
        .as_ref()
        .filter(|config| config.enabled)
        .map(CreativeOpportunitiesConfig::assembly_mode)
        .unwrap_or_default()
}

fn mode_emits_seam_marker(mode: AssemblyMode) -> bool {
    match mode {
        AssemblyMode::Inline => false,
        AssemblyMode::Esi => true,
    }
}

fn effective_assembly_mode(settings: &Settings, shared_template_authorized: bool) -> AssemblyMode {
    let configured = configured_assembly_mode(settings);
    if matches!(configured, AssemblyMode::Inline) || shared_template_authorized {
        configured
    } else {
        log::debug!(
            "assembly mode {configured:?} is unavailable for this response; falling back to inline"
        );
        AssemblyMode::Inline
    }
}

fn template_injection(mode: AssemblyMode) -> BodyCloseInjection {
    match mode {
        AssemblyMode::Inline => BodyCloseInjection::None,
        AssemblyMode::Esi => BodyCloseInjection::Marker(TEMPLATE_SEAM_PLACEHOLDER.to_string()),
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
    assembly_mode: AssemblyMode,
    csp_nonce_observed: Option<Arc<AtomicBool>>,
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
    .with_body_close(template_injection(params.assembly_mode))
    .with_render_trace_overlay(params.render_trace_overlay)
    .with_csp_nonce_observer(
        matches!(params.assembly_mode, AssemblyMode::Esi)
            .then_some(params.csp_nonce_observed)
            .flatten(),
    )
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
    /// Price granularity used to bucket bids in the browser auction projection.
    pub(crate) price_granularity: PriceGranularity,
    /// Whether to omit Trusted Server's automatic `DataDome` client-side tag.
    pub(crate) suppress_datadome_client_side_tag: bool,
    /// Request-scoped conditional diagnostics delivery decision.
    pub(crate) gpt_diagnostics:
        Option<crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision>,
    /// Server-owned request-scoped render-trace overlay decision.
    pub(crate) render_trace_overlay: bool,
    /// Set by the transform when publisher markup carries a response-bound CSP nonce.
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
                    Cow::Owned(seam_script_for(&params, settings, integration_registry))
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
                    integration_registry,
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
                        browser_slots_json: params.ad_slots_script.as_deref(),
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
            let seam = seam_script_for(&params, settings, integration_registry);
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
    integration_registry: &IntegrationRegistry,
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
    let seam = seam_script_for(params, settings, integration_registry);

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

/// Build the complete hard-cutover boot fragment inserted into a reader-neutral
/// C2 template. The template contains neither request projection nor runtime
/// bytes; both arrive atomically at this one per-reader seam.
fn seam_script_for(
    params: &OwnedProcessResponseParams,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
) -> String {
    let state = params
        .ad_bids_state
        .lock()
        .expect("should lock C2 boot projection")
        .clone();
    let (debug_comment, projection_json) = match state.as_deref() {
        Some(value) if value.starts_with("<!-- ts-debug:") => value.rsplit_once('\n').map_or(
            (Some(value), crate::tsjs::EMPTY_AUCTION_PROJECTION_JSON_V1),
            |(debug, json)| (Some(debug), json),
        ),
        Some(value) => (None, value),
        None => (None, crate::tsjs::EMPTY_AUCTION_PROJECTION_JSON_V1),
    };
    let diagnostics_requested = params
        .gpt_diagnostics
        .as_ref()
        .is_some_and(crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision::active);
    let creative = integration_registry.tsjs_creative_boot();
    let selection = crate::integrations::TsjsCatalogSelectionV1 {
        creative_enabled: creative.enabled,
        creative_click_guard: creative.click_guard,
        creative_render_guard: creative.render_guard,
        gpt_diagnostics_active: diagnostics_requested,
        render_trace_overlay: params.render_trace_overlay,
    };
    let module_ids = integration_registry.tsjs_catalog_module_ids(selection);
    let diagnostics_active = module_ids.contains(&"gpt_diagnostics");
    let integration_configs = match integration_registry.tsjs_integration_configs_v1(&module_ids) {
        Ok(configs) => configs,
        Err(error) => {
            log::error!("invalid C2 TSJS integration config projection: {error:?}");
            return String::new();
        }
    };
    let publisher_origin = request_origin(&params.request_scheme, &params.request_host);
    let boot = crate::tsjs::tsjs_bootstrap_fragment_v1(
        crate::tsjs::TsjsBootScriptConfigV1 {
            module_ids: &module_ids,
            integration_configs: &integration_configs,
            auction_projection_json: projection_json,
            creative,
            render_trace_overlay: params.render_trace_overlay,
            gpt_diagnostics_active: diagnostics_active,
        },
        &publisher_origin,
    )
    .or_else(|error| {
        log::error!("invalid C2 TSJS boot projection: {error:?}");
        crate::tsjs::tsjs_bootstrap_fragment_v1(
            crate::tsjs::TsjsBootScriptConfigV1 {
                module_ids: &module_ids,
                integration_configs: &integration_configs,
                auction_projection_json: crate::tsjs::EMPTY_AUCTION_PROJECTION_JSON_V1,
                creative,
                render_trace_overlay: params.render_trace_overlay,
                gpt_diagnostics_active: diagnostics_active,
            },
            &publisher_origin,
        )
    })
    .unwrap_or_default();

    let mut seam = String::new();
    if let Some(debug_comment) = debug_comment {
        seam.push_str(debug_comment);
        seam.push('\n');
    }
    let document_state = crate::integrations::IntegrationDocumentState::default();
    let fallback_origin_host = settings.publisher.origin_host();
    let context = crate::integrations::IntegrationHtmlContext {
        request_host: &params.request_host,
        request_scheme: &params.request_scheme,
        origin_host: if params.origin_host.is_empty() {
            &fallback_origin_host
        } else {
            &params.origin_host
        },
        document_state: &document_state,
    };
    for insert in integration_registry.head_inserts(&context) {
        seam.push_str(&insert);
    }
    seam.push_str(&boot);
    seam
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
    render_trace_overlay: bool,
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
        render_trace_overlay,
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
            let integration_registry = integration_registry.clone();
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
                            browser_slots_json: params.ad_slots_script.as_deref(),
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

                let seam = seam_script_for(&params, &settings, &integration_registry);
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
        render_trace_overlay: params.render_trace_overlay,
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

/// Returns true only when the publisher request should run the full
/// server-side ad stack: auction dispatch plus initial ad-slot injection.
pub(crate) fn should_run_server_side_ad_stack(
    is_get: bool,
    is_navigation: bool,
    is_prefetch: bool,
    is_bot: bool,
    has_active_matched_slots: bool,
    consent_allows_auction: bool,
    auction_enabled: bool,
) -> bool {
    is_get
        && is_navigation
        && !is_prefetch
        && !is_bot
        && has_active_matched_slots
        && consent_allows_auction
        && auction_enabled
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

/// Shared, single-writer initial browser projection for HTML processing and
/// per-reader C2 assembly.
#[derive(Clone, Default)]
pub(crate) struct AdBidsState {
    projection: Arc<Mutex<Option<String>>>,
}

impl core::ops::Deref for AdBidsState {
    type Target = Mutex<Option<String>>;

    fn deref(&self) -> &Self::Target {
        &self.projection
    }
}

#[cfg(test)]
impl AdBidsState {
    fn with_script(value: &str) -> Self {
        let state = Self::default();
        *state.lock().expect("should lock projection state") = Some(value.to_string());
        state
    }
}

impl AdBidsState {
    pub(crate) fn script_cell(&self) -> &Arc<Mutex<Option<String>>> {
        &self.projection
    }

    fn prepend_to_script(&self, comment: &str) {
        let mut state = self
            .lock()
            .expect("should lock projection state for debug prefix");
        match &mut *state {
            Some(projection) => *projection = format!("{comment}\n{projection}"),
            None => *state = Some(comment.to_string()),
        }
    }
}

/// Write the one exact initial browser projection into the pre-core boot state.
pub(crate) fn write_projection_to_state(
    result: &OrchestrationResult,
    price_granularity: PriceGranularity,
    ad_bids_state: &AdBidsState,
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
    let fallback_slots = browser_slots_json
        .and_then(|json| serde_json::from_str::<Vec<BrowserAuctionSlotV1>>(json).ok());
    let projected = coordinated_cutover_v1::build_browser_auction_projection_v1(
        result,
        price_granularity,
        settings,
        request_origin,
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
            let mut fallback_results: Vec<SlotAuctionDecisionV1> = result
                .decision_set
                .results
                .iter()
                .map(|decision| match decision {
                    SlotAuctionDecisionV1::Winner { slot, .. } => SlotAuctionDecisionV1::Failed {
                        slot: slot.clone(),
                        reason: AuctionSlotFailureReason::WinnerNotRenderable,
                    },
                    decision => decision.clone(),
                })
                .collect();
            let fallback_slots = fallback_slots
                .filter(|slots| {
                    slots.len() == fallback_results.len()
                        && slots
                            .iter()
                            .zip(&fallback_results)
                            .all(|(slot, decision)| slot.slot == decision.slot())
                })
                .unwrap_or_else(|| {
                    // Without exact server-generated placement coverage, the
                    // only valid browser-boot fallback is the empty projection.
                    fallback_results.clear();
                    Vec::new()
                });
            let projection = BrowserAuctionProjectionV1 {
                version: 1,
                auction: AuctionDecisionSetV1 {
                    version: 1,
                    auction_id: browser_auction_id,
                    results: fallback_results,
                },
                slots: fallback_slots,
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

/// Return only the closed, typed local drop-reason map emitted by TS itself.
/// Unknown names, non-integer counts, zero counts, and mixed-validity maps are
/// rejected as a whole rather than copying provider-controlled JSON into HTML.
fn validated_drop_reasons(
    metadata: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let reasons =
        serde_json::from_value::<AuctionDropReasons>(metadata.get("drop_reasons")?.clone()).ok()?;
    if reasons.is_empty() || reasons.values().any(|count| *count == 0) {
        return None;
    }
    serde_json::to_value(reasons).ok()
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

    if selected("drop_reasons")
        && let Some(value) = validated_drop_reasons(metadata)
    {
        safe.insert("drop_reasons".to_string(), value);
    }
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

/// Borrowed dependencies of the auction collect step.
///
/// Split from the per-auction state above because `dispatched` and `telemetry`
/// are moved out at collect time while these stay live for the rest of the
/// streaming loop.
struct AuctionCollectDeps<'a> {
    price_granularity: PriceGranularity,
    ad_bids_state: &'a AdBidsState,
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
        prepend_auction_debug_comment(
            "stream",
            &result,
            ad_bids_state,
            &settings.debug.auction_html_comment_options,
        );
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
    edge_header: EdgeCacheHeader,
) -> Result<PublisherResponse, Report<TrustedServerError>> {
    log::debug!("Proxying request to publisher_origin");

    // Adapter fallbacks prepare this before EC/cookie handling. Keep this
    // idempotent call as a direct-handler safety net and for focused tests.
    let gpt_diagnostics =
        crate::integrations::gpt_diagnostics::prepare_request(settings, &mut req)?;
    let render_trace_overlay = crate::trace_cookie::render_trace_overlay_active(&req);
    let aps_enabled = settings
        .integration_config::<ApsConfig>("aps")?
        .is_some_and(|config| config.enabled);

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

    let matched_slots = if is_get {
        settings
            .creative_opportunities
            .as_ref()
            .filter(|co_config| co_config.enabled)
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
        auction.orchestrator.is_enabled(),
    );
    let should_run_auction = should_run_ad_stack;
    // Diagnostic: shows which gate suppresses the server-side auction. Pair with
    // the `EC context: ... jurisdiction=...` line from EC-context construction
    // when `consent_allows_auction=false`.
    log::debug!(
        "server-side ad-stack gate: is_get={is_get} is_navigation={is_navigation} \
         is_prefetch={is_prefetch} is_bot={is_bot} matched_slots={} \
         consent_allows_auction={consent_allows_auction} \
         orchestrator_enabled={} -> should_run_auction={should_run_auction}",
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
            let dispatched = auction
                .orchestrator
                .dispatch_auction(&auction_request, &auction_context)
                .await;
            auction_request_for_telemetry = Some(auction_request);
            auction_observation = Some(observation);
            Some(dispatched)
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

    // Recorded before the request is consumed by the origin send: the template cache gate
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

    // Exact ordered placements are request state. Inline processing carries
    // them in the head boot; C2 carries the same JSON in its per-reader seam.
    let browser_slots_json = should_run_ad_stack
        .then(|| {
            settings
                .creative_opportunities
                .as_ref()
                .map(|config| build_browser_slots_json(&matched_slots, config, &request_path))
        })
        .flatten();
    let seam_ad_slots = browser_slots_json.clone();

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
                    let mut response = build_cached_template_response(&entry, reader_compression)?;
                    ensure_aps_publisher_frame_policy(&mut response, aps_enabled);
                    let mut params = build_template_assembly_params(
                        &entry,
                        settings,
                        request_host,
                        request_scheme,
                        price_granularity,
                        ad_bids_state.clone(),
                        render_trace_overlay,
                    );
                    params.seam_ad_slots = seam_ad_slots.clone();
                    params.ad_slots_script = browser_slots_json.clone();
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
        enforce_terminal_private_cache_privacy(&mut response);
        crate::integrations::gpt_diagnostics::finalize_response(&gpt_diagnostics, &mut response);
        append_aps_publisher_frame_policy(&mut response, aps_enabled);
        return Ok(PublisherResponse::Buffered(response));
    }

    crate::integrations::gpt_diagnostics::finalize_response(&gpt_diagnostics, &mut response);
    append_aps_publisher_frame_policy(&mut response, aps_enabled);

    // A request key only authorizes lookup. The origin response must separately
    // prove that the transformed representation is safe and fresh enough to
    // store before the reservation becomes an authorized C2 write.
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
            Ok(ttl) => Instant::now()
                .checked_add(ttl)
                .map(|expires_at| AuthorizedTemplateStore {
                    reservation,
                    expires_at,
                }),
        }
    });
    let policy_headers = if template_cache_key.is_some() {
        match replayable_policy_headers(response.headers()) {
            Ok(headers) => headers,
            Err(reason) => {
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
    let ad_slots_script = browser_slots_json.clone();

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
    // no per-navigation browser auction projection is injected, so forcing private
    // here would needlessly strip shared cacheability from ordinary publisher
    // HTML. Applies regardless of the auction *outcome* (empty bids still inject
    // per-user slot state). The separate EC-cookie cache net in the adapter's
    // `finalize_response` keeps first-visit identity responses private.
    let origin_content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let assembled_response_must_be_private = template_cache_key.is_some();
    if is_html_content_type(origin_content_type) {
        if should_run_ad_stack || assembled_response_must_be_private {
            enforce_synthesized_html_cache_privacy(&mut response);
        } else if is_get
            && is_navigation
            && !is_prefetch
            && !is_bot
            && consent_allows_auction
            && response.status() == StatusCode::OK
        {
            // Structurally inactive server-side ad templates use the short
            // browser policy. Request-scoped skips retain the origin policy
            // because another request for the same URL can still render ads.
            if !response_cache_control_is_private_or_no_store(&response) {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("max-age=60"),
                );
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

    fn cache_source_from_legacy_bid(bid: &Bid) -> Option<BidRenderSourceV1> {
        Some(BidRenderSourceV1::PbsCache(BaselinePbsCacheSourceV1 {
            version: 1,
            cache_id: bid
                .cache_id
                .as_deref()
                .filter(|value| !value.is_empty())?
                .to_string(),
            cache_host: bid
                .cache_host
                .as_deref()
                .filter(|value| !value.is_empty())?
                .to_string(),
            cache_path: bid
                .cache_path
                .as_deref()
                .filter(|value| !value.is_empty())?
                .to_string(),
            width: bid.width,
            height: bid.height,
        }))
    }

    fn project_render_source(
        bid: &Bid,
        settings: &Settings,
        request_origin: &str,
    ) -> Option<BidRenderSourceV1> {
        match (&bid.renderer, &bid.creative, &bid.cache_id) {
            (Some(source), None, None)
                if bid.cache_host.is_none()
                    && bid.cache_path.is_none()
                    && !matches!(source, BidRenderSourceV1::PbsCache(_)) =>
            {
                Some(source.clone())
            }
            (None, Some(raw_creative), None)
                if bid.cache_host.is_none() && bid.cache_path.is_none() =>
            {
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
            (None, None, Some(_)) => cache_source_from_legacy_bid(bid),
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
            let Some(render_source) = project_render_source(bid, settings, request_origin) else {
                projected_results.push(SlotAuctionDecisionV1::Failed {
                    slot: slot.clone(),
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                });
                continue;
            };
            let renderer_reservation_id = match &render_source {
                BidRenderSourceV1::Aps(_) | BidRenderSourceV1::Adm(_) => {
                    let Some(id) = mint_response_unique_base64url_identity(
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
                    Some(id)
                }
                BidRenderSourceV1::PbsCache(_) => None,
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
        if slots.len() != canonical.projection.auction.results.len() {
            return Err(Report::new(TrustedServerError::Auction {
                message: "Browser auction slots must cover every decision".to_string(),
            }));
        }
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

/// Why a response cannot enter the shared transformed-template cache.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
pub(crate) enum TemplateCacheBypassReason {
    #[display("assembly mode is inline")]
    InlineMode,
    #[display("origin response carries Set-Cookie")]
    OriginSetCookie,
    #[display("origin cache policy is not eligible for C2 sharing")]
    OriginNotShareable,
    #[display("origin cache policy is malformed")]
    MalformedCachePolicy,
    #[display("origin response has no positive shared freshness")]
    NoPositiveFreshness,
    #[display("request carried Authorization")]
    AuthorizedRequest,
    #[display("status was not 200 OK")]
    NonOkStatus,
    #[display("content type is not text/html")]
    NotHtml,
    #[display("origin representation headers are ambiguous or malformed")]
    MalformedRepresentationHeaders,
    #[display("origin content encoding is not supported by the template transform")]
    UnsupportedContentEncoding,
    #[display("request carried Cookie and the origin's Vary does not cover it")]
    CookieForwarded,
    #[display("origin varies on {_0}, which the cache key does not cover")]
    VaryNotCovered(VaryGap),
    #[display("origin sent Vary: *, so no request key can select this response")]
    VaryWildcard,
    #[display("origin sent Vary: Cookie, contradicting cookie independence")]
    VaryCookie,
    #[display("origin CSP contains a response-bound nonce")]
    CspNonce,
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
                "max-age"
                    if argument.and_then(|value| parse_delta_seconds(value).ok()) != Some(0) =>
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VaryGap(Vec<String>);

impl core::fmt::Display for VaryGap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0.join(", "))
    }
}

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
    template_cache_ttl(
        mode,
        request_had_authorization,
        cookie_disqualifies,
        status,
        content_type,
        response_headers,
        &TemplateCachePolicy::for_test(key_vary, Duration::from_secs(60)),
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
    let digits = match (value.starts_with('"'), value.ends_with('"')) {
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
        for directive in value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
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
    let freshness = surrogate_control_freshness(headers)?
        .or(standard_freshness)
        .ok_or(TemplateCacheBypassReason::NoPositiveFreshness)?;
    let age = single_header_value(headers, header::AGE)?
        .map(parse_delta_seconds)
        .transpose()?
        .unwrap_or(0);
    let apparent_age = date
        .and_then(|date| now.duration_since(date).ok())
        .unwrap_or_default();
    let remaining = freshness
        .checked_sub(Duration::from_secs(age).max(apparent_age))
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
    if crate::response_privacy::CDN_CACHE_HEADERS
        .iter()
        .filter(|name| **name != "surrogate-control")
        .any(|name| response_headers.contains_key(*name))
    {
        return Err(TemplateCacheBypassReason::OriginNotShareable);
    }
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
#[must_use]
pub fn page_bids_preflight_denied() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Forbidden"));
    *response.status_mut() = StatusCode::FORBIDDEN;
    enforce_terminal_private_cache_privacy(&mut response);
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

    // The [auction].enabled switch and a consent denial disable the
    // entire server-side ad stack. In those states the endpoint returns no slots,
    // so the coordinated runtime cannot create or refresh GPT placements.
    // Bot/prefetch requests, by contrast,
    // keep their slot definitions (the placement structure is unchanged) but
    // skip the live auction, matching the existing bot/prefetch behaviour.
    let ad_stack_enabled = auction_enabled && consent_allows_auction;

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
        NoopSecretStore, StubHttpClient, build_services_with_http_client,
        build_services_with_secret_http_client_and_client_ip, noop_services,
        noop_services_with_telemetry_sink,
    };
    use crate::test_support::tests::{
        bootstrap_transport, crate_test_settings_str, create_test_settings,
    };
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

    fn boot_auction_id(html: &str) -> String {
        bootstrap_transport(html)["boot"]["auctionProjection"]["auction"]["auctionId"]
            .as_str()
            .expect("sealed auction projection should carry an auction id")
            .to_owned()
    }

    fn boot_gpt_diagnostics_active(html: &str) -> bool {
        bootstrap_transport(html)["boot"]["diagnostics"]["gpt"]["active"]
            .as_bool()
            .expect("sealed diagnostics should carry the GPT activation state")
    }

    mod coordinated_cutover_projection_tests {
        use std::collections::{HashMap, VecDeque};
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::*;
        use crate::auction::types::{
            AdmRenderSourceV1, ApsRendererV1, ApsTagType, AuctionIdentityGenerator,
            AuctionSlotFailureReason, BidRenderSourceV1, SlotAuctionDecisionV1,
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
            assert_eq!(
                bid.renderer_reservation_id.as_deref(),
                Some("r1_BwcHBwcHBwcHBwcHBwcHBw")
            );
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
                    .as_deref()
                    .expect("ADM bid should have reservation")
            );
        }

        #[test]
        fn initial_html_state_uses_the_exact_projection_json_without_a_legacy_script() {
            let result = result_with_winners(vec![tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 2.75)]);
            let state = AdBidsState::default();
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
        fn browser_boot_attachment_requires_slots_for_every_decision() {
            let result = result_with_winners(vec![tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 2.75)]);
            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                Some("a1_AAAAAAAAAAAA"),
                &SystemAuctionIdentityGenerator,
            )
            .expect("direct projection should be canonical before boot slots attach");

            let error = coordinated_cutover_v1::attach_browser_slots_v1(
                canonical,
                Vec::new(),
                "https://publisher.example",
            )
            .expect_err("browser boot must never pair decisions with an empty slot list");
            assert!(error.to_string().contains("cover every decision"));
        }

        #[test]
        fn initial_projection_failure_preserves_exact_slot_coverage() {
            let result = result_with_winners(vec![tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 2.75)]);
            let state = AdBidsState::default();
            let slots = serde_json::to_string(&vec![BrowserAuctionSlotV1 {
                slot: "slot-1".to_string(),
                gam_unit_path: "/123/slot-1".to_string(),
                div_id: "div-slot-1".to_string(),
                formats: vec![[300, 250]],
                targeting: BTreeMap::new(),
            }])
            .expect("slot projection should serialize");

            let delivered = write_projection_to_state(
                &result,
                PriceGranularity::Dense,
                &state,
                &Settings::default(),
                "not-an-origin",
                Some(&slots),
            );

            assert!(delivered.is_empty());
            let stored = state
                .lock()
                .expect("should lock projection state")
                .clone()
                .expect("should store fail-closed projection JSON");
            let projection: BrowserAuctionProjectionV1 =
                serde_json::from_str(&stored).expect("fallback should remain contract-shaped");
            assert_eq!(projection.slots.len(), projection.auction.results.len());
            assert_eq!(projection.slots[0].slot, "slot-1");
            assert!(matches!(
                projection.auction.results[0],
                SlotAuctionDecisionV1::Failed {
                    reason: AuctionSlotFailureReason::WinnerNotRenderable,
                    ..
                }
            ));
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
        fn cache_projection_preserves_native_coordinates_without_policy_or_reservation() {
            let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
            bid.renderer = None;
            bid.cache_id = Some("f47447a0-b759-4f2f-9887-af458b79b570".to_string());
            bid.cache_host = Some("cache.example:8443".to_string());
            bid.cache_path = Some("/pbc/v1/cache/opaque%2Fpath".to_string());
            let result = result_with_winners(vec![bid]);
            let generator = ScriptedIdentityGenerator::new([]);

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result,
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                &generator,
            )
            .expect("valid cache winner should project");

            assert_eq!(generator.count.load(Ordering::SeqCst), 0);
            assert_eq!(
                serde_json::to_value(&canonical.projection.bids[0])
                    .expect("cache projection should serialize"),
                serde_json::json!({
                    "candidateId": "AAAAAAAAAAAA",
                    "slot": "slot-1",
                    "provider": "prebid",
                    "upstreamBidId": "upstream-slot-1",
                    "cpm": 1.5,
                    "currency": "USD",
                    "targeting": {"hb_bidder": "example_bidder", "hb_pb": "1.50"},
                    "renderSource": {
                        "type": "pbs_cache",
                        "version": 1,
                        "cacheId": "f47447a0-b759-4f2f-9887-af458b79b570",
                        "cacheHost": "cache.example:8443",
                        "cachePath": "/pbc/v1/cache/opaque%2Fpath",
                        "width": 300,
                        "height": 250
                    }
                })
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
                "f47447a0-b759-4f2f-9887-af458b79b570"
            );
        }

        #[test]
        fn projection_rejects_adm_with_any_coexisting_cache_coordinate() {
            for extra in ["id", "host", "path"] {
                let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
                bid.renderer = None;
                bid.creative = Some("<div>creative</div>".to_string());
                if extra == "id" {
                    bid.cache_id = Some("f47447a0-b759-4f2f-9887-af458b79b570".to_string());
                }
                if extra == "host" {
                    bid.cache_host = Some("cache.example".to_string());
                }
                if extra == "path" {
                    bid.cache_path = Some("/pbc/v1/cache".to_string());
                }
                let generator = ScriptedIdentityGenerator::new([]);
                let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                    &result_with_winners(vec![bid]),
                    PriceGranularity::Dense,
                    &Settings::default(),
                    "https://publisher.example",
                    None,
                    &generator,
                )
                .expect("ambiguous ADM source should remain attributable");

                assert!(canonical.projection.bids.is_empty(), "extra {extra}");
                assert_eq!(generator.count.load(Ordering::SeqCst), 0, "extra {extra}");
                assert_eq!(
                    canonical.projection.auction.results[0],
                    SlotAuctionDecisionV1::Failed {
                        slot: "slot-1".to_string(),
                        reason: AuctionSlotFailureReason::WinnerNotRenderable,
                    },
                    "extra {extra}"
                );
            }
        }

        #[test]
        fn rejected_adm_does_not_fall_through_to_coexisting_cache_coordinates() {
            let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
            bid.renderer = None;
            bid.creative = Some("x".repeat(1024 * 1024 + 1));
            bid.cache_id = Some("cache-id".to_string());
            bid.cache_host = Some("cache.example".to_string());
            bid.cache_path = Some("/pbc/v1/cache".to_string());
            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result_with_winners(vec![bid]),
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                &ScriptedIdentityGenerator::new([]),
            )
            .expect("rejected ADM should remain an attributable slot failure");

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
        fn cache_projection_requires_all_three_native_coordinates() {
            for missing in ["id", "host", "path"] {
                let mut bid = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 1.5);
                bid.renderer = None;
                bid.creative = None;
                bid.cache_id = (missing != "id").then(|| "cache-id".to_string());
                bid.cache_host = (missing != "host").then(|| "cache.example".to_string());
                bid.cache_path = (missing != "path").then(|| "/pbc/v1/cache".to_string());
                let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                    &result_with_winners(vec![bid]),
                    PriceGranularity::Dense,
                    &Settings::default(),
                    "https://publisher.example",
                    None,
                    &ScriptedIdentityGenerator::new([]),
                )
                .expect("incomplete cache coordinates should remain attributable");

                assert!(canonical.projection.bids.is_empty(), "missing {missing}");
                assert!(matches!(
                    canonical.projection.auction.results[0],
                    SlotAuctionDecisionV1::Failed {
                        reason: AuctionSlotFailureReason::WinnerNotRenderable,
                        ..
                    }
                ));
            }
        }

        #[test]
        fn duplicate_cache_ids_remain_slot_scoped_and_never_enter_reservation_identity() {
            let mut first = tagged_adm_bid("slot-1", "AAAAAAAAAAAA", 2.0);
            first.renderer = None;
            first.creative = None;
            first.cache_id = Some("shared-cache-id".to_string());
            first.cache_host = Some("Cache.EXAMPLE:443".to_string());
            first.cache_path = Some("/opaque//path%2Fsegment".to_string());
            let mut second = tagged_adm_bid("slot-2", "BBBBBBBBBBBB", 1.0);
            second.renderer = None;
            second.creative = None;
            second.cache_id = first.cache_id.clone();
            second.cache_host = Some("other-cache.example".to_string());
            second.cache_path = Some("/another/path".to_string());

            let canonical = coordinated_cutover_v1::build_browser_auction_projection_v1(
                &result_with_winners(vec![first, second]),
                PriceGranularity::Dense,
                &Settings::default(),
                "https://publisher.example",
                None,
                &ScriptedIdentityGenerator::new([]),
            )
            .expect("duplicate native cache ids should not collide as renderer reservations");

            assert_eq!(canonical.projection.bids.len(), 2);
            assert!(
                canonical
                    .projection
                    .bids
                    .iter()
                    .all(|bid| bid.renderer_reservation_id.is_none())
            );
            let encoded = serde_json::to_value(&canonical.projection)
                .expect("cache projection should serialize");
            assert_eq!(
                encoded["bids"][0]["renderSource"]["cacheHost"],
                "Cache.EXAMPLE:443"
            );
            assert_eq!(
                encoded["bids"][0]["renderSource"]["cachePath"],
                "/opaque//path%2Fsegment"
            );

            let direct: serde_json::Value = serde_json::from_slice(
                &crate::auction::formats::coordinated_cutover_v1::serialize_trusted_server_auction_response_v1(
                    &canonical,
                )
                .expect("direct wire should serialize"),
            )
            .expect("direct wire should be JSON");
            let direct_bids = direct["seatbid"]
                .as_array()
                .expect("direct response should contain seats")
                .iter()
                .flat_map(|seat| {
                    seat["bid"]
                        .as_array()
                        .expect("each direct seat should contain bids")
                })
                .collect::<Vec<_>>();
            assert_eq!(direct_bids.len(), 2);
            assert!(direct_bids.iter().all(|bid| bid["id"] == "shared-cache-id"));
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "debug-auction".to_string(),
                results: Vec::new(),
            },
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
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
        }
    }

    #[test]
    fn c2_reader_seam_inserts_one_complete_hard_cutover_boot() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let projection = r#"{"version":1,"auction":{"version":1,"auctionId":"auction-c2-reader","results":[]},"slots":[],"bids":[]}"#;
        let mut params = make_stream_params(&settings, "");
        params.content_type = "text/html; charset=utf-8".to_string();
        params.ad_bids_state = AdBidsState::with_script(projection);

        let seam = seam_script_for(&params, &settings, &registry);
        let boot = seam
            .find("const __TSJS_SERVER_BOOT_TRANSPORT_V1__=")
            .expect("should emit one immutable sealed boot transport");
        let runtime = seam
            .find("id=\"trustedserver-js\"")
            .expect("should emit the selected parser-time runtime");

        assert!(boot < runtime, "boot input must precede runtime execution");
        assert_eq!(
            seam.matches("const __TSJS_SERVER_BOOT_TRANSPORT_V1__=")
                .count(),
            1,
            "reader seam must carry exactly one complete boot"
        );
        assert_eq!(
            bootstrap_transport(&seam)["boot"]["auctionProjection"]["auction"]["auctionId"],
            "auction-c2-reader"
        );
        assert!(seam.contains(r#"performance.mark("tsjs:bids-script")"#));
        assert!(!seam.contains(AD_ASSEMBLY_SEAM));
        assert!(!seam.contains(".adSlots"));
        assert!(!seam.contains(".bids="));
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
            boot_gpt_diagnostics_active(&html),
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

    mod template_cache_gate_tests {
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::VaryNotCovered(VaryGap(vec![
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
                template_cache_bypass_reason(
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
                Some(TemplateCacheBypassReason::VaryCookie),
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &split,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::OriginNotShareable),
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &split,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::OriginNotShareable)
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
                    template_cache_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &split,
                        &nothing_covered(),
                    ),
                    Some(TemplateCacheBypassReason::OriginNotShareable),
                    "template cache must fail closed on the CDN-specific policy header {name}"
                );
            }
        }

        #[test]
        fn unsupported_vendor_freshness_does_not_authorize_template_cache() {
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
                    template_cache_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &split,
                        &nothing_covered(),
                    ),
                    Some(TemplateCacheBypassReason::OriginNotShareable),
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
                template_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &publisher_headers,
                    &TemplateCachePolicy::for_test(&nothing_covered(), Duration::from_secs(300),),
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
                    template_cache_ttl(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &publisher_headers,
                        &TemplateCachePolicy::for_test(
                            &nothing_covered(),
                            Duration::from_secs(600),
                        ),
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
                template_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &publisher_headers,
                    &TemplateCachePolicy::for_test(&nothing_covered(), Duration::from_secs(600),),
                ),
                Ok(Duration::from_secs(10)),
                "stale windows are validated metadata, not fresh template cache lifetime"
            );
        }

        #[test]
        fn ambiguous_or_unsupported_surrogate_policy_fails_closed() {
            for (policy, expected) in [
                ("max-age", TemplateCacheBypassReason::MalformedCachePolicy),
                (
                    "max-age=30, max-age=60",
                    TemplateCacheBypassReason::MalformedCachePolicy,
                ),
                (
                    "max-age=tomorrow",
                    TemplateCacheBypassReason::MalformedCachePolicy,
                ),
                (
                    "max-age=30, public",
                    TemplateCacheBypassReason::MalformedCachePolicy,
                ),
                (
                    "stale-if-error=60",
                    TemplateCacheBypassReason::NoPositiveFreshness,
                ),
                ("max-age=0", TemplateCacheBypassReason::NoPositiveFreshness),
                (
                    "max-age=30,",
                    TemplateCacheBypassReason::MalformedCachePolicy,
                ),
            ] {
                let mut publisher_headers = shareable();
                publisher_headers.insert(
                    header::HeaderName::from_static("surrogate-control"),
                    HeaderValue::from_str(policy).expect("should build Surrogate-Control"),
                );

                assert_eq!(
                    template_cache_bypass_reason(
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
                    template_cache_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &publisher_headers,
                        &nothing_covered(),
                    ),
                    Some(TemplateCacheBypassReason::OriginNotShareable),
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
                    template_cache_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &publisher_headers,
                        &nothing_covered(),
                    ),
                    Some(TemplateCacheBypassReason::OriginNotShareable),
                    "standard `{directive}` must refuse template cache even with positive edge freshness"
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
                template_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &publisher_headers,
                    &TemplateCachePolicy::for_test(&nothing_covered(), Duration::from_secs(300),),
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
                template_cache_bypass_reason(
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
                template_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &fresh_headers,
                    &TemplateCachePolicy::for_test(&nothing_covered(), Duration::from_secs(60),),
                ),
                Ok(Duration::from_secs(60))
            );

            let aged = headers(&[
                (header::CACHE_CONTROL, "s-maxage=50, max-age=300"),
                (header::AGE, "35"),
            ]);
            assert_eq!(
                template_cache_ttl(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &aged,
                    &TemplateCachePolicy::for_test(&nothing_covered(), Duration::from_secs(60),),
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
                Err(TemplateCacheBypassReason::NoPositiveFreshness),
                "an old Date is apparent age even when an upstream omitted Age"
            );
        }

        #[test]
        fn zero_exhausted_missing_and_malformed_freshness_are_refused() {
            for (map, expected) in [
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=0")]),
                    TemplateCacheBypassReason::NoPositiveFreshness,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=60"), (header::AGE, "60")]),
                    TemplateCacheBypassReason::NoPositiveFreshness,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "public")]),
                    TemplateCacheBypassReason::NoPositiveFreshness,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=tomorrow")]),
                    TemplateCacheBypassReason::MalformedCachePolicy,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=\"60")]),
                    TemplateCacheBypassReason::MalformedCachePolicy,
                ),
                (
                    headers(&[(header::CACHE_CONTROL, "max-age=+60")]),
                    TemplateCacheBypassReason::MalformedCachePolicy,
                ),
            ] {
                assert_eq!(
                    template_cache_bypass_reason(
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
                Err(TemplateCacheBypassReason::NoPositiveFreshness)
            );
        }

        #[test]
        fn request_semantics_bypass_template_cache_except_for_a_max_age_zero_reload() {
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
                assert!(
                    request_bypasses_template_cache(&map),
                    "{name}: {value} must bypass"
                );
            }
            assert!(
                !request_bypasses_template_cache(&headers(&[(header::CACHE_CONTROL, "max-age=0")])),
                "a browser reload may reuse template cache because the assembled response and auction \
                 are still rebuilt for this reader"
            );
            assert!(!request_bypasses_template_cache(&headers(&[(
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::VaryWildcard)
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
                template_cache_bypass_reason(
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &VarySpec::new(["rsc".to_string()]),
                ),
                Some(TemplateCacheBypassReason::VaryNotCovered(VaryGap(vec![
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &varying,
                    &VarySpec::new(["rsc".to_string()]),
                ),
                Some(TemplateCacheBypassReason::VaryCookie),
                "a repeated Vary header must not hide names behind the first value"
            );
        }

        #[test]
        fn a_plain_shareable_html_200_is_cacheable() {
            assert_eq!(
                template_cache_bypass_reason(
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
                template_cache_bypass_reason(
                    AssemblyMode::Inline,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &shareable(),
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::InlineMode),
                "inline has no shared template to write"
            );
        }

        #[test]
        fn an_authorized_request_is_never_cached() {
            assert_eq!(
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    true,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &shareable(),
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::AuthorizedRequest),
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    true,
                    StatusCode::OK,
                    "text/html",
                    &no_cache_control,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::CookieForwarded),
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &with_cookie,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::OriginSetCookie),
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
                    template_cache_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        "text/html",
                        &map,
                        &nothing_covered(),
                    ),
                    Some(TemplateCacheBypassReason::OriginNotShareable),
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::FORBIDDEN,
                    "text/html",
                    &shareable(),
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::NonOkStatus),
                "a blocked document must not become the shared template"
            );
        }

        #[test]
        fn non_html_is_refused() {
            for content_type in ["text/x-component", "application/json", ""] {
                assert_eq!(
                    template_cache_bypass_reason(
                        AssemblyMode::Esi,
                        false,
                        false,
                        StatusCode::OK,
                        content_type,
                        &shareable(),
                        &nothing_covered(),
                    ),
                    Some(TemplateCacheBypassReason::NotHtml),
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &map,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::UnsupportedContentEncoding)
            );

            let mut repeated = headers(&[(header::CACHE_CONTROL, "public, max-age=60")]);
            repeated.append(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
            repeated.append(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            assert_eq!(
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    false,
                    false,
                    StatusCode::OK,
                    "text/html",
                    &repeated,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::MalformedRepresentationHeaders)
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
                template_cache_bypass_reason(
                    AssemblyMode::Esi,
                    true,
                    false,
                    StatusCode::FORBIDDEN,
                    "application/json",
                    &map,
                    &nothing_covered(),
                ),
                Some(TemplateCacheBypassReason::AuthorizedRequest),
                "authorization is the most serious disqualifier and should win"
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
                 [creative_opportunities]\nenabled = true\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml)
                .expect("should parse settings with auction and creative opportunities enabled")
        }

        fn settings_with_disabled_creative_opportunities() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n\
                 [creative_opportunities]\nenabled = false\ngam_network_id = \"12345\"\nassembly_mode = \"esi\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml)
                .expect("should parse settings with creative opportunities disabled")
        }

        fn settings_without_creative_opportunities() -> Settings {
            Settings::from_toml(&crate_test_settings_str())
                .expect("should parse settings without creative opportunities")
        }
        fn settings_with_dispatching_provider() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\nproviders = [\"{UNEXPECTED_304_PROVIDER}\"]\n\n\
                 [creative_opportunities]\nenabled = true\ngam_network_id = \"12345\"\n",
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
        async fn disabled_creative_opportunities_keep_matching_navigation_inactive() {
            let settings = settings_with_disabled_creative_opportunities();
            assert_eq!(
                configured_assembly_mode(&settings),
                AssemblyMode::Inline,
                "disabled template delivery must not activate configured assembly machinery"
            );
            let stub = Arc::new(StubHttpClient::new());
            queue_cacheable_html_response(&stub);
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let slots = [article_slot()];

            let response = run_with_slots(
                &settings,
                &services,
                &slots,
                conditional_navigation_request(),
            )
            .await;
            let response_head = response_head(response);

            assert_eq!(
                stub.recorded_cache_bypass_flags(),
                vec![false],
                "disabled template delivery must not bypass the publisher cache"
            );
            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("max-age=60"),
                "disabled template delivery should use the inactive HTML policy"
            );
            assert_eq!(
                response_head.headers.get(header::ETAG),
                Some(&HeaderValue::from_static(ORIGIN_ETAG)),
                "disabled template delivery must preserve the origin validator"
            );
        }

        #[tokio::test]
        async fn navigation_without_matched_slots_replaces_origin_cache_policy() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();

            for cache_control in ["no-cache", "max-age=0", "must-revalidate", "s-maxage=0"] {
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_cache_control(&stub, cache_control);
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                let response =
                    run_with_slots(&settings, &services, &[], conditional_navigation_request())
                        .await;
                let response_head = response_head(response);

                assert_eq!(
                    response_head
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some("max-age=60"),
                    "inactive server-side ad templates should replace origin {cache_control} policy"
                );
            }
        }

        #[tokio::test]
        async fn navigation_without_matched_slots_preserves_private_origin_cache_policy() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();

            for cache_control in ["private, max-age=0", "No-Store"] {
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_cache_control(&stub, cache_control);
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                let response =
                    run_with_slots(&settings, &services, &[], conditional_navigation_request())
                        .await;
                let response_head = response_head(response);

                assert_eq!(
                    response_head
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some(cache_control),
                    "inactive server-side ad templates should preserve private origin {cache_control} policy"
                );
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
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_cache_control(&stub, "no-cache");
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                let response =
                    run_with_slots_and_consent(&settings, &services, &slots, request, consent)
                        .await;
                let response_head = response_head(response);

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
            let settings = settings_without_creative_opportunities();
            let stub = Arc::new(StubHttpClient::new());
            queue_html_response_with_cache_control(&stub, "no-cache");
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );

            let response =
                run_with_slots(&settings, &services, &[], conditional_navigation_request()).await;
            let response_head = response_head(response);

            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("max-age=60"),
                "absent creative opportunities should use the inactive browser cache policy"
            );
        }

        #[tokio::test]
        async fn inactive_ad_stack_preserves_non_ok_response_cache_policy() {
            let settings = settings_with_disabled_creative_opportunities();

            for status in [206, 404, 500, 503] {
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_status_and_cache_control(&stub, status, "no-cache");
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                let response = run_with_slots(
                    &settings,
                    &services,
                    &[article_slot()],
                    conditional_navigation_request(),
                )
                .await;
                let response_head = response_head(response);

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
            let mut settings = settings_with_disabled_creative_opportunities();
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

            let response = run_with_slots(&settings, &services, &[article_slot()], request).await;
            let response_head = response_head(response);

            assert_eq!(
                response_head
                    .headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, no-store"),
                "active GPT diagnostics should retain cache privacy when the ad stack is inactive"
            );
        }

        #[tokio::test]
        async fn inactive_ad_stack_preserves_non_get_and_non_document_cache_policy() {
            let settings = settings_with_disabled_creative_opportunities();

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
                let stub = Arc::new(StubHttpClient::new());
                queue_html_response_with_cache_control(&stub, "no-cache");
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                let response =
                    run_with_slots(&settings, &services, &[article_slot()], request).await;
                let response_head = response_head(response);

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
        async fn inactive_html_preserves_case_insensitive_private_origin_policy() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();

            for origin_policy in ["PuBlIc, PrIvAtE, max-age=300", "public, NO-STORE"] {
                let stub = Arc::new(StubHttpClient::new());
                stub.push_response_with_headers(
                    200,
                    b"<html><body>origin</body></html>".to_vec(),
                    vec![
                        ("content-type", "text/html; charset=utf-8"),
                        ("cache-control", origin_policy),
                    ],
                );
                let services = build_services_with_http_client(
                    Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
                );

                let response =
                    run_with_slots(&settings, &services, &[], conditional_navigation_request())
                        .await;
                let response_head = response_head(response);

                assert_eq!(
                    response_head
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some(origin_policy),
                    "inactive HTML must preserve origin private/no-store directives"
                );
            }
        }

        #[tokio::test]
        async fn inactive_html_preserves_private_policy_on_a_repeated_header_line() {
            let settings = settings_with_enabled_auction_and_creative_opportunities();
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response_with_headers(
                200,
                b"<html><body>origin</body></html>".to_vec(),
                vec![
                    ("content-type", "text/html; charset=utf-8"),
                    ("cache-control", "public, max-age=300"),
                    ("cache-control", "PrIvAtE"),
                ],
            );
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );

            let response =
                run_with_slots(&settings, &services, &[], conditional_navigation_request()).await;
            let response_head = response_head(response);
            let policies = response_head
                .headers
                .get_all(header::CACHE_CONTROL)
                .iter()
                .map(|value| value.to_str().expect("cache policy should be ASCII"))
                .collect::<Vec<_>>();

            assert_eq!(
                policies,
                ["public, max-age=300", "PrIvAtE"],
                "a private directive on any origin header line must prevent the inactive rewrite"
            );
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
                PublisherResponse::PassThrough { .. }
                | PublisherResponse::Stream { .. }
                | PublisherResponse::AssembleTemplate { .. } => {
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
    async fn aps_enabled_publisher_response_appends_an_independent_frame_policy() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "aps",
                &serde_json::json!({ "enabled": true, "account_id": "test-account" }),
            )
            .expect("should enable APS");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><head></head><body>origin</body></html>".to_vec(),
            vec![
                ("content-type", "text/html; charset=utf-8"),
                (
                    "content-security-policy",
                    "default-src 'self'; frame-ancestors https://operator.example",
                ),
            ],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let request = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/article")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build publisher request");

        let response = run_publisher_proxy(&settings, &services, request).await;
        let headers = match response {
            PublisherResponse::Buffered(response)
            | PublisherResponse::PassThrough { response, .. }
            | PublisherResponse::Stream { response, .. }
            | PublisherResponse::AssembleTemplate { response, .. } => {
                response.into_parts().0.headers
            }
        };
        let policies = headers
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .map(|value| value.to_str().expect("policy should be ASCII"))
            .collect::<Vec<_>>();

        assert_eq!(
            policies,
            [
                "default-src 'self'; frame-ancestors https://operator.example",
                "frame-ancestors 'self'",
            ],
            "APS framing policy should append after and intersect operator policy"
        );
    }

    #[tokio::test]
    async fn aps_disabled_publisher_response_does_not_add_a_frame_policy() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "aps",
                &serde_json::json!({ "enabled": false, "account_id": "test-account" }),
            )
            .expect("should configure disabled APS");
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><head></head><body>origin</body></html>".to_vec(),
            vec![
                ("content-type", "text/html; charset=utf-8"),
                ("content-security-policy", "default-src 'self'"),
            ],
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let request = HttpRequest::builder()
            .method(Method::GET)
            .uri("https://publisher.example/article")
            .header(header::HOST, "publisher.example")
            .body(EdgeBody::empty())
            .expect("should build publisher request");

        let response = run_publisher_proxy(&settings, &services, request).await;
        let headers = match response {
            PublisherResponse::Buffered(response)
            | PublisherResponse::PassThrough { response, .. }
            | PublisherResponse::Stream { response, .. }
            | PublisherResponse::AssembleTemplate { response, .. } => {
                response.into_parts().0.headers
            }
        };
        let policies = headers
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .map(|value| value.to_str().expect("policy should be ASCII"))
            .collect::<Vec<_>>();

        assert_eq!(policies, ["default-src 'self'"]);
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
            | PublisherResponse::Stream { response, .. }
            | PublisherResponse::AssembleTemplate { response, .. } => {
                response.into_parts().0.headers
            }
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
            assert!(!boot_gpt_diagnostics_active(&body));
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
        assert!(boot_gpt_diagnostics_active(&active_body));
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
        assert!(!boot_gpt_diagnostics_active(&disabled_body));
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
        let transport = bootstrap_transport(&body);
        let diagnostics = &transport["boot"]["diagnostics"];

        assert!(
            diagnostics["renderTraceOverlay"] == true,
            "the exact server-owned trace cookie must populate DiagnosticsBootV1: {body}"
        );
        assert_eq!(
            body.matches("const __TSJS_SERVER_BOOT_TRANSPORT_V1__=")
                .count(),
            1,
            "the immutable server boot must carry one active diagnostics object"
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
            assert!(!boot_gpt_diagnostics_active(&body));
            assert!(!body.contains("tsjs-gpt_diagnostics.min.js"));
            assert!(!body.contains("tsjs-gpt_diagnostics-bootstrap.min.js"));
            assert!(!body.contains("history.replaceState"));
        }
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
        let selection = registry
            .tsjs_static_transport_selections(false)
            .pop()
            .expect("should expose one transport selection");
        let src = registry
            .tsjs_takeover_artifact(selection)
            .expect("should precompute selected takeover artifact")
            .src();
        let req = build_request(Method::GET, &format!("https://publisher.example{src}"));

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");
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
    fn tsjs_dynamic_head_matches_get_metadata_without_a_body() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let selection = registry
            .tsjs_static_transport_selections(false)
            .pop()
            .expect("should expose one transport selection");
        let src = registry
            .tsjs_takeover_artifact(selection)
            .expect("should precompute selected takeover artifact")
            .src();
        let get = build_request(Method::GET, &format!("https://publisher.example{src}"));
        let head = build_request(Method::HEAD, &format!("https://publisher.example{src}"));

        let get_response = handle_tsjs_dynamic(&get, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should serve GET metadata");
        let head_response = handle_tsjs_dynamic(&head, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should serve HEAD metadata");

        assert_eq!(head_response.status(), get_response.status());
        assert_eq!(head_response.headers(), get_response.headers());
        assert!(
            matches!(head_response.body(), EdgeBody::Once(bytes) if bytes.is_empty()),
            "HEAD must not return embedded TSJS bytes"
        );
    }

    #[test]
    fn tsjs_dynamic_serves_the_exact_rewritten_creative_bundle() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let creative_hash = trusted_server_js::concatenated_hash(&["render_runtime", "creative"]);
        let src = registry
            .tsjs_static_artifact(&creative_hash)
            .expect("should precompute rewritten creative artifact")
            .src();
        let req = build_request(Method::GET, &format!("https://publisher.example{src}"));

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");

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
        let ids = registry.tsjs_takeover_module_ids(selection);
        let src = crate::tsjs::tsjs_script_src(&ids);
        let first = build_request(Method::GET, &format!("https://publisher.example{src}"));
        let first_response =
            handle_tsjs_dynamic(&first, &registry, EdgeCacheHeader::SMaxageFallback)
                .expect("should serve current release");
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

        let response =
            handle_tsjs_dynamic(&conditional, &registry, EdgeCacheHeader::SMaxageFallback)
                .expect("should handle conditional request");

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(header::ETAG), Some(&etag));
        assert_eq!(
            response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
    }

    #[test]
    fn tsjs_dynamic_serves_only_the_exact_precomputed_first_display_mask_and_hash() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt", &serde_json::json!({}))
            .expect("should enable GPT");
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let mask = *registry
            .tsjs_first_display_masks()
            .first()
            .expect("GPT should permit one first-display mask");
        let selected = trusted_server_js::all_first_display_ids()
            .into_iter()
            .enumerate()
            .filter_map(|(index, id)| (mask & (1 << index) != 0).then_some(id))
            .collect::<Vec<_>>();
        let hash = trusted_server_js::concatenated_first_display_hash(&selected[1..])
            .expect("selected slices should hash");
        let src = format!("/static/tsjs=tsjs-first-display.min.js?m={mask:04x}&v={hash}");

        let request = build_request(Method::GET, &format!("https://publisher.example{src}"));
        let response = handle_tsjs_dynamic(&request, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should serve artifact");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static(
                "application/javascript; charset=utf-8"
            ))
        );
        assert_eq!(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .as_ref(),
            trusted_server_js::concatenate_first_display_slices(&selected[1..])
                .expect("should compose selected slices")
                .as_bytes()
        );

        let head = build_request(Method::HEAD, &format!("https://publisher.example{src}"));
        let response = handle_tsjs_dynamic(&head, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should serve HEAD metadata");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .is_empty()
        );

        for suffix in [
            format!("tsjs-first-display.min.js?v={hash}&m={mask:04x}"),
            format!("tsjs-first-display.min.js?m={mask:04x}&v={hash}&x=1"),
            format!(
                "tsjs-first-display.min.js?m={:04x}&v={hash}",
                mask ^ (1 << 1)
            ),
            format!(
                "tsjs-first-display.min.js?m={mask:04x}&v={}",
                "0".repeat(64)
            ),
            format!("tsjs-first_display.min.js?m={mask:04x}&v={hash}"),
        ] {
            let request = build_request(
                Method::GET,
                &format!("https://publisher.example/static/tsjs={suffix}"),
            );
            let response =
                handle_tsjs_dynamic(&request, &registry, EdgeCacheHeader::SMaxageFallback)
                    .expect("should reject locally");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "case {suffix}");
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL),
                Some(&HeaderValue::from_static("no-store")),
                "case {suffix}"
            );
        }
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
        let ids = registry.tsjs_takeover_module_ids(selection);
        let hash = trusted_server_js::concatenated_hash(&ids);
        let cases = [
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
                        .expect("should hash the creative runtime")
                ),
            ),
        ];

        for (method, suffix) in cases {
            let req = build_request(
                method,
                &format!("https://publisher.example/static/tsjs={suffix}"),
            );
            let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
                .expect("should reject locally");
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
    fn tsjs_dynamic_rejects_takeover_diagnostics_as_a_standalone_alias() {
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

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");

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

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should serve cleanup asset");
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

        let response = handle_tsjs_dynamic(&req, &registry, EdgeCacheHeader::SMaxageFallback)
            .expect("should handle tsjs request");
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
            let state = AdBidsState::default();
            let auction_request = test_auction_request();
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
            let auction_id = boot_auction_id(&html);
            assert!(
                html.contains("hello"),
                "should preserve streamed HTML content. Got: {html}"
            );
            assert!(
                auction_id.starts_with("a1_"),
                "the immutable pre-core boot must contain the collected auction projection with a browser-only auction id. Got: {html}"
            );
            assert!(
                auction_id != auction_request.id,
                "the immutable pre-core boot must not expose the upstream auction id. Got: {html}"
            );
            assert!(
                auction_id != "initial",
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
            let auction_id = boot_auction_id(&html);
            assert!(
                html.contains("hello"),
                "should decode the first gzip member. Got: {html}"
            );
            assert!(
                html.contains("</body></html>"),
                "should decode the second gzip member that a single-member decoder drops. Got: {html}"
            );
            assert!(
                auction_id.starts_with("a1_"),
                "should emit the collected projection with a browser-only auction id before the head bundle. Got: {html}"
            );
            assert!(
                auction_id != "test-auction",
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
            render_trace_overlay: false,
            suppress_datadome_client_side_tag: false,
        }
    }

    #[test]
    fn streaming_finalize_emits_tsjs_head_before_origin_eof_without_legacy_gam_transport() {
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
            html.contains("id=\"trustedserver-js\""),
            "first rewritten head chunk should carry the parser-time TSJS runtime: {html}"
        );
        assert!(
            !html.contains("__tsjs_gam_attribution_enabled")
                && !html.contains("data-ts-gam-attribution"),
            "hard cutover must not retain either legacy GAM activation transport: {html}"
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
        let auction_id = boot_auction_id(&html);
        assert!(
            html.contains("hello"),
            "the origin body should remain lazy after the pre-head collect. Got: {html}"
        );
        assert!(
            auction_id.starts_with("a1_"),
            "the first head chunk must contain the exact collected projection with a browser-only auction id. Got: {html}"
        );
        assert!(
            auction_id != "test-auction",
            "the first head chunk must not expose the upstream auction id. Got: {html}"
        );
        assert!(
            auction_id != "initial",
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
        let auction_id = boot_auction_id(&html);
        assert!(
            auction_id.starts_with("a1_"),
            "should collect the exact projection with a browser-only auction id before the compressed head. Got head: {}",
            &html[..html.len().min(500)]
        );
        assert!(
            auction_id != "test-auction",
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
            boot_auction_id(&html) == "initial",
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
                "{}\n[auction]\nenabled = true\n\n[creative_opportunities]\nenabled = true\ngam_network_id = \"12345\"\n",
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
                "{}\n[auction]\nenabled = false\n\n[creative_opportunities]\nenabled = true\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml).expect("should parse settings with creative_opportunities")
        }

        fn settings_with_co_disabled() -> Settings {
            let toml = format!(
                "{}\n[auction]\nenabled = true\n\n[creative_opportunities]\nenabled = false\ngam_network_id = \"12345\"\n",
                crate_test_settings_str()
            );
            Settings::from_toml(&toml)
                .expect("should parse settings with creative opportunity delivery disabled")
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
            // An enabled configuration with no matching definitions has no work.
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
        async fn disabled_creative_opportunities_return_an_exact_empty_projection() {
            let settings = settings_with_co_disabled();
            let orchestrator = AuctionOrchestrator::new(settings.auction.clone());
            let slots = article_slot();
            let req = make_page_bids_request("/2024/01/my-article/");

            let body = run_page_bids_consent_allowed(&settings, &orchestrator, &slots, req).await;

            assert_eq!(body["version"], 1);
            assert_eq!(body["auction"]["version"], 1);
            assert_eq!(body["auction"]["results"], serde_json::json!([]));
            assert_eq!(body["slots"], serde_json::json!([]));
            assert_eq!(body["bids"], serde_json::json!([]));
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
                 [creative_opportunities]\nenabled = true\ngam_network_id = \"12345\"\n",
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
