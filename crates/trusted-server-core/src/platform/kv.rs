use bytes::Bytes;
use edgezero_core::key_value_store::{KvError, KvHandle, KvPage, KvStore as PlatformKvStore};

/// A [`PlatformKvStore`] stand-in used when the primary KV store cannot be
/// opened at startup.
///
/// Every method returns [`KvError::Unavailable`], ensuring that handlers
/// which call [`crate::platform::RuntimeServices::kv_handle`] receive a typed
/// error rather than a panic. Routes that do not touch the KV store are
/// unaffected.
///
/// Adapter crates should use this type rather than defining their own stub so
/// the fallback behaviour is consistent across all platform implementations.
pub struct UnavailableKvStore;

#[async_trait::async_trait(?Send)]
impl PlatformKvStore for UnavailableKvStore {
    async fn get_bytes(&self, _key: &str) -> Result<Option<Bytes>, KvError> {
        Err(KvError::Unavailable)
    }

    async fn put_bytes(&self, _key: &str, _value: Bytes) -> Result<(), KvError> {
        Err(KvError::Unavailable)
    }

    async fn put_bytes_with_ttl(
        &self,
        _key: &str,
        _value: Bytes,
        _ttl: std::time::Duration,
    ) -> Result<(), KvError> {
        Err(KvError::Unavailable)
    }

    async fn delete(&self, _key: &str) -> Result<(), KvError> {
        Err(KvError::Unavailable)
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Err(KvError::Unavailable)
    }
}

/// A [`PlatformKvStore`] over an `EdgeZero` [`KvHandle`].
///
/// Adapters receive store handles from `EdgeZero` (or open them directly) as
/// [`KvHandle`]s. This bridge forwards every trait method through the handle,
/// so the handle's key, value and TTL validation runs before the backend is
/// reached. Adapter crates should use it rather than defining their own
/// bridge.
pub struct KvHandleStore(KvHandle);

impl KvHandleStore {
    /// Wrap `handle` as a [`PlatformKvStore`].
    #[must_use]
    pub fn new(handle: KvHandle) -> Self {
        Self(handle)
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformKvStore for KvHandleStore {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        self.0.get_bytes(key).await
    }

    async fn exists(&self, key: &str) -> Result<bool, KvError> {
        self.0.exists(key).await
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.0.put_bytes(key, value).await
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        ttl: std::time::Duration,
    ) -> Result<(), KvError> {
        self.0.put_bytes_with_ttl(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.0.delete(key).await
    }

    async fn list_keys_page(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<KvPage, KvError> {
        self.0.list_keys_page(prefix, cursor, limit).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use futures::executor::block_on;

    use super::*;

    #[derive(Default)]
    struct MemoryStore {
        entries: Mutex<BTreeMap<String, Bytes>>,
        calls: Mutex<usize>,
    }

    impl MemoryStore {
        fn record_call(&self) {
            *self.calls.lock().expect("should lock the call counter") += 1;
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().expect("should lock the call counter")
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformKvStore for MemoryStore {
        async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
            self.record_call();
            Ok(self
                .entries
                .lock()
                .expect("should lock the entries")
                .get(key)
                .cloned())
        }

        async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
            self.record_call();
            self.entries
                .lock()
                .expect("should lock the entries")
                .insert(key.to_owned(), value);
            Ok(())
        }

        async fn put_bytes_with_ttl(
            &self,
            key: &str,
            value: Bytes,
            _ttl: std::time::Duration,
        ) -> Result<(), KvError> {
            self.put_bytes(key, value).await
        }

        async fn delete(&self, key: &str) -> Result<(), KvError> {
            self.record_call();
            self.entries
                .lock()
                .expect("should lock the entries")
                .remove(key);
            Ok(())
        }

        async fn list_keys_page(
            &self,
            prefix: &str,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<KvPage, KvError> {
            self.record_call();
            let keys = self
                .entries
                .lock()
                .expect("should lock the entries")
                .keys()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect();
            Ok(KvPage { cursor: None, keys })
        }
    }

    fn bridge_over(store: &Arc<MemoryStore>) -> KvHandleStore {
        KvHandleStore::new(KvHandle::new(Arc::clone(store) as Arc<dyn PlatformKvStore>))
    }

    #[test]
    fn kv_handle_store_round_trips_through_the_handle() {
        let store = Arc::new(MemoryStore::default());
        let bridge = bridge_over(&store);

        block_on(bridge.put_bytes("example-key", Bytes::from_static(b"example-value")))
            .expect("should write through the bridge");

        assert_eq!(
            block_on(bridge.get_bytes("example-key")).expect("should read through the bridge"),
            Some(Bytes::from_static(b"example-value")),
            "should read back the value written through the bridge"
        );
        assert!(
            block_on(bridge.exists("example-key")).expect("should check existence"),
            "should report the written key as existing"
        );
        assert_eq!(
            block_on(bridge.list_keys_page("example-", None, 10))
                .expect("should list keys")
                .keys,
            vec!["example-key".to_owned()],
            "should list the written key under its prefix"
        );

        block_on(bridge.delete("example-key")).expect("should delete through the bridge");

        assert!(
            !block_on(bridge.exists("example-key")).expect("should check existence"),
            "should report the deleted key as absent"
        );
    }

    #[test]
    fn kv_handle_store_runs_handle_validation_before_the_backend() {
        let store = Arc::new(MemoryStore::default());
        let bridge = bridge_over(&store);

        let read = block_on(bridge.get_bytes(""));
        let write = block_on(bridge.put_bytes("", Bytes::from_static(b"value")));

        assert!(
            matches!(read, Err(KvError::Validation(_))),
            "should reject an empty key on read, got {read:?}"
        );
        assert!(
            matches!(write, Err(KvError::Validation(_))),
            "should reject an empty key on write, got {write:?}"
        );
        assert_eq!(
            store.call_count(),
            0,
            "should reject invalid keys before reaching the backend"
        );
    }
}
