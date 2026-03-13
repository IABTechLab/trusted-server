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

    log::info!("Input string for fresh ID: {} {}", input_string, data);

    let mut mac = HmacSha256::new_from_slice(settings.synthetic.secret_key.as_bytes())
        .change_context(TrustedServerError::SyntheticId {
            message: "Failed to create HMAC instance".to_string(),
        })?;
    mac.update(input_string.as_bytes());
    let hmac_hash = hex::encode(mac.finalize().into_bytes());

    // Append random 6-character alphanumeric suffix for additional uniqueness
    let random_suffix = generate_random_suffix(6);
    let synthetic_id = format!("{}.{}", hmac_hash, random_suffix);

    log::info!("Generated fresh ID: {}", synthetic_id);

    Ok(synthetic_id)
}

/// Gets or creates a synthetic ID from the request.
///
/// Attempts to retrieve an existing synthetic ID from:
/// 1. The `x-synthetic-id` header
/// 2. The `synthetic_id` cookie
///
/// If neither exists, generates a new synthetic ID.
///
/// # Errors
///
/// - [`TrustedServerError::Template`] if template rendering fails during generation
/// - [`TrustedServerError::SyntheticId`] if ID generation fails
pub fn get_synthetic_id(req: &Request) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(synthetic_id) = req
        .get_header(HEADER_X_SYNTHETIC_ID)
        .and_then(|h| h.to_str().ok())
    {
        let id = synthetic_id.to_string();
        log::info!("Using existing Synthetic ID from header: {}", id);
        return Ok(Some(id));
    }

    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get(COOKIE_SYNTHETIC_ID) {
                let id = cookie.value().to_string();
                log::info!("Using existing Trusted Server ID from cookie: {}", id);
                return Ok(Some(id));
            }
        }
        None => {
            log::debug!("No cookie header found in request");
        }
    }

    Ok(None)
}

/// Gets or creates a synthetic ID from the request.
///
/// Attempts to retrieve an existing synthetic ID from:
/// 1. The `x-synthetic-id` header
/// 2. The `synthetic_id` cookie
///
/// If neither exists, generates a new synthetic ID.
///
/// # Errors
///
/// Returns an error if template rendering fails during generation or if ID generation fails.
pub fn get_or_generate_synthetic_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    if let Some(id) = get_synthetic_id(req)? {
        return Ok(id);
    }

    // If no existing Synthetic ID found, generate a fresh one
    let synthetic_id = generate_synthetic_id(settings, req)?;
    log::info!("No existing synthetic_id, generated: {}", synthetic_id);
    Ok(synthetic_id)
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

    fn is_synthetic_id_format(value: &str) -> bool {
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
    fn test_generate_synthetic_id() {
        let settings: Settings = create_test_settings();
        let req = create_test_request(vec![
            (header::USER_AGENT, "Mozilla/5.0"),
            (header::ACCEPT_LANGUAGE, "en-US,en;q=0.9"),
            (header::ACCEPT_ENCODING, "gzip, deflate, br"),
        ]);

        let synthetic_id =
            generate_synthetic_id(&settings, &req).expect("should generate synthetic ID");
        log::info!("Generated synthetic ID: {}", synthetic_id);
        assert!(
            is_synthetic_id_format(&synthetic_id),
            "should match synthetic ID format"
        );
    }

    #[test]
    fn test_is_synthetic_id_format_accepts_valid_value() {
        let value = format!("{}.{}", "a".repeat(64), "Ab12z9");
        assert!(
            is_synthetic_id_format(&value),
            "should accept a valid synthetic ID format"
        );
    }

    #[test]
    fn test_is_synthetic_id_format_rejects_invalid_values() {
        let missing_suffix = "a".repeat(64);
        assert!(
            !is_synthetic_id_format(&missing_suffix),
            "should reject missing suffix"
        );

        let invalid_hex = format!("{}.{}", "a".repeat(63) + "g", "Ab12z9");
        assert!(
            !is_synthetic_id_format(&invalid_hex),
            "should reject non-hex HMAC content"
        );

        let invalid_suffix = format!("{}.{}", "a".repeat(64), "ab-129");
        assert!(
            !is_synthetic_id_format(&invalid_suffix),
            "should reject non-alphanumeric suffix"
        );

        let extra_segment = format!("{}.{}.{}", "a".repeat(64), "Ab12z9", "zz");
        assert!(
            !is_synthetic_id_format(&extra_segment),
            "should reject extra segments"
        );
    }

    #[test]
    fn test_get_synthetic_id_with_header() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(HEADER_X_SYNTHETIC_ID, "existing_synthetic_id")]);

        let synthetic_id = get_synthetic_id(&req).expect("should get synthetic ID");
        assert_eq!(synthetic_id, Some("existing_synthetic_id".to_string()));

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should reuse header synthetic ID");
        assert_eq!(synthetic_id, "existing_synthetic_id");
    }

    #[test]
    fn test_get_synthetic_id_with_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(
            header::COOKIE,
            &format!("{}=existing_cookie_id", COOKIE_SYNTHETIC_ID),
        )]);

        let synthetic_id = get_synthetic_id(&req).expect("should get synthetic ID");
        assert_eq!(synthetic_id, Some("existing_cookie_id".to_string()));

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should reuse cookie synthetic ID");
        assert_eq!(synthetic_id, "existing_cookie_id");
    }

    #[test]
    fn test_get_synthetic_id_none() {
        let req = create_test_request(vec![]);
        let synthetic_id = get_synthetic_id(&req).expect("should handle missing ID");
        assert!(synthetic_id.is_none());
    }

    #[test]
    fn test_get_or_generate_synthetic_id_generate_new() {
        let settings = create_test_settings();
        let req = create_test_request(vec![]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should get or generate synthetic ID");
        assert!(!synthetic_id.is_empty());
    }
}
