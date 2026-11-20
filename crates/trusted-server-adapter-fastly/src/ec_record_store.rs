//! Opener for the Fastly binding behind core's lazily opened EC record store.
//!
//! The identity app holds one store per sandbox, but opening the Fastly
//! binding when the sandbox starts would turn a missing or failing store into
//! an assembly error for every route. Core's [`LazyRecordEcKvStore`] calls
//! [`open_ec_records`] on the first EC operation instead, so only EC work
//! fails, and caches the outcome for the sandbox.
//!
//! [`LazyRecordEcKvStore`]: trusted_server_core::ec::record_backend::LazyRecordEcKvStore

use std::sync::Arc;

use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_core::RecordLayout;
use edgezero_core::key_value_store::{KvError, KvHandle};

/// Opens the named Fastly KV store as a record-capable `EdgeZero` binding.
///
/// # Errors
///
/// Returns [`KvError::OpenFailed`] when the Fastly binding cannot be opened.
pub(crate) fn open_ec_records(store_name: &str, layout: RecordLayout) -> Result<KvHandle, KvError> {
    let provider = FastlyKvStore::open_records(store_name, layout)?;
    Ok(KvHandle::new(Arc::new(provider)))
}

#[cfg(test)]
mod tests {
    use trusted_server_core::ec::kv_backend::EcKvStore as _;
    use trusted_server_core::ec::record_backend::LazyRecordEcKvStore;
    use trusted_server_core::error::TrustedServerError;

    use super::*;

    fn lazy_store(store_name: &str) -> LazyRecordEcKvStore {
        let name = store_name.to_owned();
        LazyRecordEcKvStore::new(
            store_name.to_owned(),
            Box::new(move |layout| open_ec_records(&name, layout)),
        )
    }

    #[test]
    fn selecting_a_store_does_not_open_it() {
        fastly::init();
        let store = lazy_store("missing_ec_fixture");
        assert!(
            !store.is_opened(),
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
            store.is_opened() && store.store().is_err(),
            "should cache the failed open for the sandbox"
        );
    }

    #[test]
    fn opens_the_linked_store_on_first_use() {
        fastly::init();
        let store = lazy_store("record_ec_fixture");

        let opened = store
            .store()
            .expect("should open the linked record store on first use")
            .clone();

        futures::executor::block_on(
            trusted_server_core::test_support::record_ec::assert_record_ec_lifecycle(&opened),
        );
    }
}
