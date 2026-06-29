//! Fastly KV Store implementation of the core [`EcKvStore`] primitives.
//!
//! Maps the platform-neutral identity-graph store operations onto the
//! Fastly KV Store API, including generation markers for compare-and-swap
//! writes (`if_generation_match`).

use error_stack::{Report, ResultExt};
use fastly::kv_store::{InsertMode, KVStore};
use trusted_server_core::ec::kv_backend::{
    EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
};
use trusted_server_core::error::TrustedServerError;

/// Fastly KV Store backend for the EC identity graph.
#[derive(Debug, Clone)]
pub struct FastlyEcKvStore {
    store_name: String,
}

impl FastlyEcKvStore {
    /// Creates a backend for the named Fastly KV store.
    #[must_use]
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
        }
    }

    /// Opens the underlying Fastly KV store.
    fn open_store(&self) -> Result<KVStore, Report<TrustedServerError>> {
        KVStore::open(&self.store_name)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: "Failed to open KV store".to_owned(),
            })?
            .ok_or_else(|| {
                Report::new(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: "KV store not found".to_owned(),
                })
            })
    }
}

impl EcKvStore for FastlyEcKvStore {
    fn store_name(&self) -> &str {
        &self.store_name
    }

    fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let mut response = match store.lookup(key) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to read key '{key}'"),
                    }),
                );
            }
        };

        let generation = response.current_generation();
        let metadata = response.metadata().map(|bytes| bytes.to_vec());
        let body = response.take_body_bytes();

        Ok(Some(EcKvLookup {
            body,
            metadata,
            generation,
        }))
    }

    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let mut builder = store
            .build_insert()
            .metadata(write.metadata)
            .time_to_live(write.ttl);

        builder = match write.mode {
            EcKvWriteMode::Add => builder.mode(InsertMode::Add),
            EcKvWriteMode::Overwrite => builder,
            EcKvWriteMode::IfGenerationMatch(generation) => builder.if_generation_match(generation),
        };

        match builder.execute(key, write.body) {
            Ok(()) => Ok(EcKvWriteOutcome::Written),
            Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                Ok(EcKvWriteOutcome::PreconditionFailed)
            }
            Err(err) => Err(
                Report::new(err).change_context(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: format!("Failed to write entry for key '{key}'"),
                }),
            ),
        }
    }

    fn count_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<u32, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let page = store
            .build_list()
            .prefix(prefix)
            .limit(limit)
            .execute()
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!(
                    "Failed to list keys with prefix '{}'",
                    prefix.get(..8).unwrap_or(prefix),
                ),
            })?;

        #[allow(clippy::cast_possible_truncation)]
        let count = page.keys().len() as u32;
        Ok(count)
    }

    fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        store
            .delete(key)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to delete key '{key}'"),
            })
    }
}
