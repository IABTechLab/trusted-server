//! Compatibility bridge between `fastly` SDK types and `http` crate types.
//!
//! Contains only the functions used by the legacy `main()` entry point.
//! Relocated from `trusted-server-core` as part of removing all `fastly` crate
//! imports from the core library.

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{Request as HttpRequest, RequestBuilder, Response as HttpResponse, Uri};
use trusted_server_core::http_util::SPOOFABLE_FORWARDED_HEADERS;

fn build_http_request(req: &fastly::Request, body: EdgeBody) -> HttpRequest {
    let uri: Uri = req
        .get_url_str()
        .parse()
        .expect("should parse fastly request URL as URI");

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
/// # Panics
///
/// Panics if the Fastly request URL cannot be parsed as an `http::Uri`.
pub(crate) fn from_fastly_request(mut req: fastly::Request) -> HttpRequest {
    let body = EdgeBody::from(req.take_body_bytes());
    build_http_request(&req, body)
}

/// Convert a `fastly::Response` into an [`HttpResponse`].
pub(crate) fn from_fastly_response(mut resp: fastly::Response) -> HttpResponse {
    let status = resp.get_status();
    let mut builder = edgezero_core::http::response_builder().status(status);
    for (name, value) in resp.get_headers() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    builder
        .body(EdgeBody::from(resp.take_body_bytes()))
        .expect("should build http response from fastly response")
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
