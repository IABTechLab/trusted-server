//! EC record store that opens its Fastly KV binding on first use.
//!
//! The identity app holds one store per sandbox, but opening the Fastly
//! binding when the sandbox starts would turn a missing or failing store into
//! an assembly error for every route. Opening on the first EC operation keeps
//! the existing timing: only EC work fails, and the outcome is cached for the
//! sandbox the way `EdgeZero`'s declared stores cache theirs.

use std::sync::OnceLock;

use async_trait::async_trait;
use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_core::RecordLayout;
use edgezero_core::key_value_store::KvHandle;
use error_stack::Report;
use trusted_server_core::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteOutcome};
use trusted_server_core::ec::record_backend::{EcRecordLayout, RecordEcKvStore};
use trusted_server_core::error::TrustedServerError;

/// An [`EcKvStore`] over a Fastly record store, opened on first use.
pub(crate) struct LazyEcRecordStore {
    store_name: String,
    opened: OnceLock<Result<RecordEcKvStore, String>>,
}

impl LazyEcRecordStore {
    /// Selects the store without opening it.
    pub(crate) fn new(store_name: String) -> Self {
        Self {
            store_name,
            opened: OnceLock::new(),
        }
    }

    fn store(&self) -> Result<&RecordEcKvStore, Report<TrustedServerError>> {
        self.opened
            .get_or_init(|| {
                open(&self.store_name).inspect_err(|message| {
                    log::warn!(
                        "EC record store '{}' is unavailable: {message}",
                        self.store_name
                    );
                })
            })
            .as_ref()
            .map_err(|message| {
                Report::new(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: message.clone(),
                })
            })
    }
}

fn open(store_name: &str) -> Result<RecordEcKvStore, String> {
    let layout = RecordLayout::new(EcRecordLayout)
        .map_err(|error| format!("invalid EC record layout: {error}"))?;
    let provider = FastlyKvStore::open_records(store_name, layout)
        .map_err(|error| format!("failed to open EC record store: {error}"))?;
    let handle = KvHandle::new(std::sync::Arc::new(provider));
    RecordEcKvStore::new(store_name.to_owned(), &handle).map_err(|error| format!("{error}"))
}

#[async_trait(?Send)]
impl EcKvStore for LazyEcRecordStore {
    fn store_name(&self) -> &str {
        &self.store_name
    }

    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.store()?.lookup(key).await
    }

    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.store()?.key_exists(key).await
    }

    async fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        self.store()?.insert(key, write).await
    }

    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.store()?.list_keys_with_prefix(prefix, limit).await
    }

    async fn count_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<u32, Report<TrustedServerError>> {
        self.store()?.count_keys_with_prefix(prefix, limit).await
    }

    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.store()?.delete(key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_a_store_does_not_open_it() {
        fastly::init();
        let store = LazyEcRecordStore::new("missing_ec_fixture".to_owned());
        assert!(
            store.opened.get().is_none(),
            "should not open the store until the first EC operation"
        );
        let error = futures::executor::block_on(store.lookup("example-key"))
            .expect_err("should fail the EC operation for an unlinked store");
        assert!(
            matches!(
                error.current_context(),
                TrustedServerError::KvStore { store_name, .. } if store_name == "missing_ec_fixture"
            ),
            "should report a KV store error naming the store"
        );
        assert!(
            store.opened.get().is_some_and(Result::is_err),
            "should cache the failed open for the sandbox"
        );
    }

    #[test]
    fn opens_the_linked_store_on_first_use() {
        fastly::init();
        let store = LazyEcRecordStore::new("record_ec_fixture".to_owned());
        let opened = store
            .store()
            .expect("should open the linked record store on first use")
            .clone();
        futures::executor::block_on(
            trusted_server_core::test_support::record_ec::assert_record_ec_lifecycle(&opened),
        );
    }
}
