use crate::http_util::{compute_encrypted_sha256_token, ct_str_eq, enforce_max_body_size};
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{Uri as EdgeUri, request_builder as edge_request_builder};
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
    HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
};
use crate::creative::{CreativeCssProcessor, CreativeHtmlProcessor};
use crate::edge_cookie::get_ec_id;
use crate::error::TrustedServerError;
use crate::platform::{
    DEFAULT_FIRST_BYTE_TIMEOUT, PlatformBackendSpec, PlatformHttpRequest, PlatformResponse,
    RuntimeServices, StoreName,
};
use crate::redacted::Redacted;
use crate::s3_sigv4::{self, S3Credentials};
use crate::settings::{
    AssetOriginAuth, OriginQueryPolicy, ProxyAssetRoute, S3SigV4AuthConfig, Settings,
};
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};

/// Chunk size used for streaming content through the rewrite pipeline.
const STREAMING_CHUNK_SIZE: usize = 8192;

/// Fallback `Content-Type` for image-like responses missing an origin type.
const IMAGE_FALLBACK_CONTENT_TYPE: &str = "application/octet-stream";

const SIGN_MAX_BODY_BYTES: usize = 65536;
const REBUILD_MAX_BODY_BYTES: usize = 65536;

fn body_as_reader(body: EdgeBody) -> Cursor<bytes::Bytes> {
    Cursor::new(body.into_bytes().unwrap_or_default())
}

fn request_body_bytes(
    body: EdgeBody,
    endpoint: &str,
) -> Result<bytes::Bytes, Report<TrustedServerError>> {
    if body.is_stream() {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: format!("{endpoint} request body must be buffered, not streamed"),
        }));
    }

    Ok(body.into_bytes().unwrap_or_default())
}

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

const ASSET_PROXY_STRIP_RESPONSE_HEADERS: [&str; 3] =
    ["set-cookie", "strict-transport-security", "clear-site-data"];

/// Cache-control value used when asset proxy responses must not be stored.
pub const ASSET_NO_STORE_PRIVATE_CACHE_CONTROL: &str = "no-store, private";

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
}

impl AssetProxyCachePolicy {
    /// Apply protected cache headers after route-level response finalization.
    pub fn apply_after_route_finalization(self, response: &mut Response<EdgeBody>) {
        if self == Self::NoStorePrivate {
            apply_no_store_cache_control(response);
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

    /// Return cache policy metadata for router finalization.
    #[must_use]
    pub fn cache_policy(&self) -> AssetProxyCachePolicy {
        self.cache_policy
    }

    /// Consume this wrapper and return a buffered Fastly response.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Proxy`] when the response carries a
    /// streaming body. Use [`Self::into_response_and_body`] when the caller
    /// needs to handle both buffered and streaming asset responses.
    pub fn into_response(self) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        if self.stream_body.is_some() {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "streaming asset response cannot be converted into a buffered response"
                    .to_string(),
            }));
        }

        Ok(self.response)
    }

    /// Consume this wrapper and return the response headers plus optional stream body.
    #[must_use]
    pub fn into_response_and_body(self) -> (Response<EdgeBody>, Option<EdgeBody>) {
        (self.response, self.stream_body)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct S3CredentialsCacheKey {
    secret_store: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

static S3_CREDENTIALS_CACHE: LazyLock<Mutex<HashMap<S3CredentialsCacheKey, Arc<S3Credentials>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Convert a platform-neutral response into a buffered [`Response`] for downstream processing.
///
/// # Errors
///
/// Returns [`TrustedServerError::Proxy`] when `platform_resp` carries a
/// streaming body, which the buffered proxy path cannot materialize.
pub(crate) fn platform_response_to_fastly(
    platform_resp: PlatformResponse,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let (parts, body) = platform_resp.response.into_parts();
    let body_bytes = match body {
        EdgeBody::Once(bytes) => bytes.to_vec(),
        EdgeBody::Stream(_) => {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "streaming platform response body is not supported by Fastly response conversion"
                    .to_string(),
            }));
        }
    };
    Ok(Response::from_parts(parts, EdgeBody::from(body_bytes)))
}

fn platform_response_to_fastly_asset(platform_resp: PlatformResponse) -> AssetProxyResponse {
    let (parts, body) = platform_resp.response.into_parts();

    match body {
        EdgeBody::Once(bytes) => AssetProxyResponse::origin_controlled(Response::from_parts(
            parts,
            EdgeBody::from(bytes.to_vec()),
        )),
        stream_body @ EdgeBody::Stream(_) => {
            let resp = Response::from_parts(parts, EdgeBody::empty());
            AssetProxyResponse::origin_controlled_stream(resp, stream_body)
        }
    }
}

/// Stream an asset response body directly to a writable client stream.
///
/// # Errors
///
/// Returns an error if the platform stream yields an error or if writing a
/// chunk to the client stream fails.
pub async fn stream_asset_body<W: Write>(
    body: EdgeBody,
    output: &mut W,
) -> Result<(), Report<TrustedServerError>> {
    match body {
        EdgeBody::Once(bytes) => {
            output
                .write_all(bytes.as_ref())
                .change_context(TrustedServerError::Proxy {
                    message: "failed to write buffered asset response body".to_string(),
                })?;
        }
        EdgeBody::Stream(mut stream) => {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|err| {
                    Report::new(TrustedServerError::Proxy {
                        message: format!("streaming platform response body failed: {err}"),
                    })
                })?;
                output
                    .write_all(chunk.as_ref())
                    .change_context(TrustedServerError::Proxy {
                        message: "failed to write streaming asset response body".to_string(),
                    })?;
            }
        }
    }

    Ok(())
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
        .process(body_as_reader(body), &mut output)
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
/// a warning if size or path heuristics suggest a tracking pixel. Both call
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

/// Finalize a proxied response, choosing between streaming passthrough and full
/// content processing based on the `stream_passthrough` flag.
fn finalize_response(
    settings: &Settings,
    req: &Request<EdgeBody>,
    url: &str,
    beresp: Response<EdgeBody>,
    stream_passthrough: bool,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if stream_passthrough {
        Ok(finalize_proxied_response_streaming(req, url, beresp))
    } else {
        finalize_proxied_response(settings, req, url, beresp)
    }
}

/// Bundles per-request header configuration and [`RuntimeServices`] for the proxy redirect loop.
struct ProxyRequestHeaders<'a> {
    additional_headers: &'a [(header::HeaderName, HeaderValue)],
    copy_request_headers: bool,
    services: &'a RuntimeServices,
}

struct ProxyRedirectPolicy<'a> {
    follow_redirects: bool,
    stream_passthrough: bool,
    allowed_domains: &'a [String],
    require_https: bool,
}

/// Proxy a request to a clear target URL while reusing creative rewrite logic.
///
/// This forwards a curated header set, follows redirects when enabled, and can append
/// the caller's EC ID as a `ts-ec` query parameter to the target URL.
/// Optional bodies/headers can be supplied via [`ProxyRequestConfig`].
///
/// # Errors
///
/// - [`TrustedServerError::Proxy`] if the target URL is invalid, uses an unsupported
///   scheme, lacks a host, or the upstream fetch fails
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
        allowed_domains,
        require_https,
    } = config;

    let mut target_url_parsed = url::Url::parse(target_url).map_err(|_| {
        Report::new(TrustedServerError::Proxy {
            message: "invalid url".to_string(),
        })
    })?;

    if forward_ec_id {
        append_ec_id(&req, &mut target_url_parsed);
    }

    proxy_with_redirects(
        settings,
        &req,
        target_url_parsed,
        body.as_deref(),
        ProxyRequestHeaders {
            additional_headers: &headers,
            copy_request_headers,
            services,
        },
        ProxyRedirectPolicy {
            follow_redirects,
            stream_passthrough,
            allowed_domains,
            require_https,
        },
    )
    .await
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

fn s3_credentials_cache_key(config: &S3SigV4AuthConfig) -> S3CredentialsCacheKey {
    S3CredentialsCacheKey {
        secret_store: config.secret_store.clone(),
        access_key_id: config.access_key_id.clone(),
        secret_access_key: config.secret_access_key.clone(),
        session_token: config.session_token.clone(),
    }
}

