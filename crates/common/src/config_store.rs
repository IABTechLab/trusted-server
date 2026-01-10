//! Platform-agnostic configuration store abstraction.
//!
//! This module provides a trait for accessing configuration from edge platform
//! key-value stores (Fastly Config Store, Cloudflare KV, Akamai EdgeKV).
//!
//! # Config Store Keys
//!
//! The following keys are standardized:
//! - `settings` - The UTF-8 TOML configuration payload
//! - `settings-hash` - SHA-256 hash of the settings bytes (`sha256:<hex>`)
//! - `settings-signature` - Optional DSSE envelope signing the settings
//! - `settings-metadata` - Optional JSON with version, timestamps, and policy info

use sha2::{Digest, Sha256};

use crate::error::TrustedServerError;

/// Key for the main settings TOML payload.
pub const SETTINGS_KEY: &str = "settings";

/// Key for the SHA-256 hash of the settings.
pub const SETTINGS_HASH_KEY: &str = "settings-hash";

/// Key for the DSSE signature envelope.
pub const SETTINGS_SIGNATURE_KEY: &str = "settings-signature";

/// Key for metadata (version, timestamps, policy).
pub const SETTINGS_METADATA_KEY: &str = "settings-metadata";

/// Platform-agnostic configuration store trait.
///
/// Implementations provide access to key-value stores on different edge platforms:
/// - Fastly: Config Store
/// - Cloudflare: Workers KV
/// - Akamai: EdgeKV
pub trait ConfigStore {
    /// Retrieve a value by key.
    ///
    /// Returns `Ok(Some(value))` if the key exists,
    /// `Ok(None)` if the key doesn't exist,
    /// or `Err` if there was an error accessing the store.
    fn get(&self, key: &str) -> Result<Option<String>, TrustedServerError>;
}

/// Metadata about the configuration stored in `settings-metadata`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SettingsMetadata {
    /// Version identifier (monotonic or timestamp).
    pub version: String,
    /// When the config was published (RFC3339).
    pub published_at: String,
    /// Optional validity window end (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    /// Optional policy identifier for vendor compliance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
}

/// Compute the SHA-256 hash of configuration bytes.
///
/// Returns the hash in the format `sha256:<hex>`.
pub fn compute_settings_hash(content: &str) -> String {
    // Normalize line endings for consistent hashing across platforms
    let normalized = content.replace("\r\n", "\n");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let hash = hasher.finalize();
    format!("sha256:{}", hex::encode(hash))
}

/// Verify that a settings hash matches the content.
pub fn verify_settings_hash(content: &str, expected_hash: &str) -> bool {
    let computed = compute_settings_hash(content);
    computed == expected_hash
}

/// Load and parse settings metadata from the config store.
pub fn load_settings_metadata<S: ConfigStore>(
    store: &S,
) -> Result<Option<SettingsMetadata>, TrustedServerError> {
    match store.get(SETTINGS_METADATA_KEY)? {
        Some(json_str) => {
            let metadata: SettingsMetadata =
                serde_json::from_str(&json_str).map_err(|e| TrustedServerError::Configuration {
                    message: format!("Failed to parse settings metadata: {}", e),
                })?;
            Ok(Some(metadata))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_settings_hash() {
        let content = "[publisher]\ndomain = \"example.com\"\n";
        let hash = compute_settings_hash(content);
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 7 + 64); // "sha256:" + 64 hex chars
    }

    #[test]
    fn test_hash_normalization() {
        // CRLF and LF should produce the same hash
        let lf_content = "line1\nline2\n";
        let crlf_content = "line1\r\nline2\r\n";

        let lf_hash = compute_settings_hash(lf_content);
        let crlf_hash = compute_settings_hash(crlf_content);

        assert_eq!(lf_hash, crlf_hash);
    }

    #[test]
    fn test_verify_settings_hash() {
        let content = "[publisher]\ndomain = \"example.com\"\n";
        let hash = compute_settings_hash(content);

        assert!(verify_settings_hash(content, &hash));
        assert!(!verify_settings_hash(content, "sha256:invalid"));
    }
}
