//! Deterministic config-store payloads for Trusted Server settings.
//!
//! The `ts` CLI uses this module to flatten validated [`Settings`] into
//! `EdgeZero` config-store entries. Runtime loading uses the same escaping,
//! hashing, and reconstruction rules so push-time and runtime semantics cannot
//! drift.

use std::collections::BTreeMap;

use error_stack::{Report, ResultExt};
use serde_json::{Map as JsonMap, Value as JsonValue};
use sha2::{Digest as _, Sha256};

use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Metadata key containing the SHA-256 hash of settings-only entries.
pub const CONFIG_HASH_KEY: &str = "ts-config-hash";
/// Metadata key containing the sorted list of settings-only entry keys.
pub const CONFIG_KEYS_KEY: &str = "ts-config-keys";
/// Prefix reserved for Trusted Server config metadata keys.
pub const CONFIG_METADATA_PREFIX: &str = "ts-config-";

/// Flattened Trusted Server config payload ready for config-store publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPayload {
    /// Flattened settings entries, excluding metadata entries.
    pub settings_entries: BTreeMap<String, String>,
    /// Flattened settings entries plus Trusted Server metadata entries.
    pub entries: BTreeMap<String, String>,
    /// Sorted flattened settings keys, excluding metadata entries.
    pub keys: Vec<String>,
    /// `sha256:<hex>` over the canonical settings-only entry map.
    pub hash: String,
}

/// Escape one flattened-key path segment.
#[must_use]
pub fn escape_key_segment(segment: &str) -> String {
    let mut escaped = String::with_capacity(segment.len());
    for ch in segment.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '.' => escaped.push_str("\\."),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Split an escaped dotted key into unescaped path segments.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the key has an empty
/// segment or ends with a dangling escape character.
pub fn split_escaped_key(key: &str) -> Result<Vec<String>, Report<TrustedServerError>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut escaping = false;

    for ch in key.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }

        match ch {
            '\\' => escaping = true,
            '.' => {
                if current.is_empty() {
                    return configuration_error(format!(
                        "flattened config key `{key}` contains an empty path segment"
                    ));
                }
                segments.push(current);
                current = String::new();
            }
            other => current.push(other),
        }
    }

    if escaping {
        return configuration_error(format!(
            "flattened config key `{key}` ends with an incomplete escape"
        ));
    }
    if current.is_empty() {
        return configuration_error(format!(
            "flattened config key `{key}` contains an empty path segment"
        ));
    }

    segments.push(current);
    Ok(segments)
}

/// Build a deterministic config-store payload from validated settings.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when settings cannot be
/// serialized, flattened, or hashed.
pub fn build_config_payload(
    settings: &Settings,
) -> Result<ConfigPayload, Report<TrustedServerError>> {
    let json =
        serde_json::to_value(settings).change_context(TrustedServerError::Configuration {
            message: "failed to serialize settings to JSON".to_string(),
        })?;

    let mut settings_entries = BTreeMap::new();
    flatten_json_value(&json, &mut Vec::new(), &mut settings_entries)?;

    for key in settings_entries.keys() {
        if key.starts_with(CONFIG_METADATA_PREFIX) {
            return configuration_error(format!(
                "settings key `{key}` uses reserved metadata prefix `{CONFIG_METADATA_PREFIX}`"
            ));
        }
    }

    let keys: Vec<String> = settings_entries.keys().cloned().collect();
    let hash = hash_settings_entries(&settings_entries)?;
    let mut entries = settings_entries.clone();
    let keys_json =
        serde_json::to_string(&keys).change_context(TrustedServerError::Configuration {
            message: "failed to serialize config key metadata".to_string(),
        })?;
    entries.insert(CONFIG_KEYS_KEY.to_string(), keys_json);
    entries.insert(CONFIG_HASH_KEY.to_string(), hash.clone());

    Ok(ConfigPayload {
        settings_entries,
        entries,
        keys,
        hash,
    })
}

