use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::env_config::EnvConfig;
use error_stack::Report;
use futures::executor::block_on;

use crate::config_payload::{settings_from_config_blob, CONFIG_BLOB_KEY};
use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Returns the default config-store key containing the app-config blob.
#[must_use]
pub fn default_config_key() -> String {
    EnvConfig::from_env().store_key("config", CONFIG_BLOB_KEY)
}

/// Loads [`Settings`] from an `EdgeZero` [`ConfigStoreHandle`] and key.
///
/// The handle is already bound to a specific config store, so only the blob
/// `key` is supplied. Reads resolve through the handle's async
/// [`ConfigStoreHandle::get`], driven to completion with [`block_on`]. The
/// handle returns a fully resolved envelope: platform-specific storage details
/// such as Fastly's config-entry chunking are reassembled by `EdgeZero`'s
/// config store, not here.
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
    let envelope_json = read_config_entry(config_store, key)?;
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
    fn fails_when_blob_value_is_not_an_envelope() {
        let handle = handle_with(&[(CONFIG_BLOB_KEY, "not-an-envelope")]);

        let err = get_settings_from_config_store(&handle, CONFIG_BLOB_KEY)
            .expect_err("should reject a value that is not a blob envelope");

        assert!(
            !err.to_string().is_empty(),
            "should report a configuration error: {err:?}"
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