fn load_s3_credentials(
    services: &RuntimeServices,
    config: &S3SigV4AuthConfig,
) -> Result<Arc<S3Credentials>, Report<TrustedServerError>> {
    let cache_key = s3_credentials_cache_key(config);
    if let Some(credentials) = S3_CREDENTIALS_CACHE
        .lock()
        .expect("should lock S3 credentials cache")
        .get(&cache_key)
        .cloned()
    {
        return Ok(credentials);
    }

    let store_name = StoreName::from(config.secret_store.as_str());
    let access_key_id = services
        .secret_store()
        .get_string(&store_name, &config.access_key_id)
        .change_context(TrustedServerError::Proxy {
            message: "failed to read S3 access key ID from secret store".to_string(),
        })?;
    let secret_access_key = services
        .secret_store()
        .get_string(&store_name, &config.secret_access_key)
        .change_context(TrustedServerError::Proxy {
            message: "failed to read S3 secret access key from secret store".to_string(),
        })?;
    let session_token = config
        .session_token
        .as_deref()
        .map(|key| {
            services
                .secret_store()
                .get_string(&store_name, key)
                .change_context(TrustedServerError::Proxy {
                    message: "failed to read S3 session token from secret store".to_string(),
                })
        })
        .transpose()?;
    let credentials = Arc::new(S3Credentials {
        access_key_id,
        secret_access_key: Redacted::new(secret_access_key),
        session_token: session_token.map(Redacted::new),
    });

    let mut cache = S3_CREDENTIALS_CACHE
        .lock()
        .expect("should lock S3 credentials cache");
    Ok(Arc::clone(cache.entry(cache_key).or_insert(credentials)))
}

#[cfg(test)]
fn clear_s3_credentials_cache_for_tests() {
    S3_CREDENTIALS_CACHE
        .lock()
        .expect("should lock S3 credentials cache")
        .clear();
}

fn apply_asset_origin_auth(
    services: &RuntimeServices,
    method: &Method,
    target_url: &url::Url,
    headers: &mut http::HeaderMap,
    auth: &AssetOriginAuth,
) -> Result<(), Report<TrustedServerError>> {
    match auth {
        AssetOriginAuth::S3SigV4(config) => {
            let credentials = load_s3_credentials(services, config)?;
            s3_sigv4::sign_headers(
                method,
                target_url,
                headers,
                &config.region,
                credentials.as_ref(),
                // s3_sigv4 converts this via chrono's `DateTime::<Utc>::from`, which
                // only accepts `std::time::SystemTime`. `std::time::SystemTime::now()`
                // panics on `wasm32-unknown-unknown` (Cloudflare Workers), so derive an
                // equivalent `std::time::SystemTime` from the wasm-safe `web_time` clock:
                // `UNIX_EPOCH + elapsed` is pure arithmetic and never calls the panicking
                // `now()`. On Fastly (wasm32-wasip1) and native, `web_time` delegates to
                // the std clock, so behavior is unchanged there.
                std::time::UNIX_EPOCH
                    + web_time::SystemTime::now()
                        .duration_since(web_time::UNIX_EPOCH)
                        .unwrap_or_default(),
            )
        }
    }
}

fn build_asset_platform_request(
    method: &Method,
    target_url: &url::Url,
    outbound_headers: &http::HeaderMap,
    backend_name: &str,
) -> Result<PlatformHttpRequest, Report<TrustedServerError>> {
    let mut builder = edge_request_builder().method(method.clone()).uri(
        target_url
            .as_str()
            .parse::<EdgeUri>()
            .change_context(TrustedServerError::Proxy {
                message: "invalid asset target URL".to_string(),
            })?,
    );

    for (name, value) in outbound_headers {
        builder = builder.header(name, value);
    }

    let edge_req =
        builder
            .body(EdgeBody::from(Vec::new()))
            .change_context(TrustedServerError::Proxy {
                message: "failed to build asset proxy request".to_string(),
            })?;

    Ok(PlatformHttpRequest::new(edge_req, backend_name))
}

async fn send_asset_origin_request(
    services: &RuntimeServices,
    backend_name: &str,
    method: &Method,
    target_url: &url::Url,
    outbound_headers: &http::HeaderMap,
    stream_response: bool,
) -> Result<AssetProxyResponse, Report<TrustedServerError>> {
    let mut platform_req =
        build_asset_platform_request(method, target_url, outbound_headers, backend_name)?;
    if stream_response {
        platform_req = platform_req.with_stream_response();
    }
    let platform_resp = services
        .http_client()
        .send(platform_req)
        .await
        .change_context(TrustedServerError::Proxy {
            message: "Failed to proxy asset request".to_string(),
        })?;

    if stream_response {
        Ok(platform_response_to_fastly_asset(platform_resp))
    } else {
        platform_response_to_fastly(platform_resp).map(AssetProxyResponse::origin_controlled)
    }
}

fn strip_asset_proxy_response_headers(response: &mut Response<EdgeBody>) {
    // Asset origins must not be able to mutate publisher-domain browser state
    // or security policy through this proxy path.
    for header_name in ASSET_PROXY_STRIP_RESPONSE_HEADERS {
        response.headers_mut().remove(header_name);
    }
}

fn apply_no_store_cache_control(response: &mut Response<EdgeBody>) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(ASSET_NO_STORE_PRIVATE_CACHE_CONTROL),
    );
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
    backend_name: &str,
) -> Result<Option<AssetProxyResponse>, Report<TrustedServerError>> {
    let Some(auth @ AssetOriginAuth::S3SigV4(_)) = route.auth.as_ref() else {
        return Ok(None);
    };

    // Fastly Image Optimizer can mask or transform S3 origin errors. A signed
    // HEAD preflight lets missing or unauthorized objects return raw S3 errors
    // without invoking IO on the failure path.
    let mut head_headers = unsigned_headers.clone();
    apply_asset_origin_auth(services, &Method::HEAD, target_url, &mut head_headers, auth)?;
    let head_response = send_asset_origin_request(
        services,
        backend_name,
        &Method::HEAD,
        target_url,
        &head_headers,
        false,
    )
    .await?;
    let head_status = head_response.response().status();
    if head_status.is_success() || head_status == StatusCode::NOT_MODIFIED {
        return Ok(None);
    }

    if request_method == Method::HEAD {
        let mut response = head_response.into_response()?;
        strip_asset_proxy_response_headers(&mut response);
        apply_no_store_cache_control(&mut response);
        return Ok(Some(AssetProxyResponse::no_store_private(response)));
    }

    let mut get_headers = unsigned_headers.clone();
    apply_asset_origin_auth(services, &Method::GET, target_url, &mut get_headers, auth)?;
    let mut response = send_asset_origin_request(
        services,
        backend_name,
        &Method::GET,
        target_url,
        &get_headers,
        true,
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
/// is path rewrite, profile-table Image Optimizer metadata extraction, optional
/// origin query stripping, optional origin authentication, then platform send.
///
/// The origin query policy is applied before S3 signing so the signature covers
/// the exact URL sent to the asset origin. Image Optimizer metadata remains
/// separate from the origin URL and is translated by the platform adapter.
///
/// # Errors
///
/// Returns an error if the configured origin URL is invalid, backend
/// registration fails, S3 credentials cannot be read, signing fails, image
/// optimizer metadata cannot be built, or the upstream request cannot be sent.
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
        crate::asset_image_optimizer::options_for_asset_request(settings, route, incoming_query)?
    };

    if route.origin_query_policy() == OriginQueryPolicy::Strip {
        target_url.set_query(None);
    }

    let scheme = target_url.scheme();
    let host = target_url.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "Missing host in asset target URL".to_string(),
        })
    })?;

    let backend_name = services
        .backend()
        .ensure(&PlatformBackendSpec {
            scheme: scheme.to_string(),
            host: host.to_string(),
            port: target_url.port(),
            host_header_override: None,
            certificate_check: settings.proxy.certificate_check,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            between_bytes_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
        })
        .change_context(TrustedServerError::Proxy {
            message: "asset backend registration failed".to_string(),
        })?;

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
            &backend_name,
        )
        .await?
    {
        return Ok(response);
    }

    if let Some(auth) = &route.auth {
        apply_asset_origin_auth(
            services,
            req.method(),
            &target_url,
            &mut outbound_headers,
            auth,
        )?;
    }

    let mut platform_req =
        build_asset_platform_request(req.method(), &target_url, &outbound_headers, &backend_name)?;
    if let Some(image_optimizer) = image_optimizer {
        platform_req = platform_req.with_image_optimizer(image_optimizer);
    }
    platform_req = platform_req.with_stream_response();

    let platform_resp = services
        .http_client()
        .send(platform_req)
        .await
        .change_context(TrustedServerError::Proxy {
            message: "Failed to proxy asset request".to_string(),
        })?;

    let mut response = platform_response_to_fastly_asset(platform_resp);
    strip_asset_proxy_response_headers(response.response_mut());

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

