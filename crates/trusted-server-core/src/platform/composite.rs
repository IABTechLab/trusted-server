//! Registry-backed composite config/secret stores (decision D6-a).
//!
//! Reads resolve through the per-request EdgeZero
//! [`ConfigRegistry`](edgezero_core::store_registry::ConfigRegistry) /
//! [`SecretRegistry`](edgezero_core::store_registry::SecretRegistry) by logical
//! store id; writes delegate to a management-path
//! [`PlatformConfigWriter`](super::PlatformConfigWriter) /
//! [`PlatformSecretWriter`](super::PlatformSecretWriter).

use std::sync::Arc;

use edgezero_core::store_registry::{ConfigRegistry, SecretRegistry};
use error_stack::Report;
use futures::executor::block_on;

use super::{
    PlatformConfigStore, PlatformConfigWriter, PlatformError, PlatformSecretStore,
    PlatformSecretWriter, StoreId, StoreName,
};

/// Config store whose reads resolve through an EdgeZero [`ConfigRegistry`] and
/// whose writes delegate to a management-path [`PlatformConfigWriter`].
///
/// Reads resolve `store_name` as a **logical store id** via
/// [`ConfigRegistry::named`]; both an absent registry and an unknown id are
/// hard errors, never a silent fallback to the default store.
pub struct CompositeConfigStore {
    reader: Option<ConfigRegistry>,
    writer: Arc<dyn PlatformConfigWriter>,
}

impl CompositeConfigStore {
    /// Create a composite config store from an optional read registry and a
    /// write delegate.
    ///
    /// The reader is `Option` because an empty [`ConfigRegistry`] cannot be
    /// constructed (`from_parts` returns `None`), so an absent registry is
    /// represented as `None`.
    #[must_use]
    pub fn new(reader: Option<ConfigRegistry>, writer: Arc<dyn PlatformConfigWriter>) -> Self {
        Self { reader, writer }
    }
}

impl PlatformConfigStore for CompositeConfigStore {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let registry = self
            .reader
            .as_ref()
            .ok_or_else(|| Report::new(PlatformError::ConfigStore))?;
        let binding = registry
            .named(store_name.as_ref())
            .ok_or_else(|| Report::new(PlatformError::ConfigStore))?;
        match block_on(binding.handle.get(key)) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(Report::new(PlatformError::ConfigStore)
                .attach(format!("config key `{key}` not found"))),
            Err(error) => Err(Report::new(PlatformError::ConfigStore)
                .attach(format!("config store read failed: {error}"))),
        }
    }

    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>> {
        self.writer.put(store_id, key, value)
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        self.writer.delete(store_id, key)
    }
}

/// Secret store whose reads resolve through an EdgeZero [`SecretRegistry`] and
/// whose writes delegate to a management-path [`PlatformSecretWriter`].
///
/// Reads resolve `store_name` as a **logical store id** via
/// [`SecretRegistry::named`]; both an absent registry and an unknown id are
/// hard errors, never a silent fallback to the default store.
pub struct CompositeSecretStore {
    reader: Option<SecretRegistry>,
    writer: Arc<dyn PlatformSecretWriter>,
}

impl CompositeSecretStore {
    /// Create a composite secret store from an optional read registry and a
    /// write delegate.
    ///
    /// The reader is `Option` because an empty [`SecretRegistry`] cannot be
    /// constructed (`from_parts` returns `None`), so an absent registry is
    /// represented as `None`.
    #[must_use]
    pub fn new(reader: Option<SecretRegistry>, writer: Arc<dyn PlatformSecretWriter>) -> Self {
        Self { reader, writer }
    }
}

