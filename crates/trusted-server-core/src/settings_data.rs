use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::env_config::EnvConfig;
use error_stack::Report;
use futures::executor::block_on;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::config_payload::{settings_from_config_blob, CONFIG_BLOB_KEY};
use crate::error::TrustedServerError;
use crate::settings::Settings;

const FASTLY_CHUNK_POINTER_KIND: &str = "fastly_config_chunks";
const FASTLY_CONFIG_ENTRY_LIMIT: usize = 8_000;

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

/// Returns the default config-store key containing the app-config blob.
#[must_use]
pub fn default_config_key() -> String {
    EnvConfig::from_env().store_key("config", CONFIG_BLOB_KEY)
}

/// Loads [`Settings`] from an `EdgeZero` [`ConfigStoreHandle`] and key.
///
/// The handle is already bound to a specific config store, so only the blob
/// `key` is supplied. Reads resolve through the handle's async
/// [`ConfigStoreHandle::get`], driven to completion with [`block_on`]; the
/// Fastly chunk-pointer path reads its additional chunk keys from the same
/// handle.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the config blob is
/// missing, cannot be read, fails envelope verification, or fails Trusted
/// Server settings validation.
pub fn get_settings_from_config_store(
    config_store: &ConfigStoreHandle,
    key: &str,
) -> Result<Settings, Report<TrustedServerError>> {
    let raw_value = read_config_entry(config_store, key)?;
    let envelope_json = resolve_fastly_chunk_pointer(config_store, &raw_value)?;
    settings_from_config_blob(&envelope_json)
}

fn read_config_entry(
    config_store: &ConfigStoreHandle,
    key: &str,
) -> Result<String, Report<TrustedServerError>> {
    match block_on(config_store.get(key)) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => configuration_error(format!(
            "Trusted Server app config key `{key}` was not found in the config store"
        )),
        Err(error) => configuration_error(format!(
            "failed to read Trusted Server app config key `{key}` from the config store: {error}"
        )),
    }
}

