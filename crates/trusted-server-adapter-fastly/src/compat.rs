//! Compatibility bridge between `fastly` SDK types and `http` crate types.
//!
//! Contains only the functions used by the legacy `main()` entry point.
//! Relocated from `trusted-server-core` as part of removing all `fastly` crate
//! imports from the core library.

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{Request as HttpRequest, RequestBuilder, Response as HttpResponse, Uri};
use trusted_server_core::http_util::SPOOFABLE_FORWARDED_HEADERS;

fn build_http_request(req: &fastly::Request, body: EdgeBody) -> HttpRequest {
    // Does not panic in practice: a URL that Fastly accepts but `http::Uri`
    // rejects degrades to "/" instead of aborting the Wasm instance.
    let uri: Uri = req.get_url_str().parse().unwrap_or_else(|_| {
        log::warn!(
            "failed to parse fastly request URL '{}'; falling back to '/'",
            req.get_url_str()
        );
        Uri::from_static("/")
    });

    let mut builder: RequestBuilder = edgezero_core::http::request_builder()
        .method(req.get_method().clone())
        .uri(uri);

    for (name, value) in req.get_headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }

    builder
        .body(body)
        .expect("should build http request from fastly request")
}

/// Convert an owned `fastly::Request` into an [`HttpRequest`].
///
/// URLs that Fastly accepts but `http::Uri` rejects fall back to `/` with a
/// warning instead of panicking, preserving availability on the legacy path.
pub(crate) fn from_fastly_request(mut req: fastly::Request) -> HttpRequest {
    let body = EdgeBody::from(req.take_body_bytes());
    build_http_request(&req, body)
}

/// Convert an [`HttpResponse`] into a `fastly::Response`.
pub(crate) fn to_fastly_response(resp: HttpResponse) -> fastly::Response {
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
            // Streaming bodies cannot cross the compat boundary. Both audited call sites
            // (legacy_main buffered arm and edgezero_main after EdgeZero collapses bodies
            // to Once) only pass Once bodies — a Stream here is a caller error.
            // The assert is suppressed in test builds where the behavior-documentation
            // test deliberately exercises this path.
            #[cfg(not(test))]
            debug_assert!(
                false,
                "to_fastly_response: streaming body will be silently dropped; \
                 use to_fastly_response_skeleton + stream_to_client for streaming responses"
            );
            log::warn!("streaming body in compat::to_fastly_response; body will be empty");
        }
    }

    fastly_resp
}

/// Convert an [`HttpResponse`] into a `fastly::Response` without a body.
///
/// Use this when the caller will stream the body separately through
/// [`fastly::Response::stream_to_client`].
pub(crate) fn to_fastly_response_skeleton(resp: HttpResponse) -> fastly::Response {
    let (parts, _body) = resp.into_parts();
    let mut fastly_resp = fastly::Response::from_status(parts.status.as_u16());
    for (name, value) in &parts.headers {
        fastly_resp.append_header(name.as_str(), value.as_bytes());
    }
    fastly_resp
}

/// Sanitize forwarded headers on a `fastly::Request`.
///
/// Strips headers that clients can spoof before any request-derived context
/// is built or the request is converted to core HTTP types.
pub(crate) fn sanitize_fastly_forwarded_headers(req: &mut fastly::Request) {
    for &name in SPOOFABLE_FORWARDED_HEADERS {
        if req.get_header(name).is_some() {
            log::debug!("Stripped spoofable header: {name}");
            req.remove_header(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_fastly_forwarded_headers_strips_spoofable() {
        let mut req = fastly::Request::get("https://example.com/");
        req.set_header("forwarded", "for=1.2.3.4");
        req.set_header("x-forwarded-host", "evil.example.com");
        req.set_header("x-forwarded-proto", "http");
        req.set_header("fastly-ssl", "1");
        req.set_header("host", "example.com");

        sanitize_fastly_forwarded_headers(&mut req);

        assert!(
            req.get_header("forwarded").is_none(),
            "should strip forwarded"
        );
        assert!(
            req.get_header("x-forwarded-host").is_none(),
            "should strip x-forwarded-host"
        );
        assert!(
            req.get_header("x-forwarded-proto").is_none(),
            "should strip x-forwarded-proto"
        );
        assert!(
            req.get_header("fastly-ssl").is_none(),
            "should strip fastly-ssl"
        );
        assert!(req.get_header("host").is_some(), "should preserve host");
    }

    #[test]
    fn from_fastly_request_falls_back_to_root_on_unparseable_url() {
        // `url::Url` (used by fastly) has no length limit, but `http::Uri`
        // rejects URIs longer than 65534 bytes — an accepted-by-Fastly,
        // rejected-by-Uri divergence the fallback guards against.
        let long_url = format!("https://example.com/{}", "a".repeat(70_000));
        let req = fastly::Request::get(long_url);

        let http_req = from_fastly_request(req);

        assert_eq!(
            http_req.uri(),
            &Uri::from_static("/"),
            "should fall back to '/' when the fastly URL cannot be parsed as an http::Uri"
        );
    }

    #[test]
    fn to_fastly_response_with_streaming_body_produces_empty_body() {
        use edgezero_core::http::StatusCode;

        let stream = futures::stream::empty::<bytes::Bytes>();
        let stream_body = EdgeBody::stream(stream);

        let http_resp = edgezero_core::http::response_builder()
            .status(StatusCode::OK)
            .body(stream_body)
            .expect("should build response");

        let mut fastly_resp = to_fastly_response(http_resp);

        assert_eq!(
            fastly_resp.get_status().as_u16(),
            200,
            "should preserve status"
        );
        assert!(
            fastly_resp.take_body_bytes().is_empty(),
            "should produce empty body for streaming response"
        );
    }
}
