use edgezero_core::env_config::EnvConfig;
use error_stack::{Report, ResultExt};

pub use crate::config_payload::DEFAULT_CONFIG_STORE_ID;
use crate::config_payload::{DEFAULT_SECRET_STORE_ID, settings_from_config_blob_with};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationBuilder;
use crate::platform::{PlatformConfigStore, PlatformSecretStore, PolicyOrigin, StoreName};
use crate::settings::Settings;

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

/// Resolves the `EdgeZero` app-config store name from the process environment.
///
/// Native adapters use this wrapper. An adapter whose platform
/// links each store to the deployment under its logical ID opens the store
/// under that ID and reads no store name at runtime. Without an override,
/// this wrapper also returns the manifest default logical store ID.
#[must_use]
pub fn default_config_store_name() -> StoreName {
    config_store_name(&EnvConfig::from_env())
}

/// Resolves the app-config blob key from the process environment.
///
/// Native adapters use this wrapper and honour a `__KEY`
/// override, which must match `ts config push --key`, because an ordinary
/// push without that flag writes at the logical store ID. An adapter that
/// chooses its key for each request resolves the key itself.
#[must_use]
pub fn default_config_key() -> String {
    config_key(&EnvConfig::from_env())
}

/// Returns the default `EdgeZero` secret-store name for Trusted Server secrets.
#[must_use]
pub fn default_secret_store_name() -> StoreName {
    StoreName::from(EnvConfig::from_env().store_name("secrets", DEFAULT_SECRET_STORE_ID))
}

/// Loads [`Settings`] from a platform config store and key, validating
/// against the built-in integrations only, with the permission policy they
/// name read from `policy_origin`.
///
/// A deployment that composes builders of its own calls
/// [`get_settings_from_config_store_with`] instead, because validation runs
/// during the load and refuses a `[demand]` or `[ad-server]` name it cannot
/// see a builder for.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the config blob is
/// missing, cannot be read, fails envelope verification, secret resolution,
/// or Trusted Server settings validation, or when its permission policy does
/// not load.
pub fn get_settings_from_config_store(
    config_store: &dyn PlatformConfigStore,
    secret_store: &dyn PlatformSecretStore,
    store_name: &StoreName,
    key: &str,
    default_secret_store_name: &StoreName,
    policy_origin: PolicyOrigin<'_>,
) -> Result<Settings, Report<TrustedServerError>> {
    get_settings_from_config_store_with(
        config_store,
        secret_store,
        store_name,
        key,
        default_secret_store_name,
        policy_origin,
        &[],
    )
}

/// Loads [`Settings`] from a platform config store and key, validating
/// against the built-in integrations followed by `extra_integrations`, with
/// the permission policy they name read from `policy_origin`.
///
/// The entry is read as the platform's config store returns it, whole. A
/// platform that stores a long document in parts reassembles it in its own
/// config store read.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the config blob is
/// missing, cannot be read, fails envelope verification, secret resolution,
/// or Trusted Server settings validation, or when its permission policy does
/// not load.
pub fn get_settings_from_config_store_with(
    config_store: &dyn PlatformConfigStore,
    secret_store: &dyn PlatformSecretStore,
    store_name: &StoreName,
    key: &str,
    default_secret_store_name: &StoreName,
    policy_origin: PolicyOrigin<'_>,
    extra_integrations: &[IntegrationBuilder],
) -> Result<Settings, Report<TrustedServerError>> {
    let envelope_json = read_config_entry(config_store, store_name, key)?;
    settings_from_config_blob_with(
        &envelope_json,
        secret_store,
        default_secret_store_name,
        policy_origin,
        extra_integrations,
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};
    use crate::platform::{PlatformError, StoreId};
    use crate::settings::Settings;
    use crate::test_support::tests::{crate_test_settings_str, sample_permissions};
    use edgezero_core::blob_envelope::BlobEnvelope;
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
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    struct EchoSecretStore;

    impl PlatformSecretStore for EchoSecretStore {
        fn get_bytes(
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

    /// The blob envelope `ts config push` writes for `settings`, naming the
    /// repository's sample policy.
    fn envelope_json(settings: &Settings) -> String {
        let mut settings = settings.clone();
        settings.permissions = sample_permissions();
        let data = serde_json::to_value(&settings).expect("should serialize settings to JSON");
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
        serde_json::to_string(&envelope).expect("should serialize envelope")
    }

    fn load_settings(
        config_store: &dyn PlatformConfigStore,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Settings, Report<TrustedServerError>> {
        get_settings_from_config_store(
            config_store,
            &EchoSecretStore,
            store_name,
            key,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
        )
    }

    #[test]
    fn config_defaults_match_edgezero_manifest() {
        let manifest = edgezero_core::manifest::ManifestLoader::try_load_from_str(include_str!(
            "../../../edgezero.toml"
        ))
        .expect("should load the repository EdgeZero manifest");
        let manifest_default = manifest
            .manifest()
            .stores
            .config
            .as_ref()
            .expect("should declare [stores.config]")
            .default_id();

        assert_eq!(
            DEFAULT_CONFIG_STORE_ID, manifest_default,
            "compiled default should match edgezero.toml"
        );
        assert_eq!(
            CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID,
            "default blob key should match the default config store id"
        );
        assert_eq!(
            manifest_default, "trusted_server_config",
            "Trusted Server should retain its expected default config store"
        );
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
        let store = MemoryConfigStore {
            entries: BTreeMap::from([(CONFIG_BLOB_KEY.to_string(), envelope_json)]),
        };

        let loaded = load_settings(
            &store,
            &StoreName::from(DEFAULT_CONFIG_STORE_ID),
            CONFIG_BLOB_KEY,
        )
        .expect("should load settings");

        assert_eq!(
            loaded.publisher.domain, settings.publisher.domain,
            "should load publisher domain"
        );
    }

    #[test]
    fn fails_when_blob_key_is_missing() {
        let store = MemoryConfigStore {
            entries: BTreeMap::new(),
        };

        let err = load_settings(
            &store,
            &StoreName::from(DEFAULT_CONFIG_STORE_ID),
            CONFIG_BLOB_KEY,
        )
        .expect_err("should fail when blob is missing");

        assert!(
            err.to_string().contains(CONFIG_BLOB_KEY),
            "error should mention missing blob key"
        );
    }
}
