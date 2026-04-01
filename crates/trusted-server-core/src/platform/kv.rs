use bytes::Bytes;
use edgezero_core::key_value_store::{KvError, KvPage, KvStore as PlatformKvStore};

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
