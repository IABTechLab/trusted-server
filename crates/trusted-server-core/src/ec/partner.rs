//! Partner validation helpers and ID hashing.
//!
//! Provides partner ID format validation, reserved name checks, and
//! API key hashing. The actual partner registry is in [`super::registry`].

use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// Regex pattern for valid partner IDs.
/// Lowercase alphanumeric, hyphens, underscores; 1-32 characters.
const PARTNER_ID_PATTERN: &str = r"^[a-z0-9_-]{1,32}$";

/// Reserved partner IDs that would collide with managed `X-ts-*` headers.
const RESERVED_PARTNER_IDS: &[&str] = &[
    "ec",
    "eids",
    "ec-consent",
    "eids-truncated",
    "synthetic",
    "ts",
    "version",
    "env",
];

/// Cached compiled regex for partner ID validation.
static PARTNER_ID_REGEX: OnceLock<Result<Regex, String>> = OnceLock::new();

fn partner_id_regex() -> Result<&'static Regex, String> {
    PARTNER_ID_REGEX
        .get_or_init(|| {
            Regex::new(PARTNER_ID_PATTERN)
                .map_err(|e| format!("internal error compiling partner ID regex: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Validates a partner ID format and checks against reserved names.
///
/// # Errors
///
/// Returns a descriptive error string on validation failure.
pub fn validate_partner_id(id: &str) -> Result<(), String> {
    let re = partner_id_regex()?;
    if !re.is_match(id) {
        return Err(format!(
            "partner ID must match {PARTNER_ID_PATTERN}, got: '{id}'"
        ));
    }
    if RESERVED_PARTNER_IDS.contains(&id) {
        return Err(format!("partner ID '{id}' is reserved"));
    }
    Ok(())
}

/// Computes the SHA-256 hex digest of an API key.
#[must_use]
pub fn hash_api_key(api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_partner_id_accepts_valid_ids() {
        assert!(
            validate_partner_id("ssp_x").is_ok(),
            "should accept underscored ID"
        );
        assert!(
            validate_partner_id("dsp-y").is_ok(),
            "should accept hyphenated ID"
        );
        assert!(
            validate_partner_id("liveramp").is_ok(),
            "should accept lowercase alpha"
        );
        assert!(
            validate_partner_id("id5").is_ok(),
            "should accept alphanumeric"
        );
    }

    #[test]
    fn validate_partner_id_rejects_invalid_ids() {
        assert!(validate_partner_id("").is_err(), "should reject empty ID");
        assert!(
            validate_partner_id("SSP").is_err(),
            "should reject uppercase"
        );
        assert!(
            validate_partner_id("a".repeat(33).as_str()).is_err(),
            "should reject >32 chars"
        );
        assert!(
            validate_partner_id("has space").is_err(),
            "should reject spaces"
        );
    }

    #[test]
    fn validate_partner_id_rejects_reserved_ids() {
        assert!(
            validate_partner_id("ec").is_err(),
            "should reject reserved 'ec'"
        );
        assert!(
            validate_partner_id("ts").is_err(),
            "should reject reserved 'ts'"
        );
        assert!(
            validate_partner_id("eids").is_err(),
            "should reject reserved 'eids'"
        );
    }

    #[test]
    fn hash_api_key_produces_hex_digest() {
        let hash = hash_api_key("test-key");
        assert_eq!(hash.len(), 64, "should produce 64-char hex digest");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "should only contain hex characters"
        );
    }

    #[test]
    fn hash_api_key_is_deterministic() {
        let hash1 = hash_api_key("same-key");
        let hash2 = hash_api_key("same-key");
        assert_eq!(hash1, hash2, "should produce same hash for same input");
    }

    #[test]
    fn hash_api_key_differs_for_different_keys() {
        let hash1 = hash_api_key("key-a");
        let hash2 = hash_api_key("key-b");
        assert_ne!(
            hash1, hash2,
            "should produce different hashes for different inputs"
        );
    }
}
