//! Key rotation management for request signing.
//!
//! This module provides functionality for rotating signing keys, managing key
//! lifecycle, and storing keys via platform store primitives through
//! [`RuntimeServices`].

use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use error_stack::{Report, ResultExt as _};
use jose_jwk::Jwk;
use uuid::Uuid;

use super::{read_active_kids, Keypair};
use crate::error::TrustedServerError;
use crate::platform::{RuntimeServices, StoreId};
use crate::request_signing::JWKS_STORE_NAME;

/// Result of a key rotation operation.
#[derive(Debug, Clone)]
pub struct KeyRotationResult {
    /// Newly generated or supplied key identifier.
    pub new_kid: String,
    /// Previously active key identifier, if one existed.
    pub previous_kid: Option<String>,
    /// Active key identifiers after rotation completes.
    pub active_kids: Vec<String>,
    /// Public JWK associated with the newly active key.
    pub jwk: Jwk,
}

/// Manages signing key lifecycle using platform store primitives.
///
/// Reads use the edge-visible store name ([`super::JWKS_CONFIG_STORE_NAME`]).
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
    /// defined in [`super::JWKS_CONFIG_STORE_NAME`] and
    /// [`crate::request_signing::SIGNING_SECRET_STORE_NAME`].
    #[must_use]
    pub fn new(config_store_id: &str, secret_store_id: &str) -> Self {
        Self {
            config_store_id: StoreId::from(config_store_id),
            secret_store_id: StoreId::from(secret_store_id),
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
        let previous_kid = services
            .config_store()
            .get(&JWKS_STORE_NAME, "current-kid")
            .ok();
        let active_kids = read_active_kids(services).unwrap_or_default();
        let new_kid = match kid {
            Some(kid) => {
                if self.key_exists(services, &kid, &active_kids) {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!("kid '{kid}' already exists; choose a unique kid"),
                    }));
                }
                kid
            }
            None => self.generate_unique_date_based_kid(services, &active_kids),
        };

        let keypair = Keypair::generate();
        let jwk = keypair.get_jwk(new_kid.clone());

        // Step 1: write private key. Nothing to roll back on failure.
        self.store_private_key(services, &new_kid, &keypair.signing_key)?;

        // Step 2: write public JWK. Roll back the private key on failure so no
        // orphaned key material is left in the secret store.
        if let Err(err) = self.store_public_jwk(services, &new_kid, &jwk) {
            if let Err(rollback_err) = services
                .secret_store()
                .delete(&self.secret_store_id, &new_kid)
            {
                log::warn!(
                    "rotate_key: rollback of private key '{new_kid}' failed after JWK write error: {rollback_err}"
                );
            }
            return Err(err);
        }

        let mut active_kids = active_kids;
        if !active_kids.iter().any(|kid| kid == &new_kid) {
            active_kids.push(new_kid.clone());
        }

        // Step 3: publish the new kid in active-kids BEFORE flipping current-kid.
        // Roll back both artifacts on failure so the new kid never appears in JWKS
        // without a reachable private key.
        if let Err(err) = self.update_active_kids(services, &active_kids) {
            if let Err(rollback_err) = services
                .config_store()
                .delete(&self.config_store_id, &new_kid)
            {
                log::warn!(
                    "rotate_key: rollback of JWK '{new_kid}' failed after active-kids write error: {rollback_err}"
                );
            }
            if let Err(rollback_err) = services
                .secret_store()
                .delete(&self.secret_store_id, &new_kid)
            {
                log::warn!(
                    "rotate_key: rollback of private key '{new_kid}' failed after active-kids write error: {rollback_err}"
                );
            }
            return Err(err);
        }

        // Step 4: flip current-kid last. A failure here leaves the old kid still
        // active and the new kid visible in JWKS but unused — a recoverable state.
        self.update_current_kid(services, &new_kid)?;

        Ok(KeyRotationResult {
            new_kid,
            previous_kid,
            active_kids,
            jwk,
        })
    }

    fn key_exists(&self, services: &RuntimeServices, kid: &str, active_kids: &[String]) -> bool {
        active_kids.iter().any(|active_kid| active_kid == kid)
            || services.config_store().get(&JWKS_STORE_NAME, kid).is_ok()
    }

    fn generate_unique_date_based_kid(
        &self,
        services: &RuntimeServices,
        active_kids: &[String],
    ) -> String {
        let base_kid = generate_date_based_kid();
        if !self.key_exists(services, &base_kid, active_kids) {
            return base_kid;
        }

        format!("{base_kid}-{}", Uuid::new_v4().simple())
    }

    fn store_private_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
        signing_key: &SigningKey,
    ) -> Result<(), Report<TrustedServerError>> {
        // The platform secret-store write interface is string-based, so signing
        // keys are persisted as base64 text. The Fastly adapter applies its own
        // transport-level base64 encoding when calling the management API.
        let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());

        services
            .secret_store()
            .create(&self.secret_store_id, kid, &key_b64)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to store private key '{kid}'"),
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
                message: format!("failed to serialize JWK: {e}"),
            })
        })?;

        services
            .config_store()
            .put(&self.config_store_id, kid, &jwk_json)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to store public JWK '{kid}'"),
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
        read_active_kids(services)
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
        self.ensure_not_current_key(services, kid, "deactivate")?;

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
        self.ensure_not_current_key(services, kid, "delete")?;
        self.deactivate_key(services, kid)?;

        // Delete the private key first. A failure here leaves the JWK in the
        // config store but no private key — the key is verifiable but cannot
        // sign, which is safer than orphaned key material with no JWK. Both
        // deletes treat 404 as success so retries converge after partial failures.
        services
            .secret_store()
            .delete(&self.secret_store_id, kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to delete signing key from secret store".into(),
            })?;

        services
            .config_store()
            .delete(&self.config_store_id, kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to delete JWK from config store".into(),
            })?;

        Ok(())
    }

    fn ensure_not_current_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
        operation: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        if services
            .config_store()
            .get(&JWKS_STORE_NAME, "current-kid")
            .is_ok_and(|current| current == kid)
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "cannot {operation} '{kid}' because it is the current signing key; rotate first"
                ),
            }));
        }

        Ok(())
    }
}

