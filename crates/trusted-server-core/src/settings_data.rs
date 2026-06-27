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
/// Returns [`TrustedServerError::ConfigStoreUnavailable`] (HTTP 503) when the
/// config blob (or a referenced chunk) cannot be read — store unseeded,
/// transient backend, or a chunk key missing. Returns
/// [`TrustedServerError::Configuration`] (HTTP 500) when the read succeeds but
/// the blob fails envelope/chunk verification or Trusted Server settings
/// validation.
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
/// Returns [`TrustedServerError::ConfigStoreUnavailable`] (HTTP 503) when the
/// config blob (or a referenced chunk) cannot be read, and
/// [`TrustedServerError::Configuration`] (HTTP 500) when the read succeeds but
/// envelope/chunk verification or settings validation fails.
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
    config_store
        .get(store_name, key)
        .change_context(TrustedServerError::ConfigStoreUnavailable {
            store_name: store_name.to_string(),
            message: format!(
                "unavailable or not seeded (failed to read `{key}`) — run `ts config push`"
            ),
        })
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

    let mut envelope_json = String::with_capacity(pointer.envelope_len);
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
    use crate::error::IntoHttpResponse;
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
    fn unseeded_blob_is_config_store_unavailable_503() {
        let store = MemoryConfigStore {
            entries: BTreeMap::new(),
        };

        let err =
            get_settings_from_config_store(&store, &StoreName::from("app_config"), CONFIG_BLOB_KEY)
                .expect_err("should fail when blob is missing");

        // Unseeded store is a read failure → 503, not an opaque 500.
        assert_eq!(
            err.current_context().status_code(),
            http::StatusCode::SERVICE_UNAVAILABLE,
            "unseeded config blob should map to 503"
        );
        // The actionable hint must ride the error chain so it reaches the
        // server log; the public 503 body stays generic by design.
        assert!(
            format!("{err:?}").contains("ts config push"),
            "error chain should carry the actionable `ts config push` hint for logs"
        );
    }

    #[test]
    fn missing_chunk_is_config_store_unavailable_503() {
        // The blob key resolves to a chunk pointer, but one referenced chunk is
        // absent — still a config-store read failure → 503.
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
                { "key": first_key, "sha256": sha256_hex(first_chunk.as_bytes()), "len": first_chunk.len() },
                { "key": second_key, "sha256": sha256_hex(second_chunk.as_bytes()), "len": second_chunk.len() }
            ]
        })
        .to_string();
        // Seed the pointer + the first chunk only; the second chunk is missing.
        let store = MemoryConfigStore {
            entries: BTreeMap::from([
                (CONFIG_BLOB_KEY.to_string(), pointer),
                (first_key, first_chunk),
            ]),
        };

        let err =
            get_settings_from_config_store(&store, &StoreName::from("app_config"), CONFIG_BLOB_KEY)
                .expect_err("missing chunk must error");

        assert_eq!(
            err.current_context().status_code(),
            http::StatusCode::SERVICE_UNAVAILABLE,
            "a referenced chunk missing is a read failure → 503"
        );
    }

    #[test]
    fn malformed_blob_stays_500() {
        // The blob key reads fine (not a chunk pointer), but its contents are not
        // a valid envelope — reconstruct/verify failure → 500, not 503.
        let store = MemoryConfigStore {
            entries: BTreeMap::from([(
                CONFIG_BLOB_KEY.to_string(),
                "not a valid blob envelope".to_string(),
            )]),
        };

        let err =
            get_settings_from_config_store(&store, &StoreName::from("app_config"), CONFIG_BLOB_KEY)
                .expect_err("malformed blob must error");

        assert_eq!(
            err.current_context().status_code(),
            http::StatusCode::INTERNAL_SERVER_ERROR,
            "read-OK-but-invalid blob should stay 500"
        );
    }

    #[test]
    fn chunk_verification_failure_stays_500() {
        // Pointer + chunks all read successfully, but a chunk's bytes no longer
        // match the recorded length/sha — reconstruct/verify failure → 500.
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
                { "key": first_key, "sha256": sha256_hex(first_chunk.as_bytes()), "len": first_chunk.len() },
                { "key": second_key, "sha256": sha256_hex(second_chunk.as_bytes()), "len": second_chunk.len() }
            ]
        })
        .to_string();
        // Store a corrupted second chunk: reads OK, but fails length/sha checks.
        let store = MemoryConfigStore {
            entries: BTreeMap::from([
                (CONFIG_BLOB_KEY.to_string(), pointer),
                (first_key, first_chunk),
                (second_key, "corrupted chunk bytes".to_string()),
            ]),
        };

        let err =
            get_settings_from_config_store(&store, &StoreName::from("app_config"), CONFIG_BLOB_KEY)
                .expect_err("corrupt chunk must error");

        assert_eq!(
            err.current_context().status_code(),
            http::StatusCode::INTERNAL_SERVER_ERROR,
            "chunk that reads but fails verification should stay 500"
        );
    }
}
