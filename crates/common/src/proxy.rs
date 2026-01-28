use crate::http_util::compute_encrypted_sha256_token;
use error_stack::{Report, ResultExt};
use fastly::http::{header, HeaderValue, Method, StatusCode};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
    HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
};
use crate::error::TrustedServerError;
use crate::settings::Settings;
use crate::synthetic::get_synthetic_id;

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
    /// When true, stream the origin response without HTML/CSS rewrites.
    pub stream_passthrough: bool,
}

impl<'a> ProxyRequestConfig<'a> {
    /// Build a proxy configuration that follows redirects and forwards the synthetic ID.
    #[must_use]
    pub fn new(target_url: &'a str) -> Self {
        Self {
            target_url,
            follow_redirects: true,
            forward_synthetic_id: true,
            body: None,
            headers: Vec::new(),
            stream_passthrough: false,
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

    /// Enable streaming passthrough (no HTML/CSS rewrites).
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.stream_passthrough = true;
        self
    }
}

/// Copy a curated set of request headers to a proxied request.
fn copy_proxy_forward_headers(src: &Request, dst: &mut Request) {
    for header_name in [
        HEADER_USER_AGENT,
        HEADER_ACCEPT,
        HEADER_ACCEPT_LANGUAGE,
        HEADER_ACCEPT_ENCODING,
        HEADER_REFERER,
        HEADER_X_FORWARDED_FOR,
    ] {
        if let Some(v) = src.get_header(&header_name) {
            dst.set_header(&header_name, v);
        }
    }
}

// Transform the backend response into the final response sent to the client.
// Handles HTML and CSS rewrites and image content-type normalization.
fn rebuild_text_response(beresp: Response, content_type: &'static str, body: String) -> Response {
    let status = beresp.get_status();
    let headers: Vec<(header::HeaderName, HeaderValue)> = beresp
        .get_headers()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut resp = Response::from_status(status);
    for (name, value) in headers {
        if name == header::CONTENT_LENGTH || name == header::CONTENT_TYPE {
            continue;
        }
        resp.set_header(name, value);
    }
    resp.set_header(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    resp.set_body(body);
    resp
}

fn finalize_proxied_response(
    settings: &Settings,
    req: &Request,
    target_url: &str,
    mut beresp: Response,
) -> Response {
    // Determine content-type from response headers
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

    if ct.contains("text/html") {
        // HTML: rewrite and serve as HTML (safe to read as string)
        let body = beresp.take_body_str();
        let rewritten = crate::creative::rewrite_creative_html(settings, &body);
        return rebuild_text_response(beresp, "text/html; charset=utf-8", rewritten);
    }

    if ct.contains("text/css") {
        // CSS: rewrite url(...) references in stylesheets (safe to read as string)
        let body = beresp.take_body_str();
        let rewritten = crate::creative::rewrite_css_body(settings, &body);
        return rebuild_text_response(beresp, "text/css; charset=utf-8", rewritten);
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

        return beresp;
    }

    // Passthrough for non-text, non-image responses
    beresp
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
    req: Request,
    config: ProxyRequestConfig<'_>,
) -> Result<Response, Report<TrustedServerError>> {
    let ProxyRequestConfig {
        target_url,
        follow_redirects,
        forward_synthetic_id,
        body,
        headers,
        stream_passthrough,
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
        follow_redirects,
        body.as_deref(),
        &headers,
        stream_passthrough,
    )
    .await
}

fn append_synthetic_id(req: &Request, target_url_parsed: &mut url::Url) {
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

async fn proxy_with_redirects(
    settings: &Settings,
    req: &Request,
    target_url_parsed: url::Url,
    follow_redirects: bool,
    body: Option<&[u8]>,
    headers: &[(header::HeaderName, HeaderValue)],
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

        let backend_name = crate::backend::ensure_origin_backend(&scheme, host, parsed_url.port())?;

        let mut proxy_req = Request::new(current_method.clone(), &current_url);
        copy_proxy_forward_headers(req, &mut proxy_req);
        if let Some(body_bytes) = body {
            proxy_req.set_body(body_bytes.to_vec());
        }

        for (name, value) in headers {
            proxy_req.set_header(name.clone(), value.clone());
        }

        let beresp = proxy_req
            .send(&backend_name)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to proxy".to_string(),
            })?;

        if !follow_redirects {
            return Ok(if stream_passthrough {
                finalize_proxied_response_streaming(req, &current_url, beresp)
            } else {
                finalize_proxied_response(settings, req, &current_url, beresp)
            });
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
            return Ok(if stream_passthrough {
                finalize_proxied_response_streaming(req, &current_url, beresp)
            } else {
                finalize_proxied_response(settings, req, &current_url, beresp)
            });
        }

        let Some(location) = beresp
            .get_header(header::LOCATION)
            .and_then(|h| h.to_str().ok())
            .filter(|value| !value.is_empty())
        else {
            return Ok(if stream_passthrough {
                finalize_proxied_response_streaming(req, &current_url, beresp)
            } else {
                finalize_proxied_response(settings, req, &current_url, beresp)
            });
        };

        if redirect_attempt == MAX_REDIRECTS {
            log::warn!(
                "redirect limit reached for {}; returning redirect response",
                current_url
            );
            return Ok(finalize_proxied_response(
                settings,
                req,
                &current_url,
                beresp,
            ));
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
            return Ok(if stream_passthrough {
                finalize_proxied_response_streaming(req, &current_url, beresp)
            } else {
                finalize_proxied_response(settings, req, &current_url, beresp)
            });
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
pub async fn handle_first_party_proxy(
    settings: &Settings,
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
            forward_synthetic_id: true,
            body: None,
            headers: Vec::new(),
            stream_passthrough: false,
        },
    )
    .await
}

/// First-party click redirect endpoint.
///
/// Accepts the same parameters as the proxy scheme, but instead of proxying the
/// content, it validates the URL and issues a 302 redirect to the reconstructed
/// target URL. This avoids parsing/downloading the content and lets the browser
/// navigate directly to the destination under first-party control.
pub async fn handle_first_party_click(
    settings: &Settings,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let SignedTarget {
        target_url: full_for_token,
        tsurl,
        had_params,
    } = reconstruct_and_validate_signed_target(settings, req.get_url_str())?;

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
        .get_header(HEADER_USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let referer = req
        .get_header(HEADER_REFERER)
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
    Ok(Response::from_status(fastly::http::StatusCode::FOUND)
        .with_header(header::LOCATION, &redirect_target)
        .with_header(header::CACHE_CONTROL, "no-store, private"))
}

/// Sign an arbitrary asset URL so creatives can request first-party proxying at runtime.
/// Supports POST JSON and GET query (`?url=`) payloads. Embeds a short-lived `tsexp` (30s)
/// in the signature so the signed URL cannot be replayed indefinitely. Returns JSON
/// `{ href, base }` where `href` is the signed `/first-party/proxy?...` path and `base`
/// is the normalized clear URL.
pub async fn handle_first_party_proxy_sign(
    settings: &Settings,
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
pub async fn handle_first_party_proxy_rebuild(
    settings: &Settings,
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
    if expected != sig {
        return Err(Report::new(TrustedServerError::Proxy {
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
        copy_proxy_forward_headers, handle_first_party_click, handle_first_party_proxy,
        handle_first_party_proxy_rebuild, handle_first_party_proxy_sign,
        reconstruct_and_validate_signed_target, ProxyRequestConfig,
    };
    use crate::error::{IntoHttpResponse, TrustedServerError};
    use crate::test_support::tests::create_test_settings;
    use crate::{
        constants::{
            HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
            HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
        },
        creative,
    };
    use error_stack::Report;
    use fastly::http::{header, HeaderValue, Method, StatusCode};
    use fastly::{Request, Response};

    #[tokio::test]
    async fn proxy_missing_param_returns_400() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/first-party/proxy");
        let err: Report<TrustedServerError> = handle_first_party_proxy(&settings, req)
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
        let err: Report<TrustedServerError> = handle_first_party_proxy(&settings, req)
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
        let mut resp = handle_first_party_proxy_sign(&settings, req)
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
        let err: Report<TrustedServerError> = handle_first_party_proxy_sign(&settings, req)
            .await
            .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn proxy_request_config_supports_streaming_and_headers() {
        let cfg = ProxyRequestConfig::new("https://example.com/asset")
            .with_body(vec![1, 2, 3])
            .with_header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            )
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
            cfg.stream_passthrough,
            "should enable streaming passthrough"
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
    async fn click_missing_params_returns_400() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://edge.example/first-party/click");
        let err: Report<TrustedServerError> = handle_first_party_click(&settings, req)
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
        let resp = handle_first_party_click(&settings, req)
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
    async fn click_appends_synthetic_id_when_present() {
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
        req.set_header(crate::constants::HEADER_X_SYNTHETIC_ID, "synthetic-123");

        let resp = handle_first_party_click(&settings, req)
            .await
            .expect("should redirect");

        let loc = resp
            .get_header(header::LOCATION)
            .and_then(|h| h.to_str().ok())
            .unwrap();
        let parsed = url::Url::parse(loc).expect("should parse location");
        let mut pairs: std::collections::HashMap<String, String> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(pairs.remove("foo").as_deref(), Some("1"));
        assert_eq!(
            pairs.remove("synthetic_id").as_deref(),
            Some("synthetic-123")
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
        let mut req = Request::new(
            Method::POST,
            "https://edge.example/first-party/proxy-rebuild",
        );
        req.set_body(serde_json::to_string(&body).unwrap());
        let mut resp = handle_first_party_proxy_rebuild(&settings, req)
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
        let err: Report<TrustedServerError> = handle_first_party_proxy(&settings, req)
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
        let err: Report<TrustedServerError> = handle_first_party_proxy(&settings, req)
            .await
            .expect_err("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn header_copy_copies_curated_set() {
        let mut src = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        src.set_header(HEADER_USER_AGENT, "UA/1.0");
        src.set_header(HEADER_ACCEPT, "image/*");
        src.set_header(HEADER_ACCEPT_LANGUAGE, "en-US");
        src.set_header(HEADER_ACCEPT_ENCODING, "gzip");
        src.set_header(HEADER_REFERER, "https://pub.example/page");
        src.set_header(HEADER_X_FORWARDED_FOR, "203.0.113.1");

        let mut dst = Request::new(Method::GET, "https://cdn.example/a.png");
        copy_proxy_forward_headers(&src, &mut dst);

        assert_eq!(
            dst.get_header(HEADER_USER_AGENT).unwrap().to_str().unwrap(),
            "UA/1.0"
        );
        assert_eq!(
            dst.get_header(HEADER_ACCEPT).unwrap().to_str().unwrap(),
            "image/*"
        );
        assert_eq!(
            dst.get_header(HEADER_ACCEPT_LANGUAGE)
                .unwrap()
                .to_str()
                .unwrap(),
            "en-US"
        );
        assert_eq!(
            dst.get_header(HEADER_ACCEPT_ENCODING)
                .unwrap()
                .to_str()
                .unwrap(),
            "gzip"
        );
        assert_eq!(
            dst.get_header(HEADER_REFERER).unwrap().to_str().unwrap(),
            "https://pub.example/page"
        );
        assert_eq!(
            dst.get_header(HEADER_X_FORWARDED_FOR)
                .unwrap()
                .to_str()
                .unwrap(),
            "203.0.113.1"
        );
    }

    #[tokio::test]
    async fn click_sets_cache_control_no_store_private() {
        let settings = create_test_settings();
        let clear = "https://cdn.example/landing.html?x=1";
        let first_party = creative::build_click_url(&settings, clear);
        let req = Request::new(Method::GET, format!("https://edge.example{}", first_party));
        let resp = handle_first_party_click(&settings, req)
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
        let out = finalize(&settings, &req, "https://cdn.example/a.png", beresp);
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
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
        let mut out = finalize(&settings, &req, "https://cdn.example/bg.png", beresp);
        let body = out.take_body_str();
        assert!(body.contains("/first-party/proxy?tsurl="), "{}", body);
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "text/css; charset=utf-8");
    }

    #[test]
    fn image_accept_sets_generic_content_type_when_missing() {
        let settings = create_test_settings();
        let beresp = Response::from_status(StatusCode::OK).with_body("PNG");
        let mut req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        req.set_header(HEADER_ACCEPT, "image/*");
        let out = finalize(&settings, &req, "https://cdn.example/pixel.gif", beresp);
        // Since CT was missing and Accept indicates image, it should set generic image/*
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "image/*");
    }

    #[test]
    fn non_image_non_html_passthrough() {
        let settings = create_test_settings();
        let beresp = Response::from_status(StatusCode::ACCEPTED)
            .with_header(header::CONTENT_TYPE, "application/json")
            .with_body("{\"ok\":true}");
        let req = Request::new(Method::GET, "https://edge.example/first-party/proxy");
        let mut out = finalize(&settings, &req, "https://api.example/ok", beresp);
        // Should not rewrite, preserve status and content-type
        assert_eq!(out.get_status(), StatusCode::ACCEPTED);
        let ct = out
            .get_header(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "application/json");
        let body = out.take_body_str();
        assert_eq!(body, "{\"ok\":true}");
    }
}
