use std::collections::BTreeMap;

use error_stack::{Report, ResultExt};

use crate::config_payload::{settings_from_config_entries, CONFIG_HASH_KEY, CONFIG_KEYS_KEY};
use crate::error::TrustedServerError;
use crate::platform::{PlatformConfigStore, RuntimeServices, StoreName};
use crate::settings::Settings;

const DEFAULT_CONFIG_STORE_ID: &str = "app_config";

/// Loads [`Settings`] from the default `EdgeZero` `app_config` config store.
///
/// The store name is resolved from `EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME`
/// and falls back to the logical id `app_config`.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when metadata or any flattened
/// config entry is missing, cannot be read, fails hash verification, or fails
/// Trusted Server settings validation.
pub fn get_settings_from_services(
    services: &RuntimeServices,
) -> Result<Settings, Report<TrustedServerError>> {
    let store_name = default_config_store_name();
    get_settings_from_config_store(services.config_store(), &store_name)
}

/// Returns the default `EdgeZero` app-config store name.
#[must_use]
pub fn default_config_store_name() -> StoreName {
    StoreName::from(
        std::env::var("EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME")
            .unwrap_or_else(|_| DEFAULT_CONFIG_STORE_ID.to_string()),
    )
}

/// Loads [`Settings`] from a platform config store.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when metadata or any flattened
/// config entry is missing, cannot be read, fails hash verification, or fails
/// Trusted Server settings validation.
pub fn get_settings_from_config_store(
    config_store: &dyn PlatformConfigStore,
    store_name: &StoreName,
) -> Result<Settings, Report<TrustedServerError>> {
    let mut entries = BTreeMap::new();

    let keys_raw = read_config_entry(config_store, store_name, CONFIG_KEYS_KEY)?;
    let keys: Vec<String> =
        serde_json::from_str(&keys_raw).change_context(TrustedServerError::Configuration {
            message: format!("`{CONFIG_KEYS_KEY}` metadata is not a JSON string array"),
        })?;
    entries.insert(CONFIG_KEYS_KEY.to_string(), keys_raw);

    let hash = read_config_entry(config_store, store_name, CONFIG_HASH_KEY)?;
    entries.insert(CONFIG_HASH_KEY.to_string(), hash);

    for key in keys {
        let value = read_config_entry(config_store, store_name, &key)?;
        entries.insert(key, value);
    }

    settings_from_config_entries(&entries)
}

fn read_config_entry(
    config_store: &dyn PlatformConfigStore,
    store_name: &StoreName,
    key: &str,
) -> Result<String, Report<TrustedServerError>> {
    config_store
        .get(store_name, key)
        .change_context(TrustedServerError::Configuration {
            message: format!(
                "failed to read Trusted Server app config key `{key}` from config store `{store_name}`"
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_payload::build_config_payload;
    use crate::platform::PlatformError;
    use crate::settings::Settings;
    use crate::test_support::tests::crate_test_settings_str;

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

    #[test]
    fn loads_settings_from_flattened_config_store_entries() {
        let settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        let payload = build_config_payload(&settings).expect("should build payload");
        let store = MemoryConfigStore {
            entries: payload.entries,
        };

        let loaded = get_settings_from_config_store(&store, &StoreName::from("app_config"))
            .expect("should load settings");

        assert_eq!(
            loaded.publisher.domain, settings.publisher.domain,
            "should load publisher domain"
        );
    }

    #[test]
    fn fails_when_metadata_is_missing() {
        let store = MemoryConfigStore {
            entries: BTreeMap::new(),
        };

        let err = get_settings_from_config_store(&store, &StoreName::from("app_config"))
            .expect_err("should fail when metadata is missing");

        assert!(
            err.to_string().contains(CONFIG_KEYS_KEY),
            "error should mention missing keys metadata"
        );
    }
}
