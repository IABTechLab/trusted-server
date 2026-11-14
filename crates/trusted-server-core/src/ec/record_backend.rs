//! Shared EC persistence over an already-prepared `EdgeZero` record binding.
//!
//! `EdgeZero` owns provider atomics, generations, TTL and pagination. The identity
//! graph retains entry validation and caller-specific conflict/recovery policy.

use std::collections::BTreeSet;

use async_trait::async_trait;
use edgezero_core::key_value_store::{KvError, KvHandle};
use edgezero_core::record_store::{
    RecordGeneration, RecordHandle, RecordKeyLayout, RecordLayout, RecordWrite, RecordWriteMode,
    RecordWriteOutcome,
};
use error_stack::Report;

use super::kv::WITHDRAWAL_MARKER_PREFIX;
use super::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use crate::error::TrustedServerError;

/// Stable placement for EC roots and withdrawal-completion markers.
///
/// Roots share their IP-hash prefix, ending in a dot. Completion markers share
/// their marker namespace and EC ID, ending in a colon. These rules preserve
/// key bytes and only establish placement; the graph validates identity contents
/// and marker expiry rather than trusting a storage key's shape.
#[derive(Clone, Copy, Debug)]
pub struct EcRecordLayout;

impl RecordKeyLayout for EcRecordLayout {
    fn id(&self) -> &'static str {
        "trusted-server-ec"
    }

    fn partition_prefix<'key>(&self, key: &'key str) -> Result<&'key str, KvError> {
        let end = if let Some(marker) = key.strip_prefix(WITHDRAWAL_MARKER_PREFIX) {
            let separator = marker
                .find(':')
                .filter(|offset| *offset > 0)
                .ok_or_else(invalid_layout)?;
            WITHDRAWAL_MARKER_PREFIX.len() + separator
        } else {
            key.find('.')
                .filter(|offset| *offset > 0)
                .ok_or_else(invalid_layout)?
        };
        if end + 1 == key.len() {
            return Err(invalid_layout());
        }
        key.get(..=end).ok_or_else(invalid_layout)
    }

    fn validate(&self) -> Result<(), KvError> {
        Ok(())
    }

    fn validate_partition(&self, prefix: &str) -> Result<(), KvError> {
        let valid = if let Some(marker) = prefix.strip_prefix(WITHDRAWAL_MARKER_PREFIX) {
            marker
                .strip_suffix(':')
                .is_some_and(|ec_id| !ec_id.is_empty() && !ec_id.contains(':'))
        } else {
            prefix
                .strip_suffix('.')
                .is_some_and(|hash| !hash.is_empty() && !hash.contains('.'))
        };
        if valid { Ok(()) } else { Err(invalid_layout()) }
    }

    fn version(&self) -> u32 {
        1
    }
}

/// The provider-independent implementation used by the EC graph.
#[derive(Clone, Debug)]
pub struct RecordEcKvStore {
    records: RecordHandle,
    store_name: String,
}

impl RecordEcKvStore {
    /// Acquire an immutable record view without I/O or changing its routing.
    ///
    /// The application must declare [`EcRecordLayout`] during configuration,
    /// before `EdgeZero` prepares this handle.
    ///
    /// # Errors
    /// Returns a sanitized store error for unsupported or mismatched bindings.
    pub fn new(store_name: String, handle: &KvHandle) -> Result<Self, Report<TrustedServerError>> {
        let records = handle
            .require_records()
            .map_err(|error| store_error(&store_name, &error))?;
        let expected =
            RecordLayout::new(EcRecordLayout).map_err(|error| store_error(&store_name, &error))?;
        if records.binding().layout != expected {
            return Err(store_error(&store_name, &invalid_layout()));
        }
        Ok(Self {
            records,
            store_name,
        })
    }
}

