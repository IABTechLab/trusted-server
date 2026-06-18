//! Edge Cookie (EC) ID generation using HMAC.
//!
//! This module provides functionality for generating privacy-preserving EC IDs
//! based on the client IP address and a secret key.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::Request;

use crate::constants::{COOKIE_TS_EC, HEADER_X_TS_EC};
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
#[cfg(test)]
use crate::ec::generation::normalize_ip;
#[cfg(test)]
use crate::ec::provider::{IdentityInput, build_provider};
use crate::error::TrustedServerError;
#[cfg(test)]
use crate::evidence::BorrowedRequestInfo;
#[cfg(test)]
use crate::platform::RuntimeServices;
#[cfg(test)]
use crate::settings::Settings;

/// Generates a fresh EC ID using the configured Edge Cookie provider.
///
/// Routes through the pluggable provider model: the active `[ec] provider`
/// selection decides the outcome. Returns `Ok(None)` when no provider is
/// configured, so Trusted Server runs statelessly and mints no Edge Cookie.
/// `request_headers` lets a provider that derives identity from request
/// evidence read it; the built-in HMAC provider ignores it and uses only the
/// normalized client IP.
///
/// # Errors
///
/// - [`TrustedServerError::EdgeCookie`] if provider generation fails
///
/// Currently exercised only by tests: the production EC lifecycle generates IDs
/// through [`crate::ec`]/`EcContext` rather than this edge-cookie helper.
#[cfg(test)]
pub fn generate_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
    request_headers: Option<&http::HeaderMap>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    // Fall back to "unknown" when the client IP is unavailable (for example in
    // local testing). All such requests share the same HMAC base; the random
    // suffix provides uniqueness.
    let client_ip = services
        .client_info()
        .client_ip
        .map(normalize_ip)
        .unwrap_or_else(|| "unknown".to_string());

    log::trace!("Generating fresh EC ID from normalized client context");

    let Some(provider) = build_provider(&settings.ec, services.host_signals())? else {
        log::info!("No Edge Cookie provider configured; running statelessly");
        return Ok(None);
    };

    // The provider reads request data (the client IP, and on a fingerprinting
    // host the TLS/HTTP-2 signals) borrowed at call time, so nothing is cloned.
    let request_info = BorrowedRequestInfo::new(&client_ip, request_headers);
    // The publisher path applies the permission gate at the call site, and the
    // built-in provider reads neither the resolved permissions nor consent, so
    // they are not threaded here.
    let generated = provider.generate(&request_info, &IdentityInput::default())?;
    Ok(generated.id)
}

/// Gets an existing EC ID from the request.
///
/// Attempts to retrieve an existing EC ID from:
/// 1. The `x-ts-ec` header
/// 2. The `ts-ec` cookie
///
/// Returns `None` if neither source contains an EC ID.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
pub fn get_ec_id(req: &Request<EdgeBody>) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(ec_id) = req
        .headers()
        .get(HEADER_X_TS_EC)
        .and_then(|h| h.to_str().ok())
    {
        if ec_id_has_only_allowed_chars(ec_id) {
            log::trace!("Using existing EC ID from header");
            return Ok(Some(ec_id.to_string()));
        }
        log::warn!("Rejected EC ID from x-ts-ec header with disallowed characters");
    }

    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get(COOKIE_TS_EC) {
                let value = cookie.value();
                if ec_id_has_only_allowed_chars(value) {
                    log::trace!("Using existing EC ID from cookie");
                    return Ok(Some(value.to_string()));
                }
                log::warn!("Rejected EC ID from cookie with disallowed characters");
            }
        }
        None => {
            log::debug!("No cookie header found in request");
        }
    }

    Ok(None)
}

/// Gets or creates an EC ID from the request.
///
/// Attempts to retrieve an existing EC ID from:
/// 1. The `x-ts-ec` header
/// 2. The `ts-ec` cookie
///
/// If neither exists, generates a new EC ID via the configured provider.
///
/// Returns `Ok(None)` when no existing EC ID is present and no Edge Cookie
/// provider is configured, so the caller proceeds statelessly.
///
/// # Errors
///
/// Returns an error if ID generation fails.
#[cfg(test)]
pub(crate) fn get_or_generate_ec_id_from_http_request(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(id) = get_ec_id(req)? {
        return Ok(Some(id));
    }

    // If no existing EC ID found, generate a fresh one through the provider.
    let ec_id = generate_ec_id(settings, services, Some(req.headers()))?;
    if ec_id.is_some() {
        log::trace!("No existing EC ID found; generated a fresh EC ID");
    }
    Ok(ec_id)
}

