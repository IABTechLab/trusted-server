//! Declare native EC placement before preparing the selected `EdgeZero` binding.

use edgezero_adapter_axum::dev_server::PreparedStores;
use edgezero_core::EdgeError;
use edgezero_core::app::{App, StoreMetadata};
use trusted_server_core::ec::record_backend::{EcRecordLayout, RecordEcKvStore};

const STORE_ID: &str = "trusted_server_kv";

fn declared() -> Result<bool, EdgeError> {
    match std::env::var("TS_EC_DYNAMODB_ENABLED") {
        Err(std::env::VarError::NotPresent) => {}
        Ok(value) if value == "false" => {}
        _ => {
            return Err(EdgeError::validation(
                "legacy TS_EC_DYNAMODB_ENABLED is obsolete; declare TS_EC_RECORDS_ENABLED and select an EdgeZero record binding",
            ));
        }
    }
    match std::env::var("TS_EC_RECORDS_ENABLED") {
        Err(std::env::VarError::NotPresent) => Ok(false),
        Ok(value) if value == "false" => Ok(false),
        Ok(value) if value == "true" => Ok(true),
        _ => Err(EdgeError::validation(
            "invalid TS_EC_RECORDS_ENABLED declaration",
        )),
    }
}

pub(crate) fn configure(app: &mut App) -> Result<(), EdgeError> {
    if declared().inspect_err(|error| log::error!("native EC declaration failed: {error}"))? {
        app.require_record_store(STORE_ID, EcRecordLayout)
            .map_err(|_| EdgeError::validation("invalid native EC record declaration"))?;
    }
    Ok(())
}

pub(crate) fn metadata() -> Option<StoreMetadata> {
    declared()
        .ok()
        .filter(|enabled| *enabled)
        .map(|_| StoreMetadata {
            default: STORE_ID,
            ids: &[STORE_ID],
        })
}

pub(crate) fn acquire(
    stores: &PreparedStores,
    store_id: &str,
) -> Result<RecordEcKvStore, EdgeError> {
    let handle = stores.kv().and_then(|registry| registry.named_ref(store_id))
        .ok_or_else(|| EdgeError::validation("configured EC record store is not prepared; declare it at native boot before provider preparation"))?;
    RecordEcKvStore::new(store_id.to_owned(), handle)
        .map_err(|_| EdgeError::validation("native EC record store capability or layout mismatch"))
}
