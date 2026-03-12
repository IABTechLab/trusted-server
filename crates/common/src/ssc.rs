//! Server Side Cookie (SSC) ID generation using HMAC.
//!
//! This module provides functionality for generating privacy-preserving SSC IDs
//! based on the client IP address and a secret key.

use std::net::IpAddr;

use error_stack::{Report, ResultExt};
use fastly::Request;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

use crate::constants::{COOKIE_TS_SSC, HEADER_X_TS_SSC};
use crate::cookies::handle_request_cookies;
use crate::error::TrustedServerError;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

const ALPHANUMERIC_CHARSET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Normalizes an IP address for stable SSC ID generation.
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

/// Generates a fresh SSC ID based on client IP address.
///
/// Creates an HMAC-SHA256-based ID using the configured secret key and
/// the client IP address, then appends a random suffix for additional
/// uniqueness. The resulting format is `{64hex}.{6alnum}`.
///
/// # Errors
///
/// - [`TrustedServerError::Ssc`] if HMAC generation fails
pub fn generate_ssc_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    let client_ip = req
        .get_client_ip_addr()
        .map(normalize_ip)
        .unwrap_or_else(|| "unknown".to_string());

    log::debug!("Input for fresh SSC ID: client_ip={}", client_ip);

    let mut mac = HmacSha256::new_from_slice(settings.ssc.secret_key.as_bytes()).change_context(
        TrustedServerError::Ssc {
            message: "Failed to create HMAC instance".to_string(),
        },
    )?;
    mac.update(client_ip.as_bytes());
    let hmac_hash = hex::encode(mac.finalize().into_bytes());

    // Append random 6-character alphanumeric suffix for additional uniqueness
    let random_suffix = generate_random_suffix(6);
    let ssc_id = format!("{hmac_hash}.{random_suffix}");

    log::debug!("Generated fresh SSC ID: {}", ssc_id);

    Ok(ssc_id)
}

/// Gets an existing SSC ID from the request.
///
/// Attempts to retrieve an existing SSC ID from:
/// 1. The `x-ts-ssc` header
/// 2. The `ts-ssc` cookie
///
/// Returns `None` if neither source contains an SSC ID.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
pub fn get_ssc_id(req: &Request) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(ssc_id) = req
        .get_header(HEADER_X_TS_SSC)
        .and_then(|h| h.to_str().ok())
    {
        let id = ssc_id.to_string();
        log::debug!("Using existing SSC ID from header: {}", id);
        return Ok(Some(id));
    }

    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get(COOKIE_TS_SSC) {
                let id = cookie.value().to_string();
                log::debug!("Using existing SSC ID from cookie: {}", id);
                return Ok(Some(id));
            }
        }
        None => {
            log::debug!("No cookie header found in request");
        }
    }

    Ok(None)
}

/// Gets or creates an SSC ID from the request.
///
/// Attempts to retrieve an existing SSC ID from:
/// 1. The `x-ts-ssc` header
/// 2. The `ts-ssc` cookie
///
/// If neither exists, generates a new SSC ID.
///
/// # Errors
///
/// Returns an error if ID generation fails.
pub fn get_or_generate_ssc_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    if let Some(id) = get_ssc_id(req)? {
        return Ok(id);
    }

    // If no existing SSC ID found, generate a fresh one
    let ssc_id = generate_ssc_id(settings, req)?;
    log::debug!("No existing SSC ID, generated: {}", ssc_id);
    Ok(ssc_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::http::{HeaderName, HeaderValue};
    use std::net::{Ipv4Addr, Ipv6Addr};

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

    fn create_test_request(headers: Vec<(HeaderName, &str)>) -> Request {
        let mut req = Request::new("GET", "http://example.com");
        for (key, value) in headers {
            req.set_header(
                key,
                HeaderValue::from_str(value).expect("should create valid header value"),
            );
        }

        req
    }

    fn is_ssc_id_format(value: &str) -> bool {
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
    fn test_generate_ssc_id() {
        let settings: Settings = create_test_settings();
        let req = create_test_request(vec![]);

        let ssc_id = generate_ssc_id(&settings, &req).expect("should generate SSC ID");
        log::info!("Generated SSC ID: {}", ssc_id);
        assert!(
            is_ssc_id_format(&ssc_id),
            "should match SSC ID format: {{64hex}}.{{6alnum}}"
        );
    }

    #[test]
    fn test_is_ssc_id_format_accepts_valid_value() {
        let value = format!("{}.{}", "a".repeat(64), "Ab12z9");
        assert!(
            is_ssc_id_format(&value),
            "should accept a valid SSC ID format"
        );
    }

    #[test]
    fn test_is_ssc_id_format_rejects_invalid_values() {
        let missing_suffix = "a".repeat(64);
        assert!(
            !is_ssc_id_format(&missing_suffix),
            "should reject missing suffix"
        );

        let invalid_hex = format!("{}.{}", "a".repeat(63) + "g", "Ab12z9");
        assert!(
            !is_ssc_id_format(&invalid_hex),
            "should reject non-hex HMAC content"
        );

        let invalid_suffix = format!("{}.{}", "a".repeat(64), "ab-129");
        assert!(
            !is_ssc_id_format(&invalid_suffix),
            "should reject non-alphanumeric suffix"
        );

        let extra_segment = format!("{}.{}.{}", "a".repeat(64), "Ab12z9", "zz");
        assert!(
            !is_ssc_id_format(&extra_segment),
            "should reject extra segments"
        );
    }

    #[test]
    fn test_get_ssc_id_with_header() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(HEADER_X_TS_SSC, "existing_ssc_id")]);

        let ssc_id = get_ssc_id(&req).expect("should get SSC ID");
        assert_eq!(ssc_id, Some("existing_ssc_id".to_string()));

        let ssc_id = get_or_generate_ssc_id(&settings, &req).expect("should reuse header SSC ID");
        assert_eq!(ssc_id, "existing_ssc_id");
    }

    #[test]
    fn test_get_ssc_id_with_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(
            fastly::http::header::COOKIE,
            &format!("{}=existing_cookie_id", COOKIE_TS_SSC),
        )]);

        let ssc_id = get_ssc_id(&req).expect("should get SSC ID");
        assert_eq!(ssc_id, Some("existing_cookie_id".to_string()));

        let ssc_id = get_or_generate_ssc_id(&settings, &req).expect("should reuse cookie SSC ID");
        assert_eq!(ssc_id, "existing_cookie_id");
    }

    #[test]
    fn test_get_ssc_id_none() {
        let req = create_test_request(vec![]);
        let ssc_id = get_ssc_id(&req).expect("should handle missing ID");
        assert!(ssc_id.is_none());
    }

    #[test]
    fn test_get_or_generate_ssc_id_generate_new() {
        let settings = create_test_settings();
        let req = create_test_request(vec![]);

        let ssc_id =
            get_or_generate_ssc_id(&settings, &req).expect("should get or generate SSC ID");
        assert!(!ssc_id.is_empty());
    }
}
