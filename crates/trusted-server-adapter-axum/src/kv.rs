//! Asynchronous forwarding of the prepared native KV handle.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::key_value_store::{KvError, KvHandle, KvPage, KvStore};

/// Logical store used only by the celld-enabled native application.
pub const STATE_STORE_ID: &str = "trusted_server_state";

/// Adapts a prepared handle without extracting its private underlying Arc.
///
/// Both handles retain their cursor encoding layer. The caller must return the
/// outer cursor unchanged, so each layer decodes exactly what it encoded.
pub struct PreparedKvStore(KvHandle);

impl PreparedKvStore {
    /// Retain the provider prepared by the native runner.
    #[must_use]
    pub fn shared(handle: KvHandle) -> Arc<dyn KvStore> {
        Arc::new(Self(handle))
    }
}

#[async_trait::async_trait(?Send)]
impl KvStore for PreparedKvStore {
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
        ttl: Duration,
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
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingStore {
        rows: Mutex<BTreeMap<String, Bytes>>,
        ttl: Mutex<Option<Duration>>,
    }

    #[async_trait::async_trait(?Send)]
    impl KvStore for RecordingStore {
        async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
            if key == "failure" {
                return Err(KvError::Unavailable);
            }
            Ok(self
                .rows
                .lock()
                .expect("should lock rows")
                .get(key)
                .cloned())
        }
        async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
            self.rows
                .lock()
                .expect("should lock rows")
                .insert(key.to_owned(), value);
            Ok(())
        }
        async fn put_bytes_with_ttl(
            &self,
            key: &str,
            value: Bytes,
            ttl: Duration,
        ) -> Result<(), KvError> {
            *self.ttl.lock().expect("should lock TTL") = Some(ttl);
            self.put_bytes(key, value).await
        }
        async fn delete(&self, key: &str) -> Result<(), KvError> {
            self.rows.lock().expect("should lock rows").remove(key);
            Ok(())
        }
        async fn list_keys_page(
            &self,
            prefix: &str,
            cursor: Option<&str>,
            limit: usize,
        ) -> Result<KvPage, KvError> {
            assert_eq!(prefix, "proof/");
            assert_eq!(limit, 2);
            match cursor {
                None => Ok(KvPage {
                    keys: vec![],
                    cursor: Some("opaque/+raw==".to_owned()),
                }),
                Some("opaque/+raw==") => Ok(KvPage {
                    keys: vec!["proof/a".to_owned()],
                    cursor: None,
                }),
                _ => Err(KvError::Unavailable),
            }
        }
    }

    #[tokio::test]
    async fn bridge_preserves_binary_empty_missing_errors_ttl_and_delete() {
        let provider = Arc::new(RecordingStore::default());
        let handle = KvHandle::new(PreparedKvStore::shared(KvHandle::new(provider.clone())));
        assert_eq!(
            handle
                .get_bytes("missing")
                .await
                .expect("should read missing"),
            None
        );
        handle
            .put_bytes("empty", Bytes::new())
            .await
            .expect("should write empty");
        assert_eq!(
            handle
                .get_bytes("empty")
                .await
                .expect("should distinguish empty"),
            Some(Bytes::new())
        );
        assert!(
            handle
                .exists("empty")
                .await
                .expect("should check existence")
        );
        let binary = Bytes::from_static(&[0, 255, 0, 128]);
        let ttl = Duration::from_secs(60);
        handle
            .put_bytes_with_ttl("binary", binary.clone(), ttl)
            .await
            .expect("should forward TTL");
        assert_eq!(
            handle
                .get_bytes("binary")
                .await
                .expect("should read binary"),
            Some(binary)
        );
        assert_eq!(*provider.ttl.lock().expect("should inspect TTL"), Some(ttl));
        assert!(matches!(
            handle.get_bytes("failure").await,
            Err(KvError::Unavailable)
        ));
        handle.delete("binary").await.expect("should delete");
        assert_eq!(
            handle
                .get_bytes("binary")
                .await
                .expect("should read deleted"),
            None
        );
    }

    #[tokio::test]
    async fn bridge_round_trips_both_cursor_layers_and_empty_intermediate_pages() {
        let handle = KvHandle::new(PreparedKvStore::shared(KvHandle::new(Arc::new(
            RecordingStore::default(),
        ))));
        let first = handle
            .list_keys_page("proof/", None, 2)
            .await
            .expect("should list first page");
        assert!(first.keys.is_empty());
        let cursor = first
            .cursor
            .expect("should retain continuation despite empty keys");
        assert_ne!(cursor, "opaque/+raw==");
        let second = handle
            .list_keys_page("proof/", Some(&cursor), 2)
            .await
            .expect("should decode both layers");
        assert_eq!(second.keys, ["proof/a"]);
        assert!(second.cursor.is_none());
        assert!(matches!(
            handle.list_keys_page("other/", Some(&cursor), 2).await,
            Err(KvError::Validation(_))
        ));
    }
}
