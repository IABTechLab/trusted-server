//! Authenticated issuance timestamps for EC cookie refreshes.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit as _, Mac};
use sha2::Sha256;

pub(crate) const MAX_MARKER_BYTES: usize = 80;
const AUTHENTICATION_PREFIX: &[u8] = b"trusted-server/ec-cookie-issued-at/v1\0";

/// Encodes an authenticated issuance timestamp bound to the publisher and full EC ID.
///
/// Returns `None` when a field cannot fit the authenticated length encoding.
pub(crate) fn encode_marker(
    passphrase: &str,
    publisher: &str,
    ec_id: &str,
    now: u64,
) -> Option<String> {
    let timestamp = now.to_string();
    let mac = authenticated_mac(passphrase, publisher, ec_id, &timestamp)?;
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Some(format!("v1.i.{timestamp}.{signature}"))
}

/// Checks whether the issuance marker requires a same-EC cookie refresh.
///
/// Missing, malformed, unauthenticated, or future markers require a refresh.
/// Valid markers become due when their age reaches `after_seconds`.
pub(crate) fn should_refresh(
    passphrase: &str,
    publisher: &str,
    ec_id: &str,
    marker: Option<&str>,
    now: u64,
    after_seconds: u64,
) -> bool {
    marker_age(passphrase, publisher, ec_id, marker, now).is_none_or(|age| age >= after_seconds)
}

fn marker_age(
    passphrase: &str,
    publisher: &str,
    ec_id: &str,
    marker: Option<&str>,
    now: u64,
) -> Option<u64> {
    let marker = marker.filter(|value| value.len() <= MAX_MARKER_BYTES)?;
    let mut fields = marker.split('.');
    if fields.next()? != "v1" || fields.next()? != "i" {
        return None;
    }
    let timestamp = fields.next()?;
    let signature = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let issued_at = timestamp.parse::<u64>().ok()?;
    if issued_at.to_string() != timestamp {
        return None;
    }
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    let mac = authenticated_mac(passphrase, publisher, ec_id, timestamp)?;
    mac.verify_slice(&signature).ok()?;
    now.checked_sub(issued_at)
}

fn authenticated_mac(
    passphrase: &str,
    publisher: &str,
    ec_id: &str,
    timestamp: &str,
) -> Option<Hmac<Sha256>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(passphrase.as_bytes()).ok()?;
    mac.update(AUTHENTICATION_PREFIX);
    let publisher = publisher.to_ascii_lowercase();
    for field in [
        publisher.as_bytes(),
        ec_id.as_bytes(),
        b"i",
        timestamp.as_bytes(),
    ] {
        let length = u32::try_from(field.len()).ok()?;
        mac.update(&length.to_be_bytes());
        mac.update(field);
    }
    Some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSPHRASE: &str = "example-passphrase";
    const PUBLISHER: &str = "example.com";
    const EC_ID: &str = "ec-example-AbCd";
    const ISSUED_AT: u64 = 1_700_000_000;
    const VECTOR: &str = "v1.i.1700000000.WaVVZ8P1cJiw3WSQ--FOvpGBOXWjUoeojp5cnDtVqvc";

    #[test]
    fn matches_independent_python_hmac_vector() {
        assert_eq!(
            encode_marker(PASSPHRASE, "EXAMPLE.COM", EC_ID, ISSUED_AT).as_deref(),
            Some(VECTOR),
            "should match Python standard-library HMAC-SHA256 with length-prefixed fields"
        );
    }

    #[test]
    fn refreshes_at_the_exact_age_threshold() {
        for (now, expected) in [
            (ISSUED_AT, false),
            (ISSUED_AT + 59, false),
            (ISSUED_AT + 60, true),
            (ISSUED_AT + 61, true),
        ] {
            assert_eq!(
                should_refresh(PASSPHRASE, PUBLISHER, EC_ID, Some(VECTOR), now, 60),
                expected,
                "should refresh only at or beyond the threshold at {now}"
            );
        }
        assert!(
            should_refresh(PASSPHRASE, PUBLISHER, EC_ID, Some(VECTOR), ISSUED_AT, 0),
            "should refresh immediately when the threshold is zero"
        );
    }

    #[test]
    fn requires_the_same_key_publisher_and_case_sensitive_complete_ec() {
        for (passphrase, publisher, ec_id, expected) in [
            (PASSPHRASE, "EXAMPLE.COM", EC_ID, false),
            ("other-example-passphrase", PUBLISHER, EC_ID, true),
            (PASSPHRASE, "other.example.com", EC_ID, true),
            (PASSPHRASE, PUBLISHER, "ec-example-abcd", true),
            (PASSPHRASE, PUBLISHER, "ec-example-AbCd-extra", true),
        ] {
            assert_eq!(
                should_refresh(passphrase, publisher, ec_id, Some(VECTOR), ISSUED_AT, 60),
                expected,
                "should bind the complete identity and normalize only publisher ASCII case"
            );
        }
    }

    #[test]
    fn bootstraps_missing_future_or_invalid_markers() {
        let oversized = "a".repeat(81);
        let padded = format!("{VECTOR}=");
        let extra_field = format!("{VECTOR}.extra");
        let damaged = VECTOR.replace("WaVV", "XaVV");
        for marker in [
            None,
            Some(""),
            Some(oversized.as_str()),
            Some(padded.as_str()),
            Some(extra_field.as_str()),
            Some(damaged.as_str()),
            Some("v2.i.1700000000.WaVVZ8P1cJiw3WSQ--FOvpGBOXWjUoeojp5cnDtVqvc"),
            Some("v1.b.1700000000.WaVVZ8P1cJiw3WSQ--FOvpGBOXWjUoeojp5cnDtVqvc"),
            Some("v1.i.01700000000.WaVVZ8P1cJiw3WSQ--FOvpGBOXWjUoeojp5cnDtVqvc"),
            Some("v1.i.+1700000000.WaVVZ8P1cJiw3WSQ--FOvpGBOXWjUoeojp5cnDtVqvc"),
            Some("v1.i.18446744073709551616.WaVVZ8P1cJiw3WSQ--FOvpGBOXWjUoeojp5cnDtVqvc"),
            Some("v1.i.1700000000.invalid"),
            Some("v1.i.1700000000.WaVVZ8P1cJiw3WSQ--FOvpGBOXWjUoeojp5cnDtVqvd"),
        ] {
            assert!(
                should_refresh(PASSPHRASE, PUBLISHER, EC_ID, marker, ISSUED_AT, 60),
                "should bootstrap an absent or invalid marker: {marker:?}"
            );
        }
        assert!(
            should_refresh(
                PASSPHRASE,
                PUBLISHER,
                EC_ID,
                Some(VECTOR),
                ISSUED_AT - 1,
                60
            ),
            "should bootstrap a correctly signed future timestamp"
        );
    }

    #[test]
    fn supports_canonical_zero_and_maximum_timestamps() {
        for issued_at in [0, u64::MAX] {
            let marker = encode_marker(PASSPHRASE, PUBLISHER, EC_ID, issued_at)
                .expect("should encode all representable timestamps");
            assert!(marker.len() <= 80, "should fit the bounded marker format");
            assert!(
                !should_refresh(PASSPHRASE, PUBLISHER, EC_ID, Some(&marker), issued_at, 1),
                "should accept canonical timestamps without overflowing"
            );
        }
    }
}