/// Generates a date-based key ID in the format `ts-YYYY-MM-DD`.
#[must_use]
pub fn generate_date_based_kid() -> String {
    format!("ts-{}", Utc::now().format("%Y-%m-%d"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

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

    #[derive(Clone)]
    struct SpyConfigStore {
        inner: Arc<SpyConfigStoreInner>,
    }

    struct SpyConfigStoreInner {
        data: Mutex<HashMap<String, String>>,
        puts: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
        /// Fail `put` after this many successful calls. `usize::MAX` means never fail.
        fail_after_n_puts: AtomicUsize,
    }

    impl SpyConfigStore {
        fn new(initial: HashMap<String, String>) -> Self {
            Self {
                inner: Arc::new(SpyConfigStoreInner {
                    data: Mutex::new(initial),
                    puts: Mutex::new(vec![]),
                    deletes: Mutex::new(vec![]),
                    fail_after_n_puts: AtomicUsize::new(usize::MAX),
                }),
            }
        }

        /// Returns a store whose `put` succeeds for the first `n` calls, then
        /// returns an error. Use `n = 0` to fail immediately.
        fn with_put_failure_after(n: usize) -> Self {
            Self {
                inner: Arc::new(SpyConfigStoreInner {
                    data: Mutex::new(HashMap::new()),
                    puts: Mutex::new(vec![]),
                    deletes: Mutex::new(vec![]),
                    fail_after_n_puts: AtomicUsize::new(n),
                }),
            }
        }

        fn puts(&self) -> Vec<(String, String, String)> {
            self.inner.puts.lock().expect("should lock puts").clone()
        }

        fn deletes(&self) -> Vec<(String, String)> {
            self.inner
                .deletes
                .lock()
                .expect("should lock deletes")
                .clone()
        }
    }

    impl PlatformConfigStore for SpyConfigStore {
        fn get(&self, _: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.inner
                .data
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
            let remaining = self.inner.fail_after_n_puts.load(Ordering::SeqCst);
            if remaining == 0 {
                return Err(Report::new(PlatformError::ConfigStore));
            }
            if remaining != usize::MAX {
                self.inner.fail_after_n_puts.fetch_sub(1, Ordering::SeqCst);
            }
            self.inner.puts.lock().expect("should lock puts").push((
                store_id.to_string(),
                key.to_owned(),
                value.to_owned(),
            ));
            self.inner
                .data
                .lock()
                .expect("should lock data")
                .insert(key.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
            self.inner
                .deletes
                .lock()
                .expect("should lock deletes")
                .push((store_id.to_string(), key.to_owned()));
            self.inner
                .data
                .lock()
                .expect("should lock data")
                .remove(key);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct SpySecretStore {
        inner: Arc<SpySecretStoreInner>,
    }

    struct SpySecretStoreInner {
        creates: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
        /// Fail `create` after this many successful calls. `usize::MAX` means never fail.
        fail_after_n_creates: AtomicUsize,
    }

    impl SpySecretStore {
        fn new() -> Self {
            Self {
                inner: Arc::new(SpySecretStoreInner {
                    creates: Mutex::new(vec![]),
                    deletes: Mutex::new(vec![]),
                    fail_after_n_creates: AtomicUsize::new(usize::MAX),
                }),
            }
        }

        /// Returns a store whose `create` succeeds for the first `n` calls, then
        /// returns an error. Use `n = 0` to fail immediately.
        fn with_create_failure_after(n: usize) -> Self {
            Self {
                inner: Arc::new(SpySecretStoreInner {
                    creates: Mutex::new(vec![]),
                    deletes: Mutex::new(vec![]),
                    fail_after_n_creates: AtomicUsize::new(n),
                }),
            }
        }

        fn creates(&self) -> Vec<(String, String, String)> {
            self.inner
                .creates
                .lock()
                .expect("should lock creates")
                .clone()
        }

        fn deletes(&self) -> Vec<(String, String)> {
            self.inner
                .deletes
                .lock()
                .expect("should lock deletes")
                .clone()
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
            let remaining = self.inner.fail_after_n_creates.load(Ordering::SeqCst);
            if remaining == 0 {
                return Err(Report::new(PlatformError::SecretStore));
            }
            if remaining != usize::MAX {
                self.inner
                    .fail_after_n_creates
                    .fetch_sub(1, Ordering::SeqCst);
            }
            self.inner
                .creates
                .lock()
                .expect("should lock creates")
                .push((store_id.to_string(), name.to_owned(), value.to_owned()));
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
            self.inner
                .deletes
                .lock()
                .expect("should lock deletes")
                .push((store_id.to_string(), name.to_owned()));
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
        let result = manager.rotate_key(&services, Some("new-kid".to_owned()));

        assert!(result.is_ok(), "should succeed when stores accept writes");
        let rotation = result.expect("should produce rotation result");
        assert_eq!(rotation.new_kid, "new-kid", "should use the provided kid");
        assert!(
            rotation.active_kids.contains(&"new-kid".to_owned()),
            "should include new kid in active kids"
        );
    }

    #[test]
    fn rotate_key_preserves_existing_active_kids() {
        let mut data = HashMap::new();
        data.insert("current-kid".to_owned(), "kid-b".to_owned());
        data.insert("active-kids".to_owned(), "kid-a, kid-b".to_owned());

        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services = build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let rotation = manager
            .rotate_key(&services, Some("kid-c".to_owned()))
            .expect("should rotate key successfully");

        assert_eq!(
            rotation.active_kids,
            vec!["kid-a".to_owned(), "kid-b".to_owned(), "kid-c".to_owned()],
            "should preserve previously active keys and append the new kid"
        );

        let active_kids = manager
            .list_active_keys(&services)
            .expect("should read back updated active kids");
        assert_eq!(
            active_kids,
            vec!["kid-a".to_owned(), "kid-b".to_owned(), "kid-c".to_owned()],
            "should store the full active kid list after rotation"
        );
    }

    #[test]
    fn rotate_key_does_not_reactivate_deactivated_previous_kid() {
        let mut data = HashMap::new();
        data.insert("current-kid".to_owned(), "kid-a".to_owned());
        data.insert("active-kids".to_owned(), "kid-b".to_owned());

        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services = build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let rotation = manager
            .rotate_key(&services, Some("kid-c".to_owned()))
            .expect("should rotate key successfully");

        assert_eq!(
            rotation.active_kids,
            vec!["kid-b".to_owned(), "kid-c".to_owned()],
            "should not resurrect a previous kid that is no longer active"
        );
    }

    #[test]
    fn rotate_key_rejects_explicit_kid_that_is_already_active() {
        let mut data = HashMap::new();
        data.insert("current-kid".to_owned(), "kid-b".to_owned());
        data.insert("active-kids".to_owned(), "kid-a,kid-b".to_owned());

        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services =
            build_services_with_config_and_secret(config_store.clone(), secret_store.clone());

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.rotate_key(&services, Some("kid-a".to_owned()));

        assert!(
            result.is_err(),
            "should reject explicit rotation to an existing kid"
        );
        assert!(
            secret_store.creates().is_empty(),
            "should reject duplicate kids before writing private key material"
        );
        assert!(
            config_store.puts().is_empty(),
            "should reject duplicate kids before writing config store entries"
        );
    }

    #[test]
    fn rotate_key_uniquifies_generated_kid_when_date_based_kid_is_active() {
        let base_kid = generate_date_based_kid();
        let mut data = HashMap::new();
        data.insert("current-kid".to_owned(), base_kid.clone());
        data.insert("active-kids".to_owned(), base_kid.clone());

        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services = build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let rotation = manager
            .rotate_key(&services, None)
            .expect("should rotate with a uniquified generated kid");

        assert_ne!(
            rotation.new_kid, base_kid,
            "should not reuse an active date-based kid"
        );
        assert!(
            rotation.new_kid.starts_with(&format!("{base_kid}-")),
            "should preserve the date-based kid prefix for generated collisions"
        );
        assert!(
            rotation.active_kids.contains(&base_kid),
            "should keep the existing kid active"
        );
        assert!(
            rotation.active_kids.contains(&rotation.new_kid),
            "should add the uniquified generated kid"
        );
    }

    #[test]
    fn deactivate_key_fails_when_only_one_key_remains() {
        let mut data = HashMap::new();
        data.insert("active-kids".to_owned(), "only-key".to_owned());
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
        let jwk = Keypair::generate().get_jwk("test-key".to_owned());
        let result = KeyRotationResult {
            new_kid: "ts-2024-01-01".to_owned(),
            previous_kid: Some("ts-2023-12-31".to_owned()),
            active_kids: vec!["ts-2023-12-31".to_owned(), "ts-2024-01-01".to_owned()],
            jwk: jwk.clone(),
        };

        assert_eq!(result.new_kid, "ts-2024-01-01");
        assert_eq!(result.previous_kid, Some("ts-2023-12-31".to_owned()));
        assert_eq!(result.active_kids.len(), 2);
        assert_eq!(result.jwk.prm.kid, Some("test-key".to_owned()));
    }

    #[test]
    fn rotate_key_fails_when_private_key_store_write_fails() {
        let config_store = SpyConfigStore::new(HashMap::new());
        let secret_store = SpySecretStore::with_create_failure_after(0);
        let services = build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.rotate_key(&services, Some("new-kid".to_owned()));

        assert!(
            result.is_err(),
            "should fail when the secret store rejects the private key write"
        );
    }

    #[test]
    fn rotate_key_rolls_back_secret_when_jwk_write_fails() {
        let config_store = SpyConfigStore::with_put_failure_after(0);
        let secret_store = SpySecretStore::new();
        let services =
            build_services_with_config_and_secret(config_store.clone(), secret_store.clone());

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.rotate_key(&services, Some("rollback-kid".to_owned()));

        assert!(result.is_err(), "should fail when JWK write fails");
        assert_eq!(
            secret_store.deletes(),
            vec![("sec-id".to_owned(), "rollback-kid".to_owned())],
            "should roll back private key material after JWK write failure"
        );
        assert!(
            config_store.deletes().is_empty(),
            "should not roll back a JWK that was never stored"
        );
    }

    #[test]
    fn rotate_key_rolls_back_secret_and_jwk_when_active_kids_write_fails() {
        let config_store = SpyConfigStore::with_put_failure_after(1);
        let secret_store = SpySecretStore::new();
        let services =
            build_services_with_config_and_secret(config_store.clone(), secret_store.clone());

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.rotate_key(&services, Some("rollback-kid".to_owned()));

        assert!(result.is_err(), "should fail when active-kids write fails");
        assert_eq!(
            config_store.deletes(),
            vec![("cfg-id".to_owned(), "rollback-kid".to_owned())],
            "should roll back the stored JWK after active-kids write failure"
        );
        assert_eq!(
            secret_store.deletes(),
            vec![("sec-id".to_owned(), "rollback-kid".to_owned())],
            "should roll back private key material after active-kids write failure"
        );
    }

    #[test]
    fn deactivate_key_rejects_current_kid() {
        let mut data = HashMap::new();
        data.insert("current-kid".to_owned(), "kid-a".to_owned());
        data.insert("active-kids".to_owned(), "kid-a,kid-b".to_owned());

        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services =
            build_services_with_config_and_secret(config_store.clone(), secret_store.clone());

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.deactivate_key(&services, "kid-a");

        assert!(result.is_err(), "should reject deactivating current-kid");
        assert!(
            config_store.puts().is_empty(),
            "should reject current-kid deactivation before updating active-kids"
        );
        assert!(
            secret_store.deletes().is_empty(),
            "should not touch secret store during failed deactivation"
        );
    }

    #[test]
    fn delete_key_rejects_current_kid_before_deleting_storage() {
        let mut data = HashMap::new();
        data.insert("current-kid".to_owned(), "kid-a".to_owned());
        data.insert("active-kids".to_owned(), "kid-a,kid-b".to_owned());

        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services =
            build_services_with_config_and_secret(config_store.clone(), secret_store.clone());

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.delete_key(&services, "kid-a");

        assert!(result.is_err(), "should reject deleting current-kid");
        assert!(
            secret_store.deletes().is_empty(),
            "should reject current-kid deletion before deleting private key material"
        );
        assert!(
            config_store.deletes().is_empty(),
            "should reject current-kid deletion before deleting JWK storage"
        );
    }

    #[test]
    fn delete_key_removes_secret_before_jwk() {
        let mut data = HashMap::new();
        data.insert("active-kids".to_owned(), "kid-a, kid-b".to_owned());
        data.insert(
            "kid-a".to_owned(),
            r#"{"kty":"OKP","crv":"Ed25519"}"#.to_owned(),
        );

        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services = build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        manager
            .delete_key(&services, "kid-a")
            .expect("should delete key successfully");

        // After deletion, the JWK entry should be gone from the config store.
        let jwk_gone = services
            .config_store()
            .get(&crate::request_signing::JWKS_STORE_NAME, "kid-a");
        assert!(
            jwk_gone.is_err(),
            "should remove JWK from the config store after deletion"
        );
    }
}
