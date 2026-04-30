//! Compatibility bridge between `fastly` SDK types and `http` crate types.
//!
//! All items in this module are temporary scaffolding created in PR 11 and
//! scheduled for deletion in PR 15. Do not add new callers after PR 13.
//!
//! # PR 15 removal target

use edgezero_core::body::Body as EdgeBody;
use fastly::http::header;

use crate::constants::INTERNAL_HEADERS;
use crate::http_util::SPOOFABLE_FORWARDED_HEADERS;

fn build_http_request(req: &fastly::Request, body: EdgeBody) -> http::Request<EdgeBody> {
    let uri: http::Uri = req.get_url_str().parse().unwrap_or_else(|_| {
        log::warn!(
            "Failed to parse request URL '{}'; falling back to '/'",
            req.get_url_str()
        );
        http::Uri::from_static("/")
    });

    let mut builder = http::Request::builder()
        .method(req.get_method().clone())
        .uri(uri);

    for (name, value) in req.get_headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }

    // Cannot fail: URI is always valid (parsed above or the "/" fallback),
    // and Fastly pre-validates all method and header values.
    builder
        .body(body)
        .expect("should build http request from fastly request")
}

/// Convert a borrowed `fastly::Request` into an `http::Request<EdgeBody>` for reading.
///
/// Headers are copied; the body is empty.
///
/// # PR 15 removal target
///
/// # Panics
///
/// Does not panic in practice — URL parse failure falls back to `"/"` (logged
/// as a warning), and the subsequent `builder.body()` cannot fail given a valid
/// method and URI. Listed here only because clippy cannot prove it statically.
pub fn from_fastly_headers_ref(req: &fastly::Request) -> http::Request<EdgeBody> {
    build_http_request(req, EdgeBody::empty())
}

/// Convert an `http::Response<EdgeBody>` into a `fastly::Response`.
///
/// # PR 15 removal target
pub fn to_fastly_response(resp: http::Response<EdgeBody>) -> fastly::Response {
    let (parts, body) = resp.into_parts();
    let mut fastly_resp = fastly::Response::from_status(parts.status.as_u16());
    for (name, value) in &parts.headers {
        fastly_resp.append_header(name.as_str(), value.as_bytes());
    }

    debug_assert!(
        matches!(&body, EdgeBody::Once(_)),
        "streaming body passed to compat::to_fastly_response will be silently truncated"
    );
    match body {
        EdgeBody::Once(bytes) => {
            if !bytes.is_empty() {
                fastly_resp.set_body(bytes.to_vec());
            }
        }
        EdgeBody::Stream(_) => {
            log::warn!("streaming body in compat::to_fastly_response; body will be empty");
        }
    }

    fastly_resp
}

/// Sanitize forwarded headers on a `fastly::Request`.
///
/// # PR 15 removal target
pub fn sanitize_fastly_forwarded_headers(req: &mut fastly::Request) {
    for &name in SPOOFABLE_FORWARDED_HEADERS {
        if req.get_header(name).is_some() {
            log::debug!("Stripped spoofable header: {name}");
            req.remove_header(name);
        }
    }
}

/// Copy `X-*` custom headers between two `fastly::Request` values.
///
/// # PR 15 removal target
pub fn copy_fastly_custom_headers(from: &fastly::Request, to: &mut fastly::Request) {
    for (name, value) in from.get_headers() {
        let name_str = name.as_str();
        if name_str.starts_with("x-") && !INTERNAL_HEADERS.contains(&name_str) {
            to.append_header(name_str, value);
        }
    }
}

/// Forward the `Cookie` header from one `fastly::Request` to another.
///
/// # PR 15 removal target
pub fn forward_fastly_cookie_header(
    from: &fastly::Request,
    to: &mut fastly::Request,
    strip_consent: bool,
) {
    use crate::cookies::{strip_cookies, CONSENT_COOKIE_NAMES};

    let Some(cookie_value) = from.get_header(header::COOKIE) else {
        return;
    };

    if !strip_consent {
        to.set_header(header::COOKIE, cookie_value);
        return;
    }

    match cookie_value.to_str() {
        Ok(value) => {
            let stripped = strip_cookies(value, CONSENT_COOKIE_NAMES);
            if !stripped.is_empty() {
                to.set_header(header::COOKIE, &stripped);
            }
        }
        Err(_) => {
            to.set_header(header::COOKIE, cookie_value);
        }
    }
}

