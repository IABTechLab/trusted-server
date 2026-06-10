//! Fastly-backed secret store (legacy).
//!
//! This module holds the pre-platform [`FastlySecretStore`] type.
//! New code should use [`crate::platform::PlatformSecretStore`] via
//! [`crate::platform::RuntimeServices`] instead. This type will be removed
//! once all call sites have migrated.

use core::fmt::Display;

use error_stack::{Report, ResultExt as _};
use fastly::SecretStore;

use crate::error::TrustedServerError;

#[derive(Clone)]
enum SecretReadError<LookupError, DecryptError> {
    Lookup(LookupError),
    Decrypt(DecryptError),
}

type SecretBytesResult<LookupError, DecryptError> =
    Result<Option<Vec<u8>>, SecretReadError<LookupError, DecryptError>>;

trait SecretStoreReader: Sized {
    type LookupError: Display;
    type DecryptError: Display;

    fn try_get_bytes(&self, key: &str) -> SecretBytesResult<Self::LookupError, Self::DecryptError>;
}

impl SecretStoreReader for SecretStore {
    type LookupError = fastly::secret_store::LookupError;
    type DecryptError = fastly::secret_store::DecryptError;

    fn try_get_bytes(&self, key: &str) -> SecretBytesResult<Self::LookupError, Self::DecryptError> {
        let secret = self.try_get(key).map_err(SecretReadError::Lookup)?;
        let Some(secret) = secret else {
            return Ok(None);
        };

        secret
            .try_plaintext()
            .map(|bytes| Some(bytes.into_iter().collect()))
            .map_err(SecretReadError::Decrypt)
    }
}

fn get_secret_bytes<S, Open, OpenError>(
    store_name: &str,
    key: &str,
    open_store: Open,
) -> Result<Vec<u8>, Report<TrustedServerError>>
where
    S: SecretStoreReader,
    Open: FnOnce() -> Result<S, OpenError>,
    OpenError: Display,
{
    let store = open_store().map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!("failed to open secret store '{store_name}': {error}"),
        })
    })?;

    store
        .try_get_bytes(key)
        .map_err(|error| match error {
            SecretReadError::Lookup(error) => Report::new(TrustedServerError::Configuration {
                message: format!(
                    "lookup for secret '{key}' in secret store '{store_name}' failed: {error}"
                ),
            }),
            SecretReadError::Decrypt(error) => Report::new(TrustedServerError::Configuration {
                message: format!("failed to decrypt secret '{key}': {error}"),
            }),
        })?
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!("secret '{key}' not found in secret store '{store_name}'"),
            })
        })
}

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
        get_secret_bytes::<SecretStore, _, _>(&self.store_name, key, || {
            SecretStore::open(&self.store_name)
        })
    }

    /// Retrieves a secret value from the store and decodes it as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error if the secret cannot be retrieved or is not valid UTF-8.
    pub fn get_string(&self, key: &str) -> Result<String, Report<TrustedServerError>> {
        let bytes = self.get(key)?;
        String::from_utf8(bytes).change_context(TrustedServerError::Configuration {
            message: "failed to decode secret as UTF-8".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::{self, Display};

    use super::*;

    struct StubSecretStore {
        value: SecretBytesResult<&'static str, &'static str>,
    }

    impl SecretStoreReader for StubSecretStore {
        type LookupError = &'static str;
        type DecryptError = &'static str;

        fn try_get_bytes(
            &self,
            _key: &str,
        ) -> SecretBytesResult<Self::LookupError, Self::DecryptError> {
            self.value.clone()
        }
    }

    #[derive(Clone)]
    struct StubOpenError(&'static str);

    impl Display for StubOpenError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    #[test]
    fn secret_store_new_stores_name() {
        let store = FastlySecretStore::new("test_secrets");
        assert_eq!(
            store.store_name, "test_secrets",
            "should store the store name"
        );
    }

    #[test]
    fn get_secret_bytes_includes_open_error_details() {
        let err = get_secret_bytes::<StubSecretStore, _, _>("signing_keys", "active", || {
            Err(StubOpenError("permission denied"))
        })
        .expect_err("should return an error when the secret store cannot be opened");

        assert!(
            err.to_string()
                .contains("failed to open secret store 'signing_keys': permission denied"),
            "should preserve the original open error message"
        );
    }
}