/// Reconstruct validated [`Settings`] from flattened config-store entries.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when metadata is missing, the
/// hash does not match, flattened keys cannot be reconstructed, or the resulting
/// settings fail schema or semantic validation.
pub fn settings_from_config_entries(
    entries: &BTreeMap<String, String>,
) -> Result<Settings, Report<TrustedServerError>> {
    let keys_value = entries.get(CONFIG_KEYS_KEY).ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: format!("missing `{CONFIG_KEYS_KEY}` metadata entry"),
        })
    })?;
    let keys: Vec<String> =
        serde_json::from_str(keys_value).change_context(TrustedServerError::Configuration {
            message: format!("`{CONFIG_KEYS_KEY}` metadata is not a JSON string array"),
        })?;

    let mut settings_entries = BTreeMap::new();
    for key in &keys {
        if key.starts_with(CONFIG_METADATA_PREFIX) {
            return configuration_error(format!(
                "settings key `{key}` uses reserved metadata prefix `{CONFIG_METADATA_PREFIX}`"
            ));
        }
        let value = entries.get(key).ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!("missing flattened config entry `{key}`"),
            })
        })?;
        settings_entries.insert(key.clone(), value.clone());
    }

    let expected_hash = hash_settings_entries(&settings_entries)?;
    let actual_hash = entries.get(CONFIG_HASH_KEY).ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: format!("missing `{CONFIG_HASH_KEY}` metadata entry"),
        })
    })?;
    if actual_hash != &expected_hash {
        return configuration_error(format!(
            "config hash mismatch: expected `{expected_hash}`, got `{actual_hash}`"
        ));
    }

    let mut root = JsonMap::new();
    for (key, raw_value) in settings_entries {
        let path = split_escaped_key(&key)?;
        insert_flattened_value(&mut root, &path, parse_entry_value(&raw_value))?;
    }

    let settings = Settings::from_json_value(JsonValue::Object(root))?;
    settings.reject_placeholder_secrets()?;
    Ok(settings)
}

fn flatten_json_value(
    value: &JsonValue,
    path: &mut Vec<String>,
    out: &mut BTreeMap<String, String>,
) -> Result<(), Report<TrustedServerError>> {
    match value {
        JsonValue::Null => Ok(()),
        JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {
            insert_leaf(path, value, out)
        }
        JsonValue::Array(_) => {
            let canonical = canonical_json_value(value);
            insert_leaf(path, &canonical, out)
        }
        JsonValue::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, child) in map {
                sorted.insert(escape_key_segment(key), child);
            }
            for (escaped_key, child) in sorted {
                path.push(escaped_key);
                flatten_json_value(child, path, out)?;
                path.pop();
            }
            Ok(())
        }
    }
}

fn insert_leaf(
    path: &[String],
    value: &JsonValue,
    out: &mut BTreeMap<String, String>,
) -> Result<(), Report<TrustedServerError>> {
    if path.is_empty() {
        return configuration_error(
            "settings serialized to a scalar; expected a JSON object".to_string(),
        );
    }
    let encoded =
        serde_json::to_string(value).change_context(TrustedServerError::Configuration {
            message: "failed to serialize flattened config value".to_string(),
        })?;
    let key = path.join(".");
    out.insert(key, encoded);
    Ok(())
}

fn canonical_json_value(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(items) => {
            JsonValue::Array(items.iter().map(canonical_json_value).collect())
        }
        JsonValue::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in map {
                sorted.insert(key.clone(), canonical_json_value(value));
            }
            let mut canonical = JsonMap::new();
            for (key, value) in sorted {
                canonical.insert(key, value);
            }
            JsonValue::Object(canonical)
        }
        other => other.clone(),
    }
}

fn hash_settings_entries(
    entries: &BTreeMap<String, String>,
) -> Result<String, Report<TrustedServerError>> {
    let bytes = serde_json::to_vec(entries).change_context(TrustedServerError::Configuration {
        message: "failed to serialize canonical settings entries".to_string(),
    })?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("sha256:{}", hex::encode(digest)))
}

fn insert_flattened_value(
    root: &mut JsonMap<String, JsonValue>,
    path: &[String],
    value: JsonValue,
) -> Result<(), Report<TrustedServerError>> {
    if path.is_empty() {
        return configuration_error("flattened config key path is empty".to_string());
    }

    let mut current = root;
    for segment in &path[..path.len().saturating_sub(1)] {
        let entry = current
            .entry(segment.clone())
            .or_insert_with(|| JsonValue::Object(JsonMap::new()));
        let JsonValue::Object(next) = entry else {
            return configuration_error(format!(
                "flattened config key collision at segment `{segment}`"
            ));
        };
        current = next;
    }

    let leaf = path.last().expect("should have at least one segment");
    if current.insert(leaf.clone(), value).is_some() {
        return configuration_error(format!(
            "duplicate flattened config key `{}`",
            path.join(".")
        ));
    }
    Ok(())
}

fn parse_entry_value(raw: &str) -> JsonValue {
    serde_json::from_str(raw).unwrap_or_else(|_| JsonValue::String(raw.to_string()))
}

