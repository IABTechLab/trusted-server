//! Select the prepared `EdgeZero` record binding for native EC persistence.

use edgezero_adapter_axum::dev_server::PreparedStores;
use edgezero_core::EdgeError;
use edgezero_core::app::{App, StoreMetadata};
use trusted_server_core::ec::record_backend::{EcRecordLayout, RecordEcKvStore};

pub(crate) const STORE_ID: &str = "trusted_server_kv";

fn selected() -> Result<bool, EdgeError> {
    match trusted_server_core::runtime_env!("TRUSTED_SERVER__EC_DYNAMODB_ENABLED") {
        Err(std::env::VarError::NotPresent) => Ok(false),
        Ok(value) if value == "false" => Ok(false),
        Ok(value) if value == "true" => Ok(true),
        _ => Err(EdgeError::validation("invalid native EC record selection")),
    }
}

pub(crate) fn configure(app: &mut App) -> Result<(), EdgeError> {
    if selected()? {
        if !cfg!(feature = "aws-dynamodb-ec") {
            return Err(EdgeError::validation(
                "native EC records require aws-dynamodb-ec",
            ));
        }
        app.require_record_store(STORE_ID, EcRecordLayout)
            .map_err(|_| EdgeError::validation("invalid native EC record declaration"))?;
    }
    Ok(())
}

pub(crate) fn metadata() -> Option<StoreMetadata> {
    selected()
        .ok()
        .filter(|enabled| *enabled)
        .map(|_| StoreMetadata {
            default: STORE_ID,
            ids: &[STORE_ID],
        })
}

pub(crate) fn acquire(stores: &PreparedStores) -> Result<Option<RecordEcKvStore>, EdgeError> {
    if !selected()? {
        return Ok(None);
    }
    let handle = stores
        .kv()
        .and_then(|registry| registry.named_ref(STORE_ID))
        .ok_or_else(|| EdgeError::validation("native EC record store is not prepared"))?;
    RecordEcKvStore::new(STORE_ID.to_owned(), handle)
        .map(Some)
        .map_err(|_| EdgeError::validation("native EC record store capability or layout mismatch"))
}
