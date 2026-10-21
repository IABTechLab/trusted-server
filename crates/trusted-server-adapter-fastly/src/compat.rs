//! Fastly request trust-header normalization before managed admission.

use std::net::IpAddr;

use trusted_server_core::http_util::SPOOFABLE_FORWARDED_HEADERS;
use trusted_server_core::settings::TrustedClientIpConfig;

use crate::platform::resolve_client_ip;

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

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::redacted::Redacted;

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
}