/// Set the EC ID cookie on a `fastly::Response`.
///
/// # PR 15 removal target
pub fn set_fastly_ec_cookie(
    settings: &crate::settings::Settings,
    response: &mut fastly::Response,
    ec_id: &str,
) {
    if !crate::cookies::ec_cookie_value_is_safe(ec_id) {
        log::warn!(
            "Rejecting EC ID for Set-Cookie: value of {} bytes contains characters illegal in a cookie value",
            ec_id.len()
        );
        return;
    }
    response.append_header(
        header::SET_COOKIE,
        crate::cookies::create_ec_cookie(settings, ec_id),
    );
}

/// Expire the EC ID cookie on a `fastly::Response`.
///
/// # PR 15 removal target
pub fn expire_fastly_ec_cookie(
    settings: &crate::settings::Settings,
    response: &mut fastly::Response,
) {
    response.append_header(
        header::SET_COOKIE,
        format!(
            "{}=; {}",
            crate::constants::COOKIE_TS_EC,
            crate::cookies::ec_cookie_attributes(settings, 0),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_once_body_eq(body: EdgeBody, expected: &[u8]) {
        match body {
            EdgeBody::Once(bytes) => assert_eq!(bytes.as_ref(), expected, "should copy body bytes"),
            EdgeBody::Stream(_) => panic!("expected non-streaming body"),
        }
    }

    #[test]
    fn from_fastly_headers_ref_copies_headers() {
        let mut fastly_req =
            fastly::Request::new(fastly::http::Method::GET, "https://example.com/path");
        fastly_req.set_header("x-custom", "value");

        let http_req = from_fastly_headers_ref(&fastly_req);

        assert_eq!(http_req.uri().path(), "/path", "should copy path");
        assert_eq!(
            http_req
                .headers()
                .get("x-custom")
                .and_then(|v| v.to_str().ok()),
            Some("value"),
            "should copy custom header"
        );
    }

    #[test]
    fn from_fastly_headers_ref_preserves_duplicate_headers() {
        let mut fastly_req =
            fastly::Request::new(fastly::http::Method::GET, "https://example.com/path");
        fastly_req.append_header("x-custom", "first");
        fastly_req.append_header("x-custom", "second");

        let http_req = from_fastly_headers_ref(&fastly_req);
        let values: Vec<_> = http_req
            .headers()
            .get_all("x-custom")
            .iter()
            .map(|value| value.to_str().expect("should be valid utf8"))
            .collect();

        assert_eq!(
            values,
            vec!["first", "second"],
            "should preserve duplicates"
        );
    }

    #[test]
    fn from_fastly_headers_ref_body_is_empty() {
        let fastly_req = fastly::Request::new(fastly::http::Method::POST, "https://example.com/");

        let http_req = from_fastly_headers_ref(&fastly_req);

        assert_eq!(http_req.method(), http::Method::POST, "should copy method");
        assert_once_body_eq(http_req.into_body(), b"");
    }

    #[test]
    fn to_fastly_response_copies_status_and_headers() {
        let http_resp = http::Response::builder()
            .status(201)
            .header("content-type", "application/json")
            .body(EdgeBody::from(b"{}".as_ref()))
            .expect("should build response");

        let fastly_resp = to_fastly_response(http_resp);

        assert_eq!(fastly_resp.get_status().as_u16(), 201, "should copy status");
        assert!(
            fastly_resp.get_header("content-type").is_some(),
            "should copy content-type header"
        );
    }

    #[test]
    fn sanitize_fastly_forwarded_headers_strips_spoofable() {
        let mut req = fastly::Request::new(fastly::http::Method::GET, "https://example.com");
        req.set_header("forwarded", "host=evil.com");
        req.set_header("x-forwarded-host", "evil.com");
        req.set_header("x-forwarded-proto", "https");
        req.set_header("fastly-ssl", "1");
        req.set_header("host", "legit.example.com");

        sanitize_fastly_forwarded_headers(&mut req);

        assert!(
            req.get_header("forwarded").is_none(),
            "should strip Forwarded"
        );
        assert!(
            req.get_header("x-forwarded-host").is_none(),
            "should strip X-Forwarded-Host"
        );
        assert!(
            req.get_header("x-forwarded-proto").is_none(),
            "should strip X-Forwarded-Proto"
        );
        assert!(
            req.get_header("fastly-ssl").is_none(),
            "should strip Fastly-SSL"
        );
        assert_eq!(
            req.get_header("host").and_then(|v| v.to_str().ok()),
            Some("legit.example.com"),
            "should preserve Host"
        );
    }

    #[test]
    fn forward_fastly_cookie_header_strips_consent() {
        let mut from_req = fastly::Request::new(fastly::http::Method::GET, "https://example.com");
        from_req.set_header(header::COOKIE, "euconsent-v2=BOE; session=abc");
        let mut to_req = fastly::Request::new(fastly::http::Method::GET, "https://partner.com");

        forward_fastly_cookie_header(&from_req, &mut to_req, true);

        let forwarded = to_req
            .get_header(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            !forwarded.contains("euconsent-v2"),
            "should strip consent cookie"
        );
        assert!(
            forwarded.contains("session=abc"),
            "should keep non-consent cookie"
        );
    }

    #[test]
    fn copy_fastly_custom_headers_filters_internal() {
        let mut from_req = fastly::Request::new(fastly::http::Method::GET, "https://example.com");
        from_req.set_header("x-custom-data", "present");
        from_req.set_header("x-ts-ec", "should-not-copy");
        let mut to_req = fastly::Request::new(fastly::http::Method::GET, "https://partner.com");

        copy_fastly_custom_headers(&from_req, &mut to_req);

        assert_eq!(
            to_req
                .get_header("x-custom-data")
                .and_then(|v| v.to_str().ok()),
            Some("present"),
            "should copy arbitrary x-header"
        );
        assert!(
            to_req.get_header("x-ts-ec").is_none(),
            "should not copy internal header"
        );
    }

    #[test]
    fn copy_fastly_custom_headers_preserves_duplicate_values() {
        let mut from_req = fastly::Request::new(fastly::http::Method::GET, "https://example.com");
        from_req.append_header("x-custom-data", "first");
        from_req.append_header("x-custom-data", "second");
        let mut to_req = fastly::Request::new(fastly::http::Method::GET, "https://partner.com");

        copy_fastly_custom_headers(&from_req, &mut to_req);

        let values: Vec<_> = to_req
            .get_headers()
            .filter(|(name, _)| name.as_str() == "x-custom-data")
            .map(|(_, value)| value.to_str().expect("should be valid utf8"))
            .collect();
        assert_eq!(
            values,
            vec!["first", "second"],
            "should preserve duplicates"
        );
    }

    #[test]
    fn set_fastly_ec_cookie_sets_cookie_header() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut response = fastly::Response::new();

        set_fastly_ec_cookie(&settings, &mut response, "abc123.XyZ789");

        let cookie = response
            .get_header(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        assert_eq!(
            cookie,
            Some(format!(
                "ts-ec=abc123.XyZ789; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=31536000",
                settings.publisher.cookie_domain
            )),
            "should set expected EC cookie"
        );
    }

    #[test]
    fn expire_fastly_ec_cookie_sets_expiry_cookie() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut response = fastly::Response::new();

        expire_fastly_ec_cookie(&settings, &mut response);

        let cookie = response
            .get_header(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        assert_eq!(
            cookie,
            Some(format!(
                "ts-ec=; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
                settings.publisher.cookie_domain
            )),
            "should set expected expiry cookie"
        );
    }
}
