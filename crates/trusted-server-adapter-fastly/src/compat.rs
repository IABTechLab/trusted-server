//! Compatibility bridge between `fastly` SDK types and `http` crate types.

use std::net::IpAddr;

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::Response as HttpResponse;
use edgezero_core::http::{HeaderMap, HeaderName, HeaderValue};
use trusted_server_core::forwarder::ForwarderPreparation;
use trusted_server_core::http_util::SPOOFABLE_FORWARDED_HEADERS;
use trusted_server_core::settings::Settings;
use trusted_server_core::settings::TrustedClientIpConfig;

use crate::platform::resolve_client_ip;

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
/// Strips configured trust headers and headers that clients can spoof before
/// any request-derived context is built or the request is converted to core
/// HTTP types.
pub(crate) fn sanitize_fastly_forwarded_headers(
    req: &mut fastly::Request,
    config: Option<&TrustedClientIpConfig>,
) {
    if let Some(config) = config {
        req.remove_header(config.ip_header.as_str());
        req.remove_header(config.auth_header.as_str());
    }

    for &name in SPOOFABLE_FORWARDED_HEADERS {
        if req.get_header(name).is_some() {
            log::debug!("Stripped spoofable header: {name}");
            req.remove_header(name);
        }
    }
}

/// Resolve the trusted client IP, then strip every trust and spoofable header.
///
/// Resolution has to observe the trust headers *before* sanitization removes
/// them. Both steps live behind this one call so that ordering is structural
/// rather than a convention the entry point has to remember.
pub(crate) fn resolve_and_sanitize_client_ip(
    req: &mut fastly::Request,
    config: Option<&TrustedClientIpConfig>,
) -> Option<IpAddr> {
    let client_ip = resolve_client_ip(req, req.get_client_ip_addr(), config);
    sanitize_fastly_forwarded_headers(req, config);
    client_ip
}

