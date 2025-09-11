use crate::http_util::decode_url;
use error_stack::{Report, ResultExt};
use fastly::http::header;
use fastly::{Request, Response};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_REFERER,
    HEADER_USER_AGENT, HEADER_X_FORWARDED_FOR,
};
use crate::error::TrustedServerError;
use crate::settings::Settings;

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
    // Parse query string
    let req_url = req.get_url_str();
    log::info!("proxy: req_url={}", req_url);

    let parsed = url::Url::parse(req_url).change_context(TrustedServerError::Proxy {
        message: "Invalid proxy URL".to_string(),
    })?;
    let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();

    // Extract required param
    let Some(u_param) = params.get("u").cloned() else {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "missing u parameter".to_string(),
        }));
    };

    // Decrypt token
    let decoded = match decode_url(settings, &u_param) {
        Some(s) => s,
        None => {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "invalid token in u".to_string(),
            }));
        }
    };

    let target_url = if decoded.starts_with("//") {
        format!("https:{}", decoded)
    } else {
        decoded
    };

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
    let backend_name = crate::backend::ensure_origin_backend(&scheme, host, u.port())?;

    // Build proxied request with selected headers
    let mut proxy_req = Request::new(req.get_method().clone(), &target_url);
    for header_name in [
        HEADER_USER_AGENT,
        HEADER_ACCEPT,
        HEADER_ACCEPT_LANGUAGE,
        HEADER_ACCEPT_ENCODING,
        HEADER_REFERER,
        HEADER_X_FORWARDED_FOR,
    ] {
        if let Some(v) = req.get_header(&header_name) {
            proxy_req.set_header(&header_name, v);
        }
    }

    let mut beresp = proxy_req
        .send(&backend_name)
        .change_context(TrustedServerError::Proxy {
            message: "Failed to proxy".to_string(),
        })?;

    // Determine content-type
    let ct = beresp
        .get_header(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ct.contains("text/html") {
        // HTML: rewrite and serve as HTML
        let status = beresp.get_status();
        let body = beresp.take_body_str();
        let rewritten = crate::creative::rewrite_creative_html(&body, settings);
        return Ok(Response::from_status(status)
            .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .with_body(rewritten));
    }

    if ct.contains("text/css") {
        // CSS: rewrite url(...) references in stylesheets
        let status = beresp.get_status();
        let body = beresp.take_body_str();
        let rewritten = crate::creative::rewrite_css_body(&body, settings);
        return Ok(Response::from_status(status)
            .with_header(header::CONTENT_TYPE, "text/css; charset=utf-8")
            .with_body(rewritten));
    }

    // Image handling: set generic content-type if missing and log pixel heuristics
    if ct.starts_with("image/")
        || req
            .get_header(HEADER_ACCEPT)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_ascii_lowercase().contains("image/"))
            .unwrap_or(false)
    {
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
    }

    Ok(beresp)
}

#[cfg(test)]
mod tests {
    use super::handle_first_party_proxy;
    use crate::error::{IntoHttpResponse, TrustedServerError};
    use crate::test_support::tests::create_test_settings;
    use error_stack::Report;
    use fastly::http::{Method, StatusCode};
    use fastly::Request;

    #[tokio::test]
    async fn proxy_missing_param_returns_400() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/first-party/proxy");
        let err: Report<TrustedServerError> = handle_first_party_proxy(&settings, req)
            .await
            .err()
            .expect("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn proxy_invalid_base64_in_u_returns_400() {
        let settings = create_test_settings();
        // invalid base64 in u; should 400
        let req = Request::new(
            Method::GET,
            "https://example.com/first-party/proxy?u=@@notb64@@",
        );
        let err: Report<TrustedServerError> = handle_first_party_proxy(&settings, req)
            .await
            .err()
            .expect("expected error");
        assert_eq!(err.current_context().status_code(), StatusCode::BAD_GATEWAY);
    }
}
