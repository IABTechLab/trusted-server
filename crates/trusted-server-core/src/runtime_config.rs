//! Runtime application configuration loading and canonicalization.
//!
//! This module defines the runtime configuration contract for Trusted Server:
//! application config is loaded as TOML, parsed strictly, validated
//! semantically, canonicalized deterministically, and hashed from canonical
//! bytes.

use std::collections::BTreeSet;

use error_stack::{Report, ResultExt};
use sha2::{Digest as _, Sha256};
use toml::Value as TomlValue;
use validator::Validate;

use crate::error::TrustedServerError;
use crate::settings::{parse_toml_document, Settings, TOP_LEVEL_APPLICATION_CONFIG_KEYS};

/// Fixed Fastly resource-link alias for the runtime application config store.
///
/// Provisioning may link any underlying Fastly Config Store resource using
/// this alias. Runtime code opens the alias, not the underlying resource name.
pub const APPLICATION_CONFIG_STORE_NAME: &str = "ts_config_store";

/// Hardcoded runtime config payload key.
pub const APPLICATION_CONFIG_KEY: &str = "ts-config";

/// Fully processed runtime config.
#[derive(Debug, Clone)]
pub struct LoadedRuntimeConfig {
    /// Validated immutable settings snapshot used for a single request.
    pub settings: Settings,
    /// Deterministic canonical TOML payload.
    pub canonical_toml: String,
    /// Lowercase hex SHA-256 of [`Self::canonical_toml`].
    pub config_hash: String,
}

/// Parse, validate, canonicalize, and hash runtime configuration.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the TOML is malformed,
/// contains unknown fields, fails semantic validation, or cannot be
/// canonicalized.
pub fn load_runtime_config(
    toml_str: &str,
) -> Result<LoadedRuntimeConfig, Report<TrustedServerError>> {
    let parsed_document = parse_toml_document(toml_str)?;
    let settings = Settings::from_toml(toml_str)?;

    settings
        .validate()
        .change_context(TrustedServerError::Configuration {
            message: "Failed to validate configuration".to_string(),
        })?;

    let normalized_value =
        TomlValue::try_from(&settings).change_context(TrustedServerError::Configuration {
            message: "Failed to serialize validated configuration".to_string(),
        })?;

    let canonical_value = retain_declared_fields(
        &TomlValue::Table(parsed_document),
        &normalized_value,
        "root",
    )?;

    let canonical_toml =
        toml::to_string(&canonical_value).change_context(TrustedServerError::Configuration {
            message: "Failed to serialize canonical TOML".to_string(),
        })?;

    let config_hash = hex::encode(Sha256::digest(canonical_toml.as_bytes()));

    Ok(LoadedRuntimeConfig {
        settings,
        canonical_toml,
        config_hash,
    })
}

/// Returns the known top-level application-config keys.
#[must_use]
pub fn known_top_level_keys() -> &'static [&'static str] {
    TOP_LEVEL_APPLICATION_CONFIG_KEYS
}

fn retain_declared_fields(
    raw: &TomlValue,
    normalized: &TomlValue,
    path: &str,
) -> Result<TomlValue, Report<TrustedServerError>> {
    match (raw, normalized) {
        (TomlValue::Table(raw_table), TomlValue::Table(normalized_table)) => {
            let mut output = toml::map::Map::new();
            let mut keys: BTreeSet<&String> = BTreeSet::new();
            for key in raw_table.keys() {
                keys.insert(key);
            }

            for key in keys {
                let child_path = format!("{path}.{key}");
                let raw_value = raw_table.get(key).expect("should contain raw key");
                let normalized_value = normalized_table.get(key).ok_or_else(|| {
                    Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "Canonicalization failed because `{child_path}` was not preserved"
                        ),
                    })
                })?;
                output.insert(
                    key.clone(),
                    retain_declared_fields(raw_value, normalized_value, &child_path)?,
                );
            }

            Ok(TomlValue::Table(output))
        }
        (TomlValue::Array(raw_items), TomlValue::Array(normalized_items)) => {
            if raw_items.len() != normalized_items.len() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "Canonicalization failed because array length changed at `{path}`"
                    ),
                }));
            }

            let mut items = Vec::with_capacity(raw_items.len());
            for (index, (raw_item, normalized_item)) in
                raw_items.iter().zip(normalized_items).enumerate()
            {
                items.push(retain_declared_fields(
                    raw_item,
                    normalized_item,
                    &format!("{path}[{index}]"),
                )?);
            }
            Ok(TomlValue::Array(items))
        }
        (_, normalized_scalar) => Ok(normalized_scalar.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> &'static str {
        r#"
[[handlers]]
path = "^/admin"
username = "admin"
password = "secret"

[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "proxy-secret"

[edge_cookie]
secret_key = "secret-key"
"#
    }

    #[test]
    fn canonicalization_preserves_only_declared_fields() {
        let loaded = load_runtime_config(base_config()).expect("should load valid config");
        assert!(
            !loaded.canonical_toml.contains("[proxy]"),
            "should not serialize omitted default sections"
        );
        assert!(
            !loaded.canonical_toml.contains("certificate_check"),
            "should not serialize omitted default fields"
        );
    }

    #[test]
    fn canonicalization_normalizes_declared_values() {
        let config = r#"
[[handlers]]
path = "^/admin"
username = "admin"
password = "secret"

[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "proxy-secret"

[edge_cookie]
secret_key = "secret-key"

[proxy]
allowed_domains = [" Example.COM ", "*.DoubleClick.Net"]
"#;

        let loaded = load_runtime_config(config).expect("should load valid config");
        assert!(
            loaded
                .canonical_toml
                .contains("allowed_domains = [\"example.com\", \"*.doubleclick.net\"]"),
            "should canonicalize normalized proxy allowed_domains"
        );
    }

    #[test]
    fn config_hash_is_stable_for_reordered_input() {
        let a = r#"
[[handlers]]
path = "^/admin"
username = "admin"
password = "secret"

[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "proxy-secret"

[edge_cookie]
secret_key = "secret-key"
"#;

        let b = r#"
[publisher]
origin_url = "https://origin.publisher.com"
proxy_secret = "proxy-secret"
cookie_domain = ".publisher.com"
domain = "publisher.com"

[edge_cookie]
secret_key = "secret-key"

[[handlers]]
username = "admin"
password = "secret"
path = "^/admin"
"#;

        let loaded_a = load_runtime_config(a).expect("should load first config");
        let loaded_b = load_runtime_config(b).expect("should load reordered config");

        assert_eq!(
            loaded_a.config_hash, loaded_b.config_hash,
            "should produce identical hashes for semantically identical config"
        );
        assert_eq!(
            loaded_a.canonical_toml, loaded_b.canonical_toml,
            "should canonicalize reordered input to the same bytes"
        );
    }
}