/// Gets or creates an EC ID from the request.
///
/// # Errors
///
/// Returns an error if ID generation fails.
#[cfg(test)]
pub fn get_or_generate_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    get_or_generate_ec_id_from_http_request(settings, services, req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body as EdgeBody;
    use http::{HeaderName, header};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use crate::ec::generation::generate_ec_id as generate_canonical_ec_id;
    use crate::platform::test_support::{noop_services, noop_services_with_client_ip};
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_generate_ec_id_matches_canonical_generator_for_ipv6() {
        // Regression guard: this module must hash the same normalized IP as
        // the canonical generator in ec::generation. A divergent IPv6 /64
        // normalization would mint non-correlating identity prefixes for the
        // same client depending on which path generated the ID.
        let settings = create_test_settings();
        let ip = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0x85a3, 0x0000, 0x8a2e, 0x0370, 0x7334, 0x1234,
        ));

        let id_here = generate_ec_id(&settings, &noop_services_with_client_ip(ip), None)
            .expect("should generate EC ID via edge_cookie")
            .expect("should configure the hmac provider in test settings");
        let passphrase = settings
            .ec
            .providers
            .hmac
            .as_ref()
            .map(|hmac| hmac.passphrase.expose().as_str())
            .unwrap_or("");
        let id_canonical = generate_canonical_ec_id(passphrase, &normalize_ip(ip))
            .expect("should generate EC ID via canonical generator");

        assert_eq!(
            crate::ec::ec_hash(&id_here),
            crate::ec::ec_hash(&id_canonical),
            "should produce the same identity hash prefix as the canonical generator"
        );
    }

    fn create_test_request(headers: &[(HeaderName, &str)]) -> Request<EdgeBody> {
        let mut builder = Request::builder().method("GET").uri("http://example.com");
        for (key, value) in headers {
            builder = builder.header(key, *value);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    fn is_ec_id_format(value: &str) -> bool {
        let mut parts = value.split('.');
        let hmac_part = match parts.next() {
            Some(part) => part,
            None => return false,
        };
        let suffix_part = match parts.next() {
            Some(part) => part,
            None => return false,
        };
        if parts.next().is_some() {
            return false;
        }
        if hmac_part.len() != 64 || suffix_part.len() != 6 {
            return false;
        }
        if !hmac_part.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        if !suffix_part.chars().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        true
    }

    #[test]
    fn test_generate_ec_id() {
        let settings: Settings = create_test_settings();

        let ec_id = generate_ec_id(&settings, &noop_services(), None)
            .expect("should generate EC ID")
            .expect("should configure the hmac provider in test settings");
        log::debug!("Generated EC ID: {}", ec_id);
        assert!(
            is_ec_id_format(&ec_id),
            "should match EC ID format: {{64hex}}.{{6alnum}}"
        );
    }

    #[test]
    fn generate_ec_id_returns_none_when_no_provider_is_configured() {
        let mut settings = create_test_settings();
        // No provider selected: Trusted Server runs statelessly.
        settings.ec.provider = None;

        let id = generate_ec_id(&settings, &noop_services(), None)
            .expect("generation should not error when no provider is configured");
        assert!(
            id.is_none(),
            "no Edge Cookie provider should mean no Edge Cookie is minted"
        );
    }

    #[test]
    fn test_generate_ec_id_uses_client_ip() {
        let settings = create_test_settings();
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));

        let id_with_ip = generate_ec_id(&settings, &noop_services_with_client_ip(ip), None)
            .expect("should generate EC ID with client IP")
            .expect("should configure the hmac provider in test settings");
        let id_without_ip = generate_ec_id(&settings, &noop_services(), None)
            .expect("should generate EC ID without client IP")
            .expect("should configure the hmac provider in test settings");

        let hmac_with_ip = id_with_ip.split_once('.').expect("should contain dot").0;
        let hmac_without_ip = id_without_ip.split_once('.').expect("should contain dot").0;

        assert_ne!(
            hmac_with_ip, hmac_without_ip,
            "should produce different HMAC when client IP differs"
        );
    }

    #[test]
    fn test_is_ec_id_format_accepts_valid_value() {
        let value = format!("{}.{}", "a".repeat(64), "Ab12z9");
        assert!(
            is_ec_id_format(&value),
            "should accept a valid EC ID format"
        );
    }

    #[test]
    fn test_is_ec_id_format_rejects_invalid_values() {
        let missing_suffix = "a".repeat(64);
        assert!(
            !is_ec_id_format(&missing_suffix),
            "should reject missing suffix"
        );

        let invalid_hex = format!("{}.{}", "a".repeat(63) + "g", "Ab12z9");
        assert!(
            !is_ec_id_format(&invalid_hex),
            "should reject non-hex HMAC content"
        );

        let invalid_suffix = format!("{}.{}", "a".repeat(64), "ab-129");
        assert!(
            !is_ec_id_format(&invalid_suffix),
            "should reject non-alphanumeric suffix"
        );

        let extra_segment = format!("{}.{}.{}", "a".repeat(64), "Ab12z9", "zz");
        assert!(
            !is_ec_id_format(&extra_segment),
            "should reject extra segments"
        );
    }

    #[test]
    fn test_get_ec_id_with_header() {
        let settings = create_test_settings();
        let req = create_test_request(&[(HEADER_X_TS_EC, "existing_ec_id")]);

        let ec_id = get_ec_id(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_ec_id".to_string()));

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .expect("should reuse header EC ID")
            .expect("an existing EC should be present");
        assert_eq!(ec_id, "existing_ec_id");
    }

    #[test]
    fn test_get_ec_id_with_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(&[(
            header::COOKIE,
            &format!("{}=existing_cookie_id", COOKIE_TS_EC),
        )]);

        let ec_id = get_ec_id(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_cookie_id".to_string()));

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .expect("should reuse cookie EC ID")
            .expect("an existing EC should be present");
        assert_eq!(ec_id, "existing_cookie_id");
    }

    #[test]
    fn test_get_ec_id_from_http_request_with_header() {
        let req = http::Request::builder()
            .method("GET")
            .uri("http://example.com")
            .header(HEADER_X_TS_EC, "existing_http_ec_id")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");

        let ec_id = get_ec_id(&req).expect("should get EC ID from http request");

        assert_eq!(ec_id, Some("existing_http_ec_id".to_string()));
    }

    #[test]
    fn test_get_or_generate_ec_id_from_http_request_reuses_cookie() {
        let settings = create_test_settings();
        let req = http::Request::builder()
            .method("GET")
            .uri("http://example.com")
            .header(
                header::COOKIE,
                format!("{}=existing_http_cookie_id", COOKIE_TS_EC),
            )
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");

        let ec_id = get_or_generate_ec_id_from_http_request(&settings, &noop_services(), &req)
            .expect("should reuse cookie EC ID from http request")
            .expect("an existing EC should be present");

        assert_eq!(ec_id, "existing_http_cookie_id");
    }

    #[test]
    fn test_get_ec_id_none() {
        let req = create_test_request(&[]);
        let ec_id = get_ec_id(&req).expect("should handle missing ID");
        assert!(ec_id.is_none());
    }

    #[test]
    fn test_get_or_generate_ec_id_generate_new() {
        let settings = create_test_settings();
        let req = create_test_request(&[]);

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .expect("should get or generate EC ID")
            .expect("should configure the hmac provider in test settings");
        assert!(!ec_id.is_empty());
    }

    #[test]
    fn test_get_ec_id_rejects_invalid_header_and_falls_back_to_cookie() {
        let req = create_test_request(&[
            (HEADER_X_TS_EC, "evil;injected"),
            (header::COOKIE, &format!("{}=valid_cookie_id", COOKIE_TS_EC)),
        ]);

        let ec_id = get_ec_id(&req).expect("should handle invalid header gracefully");
        assert_eq!(
            ec_id,
            Some("valid_cookie_id".to_string()),
            "should reject tampered header and fall back to valid cookie"
        );
    }

    #[test]
    fn test_get_or_generate_ec_id_replaces_invalid_header() {
        let settings = create_test_settings();
        let req = create_test_request(&[(HEADER_X_TS_EC, "evil;injected")]);

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .expect("should generate fresh ID on invalid header")
            .expect("should configure the hmac provider in test settings");
        assert_ne!(
            ec_id, "evil;injected",
            "should not use tampered header value"
        );
        assert!(
            is_ec_id_format(&ec_id),
            "should generate a valid EC ID format when header is rejected"
        );
    }

    #[test]
    fn test_get_ec_id_rejects_invalid_cookie() {
        let req = create_test_request(&[(
            header::COOKIE,
            &format!("{}=bad<script>value", COOKIE_TS_EC),
        )]);

        let ec_id = get_ec_id(&req).expect("should handle invalid cookie gracefully");
        assert!(
            ec_id.is_none(),
            "should reject cookie with disallowed characters"
        );
    }
}
