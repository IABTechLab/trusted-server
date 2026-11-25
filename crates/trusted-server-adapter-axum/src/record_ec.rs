//! Declare native EC placement before preparing the selected `EdgeZero` binding.

use edgezero_adapter_axum::dev_server::PreparedStores;
use edgezero_core::EdgeError;
use edgezero_core::app::{App, StoreMetadata};
use edgezero_core::key_value_store::KvHandle;
use trusted_server_core::ec::record_backend::{EcRecordLayout, RecordEcKvStore};

pub(crate) const STORE_ID: &str = "trusted_server_kv";

pub(crate) fn configure(app: &mut App) -> Result<(), EdgeError> {
    app.require_record_store(STORE_ID, EcRecordLayout)
        .map_err(|_| EdgeError::validation("invalid native EC record declaration"))
}

pub(crate) fn metadata() -> StoreMetadata {
    StoreMetadata {
        default: STORE_ID,
        ids: &[STORE_ID],
    }
}

pub(crate) fn acquire(
    stores: &PreparedStores,
    store_id: &str,
) -> Result<RecordEcKvStore, EdgeError> {
    let handle = stores.kv().and_then(|registry| registry.named_ref(store_id))
        .ok_or_else(|| EdgeError::validation("configured EC record store is not prepared; declare it at native boot before provider preparation"))?;
    acquire_handle(store_id, handle)
}

/// Rejects identity storage at startup when the prepared binding cannot meet
/// the EC record contract or layout.
fn acquire_handle(store_id: &str, handle: &KvHandle) -> Result<RecordEcKvStore, EdgeError> {
    RecordEcKvStore::new(store_id.to_owned(), handle).map_err(|report| {
        EdgeError::validation(format!(
            "native EC record store capability or layout mismatch: {}",
            report.current_context()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use edgezero_adapter_axum::key_value_store::PersistentKvStore;

    use super::*;

    #[test]
    fn ordinary_only_store_fails_identity_startup() {
        let directory = tempfile::tempdir().expect("should create a temporary directory");
        let provider = PersistentKvStore::new(directory.path().join("ordinary.redb"))
            .expect("should open an ordinary-only persistent store");
        let handle = KvHandle::new(Arc::new(provider));

        let error = acquire_handle(STORE_ID, &handle)
            .expect_err("should reject a store reporting Unsupported for the record contract");

        let message = error.to_string();
        assert!(
            message.contains("capability or layout mismatch")
                && message.contains("record contract"),
            "should explain that the store cannot meet the record contract, got {message}"
        );
    }
}
