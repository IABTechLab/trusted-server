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
///
/// # Stability
///
/// The output format is a stable contract — EC hashes stored in KV depend
/// on it. Changing the format would invalidate all existing EC identities.
/// - **IPv4:** decimal-dotted notation (e.g. `"192.168.1.1"`)
/// - **IPv6:** first 4 segments as zero-padded lowercase hex without
///   separators (e.g. `"20010db885a30000"`)
pub(crate) fn normalize_ip(ip: IpAddr) -> String {
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
///
/// Fastly Compute's `wasm32-wasip1` runtime supplies OS randomness through
/// WASI for `rand::thread_rng`; the CI wasm release build verifies that this
/// entropy path remains available for the EC suffix contract.
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
/// **Important:** `client_ip` must be pre-normalized via [`extract_client_ip`].
/// Raw IPv6 addresses produce different hashes than their normalized /64
/// form, which would create duplicate identity graph entries.
///
/// # Errors
///
/// - [`TrustedServerError::EdgeCookie`] if HMAC generation fails
pub fn generate_ec_id(
    settings: &Settings,
    client_ip: &str,
) -> Result<String, Report<TrustedServerError>> {
    let mut mac = HmacSha256::new_from_slice(settings.ec.passphrase.expose().as_bytes())
        .change_context(TrustedServerError::EdgeCookie {
            message: "Failed to create HMAC instance".to_string(),
        })?;
    mac.update(client_ip.as_bytes());
    let hmac_hash = hex::encode(mac.finalize().into_bytes());

    // Append random 6-character alphanumeric suffix for additional uniqueness.
    let random_suffix = generate_random_suffix(6);
    let ec_id = format!("{hmac_hash}.{random_suffix}");

    log::trace!("Generated fresh EC ID: {}", super::log_id(&ec_id));

    Ok(ec_id)
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

/// Normalizes an EC ID for use as a KV key by lowercasing the hash prefix.
///
/// `hex::encode` (used in [`generate_ec_id`]) always produces lowercase hex,
/// so internal EC IDs are already lowercase. This normalization is a
/// defense-in-depth measure for EC IDs submitted by external partners
/// (via batch sync) that may use uppercase hex.
#[must_use]
pub fn normalize_ec_id_for_kv(ec_id: &str) -> String {
    let mut parts = ec_id.splitn(2, '.');
    let hash = parts.next().unwrap_or_default();
    let suffix = parts.next().unwrap_or_default();
    format!("{}.{}", hash.to_ascii_lowercase(), suffix)
}

/// Checks whether a string is a valid 64-character hex EC hash prefix.
///
/// Used by batch sync, finalize, and other modules that handle the
/// `{64hex}` portion of an EC ID independently. Accepts both uppercase
/// and lowercase hex; callers that require a specific case should
/// normalize before comparison.
#[must_use]
pub fn is_valid_ec_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Checks whether a string matches the expected EC ID format.
///
/// The format is `{64hex}.{6alnum}` where the first part is a 64-character
/// **lowercase** hex string and the second part is a 6-character alphanumeric
/// string. Only lowercase hex is accepted; callers must normalize before
/// validation to prevent duplicate KV keys from case-variant EC IDs. The HMAC
/// prefix is lowercase because it comes from `hex::encode`; the random suffix
/// allows mixed-case alphanumeric characters by construction.
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
        && hmac_part
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
    fn generate_same_ip_produces_consistent_hash_prefix() {
        let settings = create_test_settings();
        let first = generate_ec_id(&settings, "192.168.1.1").expect("should generate first EC ID");
        let second =
            generate_ec_id(&settings, "192.168.1.1").expect("should generate second EC ID");

        assert_eq!(
            ec_hash(&first),
            ec_hash(&second),
            "same IP and passphrase should produce the same HMAC prefix"
        );
        assert_ne!(
            first, second,
            "random suffix should differ between generated EC IDs"
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
    fn is_valid_ec_hash_accepts_64_hex() {
        assert!(is_valid_ec_hash(&"a".repeat(64)));
        assert!(is_valid_ec_hash(&"0123456789abcdef".repeat(4)));
    }

    #[test]
    fn is_valid_ec_hash_accepts_uppercase_hex() {
        assert!(
            is_valid_ec_hash(&"A".repeat(64)),
            "should accept uppercase hex (callers normalize before KV lookup)"
        );
    }

    #[test]
    fn is_valid_ec_hash_rejects_wrong_length() {
        assert!(!is_valid_ec_hash(&"a".repeat(63)));
        assert!(!is_valid_ec_hash(&"a".repeat(65)));
        assert!(!is_valid_ec_hash(""));
    }

    #[test]
    fn is_valid_ec_hash_rejects_non_hex() {
        let mut hash = "a".repeat(64);
        hash.replace_range(0..1, "g");
        assert!(!is_valid_ec_hash(&hash));
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
