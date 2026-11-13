//! Native EC persistence through celld's typed conditional-record client.

use async_trait::async_trait;
use edgezero_core::EdgeError;
use edgezero_core::time::MonotonicClock;
use edgezero_store_celld::CelldPreparation;
use edgezero_store_celld::records::{
    CelldRecordStore, RecordWrite, RecordWriteMode, RecordWriteOutcome,
};
use edgezero_store_celld::settings::{CelldAccessToken, CelldKvSettings, CelldPreparationLimits};
use error_stack::Report;
use trusted_server_core::ec::kv_backend::{
    EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
};
use trusted_server_core::error::TrustedServerError;

/// A live provider. Clones share transport, never cached identity rows.
#[derive(Clone, Debug)]
pub struct CelldEcKv {
    records: CelldRecordStore,
}

fn preparation_error() -> EdgeError {
    EdgeError::internal(std::io::Error::other("native EC celld preparation failed"))
}

fn store_error() -> Report<TrustedServerError> {
    Report::new(TrustedServerError::KvStore {
        store_name: "native-ec-celld".to_owned(),
        message: "EC celld operation failed".to_owned(),
    })
}

impl CelldEcKv {
    /// Prepare an explicitly selected provider using one captured bootstrap token.
    ///
    /// # Errors
    /// Invalid settings, credentials, protocol or service failures prevent readiness.
    pub async fn prepare(
        settings: &CelldKvSettings,
        token: Option<&CelldAccessToken>,
    ) -> Result<Self, EdgeError> {
        let mut preparation =
            CelldPreparation::new(MonotonicClock::default(), CelldPreparationLimits::default())
                .map_err(|_| preparation_error())?;
        let records = preparation
            .prepare_records(settings, token)
            .await
            .map_err(|_| preparation_error())?;
        Ok(Self { records })
    }
}

#[async_trait(?Send)]
impl EcKvStore for CelldEcKv {
    fn store_name(&self) -> &str {
        "native-ec-celld"
    }
    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.records
            .lookup(key)
            .await
            .map(|row| {
                row.map(|row| EcKvLookup {
                    body: row.body,
                    metadata: row.metadata,
                    generation: row.generation,
                })
            })
            .map_err(|_| store_error())
    }
    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.records
            .key_exists(key)
            .await
            .map_err(|_| store_error())
    }
    async fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let mode = match write.mode {
            EcKvWriteMode::Add => RecordWriteMode::Add,
            EcKvWriteMode::Overwrite => RecordWriteMode::Overwrite,
            EcKvWriteMode::IfGenerationMatch(generation) => {
                RecordWriteMode::IfGenerationMatch(generation)
            }
        };
        self.records
            .insert(
                key,
                RecordWrite {
                    body: write.body.as_bytes(),
                    metadata: Some(write.metadata.as_bytes()),
                    ttl: write.ttl,
                    mode,
                },
            )
            .await
            .map(|outcome| match outcome {
                RecordWriteOutcome::Written => EcKvWriteOutcome::Written,
                RecordWriteOutcome::PreconditionFailed => EcKvWriteOutcome::PreconditionFailed,
            })
            .map_err(|_| store_error())
    }
    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.records
            .list_keys_with_prefix(prefix, limit)
            .await
            .map_err(|_| store_error())
    }
    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.records.delete(key).await.map_err(|_| store_error())
    }
}
