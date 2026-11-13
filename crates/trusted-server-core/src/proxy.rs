use crate::http_util::{
    RequestInfo, body_as_reader, compute_encrypted_sha256_token, ct_str_eq, read_request_body,
};
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::error::EdgeError;
use edgezero_core::ingress::RequestIngress;
use edgezero_core::outbound::{
    OutboundCachePolicy, OutboundFollowFailure, OutboundImageOptimizerOptions,
    OutboundRedirectAction, OutboundRedirectContext, OutboundRedirectLimit, OutboundRequest,
    OutboundResponse, ResponseDecoding,
};
use error_stack::{Report, ResultExt};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::asset_image_optimizer::{image_optimizer_options, profile_for_asset_request};
use crate::cache_policy::{
    CachePolicy, EdgeCacheHeader, NO_STORE_PRIVATE_CACHE_CONTROL,
    apply_no_store_private_to_headers, cache_control_headers_are_private_or_no_store,
    remove_edge_cache_headers,
};
use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
    HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
};
use crate::creative::{CreativeCssProcessor, CreativeHtmlProcessor, first_party_url};
use crate::edge_cookie::get_ec_id;
use crate::error::TrustedServerError;
use crate::outbound::{
    BUFFERED_TIMEOUT, DEFAULT_BUFFERED_RESPONSE_BYTES, OutboundRequestSource,
    REDIRECT_CHAIN_DEADLINE, STREAMED_TIMEOUT, buffered, outbound_error, outbound_request,
    streamed,
};
use crate::platform::RuntimeServices;
use crate::s3_sigv4::{self, S3Credentials};
use crate::settings::{AssetOriginAuth, OriginQueryPolicy, ProxyAssetRoute, Settings};
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};

/// Chunk size used for streaming content through the rewrite pipeline.
const STREAMING_CHUNK_SIZE: usize = 8192;

/// Fallback `Content-Type` for image-like responses missing an origin type.
const IMAGE_FALLBACK_CONTENT_TYPE: &str = "application/octet-stream";

const SIGN_MAX_BODY_BYTES: usize = 65536;
const REBUILD_MAX_BODY_BYTES: usize = 65536;

/// Headers copied from the original client request to the upstream proxy request
/// when `copy_request_headers` is enabled.
///
/// `Accept-Encoding` is also overridden in the same code path, but with a fixed
/// value ([`SUPPORTED_ENCODINGS`]) rather than forwarding the client's preference.
/// Both forwarded headers and the Accept-Encoding override are applied together in
/// the `copy_request_headers` branch of the proxy request builder.
const PROXY_FORWARD_HEADERS: [header::HeaderName; 5] = [
    HEADER_USER_AGENT,
    HEADER_ACCEPT,
    HEADER_ACCEPT_LANGUAGE,
    HEADER_REFERER,
    HEADER_X_FORWARDED_FOR,
];

/// Curated request headers preserved for asset proxying.
///
/// Unlike the HTML publisher fallback, asset requests need cache validation and
/// byte-range semantics to keep 304/206 responses working for browsers.
/// Client-supplied forwarding headers are stripped at the edge and are not
/// reconstructed here, so asset origins see Trusted Server as the client.
const ASSET_PROXY_FORWARD_HEADERS: [header::HeaderName; 11] = [
    HEADER_USER_AGENT,
    HEADER_ACCEPT,
    HEADER_ACCEPT_ENCODING,
    HEADER_ACCEPT_LANGUAGE,
    HEADER_REFERER,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::IF_MATCH,
    header::IF_UNMODIFIED_SINCE,
    header::RANGE,
    header::IF_RANGE,
];

/// Response headers an upstream must not be able to set on a body that is
/// passed through and served from the publisher's own origin.
///
/// All three take effect against the serving origin, so leaving them in place
/// would let an upstream write first-party cookies, pin HSTS, or clear the
/// publisher's site data.
pub(crate) const FIRST_PARTY_PASSTHROUGH_STRIP_HEADERS: [&str; 3] =
    ["set-cookie", "strict-transport-security", "clear-site-data"];

/// Cache-control value used when asset proxy responses must not be stored.
pub const ASSET_NO_STORE_PRIVATE_CACHE_CONTROL: &str = NO_STORE_PRIVATE_CACHE_CONTROL;

/// Cache policy metadata emitted by the asset proxy handler.
///
/// The Fastly router finalizes standard response headers after the asset handler
/// returns. This typed policy lets the router reapply protected cache directives
/// without depending on an exact header string set by the handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetProxyCachePolicy {
    /// Leave origin/cache-control headers under normal response finalization.
    OriginControlled,
    /// Reapply `Cache-Control: no-store, private` after standard finalization.
    NoStorePrivate,
    /// Reapply an operator-selected normalized cache policy after finalization.
    ///
    /// The adapter must call [`Self::apply_after_route_finalization`] after
    /// standard response and privacy finalization, passing its runtime
    /// [`EdgeCacheHeader`]. Asset rehosting is Fastly-only today; a future
    /// adapter must preserve this finalization step to emit its edge directive.
    Normalized(CachePolicy),
}

impl AssetProxyCachePolicy {
    /// Apply protected cache headers after route-level response finalization.
    pub fn apply_after_route_finalization(
        self,
        response: &mut Response<EdgeBody>,
        edge_header: EdgeCacheHeader,
    ) {
        match self {
            Self::OriginControlled => {}
            Self::NoStorePrivate => apply_no_store_cache_control(response),
            Self::Normalized(policy) => {
                if cache_control_headers_are_private_or_no_store(response.headers()) {
                    remove_edge_cache_headers(response.headers_mut());
                } else {
                    policy.apply_to_headers(response.headers_mut(), edge_header);
                }
            }
        }
    }
}

/// Asset proxy response plus metadata needed by the outer router.
pub struct AssetProxyResponse {
    response: Response<EdgeBody>,
    stream_body: Option<EdgeBody>,
    cache_policy: AssetProxyCachePolicy,
}

impl AssetProxyResponse {
    fn new(
        response: Response<EdgeBody>,
        stream_body: Option<EdgeBody>,
        cache_policy: AssetProxyCachePolicy,
    ) -> Self {
        Self {
            response,
            stream_body,
            cache_policy,
        }
    }

    fn origin_controlled(response: Response<EdgeBody>) -> Self {
        Self::new(response, None, AssetProxyCachePolicy::OriginControlled)
    }

    fn origin_controlled_stream(response: Response<EdgeBody>, stream_body: EdgeBody) -> Self {
        Self::new(
            response,
            Some(stream_body),
            AssetProxyCachePolicy::OriginControlled,
        )
    }

    fn no_store_private(response: Response<EdgeBody>) -> Self {
        Self::new(response, None, AssetProxyCachePolicy::NoStorePrivate)
    }

    fn response(&self) -> &Response<EdgeBody> {
        &self.response
    }

    fn response_mut(&mut self) -> &mut Response<EdgeBody> {
        &mut self.response
    }

    fn apply_no_store_private_policy(&mut self) {
        self.cache_policy = AssetProxyCachePolicy::NoStorePrivate;
        apply_no_store_cache_control(&mut self.response);
    }

    fn apply_normalized_cache_policy(&mut self, policy: CachePolicy) {
        self.cache_policy = AssetProxyCachePolicy::Normalized(policy);
        policy.apply_to_headers(self.response.headers_mut(), EdgeCacheHeader::None);
    }

    /// Return cache policy metadata for router finalization.
    #[must_use]
    pub fn cache_policy(&self) -> AssetProxyCachePolicy {
        self.cache_policy
    }

    /// Consume this wrapper and return the response headers plus optional stream body.
    #[must_use]
    pub fn into_response_and_body(self) -> (Response<EdgeBody>, Option<EdgeBody>) {
        (self.response, self.stream_body)
    }
}

#[derive(Deserialize)]
struct ProxySignReq {
    url: String,
}

#[derive(Serialize)]
struct ProxySignResp {
    href: String,
    base: String,
}

/// Configuration for outbound proxying from integration routes.
#[derive(Debug, Clone)]
pub struct ProxyRequestConfig<'a> {
    /// Target URL to proxy to (must be http/https).
    pub target_url: &'a str,
    /// Whether redirects should be followed automatically.
    pub follow_redirects: bool,
    /// Whether to append the caller's EC ID as a query param.
    pub forward_ec_id: bool,
    /// Optional body to send to the origin.
    pub body: Option<Vec<u8>>,
    /// Additional headers to forward to the origin.
    pub headers: Vec<(header::HeaderName, HeaderValue)>,
    /// Whether to forward the helper's curated request-header set.
    pub copy_request_headers: bool,
    /// When true, stream the origin response without HTML/CSS rewrites.
    pub stream_passthrough: bool,
    /// When true, stream the upstream response body instead of buffering it.
    pub stream_response: bool,
    /// Cap on a buffered upstream response, in bytes.
    ///
    /// A streamed response, see [`Self::stream_response`], has no cap.
    pub max_response_bytes: u64,
    /// Domains allowed for the initial request and any redirects.
    ///
    /// **Open mode** (`&[]`): every host is permitted. Most integration proxies pass
    /// `&[]` because their target URLs originate from operator-controlled configuration
    /// (e.g. `trusted-server.toml` integration settings) and are therefore trusted at
    /// operator setup time rather than at request time.
    ///
    /// **Restricted mode** (non-empty slice): only hosts matching a listed pattern are
    /// permitted. First-party creative proxying and managed static-asset proxies pass
    /// `&settings.proxy.allowed_domains` because they follow redirect chains that must
    /// stay constrained to operator-approved hosts.
    pub allowed_domains: &'a [String],
    /// Require the initial target and every followed redirect hop to use HTTPS.
    pub require_https: bool,
}

impl<'a> ProxyRequestConfig<'a> {
    /// Build a proxy configuration that follows redirects and forwards the EC ID.
    #[must_use]
    pub fn new(target_url: &'a str) -> Self {
        Self {
            target_url,
            follow_redirects: true,
            forward_ec_id: true,
            body: None,
            headers: Vec::new(),
            copy_request_headers: true,
            stream_passthrough: false,
            stream_response: false,
            max_response_bytes: DEFAULT_BUFFERED_RESPONSE_BYTES,
            allowed_domains: &[],
            require_https: false,
        }
    }

    /// Attach a request body to the proxied request.
    #[must_use]
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }

    /// Append an additional header to the proxied request.
    #[must_use]
    pub fn with_header(mut self, name: header::HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }

    /// Disable forwarding of the helper's curated request-header set.
    #[must_use]
    pub fn without_forward_headers(mut self) -> Self {
        self.copy_request_headers = false;
        self
    }

    /// Enable streaming passthrough (no HTML/CSS rewrites).
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.stream_passthrough = true;
        self
    }

    /// Disable EC ID query-param forwarding to the target URL.
    #[must_use]
    pub fn without_ec_id(mut self) -> Self {
        self.forward_ec_id = false;
        self
    }

    /// Enforce a domain allowlist on the target URL and followed redirects.
    #[must_use]
    pub fn with_allowed_domains(mut self, allowed_domains: &'a [String]) -> Self {
        self.allowed_domains = allowed_domains;
        self
    }

    /// Require HTTPS for the target URL and followed redirects.
    #[must_use]
    pub fn with_https_only(mut self) -> Self {
        self.require_https = true;
        self
    }

    /// Stream the upstream response body instead of buffering it.
    #[must_use]
    pub fn with_stream_response(mut self) -> Self {
        self.stream_response = true;
        self
    }

    /// Cap a buffered upstream response at `max_response_bytes`.
    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: u64) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }
}

/// Encodings we support decompressing in `finalize_proxied_response`.
/// We override the client's Accept-Encoding to only advertise these.
const SUPPORTED_ENCODINGS: &str = "gzip, deflate, br";

/// Rebuild a response with a new body, preserving headers except Content-Length.
/// If `preserve_encoding` is true, the Content-Encoding header is kept (for compressed responses).
/// If false, Content-Encoding is stripped (for decompressed responses).
fn rebuild_response_with_body(
    beresp: Response<EdgeBody>,
    content_type: &'static str,
    body: Vec<u8>,
    preserve_encoding: bool,
) -> Response<EdgeBody> {
    let (mut parts, _) = beresp.into_parts();

    // Always skip Content-Length (size changed) and Content-Type (we set it)
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_TYPE);

    // Skip Content-Encoding only if we're not preserving it
    if !preserve_encoding {
        parts.headers.remove(header::CONTENT_ENCODING);
    }

    parts
        .headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));

    Response::from_parts(parts, EdgeBody::from(body))
}

/// Process a response body through a streaming pipeline with the given processor.
///
/// Handles decompression, content processing, and re-compression while preserving
/// the response status and headers.
fn process_response_with_pipeline<P: StreamProcessor>(
    mut beresp: Response<EdgeBody>,
    processor: P,
    compression: Compression,
    content_type: &'static str,
    error_context: &'static str,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let config = PipelineConfig {
        input_compression: compression,
        output_compression: compression,
        chunk_size: STREAMING_CHUNK_SIZE,
    };

    let body = std::mem::replace(beresp.body_mut(), EdgeBody::empty());
    let mut output = Vec::new();
    let mut pipeline = StreamingPipeline::new(config, processor);
    pipeline
        .process(body_as_reader(body, "proxy")?, &mut output)
        .change_context(TrustedServerError::Proxy {
            message: error_context.to_string(),
        })?;

    Ok(rebuild_response_with_body(
        beresp,
        content_type,
        output,
        compression != Compression::None,
    ))
}

/// Extracted content-type and content-encoding from an origin response.
struct OriginResponseMeta {
    ct_raw: String,
    content_encoding: String,
}

/// Extract content-type and content-encoding and log origin response metadata.
///
/// When `log_encoding` is `true`, the `ce=` field is included in the log line
/// (used by the buffered finalizer which performs content-encoding processing).
/// The streaming finalizer omits it since it never decodes the body.
fn origin_response_metadata(
    req: &Request<EdgeBody>,
    beresp: &Response<EdgeBody>,
    target_url: &str,
    log_encoding: bool,
) -> OriginResponseMeta {
    let status_code = beresp.status().as_u16();
    let ct_raw = beresp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let content_encoding = beresp
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let cl_raw = beresp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-");
    let accept_raw = req
        .headers()
        .get(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-");

    let ct_for_log: &str = if ct_raw.is_empty() { "-" } else { &ct_raw };

    if log_encoding {
        let ce_for_log: &str = if content_encoding.is_empty() {
            "-"
        } else {
            &content_encoding
        };
        log::info!(
            "origin response status={} ct={} ce={} cl={} accept={} url={}",
            status_code,
            ct_for_log,
            ce_for_log,
            cl_raw,
            accept_raw,
            target_url
        );
    } else {
        log::info!(
            "origin response status={} ct={} cl={} accept={} url={}",
            status_code,
            ct_for_log,
            cl_raw,
            accept_raw,
            target_url
        );
    }

    OriginResponseMeta {
        ct_raw,
        content_encoding,
    }
}

/// Apply image content-type header and log pixel heuristics.
///
/// Sets a generic `image/*` content-type when the response has none, then logs
/// a warning if size or path heuristics suggest a pixel image. Both call
/// sites pass the response through unchanged afterwards, so this returns
/// nothing.
fn apply_image_passthrough_metadata(
    req: &Request<EdgeBody>,
    target_url: &str,
    ct: &str,
    beresp: &mut Response<EdgeBody>,
    log_prefix: &str,
) {
    let req_accept_images = req
        .headers()
        .get(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("image/"))
        .unwrap_or(false);

    if !ct.starts_with("image/") && !req_accept_images {
        return;
    }

    if beresp.headers().get(header::CONTENT_TYPE).is_none() {
        beresp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(IMAGE_FALLBACK_CONTENT_TYPE),
        );
    }

    let mut is_pixel = false;
    if let Some(cl) = beresp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        && cl <= 256
    {
        is_pixel = true;
    }
    if !is_pixel {
        let lower = target_url.to_ascii_lowercase();
        if lower.contains("/pixel")
            || lower.ends_with("/p.gif")
            || lower.contains("1x1")
            || lower.contains("/track")
        {
            is_pixel = true;
        }
    }
    if is_pixel {
        log::info!(
            "{}likely pixel image fetched: {} ct={}",
            log_prefix,
            target_url,
            ct
        );
    }
}

fn finalize_proxied_response(
    settings: &Settings,
    req: &Request<EdgeBody>,
    target_url: &str,
    mut beresp: Response<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let meta = origin_response_metadata(req, &beresp, target_url, true);
    let ct = meta.ct_raw.to_ascii_lowercase();
    let compression = Compression::from_content_encoding(&meta.content_encoding);

    // A HEAD response has no body to rewrite, and decoding its empty body would
    // fail for a compressed type, so skip the rewrite. Its representation
    // headers must still describe the body a GET would return: the GET path
    // rewrites HTML and CSS and drops the upstream length, so drop it here too.
    if req.method() == Method::HEAD {
        if ct.contains("text/html") || ct.contains("text/css") {
            beresp.headers_mut().remove(header::CONTENT_LENGTH);
        }
        apply_image_passthrough_metadata(req, target_url, &ct, &mut beresp, "");
        return Ok(beresp);
    }

    if ct.contains("text/html") {
        let processor = CreativeHtmlProcessor::new(settings);
        return process_response_with_pipeline(
            beresp,
            processor,
            compression,
            "text/html; charset=utf-8",
            "Failed to process HTML response",
        );
    }

    if ct.contains("text/css") {
        let processor = CreativeCssProcessor::new(settings);
        return process_response_with_pipeline(
            beresp,
            processor,
            compression,
            "text/css; charset=utf-8",
            "Failed to process CSS response",
        );
    }

    apply_image_passthrough_metadata(req, target_url, &ct, &mut beresp, "");
    Ok(beresp)
}

fn finalize_proxied_response_streaming(
    req: &Request<EdgeBody>,
    target_url: &str,
    mut beresp: Response<EdgeBody>,
) -> Response<EdgeBody> {
    let meta = origin_response_metadata(req, &beresp, target_url, false);
    let ct = meta.ct_raw.to_ascii_lowercase();
    apply_image_passthrough_metadata(req, target_url, &ct, &mut beresp, "stream: ");
    beresp
}

/// CORS policy headers stripped from every proxied response.
///
/// Emitting none of our own is not enough: the buffered path preserves upstream
/// headers wholesale and the streaming path passes the upstream response
/// through, so an upstream that answers `Access-Control-Allow-Origin: *` (or
/// `null`) would hand the browser a readable cross-origin response through our
/// endpoint. Removing the policy makes the browser fall back to the same-origin
/// rule, which is the intent.
const STRIPPED_CORS_RESPONSE_HEADERS: [header::HeaderName; 3] = [
    header::ACCESS_CONTROL_ALLOW_ORIGIN,
    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
    header::ACCESS_CONTROL_EXPOSE_HEADERS,
];

/// Remove any upstream CORS grant so proxied bodies stay unreadable cross-origin.
fn strip_cors_policy(response: &mut Response<EdgeBody>) {
    for name in STRIPPED_CORS_RESPONSE_HEADERS {
        response.headers_mut().remove(name);
    }
}

/// Finalize a proxied response, choosing between streaming passthrough and full
/// content processing based on the `stream_passthrough` flag.
///
/// Guarantees no CORS grant reaches the browser — neither one of ours nor one
/// forwarded from upstream. `/first-party/proxy` is a generic signed fetcher: it
/// forwards the EC ID and curated client-derived headers, follows redirects, and
/// runs in open mode when `proxy.allowed_domains` is empty. A signature proves
/// only that this service minted the URL — and with `sanitize_creatives`
/// disabled the rewriter mints them from bidder-controlled markup — so letting
/// an opaque creative frame *read* those bodies would turn the endpoint into a
/// readable bidder-controlled proxy. Non-CORS subresources (`<img>`,
/// `<script src>`, stylesheets) are unaffected; CORS-mode ones are covered by
/// the constrained asset capability tracked in
/// <https://github.com/IABTechLab/trusted-server/issues/982>.
fn finalize_response(
    settings: &Settings,
    req: &Request<EdgeBody>,
    url: &str,
    beresp: Response<EdgeBody>,
    stream_passthrough: bool,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let mut response = if stream_passthrough {
        finalize_proxied_response_streaming(req, url, beresp)
    } else {
        finalize_proxied_response(settings, req, url, beresp)?
    };
    strip_cors_policy(&mut response);
    Ok(response)
}

/// Redirects a followed proxy request takes before it returns the next one.
const MAX_REDIRECTS: usize = 4;

/// Proxy a request to a clear target URL while reusing creative rewrite logic.
///
/// This forwards a curated header set, follows redirects when enabled, and can append
/// the caller's EC ID as a `ts-ec` query parameter to the target URL.
/// Optional bodies/headers can be supplied via [`ProxyRequestConfig`].
///
/// The target must pass [`check_proxy_target`] before anything is sent. A followed
/// request goes through `EdgeZero`'s redirect follower, which shows every redirect
/// to [`redirect_policy`]. Each hop gets the request's own timeout, 15 s buffered
/// or 60 s streamed, and the whole chain shares a 60 s deadline. A request that does not follow redirects is sent
/// once, and a `3xx` answer is returned as it is.
///
/// Followed `307`/`308` redirects keep the original method and body. Followed
/// `301`/`302`/`303` redirects are sent as a bodyless `GET` (a `HEAD` stays
/// `HEAD`) without body-describing headers such as `Content-Type`. A `301`/`302`
/// answering any method other than `GET`, `HEAD` or `POST` (for example `PUT`)
/// is not followed: the redirect response is returned to the caller, so an
/// update is neither replayed elsewhere nor silently turned into a `GET`.
///
/// # Errors
///
/// - [`TrustedServerError::Proxy`] if the target URL is invalid, uses an unsupported
///   scheme, lacks a host, or the upstream fetch fails
/// - [`TrustedServerError::Forbidden`] if HTTPS is required and the target or a
///   redirect is not HTTPS
/// - [`TrustedServerError::AllowlistViolation`] if the target or a redirect leaves
///   `allowed_domains`
pub async fn proxy_request(
    settings: &Settings,
    req: Request<EdgeBody>,
    config: ProxyRequestConfig<'_>,
    services: &RuntimeServices,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let ProxyRequestConfig {
        target_url,
        follow_redirects,
        forward_ec_id,
        body,
        headers,
        copy_request_headers,
        stream_passthrough,
        stream_response,
        max_response_bytes,
        allowed_domains,
        require_https,
    } = config;
    let failed = || TrustedServerError::Proxy {
        message: "Failed to proxy".to_string(),
    };

    let mut target = url::Url::parse(target_url).map_err(|_| {
        Report::new(TrustedServerError::Proxy {
            message: "invalid url".to_string(),
        })
    })?;
    if forward_ec_id {
        append_ec_id(&req, &mut target);
    }
    check_proxy_target(&target, allowed_domains, require_https)?;

    let request = proxy_outbound_request(&req, &target, body, &headers, copy_request_headers)
        .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;
    let request = if stream_response {
        streamed(request, STREAMED_TIMEOUT)
    } else {
        buffered(
            request,
            BUFFERED_TIMEOUT,
            max_response_bytes,
            ResponseDecoding::Original,
        )
    };

    let (response, final_uri) = if follow_redirects {
        let limit = OutboundRedirectLimit::new(MAX_REDIRECTS)
            .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;
        let chain_deadline = services.clock().deadline_after(REDIRECT_CHAIN_DEADLINE);
        let followed = services
            .outbound_client()
            .follow(
                request.deadline(chain_deadline),
                limit,
                redirect_policy(allowed_domains.to_vec(), require_https),
            )
            .await
            .map_err(|failure| match failure {
                OutboundFollowFailure::Policy { error, .. } => Report::new(error),
                OutboundFollowFailure::Hop { error, .. } => {
                    outbound_error(error, failed(), OutboundRequestSource::Internal)
                }
            })?;
        if let Some(reason) = followed.stop_reason() {
            log::warn!("returning an unfollowed redirect response: {reason:?}");
        }
        let (response, final_uri, _redirects_followed, _stop_reason) = followed.into_parts();
        (response, final_uri)
    } else {
        let final_uri = request.uri().clone();
        let response = services
            .outbound_client()
            .send(request)
            .await
            .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;
        (response, final_uri)
    };
    let response = response
        .into_response()
        .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;

    finalize_response(
        settings,
        &req,
        &final_uri.to_string(),
        response,
        stream_passthrough,
    )
}

/// Checks the first target of a proxied request before it is sent.
///
/// The target needs an `http` or `https` scheme, HTTPS when `require_https` is
/// set, a host, and a host that `allowed_domains` permits. `EdgeZero` never shows
/// the first request to [`redirect_policy`], which checks every later hop.
///
/// # Errors
///
/// Returns [`TrustedServerError::Proxy`] for another scheme or a missing host,
/// [`TrustedServerError::Forbidden`] for a non-HTTPS target that must be HTTPS,
/// and [`TrustedServerError::AllowlistViolation`] for a host outside
/// `allowed_domains`.
fn check_proxy_target(
    target: &url::Url,
    allowed_domains: &[String],
    require_https: bool,
) -> Result<(), Report<TrustedServerError>> {
    let scheme = target.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "unsupported scheme".to_string(),
        }));
    }
    if require_https && scheme != "https" {
        log::warn!("request to `{target}` blocked: HTTPS is required");
        return Err(Report::new(TrustedServerError::Forbidden {
            message: "non-HTTPS proxy target blocked".to_string(),
        }));
    }

    let host = target.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "missing host".to_string(),
        }));
    }
    if !is_host_permitted(allowed_domains, host) {
        log::warn!("request to `{host}` blocked: host not in proxy allowed_domains");
        return Err(Report::new(TrustedServerError::AllowlistViolation {
            host: host.to_string(),
        }));
    }

    Ok(())
}

