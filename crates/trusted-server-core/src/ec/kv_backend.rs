//! Platform-neutral KV primitives for the EC identity graph.
//!
//! [`super::kv::KvIdentityGraph`] owns all identity-graph business logic
//! (CAS retry loops, consent tombstone semantics, entry validation) and
//! delegates raw store access to an [`EcKvStore`] implementation provided
//! by the adapter crate (e.g. the Fastly KV Store backend in
//! `trusted-server-adapter-fastly`).
//!
//! This trait is intentionally narrow: lookup with a generation marker,
//! conditional insert, a bounded conditional update, strong prefix listing,
//! and delete. Conditional writes are expressed through [`EcKvWriteMode`] so
//! compare-and-swap loops stay in core while the platform supplies the actual
//! precondition mechanics; [`EcKvStore::update`]'s re-read and re-plan loop
//! is `EdgeZero`'s.

use std::num::NonZeroU8;
use std::time::Duration;

use error_stack::Report;

use crate::error::TrustedServerError;

/// Result of a successful [`EcKvStore::lookup`] for an existing key.
#[derive(Debug, Clone)]
pub struct EcKvLookup {
    /// Raw entry body bytes.
    pub body: Vec<u8>,
    /// Raw metadata bytes, when the platform stored any.
    pub metadata: Option<Vec<u8>>,
    /// Generation marker for subsequent compare-and-swap writes.
    pub generation: u64,
}

/// Write request passed to [`EcKvStore::insert`].
#[derive(Debug, Clone, Copy)]
pub struct EcKvWrite<'a> {
    /// Serialized entry body.
    pub body: &'a str,
    /// Serialized entry metadata.
    pub metadata: &'a str,
    /// Time-to-live for the written entry.
    pub ttl: Duration,
    /// Precondition mode for the write.
    pub mode: EcKvWriteMode,
}

/// Precondition mode for an [`EcKvStore::insert`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcKvWriteMode {
    /// Create the key; fail with [`EcKvWriteOutcome::PreconditionFailed`]
    /// when the key already exists.
    Add,
    /// Unconditionally overwrite any existing value.
    Overwrite,
    /// Write only when the stored generation matches the provided marker.
    IfGenerationMatch(u64),
}

/// Outcome of an [`EcKvStore::insert`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcKvWriteOutcome {
    /// The write was applied.
    Written,
    /// The write precondition failed (key exists for
    /// [`EcKvWriteMode::Add`], or generation mismatch for
    /// [`EcKvWriteMode::IfGenerationMatch`]).
    PreconditionFailed,
}

/// A replacement planned against the row [`EcKvStore::update`] read.
#[derive(Debug, Clone)]
pub struct EcKvReplacement {
    /// Serialized entry body.
    pub body: String,
    /// Serialized entry metadata.
    pub metadata: String,
    /// Time-to-live for the written entry.
    pub ttl: Duration,
}

/// The planner [`EcKvStore::update`] calls with the row as read, or `None`
/// when the lookup missed. It returns the replacement to write, or `None` to
/// leave the row unchanged.
pub type EcKvPlan<'plan> = dyn FnMut(Option<&EcKvLookup>) -> Option<EcKvReplacement> + 'plan;

/// How an [`EcKvStore::update`] ended when no read or write failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcKvUpdateOutcome {
    /// Every attempt lost its precondition, so nothing was written.
    Conflict,
    /// The planner left the row unchanged.
    Skipped,
    /// A re-read after a lost precondition returned the rejected generation
    /// or no row, so nothing was written.
    StaleRead,
    /// One replacement won its precondition and was written.
    Written,
}

/// Why an [`EcKvStore::update`] did not finish.
#[derive(Debug)]
pub enum EcKvUpdateError {
    /// The store has no bounded conditional update; the caller chooses a
    /// fallback.
    Unsupported,
    /// A read or write failed. It was not retried.
    Store(Report<TrustedServerError>),
}

