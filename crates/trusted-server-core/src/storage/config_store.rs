//! Fastly-backed config store (legacy).
//!
//! This module holds the pre-platform [`FastlyConfigStore`] type.
//! New code should use [`crate::platform::PlatformConfigStore`] via
//! [`crate::platform::RuntimeServices`] instead. This type will be removed
//! once all call sites have migrated.

use error_stack::Report;
use fastly::ConfigStore;

use crate::error::TrustedServerError;

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
        // TODO(pr3): replace ConfigStore::open with try_open when all callers migrate
        let store = ConfigStore::open(&self.store_name);
        store.get(key).ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "key '{}' not found in config store '{}'",
                    key, self.store_name
                ),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_store_new_stores_name() {
        let store = FastlyConfigStore::new("test_store");
        assert_eq!(
            store.store_name, "test_store",
            "should store the store name"
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