/// Builds the first request of a proxied fetch from the client's `req`.
///
/// With `copy_request_headers`, the request carries [`PROXY_FORWARD_HEADERS`] from
/// the client and [`SUPPORTED_ENCODINGS`] as `Accept-Encoding`. Each of the
/// caller's `headers` then replaces any value of the same name.
///
/// # Errors
///
/// Returns [`EdgeError::BadRequest`] when `target` is not a URI `EdgeZero` sends.
fn proxy_outbound_request(
    req: &Request<EdgeBody>,
    target: &url::Url,
    body: Option<Vec<u8>>,
    headers: &[(header::HeaderName, HeaderValue)],
    copy_request_headers: bool,
) -> Result<OutboundRequest, EdgeError> {
    let mut request = outbound_request(req.method().clone(), target.as_str())?;
    let outbound_headers = request.headers_mut();
    if copy_request_headers {
        for header_name in PROXY_FORWARD_HEADERS {
            if let Some(value) = req.headers().get(&header_name) {
                outbound_headers.insert(header_name, value.clone());
            }
        }
        outbound_headers.insert(
            HEADER_ACCEPT_ENCODING,
            HeaderValue::from_static(SUPPORTED_ENCODINGS),
        );
    }
    for (name, value) in headers {
        outbound_headers.insert(name.clone(), value.clone());
    }

    Ok(match body {
        Some(body) => request.body(body),
        None => request,
    })
}

/// Builds the policy a followed proxy request applies to every redirect.
///
/// It owns copies of `allowed_domains` and `require_https`, because `EdgeZero`
/// keeps the policy for the whole chain. For each redirect it reads the resolved
/// target and returns, in this order:
///
/// 1. [`TrustedServerError::Forbidden`] when `require_https` is set and the target
///    is not `https`
/// 2. [`OutboundRedirectAction::Stop`] for any other scheme that is not `http` or
///    `https`, which returns the redirect response
/// 3. [`TrustedServerError::Proxy`] for a target without a host
/// 4. [`TrustedServerError::AllowlistViolation`] when `allowed_domains` does not
///    permit the host
/// 5. [`OutboundRedirectAction::FollowPrepared`] otherwise
fn redirect_policy(
    allowed_domains: Vec<String>,
    require_https: bool,
) -> impl FnMut(&mut OutboundRedirectContext<'_>) -> Result<OutboundRedirectAction, TrustedServerError>
+ 'static {
    move |context| {
        let target =
            url::Url::parse(context.resolved_target()).map_err(|_| TrustedServerError::Proxy {
                message: "invalid redirect".to_string(),
            })?;
        let scheme = target.scheme();
        if require_https && scheme != "https" {
            log::warn!("redirect to `{target}` blocked: HTTPS is required");
            return Err(TrustedServerError::Forbidden {
                message: "non-HTTPS redirect blocked".to_string(),
            });
        }
        if scheme != "http" && scheme != "https" {
            return Ok(OutboundRedirectAction::Stop);
        }

        let Some(host) = target.host_str().filter(|host| !host.is_empty()) else {
            return Err(TrustedServerError::Proxy {
                message: "missing host in redirect location".to_string(),
            });
        };
        if !is_host_permitted(&allowed_domains, host) {
            log::warn!("redirect to `{host}` blocked: host not in proxy allowed_domains");
            return Err(TrustedServerError::AllowlistViolation {
                host: host.to_string(),
            });
        }

        Ok(OutboundRedirectAction::FollowPrepared)
    }
}

fn default_port_for_scheme(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn build_asset_proxy_target_url(
    route: &ProxyAssetRoute,
    path: &str,
    query: &str,
) -> Result<url::Url, Report<TrustedServerError>> {
    let mut target_url =
        url::Url::parse(&route.origin_url).change_context(TrustedServerError::Proxy {
            message: format!("Invalid asset origin_url: {}", route.origin_url),
        })?;

    let scheme = target_url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Report::new(TrustedServerError::Proxy {
            message: format!("Unsupported asset origin_url scheme: {scheme}"),
        }));
    }

    if target_url.host_str().is_none() {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "Missing host in asset origin_url".to_string(),
        }));
    }

    let target_path = route.target_path_for(path)?;
    target_url.set_path(&target_path);
    if query.is_empty() {
        target_url.set_query(None);
    } else {
        target_url.set_query(Some(query));
    }

    Ok(target_url)
}

fn asset_path_skips_image_optimizer(path: &str) -> bool {
    let lower_path = path.to_ascii_lowercase();
    lower_path.ends_with(".svg") || lower_path.ends_with(".svgz")
}

fn asset_origin_host_header(
    target_url: &url::Url,
) -> Result<HeaderValue, Report<TrustedServerError>> {
    let scheme = target_url.scheme();
    let host = target_url.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "Missing host in asset target URL".to_string(),
        })
    })?;
    let resolved_port = target_url.port_or_known_default().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: format!("Unsupported asset target URL scheme: {scheme}"),
        })
    })?;
    let host_header = if Some(resolved_port) == default_port_for_scheme(scheme) {
        host.to_string()
    } else {
        format!("{host}:{resolved_port}")
    };

    HeaderValue::from_str(&host_header).change_context(TrustedServerError::InvalidHeaderValue {
        message: format!("invalid asset Host header value: {host_header}"),
    })
}

fn apply_asset_origin_auth(
    method: &Method,
    target_url: &url::Url,
    headers: &mut http::HeaderMap,
    auth: &AssetOriginAuth,
) -> Result<(), Report<TrustedServerError>> {
    match auth {
        AssetOriginAuth::S3SigV4(config) => {
            let credentials = S3Credentials {
                access_key_id: config.access_key_id.expose().clone(),
                secret_access_key: config.secret_access_key.clone(),
                session_token: config.session_token.clone(),
            };
            s3_sigv4::sign_headers(
                method,
                target_url,
                headers,
                &config.region,
                &credentials,
                // s3_sigv4 converts this via chrono's `DateTime::<Utc>::from`, which
                // only accepts `std::time::SystemTime`.
                crate::ec::std_system_time_now(),
            )
        }
    }
}

/// Sends one request to an asset origin.
///
/// A `GET` streams its response under the 60 s streamed timeout. Any other method,
/// the S3 `HEAD` preflight included, buffers its response under the 15 s buffered
/// timeout. Both keep the origin's content coding. `cache_policy` and
/// `image_optimizer` options go on the request as given; `EdgeZero` rejects a
/// request that carries both [`OutboundCachePolicy::Bypass`] and options.
///
/// `outbound_headers` may carry the signer's `Host` input. `EdgeZero` drops that
/// header and sends the URI authority, from which [`asset_origin_host_header`]
/// built it.
///
/// # Errors
///
/// Returns [`TrustedServerError::Proxy`] when the request cannot be built or the
/// origin request fails.
async fn send_asset_origin_request(
    services: &RuntimeServices,
    method: &Method,
    target_url: &url::Url,
    outbound_headers: &http::HeaderMap,
    cache_policy: OutboundCachePolicy,
    image_optimizer: Option<OutboundImageOptimizerOptions>,
) -> Result<AssetProxyResponse, Report<TrustedServerError>> {
    let failed = || TrustedServerError::Proxy {
        message: "Failed to proxy asset request".to_string(),
    };
    let mut request = outbound_request(method.clone(), target_url.as_str())
        .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?
        .cache_policy(cache_policy);
    *request.headers_mut() = outbound_headers.clone();
    if let Some(options) = image_optimizer {
        request = request.image_optimizer(options);
    }
    let request = if method == Method::GET {
        streamed(request, STREAMED_TIMEOUT)
    } else {
        buffered(
            request,
            BUFFERED_TIMEOUT,
            DEFAULT_BUFFERED_RESPONSE_BYTES,
            ResponseDecoding::Original,
        )
    };

    let response = services
        .outbound_client()
        .send(request)
        .await
        .and_then(OutboundResponse::into_response)
        .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;
    let (parts, body) = response.into_parts();
    Ok(if body.is_stream() {
        AssetProxyResponse::origin_controlled_stream(
            Response::from_parts(parts, EdgeBody::empty()),
            body,
        )
    } else {
        AssetProxyResponse::origin_controlled(Response::from_parts(parts, body))
    })
}

fn strip_asset_proxy_response_headers(response: &mut Response<EdgeBody>) {
    // Asset origins must not be able to mutate publisher-domain browser state
    // or security policy through this proxy path.
    for header_name in FIRST_PARTY_PASSTHROUGH_STRIP_HEADERS {
        response.headers_mut().remove(header_name);
    }
}

fn apply_no_store_cache_control(response: &mut Response<EdgeBody>) {
    apply_no_store_private_to_headers(response.headers_mut());
}

fn should_preflight_s3(
    route: &ProxyAssetRoute,
    image_optimizer_enabled: bool,
    request_method: &Method,
) -> bool {
    image_optimizer_enabled
        && matches!(route.auth.as_ref(), Some(AssetOriginAuth::S3SigV4(_)))
        && (request_method == Method::GET || request_method == Method::HEAD)
}

async fn preflight_s3_origin_for_image_optimizer(
    services: &RuntimeServices,
    route: &ProxyAssetRoute,
    target_url: &url::Url,
    request_method: &Method,
    unsigned_headers: &http::HeaderMap,
) -> Result<Option<AssetProxyResponse>, Report<TrustedServerError>> {
    let Some(auth @ AssetOriginAuth::S3SigV4(_)) = route.auth.as_ref() else {
        return Ok(None);
    };

    // Fastly Image Optimizer can mask or transform S3 origin errors. A signed
    // HEAD preflight lets missing or unauthorized objects return raw S3 errors
    // without invoking IO on the failure path.
    //
    // The preflight HEAD and the raw error-body GET bypass the readthrough
    // cache. On a miss it can forward a signed HEAD as a GET, invalidating
    // SigV4, and the error response must come from S3 rather than from a cached
    // object.
    let mut head_headers = unsigned_headers.clone();
    apply_asset_origin_auth(&Method::HEAD, target_url, &mut head_headers, auth)?;
    let head_response = send_asset_origin_request(
        services,
        &Method::HEAD,
        target_url,
        &head_headers,
        OutboundCachePolicy::Bypass,
        None,
    )
    .await?;
    let head_status = head_response.response().status();
    if head_status.is_success() || head_status == StatusCode::NOT_MODIFIED {
        return Ok(None);
    }

    if request_method == Method::HEAD {
        // The preflight buffers its bodiless HEAD, so no stream comes back.
        let (mut response, _stream_body) = head_response.into_response_and_body();
        strip_asset_proxy_response_headers(&mut response);
        apply_no_store_cache_control(&mut response);
        return Ok(Some(AssetProxyResponse::no_store_private(response)));
    }

    let mut get_headers = unsigned_headers.clone();
    apply_asset_origin_auth(&Method::GET, target_url, &mut get_headers, auth)?;
    let mut response = send_asset_origin_request(
        services,
        &Method::GET,
        target_url,
        &get_headers,
        OutboundCachePolicy::Bypass,
        None,
    )
    .await?;
    strip_asset_proxy_response_headers(response.response_mut());
    response.apply_no_store_private_policy();

    Ok(Some(response))
}

/// Proxy a configured first-party asset path to its matched asset origin.
///
/// This is a lean raw pass-through path: it preserves status/body/headers,
/// does not follow redirects, and bypasses publisher-page processing. The flow
/// is path rewrite, profile-table Image Optimizer options, optional origin query
/// stripping, optional origin authentication, then the origin request.
///
/// The origin query policy is applied before S3 signing so the signature covers
/// the exact URL sent to the asset origin. Image Optimizer options stay separate
/// from the origin URL and go only on the final request, never on the S3 `HEAD`
/// preflight. A profile that cannot be expressed as options, which settings
/// validation rules out, serves the asset unoptimized with a warning.
///
/// # Errors
///
/// Returns an error if the configured origin URL is invalid, the request names
/// an unknown image profile in `reject` mode, signing fails, or the upstream
/// request cannot be sent.
pub async fn handle_asset_proxy_request(
    settings: &Settings,
    services: &RuntimeServices,
    req: Request<EdgeBody>,
    route: &ProxyAssetRoute,
) -> Result<AssetProxyResponse, Report<TrustedServerError>> {
    let incoming_path = req.uri().path();
    let incoming_query = req.uri().query().unwrap_or("");
    let mut target_url = build_asset_proxy_target_url(route, incoming_path, incoming_query)?;
    let skip_image_optimizer = asset_path_skips_image_optimizer(incoming_path)
        || asset_path_skips_image_optimizer(target_url.path());
    let image_optimizer = if skip_image_optimizer {
        log::debug!(
            "Skipping Image Optimizer for unsupported SVG asset path: incoming={}, target={}",
            incoming_path,
            target_url.path()
        );
        None
    } else {
        profile_for_asset_request(settings, route, incoming_query)?.and_then(|profile| {
            image_optimizer_options(&profile)
                .inspect_err(|error| {
                    log::warn!(
                        "Serving asset {incoming_path} unoptimized: its Image Optimizer profile cannot be applied: {error}"
                    );
                })
                .ok()
        })
    };

    if route.origin_query_policy() == OriginQueryPolicy::Strip {
        target_url.set_query(None);
    }

    let mut outbound_headers = http::HeaderMap::new();
    for header_name in ASSET_PROXY_FORWARD_HEADERS {
        if let Some(value) = req.headers().get(&header_name) {
            outbound_headers.insert(header_name, value.clone());
        }
    }
    outbound_headers.insert(header::HOST, asset_origin_host_header(&target_url)?);

    if should_preflight_s3(route, image_optimizer.is_some(), req.method())
        && let Some(response) = preflight_s3_origin_for_image_optimizer(
            services,
            route,
            &target_url,
            req.method(),
            &outbound_headers,
        )
        .await?
    {
        return Ok(response);
    }

    if let Some(auth) = &route.auth {
        apply_asset_origin_auth(req.method(), &target_url, &mut outbound_headers, auth)?;
    }

    // A signed raw HEAD bypasses the readthrough cache so the cache cannot turn
    // it into a GET after signing. A request with Image Optimizer options keeps
    // the platform default, because bypass disables Image Optimizer on Fastly.
    let cache_policy = if image_optimizer.is_none()
        && req.method() == Method::HEAD
        && matches!(route.auth.as_ref(), Some(AssetOriginAuth::S3SigV4(_)))
    {
        OutboundCachePolicy::Bypass
    } else {
        OutboundCachePolicy::PlatformDefault
    };
    let mut response = send_asset_origin_request(
        services,
        req.method(),
        &target_url,
        &outbound_headers,
        cache_policy,
        image_optimizer,
    )
    .await?;
    strip_asset_proxy_response_headers(response.response_mut());

    let status = response.response().status();
    if (status.is_success() || status == StatusCode::NOT_MODIFIED)
        && let Some(policy) = settings.asset_cache_policy_for_path(incoming_path)?
    {
        response.apply_normalized_cache_policy(policy);
    }

    Ok(response)
}

/// Upserts the `ts-ec` query parameter on a URL, replacing any existing value.
fn upsert_ec_query_param(url: &mut url::Url, ec_id: &str) {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k.as_ref() != "ts-ec")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    pairs.push(("ts-ec".to_string(), ec_id.to_string()));

    url.set_query(None);
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in &pairs {
        serializer.append_pair(k, v);
    }
    url.set_query(Some(&serializer.finish()));
}

fn append_ec_id(req: &Request<EdgeBody>, target_url_parsed: &mut url::Url) {
    let ec_id_param = match get_ec_id(req) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("failed to extract EC ID for forwarding: {:?}", e);
            None
        }
    };

    if let Some(ec_id) = ec_id_param {
        upsert_ec_query_param(target_url_parsed, &ec_id);
        log::debug!(
            "forwarding EC ID to origin url {}",
            target_url_parsed.as_str()
        );
    } else {
        log::debug!("no EC ID to forward to origin");
    }
}

/// Returns `true` when `host` is permitted by the proxy host policy.
///
/// When `allowed_domains` is empty every host is permitted (open mode).
/// When non-empty the host must match at least one pattern via [`is_host_allowed`].
pub(crate) fn is_host_permitted<S: AsRef<str>>(allowed_domains: &[S], host: &str) -> bool {
    allowed_domains.is_empty()
        || allowed_domains
            .iter()
            .any(|p| is_host_allowed(host, p.as_ref()))
}

/// Returns `true` if `host` is permitted by `pattern`.
///
/// - `"example.com"` matches exactly `example.com`.
/// - `"*.example.com"` matches `example.com` and any subdomain at any depth.
///
/// Comparison is case-insensitive. The wildcard check requires a dot boundary,
/// so `"*.example.com"` does **not** match `"evil-example.com"`.
pub(crate) fn is_host_allowed(host: &str, pattern: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();

    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix
            || host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('.'))
    } else {
        host == pattern
    }
}

/// Unified proxy endpoint for resources referenced by ad creatives.
///
/// Accepts:
/// - `u`: Base64 URL-safe (no padding) encoded URL of the third-party resource.
///
/// Behavior:
/// - Proxies the decoded URL, following redirects only to hosts in
///   `proxy.allowed_domains`.
/// - If the response `Content-Type` contains `text/html`, rewrites the HTML creative
///   (img/srcset/iframe to first-party) before returning `text/html; charset=utf-8`.
/// - If the response is an image or the request `Accept` indicates images, ensures an
///   `application/octet-stream` content type if origin omitted it, and logs likely 1×1
///   pixels using simple size/URL heuristics. No special response (still proxied).
///
/// # Errors
///
/// Returns an error if the signed target cannot be reconstructed or validation fails.
pub async fn handle_first_party_proxy(
    settings: &Settings,
    services: &RuntimeServices,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // Parse, reconstruct, and validate the signed target URL
    let SignedTarget { target_url, .. } =
        reconstruct_and_validate_signed_target(settings, &req.uri().to_string())?;

    proxy_request(
        settings,
        req,
        ProxyRequestConfig {
            target_url: &target_url,
            follow_redirects: true,
            forward_ec_id: true,
            body: None,
            headers: Vec::new(),
            copy_request_headers: true,
            stream_passthrough: false,
            stream_response: false,
            max_response_bytes: DEFAULT_BUFFERED_RESPONSE_BYTES,
            allowed_domains: &settings.proxy.allowed_domains,
            require_https: false,
        },
        services,
    )
    .await
}

/// First-party click redirect endpoint.
///
/// Accepts the same parameters as the proxy scheme, but instead of proxying the
/// content, it validates the URL and issues a 302 redirect to the reconstructed
/// target URL. This avoids parsing/downloading the content and lets the browser
/// navigate directly to the destination under first-party control.
///
/// # Errors
///
/// Returns an error if the signed target cannot be reconstructed or validation fails.
pub async fn handle_first_party_click(
    settings: &Settings,
    _services: &RuntimeServices,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let SignedTarget {
        target_url: full_for_token,
        tsurl,
        had_params,
    } = reconstruct_and_validate_signed_target(settings, &req.uri().to_string())?;

    let ec_id = match get_ec_id(&req) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("failed to extract EC ID for forwarding: {:?}", e);
            None
        }
    };

    let mut redirect_target = full_for_token.clone();
    if let Some(ref ec_id_value) = ec_id {
        match url::Url::parse(&redirect_target) {
            Ok(mut url) => {
                upsert_ec_query_param(&mut url, ec_id_value);
                redirect_target = url.to_string();
                log::debug!("forwarding EC ID to target url {}", redirect_target);
            }
            Err(e) => {
                log::warn!("failed to parse target url for EC ID forwarding: {:?}", e);
            }
        }
    }

    // Log click metadata for observability
    let ua = req
        .headers()
        .get(HEADER_USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let referer = req
        .headers()
        .get(HEADER_REFERER)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    log::info!(
        "redirect tsurl={} params_present={} target={} referer={} ua={} ec_id={}",
        tsurl,
        had_params,
        redirect_target,
        referer,
        ua,
        ec_id.as_deref().unwrap_or("")
    );

    // 302 redirect to target URL
    let location = HeaderValue::from_str(&redirect_target).map_err(|_| {
        Report::new(TrustedServerError::InvalidHeaderValue {
            message: "invalid redirect target".to_string(),
        })
    })?;
    let mut response = Response::new(EdgeBody::empty());
    *response.status_mut() = StatusCode::FOUND;
    response.headers_mut().insert(header::LOCATION, location);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    Ok(response)
}

/// Sign an arbitrary asset URL so creatives can request first-party proxying at runtime.
/// Supports POST JSON and GET query (`?url=`) payloads. Embeds a short-lived `tsexp` (30s)
/// in the signature so the signed URL cannot be replayed indefinitely. Returns JSON
/// `{ href, base }` where `href` is the signed `/first-party/proxy?...` path and `base`
/// is the normalized clear URL.
///
/// # Errors
///
/// Returns an error if a POST body cannot be read within 64 KiB (see
/// [`read_request_body`]), JSON parsing fails, the URL cannot be parsed, the URL uses an
/// unsupported scheme, the URL lacks a host, the host is declined by
/// `rewrite.exclude_domains` or `rewrite.include_domains`, the host violates
/// `proxy.allowed_domains`, or request metadata needed to determine the scheme
/// contains a malformed host.
pub async fn handle_first_party_proxy_sign(
    settings: &Settings,
    _services: &RuntimeServices,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let method = req.method().clone();
    let req_url = req.uri().to_string();
    // Managed ingress owns origin evidence, including an explicitly unavailable
    // origin. A raw absolute target must not fill that gap. RequestInfo keeps
    // authenticated publisher forwarding ahead of transport/TLS facts.
    // Only legacy callers without ingress metadata use an explicit URI scheme.
    let request_scheme = if req.extensions().get::<RequestIngress>().is_some() {
        RequestInfo::from_request(&req, _services.client_info())?.scheme
    } else {
        match crate::forwarder::public_origin(req.extensions())
            .map(|origin| origin.scheme.clone())
            .or_else(|| req.uri().scheme_str().map(str::to_ascii_lowercase))
        {
            Some(scheme) => scheme,
            None => RequestInfo::from_request(&req, _services.client_info())?.scheme,
        }
    };

    let payload = if method == Method::POST {
        let (parts, body) = req.into_parts();
        let body_bytes = read_request_body(&parts.headers, body, SIGN_MAX_BODY_BYTES).await?;
        let body =
            std::str::from_utf8(&body_bytes).change_context(TrustedServerError::InvalidUtf8 {
                message: "first-party sign request body should be valid UTF-8".to_string(),
            })?;
        serde_json::from_str::<ProxySignReq>(body).change_context(TrustedServerError::Proxy {
            message: "invalid JSON".to_string(),
        })?
    } else {
        let parsed = parse_request_target(&req_url)?;
        let url = parsed
            .query_pairs()
            .find(|(k, _)| k == "url")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| {
                Report::new(TrustedServerError::Proxy {
                    message: "missing url".to_string(),
                })
            })?;
        ProxySignReq { url }
    };

    let trimmed = payload.url.trim();
    let protocol_relative;
    // A protocol-relative target inherits the signing request's scheme before
    // normalization, so `//` and absolute input reach the same policy check.
    let candidate = if trimmed.starts_with("//") {
        protocol_relative = format!("{request_scheme}:{trimmed}");
        protocol_relative.as_str()
    } else {
        trimmed
    };
    let target = crate::creative::normalize_creative_url(candidate).ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "unsupported url".to_string(),
        })
    })?;
    let host = target.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "missing host".to_string(),
        })
    })?;
    if !settings.rewrite.should_proxy_asset(host) {
        log::debug!(
            "sign request for `{host}` declined: host excluded or not in rewrite.include_domains"
        );
        return Err(Report::new(TrustedServerError::Proxy {
            message: "unsupported url".to_string(),
        }));
    }
    if !is_host_permitted(&settings.proxy.allowed_domains, host) {
        log::warn!(
            "sign request for `{}` blocked: host not in proxy.allowed_domains",
            host
        );
        return Err(Report::new(TrustedServerError::AllowlistViolation {
            host: host.to_string(),
        }));
    }

    let now = SystemTime::now();
    let expires = now.checked_add(Duration::from_secs(30)).unwrap_or(now);
    let tsexp = expires
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
        .to_string();
    let extras = vec![(String::from("tsexp"), tsexp)];

    let mut base = target.clone();
    base.set_query(None);
    base.set_fragment(None);
    let public_origin = settings.publisher.effective_public_origin();
    let proxied = crate::creative::build_proxy_url_with_extras(
        settings,
        target.as_str(),
        &public_origin,
        &extras,
    );

    let resp = ProxySignResp {
        href: proxied,
        base: base.to_string(),
    };

    let mut response = Response::new(EdgeBody::from(
        serde_json::to_string(&resp).change_context(TrustedServerError::Proxy {
            message: "failed to serialize".to_string(),
        })?,
    ));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    Ok(response)
}

#[derive(Deserialize)]
struct ProxyRebuildReq {
    tsclick: String,
    add: Option<std::collections::HashMap<String, String>>,
    del: Option<Vec<String>>,
}

/// Media type of a browser form submission, the rebuild endpoint's navigation form.
const FORM_URLENCODED_MIME: &str = "application/x-www-form-urlencoded";

