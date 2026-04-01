//! Key rotation management for request signing.
//!
//! This module provides functionality for rotating signing keys, managing key
//! lifecycle, and storing keys via platform store primitives through
//! [`RuntimeServices`].

use std::sync::LazyLock;

use base64::{engine::general_purpose, Engine};
use ed25519_dalek::SigningKey;
use error_stack::{Report, ResultExt};
use jose_jwk::Jwk;

use crate::error::TrustedServerError;
use crate::platform::{RuntimeServices, StoreId, StoreName};
use crate::request_signing::JWKS_CONFIG_STORE_NAME;

use super::Keypair;

static JWKS_STORE_NAME: LazyLock<StoreName> =
    LazyLock::new(|| StoreName::from(JWKS_CONFIG_STORE_NAME));

#[derive(Debug, Clone)]
pub struct KeyRotationResult {
    pub new_kid: String,
    pub previous_kid: Option<String>,
    pub active_kids: Vec<String>,
    pub jwk: Jwk,
}

/// Manages signing key lifecycle using platform store primitives.
///
/// Reads use the edge-visible store name ([`JWKS_CONFIG_STORE_NAME`]).
/// Writes use the management API store identifiers supplied at construction.
pub struct KeyRotationManager {
    /// Management API store ID for config store writes.
    config_store_id: StoreId,
    /// Management API store ID for secret store writes.
    secret_store_id: StoreId,
}

impl KeyRotationManager {
    /// Creates a new key rotation manager.
    ///
    /// The `config_store_id` and `secret_store_id` are platform management API
    /// identifiers used for write operations. Edge reads use the store names
    /// defined in [`JWKS_CONFIG_STORE_NAME`] and
    /// [`crate::request_signing::SIGNING_SECRET_STORE_NAME`].
    #[must_use]
    pub fn new(config_store_id: impl Into<String>, secret_store_id: impl Into<String>) -> Self {
        Self {
            config_store_id: StoreId::from(config_store_id.into()),
            secret_store_id: StoreId::from(secret_store_id.into()),
        }
    }

    /// Rotates the signing key by generating a new keypair and storing it.
    ///
    /// # Errors
    ///
    /// Returns an error if key storage or update operations fail.
    pub fn rotate_key(
        &self,
        services: &RuntimeServices,
        kid: Option<String>,
    ) -> Result<KeyRotationResult, Report<TrustedServerError>> {
        let new_kid = kid.unwrap_or_else(generate_date_based_kid);

        let keypair = Keypair::generate();
        let jwk = keypair.get_jwk(new_kid.clone());
        let previous_kid = services
            .config_store()
            .get(&JWKS_STORE_NAME, "current-kid")
            .ok();

        self.store_private_key(services, &new_kid, &keypair.signing_key)?;
        self.store_public_jwk(services, &new_kid, &jwk)?;

        let active_kids = match &previous_kid {
            Some(prev) if prev != &new_kid => vec![prev.clone(), new_kid.clone()],
            _ => vec![new_kid.clone()],
        };

        self.update_current_kid(services, &new_kid)?;
        self.update_active_kids(services, &active_kids)?;

        Ok(KeyRotationResult {
            new_kid,
            previous_kid,
            active_kids,
            jwk,
        })
    }

    fn store_private_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
        signing_key: &SigningKey,
    ) -> Result<(), Report<TrustedServerError>> {
        let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());

        services
            .secret_store()
            .create(&self.secret_store_id, kid, &key_b64)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to store private key '{}'", kid),
            })
    }

    fn store_public_jwk(
        &self,
        services: &RuntimeServices,
        kid: &str,
        jwk: &Jwk,
    ) -> Result<(), Report<TrustedServerError>> {
        let jwk_json = serde_json::to_string(jwk).map_err(|e| {
            Report::new(TrustedServerError::Configuration {
                message: format!("failed to serialize JWK: {}", e),
            })
        })?;

        services
            .config_store()
            .put(&self.config_store_id, kid, &jwk_json)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to store public JWK '{}'", kid),
            })
    }

    fn update_current_kid(
        &self,
        services: &RuntimeServices,
        kid: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        services
            .config_store()
            .put(&self.config_store_id, "current-kid", kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to update current-kid".into(),
            })
    }

    fn update_active_kids(
        &self,
        services: &RuntimeServices,
        active_kids: &[String],
    ) -> Result<(), Report<TrustedServerError>> {
        let active_kids_str = active_kids.join(",");

        services
            .config_store()
            .put(&self.config_store_id, "active-kids", &active_kids_str)
            .change_context(TrustedServerError::Configuration {
                message: "failed to update active-kids".into(),
            })
    }

    /// Lists all active key IDs.
    ///
    /// # Errors
    ///
    /// Returns an error if the active keys cannot be retrieved from the config store.
    pub fn list_active_keys(
        &self,
        services: &RuntimeServices,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        let active_kids_str = services
            .config_store()
            .get(&JWKS_STORE_NAME, "active-kids")
            .change_context(TrustedServerError::Configuration {
                message: "failed to read active-kids from config store".into(),
            })?;

        Ok(active_kids_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Deactivates a key by removing it from the active keys list.
    ///
    /// # Errors
    ///
    /// Returns an error if this would deactivate the last active key, or if the update fails.
    pub fn deactivate_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut active_kids = self.list_active_keys(services)?;
        active_kids.retain(|k| k != kid);

        if active_kids.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "cannot deactivate the last active key".into(),
            }));
        }

        self.update_active_kids(services, &active_kids)
    }

    /// Deletes a key by deactivating it and removing it from storage.
    ///
    /// # Errors
    ///
    /// Returns an error if deactivation fails or if the key cannot be deleted from storage.
    pub fn delete_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        self.deactivate_key(services, kid)?;

        services
            .config_store()
            .delete(&self.config_store_id, kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to delete JWK from config store".into(),
            })?;

        services
            .secret_store()
            .delete(&self.secret_store_id, kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to delete signing key from secret store".into(),
            })?;

        Ok(())
    }
}

