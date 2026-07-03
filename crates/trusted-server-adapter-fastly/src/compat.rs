//! Compatibility bridge between `fastly` SDK types and `http` crate types.

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::Response as HttpResponse;
use trusted_server_core::http_util::SPOOFABLE_FORWARDED_HEADERS;

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