/// Build a rebuild request from `key=value` pairs, shared by the GET query form
/// and the form-encoded POST body form. `add`/`del` carry JSON payloads;
/// unparseable values are ignored rather than failing the request, matching the
/// endpoint's existing leniency about unknown parameters.
fn rebuild_request_from_pairs<'a>(
    pairs: impl Iterator<Item = (std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>,
) -> Result<ProxyRebuildReq, Report<TrustedServerError>> {
    let mut tsclick: Option<String> = None;
    let mut add: Option<std::collections::HashMap<String, String>> = None;
    let mut del: Option<Vec<String>> = None;
    for (k, v) in pairs {
        match k.as_ref() {
            "tsclick" => tsclick = Some(v.into_owned()),
            "add" => {
                if let Ok(m) = serde_json::from_str::<std::collections::HashMap<String, String>>(&v)
                {
                    add = Some(m);
                }
            }
            "del" => {
                if let Ok(arr) = serde_json::from_str::<Vec<String>>(&v) {
                    del = Some(arr);
                }
            }
            _ => {}
        }
    }

    Ok(ProxyRebuildReq {
        tsclick: tsclick.ok_or_else(|| {
            Report::new(TrustedServerError::Proxy {
                message: "missing tsclick".to_string(),
            })
        })?,
        add,
        del,
    })
}

#[derive(Serialize)]
struct ProxyRebuildResp {
    href: String,
    base: String,
    added: std::collections::BTreeMap<String, String>,
    removed: Vec<String>,
}

/// Proxy rebuild endpoint.
/// POST or GET /first-party/proxy-rebuild
/// Body: { tsclick: "/first-party/click?tsurl=...&a=1", add: {"b":"2"}, del: ["c"] }
/// - Only allows adding new parameters or removing existing ones.
/// - Base tsurl cannot change.
///
/// # Errors
///
/// Returns an error if JSON parsing fails, the URL is invalid, or the request body cannot be read.
pub async fn handle_first_party_proxy_rebuild(
    settings: &Settings,
    _services: &RuntimeServices,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let method = req.method().clone();
    let req_query = req.uri().query().unwrap_or_default().to_owned();
    // A form POST is a navigation, not a fetch: the click guard uses it when the
    // GET recovery URL would exceed the platform's request-URL limit, since the
    // click is too long to nest in a query string but a body has no such bound.
    // It answers with the same redirect a GET does — a JSON body would render as
    // text in the frame the browser just navigated.
    let is_form_post = method == Method::POST
        && req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case(FORM_URLENCODED_MIME))
            });
    let redirect_response = method == Method::GET || is_form_post;

    let payload = if method == Method::POST {
        let (parts, body) = req.into_parts();
        let body_bytes = read_request_body(&parts.headers, body, REBUILD_MAX_BODY_BYTES).await?;
        let body =
            std::str::from_utf8(&body_bytes).change_context(TrustedServerError::InvalidUtf8 {
                message: "first-party rebuild request body should be valid UTF-8".to_string(),
            })?;
        if is_form_post {
            rebuild_request_from_pairs(url::form_urlencoded::parse(body.as_bytes()))?
        } else {
            serde_json::from_str::<ProxyRebuildReq>(body).change_context(
                TrustedServerError::Proxy {
                    message: "invalid JSON".to_string(),
                },
            )?
        }
    } else {
        // Support GET: /first-party/proxy-rebuild?tsclick=...&add=...&del=...
        // Parse the query component directly rather than the full URI: browsers
        // (and the Axum/Spin adapters) deliver origin-form URIs (`/path?query`),
        // which `url::Url::parse` rejects as relative.
        rebuild_request_from_pairs(url::form_urlencoded::parse(req_query.as_bytes()))?
    };

    // Input parsing is independent of the configured output origin. The input
    // authority is discarded: signatures cover only tsurl and its params, and
    // the rebuilt href always uses the configured public origin.
    let c_url =
        parse_request_target(&payload.tsclick).change_context(TrustedServerError::Proxy {
            message: "invalid tsclick".to_string(),
        })?;
    if !matches!(c_url.scheme(), "http" | "https") || c_url.path() != "/first-party/click" {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "invalid tsclick path".to_string(),
        }));
    }
    // Validate the tstoken on the original click URL before applying any changes.
    // Without this, an attacker could submit an unsigned tsclick and mint valid
    // click redirects to arbitrary URLs.
    reconstruct_and_validate_signed_target(settings, c_url.as_str())?;

    // Extract tsurl and original params (exclude tstoken if present)
    let mut tsurl: Option<String> = None;
    let mut orig: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (k, v) in c_url.query_pairs() {
        let key = k.as_ref();
        if key == "tsurl" {
            tsurl = Some(v.into_owned());
        } else if key != "tstoken" {
            orig.insert(key.to_string(), v.into_owned());
        }
    }
    let tsurl = tsurl.ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "missing tsurl".to_string(),
        })
    })?;

    // Do not apply `proxy.allowed_domains` to the click target here. That setting
    // governs server-side proxy *fetch* redirect-chain SSRF, not advertiser click
    // 302s — and `handle_first_party_click` itself redirects any valid signed
    // target without consulting it. Applying it only during rebuild would reject
    // signed targets that normal click redirects still allow, a cross-adapter
    // regression. The original signed click URL (including `tsurl`) is already
    // validated above via `reconstruct_and_validate_signed_target`, and `tsurl`
    // is a reserved signing parameter callers cannot alter, so the redirect host
    // is fixed by the validated original.

    // Keep a snapshot before modifications for diagnostics
    let orig_before = orig.clone();

    // Signing-control parameters that callers must not add or remove during a
    // rebuild. `tsexp` is the short-lived replay bound that
    // handle_first_party_proxy_sign attaches; `tstoken`/`tsurl` are the signature
    // and target. Allowing del/add here would let a public rebuild request strip
    // the expiration from a still-valid click URL and re-sign a non-expiring one.
    const RESERVED_SIGNING_PARAMS: &[&str] = &["tsexp", "tstoken", "tsurl"];

    // Apply removals
    if let Some(del) = &payload.del {
        for k in del {
            if RESERVED_SIGNING_PARAMS.contains(&k.as_str()) {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!("cannot delete reserved signing parameter: {k}"),
                }));
            }
            orig.remove(k);
        }
    }
    // Apply additions (must be new keys only)
    if let Some(add) = &payload.add {
        for (k, v) in add {
            if RESERVED_SIGNING_PARAMS.contains(&k.as_str()) {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!("cannot add reserved signing parameter: {k}"),
                }));
            }
            if orig.contains_key(k) {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!("cannot modify existing parameter: {}", k),
                }));
            }
            orig.insert(k.clone(), v.clone());
        }
    }

    // Compute token over tsurl + updated params
    let full_for_token = if orig.is_empty() {
        tsurl.clone()
    } else {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &orig {
            s.append_pair(k, v);
        }
        format!("{}?{}", tsurl, s.finish())
    };
    let token = compute_encrypted_sha256_token(settings, &full_for_token);

    // Build final href
    let mut qs = url::form_urlencoded::Serializer::new(String::new());
    qs.append_pair("tsurl", &tsurl);
    for (k, v) in &orig {
        qs.append_pair(k, v);
    }
    qs.append_pair("tstoken", &token);
    let public_origin = settings.publisher.effective_public_origin();
    let href = first_party_url(
        &public_origin,
        &format!("/first-party/click?{}", qs.finish()),
    );

    // Compute diagnostics: added and removed
    let mut added: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (k, v) in &orig {
        if !orig_before.contains_key(k) {
            added.insert(k.clone(), v.clone());
        }
    }
    let mut removed: Vec<String> = Vec::new();
    for k in orig_before.keys() {
        if !orig.contains_key(k) {
            removed.push(k.clone());
        }
    }

    if redirect_response {
        // Redirect for navigation usage (GET, or a form-encoded POST)
        let location = HeaderValue::from_str(&href).map_err(|_| {
            Report::new(TrustedServerError::InvalidHeaderValue {
                message: "invalid rebuild redirect target".to_string(),
            })
        })?;
        let mut response = Response::new(EdgeBody::empty());
        *response.status_mut() = StatusCode::FOUND;
        response.headers_mut().insert(header::LOCATION, location);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, private"),
        );
        Ok(response)
    } else {
        let json = serde_json::to_string(&ProxyRebuildResp {
            href,
            base: tsurl.clone(),
            added,
            removed,
        })
        .change_context(TrustedServerError::Proxy {
            message: "failed to serialize rebuild response".to_string(),
        })?;
        let mut response = Response::new(EdgeBody::from(json));
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, private"),
        );
        Ok(response)
    }
}

// Shared: reconstruct and validate a signed target URL using tsurl + params + tstoken
#[derive(Debug)]
struct SignedTarget {
    target_url: String,
    tsurl: String,
    had_params: bool,
}

/// Placeholder authority used to parse origin-form request targets.
///
/// Only the path and query of the parsed value are ever read, so the authority
/// is irrelevant to behaviour; `.invalid` is reserved by RFC 2606 and can never
/// resolve.
const REQUEST_TARGET_BASE: &str = "https://request.invalid";

/// Parse a request target that may be either an absolute URL or origin-form.
///
/// Adapters differ in what `Request::uri()` carries: Fastly and the normalized
/// Spin path expose an absolute URL, while browsers send origin-form targets
/// (`/path?query`) that the Axum adapter passes through verbatim. Joining a
/// fixed placeholder origin lets one parser serve both, so first-party
/// endpoints behave identically across adapters.
///
/// # Errors
///
/// Returns [`TrustedServerError::Proxy`] when the target parses as neither form.
fn parse_request_target(req_url: &str) -> Result<url::Url, Report<TrustedServerError>> {
    match url::Url::parse(req_url) {
        Ok(url) => Ok(url),
        Err(url::ParseError::RelativeUrlWithoutBase) => url::Url::parse(REQUEST_TARGET_BASE)
            .and_then(|base| base.join(req_url))
            .change_context(TrustedServerError::Proxy {
                message: "Invalid URL".to_string(),
            }),
        Err(error) => Err(Report::new(TrustedServerError::Proxy {
            message: "Invalid URL".to_string(),
        })
        .attach(error.to_string())),
    }
}

