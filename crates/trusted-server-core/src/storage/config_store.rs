//! Fastly-backed config store (legacy).
//!
//! This module holds the pre-platform [`FastlyConfigStore`] type.
//! New code should use [`crate::platform::PlatformConfigStore`] via
//! [`crate::platform::RuntimeServices`] instead. This type will be removed
//! once all call sites have migrated.

use core::fmt::Display;

use error_stack::Report;
use fastly::ConfigStore;

use crate::error::TrustedServerError;

trait ConfigStoreReader {
    type LookupError: Display;

    fn try_get(&self, key: &str) -> Result<Option<String>, Self::LookupError>;
}

impl ConfigStoreReader for ConfigStore {
    type LookupError = fastly::config_store::LookupError;

    fn try_get(&self, key: &str) -> Result<Option<String>, Self::LookupError> {
        ConfigStore::try_get(self, key)
    }
}

fn load_config_value<S, Open, OpenError>(
    store_name: &str,
    key: &str,
    open_store: Open,
) -> Result<String, Report<TrustedServerError>>
where
    S: ConfigStoreReader,
    Open: FnOnce(&str) -> Result<S, OpenError>,
    OpenError: Display,
{
    let store = open_store(store_name).map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!("failed to open config store '{store_name}': {error}"),
        })
    })?;

    store
        .try_get(key)
        .map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("lookup for key '{key}' failed: {error}"),
            })
        })?
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!("key '{key}' not found in config store '{store_name}'"),
            })
        })
}

/// Fastly-backed config store with the store name baked in at construction.
///
/// # Migration note
///
/// This type predates the `platform` abstraction. New code should use
/// [`crate::platform::PlatformConfigStore`] via [`crate::platform::RuntimeServices`]
/// instead. `FastlyConfigStore` will be removed once all call sites have
/// migrated.
pub struct FastlyConfigStore {
    store_name: String,
}

impl FastlyConfigStore {
    /// Create a new config store handle for the named store.
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
        }
    }

    /// Retrieves a configuration value from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the key is not found in the config store.
    pub fn get(&self, key: &str) -> Result<String, Report<TrustedServerError>> {
        load_config_value::<ConfigStore, _, _>(&self.store_name, key, ConfigStore::try_open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubConfigStore {
        value: Result<Option<String>, &'static str>,
    }

    impl ConfigStoreReader for StubConfigStore {
        type LookupError = &'static str;

        fn try_get(&self, _key: &str) -> Result<Option<String>, Self::LookupError> {
            self.value.clone()
        }
    }

    #[test]
    fn config_store_new_stores_name() {
        let store = FastlyConfigStore::new("test_store");
        assert_eq!(
            store.store_name, "test_store",
            "should store the store name"
        );
    }

    #[test]
    fn load_config_value_returns_error_when_open_fails() {
        let err = load_config_value::<StubConfigStore, _, _>("jwks_store", "current-kid", |_| {
            Err("open failed")
        })
        .expect_err("should return an error when the store cannot be opened");

        assert!(
            err.to_string().contains("failed to open config store"),
            "should describe the open failure"
        );
    }

    #[test]
    fn load_config_value_returns_error_when_lookup_fails() {
        let err = load_config_value::<StubConfigStore, _, _>("jwks_store", "current-kid", |_| {
            Ok::<StubConfigStore, &'static str>(StubConfigStore {
                value: Err("lookup failed"),
            })
        })
        .expect_err("should return an error when lookup fails");

        assert!(
            err.to_string()
                .contains("lookup for key 'current-kid' failed"),
            "should describe the lookup failure"
        );
    }

    #[test]
    fn load_config_value_returns_error_when_key_is_missing() {
        let err = load_config_value::<StubConfigStore, _, _>("jwks_store", "current-kid", |_| {
            Ok::<StubConfigStore, &'static str>(StubConfigStore { value: Ok(None) })
        })
        .expect_err("should return an error when the key is absent");

        assert!(
            err.to_string()
                .contains("key 'current-kid' not found in config store 'jwks_store'"),
            "should describe the missing key"
        );
    }

    #[test]
    fn config_store_get_in_test_environment() {
        let store = FastlyConfigStore::new("jwks_store");
        let result = store.get("current-kid");
        match result {
            Ok(kid) => println!("Current KID: {}", kid),
            Err(e) => println!("Expected error in test environment: {}", e),
        }
    }
}
