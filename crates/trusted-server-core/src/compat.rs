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
    let uri: http::Uri = req
        .get_url_str()
        .parse()
        .unwrap_or_else(|_| http::Uri::from_static("/"));

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

/// Convert an owned `fastly::Request` into an `http::Request<EdgeBody>`.
///
/// # PR 15 removal target
///
/// # Panics
///
/// Panics if the Fastly request URL cannot be parsed as an `http::Uri`.
pub fn from_fastly_request(mut req: fastly::Request) -> http::Request<EdgeBody> {
    let body = EdgeBody::from(req.take_body_bytes());
    build_http_request(&req, body)
}

/// Convert a borrowed `fastly::Request` into an `http::Request<EdgeBody>` for reading.
///
/// Headers are copied; the body is empty.
///
/// # PR 15 removal target
///
/// # Panics
///
/// Panics if the Fastly request URL cannot be parsed as an `http::Uri`.
pub fn from_fastly_headers_ref(req: &fastly::Request) -> http::Request<EdgeBody> {
    build_http_request(req, EdgeBody::empty())
}

/// Convert an `http::Request<EdgeBody>` into a `fastly::Request`.
///
/// # PR 15 removal target
pub fn to_fastly_request(req: http::Request<EdgeBody>) -> fastly::Request {
    let (parts, body) = req.into_parts();
    let mut fastly_req = fastly::Request::new(parts.method, parts.uri.to_string());
    for (name, value) in &parts.headers {
        fastly_req.append_header(name.as_str(), value.as_bytes());
    }

    match body {
        EdgeBody::Once(bytes) => {
            if !bytes.is_empty() {
                fastly_req.set_body(bytes.to_vec());
            }
        }
        EdgeBody::Stream(_) => {
            log::warn!("streaming body in compat::to_fastly_request; body will be empty");
        }
    }

    fastly_req
}

/// Convert a borrowed `http::Request<EdgeBody>` into a `fastly::Request`.
///
/// Headers, method, and URI are copied; the body is empty.
///
/// # PR 15 removal target
pub fn to_fastly_request_ref(req: &http::Request<EdgeBody>) -> fastly::Request {
    let mut fastly_req = fastly::Request::new(req.method().clone(), req.uri().to_string());
    for (name, value) in req.headers() {
        fastly_req.append_header(name.as_str(), value.as_bytes());
    }

    fastly_req
}

/// Convert a `fastly::Response` into an `http::Response<EdgeBody>`.
///
/// # PR 15 removal target
///
/// # Panics
///
/// Panics if the copied Fastly response parts cannot form a valid
/// `http::Response`.
pub fn from_fastly_response(mut resp: fastly::Response) -> http::Response<EdgeBody> {
    let status = resp.get_status();
    let mut builder = http::Response::builder().status(status);
    for (name, value) in resp.get_headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }

    builder
        .body(EdgeBody::from(resp.take_body_bytes()))
        .expect("should build http response from fastly response")
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

/// Set the synthetic ID cookie on a `fastly::Response`.
///
/// # PR 15 removal target
pub fn set_fastly_synthetic_cookie(
    settings: &crate::settings::Settings,
    response: &mut fastly::Response,
    synthetic_id: &str,
) {
    if !crate::cookies::synthetic_id_cookie_value_is_safe(synthetic_id) {
        log::warn!(
            "Rejecting synthetic_id for Set-Cookie: value of {} bytes contains characters illegal in a cookie value",
            synthetic_id.len()
        );
        return;
    }

    response.append_header(
        header::SET_COOKIE,
        crate::cookies::create_synthetic_cookie(settings, synthetic_id),
    );
}