#[async_trait(?Send)]
impl EcKvStore for RecordEcKvStore {
    fn store_name(&self) -> &str {
        &self.store_name
    }

    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.records
            .lookup(key)
            .await
            .map(|record| {
                record.map(|value| EcKvLookup {
                    body: value.body.to_vec(),
                    metadata: value.metadata.map(String::into_bytes),
                    generation: value.generation.as_u64(),
                })
            })
            .map_err(|error| store_error(&self.store_name, &error))
    }

    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.records
            .exists_strong(key)
            .await
            .map_err(|error| store_error(&self.store_name, &error))
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
                RecordWriteMode::IfGenerationMatch(RecordGeneration::from_u64(generation))
            }
        };
        self.records
            .insert(
                key,
                RecordWrite {
                    body: write.body.as_bytes(),
                    metadata: Some(write.metadata),
                    mode,
                    ttl: Some(write.ttl),
                },
            )
            .await
            .map(|outcome| match outcome {
                RecordWriteOutcome::Written => EcKvWriteOutcome::Written,
                RecordWriteOutcome::PreconditionFailed => EcKvWriteOutcome::PreconditionFailed,
            })
            .map_err(|error| store_error(&self.store_name, &error))
    }

    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        if limit == 0 {
            return Err(store_error(&self.store_name, &invalid_layout()));
        }
        let partition = self
            .records
            .partition(prefix)
            .map_err(|error| store_error(&self.store_name, &error))?;
        let capacity =
            usize::try_from(limit).map_err(|_| store_error(&self.store_name, &invalid_layout()))?;
        let mut keys = Vec::new();
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        loop {
            let page_limit = (capacity - keys.len()).min(KvHandle::MAX_LIST_PAGE_SIZE);
            let page = self
                .records
                .list_partition(&partition, cursor.as_deref(), page_limit)
                .await
                .map_err(|error| store_error(&self.store_name, &error))?;
            keys.extend(page.keys);
            cursor = page.cursor;
            // This trait intentionally returns a capped count/page. Existing
            // graph callers reject saturated cleanup rather than assume absence.
            if keys.len() == capacity || cursor.is_none() {
                return Ok(keys);
            }
            if let Some(next) = &cursor
                && !seen.insert(next.clone())
            {
                return Err(store_error(&self.store_name, &invalid_layout()));
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.records
            .delete(key)
            .await
            .map_err(|error| store_error(&self.store_name, &error))
    }
}

fn invalid_layout() -> KvError {
    KvError::Validation("invalid EC record layout".to_owned())
}

fn store_error(store_name: &str, error: &KvError) -> Report<TrustedServerError> {
    let message = if matches!(error, KvError::MutationUncertain) {
        "EC record mutation completion is uncertain"
    } else {
        "EC record operation failed"
    };
    Report::new(TrustedServerError::KvStore {
        store_name: store_name.to_owned(),
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_and_markers_have_disjoint_stable_partitions() {
        let layout = RecordLayout::new(EcRecordLayout).expect("should validate layout");
        let root = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";
        let neighbor = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Zy9876";
        let root_group = layout.resolve(root).expect("should resolve root");
        assert_eq!(
            root_group,
            layout.resolve(neighbor).expect("should resolve neighbor")
        );
        let marker = format!("{WITHDRAWAL_MARKER_PREFIX}{root}:123");
        let marker_group = layout.resolve(&marker).expect("should resolve marker");
        assert_ne!(root_group, marker_group);
        assert_eq!(
            marker_group.prefix(),
            format!("{WITHDRAWAL_MARKER_PREFIX}{root}:")
        );
        let other_marker = format!("{WITHDRAWAL_MARKER_PREFIX}{neighbor}:123");
        assert_ne!(
            marker_group,
            layout
                .resolve(&other_marker)
                .expect("should resolve other marker")
        );
    }

    #[test]
    fn layout_does_not_replace_graph_content_validation() {
        let layout = RecordLayout::new(EcRecordLayout).expect("should validate layout");
        let malformed_expiry = format!("{WITHDRAWAL_MARKER_PREFIX}hash.suffix:not-a-timestamp");
        assert!(
            layout.resolve(&malformed_expiry).is_ok(),
            "should leave marker expiry validation to graph"
        );
        for incomplete in ["", "hash", "hash.suffix", WITHDRAWAL_MARKER_PREFIX] {
            assert!(
                layout.partition(incomplete).is_err(),
                "should reject incomplete partition"
            );
        }
        for key in ["hash", "hash.", "", "__ts_ec_withdrawal_complete__:root"] {
            assert!(
                layout.resolve(key).is_err(),
                "should reject unplaceable keys"
            );
        }
    }
}
