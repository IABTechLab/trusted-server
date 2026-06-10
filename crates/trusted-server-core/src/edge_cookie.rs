//! Edge Cookie (EC) ID generation using HMAC.
//!
//! This module provides functionality for generating privacy-preserving EC IDs
//! based on the client IP address and a secret key.

use std::net::IpAddr;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use hmac::{Hmac, Mac};
use http::Request;
use rand::Rng;
use sha2::Sha256;

use crate::constants::{COOKIE_TS_EC, HEADER_X_TS_EC};
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

const ALPHANUMERIC_CHARSET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Normalizes an IP address for stable EC ID generation.
///
/// For IPv6 addresses, masks to /64 prefix to handle Privacy Extensions
/// where devices rotate their interface identifier (lower 64 bits).
/// IPv4 addresses are returned unchanged.
fn normalize_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ipv4) => ipv4.to_string(),
        IpAddr::V6(ipv6) => {
            let segments = ipv6.segments();
            // Keep only the first 4 segments (64 bits) for /64 prefix
            format!(
                "{:x}:{:x}:{:x}:{:x}::",
                segments[0], segments[1], segments[2], segments[3]
            )
        }
    }
}

/// Generates a random alphanumeric string of the specified length.
fn generate_random_suffix(length: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..ALPHANUMERIC_CHARSET.len());
            ALPHANUMERIC_CHARSET[idx] as char
        })
        .collect()
}

/// Generates a fresh EC ID based on client IP address.
///
/// Uses only the client IP (not user-agent or other headers) intentionally:
/// EC IDs are meant to be simple, privacy-preserving identifiers — not
/// high-entropy fingerprints. The random suffix provides per-cookie
/// uniqueness for users behind the same NAT/proxy.
///
/// Creates an HMAC-SHA256-based ID using the configured secret key and
/// the client IP address, then appends a random suffix for additional
/// uniqueness. The resulting format is `{64hex}.{6alnum}`.
///
/// # Errors
///
/// - [`TrustedServerError::Ec`] if HMAC generation fails
pub fn generate_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
) -> Result<String, Report<TrustedServerError>> {
    // Fallback to "unknown" when client IP is unavailable (e.g., local testing).
    // All such requests share the same HMAC base; the random suffix provides uniqueness.
    let client_ip = services
        .client_info
        .client_ip
        .map(normalize_ip)
        .unwrap_or_else(|| "unknown".to_string());

    log::trace!("Input for fresh EC ID: client_ip={}", client_ip);

    let mut mac = HmacSha256::new_from_slice(settings.ec.passphrase.expose().as_bytes())
        .change_context(TrustedServerError::EdgeCookie {
            message: "Failed to create HMAC instance".to_string(),
        })?;
    mac.update(client_ip.as_bytes());
    let hmac_hash = hex::encode(mac.finalize().into_bytes());

    // Append random 6-character alphanumeric suffix for additional uniqueness
    let random_suffix = generate_random_suffix(6);
    let ec_id = format!("{hmac_hash}.{random_suffix}");

    log::trace!("Generated fresh EC ID: {}", ec_id);

    Ok(ec_id)
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
            log::trace!("Using existing EC ID from header: {}", ec_id);
            return Ok(Some(ec_id.to_string()));
        }
        log::warn!("Rejected EC ID from x-ts-ec header with disallowed characters");
    }

    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get(COOKIE_TS_EC) {
                let value = cookie.value();
                if ec_id_has_only_allowed_chars(value) {
                    log::trace!("Using existing EC ID from cookie: {}", value);
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
/// If neither exists, generates a new EC ID.
///
/// # Errors
///
/// Returns an error if ID generation fails.
pub(crate) fn get_or_generate_ec_id_from_http_request(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<String, Report<TrustedServerError>> {
    if let Some(id) = get_ec_id(req)? {
        return Ok(id);
    }

    // If no existing EC ID found, generate a fresh one
    let ec_id = generate_ec_id(settings, services)?;
    log::trace!("No existing EC ID, generated: {}", ec_id);
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
) -> Result<String, Report<TrustedServerError>> {
    get_or_generate_ec_id_from_http_request(settings, services, req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body as EdgeBody;
    use http::{header, HeaderName};
    use std::net::{Ipv4Addr, Ipv6Addr};

    use crate::platform::test_support::{noop_services, noop_services_with_client_ip};
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_normalize_ip_ipv4_unchanged() {
        let ipv4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(normalize_ip(ipv4), "192.168.1.100");
    }

    #[test]
    fn test_normalize_ip_ipv6_masks_to_64() {
        // Full IPv6 address with interface identifier
        let ipv6 = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0x85a3, 0x0000, 0x8a2e, 0x0370, 0x7334, 0x1234,
        ));
        assert_eq!(normalize_ip(ipv6), "2001:db8:85a3:0::");
    }

    #[test]
    fn test_normalize_ip_ipv6_different_suffix_same_prefix() {
        // Two IPv6 addresses with same /64 prefix but different interface identifiers
        // (simulating Privacy Extensions rotation)
        let ipv6_a = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0xabcd, 0x0001, 0x1111, 0x2222, 0x3333, 0x4444,
        ));
        let ipv6_b = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0xabcd, 0x0001, 0xaaaa, 0xbbbb, 0xcccc, 0xdddd,
        ));
        // Both should normalize to the same /64 prefix
        assert_eq!(normalize_ip(ipv6_a), normalize_ip(ipv6_b));
        assert_eq!(normalize_ip(ipv6_a), "2001:db8:abcd:1::");
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

        let ec_id = generate_ec_id(&settings, &noop_services()).expect("should generate EC ID");
        log::debug!("Generated EC ID: {}", ec_id);
        assert!(
            is_ec_id_format(&ec_id),
            "should match EC ID format: {{64hex}}.{{6alnum}}"
        );
    }

    #[test]
    fn test_generate_ec_id_uses_client_ip() {
        let settings = create_test_settings();
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));

        let id_with_ip = generate_ec_id(&settings, &noop_services_with_client_ip(ip))
            .expect("should generate EC ID with client IP");
        let id_without_ip = generate_ec_id(&settings, &noop_services())
            .expect("should generate EC ID without client IP");

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
            .expect("should reuse header EC ID");
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
            .expect("should reuse cookie EC ID");
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
            .expect("should reuse cookie EC ID from http request");

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
            .expect("should get or generate EC ID");
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
            .expect("should generate fresh ID on invalid header");
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