/// Expire the synthetic ID cookie on a `fastly::Response`.
///
/// # PR 15 removal target
pub fn expire_fastly_synthetic_cookie(
    settings: &crate::settings::Settings,
    response: &mut fastly::Response,
) {
    response.append_header(
        header::SET_COOKIE,
        crate::cookies::create_synthetic_id_expiry_cookie(settings),
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
    fn from_fastly_request_copies_body() {
        let mut fastly_req =
            fastly::Request::new(fastly::http::Method::POST, "https://example.com/path");
        fastly_req.set_header("content-type", "application/json");
        fastly_req.set_body(r#"{"ok":true}"#);

        let http_req = from_fastly_request(fastly_req);
        let (parts, body) = http_req.into_parts();

        assert_eq!(parts.method, http::Method::POST, "should copy method");
        assert_eq!(parts.uri.path(), "/path", "should copy uri path");
        assert_eq!(
            parts
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "should copy headers"
        );
        assert_once_body_eq(body, br#"{"ok":true}"#);
    }

    #[test]
    fn to_fastly_request_copies_headers_and_body() {
        let http_req = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://example.com/submit")
            .header("x-custom", "value")
            .body(EdgeBody::from(b"payload".as_ref()))
            .expect("should build request");

        let mut fastly_req = to_fastly_request(http_req);

        assert_eq!(
            fastly_req.get_method(),
            &fastly::http::Method::POST,
            "should copy method"
        );
        assert_eq!(
            fastly_req
                .get_header("x-custom")
                .and_then(|v| v.to_str().ok()),
            Some("value"),
            "should copy headers"
        );
        assert_eq!(
            fastly_req.take_body_bytes().as_slice(),
            b"payload",
            "should copy body bytes"
        );
    }

    #[test]
    fn to_fastly_request_preserves_duplicate_headers() {
        let http_req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com/")
            .header("x-custom", "first")
            .header("x-custom", "second")
            .body(EdgeBody::empty())
            .expect("should build request");

        let fastly_req = to_fastly_request(http_req);

        let values: Vec<_> = fastly_req
            .get_headers()
            .filter(|(name, _)| name.as_str() == "x-custom")
            .map(|(_, value)| value.to_str().expect("should be valid utf8"))
            .collect();
        assert_eq!(
            values,
            vec!["first", "second"],
            "should preserve duplicate headers"
        );
    }

    #[test]
    fn from_fastly_response_copies_status_headers_and_body() {
        let mut fastly_resp = fastly::Response::from_status(202);
        fastly_resp.set_header("content-type", "application/json");
        fastly_resp.set_body(r#"{"ok":true}"#);

        let http_resp = from_fastly_response(fastly_resp);
        let (parts, body) = http_resp.into_parts();

        assert_eq!(parts.status.as_u16(), 202, "should copy status");
        assert_eq!(
            parts
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "should copy headers"
        );
        assert_once_body_eq(body, br#"{"ok":true}"#);
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
    fn to_fastly_request_ref_copies_method_uri_and_headers_without_body() {
        let http_req = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://example.com/path?q=1")
            .header("x-custom", "value")
            .body(EdgeBody::from(b"payload".as_ref()))
            .expect("should build request");

        let mut fastly_req = to_fastly_request_ref(&http_req);

        assert_eq!(
            fastly_req.get_method(),
            &fastly::http::Method::POST,
            "should copy method"
        );
        assert_eq!(
            fastly_req.get_url_str(),
            "https://example.com/path?q=1",
            "should copy URI"
        );
        assert_eq!(
            fastly_req
                .get_header("x-custom")
                .and_then(|v| v.to_str().ok()),
            Some("value"),
            "should copy headers"
        );
        assert!(
            fastly_req.take_body_bytes().is_empty(),
            "borrowed conversion should not copy body bytes"
        );
    }

    #[test]
    fn to_fastly_request_ref_preserves_duplicate_headers() {
        let http_req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com/")
            .header("x-custom", "first")
            .header("x-custom", "second")
            .body(EdgeBody::empty())
            .expect("should build request");

        let fastly_req = to_fastly_request_ref(&http_req);

        let values: Vec<_> = fastly_req
            .get_headers()
            .filter(|(name, _)| name.as_str() == "x-custom")
            .map(|(_, value)| value.to_str().expect("should be valid utf8"))
            .collect();
        assert_eq!(
            values,
            vec!["first", "second"],
            "should preserve duplicate headers"
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
        from_req.set_header("x-synthetic-id", "should-not-copy");
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
            to_req.get_header("x-synthetic-id").is_none(),
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
    fn set_fastly_synthetic_cookie_sets_cookie_header() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut response = fastly::Response::new();

        set_fastly_synthetic_cookie(&settings, &mut response, "abc123.XyZ789");

        let cookie = response
            .get_header(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        assert_eq!(
            cookie,
            Some(format!(
                "synthetic_id=abc123.XyZ789; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=31536000",
                settings.publisher.cookie_domain
            )),
            "should set expected synthetic cookie"
        );
    }

    #[test]
    fn expire_fastly_synthetic_cookie_sets_expiry_cookie() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut response = fastly::Response::new();

        expire_fastly_synthetic_cookie(&settings, &mut response);

        let cookie = response
            .get_header(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        assert_eq!(
            cookie,
            Some(format!(
                "synthetic_id=; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
                settings.publisher.cookie_domain
            )),
            "should set expected expiry cookie"
        );
    }

    #[test]
    fn to_fastly_request_with_streaming_body_produces_empty_body() {
        // Stream bodies cannot cross the compat boundary: the Fastly SDK has no
        // streaming body API, so the shim drops the stream and logs a warning.
        // This test pins that silent-drop behaviour so it cannot become
        // accidentally load-bearing.  (Removal target: PR 15.)
        let body = EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::from_static(
            b"data",
        )]));
        let http_req = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://example.com/")
            .body(body)
            .expect("should build request");

        let mut fastly_req = to_fastly_request(http_req);

        assert!(
            fastly_req.take_body_bytes().is_empty(),
            "streaming body should be silently dropped; compat shim produces empty body"
        );
    }

    #[test]
    fn to_fastly_response_with_streaming_body_produces_empty_body() {
        // Same constraint as to_fastly_request: streaming bodies are dropped at
        // the compat boundary.  (Removal target: PR 15.)
        let body = EdgeBody::stream(futures::stream::iter(vec![bytes::Bytes::from_static(
            b"data",
        )]));
        let http_resp = http::Response::builder()
            .status(200)
            .body(body)
            .expect("should build response");

        let mut fastly_resp = to_fastly_response(http_resp);

        assert_eq!(
            fastly_resp.get_status().as_u16(),
            200,
            "should copy status code"
        );
        assert!(
            fastly_resp.take_body_bytes().is_empty(),
            "streaming body should be silently dropped; compat shim produces empty body"
        );
    }
}