/// Returns `true` when a redirect to `host` should be followed.
///
/// When `allowed_domains` is empty every host is permitted (open mode).
/// When non-empty the host must match at least one pattern via [`is_host_allowed`].
fn redirect_is_permitted<S: AsRef<str>>(allowed_domains: &[S], host: &str) -> bool {
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

async fn proxy_with_redirects(
    settings: &Settings,
    req: &Request<EdgeBody>,
    target_url_parsed: url::Url,
    body: Option<&[u8]>,
    request_headers: ProxyRequestHeaders<'_>,
    redirect_policy: ProxyRedirectPolicy<'_>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    const MAX_REDIRECTS: usize = 4;

    let mut current_url = target_url_parsed.to_string();
    let mut current_method: Method = req.method().clone();

    for redirect_attempt in 0..=MAX_REDIRECTS {
        let parsed_url = url::Url::parse(&current_url).map_err(|_| {
            Report::new(TrustedServerError::Proxy {
                message: "invalid url".to_string(),
            })
        })?;

        let scheme = parsed_url.scheme().to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "unsupported scheme".to_string(),
            }));
        }
        if redirect_policy.require_https && scheme != "https" {
            log::warn!("request to `{}` blocked: HTTPS is required", current_url);
            return Err(Report::new(TrustedServerError::Forbidden {
                message: "non-HTTPS proxy target blocked".to_string(),
            }));
        }

        let host = parsed_url.host_str().unwrap_or("");
        if host.is_empty() {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "missing host".to_string(),
            }));
        }

        if !redirect_is_permitted(redirect_policy.allowed_domains, host) {
            log::warn!(
                "request to `{}` blocked: host not in proxy allowed_domains",
                host
            );
            return Err(Report::new(TrustedServerError::AllowlistViolation {
                host: host.to_string(),
            }));
        }

        let backend_name = request_headers
            .services
            .backend()
            .ensure(&PlatformBackendSpec {
                scheme: scheme.clone(),
                host: host.to_string(),
                port: parsed_url.port(),
                host_header_override: None,
                certificate_check: settings.proxy.certificate_check,
                first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
                between_bytes_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            })
            .change_context(TrustedServerError::Proxy {
                message: "backend registration failed".to_string(),
            })?;

        let mut builder = edge_request_builder().method(current_method.clone()).uri(
            current_url
                .parse::<EdgeUri>()
                .change_context(TrustedServerError::Proxy {
                    message: "invalid url".to_string(),
                })?,
        );

        // Collect outbound headers using insert-semantics so additional_headers override any
        // header set by copy_request_headers, matching the old set_header() replace behavior.
        let mut outbound_headers = http::HeaderMap::new();
        if request_headers.copy_request_headers {
            for header_name in PROXY_FORWARD_HEADERS {
                if let Some(v) = req.headers().get(&header_name) {
                    outbound_headers.insert(header_name, v.clone());
                }
            }
            outbound_headers.insert(
                HEADER_ACCEPT_ENCODING,
                HeaderValue::from_static(SUPPORTED_ENCODINGS),
            );
        }
        for (name, value) in request_headers.additional_headers {
            // insert() replaces any existing value, matching set_header() semantics.
            outbound_headers.insert(name.clone(), value.clone());
        }
        for (name, value) in &outbound_headers {
            builder = builder.header(name, value);
        }
        let body_bytes = body.map(<[u8]>::to_vec).unwrap_or_default();
        let edge_req =
            builder
                .body(EdgeBody::from(body_bytes))
                .change_context(TrustedServerError::Proxy {
                    message: "failed to build proxy request".to_string(),
                })?;

        let platform_resp = request_headers
            .services
            .http_client()
            .send(PlatformHttpRequest::new(edge_req, backend_name))
            .await
            .change_context(TrustedServerError::Proxy {
                message: "Failed to proxy".to_string(),
            })?;

        let beresp = platform_resp.response;

        if !redirect_policy.follow_redirects {
            return finalize_response(
                settings,
                req,
                &current_url,
                beresp,
                redirect_policy.stream_passthrough,
            );
        }

        let status = beresp.status();
        let is_redirect = matches!(
            status,
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        );

        if !is_redirect {
            return finalize_response(
                settings,
                req,
                &current_url,
                beresp,
                redirect_policy.stream_passthrough,
            );
        }

        let Some(location) = beresp
            .headers()
            .get(header::LOCATION)
            .and_then(|h| h.to_str().ok())
            .filter(|value| !value.is_empty())
        else {
            return finalize_response(
                settings,
                req,
                &current_url,
                beresp,
                redirect_policy.stream_passthrough,
            );
        };

        if redirect_attempt == MAX_REDIRECTS {
            log::warn!(
                "redirect limit reached for {}; returning redirect response",
                current_url
            );
            return finalize_proxied_response(settings, req, &current_url, beresp);
        }

        let next_url = url::Url::parse(location)
            .or_else(|_| parsed_url.join(location))
            .map_err(|_| {
                Report::new(TrustedServerError::Proxy {
                    message: "invalid redirect".to_string(),
                })
            })?;

        let next_scheme = next_url.scheme().to_ascii_lowercase();
        if redirect_policy.require_https && next_scheme != "https" {
            log::warn!("redirect to `{}` blocked: HTTPS is required", next_url);
            return Err(Report::new(TrustedServerError::Forbidden {
                message: "non-HTTPS redirect blocked".to_string(),
            }));
        }
        if next_scheme != "http" && next_scheme != "https" {
            return finalize_response(
                settings,
                req,
                &current_url,
                beresp,
                redirect_policy.stream_passthrough,
            );
        }

        let next_host = match next_url.host_str() {
            Some(h) if !h.is_empty() => h,
            _ => {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: "missing host in redirect location".to_string(),
                }));
            }
        };
        if !redirect_is_permitted(redirect_policy.allowed_domains, next_host) {
            log::warn!(
                "redirect to `{}` blocked: host not in proxy allowed_domains",
                next_host
            );
            return Err(Report::new(TrustedServerError::AllowlistViolation {
                host: next_host.to_string(),
            }));
        }

        log::info!(
            "following redirect {} => {} (status {})",
            current_url,
            next_url,
            status.as_u16()
        );

        current_url = next_url.to_string();
        if status == StatusCode::SEE_OTHER {
            current_method = Method::GET;
        }
    }

    Err(Report::new(TrustedServerError::Proxy {
        message: "redirect handling failed".to_string(),
    }))
}