/// Validate a `/first-party/proxy|click` request and reconstruct the clear target URL.
///
/// The first-party URL encodes the clear target in `tsurl=...` along with any
/// original query params and a deterministic `tstoken` signature. This helper:
///
/// 1. Parses the incoming request URL and extracts `tsurl`, `tstoken`, and the
///    remaining query parameters in their original order.
/// 2. Rebuilds the clear-text URL (`target_url`) using the preserved parameter order
///    so signature validation matches what the creative signed.
/// 3. Verifies `tstoken` using the publisher secret. A mismatch yields
///    `TrustedServerError::Proxy`.
/// 4. Validates optional `tsexp` expirations to prevent replay of old signatures.
/// 5. Returns the reconstructed target URL, the base `tsurl` (without extra params),
///    and a flag indicating if the original clear URL had query params.
fn reconstruct_and_validate_signed_target(
    settings: &Settings,
    req_url: &str,
) -> Result<SignedTarget, Report<TrustedServerError>> {
    let parsed = parse_request_target(req_url)?;

    // Extract tsurl and tstoken while preserving original param order for others
    let mut tsurl: Option<String> = None;
    let mut sig: Option<String> = None;
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    let mut had_params = false;
    let mut tsexp: Option<String> = None;
    for (k, v) in parsed.query_pairs() {
        let key = k.as_ref();
        let value = v.into_owned();
        if key == "tsurl" {
            tsurl = Some(value);
            continue;
        }
        if key == "tstoken" {
            sig = Some(value);
            continue;
        }
        if key == "tsexp" {
            tsexp = Some(value.clone());
            ser.append_pair(key, &value);
            continue;
        }
        ser.append_pair(key, &value);
        had_params = true;
    }

    let tsurl = tsurl.ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "missing tsurl parameter".to_string(),
        })
    })?;
    let sig = sig.ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "missing tstoken parameter".to_string(),
        })
    })?;

    let finished = ser.finish();
    let full_for_token = if finished.is_empty() {
        tsurl.clone()
    } else {
        format!("{}?{}", tsurl, finished)
    };

    let expected = compute_encrypted_sha256_token(settings, &full_for_token);
    // Constant-time comparison to prevent timing side-channel attacks on the token.
    // Length is not secret (always 43 bytes for base64url-encoded SHA-256),
    // but we check explicitly to document the invariant.
    if !ct_str_eq(&expected, &sig) {
        return Err(Report::new(TrustedServerError::Forbidden {
            message: "invalid tstoken".to_string(),
        }));
    }

    if let Some(exp_str) = tsexp {
        let exp = exp_str.parse::<u64>().map_err(|_| {
            Report::new(TrustedServerError::Proxy {
                message: "invalid tsexp".to_string(),
            })
        })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs();
        if exp < now {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "expired tsexp".to_string(),
            }));
        }
    }

    Ok(SignedTarget {
        target_url: full_for_token,
        tsurl,
        had_params,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use super::{
        AssetProxyCachePolicy, AssetProxyResponse, IMAGE_FALLBACK_CONTENT_TYPE, ProxyRequestConfig,
        REBUILD_MAX_BODY_BYTES, SIGN_MAX_BODY_BYTES, SUPPORTED_ENCODINGS, asset_origin_host_header,
        asset_path_skips_image_optimizer, build_asset_proxy_target_url, handle_asset_proxy_request,
        handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
        handle_first_party_proxy_sign, is_host_allowed, is_host_permitted, proxy_request,
        rebuild_response_with_body, reconstruct_and_validate_signed_target,
    };
    use crate::cache_policy::{CachePolicy, EdgeCacheHeader};
    use crate::constants::{HEADER_ACCEPT, HEADER_X_FORWARDED_FOR};
    use crate::creative;
    use crate::error::{IntoHttpResponse, TrustedServerError};
    use crate::outbound::{
        BUFFERED_TIMEOUT, DEFAULT_BUFFERED_RESPONSE_BYTES, REDIRECT_CHAIN_DEADLINE,
        STREAMED_TIMEOUT,
    };
    use crate::platform::test_support::{build_services_with_outbound_client, noop_services};
    use crate::platform::{
        PlatformError, PlatformSecretStore, RuntimeServices, StoreId, StoreName,
    };
    use crate::redacted::Redacted;
    use crate::settings::{
        AssetImageOptimizerConfig, AssetOriginAuth, ImageOptimizerAspectRatioConfig,
        ImageOptimizerCropOffsetsConfig, ImageOptimizerProfileSet, ImageOptimizerSettings,
        OriginQueryPolicy, ProxyAssetRoute, S3SigV4AuthConfig, Settings, UnknownProfilePolicy,
    };
    use crate::test_support::body_read::body_read_failure_cases;
    use crate::test_support::logging::WarningCapture;
    use crate::test_support::scripted_client::{
        ExpectedTransport, RecordedRequest, ScriptedHttpClient, ScriptedReply, TestClock,
    };
    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};
    use bytes::Bytes;
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::http::response_builder as edge_response_builder;
    use edgezero_core::ingress::{
        CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, RequestIngress,
        TargetUnavailable,
    };
    use edgezero_core::outbound::{
        OutboundCachePolicy, OutboundCropFocus, OutboundImageFormat, OutboundImageOptimizerOptions,
        OutboundImageOptimizerRegion, OutboundResizeFilter, ResponseDecoding, ResponseMode,
    };
    use edgezero_core::time::Deadline;
    use error_stack::Report;
    use http::{HeaderValue, Method, Request as HttpRequest, Response, StatusCode, header};

    #[test]
    fn test_rebuild_response_with_body_preserves_multiple_headers() {
        let mut response = Response::new(EdgeBody::empty());
        response
            .headers_mut()
            .append(header::SET_COOKIE, HeaderValue::from_static("session=123"));
        response
            .headers_mut()
            .append(header::SET_COOKIE, HeaderValue::from_static("tracker=456"));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));

        let rebuilt =
            rebuild_response_with_body(response, "application/json", b"{}".to_vec(), false);

        let cookies: Vec<_> = rebuilt
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .collect();
        assert_eq!(cookies, vec!["session=123", "tracker=456"]);
        assert_eq!(
            rebuilt
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "application/json"
        );
    }

    fn build_http_request(method: Method, uri: impl AsRef<str>) -> HttpRequest<EdgeBody> {
        HttpRequest::builder()
            .method(method)
            .uri(uri.as_ref())
            .body(EdgeBody::empty())
            .expect("should build http request")
    }

    fn build_http_post_json_request(
        uri: impl AsRef<str>,
        body: &serde_json::Value,
    ) -> HttpRequest<EdgeBody> {
        HttpRequest::builder()
            .method(Method::POST)
            .uri(uri.as_ref())
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(body.to_string()))
            .expect("should build http post request")
    }

    fn build_proxy_sign_request(method: &Method, uri: &str, target: &str) -> HttpRequest<EdgeBody> {
        if method == Method::GET {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            serializer.append_pair("url", target);
            return build_http_request(method.clone(), format!("{uri}?{}", serializer.finish()));
        }

        build_http_post_json_request(uri, &serde_json::json!({ "url": target }))
    }

    /// Builds a JSON `POST` whose `body` arrives as a stream of two chunks.
    fn build_http_post_streaming_request(
        uri: impl AsRef<str>,
        body: &'static [u8],
    ) -> HttpRequest<EdgeBody> {
        let (head, tail) = body.split_at(body.len() / 2);
        let stream = futures::stream::iter([Bytes::from_static(head), Bytes::from_static(tail)]);
        HttpRequest::builder()
            .method(Method::POST)
            .uri(uri.as_ref())
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::stream(stream))
            .expect("should build streaming http post request")
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

    fn build_http_response(status: StatusCode, body: EdgeBody) -> Response<EdgeBody> {
        let mut response = Response::new(body);
        *response.status_mut() = status;
        response
    }

    fn response_header(response: &Response<EdgeBody>, name: header::HeaderName) -> Option<&str> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    }

    #[derive(Clone)]
    struct CountingSecretStore {
        data: Arc<HashMap<String, Vec<u8>>>,
        reads: Arc<Mutex<Vec<String>>>,
    }

    impl CountingSecretStore {
        fn new(data: HashMap<String, Vec<u8>>) -> Self {
            Self {
                data: Arc::new(data),
                reads: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn read_count(&self, key: &str) -> usize {
            self.reads
                .lock()
                .expect("should lock counting secret-store reads")
                .iter()
                .filter(|read_key| read_key.as_str() == key)
                .count()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformSecretStore for CountingSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            self.reads
                .lock()
                .expect("should lock counting secret-store reads")
                .push(key.to_string());
            self.data
                .get(key)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::SecretStore))
        }

        async fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _name: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    #[test]
    fn proxy_missing_param_returns_400() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let req = build_http_request(Method::GET, "https://example.com/first-party/proxy");
            let err: Report<TrustedServerError> =
                handle_first_party_proxy(&settings, &noop_services(), req)
                    .await
                    .expect_err("expected error");
            assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
        });
    }

    #[test]
    fn proxy_missing_or_invalid_token_returns_400() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            // missing tstoken should 400
            let req = build_http_request(
                Method::GET,
                "https://example.com/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example%2Fa.png",
            );
            let err: Report<TrustedServerError> =
                handle_first_party_proxy(&settings, &noop_services(), req)
                    .await
                    .expect_err("expected error");
            assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
        });
    }

    #[test]
    fn proxy_sign_returns_signed_url() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.publisher.public_origin =
                Some("https://ads.publisher.example:8443".to_string());
            let body = serde_json::json!({
                "url": "https://cdn.example/asset.js?c=3&b=2",
            });
            let req = build_http_post_json_request("https://edge.example/first-party/sign", &body);
            let resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect("sign ok");
            assert_eq!(resp.status(), StatusCode::OK);
            let json = response_body_string(resp);
            assert!(
                json.contains("https://ads.publisher.example:8443/first-party/proxy?tsurl="),
                "{}",
                json
            );
            assert!(json.contains("tsexp"), "{}", json);
            assert!(
                json.contains("\"base\":\"https://cdn.example/asset.js\""),
                "{}",
                json
            );
        });
    }

    #[test]
    fn proxy_sign_enforces_allowed_domains_for_get_and_post() {
        struct Case {
            name: &'static str,
            allowed_domains: &'static [&'static str],
            target: &'static str,
            signing_uri: &'static str,
            expected_base: Option<&'static str>,
            permitted: bool,
        }

        let cases = [
            Case {
                name: "exact match",
                allowed_domains: &["cdn.example.com"],
                target: "https://cdn.example.com/asset.js",
                signing_uri: "https://edge.example.com/first-party/sign",
                expected_base: None,
                permitted: true,
            },
            Case {
                name: "rejected host",
                allowed_domains: &["allowed.example.com"],
                target: "https://blocked.example.com/asset.js",
                signing_uri: "https://edge.example.com/first-party/sign",
                expected_base: None,
                permitted: false,
            },
            Case {
                name: "wildcard match",
                allowed_domains: &["*.example.com"],
                target: "https://static.cdn.example.com/asset.js",
                signing_uri: "https://edge.example.com/first-party/sign",
                expected_base: None,
                permitted: true,
            },
            Case {
                name: "protocol-relative match",
                allowed_domains: &["cdn.example.com"],
                target: "//cdn.example.com/asset.js",
                signing_uri: "http://edge.example.com/first-party/sign",
                expected_base: Some("http://cdn.example.com/asset.js"),
                permitted: true,
            },
            Case {
                name: "open mode",
                allowed_domains: &[],
                target: "https://unlisted.example.com/asset.js",
                signing_uri: "https://edge.example.com/first-party/sign",
                expected_base: None,
                permitted: true,
            },
            Case {
                name: "user information cannot bypass",
                allowed_domains: &["allowed.example.com"],
                target: "https://allowed.example.com@blocked.example.com:9443/path",
                signing_uri: "https://edge.example.com/first-party/sign",
                expected_base: None,
                permitted: false,
            },
            Case {
                name: "non-host URL parts are ignored",
                allowed_domains: &["allowed.example.com"],
                target: "https://user@allowed.example.com:9443/path?cache=1#section",
                signing_uri: "https://edge.example.com/first-party/sign",
                expected_base: None,
                permitted: true,
            },
        ];

        futures::executor::block_on(async {
            for case in cases {
                for method in [&Method::GET, &Method::POST] {
                    let label = format!("{} {}", method.as_str(), case.name);
                    let mut settings = create_test_settings();
                    settings.proxy.allowed_domains = case
                        .allowed_domains
                        .iter()
                        .map(|domain| (*domain).to_string())
                        .collect();
                    let req = build_proxy_sign_request(method, case.signing_uri, case.target);
                    let result =
                        handle_first_party_proxy_sign(&settings, &noop_services(), req).await;

                    if case.permitted {
                        let response = result.unwrap_or_else(|error| {
                            panic!("{label} should sign target: {error:?}")
                        });
                        assert_eq!(
                            response.status(),
                            StatusCode::OK,
                            "{label} should return 200"
                        );
                        let body: serde_json::Value =
                            serde_json::from_str(&response_body_string(response))
                                .expect("should parse sign response JSON");
                        let href = body["href"]
                            .as_str()
                            .expect("should include string href in sign response");
                        let signed_url =
                            url::Url::parse(href).expect("should parse absolute signed proxy URL");
                        assert_eq!(
                            signed_url.origin().ascii_serialization(),
                            settings.publisher.effective_public_origin(),
                            "{label} should use the configured public origin"
                        );
                        assert_eq!(
                            signed_url.path(),
                            "/first-party/proxy",
                            "{label} should return a first-party proxy href"
                        );
                        let signed_params: HashMap<_, _> = signed_url.query_pairs().collect();
                        for parameter in ["tsurl", "tstoken", "tsexp"] {
                            assert!(
                                signed_params.contains_key(parameter),
                                "{label} should include {parameter} in signed href"
                            );
                        }
                        if let Some(expected_base) = case.expected_base {
                            assert_eq!(
                                body["base"].as_str(),
                                Some(expected_base),
                                "{label} should inherit the signing request scheme"
                            );
                        }
                    } else {
                        let error = match result {
                            Ok(response) => {
                                panic!("{label} should reject target, got {response:?}")
                            }
                            Err(error) => error,
                        };
                        assert!(
                            matches!(
                                error.current_context(),
                                TrustedServerError::AllowlistViolation { .. }
                            ),
                            "{label} should return AllowlistViolation, got {error:?}"
                        );
                        assert_eq!(
                            error.current_context().status_code(),
                            StatusCode::FORBIDDEN,
                            "{label} should map allowlist rejection to 403"
                        );
                    }
                }
            }
        });
    }

    #[test]
    fn proxy_sign_rejects_invalid_url() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let body = serde_json::json!({
                "url": "data:image/png;base64,AAAA",
            });
            let req = build_http_post_json_request("https://edge.example/first-party/sign", &body);
            let err: Report<TrustedServerError> =
                handle_first_party_proxy_sign(&settings, &noop_services(), req)
                    .await
                    .expect_err("expected error");
            assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
        });
    }

    #[test]
    fn proxy_sign_rejects_excluded_urls() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.rewrite.exclude_domains = vec!["cdn.example".to_owned()];

            for method in [&Method::GET, &Method::POST] {
                for url in [
                    "https://cdn.example/asset.js",
                    "//cdn.example/asset.js",
                    "https://CDN.Example/asset.js",
                ] {
                    let req = build_proxy_sign_request(
                        method,
                        "https://edge.example/first-party/sign",
                        url,
                    );
                    let err: Report<TrustedServerError> =
                        handle_first_party_proxy_sign(&settings, &noop_services(), req)
                            .await
                            .expect_err("should reject excluded URL");

                    assert_eq!(
                        err.current_context().status_code(),
                        StatusCode::BAD_GATEWAY,
                        "{} should reject excluded URL `{url}` as unsupported",
                        method.as_str()
                    );
                }
            }
        });
    }

    #[test]
    fn proxy_sign_rejects_excluded_urls_case_insensitively() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.rewrite.exclude_domains = vec!["CDN.Example".to_owned()];

            for method in [&Method::GET, &Method::POST] {
                for url in ["https://cdn.example/asset.js", "//cdn.example/asset.js"] {
                    let req = build_proxy_sign_request(
                        method,
                        "https://edge.example/first-party/sign",
                        url,
                    );
                    let err: Report<TrustedServerError> =
                        handle_first_party_proxy_sign(&settings, &noop_services(), req)
                            .await
                            .expect_err("should reject a host excluded by a mixed-case entry");

                    assert_eq!(
                        err.current_context().status_code(),
                        StatusCode::BAD_GATEWAY,
                        "{} should reject `{url}` excluded by a mixed-case entry as unsupported",
                        method.as_str()
                    );
                }
            }
        });
    }

    #[test]
    fn proxy_sign_applies_rewrite_include_domains_for_get_and_post() {
        struct Case {
            name: &'static str,
            include_domains: &'static [&'static str],
            exclude_domains: &'static [&'static str],
            allowed_domains: &'static [&'static str],
            target: &'static str,
            expected: StatusCode,
        }

        let cases = [
            Case {
                name: "listed absolute",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "https://img.example.com/a.png",
                expected: StatusCode::OK,
            },
            Case {
                name: "listed protocol-relative",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "//img.example.com/a.png",
                expected: StatusCode::OK,
            },
            Case {
                name: "off-list absolute",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "https://img.example.net/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "off-list protocol-relative",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "//img.example.net/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "host in both lists",
                include_domains: &["*.example.com"],
                exclude_domains: &["img.example.com"],
                allowed_domains: &[],
                target: "https://img.example.com/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "off include list and off proxy allowlist",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &["*.example.org"],
                target: "https://img.example.net/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "listed but off proxy allowlist",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &["*.example.org"],
                target: "https://img.example.com/a.png",
                expected: StatusCode::FORBIDDEN,
            },
            Case {
                name: "mixed-case host",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "https://IMG.Example.COM/a.png",
                expected: StatusCode::OK,
            },
        ];

        let to_owned = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| (*value).to_owned()).collect()
        };

        futures::executor::block_on(async {
            for case in &cases {
                for method in [&Method::GET, &Method::POST] {
                    let label = format!("{} {}", method.as_str(), case.name);
                    let mut settings = create_test_settings();
                    settings.rewrite.include_domains = to_owned(case.include_domains);
                    settings.rewrite.exclude_domains = to_owned(case.exclude_domains);
                    settings.proxy.allowed_domains = to_owned(case.allowed_domains);
                    let req = build_proxy_sign_request(
                        method,
                        "https://edge.example.com/first-party/sign",
                        case.target,
                    );

                    let status =
                        match handle_first_party_proxy_sign(&settings, &noop_services(), req).await
                        {
                            Ok(response) => response.status(),
                            Err(error) => error.current_context().status_code(),
                        };

                    assert_eq!(
                        status, case.expected,
                        "{label} should return {}",
                        case.expected
                    );
                }
            }
        });
    }

    #[test]
    fn proxy_sign_inherits_the_request_scheme_for_protocol_relative_urls() {
        // A protocol-relative target adopts the scheme the visitor is on. It
        // must come from the request, not from the parser's placeholder base:
        // origin-form URIs (browsers, and the Axum adapter verbatim) carry no
        // scheme, so an HTTP dev server would otherwise sign an HTTPS target
        // and proxy TLS against a plaintext service.
        futures::executor::block_on(async {
            let settings = create_test_settings();

            for (uri, expected) in [
                ("/first-party/sign", "http"),
                ("http://edge.example/first-party/sign", "http"),
                ("https://edge.example/first-party/sign", "https"),
            ] {
                let body = serde_json::json!({ "url": "//cdn.example/asset.js" });
                let req = build_http_post_json_request(uri, &body);
                let resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                    .await
                    .expect("should sign protocol-relative URL");
                let json = response_body_string(resp);

                assert!(
                    json.contains(&format!("\"base\":\"{expected}://cdn.example/asset.js\"")),
                    "request `{uri}` should sign a {expected} target: {json}"
                );
            }
        });
    }

    #[test]
    fn checked_ingress_proxy_sign_overrides_absolute_request_target_scheme() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            for (wire_scheme, transport_scheme, checked_scheme) in [
                ("https", Some("http"), "http"),
                ("http", Some("https"), "https"),
                ("https", None, "http"),
            ] {
                let body = serde_json::json!({ "url": "//cdn.example/asset.js" });
                let mut req = build_http_post_json_request(
                    format!("{wire_scheme}://edge.example/first-party/sign"),
                    &body,
                );
                let origin = transport_scheme.map(|scheme| {
                    InboundOrigin::parse(scheme, "edge.example", OriginSource::TransportBinding)
                        .expect("should validate the adapter-owned origin")
                });
                req.extensions_mut().insert(RequestIngress::new(
                    CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
                    origin,
                    HeaderFidelity::default(),
                ));
                let response = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                    .await
                    .expect("should sign using checked ingress metadata");
                let json = response_body_string(response);
                assert!(
                    json.contains(&format!(
                        "\"base\":\"{checked_scheme}://cdn.example/asset.js\""
                    )),
                    "should not let an absolute request target override checked transport: {json}"
                );
            }
        });
    }

    #[test]
    fn public_origin_proxy_sign_overrides_explicit_transport_scheme() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.publisher.domain = "publisher.example.com".to_owned();
            settings.trusted_forwarder = Some(crate::settings::TrustedForwarderConfig {
                auth_header: crate::forwarder::FORWARDER_AUTH_HEADER.to_owned(),
                shared_secret: crate::redacted::Redacted::new(
                    "fictional-forwarder-secret-0123456789".to_owned(),
                ),
            });
            let body = serde_json::json!({ "url": "//cdn.example.com/asset.js" });
            let mut req =
                build_http_post_json_request("http://internal.example.com/first-party/sign", &body);
            req.headers_mut().insert(
                crate::forwarder::FORWARDER_AUTH_HEADER,
                HeaderValue::from_static("fictional-forwarder-secret-0123456789"),
            );
            req.headers_mut().insert(
                "x-forwarded-host",
                HeaderValue::from_static("publisher.example.com:8443"),
            );
            req.headers_mut()
                .insert("x-forwarded-proto", HeaderValue::from_static("https"));
            crate::test_support::tests::with_request_parts(&mut req, |parts| {
                crate::forwarder::prepare_trusted_forwarder(parts, &settings);
            });

            let resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect("should sign protocol-relative URL");
            let json = response_body_string(resp);
            assert!(
                json.contains("\"base\":\"https://cdn.example.com/asset.js\""),
                "should inherit authenticated public HTTPS: {json}"
            );
        });
    }

    #[test]
    fn proxy_sign_preserves_non_standard_port() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let body = serde_json::json!({
                "url": "https://cdn.example.com:9443/img/300x250.svg",
            });
            let req = build_http_post_json_request("https://edge.example/first-party/sign", &body);
            let resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect("should sign URL with non-standard port");
            assert_eq!(resp.status(), StatusCode::OK);
            let json = response_body_string(resp);
            // Port 9443 should be preserved (URL-encoded as %3A9443)
            assert!(
                json.contains("%3A9443"),
                "Port should be preserved in signed URL: {}",
                json
            );
        });
    }

    #[test]
    fn proxy_request_config_supports_streaming_and_headers() {
        let cfg = ProxyRequestConfig::new("https://example.com/asset")
            .with_body(vec![1, 2, 3])
            .with_header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            )
            .without_forward_headers()
            .with_streaming()
            .with_stream_response();

        assert_eq!(cfg.target_url, "https://example.com/asset");
        assert!(cfg.follow_redirects, "should follow redirects by default");
        assert!(cfg.forward_ec_id, "should forward EC ID by default");
        assert_eq!(cfg.body.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(cfg.headers.len(), 1, "should include custom header");
        assert!(
            !cfg.copy_request_headers,
            "should allow routes to disable default request-header forwarding"
        );
        assert!(
            cfg.stream_passthrough,
            "should enable streaming passthrough"
        );
        assert!(
            cfg.stream_response,
            "should request streaming platform responses"
        );
    }

    #[test]
    fn proxy_request_config_forwards_curated_headers_by_default() {
        let cfg = ProxyRequestConfig::new("https://example.com/asset");

        assert!(
            cfg.copy_request_headers,
            "should forward curated request headers unless a route opts out"
        );
    }

    #[test]
    fn reconstruct_rejects_expired_tsexp() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let settings = create_test_settings();
        let tsurl = "https://cdn.example/asset.js";
        let expired = SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs();
        let canonical = format!("{}?tsexp={}", tsurl, expired);
        let sig = crate::http_util::compute_encrypted_sha256_token(&settings, &canonical);
        let tsurl_encoded =
            url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>();
        let url = format!(
            "https://edge.example/first-party/proxy?tsurl={}&tsexp={}&tstoken={}",
            tsurl_encoded, expired, sig
        );

        let err: Report<TrustedServerError> =
            reconstruct_and_validate_signed_target(&settings, &url)
                .expect_err("expected expiration failure");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn reconstruct_rejects_tampered_tstoken() {
        let settings = create_test_settings();
        let tsurl = "https://cdn.example/asset.js";
        let tsurl_encoded =
            url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>();
        // Syntactically valid base64url token of the right length, but not the correct signature
        let bad_token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let url = format!(
            "https://edge.example/first-party/proxy?tsurl={}&tstoken={}",
            tsurl_encoded, bad_token
        );

        let err: Report<TrustedServerError> =
            reconstruct_and_validate_signed_target(&settings, &url)
                .expect_err("should reject tampered token");
        assert_eq!(
            err.current_context().status_code(),
            StatusCode::FORBIDDEN,
            "should return 403 for invalid tstoken"
        );
    }

    #[test]
    fn click_missing_params_returns_400() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let req = build_http_request(Method::GET, "https://edge.example/first-party/click");
            let err: Report<TrustedServerError> =
                handle_first_party_click(&settings, &noop_services(), req)
                    .await
                    .expect_err("expected error");
            assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
        });
    }

    #[test]
    fn click_valid_token_redirects() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let tsurl = "https://cdn.example/a.png";
            let params = "foo=1&bar=2";
            let full = format!("{}?{}", tsurl, params);
            let sig = crate::http_util::compute_encrypted_sha256_token(&settings, &full);
            let req = build_http_request(
                Method::GET,
                format!(
                    "https://edge.example/first-party/click?tsurl={}&{}&tstoken={}",
                    url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
                    params,
                    sig
                ),
            );
            let resp = handle_first_party_click(&settings, &noop_services(), req)
                .await
                .expect("should redirect");
            assert_eq!(resp.status(), StatusCode::FOUND);
            let loc = resp
                .headers()
                .get(http::header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            assert_eq!(loc, full);
        });
    }

    #[test]
    fn click_appends_ec_id_when_present() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let tsurl = "https://cdn.example/a.png";
            let params = "foo=1";
            let full = format!("{}?{}", tsurl, params);
            let sig = crate::http_util::compute_encrypted_sha256_token(&settings, &full);
            let mut req = build_http_request(
                Method::GET,
                format!(
                    "https://edge.example/first-party/click?tsurl={}&{}&tstoken={}",
                    url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
                    params,
                    sig
                ),
            );
            req.headers_mut().insert(
                crate::constants::HEADER_X_TS_EC,
                HeaderValue::from_static("ec-123"),
            );

            let resp = handle_first_party_click(&settings, &noop_services(), req)
                .await
                .expect("should redirect");

            let loc = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .expect("Location header should be present and valid");
            let parsed = url::Url::parse(loc).expect("Location should be a valid URL");
            let mut pairs: std::collections::HashMap<String, String> = parsed
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            assert_eq!(pairs.remove("foo").as_deref(), Some("1"));
            assert_eq!(pairs.remove("ts-ec").as_deref(), Some("ec-123"));
            assert!(pairs.is_empty());
        });
    }

    #[test]
    fn proxy_rebuild_adds_and_removes_params() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            // Build a properly signed click URL — rebuild validates tstoken before mutating.
            let tsurl = "https://cdn.example/landing.html";
            let full_for_token = format!("{}?x=1", tsurl);
            let token =
                crate::http_util::compute_encrypted_sha256_token(&settings, &full_for_token);
            let tsclick = format!(
                "/first-party/click?tsurl={}&x=1&tstoken={}",
                url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
                token,
            );
            let body = serde_json::json!({
                "tsclick": tsclick,
                "add": {"y": "2"},
                "del": ["x"],
            });
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://edge.example/first-party/proxy-rebuild")
                .body(EdgeBody::from(
                    serde_json::to_string(&body).expect("test JSON should serialize"),
                ))
                .expect("should build proxy rebuild request");
            let resp = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect("rebuild ok");
            assert_eq!(resp.status(), StatusCode::OK);
            let json = response_body_string(resp);
            assert!(json.contains("/first-party/click?tsurl="));
            assert!(json.contains("tstoken"));
            // Diagnostics
            assert!(
                json.contains("\"base\":\"https://cdn.example/landing.html\""),
                "{}",
                json
            );
            assert!(json.contains("\"added\":{\"y\":\"2\"}"), "{}", json);
            assert!(json.contains("\"removed\":[\"x\"]"), "{}", json);
        });
    }

    #[test]
    fn proxy_rebuild_accepts_legacy_and_absolute_clicks_and_canonicalizes_output() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.publisher.public_origin =
                Some("https://ads.publisher.example:8443".to_string());
            let tsurl = "https://advertiser.example.com/landing";
            let token = crate::http_util::compute_encrypted_sha256_token(&settings, tsurl);
            let legacy = format!(
                "/first-party/click?tsurl={}&tstoken={token}",
                url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>()
            );

            for tsclick in [
                legacy.clone(),
                format!("https://alias.example{legacy}"),
                format!("//alias.example{legacy}"),
                format!(r"\\alias.example{legacy}"),
                format!(r"/\alias.example{legacy}"),
                format!(" //alias.example{legacy}"),
                format!("\t//alias.example{legacy}"),
                format!("https:/alias.example{legacy}"),
            ] {
                let body = serde_json::json!({ "tsclick": tsclick });
                let req = HttpRequest::builder()
                    .method(Method::POST)
                    .uri("https://edge.example/first-party/proxy-rebuild")
                    .body(EdgeBody::from(
                        serde_json::to_string(&body).expect("should serialize test JSON"),
                    ))
                    .expect("should build rebuild request");
                let response = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                    .await
                    .expect("should rebuild a valid click");
                let body: serde_json::Value = serde_json::from_str(&response_body_string(response))
                    .expect("should return JSON");
                assert!(
                    body["href"]
                        .as_str()
                        .expect("should return href")
                        .starts_with("https://ads.publisher.example:8443/first-party/click?"),
                    "should canonicalize to public_origin"
                );
            }

            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("tsclick", &legacy)
                .finish();
            let req = build_http_request(
                Method::GET,
                format!("https://edge.example/first-party/proxy-rebuild?{query}"),
            );
            let response = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect("should rebuild legacy GET click");
            assert_eq!(response.status(), StatusCode::FOUND);
            assert!(
                response_header(&response, header::LOCATION)
                    .expect("should return a Location header")
                    .starts_with("https://ads.publisher.example:8443/first-party/click?")
            );
        });
    }

    #[test]
    fn proxy_rebuild_rejects_invalid_click_schemes_and_paths_with_valid_tokens() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let signed = signed_click_target(&settings);
            for tsclick in [
                format!("ftp://edge.example{signed}"),
                format!("data:{signed}"),
                format!(
                    "https://edge.example{}",
                    signed.replace("/first-party/click", "/first-party/click-extra")
                ),
                format!(
                    "https://edge.example{}",
                    signed.replace("/first-party/click", "/other-path")
                ),
            ] {
                let body = serde_json::json!({ "tsclick": tsclick });
                let req = HttpRequest::builder()
                    .method(Method::POST)
                    .uri("https://edge.example/first-party/proxy-rebuild")
                    .body(EdgeBody::from(
                        serde_json::to_string(&body).expect("should serialize test JSON"),
                    ))
                    .expect("should build rebuild request");
                let err = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                    .await
                    .expect_err("should reject the scheme or path despite a valid token");
                assert!(
                    err.to_string().contains("invalid tsclick path"),
                    "should reject {tsclick:?} before token validation: {err:?}"
                );
            }
        });
    }

    // Build a signed `/first-party/click` target with one extra param.
    fn signed_click_target(settings: &crate::settings::Settings) -> String {
        let tsurl = "https://cdn.example/landing.html";
        let full_for_token = format!("{}?x=1", tsurl);
        let token = crate::http_util::compute_encrypted_sha256_token(settings, &full_for_token);
        format!(
            "/first-party/click?tsurl={}&x=1&tstoken={}",
            url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
            token,
        )
    }

    #[test]
    fn proxied_responses_strip_upstream_cors_policy() {
        // Emitting no header of our own is not enough: the buffered path
        // preserves upstream headers and the streaming path passes the response
        // through, so an upstream CORS grant would still make the body readable
        // from an opaque creative frame.
        let settings = create_test_settings();

        for upstream_origin in ["*", "null", "https://attacker.example"] {
            for streaming in [false, true] {
                let mut beresp = build_http_response(StatusCode::OK, EdgeBody::from("secret"));
                beresp.headers_mut().insert(
                    header::ACCESS_CONTROL_ALLOW_ORIGIN,
                    HeaderValue::from_str(upstream_origin).expect("valid header"),
                );
                beresp.headers_mut().insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
                beresp.headers_mut().insert(
                    header::ACCESS_CONTROL_EXPOSE_HEADERS,
                    HeaderValue::from_static("x-secret"),
                );

                let out = super::finalize_response(
                    &settings,
                    &build_http_request(Method::GET, "https://edge.example/first-party/proxy"),
                    "https://cdn.example/asset.bin",
                    beresp,
                    streaming,
                )
                .expect("finalize should succeed");

                assert_eq!(
                    response_header(&out, header::ACCESS_CONTROL_ALLOW_ORIGIN),
                    None,
                    "upstream '{upstream_origin}' grant must be stripped (streaming={streaming})"
                );
                assert_eq!(
                    response_header(&out, header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
                    None,
                    "upstream credentials grant must be stripped (streaming={streaming})"
                );
                assert_eq!(
                    response_header(&out, header::ACCESS_CONTROL_EXPOSE_HEADERS),
                    None,
                    "upstream expose-headers must be stripped (streaming={streaming})"
                );
            }
        }
    }

    #[test]
    fn proxied_responses_are_not_cross_origin_readable() {
        // `/first-party/proxy` forwards the EC ID and client-derived headers and
        // runs in open mode with an empty allowlist, and the rewriter mints its
        // signed URLs from bidder-controlled markup when sanitization is off.
        // An allow-origin header would therefore let a creative in an opaque
        // frame read arbitrary proxied bodies, so neither branch may emit one.
        let settings = create_test_settings();

        let buffered = super::finalize_response(
            &settings,
            &build_http_request(Method::GET, "https://edge.example/first-party/proxy"),
            "https://cdn.example/app.mjs",
            build_http_response(StatusCode::OK, EdgeBody::from("body{}")),
            false,
        )
        .expect("finalize should succeed");

        assert_eq!(
            response_header(&buffered, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            None,
            "proxied responses must not be readable cross-origin"
        );

        let streamed = super::finalize_response(
            &settings,
            &build_http_request(Method::GET, "https://edge.example/first-party/proxy"),
            "https://cdn.example/font.woff2",
            build_http_response(StatusCode::OK, EdgeBody::from("font")),
            true,
        )
        .expect("streaming finalize should succeed");

        assert_eq!(
            response_header(&streamed, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            None,
            "streaming passthrough must agree"
        );
    }

    #[test]
    fn first_party_click_accepts_origin_form_uri() {
        // Browsers send origin-form request targets and the Axum adapter passes
        // them through verbatim, so the shared signed-target parser must accept
        // both forms — otherwise the second hop of the rebuild redirect chain
        // fails instead of reaching the advertiser.
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let req = HttpRequest::builder()
                .method(Method::GET)
                .uri(signed_click_target(&settings))
                .body(EdgeBody::empty())
                .expect("should build origin-form click request");

            let resp = handle_first_party_click(&settings, &noop_services(), req)
                .await
                .expect("origin-form click should succeed");

            assert_eq!(
                resp.status(),
                StatusCode::FOUND,
                "should redirect to the advertiser"
            );
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .expect("should carry a Location header");
            assert!(
                location.starts_with("https://cdn.example/landing.html"),
                "should redirect to the signed target: {location}"
            );
        });
    }

    #[test]
    fn first_party_proxy_accepts_origin_form_uri() {
        // Same parser, reached through the proxy endpoint: an origin-form target
        // must validate rather than fail as a relative URL.
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let tsurl = "https://cdn.example/pixel.png";
            let token = crate::http_util::compute_encrypted_sha256_token(&settings, tsurl);
            let uri = format!(
                "/first-party/proxy?tsurl={}&tstoken={}",
                url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
                token,
            );
            let req = HttpRequest::builder()
                .method(Method::GET)
                .uri(&uri)
                .body(EdgeBody::empty())
                .expect("should build origin-form proxy request");

            // The signature check runs before any upstream fetch; reaching a
            // non-"Invalid URL" outcome proves origin-form parsing succeeded.
            let result = handle_first_party_proxy(&settings, &noop_services(), req).await;
            if let Err(err) = result {
                let rendered = format!("{err:?}");
                assert!(
                    !rendered.contains("Invalid URL"),
                    "origin-form proxy target must parse: {rendered}"
                );
            }
        });
    }

    #[test]
    fn proxy_rebuild_get_with_origin_form_uri_redirects() {
        // The opaque-origin creative click guard navigates to this endpoint,
        // and browsers (via the Axum/Spin adapters) deliver origin-form URIs
        // (`/path?query`) rather than absolute URLs. The handler must parse the
        // query and answer with the 302 recovery redirect.
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let tsurl = "https://cdn.example/landing.html";
            let full_for_token = format!("{}?x=1", tsurl);
            let token =
                crate::http_util::compute_encrypted_sha256_token(&settings, &full_for_token);
            let tsclick = format!(
                "/first-party/click?tsurl={}&x=1&tstoken={}",
                url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
                token,
            );
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("tsclick", &tsclick);
            query.append_pair("add", "{\"y\":\"2\"}");
            query.append_pair("del", "[\"x\"]");
            let req = HttpRequest::builder()
                .method(Method::GET)
                .uri(format!("/first-party/proxy-rebuild?{}", query.finish()))
                .body(EdgeBody::empty())
                .expect("should build origin-form rebuild request");

            let resp = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect("origin-form GET rebuild should succeed");

            assert_eq!(
                resp.status(),
                StatusCode::FOUND,
                "should answer GET with the 302 recovery redirect"
            );
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .expect("should carry a Location header");
            assert!(
                location.starts_with(&format!(
                    "{}/first-party/click?tsurl=",
                    settings.publisher.effective_public_origin()
                )),
                "should redirect to the rebuilt first-party click: {location}"
            );
            assert!(
                location.contains("y=2") && !location.contains("x=1"),
                "should apply the add/del mutations: {location}"
            );
        });
    }

    #[test]
    fn proxy_rebuild_form_post_redirects_like_get() {
        // The click guard falls back to a form POST when the GET recovery URL
        // would exceed the platform's request-URL limit. A form submission is a
        // navigation, so it must answer with the same 302 a GET does rather than
        // a JSON body the browser would render as text.
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let tsclick = signed_click_target(&settings);
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("tsclick", &tsclick);
            form.append_pair("add", "{\"y\":\"2\"}");
            form.append_pair("del", "[\"x\"]");

            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("/first-party/proxy-rebuild")
                .header(
                    header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded; charset=UTF-8",
                )
                .body(EdgeBody::from(form.finish()))
                .expect("should build form rebuild request");

            let resp = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect("form-encoded rebuild should succeed");

            assert_eq!(
                resp.status(),
                StatusCode::FOUND,
                "a form navigation must redirect, not return JSON"
            );
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .expect("should carry a Location header");
            assert!(
                location.starts_with(&format!(
                    "{}/first-party/click?tsurl=",
                    settings.publisher.effective_public_origin()
                )),
                "should redirect to the rebuilt click: {location}"
            );
            assert!(
                location.contains("y=2") && !location.contains("x=1"),
                "should apply the add/del mutations: {location}"
            );
        });
    }

    #[test]
    fn proxy_rebuild_json_post_still_returns_json() {
        // The same-origin click guard depends on the JSON response shape.
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let body = serde_json::json!({
                "tsclick": signed_click_target(&settings),
                "add": {"y": "2"},
            });
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://edge.example/first-party/proxy-rebuild")
                .header(header::CONTENT_TYPE, "application/json")
                .body(EdgeBody::from(
                    serde_json::to_string(&body).expect("test JSON should serialize"),
                ))
                .expect("should build JSON rebuild request");

            let resp = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect("JSON rebuild should succeed");

            assert_eq!(resp.status(), StatusCode::OK);
            assert!(response_body_string(resp).contains(&format!(
                "\"href\":\"{}/first-party/click?tsurl=",
                settings.publisher.effective_public_origin()
            )));
        });
    }

    #[test]
    fn proxy_rebuild_accepts_absolute_tsclick() {
        // The creative runtime absolutizes hrefs so they resolve inside a
        // `srcdoc` frame; a client that echoes an absolute click back as the
        // rebuild payload must still be accepted, since the signature covers
        // `tsurl` plus params and never the origin.
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let absolute = format!(
                "https://publisher.example{}",
                signed_click_target(&settings)
            );
            let body = serde_json::json!({
                "tsclick": absolute,
                "add": {"y": "2"},
            });
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://edge.example/first-party/proxy-rebuild")
                .body(EdgeBody::from(
                    serde_json::to_string(&body).expect("test JSON should serialize"),
                ))
                .expect("should build proxy rebuild request");

            let resp = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect("absolute tsclick should be accepted");

            assert_eq!(resp.status(), StatusCode::OK);
            let json = response_body_string(resp);
            assert!(
                json.contains("/first-party/click?tsurl="),
                "should rebuild a first-party click: {json}"
            );
            assert!(json.contains("\"added\":{\"y\":\"2\"}"), "{json}");
        });
    }

    // Build a signed `/first-party/click` URL carrying a future `tsexp` replay
    // bound, returning (tsclick, tsexp_value).
    fn signed_click_with_tsexp(settings: &crate::settings::Settings) -> (String, String) {
        let tsurl = "https://cdn.example/landing.html";
        let tsexp = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("should compute unix time")
            .as_secs()
            + 3600)
            .to_string();
        // Token is computed over tsurl + params in order, including tsexp.
        let full_for_token = format!("{tsurl}?x=1&tsexp={tsexp}");
        let token = crate::http_util::compute_encrypted_sha256_token(settings, &full_for_token);
        let tsclick = format!(
            "/first-party/click?tsurl={}&x=1&tsexp={}&tstoken={}",
            url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
            tsexp,
            token,
        );
        (tsclick, tsexp)
    }

    #[test]
    fn proxy_rebuild_rejects_deleting_tsexp() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let (tsclick, _) = signed_click_with_tsexp(&settings);
            let body = serde_json::json!({
                "tsclick": tsclick,
                "del": ["tsexp"],
            });
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://edge.example/first-party/proxy-rebuild")
                .body(EdgeBody::from(
                    serde_json::to_string(&body).expect("test JSON should serialize"),
                ))
                .expect("should build proxy rebuild request");
            let err = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect_err("deleting tsexp must be rejected");
            assert_eq!(
                err.current_context().status_code(),
                StatusCode::BAD_GATEWAY,
                "rejecting a reserved-param deletion should surface as a proxy error"
            );
        });
    }

    #[test]
    fn proxy_rebuild_retains_tsexp_when_not_deleted() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let (tsclick, tsexp) = signed_click_with_tsexp(&settings);
            let body = serde_json::json!({
                "tsclick": tsclick,
                "add": {"y": "2"},
            });
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://edge.example/first-party/proxy-rebuild")
                .body(EdgeBody::from(
                    serde_json::to_string(&body).expect("test JSON should serialize"),
                ))
                .expect("should build proxy rebuild request");
            let resp = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect("rebuild ok");
            assert_eq!(resp.status(), StatusCode::OK);
            let json = response_body_string(resp);
            assert!(
                json.contains(&format!("tsexp={tsexp}")),
                "rebuilt URL must retain the original tsexp replay bound: {json}"
            );
        });
    }

    // --- Additional tests covering helper + edge cases ---

    // Helper to compute canonical full clear URL (normalized query serialization)
    fn canonical_clear_url(src: &str) -> String {
        let mut u = url::Url::parse(src).expect("parse clear url");
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        u.set_query(None);
        u.set_fragment(None);
        if pairs.is_empty() {
            u.as_str().to_string()
        } else {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in &pairs {
                s.append_pair(k, v);
            }
            format!("{}?{}", u.as_str(), s.finish())
        }
    }

    #[test]
    fn reconstruct_valid_with_params_preserves_order() {
        let settings = create_test_settings();
        let clear = "https://cdn.example/asset.js?c=3&b=2&a=1";
        // Simulate creative-generated first-party URL.
        let first_party = creative::build_proxy_url(
            &settings,
            clear,
            &settings.publisher.effective_public_origin(),
        );
        let st = reconstruct_and_validate_signed_target(&settings, &first_party)
            .expect("reconstruct ok");
        assert_eq!(st.tsurl, "https://cdn.example/asset.js");
        assert!(st.had_params);
        assert_eq!(st.target_url, canonical_clear_url(clear));
    }

    #[test]
    fn reconstruct_valid_without_params() {
        let settings = create_test_settings();
        let clear = "https://cdn.example/asset.js";
        let first_party = creative::build_proxy_url(
            &settings,
            clear,
            &settings.publisher.effective_public_origin(),
        );
        let st = reconstruct_and_validate_signed_target(&settings, &first_party)
            .expect("reconstruct ok");
        assert_eq!(st.tsurl, clear);
        assert!(!st.had_params);
        assert_eq!(st.target_url, clear);
    }

    #[test]
    fn proxy_rejects_unsupported_scheme() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let clear = "ftp://cdn.example/file.gif";
            // Build a first-party proxy URL with a token for the unsupported scheme.
            let first_party = creative::build_proxy_url(
                &settings,
                clear,
                &settings.publisher.effective_public_origin(),
            );
            let req = build_http_request(Method::GET, first_party);
            let err: Report<TrustedServerError> =
                handle_first_party_proxy(&settings, &noop_services(), req)
                    .await
                    .expect_err("expected error");
            assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
        });
    }

    #[test]
    fn proxy_invalid_target_url_errors() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            // Intentionally malformed target (host missing) but signed consistently
            let tsurl = "https://"; // invalid URL
            // Manually construct first-party URL matching creative's format
            let full_for_token = tsurl.to_string();
            let sig = crate::http_util::compute_encrypted_sha256_token(&settings, &full_for_token);
            let url = format!(
                "https://edge.example/first-party/proxy?tsurl={}&tstoken={}",
                url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
                sig
            );
            let req = build_http_request(Method::GET, &url);
            let err: Report<TrustedServerError> =
                handle_first_party_proxy(&settings, &noop_services(), req)
                    .await
                    .expect_err("expected error");
            assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
        });
    }

    #[test]
    fn click_sets_cache_control_no_store_private() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let clear = "https://cdn.example/landing.html?x=1";
            let first_party = creative::build_click_url(
                &settings,
                clear,
                &settings.publisher.effective_public_origin(),
            );
            let req = build_http_request(Method::GET, first_party);
            let resp = handle_first_party_click(&settings, &noop_services(), req)
                .await
                .expect("should redirect");
            assert_eq!(resp.status(), StatusCode::FOUND);
            let cc = response_header(&resp, header::CACHE_CONTROL).unwrap_or("");
            assert!(cc.contains("no-store"));
            assert!(cc.contains("private"));
        });
    }

    // --- Finalization path tests (no network) ---

    // Access the finalize helpers within the crate for testing
    use super::finalize_proxied_response as finalize;
    use super::finalize_proxied_response_streaming as finalize_streaming;

    #[test]
    fn html_response_is_rewritten_and_content_type_set() {
        let settings = create_test_settings();
        // HTML with an external image that should be proxied in rewrite
        let html = r#"<html><body><img src="https://cdn.example/a.png"></body></html>"#;
        let mut beresp = build_http_response(StatusCode::OK, EdgeBody::from(html));
        beresp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        beresp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        beresp.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("a=1; Path=/; Secure"),
        );
        // Sanity: header present and creative rewrite works directly
        let ct_pre = response_header(&beresp, header::CONTENT_TYPE)
            .unwrap_or("")
            .to_string();
        assert!(ct_pre.contains("text/html"), "ct_pre={}", ct_pre);
        let direct = creative::rewrite_creative_html(&settings, html);
        assert!(direct.contains("/first-party/proxy?tsurl="), "{}", direct);
        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/a.png", beresp)
            .expect("finalize should succeed");
        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_string();
        assert_eq!(ct, "text/html; charset=utf-8");
        let cc = response_header(&out, header::CACHE_CONTROL)
            .unwrap_or("")
            .to_string();
        assert_eq!(cc, "public, max-age=60");
        let cookie = response_header(&out, header::SET_COOKIE)
            .unwrap_or("")
            .to_string();
        assert!(cookie.contains("a=1"));
    }

    #[test]
    fn css_response_is_rewritten_and_content_type_set() {
        let settings = create_test_settings();
        let css = "body{background:url(https://cdn.example/bg.png)}";
        let mut beresp = build_http_response(StatusCode::OK, EdgeBody::from(css));
        beresp
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/css"));
        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/bg.png", beresp)
            .expect("finalize should succeed");
        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_string();
        let body = response_body_string(out);
        assert!(body.contains("/first-party/proxy?tsurl="), "{}", body);
        assert_eq!(ct, "text/css; charset=utf-8");
    }

    #[test]
    fn auction_rewrite_setting_does_not_change_proxied_html_or_css_rewriting() {
        let mut settings = create_test_settings();
        settings.auction.rewrite_creatives = false;
        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");

        let html = r#"<html><body><img src="https://cdn.example/ad.png"></body></html>"#;
        let mut html_response = build_http_response(StatusCode::OK, EdgeBody::from(html));
        html_response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        let html_output = finalize(
            &settings,
            &req,
            "https://cdn.example/creative.html",
            html_response,
        )
        .expect("should finalize proxied HTML");
        let html_body = response_body_string(html_output);

        let css = "body{background:url(https://cdn.example/bg.png)}";
        let mut css_response = build_http_response(StatusCode::OK, EdgeBody::from(css));
        css_response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/css"));
        let css_output = finalize(
            &settings,
            &req,
            "https://cdn.example/creative.css",
            css_response,
        )
        .expect("should finalize proxied CSS");
        let css_body = response_body_string(css_output);

        assert!(
            html_body.contains("/first-party/proxy?tsurl="),
            "should keep rewriting proxied HTML when auction rewriting is disabled: {html_body}"
        );
        assert!(
            css_body.contains("/first-party/proxy?tsurl="),
            "should keep rewriting proxied CSS when auction rewriting is disabled: {css_body}"
        );
    }

    #[test]
    fn proxied_html_click_wrapping_follows_explicit_rewrite_clicks() {
        let html = r#"<html><head><base href="https://base.example.com/"></head><body><a href="https://landing.example.com/page"><img src="https://cdn.example.com/ad.png"></a><a href="https://excluded.example.com/page">Excluded</a></body></html>"#;
        // (rewrite_creatives, rewrite_clicks, expect wrapped clicks)
        let cases = [
            (true, None, true),
            (false, None, true),
            (true, Some(true), true),
            (true, Some(false), false),
            (false, Some(false), false),
            (false, Some(true), true),
        ];

        for (rewrite_creatives, rewrite_clicks, expect_clicks) in cases {
            let mut settings = create_test_settings();
            settings.auction.rewrite_creatives = rewrite_creatives;
            settings.auction.rewrite_clicks = rewrite_clicks;
            settings.rewrite.exclude_domains = vec!["excluded.example.com".to_owned()];
            let label =
                format!("rewrite_creatives={rewrite_creatives} rewrite_clicks={rewrite_clicks:?}");
            let req = build_http_request(Method::GET, "https://edge.example.com/first-party/proxy");
            let mut response = build_http_response(StatusCode::OK, EdgeBody::from(html));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );

            let body = response_body_string(
                finalize(
                    &settings,
                    &req,
                    "https://cdn.example.com/creative.html",
                    response,
                )
                .expect("should finalize proxied HTML"),
            );

            assert!(
                body.contains("/first-party/proxy?tsurl="),
                "{label}: proxied HTML always proxies assets: {body}"
            );
            // A wrapped landing link carries the click URL in href and data-tsclick.
            assert_eq!(
                body.matches("/first-party/click?tsurl=").count(),
                if expect_clicks { 2 } else { 0 },
                "{label}: click wrapping in proxied HTML: {body}"
            );
            assert_eq!(
                body.matches("data-tsclick").count(),
                usize::from(expect_clicks),
                "{label}: data-tsclick in proxied HTML: {body}"
            );
            if !expect_clicks {
                let landing_href = body
                    .split("<a href=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next());
                assert_eq!(
                    landing_href,
                    Some("https://landing.example.com/page"),
                    "{label}: landing link keeps its raw href: {body}"
                );
            }
            assert!(
                body.contains(r#"<a href="https://excluded.example.com/page">Excluded</a>"#),
                "{label}: excluded link always stays raw: {body}"
            );
            assert!(
                !body.contains("<base"),
                "{label}: proxied HTML always removes <base>: {body}"
            );
            assert!(
                body.contains("/static/tsjs="),
                "{label}: proxied HTML always receives the runtime: {body}"
            );
        }
    }

    #[test]
    fn html_response_rewrite_preserves_non_standard_port() {
        // Verify that HTML rewriting preserves non-standard ports in sub-resource URLs.
        // This is the core test for the port preservation fix.
        let settings = create_test_settings();

        let html = r#"<!DOCTYPE html>
<html>
  <body>
    <a href="//cdn.example.com:9443/click">
      <img src="//cdn.example.com:9443/img/300x250.svg" />
    </a>
    <img src="//cdn.example.com:9443/pixel?pid=test" width="1" height="1" />
  </body>
</html>"#;

        let mut beresp = build_http_response(StatusCode::OK, EdgeBody::from(html));
        beresp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );

        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(
            &settings,
            &req,
            "https://cdn.example.com:9443/creatives/300x250.html",
            beresp,
        )
        .expect("should finalize HTML response with non-standard port URL");

        let body = response_body_string(out);

        // Port 9443 should be preserved (URL-encoded as %3A9443)
        assert!(
            body.contains("cdn.example.com%3A9443"),
            "Port 9443 should be preserved in rewritten URLs. Body:\n{}",
            body
        );
    }

    #[test]
    fn image_accept_sets_fallback_content_type_when_missing() {
        let settings = create_test_settings();
        let beresp = build_http_response(StatusCode::OK, EdgeBody::from("PNG"));
        let mut req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        req.headers_mut()
            .insert(HEADER_ACCEPT, HeaderValue::from_static("image/*"));
        let out = finalize(&settings, &req, "https://cdn.example/pixel.gif", beresp)
            .expect("finalize should succeed");
        // Since CT was missing and Accept indicates image, it should set a valid fallback.
        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("should include Content-Type header");
        assert_eq!(ct, IMAGE_FALLBACK_CONTENT_TYPE);
    }

    #[test]
    fn streaming_image_accept_sets_fallback_content_type_when_missing() {
        let beresp = build_http_response(StatusCode::OK, EdgeBody::from("GIF"));
        let mut req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        req.headers_mut()
            .insert(HEADER_ACCEPT, HeaderValue::from_static("image/*"));

        let out = finalize_streaming(&req, "https://cdn.example/pixel.gif", beresp);
        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("should include Content-Type header");

        assert_eq!(ct, IMAGE_FALLBACK_CONTENT_TYPE);
    }

    #[test]
    fn non_image_non_html_passthrough() {
        let settings = create_test_settings();
        let mut beresp = build_http_response(StatusCode::ACCEPTED, EdgeBody::from("{\"ok\":true}"));
        beresp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://api.example/ok", beresp)
            .expect("finalize should succeed");
        // Should not rewrite, preserve status and content-type
        assert_eq!(out.status(), StatusCode::ACCEPTED);
        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_string();
        assert_eq!(ct, "application/json");
        let body = response_body_string(out);
        assert_eq!(body, "{\"ok\":true}");
    }

    #[test]
    fn html_gzip_response_is_processed_with_compression_preserved() {
        use flate2::Compression;
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use std::io::{Read, Write};

        let settings = create_test_settings();
        let html = r#"<html><body><img src="https://cdn.example/a.png"></body></html>"#;

        // Gzip compress the HTML
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(html.as_bytes())
            .expect("gzip write should succeed");
        let compressed = encoder.finish().expect("gzip finish should succeed");

        let mut beresp = build_http_response(StatusCode::OK, EdgeBody::from(compressed));
        beresp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        beresp
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));

        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/a.png", beresp)
            .expect("finalize should process and succeed");

        // Content-Encoding should be preserved (gzip in -> gzip out)
        let ce = response_header(&out, header::CONTENT_ENCODING)
            .expect("Content-Encoding should be preserved")
            .to_string();
        assert_eq!(ce, "gzip");

        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_string();
        assert_eq!(ct, "text/html; charset=utf-8");

        // Decompress output to verify content was rewritten
        let compressed_output = out.into_body().into_bytes().unwrap_or_default();
        let mut decoder = GzDecoder::new(&compressed_output[..]);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .expect("should decompress output");

        assert!(
            decompressed.contains("/first-party/proxy?tsurl="),
            "HTML should be rewritten: {}",
            decompressed
        );
    }

    #[test]
    fn css_brotli_response_is_processed_with_compression_preserved() {
        use brotli::Decompressor;
        use brotli::enc::writer::CompressorWriter;
        use std::io::{Read, Write};

        let settings = create_test_settings();
        let css = "body{background:url(https://cdn.example/bg.png)}";

        // Brotli compress the CSS
        let mut compressed = Vec::new();
        {
            let mut encoder = CompressorWriter::new(&mut compressed, 4096, 4, 22);
            encoder
                .write_all(css.as_bytes())
                .expect("brotli write should succeed");
        }

        let mut beresp = build_http_response(StatusCode::OK, EdgeBody::from(compressed));
        beresp
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/css"));
        beresp
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("br"));

        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/bg.png", beresp)
            .expect("finalize should process brotli and succeed");

        // Content-Encoding should be preserved (br in -> br out)
        let ce = response_header(&out, header::CONTENT_ENCODING)
            .expect("Content-Encoding should be preserved")
            .to_string();
        assert_eq!(ce, "br");

        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_string();
        assert_eq!(ct, "text/css; charset=utf-8");

        // Decompress output to verify content was rewritten
        let compressed_output = out.into_body().into_bytes().unwrap_or_default();
        let mut decoder = Decompressor::new(&compressed_output[..], 4096);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .expect("should decompress brotli output");

        assert!(
            decompressed.contains("/first-party/proxy?tsurl="),
            "CSS should be rewritten: {}",
            decompressed
        );
    }

    #[test]
    fn html_uncompressed_response_is_processed_without_encoding() {
        let settings = create_test_settings();
        let html = r#"<html><body><img src="https://cdn.example/a.png"></body></html>"#;

        let mut beresp = build_http_response(StatusCode::OK, EdgeBody::from(html));
        beresp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );

        let req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/a.png", beresp)
            .expect("finalize should succeed");

        // No Content-Encoding since input was uncompressed
        assert!(
            response_header(&out, header::CONTENT_ENCODING).is_none(),
            "Content-Encoding should not be set for uncompressed input"
        );

        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_string();
        assert_eq!(ct, "text/html; charset=utf-8");

        let body = response_body_string(out);
        assert!(
            body.contains("/first-party/proxy?tsurl="),
            "HTML should be rewritten: {}",
            body
        );
    }

    // --- Outbound client integration ---

    const SOURCE: &str = "https://source.example.com";

    /// Test services whose outbound requests go to a fresh scripted upstream.
    fn scripted_services() -> (TestClock, ScriptedHttpClient, RuntimeServices) {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        let services = build_services_with_outbound_client(&client, &clock);
        (clock, client, services)
    }

    fn source_url(path: &str) -> String {
        format!("{SOURCE}{path}")
    }

    fn redirect_reply(status: u16, location: &str) -> ScriptedReply {
        ScriptedReply::status(StatusCode::from_u16(status).expect("should be a redirect status"))
            .header("location", location)
    }

    /// Settings every buffered proxy request carries.
    fn proxied_transport() -> ExpectedTransport {
        ExpectedTransport {
            timeout: BUFFERED_TIMEOUT,
            response_mode: ResponseMode::Buffered {
                max_bytes: DEFAULT_BUFFERED_RESPONSE_BYTES,
            },
            response_decoding: ResponseDecoding::Original,
            cache_policy: OutboundCachePolicy::PlatformDefault,
        }
    }

    /// The chain deadline a followed request takes from a clock that never moved.
    fn chain_deadline(clock: &TestClock) -> Deadline {
        clock
            .monotonic_clock()
            .deadline_after(REDIRECT_CHAIN_DEADLINE)
    }

    fn sent_methods(client: &ScriptedHttpClient) -> Vec<Method> {
        client
            .requests()
            .into_iter()
            .map(|request| request.method)
            .collect()
    }

    fn sent_uris(client: &ScriptedHttpClient) -> Vec<String> {
        client
            .requests()
            .iter()
            .map(|request| request.uri.to_string())
            .collect()
    }

    fn sent_bodies(client: &ScriptedHttpClient) -> Vec<Bytes> {
        client
            .requests()
            .into_iter()
            .map(|request| request.body)
            .collect()
    }

    fn sent_header_names(client: &ScriptedHttpClient, index: usize) -> Vec<String> {
        client.requests()[index]
            .headers
            .keys()
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn first_party_proxy_request_readback() {
        let (clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://cdn.example.com/creative.png",
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "image/png")
                .body("png"),
        );
        let settings = create_test_settings();
        let first_party =
            creative::build_proxy_url(&settings, "https://cdn.example.com/creative.png", "");
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri(format!("https://edge.example.com{first_party}"))
            .header(header::USER_AGENT, "fictional-agent/1.0")
            .header(header::ACCEPT, "image/*")
            .header(header::ACCEPT_LANGUAGE, "en-US")
            .header(header::REFERER, "https://publisher.example.com/article")
            .header(HEADER_X_FORWARDED_FOR, "198.51.100.10")
            .header(header::ACCEPT_ENCODING, "zstd")
            .header(header::COOKIE, "session=fictional")
            .header(header::AUTHORIZATION, "Basic ZmljdGlvbmFsOnNlY3JldA==")
            .body(EdgeBody::empty())
            .expect("should build the first-party proxy request");

        let response =
            futures::executor::block_on(handle_first_party_proxy(&settings, &services, req))
                .expect("should proxy the creative");

        assert_eq!(response.status(), StatusCode::OK, "should relay the origin");
        let requests = client.requests();
        assert_eq!(requests.len(), 1, "should send one creative request");
        let sent = &requests[0];
        assert_eq!(
            (&sent.method, sent.uri.to_string()),
            (
                &Method::GET,
                "https://cdn.example.com/creative.png".to_owned()
            ),
            "should fetch the signed target"
        );
        sent.assert_headers(&[
            ("user-agent", "fictional-agent/1.0"),
            ("accept", "image/*"),
            ("accept-language", "en-US"),
            ("referer", "https://publisher.example.com/article"),
            ("x-forwarded-for", "198.51.100.10"),
            ("accept-encoding", SUPPORTED_ENCODINGS),
        ]);
        sent.assert_followed_transport(proxied_transport(), chain_deadline(&clock));
    }

    #[test]
    fn follow_disabled_uses_send_with_forward_headers() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::POST,
            &source_url("/collect"),
            redirect_reply(302, "https://redirect.example.com/final"),
        );
        let settings = create_test_settings();
        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("https://edge.example.com/collect")
            .header(header::USER_AGENT, "fictional-agent/1.0")
            .header(header::ACCEPT, "text/html")
            .header(header::ACCEPT_LANGUAGE, "en-US")
            .header(header::REFERER, "https://publisher.example.com/article")
            .header(HEADER_X_FORWARDED_FOR, "198.51.100.10")
            .header(header::ACCEPT_ENCODING, "zstd")
            .header(header::COOKIE, "session=fictional")
            .body(EdgeBody::empty())
            .expect("should build the client request");
        let target = source_url("/collect");
        let mut config = ProxyRequestConfig::new(&target)
            .without_ec_id()
            .with_body(b"fictional=beacon".to_vec())
            .with_header(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))
            .with_header(header::ACCEPT, HeaderValue::from_static("*/*"));
        config.follow_redirects = false;

        let response =
            futures::executor::block_on(proxy_request(&settings, req, config, &services))
                .expect("should return the unfollowed redirect");

        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "should return the redirect instead of following it"
        );
        let requests = client.requests();
        assert_eq!(requests.len(), 1, "should send one request");
        let sent = &requests[0];
        assert_eq!(
            (&sent.method, sent.uri.to_string(), sent.body.as_ref()),
            (
                &Method::POST,
                source_url("/collect"),
                b"fictional=beacon".as_slice()
            ),
            "should send the method, target, and buffered body"
        );
        sent.assert_headers(&[
            ("user-agent", "fictional-agent/1.0"),
            ("accept", "*/*"),
            ("accept-language", "en-US"),
            ("referer", "https://publisher.example.com/article"),
            ("x-forwarded-for", "198.51.100.10"),
            ("accept-encoding", SUPPORTED_ENCODINGS),
            ("content-type", "text/plain"),
        ]);
        sent.assert_transport(proxied_transport());
    }

    #[test]
    fn proxy_request_streams_response_when_asked() {
        let (clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            &source_url("/resource"),
            ScriptedReply::status(StatusCode::OK).chunks(vec![
                Bytes::from_static(b"stream"),
                Bytes::from_static(b"ed"),
            ]),
        );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        let response = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/resource"))
                .without_ec_id()
                .without_forward_headers()
                .with_streaming()
                .with_stream_response(),
            &services,
        ))
        .expect("should proxy successfully");

        assert!(
            response.body().is_stream(),
            "should keep the origin body as a stream"
        );
        let body = futures::executor::block_on(response.into_body().into_bytes_bounded(1024))
            .expect("should read the streamed body");
        assert_eq!(body.as_ref(), b"streamed", "should stream every chunk");
        client.requests()[0].assert_followed_transport(
            ExpectedTransport {
                timeout: STREAMED_TIMEOUT,
                response_mode: ResponseMode::Streamed,
                response_decoding: ResponseDecoding::Original,
                cache_policy: OutboundCachePolicy::PlatformDefault,
            },
            chain_deadline(&clock),
        );
    }

    #[test]
    fn proxy_target_with_fragment_is_sent_without_it() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            &source_url("/creative.html"),
            ScriptedReply::status(StatusCode::OK).body("ok"),
        );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/creative.html#landing"))
                .without_ec_id()
                .without_forward_headers(),
            &services,
        ))
        .expect("should proxy a target that carries a fragment");

        assert_eq!(
            sent_uris(&client),
            vec![source_url("/creative.html")],
            "should drop the fragment from the upstream target"
        );
    }

    #[test]
    fn proxy_request_allows_open_mode_when_settings_allowlist_is_non_empty() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://blocked.example.com/resource.js",
            ScriptedReply::status(StatusCode::OK).body("ok"),
        );
        let mut settings = create_test_settings();
        settings.proxy.allowed_domains = vec!["allowed.example.com".to_string()];
        let req = build_http_request(Method::GET, "https://edge.example.com/");
        let mut config = ProxyRequestConfig::new("https://blocked.example.com/resource.js")
            .without_ec_id()
            .without_forward_headers();
        config.follow_redirects = false;

        let response =
            futures::executor::block_on(proxy_request(&settings, req, config, &services))
                .expect("open mode should ignore settings.proxy.allowed_domains");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body_string(response), "ok");
    }

    #[test]
    fn proxy_request_uses_config_allowlist_for_redirect_hops() {
        let (_clock, client, services) = scripted_services();
        client
            .respond_to(
                Method::GET,
                "https://origin.example.com/start.js",
                redirect_reply(302, "https://redirected.example.com/final.js"),
            )
            .respond_to(
                Method::GET,
                "https://redirected.example.com/final.js",
                ScriptedReply::status(StatusCode::OK).body("redirected"),
            );
        let mut settings = create_test_settings();
        settings.proxy.allowed_domains = vec!["origin.example.com".to_string()];
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        let response = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new("https://origin.example.com/start.js")
                .without_ec_id()
                .without_forward_headers(),
            &services,
        ))
        .expect("open mode should allow redirect hops outside settings allowlist");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body_string(response), "redirected");
    }

    const REDIRECT_TEST_BODY: &[u8] = b"fictional-payload=example";

    /// Sends `method` through one `status` redirect and returns the upstream and the response.
    ///
    /// A method that carries a body sends [`REDIRECT_TEST_BODY`] with body headers. `GET` and
    /// `HEAD` send none, because `EdgeZero` rejects a body on them.
    fn proxy_through_redirect(
        method: &Method,
        status: u16,
    ) -> (ScriptedHttpClient, Response<EdgeBody>) {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            method.clone(),
            &source_url("/start"),
            redirect_reply(status, "https://redirect.example.com/final"),
        );
        for follow_up in [Method::GET, Method::HEAD, method.clone()] {
            client.respond_to(
                follow_up,
                "https://redirect.example.com/final",
                ScriptedReply::status(StatusCode::OK).body("ok"),
            );
        }
        let settings = create_test_settings();
        let req = build_http_request(method.clone(), "https://edge.example.com/");
        let target = source_url("/start");
        let mut config = ProxyRequestConfig::new(&target)
            .without_ec_id()
            .without_forward_headers();
        if method != Method::GET && method != Method::HEAD {
            config = config
                .with_body(REDIRECT_TEST_BODY.to_vec())
                .with_header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
                .with_header(header::CONTENT_LANGUAGE, HeaderValue::from_static("en-US"));
        }

        let response =
            futures::executor::block_on(proxy_request(&settings, req, config, &services))
                .expect("should proxy through the redirect");

        (client, response)
    }

    #[test]
    fn proxy_request_post_redirect_drops_body_for_301_302_303() {
        for status in [301, 302, 303] {
            let (client, _response) = proxy_through_redirect(&Method::POST, status);

            assert_eq!(
                sent_methods(&client),
                vec![Method::POST, Method::GET],
                "should follow a {status} POST redirect with GET"
            );
            assert_eq!(
                sent_uris(&client)[1],
                "https://redirect.example.com/final",
                "should send the {status} follow-up to the Location target"
            );
            assert_eq!(
                sent_bodies(&client),
                vec![Bytes::from_static(REDIRECT_TEST_BODY), Bytes::new()],
                "should send the body only on the first hop for {status}"
            );
            let first_hop_headers = sent_header_names(&client, 0);
            let follow_up_headers = sent_header_names(&client, 1);
            for body_header in ["content-type", "content-language"] {
                assert!(
                    first_hop_headers.iter().any(|name| name == body_header),
                    "should send {body_header} on the first {status} hop"
                );
                assert!(
                    !follow_up_headers.iter().any(|name| name == body_header),
                    "should not forward {body_header} on a bodyless {status} follow-up"
                );
            }
        }
    }

    #[test]
    fn proxy_request_returns_301_302_unfollowed_for_non_post_body_methods() {
        for method in [Method::PUT, Method::PATCH, Method::DELETE] {
            for status in [301, 302] {
                let (client, response) = proxy_through_redirect(&method, status);

                assert_eq!(
                    response.status().as_u16(),
                    status,
                    "should return the {status} redirect for {method} to the caller"
                );
                assert_eq!(
                    sent_methods(&client),
                    vec![method.clone()],
                    "should not follow a {status} redirect for {method}"
                );
                assert_eq!(
                    sent_bodies(&client),
                    vec![Bytes::from_static(REDIRECT_TEST_BODY)],
                    "should send the {method} body only to the addressed target on {status}"
                );
            }
        }
    }

    #[test]
    fn proxy_request_put_redirect_becomes_bodyless_get_for_303() {
        let (client, response) = proxy_through_redirect(&Method::PUT, 303);

        assert_eq!(
            response.status().as_u16(),
            200,
            "should return the 303 target's response"
        );
        assert_eq!(
            sent_methods(&client),
            vec![Method::PUT, Method::GET],
            "should follow a 303 PUT redirect with GET"
        );
        assert_eq!(
            sent_bodies(&client),
            vec![Bytes::from_static(REDIRECT_TEST_BODY), Bytes::new()],
            "should send the PUT body only on the first hop"
        );
    }

    #[test]
    fn proxy_request_post_redirect_retains_method_and_body_for_307_308() {
        for status in [307, 308] {
            let (client, _response) = proxy_through_redirect(&Method::POST, status);

            assert_eq!(
                sent_methods(&client),
                vec![Method::POST, Method::POST],
                "should keep POST across a {status} redirect"
            );
            assert_eq!(
                sent_bodies(&client),
                vec![
                    Bytes::from_static(REDIRECT_TEST_BODY),
                    Bytes::from_static(REDIRECT_TEST_BODY)
                ],
                "should replay the exact body across a {status} redirect"
            );
            assert!(
                sent_header_names(&client, 1)
                    .iter()
                    .any(|name| name == "content-type"),
                "should keep Content-Type across a {status} redirect"
            );
        }
    }

    #[test]
    fn proxy_request_post_body_stays_dropped_after_302_then_307() {
        let (_clock, client, services) = scripted_services();
        client
            .respond_to(
                Method::POST,
                &source_url("/start"),
                redirect_reply(302, "https://redirect.example.com/second"),
            )
            .respond_to(
                Method::GET,
                "https://redirect.example.com/second",
                redirect_reply(307, "https://redirect.example.com/final"),
            )
            .respond_to(
                Method::GET,
                "https://redirect.example.com/final",
                ScriptedReply::status(StatusCode::OK).body("ok"),
            );
        let settings = create_test_settings();
        let req = build_http_request(Method::POST, "https://edge.example.com/");

        futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers()
                .with_body(REDIRECT_TEST_BODY.to_vec())
                .with_header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
                .with_header(
                    header::CONTENT_ENCODING,
                    HeaderValue::from_static("identity"),
                )
                .with_header(header::CONTENT_LANGUAGE, HeaderValue::from_static("en-US"))
                .with_header(
                    header::CONTENT_LOCATION,
                    HeaderValue::from_static("https://source.example.com/payload"),
                ),
            &services,
        ))
        .expect("should follow the 302 then 307 chain");

        assert_eq!(
            sent_methods(&client),
            vec![Method::POST, Method::GET, Method::GET],
            "should keep the 302 follow-up as GET across the later 307"
        );
        assert_eq!(
            sent_bodies(&client),
            vec![
                Bytes::from_static(REDIRECT_TEST_BODY),
                Bytes::new(),
                Bytes::new()
            ],
            "should not restore the dropped body on the 307 hop"
        );
        let first_hop_headers = sent_header_names(&client, 0);
        for body_header in [
            "content-type",
            "content-encoding",
            "content-language",
            "content-location",
        ] {
            assert!(
                first_hop_headers.iter().any(|name| name == body_header),
                "should send {body_header} on the first hop"
            );
            for hop in [1, 2] {
                assert!(
                    !sent_header_names(&client, hop)
                        .iter()
                        .any(|name| name == body_header),
                    "should not send {body_header} on bodyless hop {hop}"
                );
            }
        }
    }

    /// Scripts a chain of five `302` answers, one more than the redirect limit follows.
    fn script_redirect_limit(client: &ScriptedHttpClient, reply: impl Fn() -> ScriptedReply) {
        client.respond_to(Method::GET, &source_url("/start"), reply());
        for _ in 0..4 {
            client.respond_to(Method::GET, "https://redirect.example.com/next", reply());
        }
    }

    #[test]
    fn proxy_request_redirect_limit_response_strips_cors_policy() {
        let (_clock, client, services) = scripted_services();
        script_redirect_limit(&client, || {
            redirect_reply(302, "https://redirect.example.com/next")
                .header("access-control-allow-origin", "*")
        });
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        let response = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers(),
            &services,
        ))
        .expect("should return the redirect once the limit is reached");

        assert_eq!(
            client.requests().len(),
            5,
            "should stop following after the redirect limit"
        );
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "should return the last redirect response"
        );
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "should strip CORS headers from the redirect-limit response"
        );
    }

    #[test]
    fn proxy_request_redirect_limit_response_honors_stream_passthrough() {
        let (_clock, client, services) = scripted_services();
        script_redirect_limit(&client, || {
            redirect_reply(302, "https://redirect.example.com/next")
                .header("content-type", "text/html")
                .body("<p>moved</p>")
        });
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        let response = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers()
                .with_streaming(),
            &services,
        ))
        .expect("should return the redirect once the limit is reached");

        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("text/html")),
            "should pass the redirect-limit response through without the HTML rewrite"
        );
        assert_eq!(
            response_body_string(response),
            "<p>moved</p>",
            "should keep the redirect body unchanged"
        );
    }

    #[test]
    fn proxy_request_head_redirect_stays_head_for_301_302_303() {
        for status in [301, 302, 303] {
            let (client, _response) = proxy_through_redirect(&Method::HEAD, status);

            assert_eq!(
                sent_methods(&client),
                vec![Method::HEAD, Method::HEAD],
                "should keep HEAD across a {status} redirect"
            );
            assert!(
                sent_bodies(&client).iter().all(Bytes::is_empty),
                "should send no body on either {status} HEAD hop"
            );
        }
    }

    #[test]
    fn proxy_request_head_redirect_keeps_header_only_compressed_response() {
        for (content_type, content_encoding) in [("text/html", "gzip"), ("text/css", "br")] {
            let (_clock, client, services) = scripted_services();
            client
                .respond_to(
                    Method::HEAD,
                    &source_url("/start"),
                    redirect_reply(303, "https://redirect.example.com/final"),
                )
                .respond_to(
                    Method::HEAD,
                    "https://redirect.example.com/final",
                    ScriptedReply::status(StatusCode::OK)
                        .header("content-type", content_type)
                        .header("content-encoding", content_encoding)
                        .header("content-length", "1234")
                        .header("access-control-allow-origin", "*"),
                );
            let settings = create_test_settings();
            let req = build_http_request(Method::HEAD, "https://edge.example.com/");

            let response = futures::executor::block_on(proxy_request(
                &settings,
                req,
                ProxyRequestConfig::new(&source_url("/start"))
                    .without_ec_id()
                    .without_forward_headers(),
                &services,
            ))
            .expect("should return the header-only HEAD response");

            assert_eq!(
                sent_methods(&client),
                vec![Method::HEAD, Method::HEAD],
                "should keep HEAD across the 303 redirect"
            );
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "should return the target's {content_type} HEAD status"
            );
            let headers = response.headers();
            assert_eq!(
                headers.get(header::CONTENT_TYPE),
                Some(&HeaderValue::from_static(content_type)),
                "should keep the upstream Content-Type on a {content_type} HEAD response"
            );
            assert_eq!(
                headers.get(header::CONTENT_ENCODING),
                Some(&HeaderValue::from_static(content_encoding)),
                "should keep the upstream Content-Encoding on a {content_type} HEAD response"
            );
            assert!(
                headers.get(header::CONTENT_LENGTH).is_none(),
                "should drop the upstream Content-Length on a {content_type} HEAD response because a GET rewrites the body"
            );
            assert!(
                headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
                "should still strip CORS headers from a {content_type} HEAD response"
            );
        }
    }

    #[test]
    fn proxy_request_post_redirect_still_enforces_per_hop_allowlist() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::POST,
            &source_url("/start"),
            redirect_reply(307, "https://blocked.example.com/final"),
        );
        let settings = create_test_settings();
        let allowed_domains = vec!["source.example.com".to_string()];
        let req = build_http_request(Method::POST, "https://edge.example.com/");

        let err = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers()
                .with_body(REDIRECT_TEST_BODY.to_vec())
                .with_allowed_domains(&allowed_domains),
            &services,
        ))
        .expect_err("should block a redirect to a host outside the allowlist");

        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::AllowlistViolation { host } if host == "blocked.example.com"
            ),
            "should report the blocked redirect host"
        );
        assert_eq!(
            client.requests().len(),
            1,
            "should never send the body to the blocked host"
        );
    }

    #[test]
    fn redirect_policy_blocks_https_downgrade_before_other_checks() {
        for location in [
            "http://blocked.example.com/final",
            "ftp://files.example.com/final",
        ] {
            let (_clock, client, services) = scripted_services();
            client.respond_to(
                Method::GET,
                &source_url("/start"),
                redirect_reply(302, location),
            );
            let settings = create_test_settings();
            let allowed_domains = vec!["source.example.com".to_string()];
            let req = build_http_request(Method::GET, "https://edge.example.com/");

            let err = futures::executor::block_on(proxy_request(
                &settings,
                req,
                ProxyRequestConfig::new(&source_url("/start"))
                    .without_ec_id()
                    .without_forward_headers()
                    .with_allowed_domains(&allowed_domains)
                    .with_https_only(),
                &services,
            ))
            .expect_err("should block a redirect that leaves HTTPS");

            assert!(
                matches!(err.current_context(), TrustedServerError::Forbidden { .. }),
                "should forbid the {location} hop before its scheme or host is checked"
            );
            assert_eq!(client.requests().len(), 1, "should not follow {location}");
        }
    }

    #[test]
    fn redirect_policy_returns_the_redirect_for_other_schemes() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            &source_url("/start"),
            redirect_reply(302, "ftp://files.example.com/final"),
        );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        let response = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers(),
            &services,
        ))
        .expect("should return the redirect to a scheme it cannot follow");

        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "should return the 302 itself"
        );
        assert_eq!(client.requests().len(), 1, "should not follow the ftp hop");
    }

    #[test]
    fn redirect_chain_past_deadline_fails_with_site_502() {
        let (clock, client, services) = scripted_services();
        let hops = ["/start", "/one", "/two", "/three", "/four"];
        for pair in hops.windows(2) {
            client.respond_to(
                Method::GET,
                &source_url(pair[0]),
                redirect_reply(302, &source_url(pair[1])).after(Duration::from_secs(14)),
            );
        }
        client.respond_to(
            Method::GET,
            &source_url("/four"),
            ScriptedReply::status(StatusCode::OK).after(Duration::from_secs(5)),
        );
        let deadline = chain_deadline(&clock);
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");
        let target = source_url("/start");
        let mut proxied = std::pin::pin!(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&target)
                .without_ec_id()
                .without_forward_headers(),
            &services,
        ));
        let mut context = Context::from_waker(Waker::noop());

        // Each hop answers inside its own 15 s timeout, but the fifth answer lands at 61 s.
        for step in [14, 14, 14, 14, 5] {
            assert!(
                proxied.as_mut().poll(&mut context).is_pending(),
                "should wait for the next hop"
            );
            clock.advance(Duration::from_secs(step));
        }
        let Poll::Ready(outcome) = proxied.as_mut().poll(&mut context) else {
            panic!("should finish once the chain deadline passes");
        };

        let err = outcome.expect_err("should fail past the chain deadline");
        assert!(
            matches!(err.current_context(), TrustedServerError::Proxy { .. }),
            "should keep the proxy's own error"
        );
        assert_eq!(
            err.current_context().status_code(),
            StatusCode::BAD_GATEWAY,
            "should answer the expired chain with the site's 502"
        );
        let requests = client.requests();
        assert_eq!(
            requests.len(),
            5,
            "should send every hop before the deadline"
        );
        for request in &requests {
            request.assert_followed_transport(proxied_transport(), deadline);
        }
    }

    #[test]
    fn location_with_userinfo_fails_with_site_502() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            &source_url("/start"),
            redirect_reply(302, "https://fictional:secret@redirect.example.com/final"),
        );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        let err = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers(),
            &services,
        ))
        .expect_err("should reject a Location that carries userinfo");

        assert_eq!(
            err.current_context().status_code(),
            StatusCode::BAD_GATEWAY,
            "should answer the userinfo redirect with the site's 502"
        );
        assert_eq!(
            client.requests().len(),
            1,
            "should not follow the userinfo Location"
        );
    }

    #[test]
    fn cross_origin_hop_strips_authorization_and_cookie() {
        let (_clock, client, services) = scripted_services();
        client
            .respond_to(
                Method::GET,
                &source_url("/start"),
                redirect_reply(302, &source_url("/same-origin")),
            )
            .respond_to(
                Method::GET,
                &source_url("/same-origin"),
                redirect_reply(302, "https://other.example.com/final"),
            )
            .respond_to(
                Method::GET,
                "https://other.example.com/final",
                ScriptedReply::status(StatusCode::OK).body("ok"),
            );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers()
                .with_header(
                    header::AUTHORIZATION,
                    HeaderValue::from_static("Bearer fictional-token"),
                )
                .with_header(
                    header::COOKIE,
                    HeaderValue::from_static("session=fictional"),
                ),
            &services,
        ))
        .expect("should follow the chain");

        let requests = client.requests();
        assert_eq!(requests.len(), 3, "should follow both redirects");
        for (index, request) in requests.iter().enumerate() {
            let credentials = (
                request.headers.contains_key(header::AUTHORIZATION),
                request.headers.contains_key(header::COOKIE),
            );
            let expected = if index < 2 {
                (true, true)
            } else {
                (false, false)
            };
            assert_eq!(
                credentials, expected,
                "should keep credentials only on same-origin hops, hop {index} to {}",
                request.uri
            );
        }
    }

    #[test]
    fn non_ascii_location_is_percent_encoded_and_followed() {
        let (_clock, client, services) = scripted_services();
        client
            .respond_to(
                Method::GET,
                &source_url("/start"),
                redirect_reply(302, "/caf\u{e9}?q=\u{e9}t\u{e9}"),
            )
            .respond_to(
                Method::GET,
                &source_url("/caf%C3%A9?q=%C3%A9t%C3%A9"),
                ScriptedReply::status(StatusCode::OK).body("followed"),
            );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://edge.example.com/");

        let response = futures::executor::block_on(proxy_request(
            &settings,
            req,
            ProxyRequestConfig::new(&source_url("/start"))
                .without_ec_id()
                .without_forward_headers(),
            &services,
        ))
        .expect("should follow the non-ASCII Location");

        assert_eq!(
            sent_uris(&client),
            vec![
                source_url("/start"),
                source_url("/caf%C3%A9?q=%C3%A9t%C3%A9")
            ],
            "should percent-encode the Location before following it"
        );
        assert_eq!(response_body_string(response), "followed");
    }

    #[test]
    fn rebuild_response_with_body_preserves_multiple_set_cookie_headers() {
        let mut beresp = Response::new(EdgeBody::empty());
        *beresp.status_mut() = StatusCode::OK;
        beresp.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_static("a=1; Path=/; Secure"),
        );
        beresp.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_static("b=2; Path=/; Secure"),
        );

        let rebuilt = rebuild_response_with_body(
            beresp,
            "text/html; charset=utf-8",
            b"rewritten".to_vec(),
            false,
        );

        let cookies: Vec<String> = rebuilt
            .headers()
            .get_all(header::SET_COOKIE)
            .into_iter()
            .map(|value| {
                value
                    .to_str()
                    .expect("should preserve UTF-8 Set-Cookie header values")
                    .to_string()
            })
            .collect();

        assert_eq!(
            cookies,
            vec![
                "a=1; Path=/; Secure".to_string(),
                "b=2; Path=/; Secure".to_string(),
            ],
            "should preserve every Set-Cookie value when rebuilding the response"
        );
    }

    #[test]
    fn build_asset_proxy_target_url_preserves_path_and_query() {
        let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
        let target_url =
            build_asset_proxy_target_url(&route, "/.images/foo.jpg", "auto=webp&width=800")
                .expect("should build asset target URL");

        assert_eq!(
            target_url.as_str(),
            "https://assets.example.com/.images/foo.jpg?auto=webp&width=800",
            "should preserve the incoming path and query exactly"
        );
    }

    #[test]
    fn build_asset_proxy_target_url_applies_cdn_style_rewrite() {
        let mut route = ProxyAssetRoute::new("/.image/", "https://assets-cdn.example.com");
        route.path_pattern = Some(r"^/\.image/(.*)/[^/]+\.([^/.]+)$".to_string());
        route.target_path = Some("/image/upload/$1.$2".to_string());
        let target_url = build_asset_proxy_target_url(
            &route,
            "/.image/c_fit,w_1440/MjA/example.jpg",
            "auto=webp",
        )
        .expect("should build rewritten asset target URL");

        assert_eq!(
            target_url.as_str(),
            "https://assets-cdn.example.com/image/upload/c_fit,w_1440/MjA.jpg?auto=webp",
            "should rewrite the path generically while preserving query parameters"
        );
    }

    #[test]
    fn build_asset_proxy_target_url_applies_static_prefix_rewrite() {
        let mut route = ProxyAssetRoute::new("/_next/static/", "https://static-assets.example.com");
        route.path_pattern = Some(r"^(.*)$".to_string());
        route.target_path = Some("/_network$1".to_string());
        let target_url = build_asset_proxy_target_url(&route, "/_next/static/chunks/app.js", "")
            .expect("should build rewritten static asset target URL");

        assert_eq!(
            target_url.as_str(),
            "https://static-assets.example.com/_network/_next/static/chunks/app.js",
            "should prepend the configured upstream path prefix"
        );
    }

    #[test]
    fn build_asset_proxy_target_url_errors_when_rewrite_pattern_misses() {
        let mut route = ProxyAssetRoute::new("/.image/", "https://assets.example.com");
        route.path_pattern = Some(r"^/\.image/(.*)\.jpg$".to_string());
        route.target_path = Some("/image/upload/$1.jpg".to_string());
        let err = build_asset_proxy_target_url(&route, "/.image/foo.png", "")
            .expect_err("should reject paths that do not match the configured rewrite");

        assert!(
            format!("{err:?}").contains("did not match path_pattern"),
            "should explain the rewrite miss: {err:?}"
        );
    }

    #[test]
    fn build_asset_proxy_target_url_errors_when_rewrite_omits_leading_slash() {
        let mut route = ProxyAssetRoute::new("/assets/", "https://assets.example.com");
        route.path_pattern = Some(r"^/assets/(.*)$".to_string());
        route.target_path = Some("$1".to_string());
        let err = build_asset_proxy_target_url(&route, "/assets/app.js", "")
            .expect_err("should reject rewritten paths without a leading slash");

        assert!(
            format!("{err:?}").contains("must start with '/'"),
            "should explain the invalid rewritten path: {err:?}"
        );
    }

    #[test]
    fn asset_path_skips_image_optimizer_for_svg_extensions() {
        for url in [
            "https://assets.example.com/.images/logo.svg",
            "https://assets.example.com/.images/LOGO.SVG",
            "https://assets.example.com/.images/icon.svgz",
        ] {
            let target_url = url::Url::parse(url).expect("should parse target URL");
            assert!(
                asset_path_skips_image_optimizer(target_url.path()),
                "should skip Image Optimizer for {url}"
            );
        }
    }

    #[test]
    fn asset_path_uses_image_optimizer_for_raster_extensions() {
        for url in [
            "https://assets.example.com/.images/photo.jpg",
            "https://assets.example.com/.images/photo.png",
            "https://assets.example.com/.images/photo.webp",
        ] {
            let target_url = url::Url::parse(url).expect("should parse target URL");
            assert!(
                !asset_path_skips_image_optimizer(target_url.path()),
                "should allow Image Optimizer for {url}"
            );
        }
    }

    #[test]
    fn asset_origin_host_header_omits_standard_port() {
        let target_url = url::Url::parse("https://assets.example.com/.images/foo.jpg")
            .expect("should parse URL");
        let host = asset_origin_host_header(&target_url).expect("should compute Host header");
        assert_eq!(
            host.to_str().expect("should serialize Host header"),
            "assets.example.com",
            "should omit standard HTTPS port from Host header"
        );
    }

    #[test]
    fn asset_origin_host_header_includes_non_standard_port() {
        let target_url = url::Url::parse("https://assets.example.com:8443/.images/foo.jpg")
            .expect("should parse URL");
        let host = asset_origin_host_header(&target_url).expect("should compute Host header");
        assert_eq!(
            host.to_str().expect("should serialize Host header"),
            "assets.example.com:8443",
            "should include non-standard port in Host header"
        );
    }

    /// Settings an asset `GET` carries: a streamed response in its original coding,
    /// through the platform cache.
    fn asset_get_transport() -> ExpectedTransport {
        ExpectedTransport {
            timeout: STREAMED_TIMEOUT,
            response_mode: ResponseMode::Streamed,
            response_decoding: ResponseDecoding::Original,
            cache_policy: OutboundCachePolicy::PlatformDefault,
        }
    }

    /// Settings an asset `HEAD` carries: a buffered response in its original coding,
    /// through the platform cache.
    fn asset_head_transport() -> ExpectedTransport {
        ExpectedTransport {
            timeout: BUFFERED_TIMEOUT,
            response_mode: ResponseMode::Buffered {
                max_bytes: DEFAULT_BUFFERED_RESPONSE_BYTES,
            },
            response_decoding: ResponseDecoding::Original,
            cache_policy: OutboundCachePolicy::PlatformDefault,
        }
    }

    /// `transport` with the readthrough cache bypassed.
    ///
    /// The signed S3 preflight `HEAD`, its raw error-body `GET`, and a signed raw
    /// `HEAD` carry it.
    fn uncached(mut transport: ExpectedTransport) -> ExpectedTransport {
        transport.cache_policy = OutboundCachePolicy::Bypass;
        transport
    }

    /// Splits an asset response and reads its streamed body.
    fn streamed_asset(asset_response: AssetProxyResponse) -> (Response<EdgeBody>, Vec<u8>) {
        let (response, stream_body) = asset_response.into_response_and_body();
        let body = stream_body.expect("should keep the asset body");
        assert!(body.is_stream(), "should keep the asset body as a stream");
        let bytes = futures::executor::block_on(body.into_bytes_bounded(1024 * 1024))
            .expect("should stream the asset body");
        (response, bytes.to_vec())
    }

    #[test]
    fn asset_proxy_get_request_readback() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://assets.example.com:8443/.images/foo.jpg?auto=webp",
            ScriptedReply::status(StatusCode::OK).body("ok"),
        );
        let settings = create_test_settings();
        let mut req = build_http_request(
            Method::GET,
            "https://www.example.com/.images/foo.jpg?auto=webp",
        );
        for (name, value) in [
            (header::USER_AGENT, "asset-agent/1.0"),
            (header::ACCEPT, "image/avif,image/webp,image/*,*/*;q=0.8"),
            (header::ACCEPT_ENCODING, "gzip, br"),
            (header::ACCEPT_LANGUAGE, "en-US"),
            (header::REFERER, "https://www.example.com/article"),
            (header::IF_NONE_MATCH, "\"asset-etag\""),
            (header::IF_MODIFIED_SINCE, "Thu, 13 Mar 2025 08:00:00 GMT"),
            (header::IF_MATCH, "\"asset-precondition\""),
            (header::IF_UNMODIFIED_SINCE, "Thu, 13 Mar 2025 09:00:00 GMT"),
            (header::RANGE, "bytes=0-1023"),
            (header::IF_RANGE, "\"asset-range\""),
            (HEADER_X_FORWARDED_FOR, "198.51.100.10"),
            (header::HeaderName::from_static("x-custom-test"), "drop-me"),
        ] {
            req.headers_mut()
                .insert(name, HeaderValue::from_static(value));
        }
        let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com:8443");

        let (response, body) = streamed_asset(
            futures::executor::block_on(handle_asset_proxy_request(
                &settings, &services, req, &route,
            ))
            .expect("should proxy asset request"),
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body, b"ok", "should stream the asset body");
        let requests = client.requests();
        assert_eq!(requests.len(), 1, "should send one asset request");
        let sent = &requests[0];
        sent.assert_headers(&[
            ("user-agent", "asset-agent/1.0"),
            ("accept", "image/avif,image/webp,image/*,*/*;q=0.8"),
            ("accept-encoding", "gzip, br"),
            ("accept-language", "en-US"),
            ("referer", "https://www.example.com/article"),
            ("if-none-match", "\"asset-etag\""),
            ("if-modified-since", "Thu, 13 Mar 2025 08:00:00 GMT"),
            ("if-match", "\"asset-precondition\""),
            ("if-unmodified-since", "Thu, 13 Mar 2025 09:00:00 GMT"),
            ("range", "bytes=0-1023"),
            ("if-range", "\"asset-range\""),
        ]);
        assert_eq!(
            sent.host_authority, "assets.example.com:8443",
            "should send the asset origin authority, which the Host header named"
        );
        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn handle_asset_proxy_request_strips_unsafe_response_headers() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://assets.example.com/.images/foo.jpg",
            ScriptedReply::status(StatusCode::OK)
                .header(header::SET_COOKIE.as_str(), "asset=1; Path=/; Secure")
                .header(header::SET_COOKIE.as_str(), "other=2; Path=/; Secure")
                .header(
                    header::STRICT_TRANSPORT_SECURITY.as_str(),
                    "max-age=31536000; includeSubDomains; preload",
                )
                .header("clear-site-data", "\"*\"")
                .header(header::ETAG.as_str(), "\"asset-etag\"")
                .chunks(vec![Bytes::from_static(b"chunk")]),
        );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://www.example.com/.images/foo.jpg");
        let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");

        let (response, body) = streamed_asset(
            futures::executor::block_on(handle_asset_proxy_request(
                &settings, &services, req, &route,
            ))
            .expect("should proxy asset request"),
        );

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "should strip upstream Set-Cookie headers from asset responses"
        );
        assert!(
            response
                .headers()
                .get(header::STRICT_TRANSPORT_SECURITY)
                .is_none(),
            "should strip upstream HSTS headers from asset responses"
        );
        assert!(
            response.headers().get("clear-site-data").is_none(),
            "should strip upstream Clear-Site-Data headers from asset responses"
        );
        assert_eq!(
            response_header(&response, header::ETAG),
            Some("\"asset-etag\""),
            "should preserve safe cache validator headers on asset responses"
        );
        assert_eq!(body, b"chunk", "should stream the origin body");
    }

    fn fingerprinted_asset_settings() -> Settings {
        Settings::from_toml(&format!(
            r#"{}

                [[cache.asset_rules]]
                id = "fingerprinted-assets"
                enabled = true
                path_globs = ["/assets/**/*.js"]
                fingerprint_style = "hex"
                visibility = "public"
                browser_ttl_seconds = 31536000
                edge_ttl_seconds = 31536000
                immutable = true
            "#,
            crate_test_settings_str()
        ))
        .expect("should parse settings with cache asset rule")
    }

    #[test]
    fn handle_asset_proxy_request_replaces_third_party_cache_policy_for_rehosted_asset() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://assets.example.com/assets/app.0123abcd.js",
            ScriptedReply::status(StatusCode::OK)
                .header(header::CACHE_CONTROL.as_str(), "no-store")
                .body("asset"),
        );
        let settings = fingerprinted_asset_settings();
        let req = build_http_request(
            Method::GET,
            "https://www.example.com/assets/app.0123abcd.js",
        );
        let route = ProxyAssetRoute::new("/assets/", "https://assets.example.com");

        let asset_response = futures::executor::block_on(handle_asset_proxy_request(
            &settings, &services, req, &route,
        ))
        .expect("should proxy asset request");
        assert_eq!(
            asset_response.cache_policy(),
            AssetProxyCachePolicy::Normalized(CachePolicy::public_immutable(Duration::from_secs(
                31_536_000
            ))),
            "should carry normalized cache policy metadata"
        );

        let (mut response, _body) = streamed_asset(asset_response);
        assert_eq!(
            response_header(&response, header::CACHE_CONTROL),
            Some("public, max-age=31536000, immutable"),
            "configured rehost policy should replace the third-party no-store directive"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "runtime-specific edge header should wait for adapter finalization"
        );

        AssetProxyCachePolicy::Normalized(CachePolicy::public_immutable(Duration::from_secs(
            31_536_000,
        )))
        .apply_after_route_finalization(&mut response, EdgeCacheHeader::SurrogateControl);
        assert_eq!(
            response
                .headers()
                .get("surrogate-control")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000"),
            "Fastly finalization should render Surrogate-Control"
        );
    }

    #[test]
    fn normalized_asset_policy_preserves_final_private_or_no_store_directives() {
        for cache_control in ["private, max-age=0", "no-store"] {
            let mut response = edge_response_builder()
                .header(header::CACHE_CONTROL, cache_control)
                .header("surrogate-control", "max-age=31536000")
                .header("cdn-cache-control", "max-age=31536000")
                .header("cloudflare-cdn-cache-control", "max-age=31536000")
                .body(EdgeBody::empty())
                .expect("should build asset response");

            AssetProxyCachePolicy::Normalized(CachePolicy::public_immutable(Duration::from_secs(
                31_536_000,
            )))
            .apply_after_route_finalization(&mut response, EdgeCacheHeader::SurrogateControl);

            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(cache_control),
                "final privacy directive should veto normalized cache policy"
            );
            assert!(
                [
                    "surrogate-control",
                    "cdn-cache-control",
                    "cloudflare-cdn-cache-control",
                ]
                .iter()
                .all(|name| !response.headers().contains_key(*name)),
                "final privacy directive should remove every edge-cache header"
            );
        }
    }

    #[test]
    fn handle_asset_proxy_request_leaves_non_matching_assets_origin_controlled() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://assets.example.com/assets/app.js",
            ScriptedReply::status(StatusCode::OK)
                .header(header::CACHE_CONTROL.as_str(), "public, max-age=60")
                .body("asset"),
        );
        let settings = fingerprinted_asset_settings();
        let req = build_http_request(Method::GET, "https://www.example.com/assets/app.js");
        let route = ProxyAssetRoute::new("/assets/", "https://assets.example.com");

        let asset_response = futures::executor::block_on(handle_asset_proxy_request(
            &settings, &services, req, &route,
        ))
        .expect("should proxy asset request");

        assert_eq!(
            asset_response.cache_policy(),
            AssetProxyCachePolicy::OriginControlled,
            "non-fingerprinted file should not receive normalized immutable policy"
        );
        let (response, _body) = streamed_asset(asset_response);
        assert_eq!(
            response_header(&response, header::CACHE_CONTROL),
            Some("public, max-age=60"),
            "origin-controlled response should preserve origin cache header"
        );
    }

    fn test_profile_set() -> ImageOptimizerProfileSet {
        let mut profiles = HashMap::new();
        profiles.insert("default".to_string(), "width=1920".to_string());
        profiles.insert("medium".to_string(), "format=auto&width=828".to_string());
        ImageOptimizerProfileSet {
            base_params: "quality=70&resize-filter=bicubic".to_string(),
            default_profile: "default".to_string(),
            unknown_profile: Default::default(),
            profile_param: "profile".to_string(),
            aspect_ratio_param: "ar".to_string(),
            debug_param: "_io_debug".to_string(),
            profiles,
            aspect_ratios: Some(ImageOptimizerAspectRatioConfig {
                allowed: vec!["1-1".to_string()],
                profiles: vec!["medium".to_string()],
            }),
            crop_offsets: Some(ImageOptimizerCropOffsetsConfig {
                enabled: true,
                x_param: "x".to_string(),
                y_param: "y".to_string(),
                buckets: vec![10, 30, 50, 70, 90],
                default: 50,
                when_missing: Default::default(),
            }),
        }
    }

    fn test_s3_image_optimizer_route() -> ProxyAssetRoute {
        let mut route = ProxyAssetRoute::new("/.images/", "https://examplebucket.s3.example.com");
        route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: "us-east-1".to_string(),
            secret_store: None,
            access_key_id: Redacted::new("AKIAIOSFODNN7EXAMPLE".to_string()),
            secret_access_key: Redacted::new(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            ),
            session_token: None,
            origin_query: None,
        }));
        route.image_optimizer = Some(AssetImageOptimizerConfig {
            enabled: true,
            region: "us_east".to_string(),
            profile_set: "default_images".to_string(),
            origin_query: None,
        });
        route
    }

    const S3_OBJECT: &str = "https://examplebucket.s3.example.com/.images/foo.jpg";

    fn image_optimizer_settings(profile_set: ImageOptimizerProfileSet) -> Settings {
        let mut settings = create_test_settings();
        settings.image_optimizer = ImageOptimizerSettings {
            profile_sets: HashMap::from([("default_images".to_string(), profile_set)]),
        };
        settings
    }

    fn image_optimizer_route() -> ProxyAssetRoute {
        let mut route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
        route.image_optimizer = Some(AssetImageOptimizerConfig {
            enabled: true,
            region: "us_east".to_string(),
            profile_set: "default_images".to_string(),
            origin_query: None,
        });
        route
    }

    /// The options [`test_profile_set`]'s `medium` profile maps to.
    fn medium_profile_options() -> OutboundImageOptimizerOptions {
        OutboundImageOptimizerOptions::new()
            .region(OutboundImageOptimizerRegion::UsEast)
            .quality(70)
            .expect("should accept quality 70")
            .resize_filter(OutboundResizeFilter::Bicubic)
            .format(OutboundImageFormat::Auto)
            .width(828)
            .expect("should accept width 828")
    }

    #[test]
    fn handle_asset_proxy_request_signs_s3_and_strips_transform_query() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            S3_OBJECT,
            ScriptedReply::status(StatusCode::OK).body("ok"),
        );
        let settings = create_test_settings();
        let req = build_http_request(
            Method::GET,
            "https://www.example.com/.images/foo.jpg?profile=medium&ar=1-1",
        );
        let mut route = ProxyAssetRoute::new("/.images/", "https://examplebucket.s3.example.com");
        route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: "us-east-1".to_string(),
            secret_store: None,
            access_key_id: Redacted::new("AKIAIOSFODNN7EXAMPLE".to_string()),
            secret_access_key: Redacted::new(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            ),
            session_token: None,
            origin_query: Some(OriginQueryPolicy::Strip),
        }));

        futures::executor::block_on(handle_asset_proxy_request(
            &settings, &services, req, &route,
        ))
        .expect("should proxy signed S3 asset request");

        assert_eq!(
            sent_uris(&client),
            vec![S3_OBJECT],
            "should strip transform query before signing and sending to S3"
        );
        let requests = client.requests();
        let sent = &requests[0];
        assert_eq!(
            sent.host_authority, "examplebucket.s3.example.com",
            "should send the S3 origin authority it signed as Host"
        );
        assert!(
            sent.headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value
                    .starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/")
                    && value.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date")),
            "should add a SigV4 Authorization header that signs the host"
        );
        assert_eq!(
            sent.headers
                .get("x-amz-content-sha256")
                .and_then(|value| value.to_str().ok()),
            Some("UNSIGNED-PAYLOAD"),
            "should use unsigned payload for read-only asset requests"
        );
        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn handle_asset_proxy_request_uses_resolved_s3_credentials_without_store_reads() {
        let (_clock, client, outbound_services) = scripted_services();
        client
            .respond_to(
                Method::HEAD,
                S3_OBJECT,
                ScriptedReply::status(StatusCode::OK),
            )
            .respond_to(
                Method::GET,
                S3_OBJECT,
                ScriptedReply::status(StatusCode::OK).body("optimized"),
            );
        let secret_store = CountingSecretStore::new(HashMap::new());
        let observed_secret_store = secret_store.clone();
        let services = RuntimeServices {
            secret_store: Arc::new(secret_store),
            ..outbound_services
        };
        let settings = image_optimizer_settings(test_profile_set());
        let req = build_http_request(
            Method::GET,
            "https://www.example.com/.images/foo.jpg?profile=medium",
        );
        let mut route = test_s3_image_optimizer_route();
        route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: "us-east-1".to_string(),
            secret_store: None,
            access_key_id: Redacted::new("AKIAIOSFODNN7EXAMPLE".to_string()),
            secret_access_key: Redacted::new(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            ),
            session_token: Some(Redacted::new("session-token".to_string())),
            origin_query: None,
        }));

        futures::executor::block_on(handle_asset_proxy_request(
            &settings, &services, req, &route,
        ))
        .expect("should proxy optimized S3 asset request");

        assert_eq!(
            sent_methods(&client),
            vec![Method::HEAD, Method::GET],
            "should sign both the S3 preflight and final request"
        );
        assert_eq!(
            observed_secret_store.read_count("AKIAIOSFODNN7EXAMPLE"),
            0,
            "should not read S3 credentials from the runtime secret store"
        );
        assert!(
            client.requests().iter().all(|sent| sent
                .headers
                .get("x-amz-security-token")
                .is_some_and(|value| value == "session-token")),
            "should sign both requests with the cached session token"
        );
    }

    #[test]
    fn asset_proxy_image_optimizer_options_read_back_on_final_get() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://assets.example.com/.images/foo.jpg",
            ScriptedReply::status(StatusCode::OK).body("ok"),
        );
        let settings = image_optimizer_settings(test_profile_set());
        let req = build_http_request(
            Method::GET,
            "https://www.example.com/.images/foo.jpg?profile=medium&ar=1-1&x=71&y=bad",
        );

        futures::executor::block_on(handle_asset_proxy_request(
            &settings,
            &services,
            req,
            &image_optimizer_route(),
        ))
        .expect("should proxy optimized asset request");

        assert_eq!(
            sent_uris(&client),
            vec!["https://assets.example.com/.images/foo.jpg"],
            "should strip profile-table query from the origin request by default"
        );
        let expected = medium_profile_options()
            .crop(1, 1, OutboundCropFocus::Percent { x: 70, y: 50 })
            .expect("should accept the bucketed crop");
        client.requests()[0].assert_image_optimizer_transport(asset_get_transport(), &expected);
    }

    #[test]
    fn asset_proxy_image_optimizer_options_read_back_on_final_head() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::HEAD,
            "https://assets.example.com/.images/foo.jpg",
            ScriptedReply::status(StatusCode::OK).header("content-type", "image/jpeg"),
        );
        let settings = image_optimizer_settings(test_profile_set());
        let req = build_http_request(
            Method::HEAD,
            "https://www.example.com/.images/foo.jpg?profile=medium",
        );

        let (response, stream_body) = futures::executor::block_on(handle_asset_proxy_request(
            &settings,
            &services,
            req,
            &image_optimizer_route(),
        ))
        .expect("should proxy optimized asset HEAD")
        .into_response_and_body();

        assert_eq!(response.status(), StatusCode::OK, "should relay the HEAD");
        assert!(stream_body.is_none(), "should buffer the bodiless HEAD");
        let requests = client.requests();
        assert_eq!(requests.len(), 1, "should send one HEAD");
        requests[0]
            .assert_image_optimizer_transport(asset_head_transport(), &medium_profile_options());
    }

    #[test]
    fn asset_proxy_s3_preflight_is_buffered_without_image_optimizer_options() {
        for (method, transport) in [
            (Method::GET, asset_get_transport()),
            (Method::HEAD, asset_head_transport()),
        ] {
            let (_clock, client, services) = scripted_services();
            client
                .respond_to(
                    Method::HEAD,
                    S3_OBJECT,
                    ScriptedReply::status(StatusCode::OK),
                )
                .respond_to(
                    method.clone(),
                    S3_OBJECT,
                    ScriptedReply::status(StatusCode::OK).body("optimized"),
                );
            let settings = image_optimizer_settings(test_profile_set());
            let req = build_http_request(
                method.clone(),
                "https://www.example.com/.images/foo.jpg?profile=medium",
            );

            futures::executor::block_on(handle_asset_proxy_request(
                &settings,
                &services,
                req,
                &test_s3_image_optimizer_route(),
            ))
            .expect("should proxy optimized S3 asset request");

            let requests = client.requests();
            assert_eq!(
                (requests.len(), &requests[0].method, &requests[1].method),
                (2, &Method::HEAD, &method),
                "should preflight S3 with HEAD before the optimized {method}"
            );
            assert_eq!(
                sent_uris(&client),
                vec![S3_OBJECT, S3_OBJECT],
                "should strip transform query from both S3 requests"
            );
            requests[0].assert_transport(uncached(asset_head_transport()));
            requests[1].assert_image_optimizer_transport(transport, &medium_profile_options());
        }
    }

    #[test]
    fn handle_asset_proxy_request_preflights_s3_before_image_optimizer() {
        let (_clock, client, services) = scripted_services();
        client
            .respond_to(
                Method::HEAD,
                S3_OBJECT,
                ScriptedReply::status(StatusCode::OK),
            )
            .respond_to(
                Method::GET,
                S3_OBJECT,
                ScriptedReply::status(StatusCode::OK).body("optimized"),
            );
        let settings = image_optimizer_settings(test_profile_set());
        let req = build_http_request(
            Method::GET,
            "https://www.example.com/.images/foo.jpg?profile=medium",
        );

        let (response, body) = streamed_asset(
            futures::executor::block_on(handle_asset_proxy_request(
                &settings,
                &services,
                req,
                &test_s3_image_optimizer_route(),
            ))
            .expect("should proxy optimized S3 asset request"),
        );

        assert_eq!(response.status(), StatusCode::OK, "should proxy the asset");
        assert_eq!(body, b"optimized", "should forward the complete asset body");
        assert_eq!(
            sent_methods(&client),
            vec![Method::HEAD, Method::GET],
            "should preflight S3 with HEAD before the optimized GET"
        );
        assert_eq!(
            client
                .requests()
                .iter()
                .map(|sent| sent.cache_policy.clone())
                .collect::<Vec<_>>(),
            vec![
                OutboundCachePolicy::Bypass,
                OutboundCachePolicy::PlatformDefault
            ],
            "signed HEAD preflight must bypass the cache without disabling Image Optimizer GET caching"
        );
    }

    #[test]
    fn handle_asset_proxy_request_returns_raw_s3_error_before_image_optimizer() {
        let (_clock, client, services) = scripted_services();
        client
            .respond_to(
                Method::HEAD,
                S3_OBJECT,
                ScriptedReply::status(StatusCode::NOT_FOUND),
            )
            .respond_to(
                Method::GET,
                S3_OBJECT,
                ScriptedReply::status(StatusCode::NOT_FOUND)
                    .header(header::CONTENT_TYPE.as_str(), "application/xml")
                    .header(header::CACHE_CONTROL.as_str(), "public, max-age=3600")
                    .header(header::SET_COOKIE.as_str(), "asset=1; Path=/")
                    .body(
                        r#"<Error><Code>NoSuchKey</Code><Key>image/upload/missing.jpg</Key></Error>"#,
                    ),
            );
        let settings = image_optimizer_settings(test_profile_set());
        let req = build_http_request(
            Method::GET,
            "https://www.example.com/.images/foo.jpg?profile=medium",
        );

        let asset_response = futures::executor::block_on(handle_asset_proxy_request(
            &settings,
            &services,
            req,
            &test_s3_image_optimizer_route(),
        ))
        .expect("should return raw S3 error response");
        assert_eq!(
            asset_response.cache_policy(),
            AssetProxyCachePolicy::NoStorePrivate,
            "should carry a typed no-store policy for router finalization"
        );
        let (response, body) = streamed_asset(asset_response);

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response_header(&response, header::CACHE_CONTROL),
            Some("no-store, private"),
            "S3 origin errors should not be cached"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "raw S3 error should still strip unsafe response headers"
        );
        let body = String::from_utf8(body).expect("should decode the S3 error body");
        assert!(body.contains("NoSuchKey"), "should return S3 error body");
        assert!(
            body.contains("image/upload/missing.jpg"),
            "should expose the missing S3 key"
        );
        let requests = client.requests();
        assert_eq!(
            sent_methods(&client),
            vec![Method::HEAD, Method::GET],
            "should fetch S3 error body after failed HEAD preflight"
        );
        requests[0].assert_transport(uncached(asset_head_transport()));
        requests[1].assert_transport(uncached(asset_get_transport()));
    }

    #[test]
    fn handle_asset_proxy_request_returns_s3_head_errors_without_cache_or_io() {
        for status in [StatusCode::FORBIDDEN, StatusCode::NOT_FOUND] {
            let (_clock, client, services) = scripted_services();
            client.respond_to(
                Method::HEAD,
                S3_OBJECT,
                ScriptedReply::status(status)
                    .header(header::CONTENT_TYPE.as_str(), "application/xml")
                    .header(header::CONTENT_LENGTH.as_str(), "42")
                    .header(header::CACHE_CONTROL.as_str(), "public, max-age=3600")
                    .header(header::SET_COOKIE.as_str(), "asset=1; Path=/"),
            );
            let settings = image_optimizer_settings(test_profile_set());
            let req = build_http_request(
                Method::HEAD,
                "https://www.example.com/.images/foo.jpg?profile=medium",
            );

            let asset_response = futures::executor::block_on(handle_asset_proxy_request(
                &settings,
                &services,
                req,
                &test_s3_image_optimizer_route(),
            ))
            .expect("should return the raw S3 preflight error");

            assert_eq!(
                asset_response.cache_policy(),
                AssetProxyCachePolicy::NoStorePrivate,
                "S3 HEAD {status} should carry a typed no-store policy"
            );
            let (response, stream_body) = asset_response.into_response_and_body();
            assert_eq!(
                response.status(),
                status,
                "S3 HEAD {status} should keep the origin status"
            );
            assert!(
                stream_body.is_none(),
                "S3 HEAD {status} should be answered without a body stream"
            );
            assert_eq!(
                response_header(&response, header::CACHE_CONTROL),
                Some("no-store, private"),
                "S3 HEAD {status} should not be cached"
            );
            assert_eq!(
                response_header(&response, header::CONTENT_LENGTH),
                Some("42"),
                "S3 HEAD {status} should keep the origin Content-Length"
            );
            assert!(
                response.headers().get(header::SET_COOKIE).is_none(),
                "S3 HEAD {status} should strip Set-Cookie"
            );
            assert!(
                response
                    .into_body()
                    .into_bytes()
                    .expect("should buffer the HEAD answer")
                    .is_empty(),
                "S3 HEAD {status} error response must not contain a body"
            );
            assert_eq!(
                sent_methods(&client),
                vec![Method::HEAD],
                "S3 HEAD {status} should be answered from its failed preflight alone"
            );
            client.requests()[0].assert_transport(uncached(asset_head_transport()));
        }
    }

    #[test]
    fn handle_asset_proxy_request_bypasses_cache_for_signed_raw_head() {
        let s3_logo = "https://examplebucket.s3.example.com/.images/logo.svg";
        for (case, uri, origin) in [
            (
                "svg",
                "https://www.example.com/.images/logo.svg?profile=medium",
                s3_logo,
            ),
            (
                "svgz",
                "https://www.example.com/.images/logo.svgz?profile=medium",
                "https://examplebucket.s3.example.com/.images/logo.svgz",
            ),
            (
                "rewritten_svg",
                "https://www.example.com/.images/logo.png?profile=medium",
                s3_logo,
            ),
            (
                "debug",
                "https://www.example.com/.images/foo.jpg?profile=medium&_io_debug=1",
                S3_OBJECT,
            ),
            (
                "disabled_io",
                "https://www.example.com/.images/foo.jpg?profile=medium",
                "https://examplebucket.s3.example.com/.images/foo.jpg?profile=medium",
            ),
            (
                "absent_io",
                "https://www.example.com/.images/foo.jpg",
                S3_OBJECT,
            ),
        ] {
            // An asset HEAD is always buffered, so the request must carry the same
            // settings whether the origin delivers its empty body as one chunk or
            // as a stream of chunks.
            for (delivery, body) in [
                ("buffered", vec![Bytes::new()]),
                ("streamed", vec![Bytes::new(), Bytes::new()]),
            ] {
                let (_clock, client, services) = scripted_services();
                client.respond_to(
                    Method::HEAD,
                    origin,
                    ScriptedReply::status(StatusCode::OK)
                        .header(header::CONTENT_LENGTH.as_str(), "42")
                        .header(header::CACHE_CONTROL.as_str(), "public, max-age=60")
                        .chunks(body),
                );
                let mut route = test_s3_image_optimizer_route();
                match case {
                    "rewritten_svg" => {
                        route.path_pattern = Some(r"^/\.images/(.*)\.png$".to_string());
                        route.target_path = Some("/.images/$1.svg".to_string());
                    }
                    "disabled_io" => {
                        route
                            .image_optimizer
                            .as_mut()
                            .expect("should configure Image Optimizer")
                            .enabled = false;
                    }
                    "absent_io" => route.image_optimizer = None,
                    _ => {}
                }

                let asset_response = futures::executor::block_on(handle_asset_proxy_request(
                    &image_optimizer_settings(test_profile_set()),
                    &services,
                    build_http_request(Method::HEAD, uri),
                    &route,
                ))
                .expect("should proxy the signed raw HEAD");

                assert_eq!(
                    sent_methods(&client),
                    vec![Method::HEAD],
                    "{case} should send one HEAD for a {delivery} origin reply"
                );
                let requests = client.requests();
                assert_eq!(
                    requests[0].cache_policy,
                    OutboundCachePolicy::Bypass,
                    "{case} signed raw HEAD must bypass the readthrough cache for a {delivery} origin reply"
                );
                requests[0].assert_transport(uncached(asset_head_transport()));
                assert_eq!(
                    asset_response.cache_policy(),
                    AssetProxyCachePolicy::OriginControlled,
                    "{case} HEAD should keep the origin cache policy for a {delivery} origin reply"
                );
                let (response, stream_body) = asset_response.into_response_and_body();
                assert!(
                    stream_body.is_none(),
                    "{case} HEAD should be answered without a body stream for a {delivery} origin reply"
                );
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{case} HEAD should keep the origin status"
                );
                assert_eq!(
                    response_header(&response, header::CONTENT_LENGTH),
                    Some("42"),
                    "{case} HEAD should keep the origin Content-Length"
                );
                assert_eq!(
                    response_header(&response, header::CACHE_CONTROL),
                    Some("public, max-age=60"),
                    "{case} HEAD should keep the origin cache headers"
                );
                assert!(
                    response
                        .into_body()
                        .into_bytes()
                        .expect("should buffer the HEAD answer")
                        .is_empty(),
                    "{case} HEAD should stay bodiless for a {delivery} origin reply"
                );
            }
        }
    }

    #[test]
    fn handle_asset_proxy_request_keeps_cache_for_raw_get_and_unsigned_head() {
        let origin = "https://examplebucket.s3.example.com/.images/logo.svg";
        for (method, signed, transport) in [
            (Method::GET, true, asset_get_transport()),
            (Method::GET, false, asset_get_transport()),
            (Method::HEAD, false, asset_head_transport()),
        ] {
            let (_clock, client, services) = scripted_services();
            client.respond_to(
                method.clone(),
                origin,
                ScriptedReply::status(StatusCode::OK),
            );
            let mut route = test_s3_image_optimizer_route();
            if !signed {
                route.auth = None;
            }

            futures::executor::block_on(handle_asset_proxy_request(
                &create_test_settings(),
                &services,
                build_http_request(
                    method.clone(),
                    "https://www.example.com/.images/logo.svg?profile=medium",
                ),
                &route,
            ))
            .expect("should proxy the raw asset");

            assert_eq!(
                sent_methods(&client),
                vec![method.clone()],
                "{method} with signed={signed} should not add a preflight"
            );
            let requests = client.requests();
            assert_eq!(
                requests[0].cache_policy,
                OutboundCachePolicy::PlatformDefault,
                "{method} with signed={signed} should keep readthrough caching"
            );
            requests[0].assert_transport(transport);
        }
    }

    #[test]
    fn asset_proxy_keeps_caching_for_raw_control_routes() {
        for case in ["svgz", "rewritten_svg", "debug", "disabled_io", "absent_io"] {
            for (method, signed) in [
                (Method::GET, true),
                (Method::HEAD, false),
                (Method::GET, false),
            ] {
                // Existing author tests cover signed and unsigned debug GET.
                if case == "debug" && method == Method::GET {
                    continue;
                }
                let (_clock, client, services) = scripted_services();
                let mut route = test_s3_image_optimizer_route();
                let (path, query, origin_path) = match case {
                    "svgz" => ("logo.svgz", "?profile=medium", "logo.svgz"),
                    "rewritten_svg" => {
                        route.path_pattern = Some("^/\\.images/(.*)\\.png$".to_owned());
                        route.target_path = Some("/.images/$1.svg".to_owned());
                        ("logo.png", "?profile=medium", "logo.svg")
                    }
                    "debug" => ("foo.jpg", "?profile=medium&_io_debug=1", "foo.jpg"),
                    "disabled_io" => {
                        route
                            .image_optimizer
                            .as_mut()
                            .expect("should configure IO")
                            .enabled = false;
                        ("foo.jpg", "?profile=medium", "foo.jpg")
                    }
                    _ => {
                        route.image_optimizer = None;
                        ("foo.jpg", "", "foo.jpg")
                    }
                };
                if !signed {
                    route.auth = None;
                }
                let origin_query = if route.origin_query_policy() == OriginQueryPolicy::Preserve {
                    query
                } else {
                    ""
                };
                client.respond_to(
                    method.clone(),
                    &format!(
                        "https://examplebucket.s3.example.com/.images/{origin_path}{origin_query}"
                    ),
                    ScriptedReply::status(StatusCode::OK),
                );
                let request = build_http_request(
                    method.clone(),
                    format!("https://www.example.com/.images/{path}{query}"),
                );
                let response = futures::executor::block_on(handle_asset_proxy_request(
                    &image_optimizer_settings(test_profile_set()),
                    &services,
                    request,
                    &route,
                ))
                .expect("should preserve the upstream raw asset behavior");
                assert_eq!(
                    response.cache_policy(),
                    AssetProxyCachePolicy::OriginControlled
                );
                let requests = client.requests();
                assert_eq!(requests.len(), 1, "should not preflight a raw {case} asset");
                assert_eq!(requests[0].method, method);
                assert!(
                    requests[0].image_optimizer.is_none(),
                    "should not attach IO for {case}"
                );
                assert_eq!(
                    requests[0].cache_policy,
                    OutboundCachePolicy::PlatformDefault,
                    "raw GET and unsigned HEAD should retain normal caching for {case}"
                );
            }
        }
    }

    /// Proxies `uri` through `route` and returns the one origin request it sent.
    fn single_asset_request(
        settings: &Settings,
        route: &ProxyAssetRoute,
        uri: &str,
        origin: &str,
    ) -> RecordedRequest {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            origin,
            ScriptedReply::status(StatusCode::OK).body("raw"),
        );
        let req = build_http_request(Method::GET, uri);

        let (response, body) = streamed_asset(
            futures::executor::block_on(handle_asset_proxy_request(
                settings, &services, req, route,
            ))
            .expect("should proxy the asset"),
        );

        assert_eq!(response.status(), StatusCode::OK, "should proxy the asset");
        assert_eq!(body, b"raw", "should forward the complete asset body");
        let requests = client.requests();
        assert_eq!(
            requests.len(),
            1,
            "should send one origin request for {uri}"
        );
        requests[0].clone()
    }

    fn rejecting_profile_set() -> ImageOptimizerProfileSet {
        let mut profile_set = test_profile_set();
        profile_set.unknown_profile = UnknownProfilePolicy::Reject;
        profile_set
    }

    #[test]
    fn handle_asset_proxy_request_skips_image_optimizer_for_svg_assets() {
        let sent = single_asset_request(
            &image_optimizer_settings(rejecting_profile_set()),
            &image_optimizer_route(),
            "https://www.example.com/.images/logo.SVG?profile=unknown&ar=1-1",
            "https://assets.example.com/.images/logo.SVG",
        );

        assert_eq!(
            sent.uri.to_string(),
            "https://assets.example.com/.images/logo.SVG",
            "should still strip profile-table query from SVG origin requests"
        );
        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn handle_asset_proxy_request_skips_image_optimizer_for_incoming_svg() {
        let mut route = ProxyAssetRoute::new("/.image/", "https://assets.example.com");
        route.path_pattern = Some(r"^/\.image/([^/]+)/[^/]+\.([^/.]+)$".to_string());
        route.target_path = Some("/image/upload/$1".to_string());
        route.image_optimizer = image_optimizer_route().image_optimizer;

        let sent = single_asset_request(
            &image_optimizer_settings(rejecting_profile_set()),
            &route,
            "https://www.example.com/.image/object-id/logo.svg?profile=unknown&ar=1-1",
            "https://assets.example.com/image/upload/object-id",
        );

        assert_eq!(
            sent.uri.to_string(),
            "https://assets.example.com/image/upload/object-id",
            "should strip profile-table query even when SVG target path omits extension"
        );
        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn handle_asset_proxy_request_skips_image_optimizer_for_rewritten_svg_assets() {
        let mut route = ProxyAssetRoute::new("/.image/", "https://assets.example.com");
        route.path_pattern = Some(r"^/\.image/(.*)/[^/]+\.([^/.]+)$".to_string());
        route.target_path = Some("/image/upload/$1.$2".to_string());
        route.image_optimizer = image_optimizer_route().image_optimizer;

        let sent = single_asset_request(
            &image_optimizer_settings(test_profile_set()),
            &route,
            "https://www.example.com/.image/options/id/logo.svg?profile=medium",
            "https://assets.example.com/image/upload/options/id.svg",
        );

        assert_eq!(
            sent.uri.to_string(),
            "https://assets.example.com/image/upload/options/id.svg",
            "should detect SVG after route path rewriting"
        );
        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn handle_asset_proxy_request_skips_s3_preflight_for_svg_image_optimizer_routes() {
        let sent = single_asset_request(
            &create_test_settings(),
            &test_s3_image_optimizer_route(),
            "https://www.example.com/.images/logo.svg?profile=medium",
            "https://examplebucket.s3.example.com/.images/logo.svg",
        );

        assert_eq!(
            (sent.method.clone(), sent.uri.to_string()),
            (
                Method::GET,
                "https://examplebucket.s3.example.com/.images/logo.svg".to_owned()
            ),
            "SVG IO bypass should send one GET without an S3 preflight or the transform query"
        );
        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn handle_asset_proxy_request_does_not_preflight_when_io_disabled() {
        let sent = single_asset_request(
            &image_optimizer_settings(test_profile_set()),
            &test_s3_image_optimizer_route(),
            "https://www.example.com/.images/foo.jpg?profile=medium&_io_debug=1",
            S3_OBJECT,
        );

        assert_eq!(
            sent.method,
            Method::GET,
            "disabled IO should not add S3 preflight"
        );
        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn handle_asset_proxy_request_debug_param_disables_image_optimizer() {
        let sent = single_asset_request(
            &image_optimizer_settings(test_profile_set()),
            &image_optimizer_route(),
            "https://www.example.com/.images/foo.jpg?profile=medium&_io_debug=1",
            "https://assets.example.com/.images/foo.jpg",
        );

        sent.assert_transport(asset_get_transport());
    }

    #[test]
    fn image_optimizer_builder_error_serves_unoptimized_with_warning() {
        // Settings validation keeps crop offsets within 0 to 100, so only a
        // profile set built past validation can make the options builder fail.
        let mut profile_set = test_profile_set();
        profile_set.crop_offsets = Some(ImageOptimizerCropOffsetsConfig {
            enabled: true,
            x_param: "x".to_string(),
            y_param: "y".to_string(),
            buckets: vec![150],
            default: 150,
            when_missing: Default::default(),
        });
        let capture = WarningCapture::start();

        let sent = single_asset_request(
            &image_optimizer_settings(profile_set),
            &image_optimizer_route(),
            "https://www.example.com/.images/foo.jpg?profile=medium&ar=1-1&x=10&y=10",
            "https://assets.example.com/.images/foo.jpg",
        );

        sent.assert_transport(asset_get_transport());
        let warnings = capture.take();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.message.contains("unoptimized")),
            "should warn that the asset is served unoptimized, got {:?}",
            warnings
                .iter()
                .map(|warning| warning.message.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn handle_asset_proxy_request_streams_a_chunked_origin_body() {
        let (_clock, client, services) = scripted_services();
        client.respond_to(
            Method::GET,
            "https://assets.example.com/.images/foo.jpg",
            ScriptedReply::status(StatusCode::OK)
                .header(header::ETAG.as_str(), "\"stream-etag\"")
                .chunks(vec![Bytes::from_static(b"ch"), Bytes::from_static(b"unk")]),
        );
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://www.example.com/.images/foo.jpg");
        let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");

        let (response, body) = streamed_asset(
            futures::executor::block_on(handle_asset_proxy_request(
                &settings, &services, req, &route,
            ))
            .expect("should proxy a streaming asset response"),
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_header(&response, header::ETAG),
            Some("\"stream-etag\""),
            "should keep the origin validators"
        );
        assert_eq!(body, b"chunk", "should stream every origin chunk");
    }

    // --- is_host_allowed ---

    #[test]
    fn exact_match() {
        assert!(
            is_host_allowed("example.com", "example.com"),
            "should match exact domain"
        );
    }

    #[test]
    fn exact_no_match() {
        assert!(
            !is_host_allowed("other.com", "example.com"),
            "should not match different domain"
        );
    }

    #[test]
    fn wildcard_subdomain() {
        assert!(
            is_host_allowed("ad.example.com", "*.example.com"),
            "should match direct subdomain"
        );
    }

    #[test]
    fn wildcard_deep_subdomain() {
        assert!(
            is_host_allowed("a.b.example.com", "*.example.com"),
            "should match deep subdomain"
        );
    }

    #[test]
    fn wildcard_apex_match() {
        assert!(
            is_host_allowed("example.com", "*.example.com"),
            "wildcard should also match apex domain"
        );
    }

    #[test]
    fn wildcard_no_boundary_bypass() {
        assert!(
            !is_host_allowed("evil-example.com", "*.example.com"),
            "should not match host that lacks dot boundary"
        );
    }

    #[test]
    fn case_insensitive_host() {
        assert!(
            is_host_allowed("AD.EXAMPLE.COM", "*.example.com"),
            "should match uppercase host"
        );
    }

    #[test]
    fn case_insensitive_pattern() {
        assert!(
            is_host_allowed("ad.example.com", "*.EXAMPLE.COM"),
            "should match uppercase pattern"
        );
    }

    // --- redirect allowlist enforcement (logic tests via is_host_allowed) ---

    #[test]
    fn redirect_allowed_exact() {
        let allowed = ["ad.example.com".to_string()];
        assert!(
            allowed.iter().any(|p| is_host_allowed("ad.example.com", p)),
            "should permit exact-match host"
        );
    }

    #[test]
    fn redirect_allowed_wildcard() {
        let allowed = ["*.example.com".to_string()];
        assert!(
            allowed
                .iter()
                .any(|p| is_host_allowed("sub.example.com", p)),
            "should permit wildcard-matched host"
        );
    }

    #[test]
    fn redirect_blocked() {
        let allowed = ["*.example.com".to_string()];
        assert!(
            !allowed.iter().any(|p| is_host_allowed("evil.com", p)),
            "should block host not in allowlist"
        );
    }

    #[test]
    fn empty_allowlist_permits_any_host() {
        let allowed: [String; 0] = [];
        assert!(
            is_host_permitted(&allowed, "evil.com"),
            "empty allowlist should not block any host"
        );
    }

    #[test]
    fn redirect_bypass_attempt() {
        let allowed = ["*.example.com".to_string()];
        assert!(
            !allowed
                .iter()
                .any(|p| is_host_allowed("evil-example.com", p)),
            "should block dot-boundary bypass attempt"
        );
    }

    // --- is_host_permitted (full guard: empty-list bypass + is_host_allowed) ---

    #[test]
    fn host_is_permitted_when_it_matches_allowlist() {
        let allowed = vec!["ad.example.com".to_string(), "cdn.example.com".to_string()];
        assert!(
            is_host_permitted(&allowed, "ad.example.com"),
            "should permit exact-match host"
        );
        assert!(
            is_host_permitted(&allowed, "cdn.example.com"),
            "should permit second allowed host"
        );
    }

    #[test]
    fn host_is_permitted_when_it_matches_wildcard() {
        let allowed = vec!["*.example.com".to_string()];
        assert!(
            is_host_permitted(&allowed, "sub.example.com"),
            "should permit wildcard-matched subdomain"
        );
    }

    #[test]
    fn host_is_blocked_when_not_in_allowlist() {
        let allowed = vec!["ad.example.com".to_string()];
        assert!(
            !is_host_permitted(&allowed, "evil.com"),
            "should block host not in allowlist"
        );
    }

    #[test]
    fn any_host_is_permitted_when_allowlist_is_empty() {
        let allowed: Vec<String> = vec![];
        assert!(
            is_host_permitted(&allowed, "any-host.com"),
            "should allow any host when allowlist is empty (open mode)"
        );
    }

    #[test]
    fn empty_host_is_blocked_when_allowlist_is_non_empty() {
        let allowed = vec!["example.com".to_string()];
        assert!(
            !is_host_permitted(&allowed, ""),
            "should block empty host when allowlist is non-empty"
        );
    }

    #[test]
    fn is_host_permitted_accepts_str_slices() {
        // Verifies the &[impl AsRef<str>] bound works with &str literals,
        // not just Vec<String>.
        let allowed: &[&str] = &["example.com", "*.cdn.example.com"];
        assert!(
            is_host_permitted(allowed, "example.com"),
            "should permit exact match via &str slice"
        );
        assert!(
            is_host_permitted(allowed, "static.cdn.example.com"),
            "should permit wildcard match via &str slice"
        );
        assert!(
            !is_host_permitted(allowed, "evil.com"),
            "should block host not in &str slice allowlist"
        );
    }

    #[test]
    fn ip_literal_blocked_by_domain_allowlist() {
        let allowed = vec!["*.example.com".to_string()];
        assert!(
            !is_host_permitted(&allowed, "169.254.169.254"),
            "should block cloud metadata IP"
        );
        assert!(
            !is_host_permitted(&allowed, "127.0.0.1"),
            "should block loopback IPv4"
        );
        assert!(
            !is_host_permitted(&allowed, "[::1]"),
            "should block loopback IPv6"
        );
        assert!(
            !is_host_permitted(&allowed, "::1"),
            "should block bare loopback IPv6"
        );
    }

    // --- initial target allowlist enforcement (integration-level) ---
    //
    // The unit tests above cover the host-matching logic itself. The tests
    // below verify that proxy_request threads config.allowed_domains through
    // the initial target check and redirect hops.

    #[test]
    fn proxy_request_blocks_non_https_target_when_https_only() {
        let upstream = ScriptedHttpClient::forbidding();
        let services = build_services_with_outbound_client(&upstream, &TestClock::new());
        let settings = create_test_settings();
        let req = build_http_request(
            Method::GET,
            "https://edge.example.com/integrations/prebid/bundle.js",
        );
        let config = ProxyRequestConfig::new("http://assets.example.com/prebid/trusted-prebid.js")
            .without_ec_id()
            .without_forward_headers()
            .with_streaming()
            .with_https_only();

        let err = futures::executor::block_on(proxy_request(&settings, req, config, &services))
            .expect_err("should block non-HTTPS target before proxying");

        assert_eq!(
            err.current_context().status_code(),
            StatusCode::FORBIDDEN,
            "HTTPS-only proxy requests should reject http targets"
        );
        assert!(
            matches!(err.current_context(), TrustedServerError::Forbidden { .. }),
            "should return a forbidden error"
        );
        upstream.assert_no_requests();
    }

    #[test]
    fn proxy_initial_target_blocked_by_allowlist() {
        futures::executor::block_on(async {
            use crate::http_util::compute_encrypted_sha256_token;

            let mut settings = create_test_settings();
            settings.proxy.allowed_domains = vec!["allowed.com".to_string()];

            let target = "https://blocked.com/pixel.gif";
            let token = compute_encrypted_sha256_token(&settings, target);
            let url = format!(
                "https://edge.example/first-party/proxy?tsurl={}&tstoken={}",
                urlencoding::encode(target),
                token,
            );
            let req = build_http_request(Method::GET, url);
            let services = crate::platform::test_support::noop_services();
            let err = handle_first_party_proxy(&settings, &services, req)
                .await
                .expect_err("should block initial target not in allowlist");
            assert_eq!(
                err.current_context().status_code(),
                StatusCode::FORBIDDEN,
                "should return 403 for allowlist violation"
            );
            assert!(
                matches!(
                    err.current_context(),
                    TrustedServerError::AllowlistViolation { .. }
                ),
                "should be AllowlistViolation error"
            );
        });
    }

    #[test]
    fn sign_rejects_oversized_body() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let oversized = vec![b'x'; 65537];
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://edge.example/first-party/sign")
                .header(header::CONTENT_TYPE, "application/json")
                .body(EdgeBody::from(oversized))
                .expect("should build request");
            let err = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect_err("should reject oversized body");
            assert_eq!(
                err.current_context().status_code(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "should return 413 for oversized sign body"
            );
        });
    }

    #[test]
    fn sign_reads_a_streamed_body() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let req = build_http_post_streaming_request(
                "https://edge.example/first-party/sign",
                br#"{"url":"https://cdn.example/asset.js"}"#,
            );
            let resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect("should sign the URL from a streamed body");
            assert_eq!(resp.status(), StatusCode::OK);
            let json = response_body_string(resp);
            assert!(
                json.contains("\"base\":\"https://cdn.example/asset.js\""),
                "should sign the streamed target: {json}"
            );
        });
    }

    #[test]
    fn sign_body_read_failures_map_to_statuses() {
        let settings = create_test_settings();
        for case in body_read_failure_cases(SIGN_MAX_BODY_BYTES) {
            let (req, expectation) = case.into_request(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("https://edge.example/first-party/sign")
                    .header(header::CONTENT_TYPE, "application/json"),
            );

            let outcome = futures::executor::block_on(handle_first_party_proxy_sign(
                &settings,
                &noop_services(),
                req,
            ));

            expectation.assert_outcome("first-party sign", &outcome);
        }
    }

    #[test]
    fn rebuild_body_read_failures_map_to_statuses() {
        let settings = create_test_settings();
        for case in body_read_failure_cases(REBUILD_MAX_BODY_BYTES) {
            let (req, expectation) = case.into_request(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("https://edge.example/first-party/proxy-rebuild")
                    .header(header::CONTENT_TYPE, "application/json"),
            );

            let outcome = futures::executor::block_on(handle_first_party_proxy_rebuild(
                &settings,
                &noop_services(),
                req,
            ));

            expectation.assert_outcome("first-party rebuild", &outcome);
        }
    }

    #[test]
    fn rebuild_rejects_oversized_body() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let oversized = vec![b'x'; 65537];
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://edge.example/first-party/proxy-rebuild")
                .header(header::CONTENT_TYPE, "application/json")
                .body(EdgeBody::from(oversized))
                .expect("should build request");
            let err = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
                .await
                .expect_err("should reject oversized body");
            assert_eq!(
                err.current_context().status_code(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "should return 413 for oversized rebuild body"
            );
        });
    }
}
