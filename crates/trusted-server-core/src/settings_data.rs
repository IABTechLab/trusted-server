use edgezero_core::env_config::EnvConfig;
use error_stack::{Report, ResultExt};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::config_payload::settings_from_config_blob;
use crate::error::TrustedServerError;
use crate::platform::{PlatformConfigStore, RuntimeServices, StoreName};
use crate::settings::Settings;

const DEFAULT_CONFIG_STORE_ID: &str = "app_config";
const FASTLY_CHUNK_POINTER_KIND: &str = "fastly_config_chunks";

#[derive(Debug, Deserialize)]
struct FastlyChunkPointer {
    chunks: Vec<FastlyChunkRef>,
    edgezero_kind: String,
    envelope_len: usize,
    envelope_sha256: String,
    version: u8,
}

#[derive(Debug, Deserialize)]
struct FastlyChunkRef {
    key: String,
    len: usize,
    sha256: String,
}

/// Loads [`Settings`] from the default `EdgeZero` `app_config` config store.
///
/// The store name is resolved from `EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME`
/// and falls back to the logical id `app_config`. The blob key is resolved from
/// `EDGEZERO__STORES__CONFIG__APP_CONFIG__KEY` and also falls back to
/// `app_config`.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the config blob is
/// missing, cannot be read, fails envelope verification, or fails Trusted
/// Server settings validation.
pub fn get_settings_from_services(
    services: &RuntimeServices,
) -> Result<Settings, Report<TrustedServerError>> {
    let store_name = default_config_store_name();
    let config_key = default_config_key();
    get_settings_from_config_store(services.config_store(), &store_name, &config_key)
}

/// Returns the default `EdgeZero` app-config store name.
#[must_use]
pub fn default_config_store_name() -> StoreName {
    StoreName::from(
        std::env::var("EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME")
            .unwrap_or_else(|_| DEFAULT_CONFIG_STORE_ID.to_string()),
    )
}

/// Returns the default config-store key containing the app-config blob.
#[must_use]
pub fn default_config_key() -> String {
    EnvConfig::from_env().store_key("config", DEFAULT_CONFIG_STORE_ID)
}

/// Loads [`Settings`] from a platform config store and key.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the config blob is
/// missing, cannot be read, fails envelope verification, or fails Trusted
/// Server settings validation.
pub fn get_settings_from_config_store(
    config_store: &dyn PlatformConfigStore,
    store_name: &StoreName,
    key: &str,
) -> Result<Settings, Report<TrustedServerError>> {
    let raw_value = read_config_entry(config_store, store_name, key)?;
    let envelope_json = resolve_fastly_chunk_pointer(config_store, store_name, &raw_value)?;
    settings_from_config_blob(&envelope_json)
}

fn read_config_entry(
    config_store: &dyn PlatformConfigStore,
    store_name: &StoreName,
    key: &str,
) -> Result<String, Report<TrustedServerError>> {
    let message = format!(
        "failed to read Trusted Server app config key `{key}` from config store `{store_name}`"
    );
    config_store
        .get(store_name, key)
        .change_context(TrustedServerError::Configuration { message })
}

