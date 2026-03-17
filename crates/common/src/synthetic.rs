//! Synthetic ID generation using HMAC.
//!
//! This module provides functionality for generating privacy-preserving synthetic IDs
//! based on various request parameters and a secret key.

use std::net::IpAddr;

use error_stack::{Report, ResultExt};
use fastly::http::header;
use fastly::Request;
use handlebars::Handlebars;
use hmac::{Hmac, Mac};
use rand::Rng;
use serde_json::json;
use sha2::Sha256;
use uuid::Uuid;

use crate::constants::{COOKIE_SYNTHETIC_ID, HEADER_X_SYNTHETIC_ID};
use crate::cookies::handle_request_cookies;
use crate::error::TrustedServerError;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

const ALPHANUMERIC_CHARSET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Expected byte length of a valid synthetic ID: 64 hex chars + '.' + 6 alphanumeric chars.
const SYNTHETIC_ID_LEN: usize = 71;

/// Validates that `value` matches the canonical synthetic ID format.
///
/// The format is `<hmac>.<suffix>` where `<hmac>` is exactly 64 **lowercase** hex
/// characters (HMAC-SHA256 output via [`hex::encode`]) and `<suffix>` is exactly
/// 6 ASCII alphanumeric characters. Uppercase hex is rejected — the generator
/// never produces it and intermediaries that normalise case would produce an ID
/// that no longer matches its HMAC.
///
/// The total length is checked first so that oversized attacker-supplied
/// strings are rejected in O(1) before any character scanning occurs.
fn is_valid_synthetic_id(value: &str) -> bool {
    if value.len() != SYNTHETIC_ID_LEN {
        return false;
    }
    match value.split_once('.') {
        Some((hmac_part, suffix_part)) => {
            hmac_part.len() == 64
                && hmac_part
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                && suffix_part.bytes().all(|b| b.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Normalizes an IP address for stable synthetic ID generation.
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

/// Generates a fresh synthetic ID based on request parameters.
///
/// Creates an HMAC-SHA256-based ID using the configured secret key and request
/// attributes, then appends a random suffix for additional uniqueness.
///
/// # Errors
///
/// - [`TrustedServerError::Template`] if the template rendering fails
/// - [`TrustedServerError::SyntheticId`] if HMAC generation fails
pub fn generate_synthetic_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    let client_ip = req.get_client_ip_addr().map(normalize_ip);
    let user_agent = req
        .get_header(header::USER_AGENT)
        .map(|h| h.to_str().unwrap_or("unknown"));
    let accept_language = req
        .get_header(header::ACCEPT_LANGUAGE)
        .and_then(|h| h.to_str().ok())
        .map(|lang| lang.split(',').next().unwrap_or("unknown"));
    let accept_encoding = req
        .get_header(header::ACCEPT_ENCODING)
        .and_then(|h| h.to_str().ok());
    let random_uuid = Uuid::new_v4().to_string();

    let handlebars = Handlebars::new();
    let data = &json!({
        "client_ip": client_ip.unwrap_or("unknown".to_string()),
        "user_agent": user_agent.unwrap_or("unknown"),
        "accept_language": accept_language.unwrap_or("unknown"),
        "accept_encoding": accept_encoding.unwrap_or("unknown"),
        "random_uuid": random_uuid
    });

    let input_string = handlebars
        .render_template(&settings.synthetic.template, data)
        .change_context(TrustedServerError::Template {
            message: "Failed to render synthetic ID template".to_string(),
        })?;

    log::debug!("Generating fresh synthetic ID from template inputs");

    let mut mac = HmacSha256::new_from_slice(settings.synthetic.secret_key.as_bytes())
        .change_context(TrustedServerError::SyntheticId {
            message: "Failed to create HMAC instance".to_string(),
        })?;
    mac.update(input_string.as_bytes());
    let hmac_hash = hex::encode(mac.finalize().into_bytes());

    // Append random 6-character alphanumeric suffix for additional uniqueness
    let random_suffix = generate_random_suffix(6);
    let synthetic_id = format!("{}.{}", hmac_hash, random_suffix);

    debug_assert!(
        is_valid_synthetic_id(&synthetic_id),
        "should generate a synthetic ID matching the expected format"
    );

    log::debug!("Generated fresh synthetic ID");

    Ok(synthetic_id)
}

/// Reads a validated synthetic ID from the request, if one is present.
///
/// Checks the `x-synthetic-id` header first, then the `synthetic_id` cookie.
/// Values that do not match the canonical format (`<64-hex>.<6-alphanumeric>`)
/// are discarded and a warning is logged — the raw invalid value is never
/// included in log output.
///
/// Note: a non-UTF-8 `x-synthetic-id` header value is silently discarded and
/// the cookie is checked next, whereas a non-UTF-8 `Cookie` header propagates
/// as an error.
///
/// Returns `Ok(None)` when no valid ID is found, allowing the caller to
/// generate a fresh one.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if the Cookie header contains invalid UTF-8
pub fn get_synthetic_id(req: &Request) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(raw) = req
        .get_header(HEADER_X_SYNTHETIC_ID)
        .and_then(|h| h.to_str().ok())
    {
        if is_valid_synthetic_id(raw) {
            log::info!("Using existing synthetic ID from header");
            return Ok(Some(raw.to_string()));
        }
        log::warn!(
            "Rejecting synthetic ID from header: invalid format (len={})",
            raw.len()
        );
    }

    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get(COOKIE_SYNTHETIC_ID) {
                let raw = cookie.value();
                if is_valid_synthetic_id(raw) {
                    log::info!("Using existing synthetic ID from cookie");
                    return Ok(Some(raw.to_string()));
                }
                log::warn!(
                    "Rejecting synthetic ID from cookie: invalid format (len={})",
                    raw.len()
                );
            }
        }
        None => {
            log::debug!("No cookie header found in request");
        }
    }

    Ok(None)
}