/// Unified proxy endpoint for resources referenced by ad creatives.
///
/// Accepts:
/// - `u`: Base64 URL-safe (no padding) encoded URL of the third-party resource.
///
/// Behavior:
/// - Proxies the decoded URL via a dynamic backend derived from scheme/host/port.
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
/// Returns an error if JSON parsing fails, the URL cannot be parsed, or the URL uses an unsupported scheme.
pub async fn handle_first_party_proxy_sign(
    settings: &Settings,
    _services: &RuntimeServices,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let method = req.method().clone();
    let req_url = req.uri().to_string();

    let payload = if method == Method::POST {
        let body_bytes = request_body_bytes(req.into_body(), "first-party sign")?;
        enforce_max_body_size(&body_bytes, SIGN_MAX_BODY_BYTES, "first-party sign")?;
        let body =
            std::str::from_utf8(&body_bytes).change_context(TrustedServerError::InvalidUtf8 {
                message: "first-party sign request body should be valid UTF-8".to_string(),
            })?;
        serde_json::from_str::<ProxySignReq>(body).change_context(TrustedServerError::Proxy {
            message: "invalid JSON".to_string(),
        })?
    } else {
        let parsed = url::Url::parse(&req_url).change_context(TrustedServerError::Proxy {
            message: "Invalid URL".to_string(),
        })?;
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
    let abs = if trimmed.starts_with("//") {
        let default_scheme = url::Url::parse(&req_url)
            .ok()
            .map(|u| u.scheme().to_ascii_lowercase())
            .filter(|scheme| !scheme.is_empty())
            .unwrap_or_else(|| "https".to_string());
        format!("{}:{}", default_scheme, trimmed)
    } else {
        crate::creative::to_abs(settings, trimmed).ok_or_else(|| {
            Report::new(TrustedServerError::Proxy {
                message: "unsupported url".to_string(),
            })
        })?
    };

    let parsed = url::Url::parse(&abs).change_context(TrustedServerError::Proxy {
        message: "invalid url".to_string(),
    })?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "unsupported scheme".to_string(),
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

    let mut base = parsed.clone();
    base.set_query(None);
    base.set_fragment(None);
    let proxied = crate::creative::build_proxy_url_with_extras(settings, &abs, &extras);

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

#[derive(Serialize)]
struct ProxyRebuildResp {
    href: String,
    base: String,
    added: std::collections::BTreeMap<String, String>,
    removed: Vec<String>,
}

/// Proxy rebuild endpoint.
/// POST /first-party/proxy-rebuild
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
    let req_url = req.uri().to_string();
    let payload = if method == Method::POST {
        let body_bytes = request_body_bytes(req.into_body(), "first-party rebuild")?;
        enforce_max_body_size(&body_bytes, REBUILD_MAX_BODY_BYTES, "first-party rebuild")?;
        let body =
            std::str::from_utf8(&body_bytes).change_context(TrustedServerError::InvalidUtf8 {
                message: "first-party rebuild request body should be valid UTF-8".to_string(),
            })?;
        serde_json::from_str::<ProxyRebuildReq>(body).change_context(TrustedServerError::Proxy {
            message: "invalid JSON".to_string(),
        })?
    } else {
        // Support GET: /first-party/proxy-rebuild?tsclick=...&add=...&del=...
        let parsed = url::Url::parse(&req_url).change_context(TrustedServerError::Proxy {
            message: "Invalid URL".to_string(),
        })?;
        let mut tsclick: Option<String> = None;
        let mut add: Option<std::collections::HashMap<String, String>> = None;
        let mut del: Option<Vec<String>> = None;
        for (k, v) in parsed.query_pairs() {
            match k.as_ref() {
                "tsclick" => tsclick = Some(v.into_owned()),
                "add" => {
                    if let Ok(m) =
                        serde_json::from_str::<std::collections::HashMap<String, String>>(&v)
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
        ProxyRebuildReq {
            tsclick: tsclick.ok_or_else(|| {
                Report::new(TrustedServerError::Proxy {
                    message: "missing tsclick".to_string(),
                })
            })?,
            add,
            del,
        }
    };

    let base = "https://edge.local"; // dummy origin to parse relative path
    let c_url = url::Url::parse(&format!("{}{}", base, payload.tsclick)).change_context(
        TrustedServerError::Proxy {
            message: "invalid tsclick".to_string(),
        },
    )?;
    if c_url.path() != "/first-party/click" {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "invalid tsclick path".to_string(),
        }));
    }
    // Validate the tstoken on the original click URL before applying any changes.
    // Without this, an attacker could submit an unsigned tsclick and mint valid
    // click redirects to arbitrary URLs.
    reconstruct_and_validate_signed_target(settings, &format!("{}{}", base, payload.tsclick))?;

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
    let href = format!("/first-party/click?{}", qs.finish());

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

    if method == Method::GET {
        // Redirect for GET usage to streamline navigation
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
    let parsed = url::Url::parse(req_url).change_context(TrustedServerError::Proxy {
        message: "Invalid URL".to_string(),
    })?;

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
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::{
        AssetProxyCachePolicy, IMAGE_FALLBACK_CONTENT_TYPE, ProxyRequestConfig,
        SUPPORTED_ENCODINGS, asset_origin_host_header, asset_path_skips_image_optimizer,
        build_asset_proxy_target_url, clear_s3_credentials_cache_for_tests,
        handle_asset_proxy_request, handle_first_party_click, handle_first_party_proxy,
        handle_first_party_proxy_rebuild, handle_first_party_proxy_sign, is_host_allowed,
        proxy_request, rebuild_response_with_body, reconstruct_and_validate_signed_target,
        redirect_is_permitted, stream_asset_body,
    };
    use crate::constants::{HEADER_ACCEPT, HEADER_X_FORWARDED_FOR};
    use crate::creative;
    use crate::error::{IntoHttpResponse, TrustedServerError};
    use crate::platform::test_support::{
        HashMapSecretStore, StubHttpClient, build_services_with_http_client,
        build_services_with_secret_and_http_client, noop_services,
    };
    use crate::platform::{
        PlatformError, PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest,
        PlatformResponse, PlatformSecretStore, PlatformSelectResult, StoreId, StoreName,
    };
    use crate::settings::{
        AssetImageOptimizerConfig, AssetOriginAuth, ImageOptimizerAspectRatioConfig,
        ImageOptimizerCropOffsetsConfig, ImageOptimizerProfileSet, ImageOptimizerSettings,
        OriginQueryPolicy, ProxyAssetRoute, S3SigV4AuthConfig, UnknownProfilePolicy,
    };
    use crate::test_support::tests::create_test_settings;
    use bytes::Bytes;
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::http::response_builder as edge_response_builder;
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

    fn build_http_post_streaming_request(uri: impl AsRef<str>) -> HttpRequest<EdgeBody> {
        let stream = futures::stream::iter(vec![Bytes::from_static(b"{}")]);
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

    struct QueuedHttpResponse {
        status: u16,
        headers: Vec<(header::HeaderName, HeaderValue)>,
        body: Vec<u8>,
    }

    #[derive(Default)]
    struct HeaderAwareStubHttpClient {
        responses: Mutex<VecDeque<QueuedHttpResponse>>,
    }

    impl HeaderAwareStubHttpClient {
        fn new() -> Self {
            Self::default()
        }

        fn push_response(
            &self,
            status: u16,
            headers: Vec<(header::HeaderName, HeaderValue)>,
            body: Vec<u8>,
        ) {
            self.responses
                .lock()
                .expect("should lock queued responses")
                .push_back(QueuedHttpResponse {
                    status,
                    headers,
                    body,
                });
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for HeaderAwareStubHttpClient {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            let queued = self
                .responses
                .lock()
                .expect("should lock queued responses")
                .pop_front()
                .ok_or_else(|| Report::new(PlatformError::HttpClient))?;

            let mut builder = edgezero_core::http::response_builder().status(queued.status);
            for (name, value) in queued.headers {
                builder = builder.header(name, value);
            }

            let response = builder
                .body(EdgeBody::from(queued.body))
                .expect("should build stub HTTP response");

            Ok(PlatformResponse::new(response))
        }

        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
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

    /// Test double that always returns a streaming (non-buffered) response body.
    ///
    /// Used to exercise the `Body::Stream` pass-through path in
    /// `proxy_request`, which returns the response body without materialising it.
    /// Only `send` is implemented; `send_async` and
    /// `select` return `PlatformError::Unsupported`.
    struct StreamingResponseHttpClient;

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for StreamingResponseHttpClient {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            let edge_response = edge_response_builder()
                .status(StatusCode::OK)
                .header(header::SET_COOKIE.as_str(), "asset=1; Path=/; Secure")
                .header(header::SET_COOKIE.as_str(), "other=2; Path=/; Secure")
                .header(
                    header::STRICT_TRANSPORT_SECURITY.as_str(),
                    "max-age=31536000; includeSubDomains; preload",
                )
                .header("clear-site-data", "\"*\"")
                .header(header::ETAG.as_str(), "\"stream-etag\"")
                .body(EdgeBody::stream(futures::stream::iter(vec![
                    Bytes::from_static(b"chunk"),
                ])))
                .expect("should build streaming test response");

            Ok(PlatformResponse::new(edge_response).with_backend_name("stub-backend"))
        }

        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
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

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
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
            let settings = create_test_settings();
            let body = serde_json::json!({
                "url": "https://cdn.example/asset.js?c=3&b=2",
            });
            let req = build_http_post_json_request("https://edge.example/first-party/sign", &body);
            let resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect("sign ok");
            assert_eq!(resp.status(), StatusCode::OK);
            let json = response_body_string(resp);
            assert!(json.contains("/first-party/proxy?tsurl="), "{}", json);
            assert!(json.contains("tsexp"), "{}", json);
            assert!(
                json.contains("\"base\":\"https://cdn.example/asset.js\""),
                "{}",
                json
            );
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
            .with_streaming();

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
        // Simulate creative-generated first-party URL
        let first_party = creative::build_proxy_url(&settings, clear, "");
        // Reconstruct and validate (need absolute URL for parsing)
        let st = reconstruct_and_validate_signed_target(
            &settings,
            &format!("https://edge.example{}", first_party),
        )
        .expect("reconstruct ok");
        assert_eq!(st.tsurl, "https://cdn.example/asset.js");
        assert!(st.had_params);
        assert_eq!(st.target_url, canonical_clear_url(clear));
    }

    #[test]
    fn reconstruct_valid_without_params() {
        let settings = create_test_settings();
        let clear = "https://cdn.example/asset.js";
        let first_party = creative::build_proxy_url(&settings, clear, "");
        let st = reconstruct_and_validate_signed_target(
            &settings,
            &format!("https://edge.example{}", first_party),
        )
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
            // Build a first-party proxy URL with a token for the unsupported scheme
            let first_party = creative::build_proxy_url(&settings, clear, "");
            let req =
                build_http_request(Method::GET, format!("https://edge.example{}", first_party));
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
            let first_party = creative::build_click_url(&settings, clear, "");
            let req =
                build_http_request(Method::GET, format!("https://edge.example{}", first_party));
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

    // --- Platform HTTP client integration ---

    #[test]
    fn proxy_request_calls_platform_http_client_send() {
        futures::executor::block_on(async {
            use crate::platform::test_support::StubHttpClient;

            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let settings = create_test_settings();
            let req = build_http_request(Method::GET, "https://example.com/");

            let result = proxy_request(
                &settings,
                req,
                ProxyRequestConfig {
                    target_url: "https://example.com/resource",
                    follow_redirects: false,
                    forward_ec_id: false,
                    body: None,
                    headers: Vec::new(),
                    copy_request_headers: false,
                    stream_passthrough: false,
                    allowed_domains: &[],
                    require_https: false,
                },
                &services,
            )
            .await;

            assert!(result.is_ok(), "should proxy successfully");
            let calls = stub.recorded_backend_names();
            assert_eq!(calls.len(), 1, "should call send exactly once");
            assert_eq!(
                calls[0], "stub-backend",
                "should use backend name from StubBackend"
            );
        });
    }

    #[test]
    fn proxy_request_allows_open_mode_when_settings_allowlist_is_non_empty() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.proxy.allowed_domains = vec!["allowed.example".to_string()];

            let stub = Arc::new(HeaderAwareStubHttpClient::new());
            stub.push_response(200, Vec::new(), b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let req = build_http_request(Method::GET, "https://edge.example/");

            let response = proxy_request(
                &settings,
                req,
                ProxyRequestConfig {
                    target_url: "https://blocked.example/resource.js",
                    follow_redirects: false,
                    forward_ec_id: false,
                    body: None,
                    headers: Vec::new(),
                    copy_request_headers: false,
                    stream_passthrough: false,
                    allowed_domains: &[],
                    require_https: false,
                },
                &services,
            )
            .await
            .expect("open mode should ignore settings.proxy.allowed_domains");

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response_body_string(response), "ok");
        });
    }

    #[test]
    fn proxy_request_uses_config_allowlist_for_redirect_hops() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.proxy.allowed_domains = vec!["origin.example".to_string()];

            let stub = Arc::new(HeaderAwareStubHttpClient::new());
            stub.push_response(
                302,
                vec![(
                    header::LOCATION,
                    HeaderValue::from_static("https://redirected.example/final.js"),
                )],
                Vec::new(),
            );
            stub.push_response(200, Vec::new(), b"redirected".to_vec());

            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let req = build_http_request(Method::GET, "https://edge.example/");

            let response = proxy_request(
                &settings,
                req,
                ProxyRequestConfig {
                    target_url: "https://origin.example/start.js",
                    follow_redirects: true,
                    forward_ec_id: false,
                    body: None,
                    headers: Vec::new(),
                    copy_request_headers: false,
                    stream_passthrough: false,
                    allowed_domains: &[],
                    require_https: false,
                },
                &services,
            )
            .await
            .expect("open mode should allow redirect hops outside settings allowlist");

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response_body_string(response), "redirected");
        });
    }

    #[test]
    fn proxy_request_forwards_curated_headers_when_copy_request_headers_is_true() {
        futures::executor::block_on(async {
            use crate::platform::test_support::StubHttpClient;

            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let settings = create_test_settings();
            let mut req = HttpRequest::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(EdgeBody::empty())
                .expect("should build test request");
            req.headers_mut().insert(
                header::USER_AGENT,
                HeaderValue::from_static("test-agent/1.0"),
            );
            req.headers_mut()
                .insert(header::ACCEPT, HeaderValue::from_static("text/html"));
            req.headers_mut()
                .insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));

            let result = proxy_request(
                &settings,
                req,
                ProxyRequestConfig {
                    target_url: "https://example.com/resource",
                    follow_redirects: false,
                    forward_ec_id: false,
                    body: None,
                    headers: Vec::new(),
                    copy_request_headers: true,
                    stream_passthrough: false,
                    allowed_domains: &[],
                    require_https: false,
                },
                &services,
            )
            .await;

            assert!(result.is_ok(), "should proxy successfully");
            let all_headers = stub.recorded_request_headers();
            assert_eq!(all_headers.len(), 1, "should have captured one request");
            let sent = &all_headers[0];

            let header_value = |name: &str| -> Option<String> {
                sent.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
            };

            assert_eq!(
                header_value("user-agent").as_deref(),
                Some("test-agent/1.0"),
                "should forward User-Agent"
            );
            assert_eq!(
                header_value("accept").as_deref(),
                Some("text/html"),
                "should forward Accept"
            );
            assert_eq!(
                header_value("accept-language").as_deref(),
                Some("en-US"),
                "should forward Accept-Language"
            );
            assert_eq!(
                header_value("accept-encoding").as_deref(),
                Some(SUPPORTED_ENCODINGS),
                "should override Accept-Encoding with supported encodings"
            );
        });
    }

    #[test]
    fn proxy_request_passes_through_streaming_platform_response_body() {
        futures::executor::block_on(async {
            // HTTP types can carry streaming bodies; proxy_request returns Ok even when
            // the origin sends a streaming body.
            let services = build_services_with_http_client(
                Arc::new(StreamingResponseHttpClient) as Arc<dyn PlatformHttpClient>
            );
            let settings = create_test_settings();
            let req = HttpRequest::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(EdgeBody::empty())
                .expect("should build test request");

            let result = proxy_request(
                &settings,
                req,
                ProxyRequestConfig {
                    target_url: "https://example.com/resource",
                    follow_redirects: false,
                    forward_ec_id: false,
                    body: None,
                    headers: Vec::new(),
                    copy_request_headers: false,
                    stream_passthrough: false,
                    allowed_domains: &[],
                    require_https: false,
                },
                &services,
            )
            .await;

            assert!(
                result.is_ok(),
                "should pass streaming body through with HTTP types: {result:?}"
            );
            assert_eq!(
                result.expect("should succeed").status(),
                StatusCode::OK,
                "should preserve the origin status code"
            );
        });
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

    #[test]
    fn handle_asset_proxy_request_forwards_asset_headers_and_host() {
        futures::executor::block_on(async {
            use crate::platform::test_support::StubHttpClient;

            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let settings = create_test_settings();
            let mut req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/foo.jpg?auto=webp",
            );
            req.headers_mut().insert(
                header::USER_AGENT,
                HeaderValue::from_static("asset-agent/1.0"),
            );
            req.headers_mut().insert(
                header::ACCEPT,
                HeaderValue::from_static("image/avif,image/webp,image/*,*/*;q=0.8"),
            );
            req.headers_mut().insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("gzip, br"),
            );
            req.headers_mut()
                .insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));
            req.headers_mut().insert(
                header::REFERER,
                HeaderValue::from_static("https://www.example.com/article"),
            );
            req.headers_mut().insert(
                header::IF_NONE_MATCH,
                HeaderValue::from_static("\"asset-etag\""),
            );
            req.headers_mut().insert(
                header::IF_MODIFIED_SINCE,
                HeaderValue::from_static("Thu, 13 Mar 2025 08:00:00 GMT"),
            );
            req.headers_mut().insert(
                header::IF_MATCH,
                HeaderValue::from_static("\"asset-precondition\""),
            );
            req.headers_mut().insert(
                header::IF_UNMODIFIED_SINCE,
                HeaderValue::from_static("Thu, 13 Mar 2025 09:00:00 GMT"),
            );
            req.headers_mut()
                .insert(header::RANGE, HeaderValue::from_static("bytes=0-1023"));
            req.headers_mut().insert(
                header::IF_RANGE,
                HeaderValue::from_static("\"asset-range\""),
            );
            req.headers_mut().insert(
                HEADER_X_FORWARDED_FOR,
                HeaderValue::from_static("198.51.100.10"),
            );
            req.headers_mut().insert(
                header::HeaderName::from_static("x-custom-test"),
                HeaderValue::from_static("drop-me"),
            );

            let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com:8443");
            let response = handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy asset request")
                .into_response()
                .expect("should return buffered asset response");
            assert_eq!(response.status(), StatusCode::OK);

            let all_headers = stub.recorded_request_headers();
            assert_eq!(all_headers.len(), 1, "should have captured one request");
            let sent = &all_headers[0];
            let header_value = |name: &str| -> Option<String> {
                sent.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
            };

            assert_eq!(
                header_value("user-agent").as_deref(),
                Some("asset-agent/1.0"),
                "should forward User-Agent"
            );
            assert_eq!(
                header_value("accept-encoding").as_deref(),
                Some("gzip, br"),
                "should preserve the incoming Accept-Encoding"
            );
            assert_eq!(
                header_value("if-none-match").as_deref(),
                Some("\"asset-etag\""),
                "should forward conditional ETag validation headers"
            );
            assert_eq!(
                header_value("if-modified-since").as_deref(),
                Some("Thu, 13 Mar 2025 08:00:00 GMT"),
                "should forward conditional date validation headers"
            );
            assert_eq!(
                header_value("if-match").as_deref(),
                Some("\"asset-precondition\""),
                "should forward precondition headers"
            );
            assert_eq!(
                header_value("if-unmodified-since").as_deref(),
                Some("Thu, 13 Mar 2025 09:00:00 GMT"),
                "should forward date precondition headers"
            );
            assert_eq!(
                header_value("range").as_deref(),
                Some("bytes=0-1023"),
                "should forward byte-range requests"
            );
            assert_eq!(
                header_value("if-range").as_deref(),
                Some("\"asset-range\""),
                "should forward range validators"
            );
            assert_eq!(
                header_value("host").as_deref(),
                Some("assets.example.com:8443"),
                "should override Host to the asset origin host"
            );
            assert!(
                header_value("x-forwarded-for").is_none(),
                "should not forward client-supplied X-Forwarded-For"
            );
            assert!(
                header_value("x-custom-test").is_none(),
                "should not forward unrelated custom headers"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_strips_unsafe_response_headers() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response_with_headers(
                200,
                Vec::new(),
                vec![
                    (header::SET_COOKIE.as_str(), "asset=1; Path=/; Secure"),
                    (header::SET_COOKIE.as_str(), "other=2; Path=/; Secure"),
                    (
                        header::STRICT_TRANSPORT_SECURITY.as_str(),
                        "max-age=31536000; includeSubDomains; preload",
                    ),
                    ("clear-site-data", "\"*\""),
                    (header::ETAG.as_str(), "\"asset-etag\""),
                ],
            );
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let settings = create_test_settings();
            let req = build_http_request(Method::GET, "https://www.example.com/.images/foo.jpg");

            let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
            let response = handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy asset request")
                .into_response()
                .expect("should return buffered asset response");

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
        });
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

    fn test_s3_secrets() -> HashMap<String, Vec<u8>> {
        HashMap::from([
            (
                "access_key_id".to_string(),
                b"AKIAIOSFODNN7EXAMPLE".to_vec(),
            ),
            (
                "secret_access_key".to_string(),
                b"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_vec(),
            ),
        ])
    }

    fn test_s3_image_optimizer_route() -> ProxyAssetRoute {
        let mut route = ProxyAssetRoute::new(
            "/.images/",
            "https://examplebucket.s3.us-east-1.amazonaws.com",
        );
        route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: "us-east-1".to_string(),
            secret_store: "s3-auth".to_string(),
            access_key_id: "access_key_id".to_string(),
            secret_access_key: "secret_access_key".to_string(),
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

    #[test]
    fn handle_asset_proxy_request_signs_s3_and_strips_transform_query() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let mut secrets = HashMap::new();
            secrets.insert(
                "access_key_id".to_string(),
                b"AKIAIOSFODNN7EXAMPLE".to_vec(),
            );
            secrets.insert(
                "secret_access_key".to_string(),
                b"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_vec(),
            );
            let services = build_services_with_secret_and_http_client(
                HashMapSecretStore::new(secrets),
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            );
            let settings = create_test_settings();
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/foo.jpg?profile=medium&ar=1-1",
            );
            let mut route = ProxyAssetRoute::new(
                "/.images/",
                "https://examplebucket.s3.us-east-1.amazonaws.com",
            );
            route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
                region: "us-east-1".to_string(),
                secret_store: "s3-auth".to_string(),
                access_key_id: "access_key_id".to_string(),
                secret_access_key: "secret_access_key".to_string(),
                session_token: None,
                origin_query: Some(OriginQueryPolicy::Strip),
            }));

            handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy signed S3 asset request");

            let uris = stub.recorded_request_uris();
            assert_eq!(
                uris,
                vec!["https://examplebucket.s3.us-east-1.amazonaws.com/.images/foo.jpg"],
                "should strip transform query before signing and sending to S3"
            );
            let headers = stub.recorded_request_headers();
            let sent = &headers[0];
            let header_value = |name: &str| -> Option<String> {
                sent.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
            };
            assert_eq!(
                header_value("host").as_deref(),
                Some("examplebucket.s3.us-east-1.amazonaws.com"),
                "should sign for the S3 origin host"
            );
            assert!(
                header_value("authorization")
                    .as_deref()
                    .is_some_and(|value| value.starts_with("AWS4-HMAC-SHA256 Credential=")),
                "should add a SigV4 Authorization header"
            );
            assert_eq!(
                header_value("x-amz-content-sha256").as_deref(),
                Some("UNSIGNED-PAYLOAD"),
                "should use unsigned payload for read-only asset requests"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_caches_s3_credentials_for_repeated_signing() {
        futures::executor::block_on(async {
            clear_s3_credentials_cache_for_tests();
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, Vec::new());
            stub.push_response(200, b"optimized".to_vec());
            let secret_store = CountingSecretStore::new(HashMap::from([
                (
                    "cache_access_key_id".to_string(),
                    b"AKIAIOSFODNN7EXAMPLE".to_vec(),
                ),
                (
                    "cache_secret_access_key".to_string(),
                    b"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_vec(),
                ),
                ("cache_session_token".to_string(), b"session-token".to_vec()),
            ]));
            let observed_secret_store = secret_store.clone();
            let services = build_services_with_secret_and_http_client(
                secret_store,
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            );
            let mut settings = create_test_settings();
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), test_profile_set())]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/foo.jpg?profile=medium",
            );
            let mut route = test_s3_image_optimizer_route();
            route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
                region: "us-east-1".to_string(),
                secret_store: "s3-auth-cache".to_string(),
                access_key_id: "cache_access_key_id".to_string(),
                secret_access_key: "cache_secret_access_key".to_string(),
                session_token: Some("cache_session_token".to_string()),
                origin_query: None,
            }));

            handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy optimized S3 asset request");

            assert_eq!(
                stub.recorded_request_methods(),
                vec!["HEAD", "GET"],
                "should sign both the S3 preflight and final request"
            );
            assert_eq!(
                observed_secret_store.read_count("cache_access_key_id"),
                1,
                "should read S3 access key ID once despite repeated signing"
            );
            assert_eq!(
                observed_secret_store.read_count("cache_secret_access_key"),
                1,
                "should read S3 secret access key once despite repeated signing"
            );
            assert_eq!(
                observed_secret_store.read_count("cache_session_token"),
                1,
                "should read S3 session token once despite repeated signing"
            );
            let headers = stub.recorded_request_headers();
            assert!(
                headers
                    .iter()
                    .all(|sent| sent
                        .iter()
                        .any(|(name, value)| name == "x-amz-security-token"
                            && value == "session-token")),
                "should sign both requests with the cached session token"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_attaches_image_optimizer_metadata() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let mut settings = create_test_settings();
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), test_profile_set())]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/foo.jpg?profile=medium&ar=1-1&x=71&y=bad",
            );
            let mut route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
            route.image_optimizer = Some(AssetImageOptimizerConfig {
                enabled: true,
                region: "us_east".to_string(),
                profile_set: "default_images".to_string(),
                origin_query: None,
            });

            handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy optimized asset request");

            let uris = stub.recorded_request_uris();
            assert_eq!(
                uris,
                vec!["https://assets.example.com/.images/foo.jpg"],
                "should strip profile-table query from the origin request by default"
            );
            assert_eq!(
                stub.recorded_stream_response_flags(),
                vec![true],
                "final asset responses should stay streaming"
            );
            let options = stub.recorded_image_optimizer_options();
            let options = options[0]
                .as_ref()
                .expect("should attach image optimizer metadata");
            assert_eq!(options.region, "us_east");
            assert!(!options.preserve_query_string_on_origin_request);
            assert_eq!(options.params.quality, Some(70));
            assert_eq!(options.params.resize_filter.as_deref(), Some("bicubic"));
            assert_eq!(options.params.format.as_deref(), Some("auto"));
            assert_eq!(options.params.width, Some(828));
            let crop = options.params.crop.as_ref().expect("should set crop");
            assert_eq!((crop.width, crop.height), (1, 1));
            assert_eq!((crop.offset_x, crop.offset_y), (Some(70), Some(50)));
        });
    }

    #[test]
    fn handle_asset_proxy_request_skips_image_optimizer_for_svg_assets() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let mut settings = create_test_settings();
            let mut profile_set = test_profile_set();
            profile_set.unknown_profile = UnknownProfilePolicy::Reject;
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), profile_set)]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/logo.SVG?profile=unknown&ar=1-1",
            );
            let mut route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
            route.image_optimizer = Some(AssetImageOptimizerConfig {
                enabled: true,
                region: "us_east".to_string(),
                profile_set: "default_images".to_string(),
                origin_query: None,
            });

            handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy SVG asset without Image Optimizer profile parsing");

            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://assets.example.com/.images/logo.SVG"],
                "should still strip profile-table query from SVG origin requests"
            );
            assert_eq!(
                stub.recorded_image_optimizer_options(),
                vec![None],
                "SVG assets should bypass Image Optimizer metadata"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_skips_image_optimizer_for_incoming_svg() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let mut settings = create_test_settings();
            let mut profile_set = test_profile_set();
            profile_set.unknown_profile = UnknownProfilePolicy::Reject;
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), profile_set)]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.image/object-id/logo.svg?profile=unknown&ar=1-1",
            );
            let mut route = ProxyAssetRoute::new("/.image/", "https://assets.example.com");
            route.path_pattern = Some(r"^/\.image/([^/]+)/[^/]+\.([^/.]+)$".to_string());
            route.target_path = Some("/image/upload/$1".to_string());
            route.image_optimizer = Some(AssetImageOptimizerConfig {
                enabled: true,
                region: "us_east".to_string(),
                profile_set: "default_images".to_string(),
                origin_query: None,
            });

            handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy incoming SVG asset without Image Optimizer profile parsing");

            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://assets.example.com/image/upload/object-id"],
                "should strip profile-table query even when SVG target path omits extension"
            );
            assert_eq!(
                stub.recorded_image_optimizer_options(),
                vec![None],
                "incoming SVG assets should bypass Image Optimizer metadata"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_skips_image_optimizer_for_rewritten_svg_assets() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let mut settings = create_test_settings();
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), test_profile_set())]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.image/options/id/logo.svg?profile=medium",
            );
            let mut route = ProxyAssetRoute::new("/.image/", "https://assets.example.com");
            route.path_pattern = Some(r"^/\.image/(.*)/[^/]+\.([^/.]+)$".to_string());
            route.target_path = Some("/image/upload/$1.$2".to_string());
            route.image_optimizer = Some(AssetImageOptimizerConfig {
                enabled: true,
                region: "us_east".to_string(),
                profile_set: "default_images".to_string(),
                origin_query: None,
            });

            handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy rewritten SVG asset without Image Optimizer metadata");

            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://assets.example.com/image/upload/options/id.svg"],
                "should detect SVG after route path rewriting"
            );
            assert_eq!(
                stub.recorded_image_optimizer_options(),
                vec![None],
                "rewritten SVG assets should bypass Image Optimizer metadata"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_skips_s3_preflight_for_svg_image_optimizer_routes() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"raw-svg".to_vec());
            let services = build_services_with_secret_and_http_client(
                HashMapSecretStore::new(test_s3_secrets()),
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            );
            let settings = create_test_settings();
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/logo.svg?profile=medium",
            );
            let route = test_s3_image_optimizer_route();

            let response = handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy raw SVG asset through S3 route")
                .into_response()
                .expect("should return buffered asset response");

            assert_eq!(response_body_string(response), "raw-svg");
            assert_eq!(
                stub.recorded_request_methods(),
                vec!["GET"],
                "SVG IO bypass should not add an S3 preflight"
            );
            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://examplebucket.s3.us-east-1.amazonaws.com/.images/logo.svg"],
                "SVG IO bypass should still strip the transform query"
            );
            assert_eq!(
                stub.recorded_image_optimizer_options(),
                vec![None],
                "SVG IO bypass should not attach Image Optimizer metadata"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_preflights_s3_before_image_optimizer() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, Vec::new());
            stub.push_response(200, b"optimized".to_vec());
            let services = build_services_with_secret_and_http_client(
                HashMapSecretStore::new(test_s3_secrets()),
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            );
            let mut settings = create_test_settings();
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), test_profile_set())]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/foo.jpg?profile=medium",
            );
            let route = test_s3_image_optimizer_route();

            let response = handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy optimized S3 asset request")
                .into_response()
                .expect("should return buffered asset response");

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response_body_string(response), "optimized");
            assert_eq!(
                stub.recorded_request_methods(),
                vec!["HEAD", "GET"],
                "should preflight S3 with HEAD before the optimized GET"
            );
            assert_eq!(
                stub.recorded_request_uris(),
                vec![
                    "https://examplebucket.s3.us-east-1.amazonaws.com/.images/foo.jpg",
                    "https://examplebucket.s3.us-east-1.amazonaws.com/.images/foo.jpg",
                ],
                "should strip transform query from both S3 requests"
            );
            assert_eq!(
                stub.recorded_stream_response_flags(),
                vec![false, true],
                "S3 HEAD preflight stays non-streaming, final asset GET stays streaming"
            );
            let options = stub.recorded_image_optimizer_options();
            assert_eq!(options.len(), 2, "should send two origin requests");
            assert!(
                options[0].is_none(),
                "preflight should not attach IO metadata"
            );
            assert!(
                options[1].is_some(),
                "final asset request should attach IO metadata"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_returns_raw_s3_error_before_image_optimizer() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(404, Vec::new());
            stub.push_response_with_headers(
                404,
                br#"<Error><Code>NoSuchKey</Code><Key>image/upload/missing.jpg</Key></Error>"#
                    .to_vec(),
                vec![
                    (header::CONTENT_TYPE.as_str(), "application/xml"),
                    (header::CACHE_CONTROL.as_str(), "public, max-age=3600"),
                    (header::SET_COOKIE.as_str(), "asset=1; Path=/"),
                ],
            );
            let services = build_services_with_secret_and_http_client(
                HashMapSecretStore::new(test_s3_secrets()),
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            );
            let mut settings = create_test_settings();
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), test_profile_set())]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/missing.jpg?profile=medium",
            );
            let route = test_s3_image_optimizer_route();

            let asset_response = handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should return raw S3 error response");
            assert_eq!(
                asset_response.cache_policy(),
                AssetProxyCachePolicy::NoStorePrivate,
                "should carry a typed no-store policy for router finalization"
            );
            let response = asset_response
                .into_response()
                .expect("should return buffered asset response");

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
            let body = response_body_string(response);
            assert!(body.contains("NoSuchKey"), "should return S3 error body");
            assert!(
                body.contains("image/upload/missing.jpg"),
                "should expose the missing S3 key"
            );
            assert_eq!(
                stub.recorded_request_methods(),
                vec!["HEAD", "GET"],
                "should fetch S3 error body after failed HEAD preflight"
            );
            assert_eq!(
                stub.recorded_stream_response_flags(),
                vec![false, true],
                "S3 HEAD preflight stays non-streaming, error-body GET stays streaming"
            );
            let options = stub.recorded_image_optimizer_options();
            assert_eq!(
                options,
                vec![None, None],
                "should not invoke IO for S3 errors"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_does_not_preflight_when_io_disabled() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"raw".to_vec());
            let services = build_services_with_secret_and_http_client(
                HashMapSecretStore::new(test_s3_secrets()),
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            );
            let mut settings = create_test_settings();
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), test_profile_set())]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/foo.jpg?profile=medium&_io_debug=1",
            );
            let route = test_s3_image_optimizer_route();

            let response = handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy debug S3 asset request")
                .into_response()
                .expect("should return buffered asset response");

            assert_eq!(response_body_string(response), "raw");
            assert_eq!(
                stub.recorded_request_methods(),
                vec!["GET"],
                "disabled IO should not add S3 preflight"
            );
            assert_eq!(
                stub.recorded_image_optimizer_options(),
                vec![None],
                "debug query param should disable image optimizer metadata"
            );
            assert_eq!(
                stub.recorded_stream_response_flags(),
                vec![true],
                "debug asset responses should still stay streaming"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_debug_param_disables_image_optimizer() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let mut settings = create_test_settings();
            settings.image_optimizer = ImageOptimizerSettings {
                profile_sets: HashMap::from([("default_images".to_string(), test_profile_set())]),
            };
            let req = build_http_request(
                Method::GET,
                "https://www.example.com/.images/foo.jpg?profile=medium&_io_debug=1",
            );
            let mut route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
            route.image_optimizer = Some(AssetImageOptimizerConfig {
                enabled: true,
                region: "us_east".to_string(),
                profile_set: "default_images".to_string(),
                origin_query: None,
            });

            handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy debug asset request");

            let options = stub.recorded_image_optimizer_options();
            assert!(
                options[0].is_none(),
                "debug query param should disable image optimizer metadata"
            );
        });
    }

    #[test]
    fn handle_asset_proxy_request_accepts_streaming_platform_response_body() {
        futures::executor::block_on(async {
            let services = build_services_with_http_client(
                Arc::new(StreamingResponseHttpClient) as Arc<dyn PlatformHttpClient>
            );
            let settings = create_test_settings();
            let req = build_http_request(Method::GET, "https://www.example.com/.images/foo.jpg");
            let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");

            let (response, stream_body) =
                handle_asset_proxy_request(&settings, &services, req, &route)
                    .await
                    .expect("should proxy a streaming asset response")
                    .into_response_and_body();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers().get(header::SET_COOKIE).is_none(),
                "should strip upstream Set-Cookie headers from streaming asset responses"
            );
            assert!(
                response
                    .headers()
                    .get(header::STRICT_TRANSPORT_SECURITY)
                    .is_none(),
                "should strip upstream HSTS headers from streaming asset responses"
            );
            assert!(
                response.headers().get("clear-site-data").is_none(),
                "should strip upstream Clear-Site-Data headers from streaming asset responses"
            );
            assert_eq!(
                response_header(&response, header::ETAG),
                Some("\"stream-etag\""),
                "should preserve safe cache validator headers on streaming asset responses"
            );

            let mut output = Vec::new();
            stream_asset_body(
                stream_body.expect("should preserve asset body as a stream"),
                &mut output,
            )
            .await
            .expect("should stream asset body");

            assert_eq!(output, b"chunk");
        });
    }

    #[test]
    fn stream_asset_body_reports_mid_stream_origin_errors() {
        futures::executor::block_on(async {
            let body = EdgeBody::from_stream(futures::stream::iter(vec![
                Ok::<Bytes, io::Error>(Bytes::from_static(b"partial")),
                Err(io::Error::other("origin stream failed")),
            ]));
            let mut output = Vec::new();

            let err = stream_asset_body(body, &mut output)
                .await
                .expect_err("should report mid-stream origin errors");

            assert_eq!(
                output, b"partial",
                "should only write chunks received before the stream error"
            );
            assert!(
                format!("{err:?}").contains("streaming platform response body failed"),
                "should describe the streaming failure: {err:?}"
            );
        });
    }

    #[test]
    fn asset_proxy_response_into_response_rejects_stream_body() {
        futures::executor::block_on(async {
            let services = build_services_with_http_client(
                Arc::new(StreamingResponseHttpClient) as Arc<dyn PlatformHttpClient>
            );
            let settings = create_test_settings();
            let req = build_http_request(Method::GET, "https://www.example.com/.images/foo.jpg");
            let route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
            let asset_response = handle_asset_proxy_request(&settings, &services, req, &route)
                .await
                .expect("should proxy a streaming asset response");

            let err = asset_response
                .into_response()
                .expect_err("streaming asset responses should require explicit stream handling");
            assert!(
                format!("{err:?}").contains("streaming asset response cannot be converted"),
                "should describe the buffered conversion failure: {err:?}"
            );
        });
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
    fn redirect_empty_allowlist_permits_any() {
        let allowed: [String; 0] = [];
        assert!(
            redirect_is_permitted(&allowed, "evil.com"),
            "empty allowlist should not block any redirect host"
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

    // --- redirect_is_permitted (full guard: empty-list bypass + is_host_allowed) ---

    #[test]
    fn redirect_chain_allowed_when_host_matches_allowlist() {
        let allowed = vec!["ad.example.com".to_string(), "cdn.example.com".to_string()];
        assert!(
            redirect_is_permitted(&allowed, "ad.example.com"),
            "should permit redirect to exact-match host"
        );
        assert!(
            redirect_is_permitted(&allowed, "cdn.example.com"),
            "should permit redirect to second allowed host"
        );
    }

    #[test]
    fn redirect_chain_allowed_when_host_matches_wildcard() {
        let allowed = vec!["*.example.com".to_string()];
        assert!(
            redirect_is_permitted(&allowed, "sub.example.com"),
            "should permit redirect to wildcard-matched subdomain"
        );
    }

    #[test]
    fn redirect_chain_blocked_when_host_not_in_allowlist() {
        let allowed = vec!["ad.example.com".to_string()];
        assert!(
            !redirect_is_permitted(&allowed, "evil.com"),
            "should block redirect to host not in allowlist"
        );
    }

    #[test]
    fn redirect_chain_allowed_when_allowlist_is_empty() {
        let allowed: Vec<String> = vec![];
        assert!(
            redirect_is_permitted(&allowed, "any-host.com"),
            "should allow any redirect when allowlist is empty (open mode)"
        );
    }

    #[test]
    fn redirect_chain_blocked_when_host_is_empty() {
        let allowed = vec!["example.com".to_string()];
        assert!(
            !redirect_is_permitted(&allowed, ""),
            "should block redirect with empty host when allowlist is non-empty"
        );
    }

    #[test]
    fn redirect_is_permitted_accepts_str_slices() {
        // Verifies the &[impl AsRef<str>] bound works with &str literals,
        // not just Vec<String>.
        let allowed: &[&str] = &["example.com", "*.cdn.example.com"];
        assert!(
            redirect_is_permitted(allowed, "example.com"),
            "should permit exact match via &str slice"
        );
        assert!(
            redirect_is_permitted(allowed, "static.cdn.example.com"),
            "should permit wildcard match via &str slice"
        );
        assert!(
            !redirect_is_permitted(allowed, "evil.com"),
            "should block host not in &str slice allowlist"
        );
    }

    #[test]
    fn ip_literal_blocked_by_domain_allowlist() {
        let allowed = vec!["*.example.com".to_string()];
        assert!(
            !redirect_is_permitted(&allowed, "169.254.169.254"),
            "should block cloud metadata IP"
        );
        assert!(
            !redirect_is_permitted(&allowed, "127.0.0.1"),
            "should block loopback IPv4"
        );
        assert!(
            !redirect_is_permitted(&allowed, "[::1]"),
            "should block loopback IPv6"
        );
        assert!(
            !redirect_is_permitted(&allowed, "::1"),
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
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let services = crate::platform::test_support::build_services_with_http_client(
                Arc::new(StreamingResponseHttpClient) as Arc<dyn PlatformHttpClient>,
            );
            let req = build_http_request(
                Method::GET,
                "https://edge.example/integrations/prebid/bundle.js",
            );
            let config = ProxyRequestConfig::new("http://assets.example/prebid/trusted-prebid.js")
                .without_ec_id()
                .without_forward_headers()
                .with_streaming()
                .with_https_only();

            let err = proxy_request(&settings, req, config, &services)
                .await
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
        });
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
    fn sign_rejects_streaming_body() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let req = build_http_post_streaming_request("https://edge.example/first-party/sign");
            let err = handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect_err("should reject streaming sign body");
            assert_eq!(
                err.current_context().status_code(),
                StatusCode::BAD_REQUEST,
                "should return 400 for streaming sign body"
            );
            assert!(
                matches!(
                    err.current_context(),
                    TrustedServerError::BadRequest { message }
                        if message == "first-party sign request body must be buffered, not streamed"
                ),
                "should explain that sign bodies must be buffered"
            );
        });
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