/// The result of an [`EcKvStore::update`] and the conflicts it met.
#[derive(Debug)]
pub struct EcKvUpdate {
    /// Precondition rejections, counted even when the update failed.
    pub conflicts: u8,
    /// The outcome, or why the update did not finish.
    pub result: Result<EcKvUpdateOutcome, EcKvUpdateError>,
}

/// Raw KV store primitives backing the EC identity graph.
///
/// Implementations map these operations onto the platform KV API.
/// Infrastructure failures are reported as [`TrustedServerError::KvStore`];
/// write precondition failures are part of the normal control flow and are
/// returned as [`EcKvWriteOutcome::PreconditionFailed`] instead of errors.
///
/// I/O returns local futures so native backends can yield without blocking the
/// request runtime. Neither stores nor their futures need to be `Send` or `Sync`;
/// a store the shared identity app holds across requests must be both (see
/// [`crate::ec::app::IdentityPlatform`]).
#[async_trait::async_trait(?Send)]
pub trait EcKvStore {
    /// Returns the platform store name, used in log and error messages.
    fn store_name(&self) -> &str;

    /// Reads the body, metadata, and generation marker for a key.
    ///
    /// Returns `Ok(None)` when the key does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or read failure.
    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>>;

    /// Checks exact-key existence against strongly consistent store state.
    ///
    /// A completed issuance must be visible even when [`Self::lookup`] lags.
    /// Prefix matches are insufficient and an inconclusive check is an error.
    /// Logical expiry alone does not prove absence when a previously authorized
    /// conditional write can still commit against the expired row. Such backends
    /// must include physically held expired rows in this check for withdrawal
    /// and orphan protection, even when lookup and listing hide logical expiry.
    /// Returning `false` must establish that an old generation cannot still
    /// update the apparently absent row.
    ///
    /// # Implementing this method
    ///
    /// Strong consistency is a contract this signature cannot express, so a
    /// new backend has to establish it deliberately. An implementation whose
    /// listing or point read is eventually consistent must not answer from it:
    /// doing so reintroduces the bug this check exists to prevent, where a
    /// replication lag reports a recently issued identity as absent and its
    /// withdrawal is silently discarded while the identity stays live for
    /// batch sync. If the platform offers no strongly consistent read, return
    /// an error rather than a `false` the caller will trust.
    ///
    /// The in-memory double used across the core tests is trivially strong, so
    /// those tests cannot catch a backend that breaks this. Cover a new backend
    /// against its own platform.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store failure or when a
    /// bounded check cannot determine existence. Never falls back to an
    /// eventually consistent read.
    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>>;

    /// Writes an entry according to the requested precondition mode.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or write
    /// failure. Precondition failures are reported through the
    /// [`EcKvWriteOutcome`] instead.
    async fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>>;

    /// Reads `key`, plans its replacement with `plan` and writes it
    /// conditionally, re-reading and re-planning after a lost precondition
    /// for at most `attempts` writes.
    ///
    /// Each write is guarded by the generation the attempt read, or creates
    /// the key when the read missed, so a concurrent write is never
    /// overwritten and at most one replacement lands. A re-read after a lost
    /// precondition that returns the rejected generation or no row is stale
    /// and ends the update. Read and write errors are not retried. The loop is
    /// `EdgeZero`'s `RecordHandle::update`.
    ///
    /// The default reports [`EcKvUpdateError::Unsupported`] without I/O, for
    /// a store that has no such operation.
    async fn update(
        &self,
        _key: &str,
        _attempts: NonZeroU8,
        _plan: &mut EcKvPlan<'_>,
    ) -> EcKvUpdate {
        EcKvUpdate {
            conflicts: 0,
            result: Err(EcKvUpdateError::Unsupported),
        }
    }

