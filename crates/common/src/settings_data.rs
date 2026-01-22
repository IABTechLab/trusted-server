//! Settings data loading from Config Store.
//!
//! This module provides functions to load configuration from a platform Config Store
//! (Fastly, Cloudflare, Akamai). Configuration must be pushed to the Config Store
//! using the `ts-cli` tool before the service can start.
//!
//! # Config Store Keys
//!
//! The following keys are used:
//! - `settings` - The TOML configuration content (required)
//! - `settings-hash` - SHA-256 hash for verification (optional but recommended)
//! - `settings-metadata` - JSON metadata with version and timestamps (optional)
//!
//! # Pushing Configuration
//!
//! Use the `tscli` tool to push configuration:
//! ```bash
//! tscli config push -f trusted-server.toml --store-id <config-store-id>
//! ```

use error_stack::{Report, ResultExt};
use validator::Validate;

use crate::config_store::{
    compute_settings_hash, verify_settings_hash, ConfigStore, SettingsMetadata, SETTINGS_HASH_KEY,
    SETTINGS_KEY, SETTINGS_METADATA_KEY,
};
use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Default name for the Fastly Config Store containing settings.
pub const DEFAULT_SETTINGS_STORE_NAME: &str = "trusted-server-config";

/// Result of loading settings, including metadata about the source.
#[derive(Debug)]
pub struct LoadedSettings {
    /// The parsed settings.
    pub settings: Settings,
    /// Hash of the settings content.
    pub hash: String,
    /// Optional metadata from the Config Store.
    pub metadata: Option<SettingsMetadata>,
}