/// Gets a validated synthetic ID from the request, or generates a fresh one.
///
/// Checks the `x-synthetic-id` header then the `synthetic_id` cookie via
/// [`get_synthetic_id`]. Values that fail format validation are silently
/// discarded — a warning is logged and a fresh ID is generated in their place,
/// identical to the no-ID-present path.
///
/// # Errors
///
/// - [`TrustedServerError::Template`] if template rendering fails during generation
/// - [`TrustedServerError::SyntheticId`] if HMAC generation fails
pub fn get_or_generate_synthetic_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    if let Some(id) = get_synthetic_id(req)? {
        return Ok(id);
    }

    // If no existing Synthetic ID found, generate a fresh one
    let synthetic_id = generate_synthetic_id(settings, req)?;
    log::info!("No existing synthetic ID found, generated a fresh one");
    Ok(synthetic_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::http::{HeaderName, HeaderValue};
    use std::net::{Ipv4Addr, Ipv6Addr};

    use crate::test_support::tests::{create_test_settings, VALID_SYNTHETIC_ID};

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

    #[test]
    fn test_generate_synthetic_id() {
        let settings: Settings = create_test_settings();
        let req = create_test_request(vec![
            (header::USER_AGENT, "Mozilla/5.0"),
            (header::ACCEPT_LANGUAGE, "en-US,en;q=0.9"),
            (header::ACCEPT_ENCODING, "gzip, deflate, br"),
        ]);

        let synthetic_id =
            generate_synthetic_id(&settings, &req).expect("should generate synthetic ID");
        assert!(
            is_valid_synthetic_id(&synthetic_id),
            "should match synthetic ID format"
        );
    }

    #[test]
    fn test_is_valid_synthetic_id_accepts_valid_value() {
        assert!(
            is_valid_synthetic_id(VALID_SYNTHETIC_ID),
            "should accept a well-formed synthetic ID"
        );
    }

    #[test]
    fn test_is_valid_synthetic_id_rejects_invalid_values() {
        let missing_suffix = "a".repeat(64);
        assert!(
            !is_valid_synthetic_id(&missing_suffix),
            "should reject missing suffix"
        );

        let invalid_hex = format!("{}.{}", "a".repeat(63) + "g", "Ab12z9");
        assert!(
            !is_valid_synthetic_id(&invalid_hex),
            "should reject non-hex HMAC content"
        );

        let invalid_suffix = format!("{}.{}", "a".repeat(64), "ab-129");
        assert!(
            !is_valid_synthetic_id(&invalid_suffix),
            "should reject non-alphanumeric suffix"
        );

        // 74 bytes — caught by the length guard before any scan.
        let extra_segment = format!("{}.{}.{}", "a".repeat(64), "Ab12z9", "zz");
        assert!(
            !is_valid_synthetic_id(&extra_segment),
            "should reject extra segments"
        );

        // 71 bytes, dot at position 64 (correct), but suffix contains a dot — caught by
        // the suffix alphanumeric scan, not the length guard.
        let dot_in_suffix = format!("{}.Ab12.z", "a".repeat(64));
        assert!(
            !is_valid_synthetic_id(&dot_in_suffix),
            "should reject dot within suffix"
        );

        let uppercase_hex = format!("{}.{}", "A".repeat(64), "Ab12z9");
        assert!(
            !is_valid_synthetic_id(&uppercase_hex),
            "should reject uppercase hex in HMAC part"
        );

        let oversized = "a".repeat(1000);
        assert!(
            !is_valid_synthetic_id(&oversized),
            "should reject oversized input"
        );

        assert!(!is_valid_synthetic_id(""), "should reject empty string");
    }

    #[test]
    fn test_get_synthetic_id_with_header() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(HEADER_X_SYNTHETIC_ID, VALID_SYNTHETIC_ID)]);

        let synthetic_id = get_synthetic_id(&req).expect("should get synthetic ID");
        assert_eq!(
            synthetic_id,
            Some(VALID_SYNTHETIC_ID.to_string()),
            "should return the valid header ID"
        );

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should reuse header synthetic ID");
        assert_eq!(
            synthetic_id, VALID_SYNTHETIC_ID,
            "should reuse the valid header ID"
        );
    }

    #[test]
    fn test_get_synthetic_id_with_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(
            header::COOKIE,
            &format!("{}={}", COOKIE_SYNTHETIC_ID, VALID_SYNTHETIC_ID),
        )]);

        let synthetic_id = get_synthetic_id(&req).expect("should get synthetic ID");
        assert_eq!(
            synthetic_id,
            Some(VALID_SYNTHETIC_ID.to_string()),
            "should return the valid cookie ID"
        );

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should reuse cookie synthetic ID");
        assert_eq!(
            synthetic_id, VALID_SYNTHETIC_ID,
            "should reuse the valid cookie ID"
        );
    }

    #[test]
    fn test_get_synthetic_id_rejects_invalid_header() {
        let req = create_test_request(vec![(HEADER_X_SYNTHETIC_ID, "not-a-valid-id")]);

        let synthetic_id = get_synthetic_id(&req).expect("should not error on invalid header ID");
        assert!(
            synthetic_id.is_none(),
            "should discard invalid synthetic ID from header"
        );
    }

    #[test]
    fn test_get_synthetic_id_rejects_invalid_cookie() {
        let req = create_test_request(vec![(
            header::COOKIE,
            &format!("{}=not-a-valid-id", COOKIE_SYNTHETIC_ID),
        )]);

        let synthetic_id = get_synthetic_id(&req).expect("should not error on invalid cookie ID");
        assert!(
            synthetic_id.is_none(),
            "should discard invalid synthetic ID from cookie"
        );
    }

    #[test]
    fn test_get_synthetic_id_invalid_header_falls_through_to_valid_cookie() {
        let req = create_test_request(vec![
            (HEADER_X_SYNTHETIC_ID, "not-a-valid-id"),
            (
                header::COOKIE,
                &format!("{}={}", COOKIE_SYNTHETIC_ID, VALID_SYNTHETIC_ID),
            ),
        ]);

        let synthetic_id = get_synthetic_id(&req).expect("should not error when cookie is valid");
        assert_eq!(
            synthetic_id,
            Some(VALID_SYNTHETIC_ID.to_string()),
            "should fall through to valid cookie when header ID is invalid"
        );
    }

    #[test]
    fn test_get_synthetic_id_header_takes_precedence_over_cookie() {
        let cookie_id = "b2a1c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0b1a2.Zx98y7";
        let req = create_test_request(vec![
            (HEADER_X_SYNTHETIC_ID, VALID_SYNTHETIC_ID),
            (
                header::COOKIE,
                &format!("{}={}", COOKIE_SYNTHETIC_ID, cookie_id),
            ),
        ]);
        let result = get_synthetic_id(&req).expect("should succeed");
        assert_eq!(
            result,
            Some(VALID_SYNTHETIC_ID.to_string()),
            "should prefer header over cookie"
        );
    }

    #[test]
    fn test_get_synthetic_id_none() {
        let req = create_test_request(vec![]);
        let synthetic_id = get_synthetic_id(&req).expect("should handle missing ID");
        assert!(
            synthetic_id.is_none(),
            "should return None when no ID present"
        );
    }

    #[test]
    fn test_get_or_generate_synthetic_id_generates_when_invalid_header() {
        let settings = create_test_settings();
        // A string that is clearly not a valid synthetic ID (wrong format, wrong length)
        let req = create_test_request(vec![(HEADER_X_SYNTHETIC_ID, "totally-invalid-id-value")]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should generate when header ID is invalid");
        assert!(
            is_valid_synthetic_id(&synthetic_id),
            "should generate a fresh valid ID when inbound ID is invalid"
        );
    }

    #[test]
    fn test_get_or_generate_synthetic_id_generate_new() {
        let settings = create_test_settings();
        let req = create_test_request(vec![]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should get or generate synthetic ID");
        assert!(
            is_valid_synthetic_id(&synthetic_id),
            "should generate a valid synthetic ID"
        );
    }
}