impl PlatformSecretStore for CompositeSecretStore {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let registry = self
            .reader
            .as_ref()
            .ok_or_else(|| Report::new(PlatformError::SecretStore))?;
        let bound = registry
            .named(store_name.as_ref())
            .ok_or_else(|| Report::new(PlatformError::SecretStore))?;
        match block_on(bound.get_bytes(key)) {
            Ok(Some(bytes)) => Ok(bytes.to_vec()),
            Ok(None) => Err(Report::new(PlatformError::SecretStore)
                .attach(format!("secret key `{key}` not found"))),
            Err(error) => Err(Report::new(PlatformError::SecretStore)
                .attach(format!("secret store read failed: {error}"))),
        }
    }

    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        self.writer.create(store_id, name, value)
    }

    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        self.writer.delete(store_id, name)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use bytes::Bytes;
    use edgezero_core::config_store::{ConfigStore, ConfigStoreError, ConfigStoreHandle};
    use edgezero_core::secret_store::{SecretError, SecretHandle, SecretStore};
    use edgezero_core::store_registry::{
        BoundSecretStore, ConfigRegistry, ConfigStoreBinding, SecretRegistry, StoreRegistry,
    };
    use error_stack::Report;

    use super::super::{
        CompositeConfigStore, CompositeSecretStore, PlatformConfigStore, PlatformConfigWriter,
        PlatformError, PlatformSecretStore, PlatformSecretWriter, StoreId, StoreName,
    };

    /// In-memory [`ConfigStore`] double keyed by lookup key.
    struct TestConfigStore {
        data: HashMap<String, String>,
    }

    #[async_trait(?Send)]
    impl ConfigStore for TestConfigStore {
        async fn get(&self, key: &str) -> Result<Option<String>, ConfigStoreError> {
            Ok(self.data.get(key).cloned())
        }
    }

    /// In-memory [`SecretStore`] double keyed by `"{store_name}/{key}"`.
    struct TestSecretStore {
        data: HashMap<String, Bytes>,
    }

    #[async_trait(?Send)]
    impl SecretStore for TestSecretStore {
        async fn get_bytes(
            &self,
            store_name: &str,
            key: &str,
        ) -> Result<Option<Bytes>, SecretError> {
            Ok(self.data.get(&format!("{store_name}/{key}")).cloned())
        }
    }

    /// Build a [`ConfigRegistry`] from `(store_id, key, value)` entries.
    fn config_registry(entries: &[(&str, &str, &str)], default: &str) -> ConfigRegistry {
        let mut by_store: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();
        for (id, key, value) in entries {
            by_store
                .entry((*id).to_owned())
                .or_default()
                .insert((*key).to_owned(), (*value).to_owned());
        }
        let by_id: BTreeMap<String, ConfigStoreBinding> = by_store
            .into_iter()
            .map(|(id, data)| {
                let binding = ConfigStoreBinding {
                    default_key: id.clone(),
                    handle: ConfigStoreHandle::new(Arc::new(TestConfigStore { data })),
                };
                (id, binding)
            })
            .collect();
        StoreRegistry::from_parts(by_id, default.to_owned())
            .expect("should build a non-empty config registry with a present default")
    }

    /// Build a [`SecretRegistry`] from `(store_id, key, value)` entries.
    fn secret_registry(entries: &[(&str, &str, &[u8])], default: &str) -> SecretRegistry {
        let mut data: HashMap<String, Bytes> = HashMap::new();
        let mut ids: BTreeSet<String> = BTreeSet::new();
        for (id, key, value) in entries {
            data.insert(format!("{id}/{key}"), Bytes::copy_from_slice(value));
            ids.insert((*id).to_owned());
        }
        let handle = SecretHandle::new(Arc::new(TestSecretStore { data }));
        let by_id: BTreeMap<String, BoundSecretStore> = ids
            .into_iter()
            .map(|id| {
                let bound = BoundSecretStore::new(handle.clone(), id.clone());
                (id, bound)
            })
            .collect();
        StoreRegistry::from_parts(by_id, default.to_owned())
            .expect("should build a non-empty secret registry with a present default")
    }

    /// Records config write delegations so tests assert the target `StoreId` is
    /// preserved.
    #[derive(Default)]
    struct RecordingConfigWriter {
        puts: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
    }

    impl PlatformConfigWriter for RecordingConfigWriter {
        fn put(
            &self,
            store_id: &StoreId,
            key: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.puts.lock().expect("should acquire writer lock").push((
                store_id.as_ref().to_owned(),
                key.to_owned(),
                value.to_owned(),
            ));
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
            self.deletes
                .lock()
                .expect("should acquire writer lock")
                .push((store_id.as_ref().to_owned(), key.to_owned()));
            Ok(())
        }
    }

    /// Records secret write delegations so tests assert the target `StoreId` is
    /// preserved.
    #[derive(Default)]
    struct RecordingSecretWriter {
        creates: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
    }

    impl PlatformSecretWriter for RecordingSecretWriter {
        fn create(
            &self,
            store_id: &StoreId,
            name: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.creates
                .lock()
                .expect("should acquire writer lock")
                .push((
                    store_id.as_ref().to_owned(),
                    name.to_owned(),
                    value.to_owned(),
                ));
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
            self.deletes
                .lock()
                .expect("should acquire writer lock")
                .push((store_id.as_ref().to_owned(), name.to_owned()));
            Ok(())
        }
    }

    #[test]
    fn composite_config_reads_named_store_and_writes_delegate() {
        // Arrange: a ConfigRegistry with the default `trusted_server_config`
        // plus a non-default `jwks_store` (D5: config default id is
        // `trusted_server_config`, not `app_config`).
        let reader = config_registry(
            &[
                ("trusted_server_config", "current-kid", "kid-1"),
                ("jwks_store", "kid-1", "{\"kty\":\"OKP\"}"),
            ],
            "trusted_server_config",
        );
        let writer = Arc::new(RecordingConfigWriter::default());
        let composite = CompositeConfigStore::new(Some(reader), writer.clone());

        // Act + Assert: the non-default store resolves.
        let jwk = composite
            .get(&StoreName::from("jwks_store"), "kid-1")
            .expect("should read from the non-default jwks_store");
        assert_eq!(
            jwk, "{\"kty\":\"OKP\"}",
            "should resolve the non-default config store by logical id"
        );

        // Unknown store id is a strict error, not a fallback to default.
        let err = composite
            .get(&StoreName::from("nope"), "kid-1")
            .expect_err("should error on unknown store id");
        assert!(
            matches!(err.current_context(), PlatformError::ConfigStore),
            "unknown config id should map to a ConfigStore error"
        );

        // Write delegates to the management-path writer, preserving the StoreId.
        composite
            .put(&StoreId::from("jwks_store"), "current-kid", "kid-2")
            .expect("should delegate write");
        assert_eq!(
            writer
                .puts
                .lock()
                .expect("should acquire writer lock")
                .as_slice(),
            &[(
                "jwks_store".to_owned(),
                "current-kid".to_owned(),
                "kid-2".to_owned()
            )],
            "write must delegate to the writer with the same StoreId, key, and value"
        );
    }

    #[test]
    fn composite_config_absent_registry_errors() {
        // A None reader is a hard error, never a silent fallback.
        let writer = Arc::new(RecordingConfigWriter::default());
        let composite = CompositeConfigStore::new(None, writer);
        let err = composite
            .get(&StoreName::from("trusted_server_config"), "current-kid")
            .expect_err("should error when no registry is wired");
        assert!(
            matches!(err.current_context(), PlatformError::ConfigStore),
            "absent config registry should map to a ConfigStore error"
        );
    }

    #[test]
    fn composite_secret_reads_named_store_and_writes_delegate() {
        // Arrange: a SecretRegistry with default + a non-default `ts_secrets` id.
        let reader = secret_registry(
            &[
                (
                    "trusted_server_secrets",
                    "API_KEY",
                    b"default-key".as_slice(),
                ),
                ("ts_secrets", "server-side-key", b"dd-secret".as_slice()),
            ],
            "trusted_server_secrets",
        );
        let writer = Arc::new(RecordingSecretWriter::default());
        let composite = CompositeSecretStore::new(Some(reader), writer.clone());

        // The non-default store resolves.
        let value = composite
            .get_bytes(&StoreName::from("ts_secrets"), "server-side-key")
            .expect("should read from the non-default ts_secrets store");
        assert_eq!(
            value, b"dd-secret",
            "should resolve the non-default secret store by logical id"
        );

        // Unknown store id is a strict error.
        let err = composite
            .get_bytes(&StoreName::from("nope"), "x")
            .expect_err("should error on unknown secret store");
        assert!(
            matches!(err.current_context(), PlatformError::SecretStore),
            "unknown secret id should map to a SecretStore error"
        );

        // create/delete delegate with the target StoreId preserved.
        composite
            .create(&StoreId::from("ts_secrets"), "new", "val")
            .expect("should delegate create");
        assert_eq!(
            writer
                .creates
                .lock()
                .expect("should acquire writer lock")
                .as_slice(),
            &[("ts_secrets".to_owned(), "new".to_owned(), "val".to_owned())],
            "create must delegate to the writer with the same StoreId"
        );
    }

    #[test]
    fn composite_secret_absent_registry_errors() {
        // A None reader is a hard error, never a silent fallback.
        let writer = Arc::new(RecordingSecretWriter::default());
        let composite = CompositeSecretStore::new(None, writer);
        let err = composite
            .get_bytes(&StoreName::from("trusted_server_secrets"), "API_KEY")
            .expect_err("should error when no registry is wired");
        assert!(
            matches!(err.current_context(), PlatformError::SecretStore),
            "absent secret registry should map to a SecretStore error"
        );
    }
}