/// Generates a date-based key ID in the format `ts-YYYY-MM-DD`.
#[must_use]
pub fn generate_date_based_kid() -> String {
    use chrono::Utc;
    format!("ts-{}", Utc::now().format("%Y-%m-%d"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use error_stack::Report;

    use crate::platform::test_support::build_services_with_config_and_secret;
    use crate::platform::{
        PlatformConfigStore, PlatformError, PlatformSecretStore, StoreId, StoreName,
    };
    use crate::request_signing::Keypair;

    use super::*;

    // ---------------------------------------------------------------------------
    // Spy stores: record put/create/delete calls, serve preset get values
    // ---------------------------------------------------------------------------

    struct SpyConfigStore {
        data: Mutex<HashMap<String, String>>,
        puts: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
    }

    impl SpyConfigStore {
        fn new(initial: HashMap<String, String>) -> Self {
            Self {
                data: Mutex::new(initial),
                puts: Mutex::new(vec![]),
                deletes: Mutex::new(vec![]),
            }
        }
    }

    impl PlatformConfigStore for SpyConfigStore {
        fn get(&self, _: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.data
                .lock()
                .expect("should lock data")
                .get(key)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::ConfigStore))
        }

        fn put(
            &self,
            store_id: &StoreId,
            key: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.puts.lock().expect("should lock puts").push((
                store_id.to_string(),
                key.to_string(),
                value.to_string(),
            ));
            self.data
                .lock()
                .expect("should lock data")
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
            self.deletes
                .lock()
                .expect("should lock deletes")
                .push((store_id.to_string(), key.to_string()));
            self.data.lock().expect("should lock data").remove(key);
            Ok(())
        }
    }

    struct SpySecretStore {
        creates: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
    }

    impl SpySecretStore {
        fn new() -> Self {
            Self {
                creates: Mutex::new(vec![]),
                deletes: Mutex::new(vec![]),
            }
        }
    }

    impl PlatformSecretStore for SpySecretStore {
        fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore))
        }

        fn create(
            &self,
            store_id: &StoreId,
            name: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.creates.lock().expect("should lock creates").push((
                store_id.to_string(),
                name.to_string(),
                value.to_string(),
            ));
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
            self.deletes
                .lock()
                .expect("should lock deletes")
                .push((store_id.to_string(), name.to_string()));
            Ok(())
        }
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[test]
    fn generate_date_based_kid_has_correct_format() {
        let kid = generate_date_based_kid();
        assert!(kid.starts_with("ts-"), "should start with 'ts-'");
        assert!(kid.len() >= 13, "should be at least 13 characters");
        let parts: Vec<&str> = kid.split('-').collect();
        assert_eq!(parts.len(), 4, "should have 4 dash-separated parts");
        assert_eq!(parts[0], "ts", "first part should be 'ts'");
    }

    #[test]
    fn new_is_infallible_and_stores_ids() {
        let manager = KeyRotationManager::new("cfg-store-123", "sec-store-456");
        assert_eq!(
            manager.config_store_id.as_ref(),
            "cfg-store-123",
            "should store config_store_id"
        );
        assert_eq!(
            manager.secret_store_id.as_ref(),
            "sec-store-456",
            "should store secret_store_id"
        );
    }

    #[test]
    fn rotate_key_stores_private_key_via_secret_store_create() {
        let config_store = SpyConfigStore::new(HashMap::new());
        let secret_store = SpySecretStore::new();
        let services = build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.rotate_key(&services, Some("new-kid".to_string()));

        assert!(result.is_ok(), "should succeed when stores accept writes");
        let rotation = result.expect("should produce rotation result");
        assert_eq!(rotation.new_kid, "new-kid", "should use the provided kid");
        assert!(
            rotation.active_kids.contains(&"new-kid".to_string()),
            "should include new kid in active kids"
        );
    }

    #[test]
    fn deactivate_key_fails_when_only_one_key_remains() {
        let mut data = HashMap::new();
        data.insert("active-kids".to_string(), "only-key".to_string());
        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services = build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.deactivate_key(&services, "only-key");

        assert!(
            result.is_err(),
            "should fail to deactivate the last active key"
        );
    }

    #[test]
    fn key_rotation_result_structure_is_valid() {
        let jwk = Keypair::generate().get_jwk("test-key".to_string());
        let result = KeyRotationResult {
            new_kid: "ts-2024-01-01".to_string(),
            previous_kid: Some("ts-2023-12-31".to_string()),
            active_kids: vec!["ts-2023-12-31".to_string(), "ts-2024-01-01".to_string()],
            jwk: jwk.clone(),
        };

        assert_eq!(result.new_kid, "ts-2024-01-01");
        assert_eq!(result.previous_kid, Some("ts-2023-12-31".to_string()));
        assert_eq!(result.active_kids.len(), 2);
        assert_eq!(result.jwk.prm.kid, Some("test-key".to_string()));
    }
}
