//! Edge Cookie (EC) ID generation using HMAC.
//!
//! This module provides functionality for generating privacy-preserving EC IDs
//! based on the client IP address and a secret key.

use std::net::IpAddr;

use error_stack::{Report, ResultExt};
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

use crate::error::TrustedServerError;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

const ALPHANUMERIC_CHARSET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Normalizes an IP address for stable EC ID generation.
///
/// For IPv6 addresses, masks to /64 prefix to handle Privacy Extensions
/// where devices rotate their interface identifier (lower 64 bits).
/// The first 4 segments are hex-encoded without separators.
/// IPv4 addresses are returned unchanged.
fn normalize_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ipv4) => ipv4.to_string(),
        IpAddr::V6(ipv6) => {
            let segments = ipv6.segments();
            // Keep only the first 4 segments (64 bits) for /64 prefix.
            // Concatenate as zero-padded hex without separators.
            format!(
                "{:04x}{:04x}{:04x}{:04x}",
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

/// Generates a fresh EC ID from a pre-captured client IP string.
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
    client_ip: &str,
) -> Result<String, Report<TrustedServerError>> {
    log::trace!("Input for fresh EC ID: client_ip={client_ip}");

    let mut mac = HmacSha256::new_from_slice(settings.ec.passphrase.expose().as_bytes())
        .change_context(TrustedServerError::Ec {
            message: "Failed to create HMAC instance".to_string(),
        })?;
    mac.update(client_ip.as_bytes());
    let hmac_hash = hex::encode(mac.finalize().into_bytes());

    // Append random 6-character alphanumeric suffix for additional uniqueness.
    let random_suffix = generate_random_suffix(6);
    let ec_id = format!("{hmac_hash}.{random_suffix}");

    log::trace!("Generated fresh EC ID: {ec_id}");

    Ok(ec_id)
}

/// Extracts and normalizes the client IP from a request.
///
/// Returns the normalized IP as a string suitable for HMAC input.
///
/// # Errors
///
/// Returns [`TrustedServerError::Ec`] when the client IP is unavailable
/// (e.g. in certain test or proxy configurations). EC generation requires
/// a valid client IP — there is no fallback.
pub fn extract_client_ip(req: &fastly::Request) -> Result<String, Report<TrustedServerError>> {
    req.get_client_ip_addr().map(normalize_ip).ok_or_else(|| {
        Report::new(TrustedServerError::Ec {
            message: "Client IP required for EC generation but unavailable".to_string(),
        })
    })
}

/// Extracts the stable 64-character hex prefix from an EC ID.
///
/// Given an EC ID in `{64hex}.{6alnum}` format, returns the `{64hex}`
/// portion. If the ID does not contain a dot separator, returns the
/// entire string.
#[must_use]
pub fn ec_hash(ec_id: &str) -> &str {
    // Find the dot separator; if absent, return the entire string.
    match ec_id.find('.') {
        Some(pos) => &ec_id[..pos],
        None => ec_id,
    }
}

/// Checks whether a string matches the expected EC ID format.
///
/// The format is `{64hex}.{6alnum}` where the first part is a 64-character
/// lowercase hex string and the second part is a 6-character alphanumeric
/// string.
#[must_use]
pub fn is_valid_ec_id(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(hmac_part) = parts.next() else {
        return false;
    };
    let Some(suffix_part) = parts.next() else {
        return false;
    };

    // Must have exactly two segments.
    if parts.next().is_some() {
        return false;
    }

    hmac_part.len() == 64
        && suffix_part.len() == 6
        && hmac_part.bytes().all(|b| b.is_ascii_hexdigit())
        && suffix_part.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    use crate::test_support::tests::create_test_settings;

    #[test]
    fn normalize_ipv4_unchanged() {
        let ipv4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(normalize_ip(ipv4), "192.168.1.100");
    }

    #[test]
    fn normalize_ipv6_masks_to_64_no_separators() {
        let ipv6 = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0x85a3, 0x0000, 0x8a2e, 0x0370, 0x7334, 0x1234,
        ));
        assert_eq!(
            normalize_ip(ipv6),
            "20010db885a30000",
            "should concatenate first 4 segments as zero-padded hex without separators"
        );
    }

    #[test]
    fn normalize_ipv6_different_suffix_same_prefix() {
        // Two IPv6 addresses with same /64 prefix but different interface identifiers
        // (simulating Privacy Extensions rotation).
        let ipv6_a = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0xabcd, 0x0001, 0x1111, 0x2222, 0x3333, 0x4444,
        ));
        let ipv6_b = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0xabcd, 0x0001, 0xaaaa, 0xbbbb, 0xcccc, 0xdddd,
        ));
        assert_eq!(
            normalize_ip(ipv6_a),
            normalize_ip(ipv6_b),
            "should normalize to the same /64 prefix"
        );
        assert_eq!(normalize_ip(ipv6_a), "20010db8abcd0001");
    }

    #[test]
    fn generate_produces_valid_format() {
        let settings = create_test_settings();
        let ec_id = generate_ec_id(&settings, "192.168.1.1").expect("should generate EC ID");
        assert!(
            is_valid_ec_id(&ec_id),
            "should match EC ID format: {{64hex}}.{{6alnum}}, got: {ec_id}"
        );
    }

    #[test]
    fn ec_hash_extracts_prefix() {
        let id = format!("{}.Ab12z9", "a".repeat(64));
        assert_eq!(ec_hash(&id), "a".repeat(64));
    }

    #[test]
    fn ec_hash_returns_full_string_without_dot() {
        assert_eq!(ec_hash("nodot"), "nodot");
    }

    #[test]
    fn is_valid_ec_id_accepts_valid() {
        let value = format!("{}.Ab12z9", "a".repeat(64));
        assert!(is_valid_ec_id(&value), "should accept a valid EC ID format");
    }

    #[test]
    fn is_valid_ec_id_rejects_missing_suffix() {
        let missing_suffix = "a".repeat(64);
        assert!(
            !is_valid_ec_id(&missing_suffix),
            "should reject missing suffix"
        );
    }

    #[test]
    fn is_valid_ec_id_rejects_invalid_hex() {
        let invalid_hex = format!("{}.Ab12z9", "a".repeat(63) + "g");
        assert!(
            !is_valid_ec_id(&invalid_hex),
            "should reject non-hex HMAC content"
        );
    }

    #[test]
    fn is_valid_ec_id_rejects_invalid_suffix() {
        let invalid_suffix = format!("{}.ab-129", "a".repeat(64));
        assert!(
            !is_valid_ec_id(&invalid_suffix),
            "should reject non-alphanumeric suffix"
        );
    }

    #[test]
    fn is_valid_ec_id_rejects_extra_segments() {
        let extra_segment = format!("{}.Ab12z9.zz", "a".repeat(64));
        assert!(
            !is_valid_ec_id(&extra_segment),
            "should reject extra segments"
        );
    }
}