fn configuration_error<T>(message: String) -> Result<T, Report<TrustedServerError>> {
    Err(Report::new(TrustedServerError::Configuration { message }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;
    use crate::test_support::tests::crate_test_settings_str;

    fn test_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings")
    }

    #[test]
    fn escapes_and_splits_key_segments() {
        let escaped = escape_key_segment(r"a.b\c");
        assert_eq!(escaped, r"a\.b\\c");
        let parts =
            split_escaped_key(&format!("root.{escaped}.leaf")).expect("should split escaped key");
        assert_eq!(parts, vec!["root", r"a.b\c", "leaf"]);
    }

    #[test]
    fn builds_payload_with_metadata_hash() {
        let payload = build_config_payload(&test_settings()).expect("should build payload");
        assert!(
            payload.entries.contains_key(CONFIG_KEYS_KEY),
            "should include keys metadata"
        );
        assert!(
            payload.entries.contains_key(CONFIG_HASH_KEY),
            "should include hash metadata"
        );
        assert_eq!(
            payload.entries.get(CONFIG_HASH_KEY),
            Some(&payload.hash),
            "metadata hash should match payload hash"
        );
        assert!(
            !payload.settings_entries.contains_key(CONFIG_HASH_KEY),
            "settings-only map should exclude metadata"
        );
    }

    #[test]
    fn payload_round_trips_through_flattened_entries() {
        let original = test_settings();
        let payload = build_config_payload(&original).expect("should build payload");
        let reconstructed =
            settings_from_config_entries(&payload.entries).expect("should reconstruct settings");
        assert_eq!(
            reconstructed.publisher.domain, original.publisher.domain,
            "should preserve publisher domain"
        );
        assert_eq!(
            reconstructed.ec.pull_sync_concurrency, original.ec.pull_sync_concurrency,
            "should preserve numeric fields"
        );
        assert_eq!(
            reconstructed.handlers.len(),
            original.handlers.len(),
            "should preserve arrays"
        );
    }

    #[test]
    fn strings_that_look_like_json_scalars_round_trip_as_strings() {
        let mut original = test_settings();
        original.publisher.proxy_secret = Redacted::new("1234567890".to_string());
        original.ec.passphrase = Redacted::new("12345678901234567890123456789012".to_string());
        original.handlers[0].password = Redacted::new("true".to_string());

        let payload = build_config_payload(&original).expect("should build payload");
        assert_eq!(
            payload.settings_entries.get("publisher.proxy_secret"),
            Some(&"\"1234567890\"".to_string()),
            "string entries should be JSON encoded to preserve type"
        );

        let reconstructed =
            settings_from_config_entries(&payload.entries).expect("should reconstruct settings");
        assert_eq!(
            reconstructed.publisher.proxy_secret.expose(),
            original.publisher.proxy_secret.expose(),
            "numeric-looking proxy secret should remain a string"
        );
        assert_eq!(
            reconstructed.ec.passphrase.expose(),
            original.ec.passphrase.expose(),
            "numeric-looking passphrase should remain a string"
        );
        assert_eq!(
            reconstructed.handlers[0].password.expose(),
            original.handlers[0].password.expose(),
            "boolean-looking handler password should remain a string"
        );
    }

    #[test]
    fn arrays_use_canonical_object_key_order() {
        let value = serde_json::json!({
            "items": [
                {"z": 1, "a": true},
                {"b": [{"d": 4, "c": 3}]}
            ]
        });
        let mut entries = BTreeMap::new();
        flatten_json_value(&value, &mut Vec::new(), &mut entries).expect("should flatten");
        assert_eq!(
            entries.get("items"),
            Some(&r#"[{"a":true,"z":1},{"b":[{"c":3,"d":4}]}]"#.to_string()),
            "array object keys should be sorted"
        );
    }

    #[test]
    fn hash_is_stable_for_equivalent_toml_ordering() {
        let first = r#"
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "production-admin-password-32-bytes"

[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "unit-test-proxy-secret"

[ec]
passphrase = "test-secret-key-32-bytes-minimum"
pull_sync_concurrency = 5
"#;
        let second = r#"
[ec]
pull_sync_concurrency = 5
passphrase = "test-secret-key-32-bytes-minimum"

[publisher]
proxy_secret = "unit-test-proxy-secret"
origin_url = "https://origin.example.com"
cookie_domain = ".example.com"
domain = "example.com"

[[handlers]]
password = "production-admin-password-32-bytes"
username = "admin"
path = "^/_ts/admin"
"#;
        let first_settings = Settings::from_toml(first).expect("should parse first settings");
        let second_settings = Settings::from_toml(second).expect("should parse second settings");
        let first_payload = build_config_payload(&first_settings).expect("should build first");
        let second_payload = build_config_payload(&second_settings).expect("should build second");
        assert_eq!(first_payload.hash, second_payload.hash);
    }

    #[test]
    fn hash_mismatch_is_rejected() {
        let payload = build_config_payload(&test_settings()).expect("should build payload");
        let mut entries = payload.entries;
        entries.insert(CONFIG_HASH_KEY.to_string(), "sha256:bad".to_string());
        let err = settings_from_config_entries(&entries).expect_err("should reject hash mismatch");
        assert!(
            err.to_string().contains("config hash mismatch"),
            "error should mention hash mismatch"
        );
    }
}
