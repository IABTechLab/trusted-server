use crate::http_util::{compute_encrypted_sha256_token, ct_str_eq};
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{request_builder as edge_request_builder, Uri as EdgeUri};
use error_stack::{Report, ResultExt};
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
    HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
};
use crate::creative::{CreativeCssProcessor, CreativeHtmlProcessor};
use crate::error::TrustedServerError;
use crate::platform::{PlatformBackendSpec, PlatformHttpRequest, RuntimeServices};
use crate::settings::Settings;
use crate::streaming_processor::{Compression, PipelineConfig, StreamProcessor, StreamingPipeline};
use crate::synthetic::get_synthetic_id;

/// Chunk size used for streaming content through the rewrite pipeline.
const STREAMING_CHUNK_SIZE: usize = 8192;

fn body_as_reader(body: EdgeBody) -> Cursor<bytes::Bytes> {
    Cursor::new(body.into_bytes())
}

/// Headers copied from the original client request to the upstream proxy request.
const PROXY_FORWARD_HEADERS: [header::HeaderName; 5] = [
    HEADER_USER_AGENT,
    HEADER_ACCEPT,
    HEADER_ACCEPT_LANGUAGE,
    HEADER_REFERER,
    HEADER_X_FORWARDED_FOR,
];

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
    /// Whether to append the caller's synthetic ID as a query param.
    pub forward_synthetic_id: bool,
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
    /// Build a proxy configuration that follows redirects and forwards the synthetic ID.
    ///
    /// `allowed_domains` defaults to `&[]` (open mode). Override it for the
    /// first-party proxy by setting `allowed_domains` directly.
    #[must_use]
    pub fn new(target_url: &'a str) -> Self {
        Self {
            target_url,
            follow_redirects: true,
            forward_synthetic_id: true,
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

fn finalize_proxied_response(
    settings: &Settings,
    req: &Request<EdgeBody>,
    target_url: &str,
    mut beresp: Response<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // Determine content-type and content-encoding from response headers
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
        .headers()
        .get(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("image/"))
        .unwrap_or(false);

    if ct.starts_with("image/") || req_accept_images {
        if beresp.headers().get(header::CONTENT_TYPE).is_none() {
            beresp
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/*"));
        }

        // Heuristics to log likely tracking pixels without altering response
        let mut is_pixel = false;
        if let Some(cl) = beresp
            .headers()
            .get(header::CONTENT_LENGTH)
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
    req: &Request<EdgeBody>,
    target_url: &str,
    mut beresp: Response<EdgeBody>,
) -> Response<EdgeBody> {
    let status_code = beresp.status().as_u16();
    let ct_raw = beresp
        .headers()
        .get(header::CONTENT_TYPE)
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
        .headers()
        .get(HEADER_ACCEPT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("image/"))
        .unwrap_or(false);

    if ct.starts_with("image/") || req_accept_images {
        if beresp.headers().get(header::CONTENT_TYPE).is_none() {
            beresp
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/*"));
        }

        let mut is_pixel = false;
        if let Some(cl) = beresp
            .headers()
            .get(header::CONTENT_LENGTH)
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

struct ProxyRequestHeaders<'a> {
    additional_headers: &'a [(header::HeaderName, HeaderValue)],
    copy_request_headers: bool,
    services: &'a RuntimeServices,
}

struct ProxyRedirectPolicy<'a> {
    follow_redirects: bool,
    stream_passthrough: bool,
    allowed_domains: &'a [String],
}

/// Proxy a request to a clear target URL while reusing creative rewrite logic.
///
/// This forwards a curated header set, follows redirects when enabled, and can append
/// the caller's synthetic ID as a `synthetic_id` query parameter to the target URL.
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
        forward_synthetic_id,
        body,
        headers,
        copy_request_headers,
        stream_passthrough,
        allowed_domains,
    } = config;

    let mut target_url_parsed = url::Url::parse(target_url).map_err(|_| {
        Report::new(TrustedServerError::Proxy {
            message: "invalid url".to_string(),
        })
    })?;

    if forward_synthetic_id {
        append_synthetic_id(&req, &mut target_url_parsed);
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
        },
    )
    .await
}

fn append_synthetic_id(req: &Request<EdgeBody>, target_url_parsed: &mut url::Url) {
    let synthetic_id_param = match get_synthetic_id(req) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("failed to extract synthetic ID for forwarding: {:?}", e);
            None
        }
    };

    if let Some(synthetic_id) = synthetic_id_param {
        let mut pairs: Vec<(String, String)> = target_url_parsed
            .query_pairs()
            .filter(|(k, _)| k.as_ref() != "synthetic_id")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        pairs.push(("synthetic_id".to_string(), synthetic_id));

        target_url_parsed.set_query(None);
        if !pairs.is_empty() {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in &pairs {
                serializer.append_pair(k, v);
            }
            let query_str = serializer.finish();
            target_url_parsed.set_query(Some(&query_str));
        }

        log::debug!(
            "forwarding synthetic_id to origin url {}",
            target_url_parsed.as_str()
        );
    } else {
        log::debug!("no synthetic_id to forward to origin");
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
                if let Some(v) = req.headers().get(&header_name) {
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
            forward_synthetic_id: true,
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
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let SignedTarget {
        target_url: full_for_token,
        tsurl,
        had_params,
    } = reconstruct_and_validate_signed_target(settings, &req.uri().to_string())?;

    let synthetic_id = match get_synthetic_id(&req) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("failed to extract synthetic ID for forwarding: {:?}", e);
            None
        }
    };

    let mut redirect_target = full_for_token.clone();
    if let Some(ref synthetic_id_value) = synthetic_id {
        match url::Url::parse(&redirect_target) {
            Ok(mut url) => {
                let mut pairs: Vec<(String, String)> = url
                    .query_pairs()
                    .filter(|(k, _)| k.as_ref() != "synthetic_id")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                pairs.push(("synthetic_id".to_string(), synthetic_id_value.clone()));

                url.set_query(None);
                if !pairs.is_empty() {
                    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                    for (k, v) in &pairs {
                        serializer.append_pair(k, v);
                    }
                    let query_str = serializer.finish();
                    url.set_query(Some(&query_str));
                }

                let final_target = url.to_string();
                log::debug!("forwarding synthetic_id to target url {}", final_target);
                redirect_target = final_target;
            }
            Err(e) => {
                log::warn!(
                    "failed to parse target url for synthetic forwarding: {:?}",
                    e
                );
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
        "redirect tsurl={} params_present={} target={} referer={} ua={} synthetic_id={}",
        tsurl,
        had_params,
        redirect_target,
        referer,
        ua,
        synthetic_id.as_deref().unwrap_or("")
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
        let body_bytes = req.into_body().into_bytes();
        if body_bytes.len() > 65536 {
            return Err(Report::new(TrustedServerError::RequestTooLarge {
                message: format!(
                    "payload size {} exceeds limit of 65536 bytes",
                    body_bytes.len()
                ),
            }));
        }
        let body = String::from_utf8(body_bytes.to_vec()).change_context(
            TrustedServerError::InvalidUtf8 {
                message: "first-party sign request body should be valid UTF-8".to_string(),
            },
        )?;
        serde_json::from_str::<ProxySignReq>(&body).change_context(TrustedServerError::Proxy {
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
        let body_bytes = req.into_body().into_bytes();
        if body_bytes.len() > 65536 {
            return Err(Report::new(TrustedServerError::RequestTooLarge {
                message: format!(
                    "payload size {} exceeds limit of 65536 bytes",
                    body_bytes.len()
                ),
            }));
        }
        let body = String::from_utf8(body_bytes.to_vec()).change_context(
            TrustedServerError::InvalidUtf8 {
                message: "first-party rebuild request body should be valid UTF-8".to_string(),
            },
        )?;
        serde_json::from_str::<ProxyRebuildReq>(&body).change_context(
            TrustedServerError::Proxy {
                message: "invalid JSON".to_string(),
            },
        )?
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
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::{
        handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
        handle_first_party_proxy_sign, is_host_allowed, proxy_request, rebuild_response_with_body,
        reconstruct_and_validate_signed_target, redirect_is_permitted, ProxyRequestConfig,
    };
    use crate::constants::HEADER_ACCEPT;
    use crate::creative;
    use crate::error::{IntoHttpResponse, TrustedServerError};
    use crate::platform::test_support::{build_services_with_http_client, noop_services};
    use crate::platform::{
        PlatformError, PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest,
        PlatformResponse, PlatformSelectResult,
    };
    use crate::test_support::tests::create_test_settings;
    use edgezero_core::body::Body as EdgeBody;
    use error_stack::Report;
    use http::{header, HeaderValue, Method, Request as HttpRequest, Response, StatusCode};

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

    fn response_body_string(response: http::Response<EdgeBody>) -> String {
        String::from_utf8(response.into_body().into_bytes().to_vec())
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

    #[tokio::test]
    async fn proxy_missing_param_returns_400() {
        let settings = create_test_settings();
        let req = build_http_request(Method::GET, "https://example.com/first-party/proxy");
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
        let req = build_http_request(
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
    }

    #[tokio::test]
    async fn proxy_sign_rejects_invalid_url() {
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
    }

    #[tokio::test]
    async fn proxy_sign_preserves_non_standard_port() {
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
        assert!(
            cfg.forward_synthetic_id,
            "should forward synthetic id by default"
        );
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
        let req = build_http_request(Method::GET, "https://edge.example/first-party/click");
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
    }

    #[tokio::test]
    async fn click_appends_synthetic_id_when_present() {
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
        let valid_synthetic_id = crate::test_support::tests::VALID_SYNTHETIC_ID;
        req.headers_mut().insert(
            crate::constants::HEADER_X_SYNTHETIC_ID,
            HeaderValue::from_static(valid_synthetic_id),
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
        assert_eq!(
            pairs.remove("synthetic_id").as_deref(),
            Some(valid_synthetic_id)
        );
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
        let req = build_http_request(Method::GET, format!("https://edge.example{}", first_party));
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
        let req = build_http_request(Method::GET, &url);
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
        let req = build_http_request(Method::GET, format!("https://edge.example{}", first_party));
        let resp = handle_first_party_click(&settings, &noop_services(), req)
            .await
            .expect("should redirect");
        assert_eq!(resp.status(), StatusCode::FOUND);
        let cc = response_header(&resp, header::CACHE_CONTROL).unwrap_or("");
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
    fn image_accept_sets_generic_content_type_when_missing() {
        let settings = create_test_settings();
        let beresp = build_http_response(StatusCode::OK, EdgeBody::from("PNG"));
        let mut req = build_http_request(Method::GET, "https://edge.example/first-party/proxy");
        req.headers_mut()
            .insert(HEADER_ACCEPT, HeaderValue::from_static("image/*"));
        let out = finalize(&settings, &req, "https://cdn.example/pixel.gif", beresp)
            .expect("finalize should succeed");
        // Since CT was missing and Accept indicates image, it should set generic image/*
        let ct = response_header(&out, header::CONTENT_TYPE)
            .expect("Content-Type header should be present");
        assert_eq!(ct, "image/*");
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

    #[tokio::test]
    async fn proxy_request_calls_platform_http_client_send() {
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

    #[tokio::test]
    async fn proxy_request_allows_open_mode_when_settings_allowlist_is_non_empty() {
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
                forward_synthetic_id: false,
                body: None,
                headers: Vec::new(),
                copy_request_headers: false,
                stream_passthrough: false,
                allowed_domains: &[],
            },
            &services,
        )
        .await
        .expect("open mode should ignore settings.proxy.allowed_domains");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body_string(response), "ok");
    }

    #[tokio::test]
    async fn proxy_request_uses_config_allowlist_for_redirect_hops() {
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
                forward_synthetic_id: false,
                body: None,
                headers: Vec::new(),
                copy_request_headers: false,
                stream_passthrough: false,
                allowed_domains: &[],
            },
            &services,
        )
        .await
        .expect("open mode should allow redirect hops outside settings allowlist");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body_string(response), "redirected");
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
    // The unit tests above cover the host-matching logic itself. The tests
    // below verify that proxy_request threads config.allowed_domains through
    // the initial target check and redirect hops.

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
    }

    #[tokio::test]
    async fn sign_rejects_oversized_body() {
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
    }

    #[tokio::test]
    async fn rebuild_rejects_oversized_body() {
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
    }
}
