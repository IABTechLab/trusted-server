use crate::http_util::{compute_encrypted_sha256_token, ct_str_eq};
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{request_builder as edge_request_builder, Uri as EdgeUri};
use error_stack::{Report, ResultExt};
use fastly::http::{header, HeaderValue, Method, StatusCode};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
    HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
};
use crate::creative::{CreativeCssProcessor, CreativeHtmlProcessor};
use crate::edge_cookie::get_ec_id;
use crate::error::TrustedServerError;
use crate::platform::{
    PlatformBackendSpec, PlatformHttpRequest, PlatformResponse, RuntimeServices,
};
use crate::settings::Settings;
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};

/// Chunk size used for streaming content through the rewrite pipeline.
const STREAMING_CHUNK_SIZE: usize = 8192;

/// Headers copied from the original client request to the upstream proxy request.
const PROXY_FORWARD_HEADERS: [header::HeaderName; 5] = [
    HEADER_USER_AGENT,
    HEADER_ACCEPT,
    HEADER_ACCEPT_LANGUAGE,
    HEADER_REFERER,
    HEADER_X_FORWARDED_FOR,
];

/// Convert a platform-neutral response into a [`fastly::Response`] for downstream processing.
///
/// Shared with `auction/orchestrator.rs`. Both files will migrate off `fastly::Response`
/// entirely in Phase 2, at which point this conversion helper will be removed.
///
/// # Panics (debug builds only)
///
/// Panics when `platform_resp` carries a `Body::Stream` body, which indicates a
/// programming error — all outbound proxy bodies are built from byte slices and
/// are therefore always `Body::Once`.
pub(crate) fn platform_response_to_fastly(platform_resp: PlatformResponse) -> Response {
    let (parts, body) = platform_resp.response.into_parts();
    debug_assert!(
        matches!(&body, EdgeBody::Once(_)),
        "unexpected Body::Stream in platform response conversion: body will be empty"
    );
    let body_bytes = match body {
        EdgeBody::Once(bytes) => bytes.to_vec(),
        EdgeBody::Stream(_) => {
            log::warn!("streaming platform response body; body will be empty");
            vec![]
        }
    };
    let mut resp = Response::from_status(parts.status.as_u16());
    for (name, value) in parts.headers.iter() {
        resp.set_header(name.as_str(), value.as_bytes());
    }
    resp.set_body(body_bytes);
    resp
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
    /// Domain allowlist enforced on the initial target and every redirect hop.
    ///
    /// An empty slice disables allowlist enforcement (open mode).
    /// Integration proxies should pass `&[]`; first-party proxy passes
    /// `&settings.proxy.allowed_domains`.
    pub allowed_domains: &'a [String],
}

impl<'a> ProxyRequestConfig<'a> {
    /// Build a proxy configuration that follows redirects and forwards the EC ID.
    ///
    /// `allowed_domains` defaults to `&[]` (open mode). Override it for the
    /// first-party proxy by setting `allowed_domains` directly.
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
}

/// Encodings we support decompressing in `finalize_proxied_response`.
/// We override the client's Accept-Encoding to only advertise these.
const SUPPORTED_ENCODINGS: &str = "gzip, deflate, br";