fn resolve_fastly_chunk_pointer(
    config_store: &ConfigStoreHandle,
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
    if value.len() > FASTLY_CONFIG_ENTRY_LIMIT {
        return configuration_error(format!(
            "Fastly config chunk pointer is {} bytes, exceeding the {} byte entry limit",
            value.len(),
            FASTLY_CONFIG_ENTRY_LIMIT
        ));
    }

    let mut declared_envelope_len = 0usize;
    for chunk in &pointer.chunks {
        if chunk.len > FASTLY_CONFIG_ENTRY_LIMIT {
            return configuration_error(format!(
                "Fastly config chunk `{}` declares {} bytes, exceeding the {} byte entry limit",
                chunk.key, chunk.len, FASTLY_CONFIG_ENTRY_LIMIT
            ));
        }
        declared_envelope_len = match declared_envelope_len.checked_add(chunk.len) {
            Some(total) => total,
            None => {
                return configuration_error(
                    "Fastly config chunk lengths overflowed usize".to_string(),
                );
            }
        };
    }
    if declared_envelope_len != pointer.envelope_len {
        return configuration_error(format!(
            "Fastly config chunk lengths total mismatch: expected envelope length {}, got {}",
            pointer.envelope_len, declared_envelope_len
        ));
    }

    let mut envelope_json = String::with_capacity(pointer.envelope_len);
    let mut actual_envelope_len = 0usize;
    for chunk in pointer.chunks {
        let chunk_value = read_config_entry(config_store, &chunk.key)?;
        let chunk_len = chunk_value.len();
        if chunk_len != chunk.len {
            return configuration_error(format!(
                "Fastly config chunk `{}` length mismatch: expected {}, got {}",
                chunk.key, chunk.len, chunk_len
            ));
        }
        actual_envelope_len = actual_envelope_len.saturating_add(chunk_len);
        if actual_envelope_len > pointer.envelope_len {
            return configuration_error(format!(
                "Fastly config envelope exceeded declared length {} while reading chunk `{}`",
                pointer.envelope_len, chunk.key
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
    use crate::settings::Settings;
    use crate::test_support::tests::crate_test_settings_str;
    use async_trait::async_trait;
    use edgezero_core::blob_envelope::BlobEnvelope;
    use edgezero_core::config_store::{ConfigStore, ConfigStoreError};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    struct InMemoryConfigStore {
        entries: BTreeMap<String, String>,
    }

    impl InMemoryConfigStore {
        fn with(entries: &[(&str, &str)]) -> Self {
            Self {
                entries: entries
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            }
        }
    }

    #[async_trait(?Send)]
    impl ConfigStore for InMemoryConfigStore {
        async fn get(&self, key: &str) -> Result<Option<String>, ConfigStoreError> {
            Ok(self.entries.get(key).cloned())
        }
    }

    fn handle_with(entries: &[(&str, &str)]) -> ConfigStoreHandle {
        ConfigStoreHandle::new(Arc::new(InMemoryConfigStore::with(entries)))
    }

    fn envelope_json(settings: &Settings) -> String {
        let data = serde_json::to_value(settings).expect("should serialize settings to JSON");
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
        serde_json::to_string(&envelope).expect("should serialize envelope")
    }

    fn blob_envelope_json(toml: &str) -> String {
        let settings = Settings::from_toml(toml).expect("should parse settings TOML");
        envelope_json(&settings)
    }

    #[test]
    fn get_settings_reads_blob_via_edgezero_handle() {
        let blob = blob_envelope_json(&crate_test_settings_str());
        let handle = handle_with(&[(CONFIG_BLOB_KEY, &blob)]);

        let settings = get_settings_from_config_store(&handle, CONFIG_BLOB_KEY)
            .expect("should parse settings from the EdgeZero-read blob");

        assert!(
            !settings.publisher.domain.is_empty(),
            "should deserialize the config blob read through the EdgeZero handle"
        );
    }

    #[test]
    fn loads_settings_from_config_blob_entry() {
        let settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        let envelope_json = envelope_json(&settings);
        let handle = handle_with(&[(CONFIG_BLOB_KEY, &envelope_json)]);

        let loaded =
            get_settings_from_config_store(&handle, CONFIG_BLOB_KEY).expect("should load settings");

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
        let handle = handle_with(&[
            (CONFIG_BLOB_KEY, &pointer),
            (&first_key, &first_chunk),
            (&second_key, &second_chunk),
        ]);

        let loaded =
            get_settings_from_config_store(&handle, CONFIG_BLOB_KEY).expect("should load settings");

        assert_eq!(
            loaded.publisher.domain, settings.publisher.domain,
            "should reconstruct chunked envelope"
        );
    }

    #[test]
    fn rejects_chunk_pointer_when_declared_lengths_do_not_match_envelope_len() {
        let chunk_key = format!("{CONFIG_BLOB_KEY}.__edgezero_chunks.test.0");
        let pointer = json!({
            "edgezero_kind": FASTLY_CHUNK_POINTER_KIND,
            "version": 1,
            "envelope_sha256": sha256_hex(b"ab"),
            "envelope_len": 1,
            "chunks": [
                {
                    "key": chunk_key,
                    "sha256": sha256_hex(b"ab"),
                    "len": 2
                }
            ]
        })
        .to_string();
        let handle = handle_with(&[(CONFIG_BLOB_KEY, &pointer)]);

        let err = get_settings_from_config_store(&handle, CONFIG_BLOB_KEY)
            .expect_err("should reject malformed chunk length metadata");

        assert!(
            err.to_string().contains("chunk lengths total mismatch"),
            "error should explain chunk length mismatch: {err:?}"
        );
    }

    #[test]
    fn fails_when_blob_key_is_missing() {
        let handle = handle_with(&[]);

        let err = get_settings_from_config_store(&handle, CONFIG_BLOB_KEY)
            .expect_err("should fail when blob is missing");

        assert!(
            err.to_string().contains(CONFIG_BLOB_KEY),
            "error should mention missing blob key"
        );
    }
}
