//! Fastly-backed secret store (legacy).
//!
//! This module holds the pre-platform [`FastlySecretStore`] type.
//! New code should use [`crate::platform::PlatformSecretStore`] via
//! [`crate::platform::RuntimeServices`] instead. This type will be removed
//! once all call sites have migrated.

use error_stack::{Report, ResultExt};
use fastly::SecretStore;

use crate::error::TrustedServerError;

/// Fastly-backed secret store with the store name baked in at construction.
///
/// # Migration note
///
/// This type predates the `platform` abstraction. New code should use
/// [`crate::platform::PlatformSecretStore`] via [`crate::platform::RuntimeServices`]
/// instead. `FastlySecretStore` will be removed once all call sites have
/// migrated.
pub struct FastlySecretStore {
    store_name: String,
}

impl FastlySecretStore {
    /// Create a new secret store handle for the named store.
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
        }
    }

    /// Retrieves a secret value as raw bytes from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the secret store cannot be opened, the key is not
    /// found, or the plaintext cannot be retrieved.
    pub fn get(&self, key: &str) -> Result<Vec<u8>, Report<TrustedServerError>> {
        let store = SecretStore::open(&self.store_name).map_err(|_| {
            Report::new(TrustedServerError::Configuration {
                message: format!("failed to open secret store '{}'", self.store_name),
            })
        })?;

        let secret = store.get(key).ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "secret '{}' not found in secret store '{}'",
                    key, self.store_name
                ),
            })
        })?;

        secret
            .try_plaintext()
            .map_err(|_| {
                Report::new(TrustedServerError::Configuration {
                    message: "failed to retrieve secret plaintext".into(),
                })
            })
            .map(|bytes| bytes.into_iter().collect())
    }

    /// Retrieves a secret value from the store and decodes it as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error if the secret cannot be retrieved or is not valid UTF-8.
    pub fn get_string(&self, key: &str) -> Result<String, Report<TrustedServerError>> {
        let bytes = self.get(key)?;
        String::from_utf8(bytes).change_context(TrustedServerError::Configuration {
            message: "failed to decode secret as UTF-8".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FastlyConfigStore;

    #[test]
    fn secret_store_new_stores_name() {
        let store = FastlySecretStore::new("test_secrets");
        assert_eq!(
            store.store_name, "test_secrets",
            "should store the store name"
        );
    }

    #[test]
    fn secret_store_get_in_test_environment() {
        let store = FastlySecretStore::new("signing_keys");
        let config_store = FastlyConfigStore::new("jwks_store");

        match config_store.get("current-kid") {
            Ok(kid) => match store.get(&kid) {
                Ok(bytes) => {
                    assert!(!bytes.is_empty(), "should have non-empty secret bytes");
                }
                Err(e) => println!("Expected error in test environment: {}", e),
            },
            Err(e) => println!("Expected error in test environment: {}", e),
        }
    }
}