/// Load settings from a Config Store.
///
/// This function loads settings from the provided Config Store. If the Config Store
/// doesn't have the `settings` key, an error is returned.
///
/// # Hash Verification
///
/// If `settings-hash` is present in the Config Store, it is verified against
/// the computed hash of the effective settings (after environment overrides).
/// A mismatch returns an error and prevents the service from starting.
///
/// # Arguments
///
/// * `store` - The Config Store to load from
/// * `store_name` - Name of the store (for logging and error messages)
///
/// # Errors
///
/// Returns an error if:
/// - The `settings` key is not found in the Config Store
/// - The Config Store cannot be read
/// - The settings TOML is invalid
/// - The settings fail validation
pub fn get_settings_from_store<S: ConfigStore>(
    store: &S,
    store_name: &str,
) -> Result<LoadedSettings, Report<TrustedServerError>> {
    // Load settings from Config Store (required)
    let toml_str = match store.get(SETTINGS_KEY) {
        Ok(Some(content)) => content,
        Ok(None) => {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "No '{}' key found in Config Store '{}'. \
                    Push configuration using: ts-cli config push -f <config.toml> --store-id <store-id>",
                    SETTINGS_KEY, store_name
                ),
            }));
        }
        Err(e) => {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("Failed to read from Config Store '{}': {}", store_name, e),
            }));
        }
    };

    log::info!("Loading settings from Config Store '{}'", store_name);

    // Parse and validate settings (env overrides applied here)
    let settings = Settings::from_toml(&toml_str)?;
    settings
        .validate()
        .change_context(TrustedServerError::Configuration {
            message: "Settings validation failed".to_string(),
        })?;

    let canonical_toml = settings.to_canonical_toml()?;

    // Compute hash of the effective configuration (after env overrides)
    let computed_hash = compute_settings_hash(&canonical_toml);

    // Optionally verify against stored hash (hard fail on mismatch)
    match store.get(SETTINGS_HASH_KEY) {
        Ok(Some(stored_hash)) => {
            if !verify_settings_hash(&canonical_toml, &stored_hash) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "Settings hash mismatch in Config Store '{}'. Stored: {}, Computed: {}",
                        store_name, stored_hash, computed_hash
                    ),
                }));
            }
            log::debug!("Settings hash verified: {}", computed_hash);
        }
        Ok(None) => {
            log::warn!(
                "No settings-hash key found in Config Store '{}', skipping verification",
                store_name
            );
        }
        Err(e) => {
            log::warn!(
                "Failed to read settings-hash from Config Store '{}': {}",
                store_name,
                e
            );
        }
    }

    // Load optional metadata
    let metadata = match store.get(SETTINGS_METADATA_KEY) {
        Ok(Some(json_str)) => match serde_json::from_str::<SettingsMetadata>(&json_str) {
            Ok(m) => {
                log::info!(
                    "Settings metadata: version={}, published_at={}",
                    m.version,
                    m.published_at
                );
                Some(m)
            }
            Err(e) => {
                log::warn!("Failed to parse settings metadata: {}", e);
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            log::warn!("Failed to read settings metadata: {}", e);
            None
        }
    };

    Ok(LoadedSettings {
        settings,
        hash: computed_hash,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock Config Store for testing.
    struct MockConfigStore {
        settings: Option<String>,
        hash: Option<String>,
        metadata: Option<String>,
    }

    impl MockConfigStore {
        fn empty() -> Self {
            Self {
                settings: None,
                hash: None,
                metadata: None,
            }
        }

        fn with_settings(settings: &str) -> Self {
            let parsed = Settings::from_toml(settings).expect("should parse settings");
            let canonical = parsed
                .to_canonical_toml()
                .expect("should serialize settings");
            let hash = compute_settings_hash(&canonical);
            Self {
                settings: Some(settings.to_string()),
                hash: Some(hash),
                metadata: None,
            }
        }
    }

    impl ConfigStore for MockConfigStore {
        fn get(&self, key: &str) -> Result<Option<String>, TrustedServerError> {
            match key {
                SETTINGS_KEY => Ok(self.settings.clone()),
                SETTINGS_HASH_KEY => Ok(self.hash.clone()),
                SETTINGS_METADATA_KEY => Ok(self.metadata.clone()),
                _ => Ok(None),
            }
        }
    }

    #[test]
    fn test_get_settings_from_empty_store_returns_error() {
        let store = MockConfigStore::empty();
        let result = get_settings_from_store(&store, "test-store");

        assert!(result.is_err(), "should error when settings are missing");
        let err = result.unwrap_err();
        let err_str = format!("{:?}", err);
        assert!(
            err_str.contains("No 'settings' key found"),
            "should mention missing settings key"
        );
    }

    #[test]
    fn test_get_settings_from_store_with_settings() {
        // Create a minimal valid settings TOML
        let toml = r#"
[publisher]
domain = "test.com"
cookie_domain = ".test.com"
origin_url = "https://origin.test.com"
proxy_secret = "test-secret-key-that-is-long-enough"

[synthetic]
counter_store = "counter"
opid_store = "opid"
secret_key = "test-synthetic-secret-key"
template = "{{ client_ip }}"

[[handlers]]
path = "^/admin"
username = "admin"
password = "password"
"#;

        let store = MockConfigStore::with_settings(toml);
        let result = get_settings_from_store(&store, "test-store");

        assert!(result.is_ok(), "should load settings successfully");
        let loaded = result.unwrap();
        assert!(
            loaded.hash.starts_with("sha256:"),
            "should compute settings hash"
        );
        assert_eq!(
            loaded.settings.publisher.domain, "test.com",
            "should load publisher domain"
        );
    }

    #[test]
    fn test_get_settings_with_invalid_toml() {
        let store = MockConfigStore {
            settings: Some("invalid toml {{{{".to_string()),
            hash: None,
            metadata: None,
        };
        let result = get_settings_from_store(&store, "test-store");

        assert!(result.is_err(), "should error on invalid TOML");
    }

    #[test]
    fn test_get_settings_hash_mismatch_returns_error() {
        let toml = r#"
[publisher]
domain = "test.com"
cookie_domain = ".test.com"
origin_url = "https://origin.test.com"
proxy_secret = "test-secret-key-that-is-long-enough"

[synthetic]
counter_store = "counter"
opid_store = "opid"
secret_key = "test-synthetic-secret-key"
template = "{{ client_ip }}"

[[handlers]]
path = "^/admin"
username = "admin"
password = "password"
"#;

        let store = MockConfigStore {
            settings: Some(toml.to_string()),
            hash: Some("sha256:deadbeef".to_string()),
            metadata: None,
        };
        let result = get_settings_from_store(&store, "test-store");

        assert!(result.is_err(), "should fail on hash mismatch");
    }
}