    /// Lists keys sharing the given prefix from strongly consistent state.
    ///
    /// Returns one bounded page of at most `limit` keys, not an exhaustive scan.
    /// Results may be truncated; a full page does not prove that all matching
    /// keys were returned. Callers must handle truncation conservatively (for
    /// example, reject a saturated cleanup) and validate the complete key shape
    /// before using key contents for a correctness decision.
    ///
    /// Completed writes must be visible even when [`Self::lookup`] lags. If the
    /// backend cannot provide strong listing, it must return an error rather
    /// than substitute eventually consistent results.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or list failure.
    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>>;

    /// Counts keys in one strongly consistent prefix-list page, up to `limit`.
    ///
    /// Delegates to [`Self::list_keys_with_prefix`] and inherits its bounded,
    /// potentially truncated result. This is a capped count, not an exhaustive
    /// total; callers needing completeness must account for truncation.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or list failure.
    async fn count_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<u32, Report<TrustedServerError>> {
        let count = self.list_keys_with_prefix(prefix, limit).await?.len();
        #[allow(clippy::cast_possible_truncation)]
        Ok(count as u32)
    }

    /// Hard-deletes a key.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or delete failure.
    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>>;
}

#[cfg(any(test, feature = "test-utils"))]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    #[cfg(test)]
    use std::sync::Arc;
    use std::sync::Mutex;
    #[cfg(test)]
    use std::sync::atomic::AtomicBool;
    #[cfg(test)]
    use std::sync::atomic::{AtomicU32, Ordering};

    #[cfg(test)]
    use bytes::Bytes;
    #[cfg(test)]
    use edgezero_core::key_value_store::{KvError, KvHandle, KvPage, KvStore};
    #[cfg(test)]
    use edgezero_core::record_store::{
        Record, RecordBinding, RecordGeneration, RecordLayout, RecordPartition, RecordWrite,
        RecordWriteMode, RecordWriteOutcome,
    };

    use super::*;
    #[cfg(test)]
    use crate::ec::kv::KvIdentityGraph;
    #[cfg(test)]
    use crate::ec::kv_types::KvEntry;
    #[cfg(test)]
    use crate::ec::record_backend::{EcRecordLayout, RecordEcKvStore};

    /// An `EdgeZero` record [`KvStore`] over an [`EcKvStore`] test double, so
    /// [`RecordBackedEcKv`] runs `EdgeZero`'s own `RecordHandle::update` loop
    /// against the double's reads and writes.
    #[cfg(test)]
    struct EcKvRecords(Arc<dyn EcKvStore + Send + Sync>);

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl KvStore for EcKvRecords {
        async fn delete(&self, _key: &str) -> Result<(), KvError> {
            Err(KvError::Unavailable)
        }

        async fn get_bytes(&self, _key: &str) -> Result<Option<Bytes>, KvError> {
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

        async fn put_bytes(&self, _key: &str, _value: Bytes) -> Result<(), KvError> {
            Err(KvError::Unavailable)
        }

        async fn put_bytes_with_ttl(
            &self,
            _key: &str,
            _value: Bytes,
            _ttl: Duration,
        ) -> Result<(), KvError> {
            Err(KvError::Unavailable)
        }

        fn record_binding(&self) -> Option<RecordBinding> {
            Some(RecordBinding {
                format: "ec-test".to_owned(),
                layout: RecordLayout::new(EcRecordLayout).expect("should validate the EC layout"),
                max_metadata_bytes: 64 * 1024,
                scope: "test".to_owned(),
            })
        }

        async fn record_lookup(
            &self,
            key: &str,
            _partition: &RecordPartition,
        ) -> Result<Option<Record>, KvError> {
            let lookup = self.0.lookup(key).await.map_err(|_| KvError::Unavailable)?;
            Ok(lookup.map(|lookup| Record {
                body: Bytes::from(lookup.body),
                generation: RecordGeneration::from_u64(lookup.generation),
                metadata: lookup.metadata.map(|metadata| {
                    String::from_utf8(metadata).expect("should hold UTF-8 metadata")
                }),
            }))
        }

        async fn record_insert(
            &self,
            key: &str,
            _partition: &RecordPartition,
            write: RecordWrite<'_>,
        ) -> Result<RecordWriteOutcome, KvError> {
            let mode = match write.mode {
                RecordWriteMode::Add => EcKvWriteMode::Add,
                RecordWriteMode::Overwrite => EcKvWriteMode::Overwrite,
                RecordWriteMode::IfGenerationMatch(generation) => {
                    EcKvWriteMode::IfGenerationMatch(generation.as_u64())
                }
            };
            let outcome = self
                .0
                .insert(
                    key,
                    EcKvWrite {
                        body: std::str::from_utf8(write.body).expect("should write a UTF-8 row"),
                        metadata: write.metadata.unwrap_or_default(),
                        ttl: write.ttl.unwrap_or(Duration::from_secs(60)),
                        mode,
                    },
                )
                .await
                .map_err(|_| KvError::Unavailable)?;
            Ok(match outcome {
                EcKvWriteOutcome::Written => RecordWriteOutcome::Written,
                EcKvWriteOutcome::PreconditionFailed => RecordWriteOutcome::PreconditionFailed,
            })
        }
    }

    /// [`EcKvStore`] wrapper that gives a test double the production
    /// [`EcKvStore::update`]: [`RecordEcKvStore`] over `EdgeZero`'s
    /// `RecordHandle::update`, reading and writing through the double. Every
    /// other operation goes to the double directly.
    #[cfg(test)]
    pub(crate) struct RecordBackedEcKv {
        inner: Arc<dyn EcKvStore + Send + Sync>,
        records: RecordEcKvStore,
    }

    #[cfg(test)]
    impl RecordBackedEcKv {
        pub(crate) fn new(inner: Arc<dyn EcKvStore + Send + Sync>) -> Self {
            let handle = KvHandle::new(Arc::new(EcKvRecords(Arc::clone(&inner))));
            let records = RecordEcKvStore::new(inner.store_name().to_owned(), &handle)
                .expect("should accept the EC record binding");
            Self { inner, records }
        }
    }

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl EcKvStore for RecordBackedEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key).await
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key).await
        }

        async fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            self.inner.insert(key, write).await
        }

        async fn update(
            &self,
            key: &str,
            attempts: NonZeroU8,
            plan: &mut EcKvPlan<'_>,
        ) -> EcKvUpdate {
            self.records.update(key, attempts, plan).await
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit).await
        }

        async fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            self.inner.count_keys_with_prefix(prefix, limit).await
        }

        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key).await
        }
    }

    /// [`EcKvStore`] whose reads succeed but every write fails, simulating a
    /// store that becomes unwritable mid-request.
    #[cfg(test)]
    pub(crate) struct WriteFailingEcKv {
        inner: InMemoryEcKv,
        inserts: AtomicU32,
    }

    #[cfg(test)]
    impl WriteFailingEcKv {
        pub(crate) fn new() -> Self {
            Self {
                inner: InMemoryEcKv::new("write-failing-store"),
                inserts: AtomicU32::new(0),
            }
        }

        /// The number of writes attempted, every one of which failed.
        pub(crate) fn inserts(&self) -> u32 {
            self.inserts.load(Ordering::Relaxed)
        }

        /// Stores a row directly, bypassing the failing write path.
        pub(crate) async fn seed(&self, key: &str, body: &str, metadata: &str) {
            self.inner
                .insert(
                    key,
                    EcKvWrite {
                        body,
                        metadata,
                        ttl: Duration::from_secs(60),
                        mode: EcKvWriteMode::Overwrite,
                    },
                )
                .await
                .expect("should seed the in-memory store");
        }
    }

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl EcKvStore for WriteFailingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key).await
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key).await
        }

        async fn insert(
            &self,
            _key: &str,
            _write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            self.inserts.fetch_add(1, Ordering::Relaxed);
            Err(Report::new(TrustedServerError::KvStore {
                store_name: self.inner.store_name().to_owned(),
                message: "write failing test store".to_owned(),
            }))
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit).await
        }

        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key).await
        }
    }

    /// A change another writer makes to a stored row.
    #[cfg(test)]
    type Race = Box<dyn Fn(&mut KvEntry) + Send + Sync>;

    /// [`EcKvStore`] wrapper that injects generation conflicts: the first
    /// `conflicts` `IfGenerationMatch` inserts return
    /// [`EcKvWriteOutcome::PreconditionFailed`] without writing.
    ///
    /// With a race, another writer changes the stored row with it just before
    /// each of those inserts, written through
    /// [`KvIdentityGraph::serialize_entry_for_test`] so its metadata is real, and the
    /// row's generation moves on. Without one, the row and its generation stay
    /// as they were, as when the conflicting read was stale.
    #[cfg(test)]
    pub(crate) struct ConflictInjectingEcKv {
        inner: InMemoryEcKv,
        conflicts_remaining: AtomicU32,
        race: Option<Race>,
        fail_reads_after_conflict: bool,
        miss_reads_after_conflict: bool,
        conflicted: AtomicBool,
    }

    #[cfg(test)]
    impl ConflictInjectingEcKv {
        /// A store whose first `conflicts` conditional writes conflict without
        /// the row changing.
        pub(crate) fn new(conflicts: u32) -> Self {
            Self {
                inner: InMemoryEcKv::new("conflict-store"),
                conflicts_remaining: AtomicU32::new(conflicts),
                race: None,
                fail_reads_after_conflict: false,
                miss_reads_after_conflict: false,
                conflicted: AtomicBool::new(false),
            }
        }

        /// A store where `race` changes the row just before each of the first
        /// `conflicts` conditional writes, so those writes conflict.
        pub(crate) fn racing(
            conflicts: u32,
            race: impl Fn(&mut KvEntry) + Send + Sync + 'static,
        ) -> Self {
            Self {
                race: Some(Box::new(race)),
                ..Self::new(conflicts)
            }
        }

        /// Makes every read fail once a conflict has been injected.
        pub(crate) fn failing_reads_after_conflict(self) -> Self {
            Self {
                fail_reads_after_conflict: true,
                ..self
            }
        }

        /// Makes every read find no row once a conflict has been injected, as
        /// a stale replica of an eventually consistent store can.
        pub(crate) fn missing_reads_after_conflict(self) -> Self {
            Self {
                miss_reads_after_conflict: true,
                ..self
            }
        }

        /// The conflicts still to inject.
        pub(crate) fn conflicts_remaining(&self) -> u32 {
            self.conflicts_remaining.load(Ordering::Relaxed)
        }

        /// Stores `entry` under `key` directly, bypassing conflict injection.
        pub(crate) async fn seed(&self, key: &str, entry: &KvEntry) {
            let (body, metadata) =
                KvIdentityGraph::serialize_entry_for_test(entry, self.store_name())
                    .expect("should serialize the seeded row");
            self.inner
                .insert(
                    key,
                    EcKvWrite {
                        body: &body,
                        metadata: &metadata,
                        ttl: Duration::from_secs(60),
                        mode: EcKvWriteMode::Add,
                    },
                )
                .await
                .expect("should seed the in-memory store");
        }

        /// Takes one conflict to inject, if any remain.
        fn take_conflict(&self) -> bool {
            self.conflicts_remaining
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                    remaining.checked_sub(1)
                })
                .inspect(|_| self.conflicted.store(true, Ordering::Relaxed))
                .is_ok()
        }

        /// Applies the race to the stored row of `key`.
        async fn race_row(&self, race: &Race, key: &str, ttl: Duration) {
            let stored = self
                .inner
                .lookup(key)
                .await
                .expect("should read the row being raced")
                .expect("should hold the row being raced");
            let mut entry: KvEntry =
                serde_json::from_slice(&stored.body).expect("should parse the stored row");
            race(&mut entry);
            let (body, metadata) =
                KvIdentityGraph::serialize_entry_for_test(&entry, self.store_name())
                    .expect("should serialize the raced row");
            self.inner
                .insert(
                    key,
                    EcKvWrite {
                        body: &body,
                        metadata: &metadata,
                        ttl,
                        mode: EcKvWriteMode::Overwrite,
                    },
                )
                .await
                .expect("should apply the concurrent write");
        }
    }

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl EcKvStore for ConflictInjectingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            if self.fail_reads_after_conflict && self.conflicted.load(Ordering::Relaxed) {
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.inner.store_name().to_owned(),
                    message: "read failing after a conflict".to_owned(),
                }));
            }
            if self.miss_reads_after_conflict && self.conflicted.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.inner.lookup(key).await
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key).await
        }

        async fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            if matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_)) && self.take_conflict() {
                if let Some(race) = &self.race {
                    self.race_row(race, key, write.ttl).await;
                }
                return Ok(EcKvWriteOutcome::PreconditionFailed);
            }
            self.inner.insert(key, write).await
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit).await
        }

        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key).await
        }
    }

    /// [`EcKvStore`] wrapper that counts `lookup` calls through a shared counter
    /// so tests can prove exactly how many reads a flow performs.
    #[cfg(test)]
    pub(crate) struct CountingEcKv {
        inner: InMemoryEcKv,
        lookups: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[cfg(test)]
    impl CountingEcKv {
        pub(crate) fn new(
            name: impl Into<String>,
            lookups: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ) -> Self {
            Self {
                inner: InMemoryEcKv::new(name),
                lookups,
            }
        }
    }

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl EcKvStore for CountingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.lookups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.lookup(key).await
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key).await
        }

        async fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            self.inner.insert(key, write).await
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit).await
        }

        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key).await
        }
    }

    /// [`EcKvStore`] wrapper that counts conditional write attempts, optionally conflicting once.
    #[cfg(test)]
    pub(crate) struct ConditionalWriteCountingEcKv {
        inner: InMemoryEcKv,
        conflict_first: bool,
        writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[cfg(test)]
    impl ConditionalWriteCountingEcKv {
        pub(crate) fn new(
            writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            conflict_first: bool,
        ) -> Self {
            Self {
                inner: InMemoryEcKv::new("conditional-write-count"),
                conflict_first,
                writes,
            }
        }
    }

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl EcKvStore for ConditionalWriteCountingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key).await
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key).await
        }

        async fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            if matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_)) {
                let attempt = self
                    .writes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if self.conflict_first && attempt == 0 {
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
            }
            self.inner.insert(key, write).await
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit).await
        }

        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key).await
        }
    }

    /// [`EcKvStore`] wrapper that models an eventually-consistent point read.
    ///
    /// The first `stale_lookups` calls to [`EcKvStore::lookup`] report the key
    /// absent while [`EcKvStore::list_keys_with_prefix`] — the list API, which
    /// reads the primary data source — still sees it. Writes reach the inner
    /// store, so a test can assert what actually persisted.
    ///
    /// With `list_fails` set, the list API errors instead, modelling a store
    /// that can neither find the key nor prove it absent.
    #[cfg(test)]
    pub(crate) struct StaleLookupEcKv {
        inner: InMemoryEcKv,
        stale_lookups_remaining: Mutex<u32>,
        list_fails: bool,
    }

    #[cfg(test)]
    impl StaleLookupEcKv {
        pub(crate) fn new(name: impl Into<String>, stale_lookups: u32, list_fails: bool) -> Self {
            Self {
                inner: InMemoryEcKv::new(name),
                stale_lookups_remaining: Mutex::new(stale_lookups),
                list_fails,
            }
        }
    }

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl EcKvStore for StaleLookupEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            {
                let mut remaining = self
                    .stale_lookups_remaining
                    .lock()
                    .expect("should lock stale-lookup counter");
                if *remaining > 0 {
                    *remaining -= 1;
                    return Ok(None);
                }
            }
            self.inner.lookup(key).await
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            if self.list_fails {
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.inner.store_name().to_owned(),
                    message: "existence check unavailable".to_owned(),
                }));
            }
            self.inner.key_exists(key).await
        }

        async fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            self.inner.insert(key, write).await
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            if self.list_fails {
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.inner.store_name().to_owned(),
                    message: "list unavailable".to_owned(),
                }));
            }
            self.inner.list_keys_with_prefix(prefix, limit).await
        }

        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key).await
        }
    }

    /// In-memory [`EcKvStore`] with generation tracking for CAS tests.
    pub(crate) struct InMemoryEcKv {
        name: String,
        entries: Mutex<BTreeMap<String, StoredEntry>>,
    }

    struct StoredEntry {
        body: Vec<u8>,
        metadata: Option<Vec<u8>>,
        generation: u64,
    }

    impl InMemoryEcKv {
        pub(crate) fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                entries: Mutex::new(BTreeMap::new()),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl EcKvStore for InMemoryEcKv {
        fn store_name(&self) -> &str {
            &self.name
        }

        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            Ok(entries.get(key).map(|stored| EcKvLookup {
                body: stored.body.clone(),
                metadata: stored.metadata.clone(),
                generation: stored.generation,
            }))
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            Ok(entries.contains_key(key))
        }

        async fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            let mut entries = self.entries.lock().expect("should lock in-memory store");
            let existing_generation = entries.get(key).map(|stored| stored.generation);

            match write.mode {
                EcKvWriteMode::Add if existing_generation.is_some() => {
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
                EcKvWriteMode::IfGenerationMatch(expected)
                    if existing_generation != Some(expected) =>
                {
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
                EcKvWriteMode::Add
                | EcKvWriteMode::Overwrite
                | EcKvWriteMode::IfGenerationMatch(_) => {}
            }

            entries.insert(
                key.to_owned(),
                StoredEntry {
                    body: write.body.as_bytes().to_vec(),
                    metadata: Some(write.metadata.as_bytes().to_vec()),
                    generation: existing_generation.unwrap_or(0) + 1,
                },
            );
            Ok(EcKvWriteOutcome::Written)
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            Ok(entries
                .keys()
                .filter(|key| key.starts_with(prefix))
                .take(limit as usize)
                .cloned()
                .collect())
        }

        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            let mut entries = self.entries.lock().expect("should lock in-memory store");
            entries.remove(key);
            Ok(())
        }
    }

    /// [`EcKvStore`] that fails every operation, mimicking a missing or
    /// unreachable platform store.
    #[cfg(test)]
    pub(crate) struct FailingEcKv {
        name: String,
    }

    #[cfg(test)]
    impl FailingEcKv {
        pub(crate) fn new(name: impl Into<String>) -> Self {
            Self { name: name.into() }
        }

        fn error(&self, operation: &str) -> Report<TrustedServerError> {
            Report::new(TrustedServerError::KvStore {
                store_name: self.name.clone(),
                message: format!("KV store not found (failing test store, {operation})"),
            })
        }
    }

    #[cfg(test)]
    #[async_trait::async_trait(?Send)]
    impl EcKvStore for FailingEcKv {
        fn store_name(&self) -> &str {
            &self.name
        }

        async fn lookup(
            &self,
            _key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            Err(self.error("lookup"))
        }

        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            let _ = key;
            Err(self.error("key_exists"))
        }

        async fn insert(
            &self,
            _key: &str,
            _write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            Err(self.error("insert"))
        }

        async fn list_keys_with_prefix(
            &self,
            _prefix: &str,
            _limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            Err(self.error("list"))
        }

        async fn delete(&self, _key: &str) -> Result<(), Report<TrustedServerError>> {
            Err(self.error("delete"))
        }
    }
}
