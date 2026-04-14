//! Compatibility bridge between `fastly` SDK types and `http` crate types.
//!
//! Contains only the three functions used by the legacy `main()` entry point.
//! Relocated from `trusted-server-core` in PR 15 as part of removing all
//! `fastly` crate imports from the core library.

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{Request as HttpRequest, RequestBuilder, Response as HttpResponse, Uri};

/// Forwarded headers that clients can inject to spoof request context.
///
/// Inlined from `trusted_server_core::http_util` which is `pub(crate)`.
const SPOOFABLE_FORWARDED_HEADERS: &[&str] = &[
    "forwarded",
    "x-forwarded-host",
    "x-forwarded-proto",
    "fastly-ssl",
];

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