/// Rebuild a response with a new body, preserving headers except Content-Length.
/// If `preserve_encoding` is true, the Content-Encoding header is kept (for compressed responses).
/// If false, Content-Encoding is stripped (for decompressed responses).
fn rebuild_response_with_body(
    beresp: &Response,
    content_type: &'static str,
    body: Vec<u8>,
    preserve_encoding: bool,
) -> Response {
    let status = beresp.get_status();
    let headers: Vec<(header::HeaderName, HeaderValue)> = beresp
        .get_headers()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut resp = Response::from_status(status);
    for (name, value) in headers {
        // Always skip Content-Length (size changed) and Content-Type (we set it)
        if name == header::CONTENT_LENGTH || name == header::CONTENT_TYPE {
            continue;
        }
        // Skip Content-Encoding only if we're not preserving it
        if name == header::CONTENT_ENCODING && !preserve_encoding {
            continue;
        }
        resp.set_header(name, value);
    }
    resp.set_header(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    resp.set_body(body);
    resp
}

/// Process a response body through a streaming pipeline with the given processor.
///
/// Handles decompression, content processing, and re-compression while preserving
/// the response status and headers.
fn process_response_with_pipeline<P: StreamProcessor>(
    mut beresp: Response,
    processor: P,
    compression: Compression,
    content_type: &'static str,
    error_context: &'static str,
) -> Result<Response, Report<TrustedServerError>> {
    let config = PipelineConfig {
        input_compression: compression,
        output_compression: compression,
        chunk_size: STREAMING_CHUNK_SIZE,
    };

    let body = beresp.take_body();
    let mut output = Vec::new();
    let mut pipeline = StreamingPipeline::new(config, processor);
    pipeline
        .process(body, &mut output)
        .change_context(TrustedServerError::Proxy {
            message: error_context.to_string(),
        })?;

    Ok(rebuild_response_with_body(
        &beresp,
        content_type,
        output,
        compression != Compression::None,
    ))
}

fn finalize_proxied_response(
    settings: &Settings,
    req: &Request,
    target_url: &str,
    mut beresp: Response,
) -> Result<Response, Report<TrustedServerError>> {
    // Determine content-type and content-encoding from response headers
    let status_code = beresp.get_status().as_u16();
    let ct_raw = beresp
        .get_header(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let content_encoding = beresp
        .get_header(header::CONTENT_ENCODING)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let cl_raw = beresp
        .get_header(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-");
    let accept_raw = req
        .get_header(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-");

    let ct_for_log: &str = if ct_raw.is_empty() { "-" } else { &ct_raw };
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

    let ct = ct_raw.to_ascii_lowercase();
    let compression = Compression::from_content_encoding(&content_encoding);

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

    // Image handling: set generic content-type if missing and log pixel heuristics
    let req_accept_images = req
        .get_header(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("image/"))
        .unwrap_or(false);

    if ct.starts_with("image/") || req_accept_images {
        if beresp.get_header(header::CONTENT_TYPE).is_none() {
            beresp.set_header(header::CONTENT_TYPE, "image/*");
        }

        // Heuristics to log likely tracking pixels without altering response
        let mut is_pixel = false;
        if let Some(cl) = beresp
            .get_header(header::CONTENT_LENGTH)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if cl <= 256 {
                // typical 1x1 PNG/GIF are very small
                is_pixel = true;
            }
        }

        // Path heuristics: common pixel patterns
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
            log::info!("likely pixel image fetched: {} ct={}", target_url, ct);
        }

        return Ok(beresp);
    }

    // Passthrough for non-text, non-image responses
    Ok(beresp)
}

fn finalize_proxied_response_streaming(
    req: &Request,
    target_url: &str,
    mut beresp: Response,
) -> Response {
    let status_code = beresp.get_status().as_u16();
    let ct_raw = beresp
        .get_header(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let cl_raw = beresp
        .get_header(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-");
    let accept_raw = req
        .get_header(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-");

    let ct_for_log: &str = if ct_raw.is_empty() { "-" } else { &ct_raw };
    log::info!(
        "origin response status={} ct={} cl={} accept={} url={}",
        status_code,
        ct_for_log,
        cl_raw,
        accept_raw,
        target_url
    );

    let ct = ct_raw.to_ascii_lowercase();

    let req_accept_images = req
        .get_header(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("image/"))
        .unwrap_or(false);

    if ct.starts_with("image/") || req_accept_images {
        if beresp.get_header(header::CONTENT_TYPE).is_none() {
            beresp.set_header(header::CONTENT_TYPE, "image/*");
        }

        let mut is_pixel = false;
        if let Some(cl) = beresp
            .get_header(header::CONTENT_LENGTH)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if cl <= 256 {
                is_pixel = true;
            }
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
                "stream: likely pixel image fetched: {} ct={}",
                target_url,
                ct
            );
        }
    }

    beresp
}

/// Finalize a proxied response, choosing between streaming passthrough and full
/// content processing based on the `stream_passthrough` flag.
fn finalize_response(
    settings: &Settings,
    req: &Request,
    url: &str,
    beresp: Response,
    stream_passthrough: bool,
) -> Result<Response, Report<TrustedServerError>> {
    if stream_passthrough {
        Ok(finalize_proxied_response_streaming(req, url, beresp))
    } else {
        finalize_proxied_response(settings, req, url, beresp)
    }
}

struct ProxyRequestHeaders<'a> {
    additional_headers: &'a [(header::HeaderName, HeaderValue)],
    copy_request_headers: bool,
    services: &'a RuntimeServices,
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
    req: Request,
    config: ProxyRequestConfig<'_>,
    services: &RuntimeServices,
) -> Result<Response, Report<TrustedServerError>> {
    let ProxyRequestConfig {
        target_url,
        follow_redirects,
        forward_ec_id,
        body,
        headers,
        copy_request_headers,
        stream_passthrough,
        allowed_domains: _,
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
        follow_redirects,
        body.as_deref(),
        ProxyRequestHeaders {
            additional_headers: &headers,
            copy_request_headers,
            services,
        },
        stream_passthrough,
    )
    .await
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

fn append_ec_id(req: &Request, target_url_parsed: &mut url::Url) {
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
fn is_host_allowed(host: &str, pattern: &str) -> bool {
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
    req: &Request,
    target_url_parsed: url::Url,
    follow_redirects: bool,
    body: Option<&[u8]>,
    request_headers: ProxyRequestHeaders<'_>,
    stream_passthrough: bool,
) -> Result<Response, Report<TrustedServerError>> {
    const MAX_REDIRECTS: usize = 4;

    let mut current_url = target_url_parsed.to_string();
    let mut current_method: Method = req.get_method().clone();

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

        let host = parsed_url.host_str().unwrap_or("");
        if host.is_empty() {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "missing host".to_string(),
            }));
        }

        if !redirect_is_permitted(&settings.proxy.allowed_domains, host) {
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
                certificate_check: settings.proxy.certificate_check,
                first_byte_timeout: Duration::from_secs(15),
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

        if request_headers.copy_request_headers {
            for header_name in PROXY_FORWARD_HEADERS {
                if let Some(v) = req.get_header(&header_name) {
                    builder = builder.header(header_name.as_str(), v.as_bytes());
                }
            }
            builder = builder.header(
                HEADER_ACCEPT_ENCODING.as_str(),
                SUPPORTED_ENCODINGS.as_bytes(),
            );
        }
        for (name, value) in request_headers.additional_headers {
            builder = builder.header(name.clone(), value.clone());
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

        let beresp = platform_response_to_fastly(platform_resp);

        if !follow_redirects {
            return finalize_response(settings, req, &current_url, beresp, stream_passthrough);
        }

        let status = beresp.get_status();
        let is_redirect = matches!(
            status,
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        );

        if !is_redirect {
            return finalize_response(settings, req, &current_url, beresp, stream_passthrough);
        }

        let Some(location) = beresp
            .get_header(header::LOCATION)
            .and_then(|h| h.to_str().ok())
            .filter(|value| !value.is_empty())
        else {
            return finalize_response(settings, req, &current_url, beresp, stream_passthrough);
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
        if next_scheme != "http" && next_scheme != "https" {
            return finalize_response(settings, req, &current_url, beresp, stream_passthrough);
        }

        let next_host = match next_url.host_str() {
            Some(h) if !h.is_empty() => h,
            _ => {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: "missing host in redirect location".to_string(),
                }));
            }
        };
        if !redirect_is_permitted(&settings.proxy.allowed_domains, next_host) {
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
/// - If the response is an image or the request `Accept` indicates images, ensures a
///   generic `image/*` content type if origin omitted it, and logs likely 1×1 pixels
///   using simple size/URL heuristics. No special response (still proxied).
///
/// # Errors
///
/// Returns an error if the signed target cannot be reconstructed or validation fails.
pub async fn handle_first_party_proxy(
    settings: &Settings,
    services: &RuntimeServices,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Parse, reconstruct, and validate the signed target URL
    let SignedTarget { target_url, .. } =
        reconstruct_and_validate_signed_target(settings, req.get_url_str())?;

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
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let SignedTarget {
        target_url: full_for_token,
        tsurl,
        had_params,
    } = reconstruct_and_validate_signed_target(settings, req.get_url_str())?;

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
        .get_header(HEADER_USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let referer = req
        .get_header(HEADER_REFERER)
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
    Ok(Response::from_status(fastly::http::StatusCode::FOUND)
        .with_header(header::LOCATION, &redirect_target)
        .with_header(header::CACHE_CONTROL, "no-store, private"))
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
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let method = req.get_method().clone();

    let payload = if method == fastly::http::Method::POST {
        let body = req.take_body_str();
        serde_json::from_str::<ProxySignReq>(&body).change_context(TrustedServerError::Proxy {
            message: "invalid JSON".to_string(),
        })?
    } else {
        let parsed =
            url::Url::parse(req.get_url_str()).change_context(TrustedServerError::Proxy {
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
        let default_scheme = url::Url::parse(req.get_url_str())
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

    let mut response = Response::from_status(fastly::http::StatusCode::OK);
    response.set_header(header::CONTENT_TYPE, "application/json; charset=utf-8");
    response.set_body(
        serde_json::to_string(&resp).change_context(TrustedServerError::Proxy {
            message: "failed to serialize".to_string(),
        })?,
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
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let method = req.get_method().clone();
    let payload = if method == fastly::http::Method::POST {
        let body = req.take_body_str();
        serde_json::from_str::<ProxyRebuildReq>(&body).change_context(
            TrustedServerError::Proxy {
                message: "invalid JSON".to_string(),
            },
        )?
    } else {
        // Support GET: /first-party/proxy-rebuild?tsclick=...&add=...&del=...
        let parsed =
            url::Url::parse(req.get_url_str()).change_context(TrustedServerError::Proxy {
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

    // Keep a snapshot before modifications for diagnostics
    let orig_before = orig.clone();

    // Apply removals
    if let Some(del) = &payload.del {
        for k in del {
            orig.remove(k);
        }
    }
    // Apply additions (must be new keys only)
    if let Some(add) = &payload.add {
        for (k, v) in add {
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

    if method == fastly::http::Method::GET {
        // Redirect for GET usage to streamline navigation
        Ok(Response::from_status(fastly::http::StatusCode::FOUND)
            .with_header(header::LOCATION, href)
            .with_header(header::CACHE_CONTROL, "no-store, private"))
    } else {
        let json = serde_json::to_string(&ProxyRebuildResp {
            href,
            base: tsurl.clone(),
            added,
            removed,
        })
        .unwrap_or_else(|_| "{}".to_string());
        Ok(Response::from_status(fastly::http::StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .with_header(header::CACHE_CONTROL, "no-store, private")
            .with_body(json))
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
    use super::{
        handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
        handle_first_party_proxy_sign, is_host_allowed, proxy_request,
        reconstruct_and_validate_signed_target, redirect_is_permitted, ProxyRequestConfig,
    };
    use crate::constants::HEADER_ACCEPT;
    use crate::creative;
    use crate::error::{IntoHttpResponse, TrustedServerError};
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;
    use error_stack::Report;
    use fastly::http::{header, HeaderValue, Method, StatusCode};
    use fastly::{Request, Response};

    #[tokio::test]
    async fn proxy_missing_param_returns_400() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/first-party/proxy");
        let err: Report<TrustedServerError> =
            handle_first_party_proxy(&settings, &noop_services(), req)
                .await
                .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn proxy_missing_or_invalid_token_returns_400() {
        let settings = create_test_settings();
        // missing tstoken should 400
        let req = Request::new(
            Method::GET,
            "https://example.com/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example%2Fa.png",
        );
        let err: Report<TrustedServerError> =
            handle_first_party_proxy(&settings, &noop_services(), req)
                .await
                .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn proxy_sign_returns_signed_url() {
        let settings = create_test_settings();
        let body = serde_json::json!({
            "url": "https://cdn.example/asset.js?c=3&b=2",
        });
        let mut req = Request::new(Method::POST, "https://edge.example/first-party/sign");
        req.set_body(body.to_string());
        let mut resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
            .await
            .expect("sign ok");
        assert_eq!(resp.get_status(), StatusCode::OK);
        let json = resp.take_body_str();
        assert!(json.contains("/first-party/proxy?tsurl="), "{}", json);
        assert!(json.contains("tsexp"), "{}", json);
        assert!(
            json.contains("\"base\":\"https://cdn.example/asset.js\""),
            "{}",
            json
        );
    }

    #[tokio::test]
    async fn proxy_sign_rejects_invalid_url() {
        let settings = create_test_settings();
        let body = serde_json::json!({
            "url": "data:image/png;base64,AAAA",
        });
        let mut req = Request::new(Method::POST, "https://edge.example/first-party/sign");
        req.set_body(body.to_string());
        let err: Report<TrustedServerError> =
            handle_first_party_proxy_sign(&settings, &noop_services(), req)
                .await
                .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn proxy_sign_preserves_non_standard_port() {
        let settings = create_test_settings();
        let body = serde_json::json!({
            "url": "https://cdn.example.com:9443/img/300x250.svg",
        });
        let mut req = Request::new(Method::POST, "https://edge.example/first-party/sign");
        req.set_body(body.to_string());
        let mut resp = handle_first_party_proxy_sign(&settings, &noop_services(), req)
            .await
            .expect("should sign URL with non-standard port");
        assert_eq!(
            resp.get_status(),
            StatusCode::OK,
            "should return 200 for valid sign request"
        );
        let json = resp.take_body_str();
        // Port 9443 should be preserved (URL-encoded as %3A9443)
        assert!(
            json.contains("%3A9443"),
            "Port should be preserved in signed URL: {}",
            json
        );
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

    #[tokio::test]
    async fn reconstruct_rejects_expired_tsexp() {
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

    #[tokio::test]
    async fn reconstruct_rejects_tampered_tstoken() {
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

    #[tokio::test]
    async fn click_missing_params_returns_400() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://edge.example/first-party/click");
        let err: Report<TrustedServerError> =
            handle_first_party_click(&settings, &noop_services(), req)
                .await
                .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn click_valid_token_redirects() {
        let settings = create_test_settings();
        let tsurl = "https://cdn.example/a.png";
        let params = "foo=1&bar=2";
        let full = format!("{}?{}", tsurl, params);
        let sig = crate::http_util::compute_encrypted_sha256_token(&settings, &full);
        let req = Request::new(
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
        assert_eq!(resp.get_status(), StatusCode::FOUND);
        let loc = resp
            .get_header(header::LOCATION)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        assert_eq!(loc, full);
    }

    #[tokio::test]
    async fn click_appends_ec_id_when_present() {
        let settings = create_test_settings();
        let tsurl = "https://cdn.example/a.png";
        let params = "foo=1";
        let full = format!("{}?{}", tsurl, params);
        let sig = crate::http_util::compute_encrypted_sha256_token(&settings, &full);
        let mut req = Request::new(
            Method::GET,
            format!(
                "https://edge.example/first-party/click?tsurl={}&{}&tstoken={}",
                url::form_urlencoded::byte_serialize(tsurl.as_bytes()).collect::<String>(),
                params,
                sig
            ),
        );
        req.set_header(crate::constants::HEADER_X_TS_EC, "ec-123");

        let resp = handle_first_party_click(&settings, &noop_services(), req)
            .await
            .expect("should redirect");

        let loc = resp
            .get_header(header::LOCATION)
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
    }

    #[tokio::test]
    async fn proxy_rebuild_adds_and_removes_params() {
        let settings = create_test_settings();
        // Original canonical (no token)
        let tsclick = "/first-party/click?tsurl=https%3A%2F%2Fcdn.example%2Flanding.html&x=1";
        let body = serde_json::json!({
            "tsclick": tsclick,
            "add": {"y": "2"},
            "del": ["x"],
        });
        let mut req = Request::new(
            Method::POST,
            "https://edge.example/first-party/proxy-rebuild",
        );
        req.set_body(serde_json::to_string(&body).expect("test JSON should serialize"));
        let mut resp = handle_first_party_proxy_rebuild(&settings, &noop_services(), req)
            .await
            .expect("rebuild ok");
        assert_eq!(resp.get_status(), StatusCode::OK);
        let json = resp.take_body_str();
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

    #[tokio::test]
    async fn reconstruct_valid_with_params_preserves_order() {
        let settings = create_test_settings();
        let clear = "https://cdn.example/asset.js?c=3&b=2&a=1";
        // Simulate creative-generated first-party URL
        let first_party = creative::build_proxy_url(&settings, clear);
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

    #[tokio::test]
    async fn reconstruct_valid_without_params() {
        let settings = create_test_settings();
        let clear = "https://cdn.example/asset.js";
        let first_party = creative::build_proxy_url(&settings, clear);
        let st = reconstruct_and_validate_signed_target(
            &settings,
            &format!("https://edge.example{}", first_party),
        )
        .expect("reconstruct ok");
        assert_eq!(st.tsurl, clear);
        assert!(!st.had_params);
        assert_eq!(st.target_url, clear);
    }

    #[tokio::test]
    async fn proxy_rejects_unsupported_scheme() {
        let settings = create_test_settings();
        let clear = "ftp://cdn.example/file.gif";
        // Build a first-party proxy URL with a token for the unsupported scheme
        let first_party = creative::build_proxy_url(&settings, clear);
        let req = Request::new(Method::GET, format!("https://edge.example{}", first_party));
        let err: Report<TrustedServerError> =
            handle_first_party_proxy(&settings, &noop_services(), req)
                .await
                .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn proxy_invalid_target_url_errors() {
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
        let req = Request::new(Method::GET, &url);
        let err: Report<TrustedServerError> =
            handle_first_party_proxy(&settings, &noop_services(), req)
                .await
                .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn click_sets_cache_control_no_store_private() {
        let settings = create_test_settings();
        let clear = "https://cdn.example/landing.html?x=1";
        let first_party = creative::build_click_url(&settings, clear);
        let req = Request::new(Method::GET, format!("https://edge.example{}", first_party));
        let resp = handle_first_party_click(&settings, &noop_services(), req)
            .await
            .expect("should redirect");
        assert_eq!(resp.get_status(), StatusCode::FOUND);
        let cc = resp
            .get_header(header::CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        assert!(cc.contains("no-store"));
        assert!(cc.contains("private"));
    }

    // --- Finalization path tests (no network) ---

    // Access the finalize function within the crate for testing
    use super::finalize_proxied_response as finalize;

    #[test]
    fn html_response_is_rewritten_and_content_type_set() {
        let settings = create_test_settings();
        // HTML with an external image that should be proxied in rewrite
        let html = r#"<html><body><img src="https://cdn.example/a.png"></body></html>"#;
        let beresp = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .with_header(header::CACHE_CONTROL, "public, max-age=60")
            .with_header(header::SET_COOKIE, "a=1; Path=/; Secure")
            .with_body(html);
        // Sanity: header present and creative rewrite works directly
        let ct_pre = beresp
            .get_header(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct_pre.contains("text/html"), "ct_pre={}", ct_pre);
        let direct = creative::rewrite_creative_html(&settings, html);
        assert!(direct.contains("/first-party/proxy?tsurl="), "{}", direct);
        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/a.png", beresp)
            .expect("finalize should succeed");
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_str()
            .expect("Content-Type should be valid UTF-8");
        assert_eq!(ct, "text/html; charset=utf-8");
        let cc = out
            .get_header(header::CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        assert_eq!(cc, "public, max-age=60");
        let cookie = out
            .get_header(header::SET_COOKIE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        assert!(cookie.contains("a=1"));
    }

    #[test]
    fn css_response_is_rewritten_and_content_type_set() {
        let settings = create_test_settings();
        let css = "body{background:url(https://cdn.example/bg.png)}";
        let beresp = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "text/css")
            .with_body(css);
        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let mut out = finalize(&settings, &req, "https://cdn.example/bg.png", beresp)
            .expect("finalize should succeed");
        let body = out.take_body_str();
        assert!(body.contains("/first-party/proxy?tsurl="), "{}", body);
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_str()
            .expect("Content-Type should be valid UTF-8");
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

        let beresp = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .with_body(html);

        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let mut out = finalize(
            &settings,
            &req,
            "https://cdn.example.com:9443/creatives/300x250.html",
            beresp,
        )
        .expect("should finalize HTML response with non-standard port URL");

        let body = out.take_body_str();

        // Port 9443 should be preserved (URL-encoded as %3A9443)
        assert!(
            body.contains("cdn.example.com%3A9443"),
            "Port 9443 should be preserved in rewritten URLs. Body:\n{}",
            body
        );
    }

    #[test]
    fn image_accept_sets_generic_content_type_when_missing() {
        let settings = create_test_settings();
        let beresp = Response::from_status(StatusCode::OK).with_body("PNG");
        let mut req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        req.set_header(HEADER_ACCEPT, "image/*");
        let out = finalize(&settings, &req, "https://cdn.example/pixel.gif", beresp)
            .expect("finalize should succeed");
        // Since CT was missing and Accept indicates image, it should set generic image/*
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_str()
            .expect("Content-Type should be valid UTF-8");
        assert_eq!(ct, "image/*");
    }

    #[test]
    fn non_image_non_html_passthrough() {
        let settings = create_test_settings();
        let beresp = Response::from_status(StatusCode::ACCEPTED)
            .with_header(header::CONTENT_TYPE, "application/json")
            .with_body("{\"ok\":true}");
        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let mut out = finalize(&settings, &req, "https://api.example/ok", beresp)
            .expect("finalize should succeed");
        // Should not rewrite, preserve status and content-type
        assert_eq!(out.get_status(), StatusCode::ACCEPTED);
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_str()
            .expect("Content-Type should be valid UTF-8");
        assert_eq!(ct, "application/json");
        let body = out.take_body_str();
        assert_eq!(body, "{\"ok\":true}");
    }

    #[test]
    fn html_gzip_response_is_processed_with_compression_preserved() {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::{Read, Write};

        let settings = create_test_settings();
        let html = r#"<html><body><img src="https://cdn.example/a.png"></body></html>"#;

        // Gzip compress the HTML
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(html.as_bytes())
            .expect("gzip write should succeed");
        let compressed = encoder.finish().expect("gzip finish should succeed");

        let beresp = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .with_header(header::CONTENT_ENCODING, "gzip")
            .with_body(compressed);

        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/a.png", beresp)
            .expect("finalize should process and succeed");

        // Content-Encoding should be preserved (gzip in -> gzip out)
        let ce = out
            .get_header(header::CONTENT_ENCODING)
            .expect("Content-Encoding should be preserved")
            .to_str()
            .expect("Content-Encoding should be valid UTF-8");
        assert_eq!(ce, "gzip");

        let ct = out
            .get_header(header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_str()
            .expect("Content-Type should be valid UTF-8");
        assert_eq!(ct, "text/html; charset=utf-8");

        // Decompress output to verify content was rewritten
        let compressed_output = out.into_body().into_bytes();
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
        use brotli::enc::writer::CompressorWriter;
        use brotli::Decompressor;
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

        let beresp = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "text/css")
            .with_header(header::CONTENT_ENCODING, "br")
            .with_body(compressed);

        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let out = finalize(&settings, &req, "https://cdn.example/bg.png", beresp)
            .expect("finalize should process brotli and succeed");

        // Content-Encoding should be preserved (br in -> br out)
        let ce = out
            .get_header(header::CONTENT_ENCODING)
            .expect("Content-Encoding should be preserved")
            .to_str()
            .expect("Content-Encoding should be valid UTF-8");
        assert_eq!(ce, "br");

        let ct = out
            .get_header(header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_str()
            .expect("Content-Type should be valid UTF-8");
        assert_eq!(ct, "text/css; charset=utf-8");

        // Decompress output to verify content was rewritten
        let compressed_output = out.into_body().into_bytes();
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

        let beresp = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .with_body(html);

        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let mut out = finalize(&settings, &req, "https://cdn.example/a.png", beresp)
            .expect("finalize should succeed");

        // No Content-Encoding since input was uncompressed
        assert!(
            out.get_header(header::CONTENT_ENCODING).is_none(),
            "Content-Encoding should not be set for uncompressed input"
        );

        let ct = out
            .get_header(header::CONTENT_TYPE)
            .expect("Content-Type header should be present")
            .to_str()
            .expect("Content-Type should be valid UTF-8");
        assert_eq!(ct, "text/html; charset=utf-8");

        let body = out.take_body_str();
        assert!(
            body.contains("/first-party/proxy?tsurl="),
            "HTML should be rewritten: {}",
            body
        );
    }

    // --- Platform HTTP client integration ---

    #[tokio::test]
    async fn proxy_request_calls_platform_http_client_send() {
        use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
        use std::sync::Arc;

        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/");

        let result = proxy_request(
            &settings,
            req,
            ProxyRequestConfig {
                target_url: "https://example.com/resource",
                follow_redirects: false,
                forward_synthetic_id: false,
                body: None,
                headers: Vec::new(),
                copy_request_headers: false,
                stream_passthrough: false,
                allowed_domains: &[],
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
        // The guard at proxy_with_redirects checks `!allowed_domains.is_empty()`
        // before calling is_host_allowed, so no host is ever blocked when the
        // list is empty. Verify the combined condition is false for any host.
        let allowed: [String; 0] = [];
        let would_block =
            !allowed.is_empty() && !allowed.iter().any(|p| is_host_allowed("evil.com", p));
        assert!(
            !would_block,
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
    // NOTE: A test for Nth-hop redirect blocking (i.e. exercising the
    // `redirect_is_permitted` check that fires *after* receiving a 302
    // response) requires a Viceroy backend fixture that returns a redirect.
    // That infrastructure is not available here. The unit tests above for
    // `redirect_is_permitted` and `ip_literal_blocked_by_domain_allowlist`
    // cover the blocking logic used at every hop.

    #[tokio::test]
    async fn proxy_initial_target_blocked_by_allowlist() {
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
        let req = Request::new(Method::GET, url);
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
    }
}
