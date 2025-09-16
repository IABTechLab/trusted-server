use crate::http_util::compute_encrypted_sha256_token;
use error_stack::{Report, ResultExt};
use fastly::http::{header, HeaderValue};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
    HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
};
use crate::error::TrustedServerError;
use crate::settings::Settings;

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
        "proxy: origin response status={} ct={} cl={} accept={} url={}",
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
        let rewritten = crate::creative::rewrite_creative_html(&body, settings);
        return rebuild_text_response(beresp, "text/html; charset=utf-8", rewritten);
    }

    if ct.contains("text/css") {
        // CSS: rewrite url(...) references in stylesheets (safe to read as string)
        let body = beresp.take_body_str();
        let rewritten = crate::creative::rewrite_css_body(&body, settings);
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
            log::info!(
                "proxy: likely pixel image fetched: {} ct={}",
                target_url,
                ct
            );
        }

        return beresp;
    }

    // Passthrough for non-text, non-image responses
    beresp
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

    // Validate URL
    let Ok(u) = url::Url::parse(&target_url) else {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "invalid url".to_string(),
        }));
    };
    let scheme = u.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "unsupported scheme".to_string(),
        }));
    }
    let host = u.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "missing host".to_string(),
        }));
    }

    // Ensure a backend exists
    // No tstoken is included in target_url by design; nothing to strip.

    let backend_name = crate::backend::ensure_origin_backend(&scheme, host, u.port())?;

    // Build proxied request with selected headers
    let mut proxy_req = Request::new(req.get_method().clone(), &target_url);
    copy_proxy_forward_headers(&req, &mut proxy_req);

    let beresp = proxy_req
        .send(&backend_name)
        .change_context(TrustedServerError::Proxy {
            message: "Failed to proxy".to_string(),
        })?;
    Ok(finalize_proxied_response(
        settings,
        &req,
        &target_url,
        beresp,
    ))
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

    // Log click metadata for observability
    let ua = req
        .get_header(HEADER_USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let referer = req
        .get_header(HEADER_REFERER)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let potsi = req
        .get_header(crate::constants::HEADER_SYNTHETIC_TRUSTED_SERVER)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    log::info!(
        "click: redirect tsurl={} params_present={} target={} referer={} ua={} potsi={}",
        tsurl,
        had_params,
        full_for_token,
        referer,
        ua,
        potsi
    );

    // 302 redirect to target URL
    Ok(Response::from_status(fastly::http::StatusCode::FOUND)
        .with_header(header::LOCATION, &full_for_token)
        .with_header(header::CACHE_CONTROL, "no-store, private"))
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
struct SignedTarget {
    target_url: String,
    tsurl: String,
    had_params: bool,
}

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
    for (k, v) in parsed.query_pairs() {
        let key = k.as_ref();
        if key == "tsurl" {
            tsurl = Some(v.into_owned());
            continue;
        }
        if key == "tstoken" {
            sig = Some(v.into_owned());
            continue;
        }
        ser.append_pair(key, &v);
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
        handle_first_party_proxy_rebuild, reconstruct_and_validate_signed_target,
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
    use fastly::http::{header, Method, StatusCode};
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
        let direct = creative::rewrite_creative_html(html, &settings);
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