fn resolve_fastly_chunk_pointer(
    config_store: &dyn PlatformConfigStore,
    store_name: &StoreName,
    value: &str,
) -> Result<String, Report<TrustedServerError>> {
    let Ok(pointer) = serde_json::from_str::<FastlyChunkPointer>(value) else {
        return Ok(value.to_string());
    };
    if pointer.edgezero_kind != FASTLY_CHUNK_POINTER_KIND {
        return Ok(value.to_string());
    }
    if pointer.version != 1 {
        return configuration_error(format!(
            "unsupported Fastly config chunk pointer version {}; expected 1",
            pointer.version
        ));
    }

    let mut envelope_json = String::new();
    for chunk in pointer.chunks {
        let chunk_value = read_config_entry(config_store, store_name, &chunk.key)?;
        let chunk_len = chunk_value.len();
        if chunk_len != chunk.len {
            return configuration_error(format!(
                "Fastly config chunk `{}` length mismatch: expected {}, got {}",
                chunk.key, chunk.len, chunk_len
            ));
        }
        let chunk_sha = sha256_hex(chunk_value.as_bytes());
        if chunk_sha != chunk.sha256 {
            return configuration_error(format!(
                "Fastly config chunk `{}` sha mismatch: expected {}, got {}",
                chunk.key, chunk.sha256, chunk_sha
            ));
        }
        envelope_json.push_str(&chunk_value);
    }

    if envelope_json.len() != pointer.envelope_len {
        return configuration_error(format!(
            "Fastly config envelope length mismatch: expected {}, got {}",
            pointer.envelope_len,
            envelope_json.len()
        ));
    }
    let envelope_sha = sha256_hex(envelope_json.as_bytes());
    if envelope_sha != pointer.envelope_sha256 {
        return configuration_error(format!(
            "Fastly config envelope sha mismatch: expected {}, got {}",
            pointer.envelope_sha256, envelope_sha
        ));
    }

    Ok(envelope_json)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn configuration_error<T>(message: String) -> Result<T, Report<TrustedServerError>> {
    Err(Report::new(TrustedServerError::Configuration { message }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_payload::CONFIG_BLOB_KEY;
    use crate::platform::PlatformError;
    use crate::settings::Settings;
    use crate::test_support::tests::crate_test_settings_str;
    use edgezero_core::blob_envelope::BlobEnvelope;
    use serde_json::json;
    use std::collections::BTreeMap;

    struct MemoryConfigStore {
        entries: BTreeMap<String, String>,
    }

    impl PlatformConfigStore for MemoryConfigStore {
        fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.entries.get(key).cloned().ok_or_else(|| {
                Report::new(PlatformError::ConfigStore).attach(format!("missing key `{key}`"))
            })
        }

        fn put(
            &self,
            _store_id: &crate::platform::StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(
            &self,
            _store_id: &crate::platform::StoreId,
            _key: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    fn envelope_json(settings: &Settings) -> String {
        let data = serde_json::to_value(settings).expect("should serialize settings to JSON");
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
        serde_json::to_string(&envelope).expect("should serialize envelope")
    }

    #[test]
    fn loads_settings_from_config_blob_entry() {
        let settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        let envelope_json = envelope_json(&settings);
        let store = MemoryConfigStore {
            entries: BTreeMap::from([(CONFIG_BLOB_KEY.to_string(), envelope_json)]),
        };

        let loaded =
            get_settings_from_config_store(&store, &StoreName::from("app_config"), CONFIG_BLOB_KEY)
                .expect("should load settings");

        assert_eq!(
            loaded.publisher.domain, settings.publisher.domain,
            "should load publisher domain"
        );
    }

    #[test]
    fn loads_settings_from_fastly_chunk_pointer() {
        let settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        let envelope_json = envelope_json(&settings);
        let midpoint = envelope_json.len() / 2;
        let first_chunk = envelope_json[..midpoint].to_string();
        let second_chunk = envelope_json[midpoint..].to_string();
        let first_key = format!("{CONFIG_BLOB_KEY}.__edgezero_chunks.test.0");
        let second_key = format!("{CONFIG_BLOB_KEY}.__edgezero_chunks.test.1");
        let pointer = json!({
            "edgezero_kind": FASTLY_CHUNK_POINTER_KIND,
            "version": 1,
            "envelope_sha256": sha256_hex(envelope_json.as_bytes()),
            "envelope_len": envelope_json.len(),
            "chunks": [
                {
                    "key": first_key,
                    "sha256": sha256_hex(first_chunk.as_bytes()),
                    "len": first_chunk.len()
                },
                {
                    "key": second_key,
                    "sha256": sha256_hex(second_chunk.as_bytes()),
                    "len": second_chunk.len()
                }
            ]
        })
        .to_string();
        let store = MemoryConfigStore {
            entries: BTreeMap::from([
                (CONFIG_BLOB_KEY.to_string(), pointer),
                (first_key, first_chunk),
                (second_key, second_chunk),
            ]),
        };

        let loaded =
            get_settings_from_config_store(&store, &StoreName::from("app_config"), CONFIG_BLOB_KEY)
                .expect("should load settings");

        assert_eq!(
            loaded.publisher.domain, settings.publisher.domain,
            "should reconstruct chunked envelope"
        );
    }

    #[test]
    fn fails_when_blob_key_is_missing() {
        let store = MemoryConfigStore {
            entries: BTreeMap::new(),
        };

        let err =
            get_settings_from_config_store(&store, &StoreName::from("app_config"), CONFIG_BLOB_KEY)
                .expect_err("should fail when blob is missing");

        assert!(
            err.to_string().contains(CONFIG_BLOB_KEY),
            "error should mention missing blob key"
        );
    }
}
