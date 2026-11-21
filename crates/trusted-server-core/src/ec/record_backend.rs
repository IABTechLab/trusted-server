//! Shared EC persistence over an already-prepared `EdgeZero` record binding.
//!
//! `EdgeZero` owns provider atomics, generations, TTL and pagination. The identity
//! graph retains entry validation and caller-specific conflict/recovery policy.

use std::collections::BTreeSet;
use std::sync::OnceLock;

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

/// Opens the `EdgeZero` KV binding behind a [`LazyRecordEcKvStore`].
///
/// The adapter supplies it: it receives the EC [`RecordLayout`] and returns a
/// handle over its platform's record-capable provider.
pub type EcRecordOpener = Box<dyn Fn(RecordLayout) -> Result<KvHandle, KvError> + Send + Sync>;

/// An [`EcKvStore`] that opens its record binding on the first EC operation.
///
/// An adapter that holds one store per process or sandbox but must not open
/// the platform binding at startup (Fastly opens it on first use, so a missing
/// or failing store fails only EC work rather than every route) selects the
/// store with this type. The open outcome, success or failure, is cached for
/// the store's lifetime, the way `EdgeZero`'s declared stores cache theirs.
/// Record-contract checks still run on that first open through
/// [`RecordEcKvStore::new`].
pub struct LazyRecordEcKvStore {
    store_name: String,
    open: EcRecordOpener,
    opened: OnceLock<Result<RecordEcKvStore, String>>,
}

impl core::fmt::Debug for LazyRecordEcKvStore {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LazyRecordEcKvStore")
            .field("store_name", &self.store_name)
            .field("opened", &self.opened.get().map(Result::is_ok))
            .finish_non_exhaustive()
    }
}

impl LazyRecordEcKvStore {
    /// Selects the store without opening it or performing I/O.
    #[must_use]
    pub fn new(store_name: String, open: EcRecordOpener) -> Self {
        Self {
            store_name,
            open,
            opened: OnceLock::new(),
        }
    }

    /// Returns whether the first EC operation has already tried to open the store.
    #[must_use]
    pub fn is_opened(&self) -> bool {
        self.opened.get().is_some()
    }

    /// Opens the store on first use and returns the cached outcome.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] naming the store when the
    /// binding failed to open or does not meet the EC record contract.
    pub fn store(&self) -> Result<&RecordEcKvStore, Report<TrustedServerError>> {
        self.opened
            .get_or_init(|| {
                self.open_store().inspect_err(|message| {
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

    fn open_store(&self) -> Result<RecordEcKvStore, String> {
        let layout = RecordLayout::new(EcRecordLayout)
            .map_err(|error| format!("invalid EC record layout: {error}"))?;
        let handle = (self.open)(layout)
            .map_err(|error| format!("failed to open EC record store: {error}"))?;
        RecordEcKvStore::new(self.store_name.clone(), &handle).map_err(|report| {
            match report.current_context() {
                TrustedServerError::KvStore { message, .. } => message.clone(),
                other => other.to_string(),
            }
        })
    }
}

#[async_trait(?Send)]
impl EcKvStore for LazyRecordEcKvStore {
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

fn invalid_layout() -> KvError {
    KvError::Validation("invalid EC record layout".to_owned())
}

fn store_error(store_name: &str, error: &KvError) -> Report<TrustedServerError> {
    let message = match error {
        KvError::MutationUncertain => "EC record mutation completion is uncertain",
        KvError::Unsupported { .. } => {
            "EC record store does not support the EdgeZero record contract; identity storage needs a record-capable store"
        }
        _ => "EC record operation failed",
    };
    Report::new(TrustedServerError::KvStore {
        store_name: store_name.to_owned(),
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use edgezero_core::key_value_store::NoopKvStore;

    use super::*;

    const ROOT_KEY: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";

    fn counting_opener(
        opens: &Arc<AtomicUsize>,
        open: fn() -> Result<KvHandle, KvError>,
    ) -> EcRecordOpener {
        let opens = Arc::clone(opens);
        Box::new(move |_layout| {
            opens.fetch_add(1, Ordering::SeqCst);
            open()
        })
    }

    fn ordinary_only_handle() -> Result<KvHandle, KvError> {
        Ok(KvHandle::new(Arc::new(NoopKvStore)))
    }

    #[test]
    fn ordinary_only_binding_is_rejected_for_the_record_contract() {
        let handle = KvHandle::new(Arc::new(NoopKvStore));

        let error = RecordEcKvStore::new("example_ec_store".to_owned(), &handle)
            .expect_err("should reject a binding without the record contract");

        assert!(
            matches!(
                error.current_context(),
                TrustedServerError::KvStore { store_name, message }
                    if store_name == "example_ec_store" && message.contains("record contract")
            ),
            "should name the store and the unmet record contract, got {error:?}"
        );
    }

    #[test]
    fn lazy_store_opens_on_first_use_and_caches_a_failed_open() {
        let opens = Arc::new(AtomicUsize::new(0));
        let store = LazyRecordEcKvStore::new(
            "example_ec_store".to_owned(),
            counting_opener(&opens, || Err(KvError::Unavailable)),
        );
        assert!(
            !store.is_opened() && opens.load(Ordering::SeqCst) == 0,
            "should not open the store until the first EC operation"
        );

        let first = futures::executor::block_on(store.lookup(ROOT_KEY))
            .expect_err("should fail the EC operation for an unlinked store");
        let second = futures::executor::block_on(store.key_exists(ROOT_KEY))
            .expect_err("should keep failing EC operations for an unlinked store");

        for error in [first, second] {
            assert!(
                matches!(
                    error.current_context(),
                    TrustedServerError::KvStore { store_name, .. } if store_name == "example_ec_store"
                ),
                "should report a KV store error naming the store, got {error:?}"
            );
        }
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "should cache the failed open instead of reopening per operation"
        );
    }

    #[test]
    fn lazy_store_reports_an_ordinary_only_binding_on_first_use() {
        let opens = Arc::new(AtomicUsize::new(0));
        let store = LazyRecordEcKvStore::new(
            "example_ec_store".to_owned(),
            counting_opener(&opens, ordinary_only_handle),
        );

        let error = futures::executor::block_on(store.delete(ROOT_KEY))
            .expect_err("should fail EC work on a binding without the record contract");

        assert!(
            matches!(
                error.current_context(),
                TrustedServerError::KvStore { message, .. } if message.contains("record contract")
            ),
            "should explain the unmet record contract, got {error:?}"
        );
        assert!(store.is_opened(), "should cache the rejected binding");
        assert_eq!(opens.load(Ordering::SeqCst), 1, "should open exactly once");
    }

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
