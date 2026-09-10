use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::env_config::EnvConfig;
use error_stack::Report;

use crate::config_payload::{DEFAULT_SECRET_STORE_ID, settings_from_config_blob};
use crate::error::TrustedServerError;
use crate::platform::{PlatformSecretStore, StoreName};
use crate::settings::Settings;

/// Canonical logical config store used by Trusted Server app config.
pub const DEFAULT_CONFIG_STORE_ID: &str = "trusted_server_config";

/// Resolves the `EdgeZero` app-config store name from runtime configuration.
#[must_use]
pub fn config_store_name(env: &EnvConfig) -> StoreName {
    StoreName::from(env.store_name("config", DEFAULT_CONFIG_STORE_ID))
}

/// Resolves the config-store key containing the app-config blob.
#[must_use]
pub fn config_key(env: &EnvConfig) -> String {
    env.store_key("config", DEFAULT_CONFIG_STORE_ID)
}

/// Returns the default `EdgeZero` app-config store name.
#[must_use]
pub fn default_config_store_name() -> StoreName {
    config_store_name(&EnvConfig::from_env())
}

/// Returns the default config-store key containing the app-config blob.
#[must_use]
pub fn default_config_key() -> String {
    config_key(&EnvConfig::from_env())
}

/// Returns the default `EdgeZero` secret-store name for Trusted Server secrets.
#[must_use]
pub fn default_secret_store_name() -> StoreName {
    StoreName::from(EnvConfig::from_env().store_name("secrets", DEFAULT_SECRET_STORE_ID))
}

/// Loads [`Settings`] from an `EdgeZero` [`ConfigStoreHandle`] and key.
///
/// The handle is already bound to a specific config store, so only the blob
/// `key` is supplied. Reads resolve through the handle's async
/// [`ConfigStoreHandle::get`]. The handle returns a fully resolved envelope:
/// platform-specific storage details such as Fastly's config-entry chunking are
/// reassembled by `EdgeZero`'s config store, not here. Secret references in the
/// verified blob are resolved from `secret_store` before deserialization.
///
/// This is an async startup read: adapters drive it to completion at process
/// boot (outside any request executor).
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the config blob is
/// missing, cannot be read, fails envelope verification, secret resolution,
/// or Trusted Server settings validation.
pub async fn get_settings_from_config_store(
    config_store: &ConfigStoreHandle,
    key: &str,
    secret_store: &dyn PlatformSecretStore,
    default_secret_store_name: &StoreName,
) -> Result<Settings, Report<TrustedServerError>> {
    let envelope_json = read_config_entry(config_store, key).await?;
    settings_from_config_blob(&envelope_json, secret_store, default_secret_store_name).await
}

async fn read_config_entry(
    config_store: &ConfigStoreHandle,
    key: &str,
) -> Result<String, Report<TrustedServerError>> {
    match config_store.get(key).await {
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
    use crate::platform::{PlatformError, StoreId};
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

    struct EchoSecretStore;

    #[async_trait(?Send)]
    impl PlatformSecretStore for EchoSecretStore {
        async fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            let value = match key {
                "unit-test-proxy-secret" => "unit-test-proxy-secret-32-bytes-ok",
                _ => key,
            };
            Ok(value.as_bytes().to_vec())
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    fn envelope_json(settings: &Settings) -> String {
        let payload = serde_json::to_value(settings).expect("should serialize settings");
        let envelope = BlobEnvelope::new(payload, "2026-01-01T00:00:00Z".to_string());
        serde_json::to_string(&envelope).expect("should serialize envelope")
    }

    fn load_settings(
        handle: &ConfigStoreHandle,
        key: &str,
    ) -> Result<Settings, Report<TrustedServerError>> {
        futures::executor::block_on(get_settings_from_config_store(
            handle,
            key,
            &EchoSecretStore,
            &StoreName::from("trusted_server_secrets"),
        ))
    }

    #[test]
    fn config_selectors_default_to_the_logical_store_id() {
        let env = EnvConfig::default();

        assert_eq!(
            config_store_name(&env),
            StoreName::from("trusted_server_config")
        );
        assert_eq!(config_key(&env), "trusted_server_config");
    }

    #[test]
    fn config_key_selects_the_staging_key() {
        let env = EnvConfig::from_vars([(
            "EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY",
            "trusted_server_config_staging",
        )]);

        assert_eq!(config_key(&env), "trusted_server_config_staging");
    }

    #[test]
    fn config_store_name_override_preserves_the_independently_selected_key() {
        let env = EnvConfig::from_vars([
            (
                "EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME",
                "publisher-config-store",
            ),
            (
                "EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY",
                "trusted_server_config_staging",
            ),
        ]);

        assert_eq!(
            config_store_name(&env),
            StoreName::from("publisher-config-store")
        );
        assert_eq!(config_key(&env), "trusted_server_config_staging");
    }

    #[test]
    fn loads_settings_from_config_blob_entry() {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
        let envelope_json = envelope_json(&settings);
        let handle = handle_with(&[(CONFIG_BLOB_KEY, &envelope_json)]);

        let loaded = load_settings(&handle, CONFIG_BLOB_KEY).expect("should load settings");

        assert_eq!(
            loaded.publisher.domain, settings.publisher.domain,
            "should deserialize the config blob read through the EdgeZero handle"
        );
    }

    #[test]
    fn fails_when_blob_value_is_not_an_envelope() {
        let handle = handle_with(&[(CONFIG_BLOB_KEY, "not-an-envelope")]);

        let err = load_settings(&handle, CONFIG_BLOB_KEY)
            .expect_err("should reject a value that is not a blob envelope");

        assert!(
            !err.to_string().is_empty(),
            "should report a configuration error: {err:?}"
        );
    }

    #[test]
    fn fails_when_blob_key_is_missing() {
        let handle = handle_with(&[]);

        let err =
            load_settings(&handle, CONFIG_BLOB_KEY).expect_err("should fail when blob is missing");

        assert!(
            err.to_string().contains(CONFIG_BLOB_KEY),
            "error should mention missing blob key"
        );
    }
}