/// Capture authenticated public origin before native forwarding sanitation.
pub(crate) fn capture_fastly_forwarder(
    req: &mut fastly::Request,
    settings: &Settings,
) -> ForwarderPreparation {
    let Some(config) = &settings.trusted_forwarder else {
        return ForwarderPreparation::default();
    };
    let mut headers = HeaderMap::new();
    let mut valid = true;
    for name in [
        config.auth_header.as_str(),
        "x-forwarded-host",
        "x-forwarded-proto",
    ] {
        for value in req.get_header_all(name) {
            match (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            ) {
                (Ok(name), Ok(value)) => {
                    headers.append(name, value);
                }
                _ => {
                    valid = false;
                }
            }
        }
    }
    req.remove_header(config.auth_header.as_str());
    if valid {
        ForwarderPreparation::capture(&headers, settings)
    } else {
        ForwarderPreparation::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::forwarder::prepare_trusted_forwarder;
    use trusted_server_core::http_util::RequestInfo;
    use trusted_server_core::platform::ClientInfo;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::TrustedForwarderConfig;

    #[test]
    fn fastly_forwarder_survives_native_sanitation_without_credentials() {
        let mut settings = Settings::default();
        settings.publisher.domain = "publisher.example.com".to_owned();
        settings.trusted_forwarder = Some(TrustedForwarderConfig {
            auth_header: "x-example-forwarder-auth".to_owned(),
            shared_secret: Redacted::new("fictional-forwarder-secret-0123456789".to_owned()),
        });
        for duplicate in [false, true] {
            let mut native = fastly::Request::get("http://upstream.example.com/path");
            native.set_header(
                "x-example-forwarder-auth",
                "fictional-forwarder-secret-0123456789",
            );
            native.set_header("x-forwarded-host", "publisher.example.com:8443");
            native.set_header("x-forwarded-proto", "https");
            if duplicate {
                native.append_header("x-forwarded-host", "publisher.example.com:8443");
            }
            let prepared = capture_fastly_forwarder(&mut native, &settings);
            assert!(
                native.get_header("x-example-forwarder-auth").is_none(),
                "should remove native forwarding credential"
            );
            sanitize_fastly_forwarded_headers(&mut native, None);
            let mut core = edgezero_core::http::request_builder()
                .uri("http://upstream.example.com/path")
                .header("host", "upstream.example.com")
                .body(EdgeBody::empty())
                .expect("should construct converted request");
            core.extensions_mut().insert(prepared);
            prepare_trusted_forwarder(&mut core, &settings);
            let info = RequestInfo::from_request(&core, &ClientInfo::default());
            assert_eq!(
                info.host,
                if duplicate {
                    "upstream.example.com"
                } else {
                    "publisher.example.com:8443"
                },
                "should retain the frozen native authentication outcome"
            );
            assert_eq!(
                info.scheme,
                if duplicate { "http" } else { "https" },
                "should retain the authenticated public scheme"
            );
        }
    }

    fn trusted_client_ip_config(ip_header: &str) -> TrustedClientIpConfig {
        TrustedClientIpConfig {
            ip_header: ip_header.to_owned(),
            auth_header: "x-trusted-client-auth".to_owned(),
            shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
        }
    }

    #[test]
    fn sanitize_fastly_forwarded_headers_strips_spoofable() {
        let mut req = fastly::Request::get("https://example.com/");
        req.set_header("forwarded", "for=1.2.3.4");
        req.set_header("x-forwarded-host", "evil.example.com");
        req.set_header("x-forwarded-proto", "http");
        req.set_header("fastly-ssl", "1");
        req.set_header("fastly-client-ip", "198.51.100.7");
        req.set_header("host", "example.com");

        sanitize_fastly_forwarded_headers(&mut req, None);

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
        assert!(
            req.get_header("fastly-client-ip").is_none(),
            "should strip fastly-client-ip"
        );
        assert!(req.get_header("host").is_some(), "should preserve host");
    }

    #[test]
    fn sanitize_fastly_forwarded_headers_strips_configured_headers() {
        let config = trusted_client_ip_config("x-trusted-client-ip");
        let mut req = fastly::Request::get("https://example.com/");
        req.set_header("x-trusted-client-ip", "198.51.100.7");
        req.set_header(
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        );
        req.set_header("host", "example.com");

        sanitize_fastly_forwarded_headers(&mut req, Some(&config));

        assert!(
            req.get_header("x-trusted-client-ip").is_none(),
            "should strip the configured IP header"
        );
        assert!(
            req.get_header("x-trusted-client-auth").is_none(),
            "should strip the configured auth header"
        );
        assert!(req.get_header("host").is_some(), "should preserve host");
    }

    #[test]
    fn sanitize_fastly_forwarded_headers_allows_static_and_dynamic_overlap() {
        let config = trusted_client_ip_config("fastly-client-ip");
        let mut req = fastly::Request::get("https://example.com/");
        req.set_header("fastly-client-ip", "198.51.100.7");
        req.set_header(
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        );

        sanitize_fastly_forwarded_headers(&mut req, Some(&config));

        assert!(
            req.get_header("fastly-client-ip").is_none(),
            "should tolerate removing fastly-client-ip twice"
        );
        assert!(
            req.get_header("x-trusted-client-auth").is_none(),
            "should strip the configured auth header"
        );
    }

    #[test]
    fn resolve_and_sanitize_client_ip_reads_trust_headers_before_stripping_them() {
        let config = trusted_client_ip_config("x-trusted-client-ip");
        let mut req = fastly::Request::get("https://example.com/");
        req.set_header("x-trusted-client-ip", "198.51.100.7");
        req.set_header(
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        );

        let resolved = resolve_and_sanitize_client_ip(&mut req, Some(&config));

        assert_eq!(
            resolved,
            Some(IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 7))),
            "should resolve the forwarded IP before sanitization removes the headers"
        );
        assert!(
            req.get_header("x-trusted-client-ip").is_none(),
            "should strip the configured IP header after resolving"
        );
        assert!(
            req.get_header("x-trusted-client-auth").is_none(),
            "should strip the configured auth header after resolving"
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
