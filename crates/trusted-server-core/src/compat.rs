//! Compatibility adapter bridging `fastly::Request/Response` with `http::Request/Response`.
//!
//! This module provides type conversion functions and request extension types used during the
//! Phase 2 of the platform migration. All items here are temporary bridges scheduled for removal
//! once the full type migration is complete.
//!
//! ## Usage pattern
//!
//! Handler-layer functions that still accept `fastly::Request` call these helpers at each
//! utility-function boundary:
//!
//! ```ignore
//! // read-only: borrow-based — no body copy
//! let http_req = compat::from_fastly_request_ref(&req);
//! let info = http_util::RequestInfo::from_request(&http_req);
//!
//! // mutable: build an http outgoing request, then convert back
//! let mut http_out = http::Request::builder().method(...).uri(...).body(Body::empty())...;
//! cookies::forward_cookie_header(&http_req, &mut http_out, true);
//! let fastly_out = compat::to_fastly_request(http_out);
//! ```
//!
//! # TODO(PR15)
//!
//! Remove this entire module after handler-layer type migration is complete.

use std::net::IpAddr;

use edgezero_core::body::Body;
use http::{HeaderName, HeaderValue, StatusCode};

// ── Request extension types ────────────────────────────────────────────────

/// TLS protocol string detected by the Fastly SDK.
///
/// Stored as a [`http::Request`] extension by [`from_fastly_request_ref`] so that
/// `http_util::detect_request_scheme` can check it without Fastly SDK access.
///
/// # TODO(PR15): Remove once handler-layer migration is complete.
#[derive(Debug, Clone)]
pub struct TlsProtocol(pub Option<&'static str>);

/// TLS cipher string detected by the Fastly SDK.
///
/// Stored as a [`http::Request`] extension by [`from_fastly_request_ref`] so that
/// `http_util::detect_request_scheme` can check it without Fastly SDK access.
///
/// # TODO(PR15): Remove once handler-layer migration is complete.
#[derive(Debug, Clone)]
pub struct TlsCipher(pub Option<&'static str>);

/// Client IP address captured from the Fastly SDK.
///
/// Stored as a [`http::Request`] extension by [`from_fastly_request_ref`] so that
/// `edge_cookie::generate_ec_id` can read the client IP without Fastly SDK access.
///
/// # TODO(PR15): Remove once handler-layer migration is complete.
#[derive(Debug, Clone)]
pub struct ClientIpExt(pub Option<IpAddr>);

// ── Request conversions ────────────────────────────────────────────────────

/// Create an `http::Request<Body>` from a `&fastly::Request` reference.
///
/// Copies all headers and the URI from the Fastly request. The body is **not** copied —
/// the returned request carries an empty body. Fastly-specific TLS and client IP
/// information is preserved in the request's extensions.
///
/// Prefer this over [`from_fastly_request`] for utility calls that only read
/// headers or the request path, so that the original `fastly::Request` body is
/// not consumed.
///
/// # Panics
///
/// Panics if the Fastly request URL cannot be parsed as an `http::Uri`.
///
/// # TODO(PR15): Remove after handler-layer migration is complete.
pub fn from_fastly_request_ref(req: &fastly::Request) -> http::Request<Body> {
    let uri: http::Uri = req
        .get_url_str()
        .parse()
        .expect("should parse fastly request URL as URI");

    let mut builder = http::Request::builder()
        .method(req.get_method().clone())
        .uri(uri);

    for (name, value) in req.get_headers() {
        builder = builder.header(name.clone(), value.clone());
    }

    let mut http_req = builder
        .body(Body::empty())
        .expect("should build http request from fastly request reference");

    http_req
        .extensions_mut()
        .insert(TlsProtocol(req.get_tls_protocol()));
    http_req
        .extensions_mut()
        .insert(TlsCipher(req.get_tls_cipher_openssl_name()));
    http_req
        .extensions_mut()
        .insert(ClientIpExt(req.get_client_ip_addr()));

    http_req
}

/// Consume a `fastly::Request` and produce an `http::Request<Body>`.
///
/// Moves the request body out of the Fastly request. Use this when the utility layer
/// needs to read or process the request body. Fastly-specific TLS and client IP
/// information is preserved in the request's extensions.
///
/// # Panics
///
/// Panics if the Fastly request URL cannot be parsed as an `http::Uri`.
///
/// # TODO(PR15): Remove after handler-layer migration is complete.
pub fn from_fastly_request(mut req: fastly::Request) -> http::Request<Body> {
    let uri: http::Uri = req
        .get_url_str()
        .parse()
        .expect("should parse fastly request URL as URI");

    let tls_protocol = req.get_tls_protocol();
    let tls_cipher = req.get_tls_cipher_openssl_name();
    let client_ip = req.get_client_ip_addr();

    let mut builder = http::Request::builder()
        .method(req.get_method().clone())
        .uri(uri);

    for (name, value) in req.get_headers() {
        builder = builder.header(name.clone(), value.clone());
    }

    let body = Body::from_bytes(req.take_body_bytes());

    let mut http_req = builder
        .body(body)
        .expect("should build http request from fastly request");

    http_req.extensions_mut().insert(TlsProtocol(tls_protocol));
    http_req.extensions_mut().insert(TlsCipher(tls_cipher));
    http_req.extensions_mut().insert(ClientIpExt(client_ip));

    http_req
}

/// Convert an `http::Request<Body>` back to a `fastly::Request`.
///
/// Used at integration layer boundaries where the utility layer produces an
/// `http::Request<Body>` (e.g. after header manipulation) but the downstream
/// operation still requires a `fastly::Request` for sending via the Fastly SDK.
///
/// Body streaming is not supported — the body is materialised into bytes before
/// conversion. Avoid this function for large bodies until streaming support is added.
///
/// # TODO(PR15): Remove after handler-layer migration is complete.
pub fn to_fastly_request(req: http::Request<Body>) -> fastly::Request {
    let (parts, body) = req.into_parts();

    let mut fastly_req = fastly::Request::new(parts.method, parts.uri.to_string());

    for (name, value) in &parts.headers {
        fastly_req.set_header(name, value);
    }

    let body_bytes = body.into_bytes();
    if !body_bytes.is_empty() {
        fastly_req.set_body_octet_stream(&body_bytes);
    }

    fastly_req
}

// ── Response conversions ───────────────────────────────────────────────────

/// Convert a `fastly::Response` to an `http::Response<Body>`.
///
/// Body streaming is not supported — the body is materialised into bytes.
///
/// # Panics
///
/// Panics if the `http::Response` builder fails (unreachable in practice).
///
/// # TODO(PR15): Remove after handler-layer migration is complete.
pub fn from_fastly_response(mut res: fastly::Response) -> http::Response<Body> {
    let mut builder = http::Response::builder().status(res.get_status());

    for (name, value) in res.get_headers() {
        builder = builder.header(name.clone(), value.clone());
    }

    let body = Body::from_bytes(res.take_body_bytes());

    builder
        .body(body)
        .expect("should build http response from fastly response")
}

/// Convert an `http::Response<Body>` to a `fastly::Response`.
///
/// Body streaming is not supported — the body is materialised into bytes.
///
/// # TODO(PR15): Remove after handler-layer migration is complete.
pub fn to_fastly_response(res: http::Response<Body>) -> fastly::Response {
    let (parts, body) = res.into_parts();

    let mut fastly_res = fastly::Response::from_status(parts.status);

    for (name, value) in &parts.headers {
        fastly_res.set_header(name, value);
    }

    let body_bytes = body.into_bytes();
    if !body_bytes.is_empty() {
        fastly_res.set_body_octet_stream(&body_bytes);
    }

    fastly_res
}

// ── Response builder helpers ───────────────────────────────────────────────

/// Build an `http::Response<Body>` with a given status and no body.
///
/// # Panics
///
/// Panics if the `http::Response` builder fails (unreachable in practice).
///
/// # TODO(PR15): Remove — callers should use `http::Response::builder()` directly.
#[must_use]
pub fn response_from_status(status: StatusCode) -> http::Response<Body> {
    http::Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("should build response from status")
}

/// Append a header to an `http::Response<Body>`, returning the response.
///
/// Unlike [`http::Response::headers_mut().append()`], this is chainable.
///
/// # TODO(PR15): Remove — callers should use `response.headers_mut().append()` directly.
pub fn append_header_to_response(
    mut res: http::Response<Body>,
    name: HeaderName,
    value: HeaderValue,
) -> http::Response<Body> {
    res.headers_mut().append(name, value);
    res
}

// ── Fastly-boundary bridge functions ──────────────────────────────────────
//
// These are handler/integration-layer shims that let code still holding
// `fastly::Request` / `fastly::Response` call into the migrated utility
// functions without converting the entire call stack in PR 11. They are
// removed in PR 15 once the handler and integration layers are fully migrated.

/// Apply `http_util::sanitize_forwarded_headers` to a `fastly::Request`.
///
/// Removes client-spoofable forwarded headers in place.
///
/// # TODO(PR15): Remove once the handler layer migrates to `http` types.
pub fn sanitize_forwarded_headers_fastly(req: &mut fastly::Request) {
    use crate::http_util::SPOOFABLE_FORWARDED_HEADERS;
    for header in SPOOFABLE_FORWARDED_HEADERS {
        if req.get_header(*header).is_some() {
            log::debug!("Stripped spoofable header: {}", header);
            req.remove_header(*header);
        }
    }
}

/// Apply `auth::enforce_basic_auth` with a `fastly::Request`, returning a `fastly::Response`.
///
/// Converts the request to `http::Request<Body>` and the returned response to
/// `fastly::Response` for the adapter entry point.
///
/// # Errors
///
/// Returns an error when handler configuration is invalid.
///
/// # TODO(PR15): Remove once the adapter entry point migrates to `http` types.
pub fn enforce_basic_auth_fastly(
    settings: &crate::settings::Settings,
    req: &fastly::Request,
) -> Result<Option<fastly::Response>, error_stack::Report<crate::error::TrustedServerError>> {
    use crate::auth;
    let http_req = from_fastly_request_ref(req);
    auth::enforce_basic_auth(settings, &http_req).map(|opt| opt.map(to_fastly_response))
}

/// Apply `http_util::serve_static_with_etag` with a `fastly::Request`, returning a
/// `fastly::Response`.
///
/// # TODO(PR15): Remove once the handler layer migrates to `http` types.
pub fn serve_static_with_etag_fastly(
    body: &str,
    req: &fastly::Request,
    content_type: &str,
) -> fastly::Response {
    use crate::http_util;
    let http_req = from_fastly_request_ref(req);
    let http_resp = http_util::serve_static_with_etag(body, &http_req, content_type);
    to_fastly_response(http_resp)
}

/// Apply `cookies::set_ec_cookie` to a `fastly::Response`.
///
/// Creates a temporary `http::Response<Body>` to collect the Set-Cookie header,
/// then appends it to the Fastly response.
///
/// # Panics
///
/// Panics if the temporary `http::Response` builder fails (unreachable in practice).
///
/// # TODO(PR15): Remove once publisher/registry migrate to `http` types.
pub fn set_ec_cookie_fastly(
    settings: &crate::settings::Settings,
    response: &mut fastly::Response,
    ec_id: &str,
) {
    use crate::cookies;
    let mut temp = http::Response::builder()
        .status(200u16)
        .body(Body::empty())
        .expect("should build temp response for cookie collection");
    cookies::set_ec_cookie(settings, &mut temp, ec_id);
    for value in temp.headers().get_all(http::header::SET_COOKIE) {
        response.append_header(http::header::SET_COOKIE, value);
    }
}

/// Apply `cookies::expire_ec_cookie` to a `fastly::Response`.
///
/// # Panics
///
/// Panics if the temporary `http::Response` builder fails (unreachable in practice).
///
/// # TODO(PR15): Remove once publisher/registry migrate to `http` types.
pub fn expire_ec_cookie_fastly(
    settings: &crate::settings::Settings,
    response: &mut fastly::Response,
) {
    use crate::cookies;
    let mut temp = http::Response::builder()
        .status(200u16)
        .body(Body::empty())
        .expect("should build temp response for cookie collection");
    cookies::expire_ec_cookie(settings, &mut temp);
    for value in temp.headers().get_all(http::header::SET_COOKIE) {
        response.append_header(http::header::SET_COOKIE, value);
    }
}

/// Apply `cookies::forward_cookie_header` with `fastly::Request` types.
///
/// Converts `from` to `http::Request<Body>`, runs the migrated utility, then
/// applies any Cookie header changes back to `to`.
///
/// # Panics
///
/// Panics if the temporary `http::Request` builder fails (unreachable in practice).
///
/// # TODO(PR15): Remove once integration layer migrates to `http` types.
pub fn forward_cookie_header_fastly(
    from: &fastly::Request,
    to: &mut fastly::Request,
    strip_consent: bool,
) {
    use crate::cookies;
    let http_from = from_fastly_request_ref(from);
    let mut http_to = http::Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .expect("should build temp request for cookie forwarding");
    cookies::forward_cookie_header(&http_from, &mut http_to, strip_consent);
    match http_to.headers().get(http::header::COOKIE) {
        Some(cookie) => to.set_header(http::header::COOKIE, cookie),
        None => {
            to.remove_header(http::header::COOKIE);
        }
    }
}

/// Apply `http_util::copy_custom_headers` with `fastly::Request` types.
///
/// Converts `from` to `http::Request<Body>`, runs the migrated utility, then
/// copies all resulting X-* headers to `to`.
///
/// # Panics
///
/// Panics if the temporary `http::Request` builder fails (unreachable in practice).
///
/// # TODO(PR15): Remove once integration layer migrates to `http` types.
pub fn copy_custom_headers_fastly(from: &fastly::Request, to: &mut fastly::Request) {
    use crate::http_util;
    let http_from = from_fastly_request_ref(from);
    let mut http_to = http::Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .expect("should build temp request for custom header copying");
    http_util::copy_custom_headers(&http_from, &mut http_to);
    for (name, value) in http_to.headers() {
        to.set_header(name, value);
    }
}

/// Apply `http_util::RequestInfo::from_request` with a `fastly::Request`.
///
/// # TODO(PR15): Remove once the handler layer migrates to `http` types.
pub fn request_info_from_fastly(req: &fastly::Request) -> crate::http_util::RequestInfo {
    use crate::http_util::RequestInfo;
    let http_req = from_fastly_request_ref(req);
    RequestInfo::from_request(&http_req)
}

/// Apply `edge_cookie::get_ec_id` with a `fastly::Request`.
///
/// # Errors
///
/// Returns an error if cookie parsing fails.
///
/// # TODO(PR15): Remove once the handler layer migrates to `http` types.
pub fn get_ec_id_fastly(
    req: &fastly::Request,
) -> Result<Option<String>, error_stack::Report<crate::error::TrustedServerError>> {
    use crate::edge_cookie;
    let http_req = from_fastly_request_ref(req);
    edge_cookie::get_ec_id(&http_req)
}

/// Apply `edge_cookie::get_or_generate_ec_id` with a `fastly::Request`.
///
/// # Errors
///
/// Returns an error if EC ID generation fails.
///
/// # TODO(PR15): Remove once the handler layer migrates to `http` types.
pub fn get_or_generate_ec_id_fastly(
    settings: &crate::settings::Settings,
    req: &fastly::Request,
) -> Result<String, error_stack::Report<crate::error::TrustedServerError>> {
    use crate::edge_cookie;
    let http_req = from_fastly_request_ref(req);
    edge_cookie::get_or_generate_ec_id(settings, &http_req)
}

/// Apply `edge_cookie::generate_ec_id` with a `fastly::Request`.
///
/// # Errors
///
/// Returns an error if EC ID generation fails.
///
/// # TODO(PR15): Remove once the handler layer migrates to `http` types.
pub fn generate_ec_id_fastly(
    settings: &crate::settings::Settings,
    req: &fastly::Request,
) -> Result<String, error_stack::Report<crate::error::TrustedServerError>> {
    use crate::edge_cookie;
    let http_req = from_fastly_request_ref(req);
    edge_cookie::generate_ec_id(settings, &http_req)
}

/// Apply `cookies::handle_request_cookies` with a `fastly::Request`.
///
/// # Errors
///
/// Returns an error if the Cookie header contains invalid UTF-8.
///
/// # TODO(PR15): Remove once the handler layer migrates to `http` types.
pub fn handle_request_cookies_fastly(
    req: &fastly::Request,
) -> Result<Option<cookie::CookieJar>, error_stack::Report<crate::error::TrustedServerError>> {
    use crate::cookies;
    let http_req = from_fastly_request_ref(req);
    cookies::handle_request_cookies(&http_req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header;

    fn make_fastly_req_with_headers() -> fastly::Request {
        let mut req =
            fastly::Request::new(fastly::http::Method::GET, "https://example.com/path?q=1");
        req.set_header("x-custom", "value");
        req.set_header(header::HOST, "example.com");
        req
    }

    #[test]
    fn from_fastly_request_ref_copies_headers_and_uri() {
        let fastly_req = make_fastly_req_with_headers();
        let http_req = from_fastly_request_ref(&fastly_req);

        assert_eq!(
            http_req
                .headers()
                .get("x-custom")
                .map(http::HeaderValue::as_bytes),
            Some(b"value".as_ref()),
            "should copy x-custom header"
        );
        assert_eq!(
            http_req.uri().path(),
            "/path",
            "should preserve request path"
        );
        assert_eq!(
            http_req.uri().query(),
            Some("q=1"),
            "should preserve query string"
        );
    }

    #[test]
    fn from_fastly_request_ref_has_empty_body() {
        let fastly_req = make_fastly_req_with_headers();
        let http_req = from_fastly_request_ref(&fastly_req);

        assert!(
            http_req.body().as_bytes().is_empty(),
            "should have empty body when using reference conversion"
        );
    }

    #[test]
    fn from_fastly_request_ref_stores_client_ip_extension() {
        let fastly_req = make_fastly_req_with_headers();
        let http_req = from_fastly_request_ref(&fastly_req);

        assert!(
            http_req.extensions().get::<ClientIpExt>().is_some(),
            "should store ClientIpExt extension"
        );
    }

    #[test]
    fn from_fastly_request_ref_stores_tls_extensions() {
        let fastly_req = make_fastly_req_with_headers();
        let http_req = from_fastly_request_ref(&fastly_req);

        assert!(
            http_req.extensions().get::<TlsProtocol>().is_some(),
            "should store TlsProtocol extension"
        );
        assert!(
            http_req.extensions().get::<TlsCipher>().is_some(),
            "should store TlsCipher extension"
        );
    }

    #[test]
    fn from_fastly_request_copies_body() {
        let mut fastly_req = fastly::Request::post("https://example.com/api");
        fastly_req.set_body_octet_stream(b"hello body");

        let http_req = from_fastly_request(fastly_req);

        assert_eq!(
            http_req.body().as_bytes(),
            b"hello body",
            "should copy request body bytes"
        );
    }

    #[test]
    fn to_fastly_request_preserves_method_uri_headers() {
        let http_req = http::Request::builder()
            .method("POST")
            .uri("https://api.example.com/submit")
            .header("x-req-id", "abc-123")
            .body(Body::empty())
            .expect("should build http request");

        let fastly_req = to_fastly_request(http_req);

        assert_eq!(
            fastly_req.get_method_str(),
            "POST",
            "should preserve HTTP method"
        );
        assert!(
            fastly_req.get_url_str().contains("api.example.com"),
            "should preserve URL"
        );
        assert!(
            fastly_req.get_header("x-req-id").is_some(),
            "should preserve request headers"
        );
    }

    #[test]
    fn response_roundtrip_preserves_status_and_headers() {
        let mut fastly_res = fastly::Response::from_status(202);
        fastly_res.set_header("x-resp-id", "xyz");

        let http_res = from_fastly_response(fastly_res);
        assert_eq!(
            http_res.status().as_u16(),
            202,
            "should preserve status code"
        );
        assert_eq!(
            http_res
                .headers()
                .get("x-resp-id")
                .map(http::HeaderValue::as_bytes),
            Some(b"xyz".as_ref()),
            "should preserve response headers"
        );

        let fastly_res2 = to_fastly_response(http_res);
        assert_eq!(
            fastly_res2.get_status().as_u16(),
            202,
            "should round-trip status code"
        );
        assert!(
            fastly_res2.get_header("x-resp-id").is_some(),
            "should round-trip response headers"
        );
    }

    #[test]
    fn response_from_status_has_correct_status_and_empty_body() {
        let res = response_from_status(StatusCode::NOT_FOUND);

        assert_eq!(res.status().as_u16(), 404, "should have 404 status");
        assert!(res.body().as_bytes().is_empty(), "should have empty body");
    }
}
