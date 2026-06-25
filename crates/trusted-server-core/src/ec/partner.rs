//! Partner validation helpers and API key hashing.
//!
//! Provides source-domain normalization for EC partner configuration and
//! API key hashing. The partner registry is in [`super::registry`].

use sha2::{Digest as _, Sha256};

/// Maximum allowed length for partner source domains.
const MAX_SOURCE_DOMAIN_LENGTH: usize = 255;

/// Normalizes a partner source domain for use as the canonical EC partner key.
///
/// The returned value is lowercase ASCII with any trailing dot removed. It is
/// suitable for matching `OpenRTB` EID `source` values and for use as the EC KV
/// `ids` map key.
///
/// # Errors
///
/// Returns a descriptive error when the value is not a plain hostname.
pub fn normalize_partner_source_domain(source_domain: &str) -> Result<String, String> {
    let trimmed = source_domain.trim();
    if trimmed.is_empty() {
        return Err("source_domain must not be empty".to_owned());
    }
    if trimmed != source_domain {
        return Err(format!(
            "source_domain must not contain leading or trailing whitespace, got: '{source_domain}'"
        ));
    }
    if trimmed.len() > MAX_SOURCE_DOMAIN_LENGTH {
        return Err(format!(
            "source_domain exceeds {MAX_SOURCE_DOMAIN_LENGTH} bytes, got: '{source_domain}'"
        ));
    }
    if !trimmed.is_ascii() {
        return Err(format!(
            "source_domain must be ASCII, got: '{source_domain}'"
        ));
    }
    if trimmed.contains("://") || trimmed.contains('/') || trimmed.contains(':') {
        return Err(format!(
            "source_domain must be a hostname without scheme, path, or port, got: '{source_domain}'"
        ));
    }

    let normalized = trimmed.trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() || normalized.len() > MAX_SOURCE_DOMAIN_LENGTH {
        return Err(format!("source_domain is invalid, got: '{source_domain}'"));
    }

    let mut saw_label = false;
    for label in normalized.split('.') {
        saw_label = true;
        if label.is_empty() || label.len() > 63 {
            return Err(format!(
                "source_domain has invalid label, got: '{source_domain}'"
            ));
        }

        let bytes = label.as_bytes();
        let Some(first) = bytes.first().copied() else {
            return Err(format!(
                "source_domain has invalid label, got: '{source_domain}'"
            ));
        };
        let Some(last) = bytes.last().copied() else {
            return Err(format!(
                "source_domain has invalid label, got: '{source_domain}'"
            ));
        };
        if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
            return Err(format!(
                "source_domain has invalid label, got: '{source_domain}'"
            ));
        }
        if !bytes
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!(
                "source_domain has invalid label, got: '{source_domain}'"
            ));
        }
    }

    if !saw_label {
        return Err(format!("source_domain is invalid, got: '{source_domain}'"));
    }

    Ok(normalized)
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
    fn normalize_partner_source_domain_lowercases_and_trims_trailing_dot() {
        let normalized = normalize_partner_source_domain("SSP.Example.Com.")
            .expect("should normalize source domain");

        assert_eq!(normalized, "ssp.example.com");
    }

    #[test]
    fn normalize_partner_source_domain_rejects_invalid_values() {
        for source_domain in [
            "",
            " ssp.example.com",
            "ssp.example.com ",
            "https://ssp.example.com",
            "ssp.example.com/path",
            "ssp.example.com:443",
            "bad_domain.example.com",
            "-bad.example.com",
            "bad-.example.com",
            "bad..example.com",
        ] {
            assert!(
                normalize_partner_source_domain(source_domain).is_err(),
                "should reject invalid source_domain {source_domain:?}"
            );
        }
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
        let hash1 = hash_api_key("key1");
        let hash2 = hash_api_key("key2");
        assert_ne!(hash1, hash2, "should produce different hashes");
    }
}
