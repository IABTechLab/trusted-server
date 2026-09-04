//! Platform-neutral KV primitives for the EC identity graph.
//!
//! [`super::kv::KvIdentityGraph`] owns all identity-graph business logic
//! (CAS retry loops, consent tombstone semantics, entry validation) and
//! delegates raw store access to an [`EcKvStore`] implementation provided
//! by the adapter crate (e.g. the Fastly KV Store backend in
//! `trusted-server-adapter-fastly`).
//!
//! This trait is intentionally narrow: lookup with a generation marker,
//! conditional insert, prefix counting, and delete. Conditional writes are
//! expressed through [`EcKvWriteMode`] so compare-and-swap loops stay in
//! core while the platform supplies the actual precondition mechanics.

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

/// Raw KV store primitives backing the EC identity graph.
///
/// Implementations map these operations onto the platform KV API.
/// Infrastructure failures are reported as [`TrustedServerError::KvStore`];
/// write precondition failures are part of the normal control flow and are
/// returned as [`EcKvWriteOutcome::PreconditionFailed`] instead of errors.
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
    fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>>;

    /// Writes an entry according to the requested precondition mode.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or write
    /// failure. Precondition failures are reported through the
    /// [`EcKvWriteOutcome`] instead.
    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>>;

    /// Counts keys sharing the given prefix, up to `limit`.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or list failure.
    fn count_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<u32, Report<TrustedServerError>>;

    /// Hard-deletes a key.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or delete failure.
    fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>>;
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

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

    impl EcKvStore for InMemoryEcKv {
        fn store_name(&self) -> &str {
            &self.name
        }

        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            Ok(entries.get(key).map(|stored| EcKvLookup {
                body: stored.body.clone(),
                metadata: stored.metadata.clone(),
                generation: stored.generation,
            }))
        }

        fn insert(
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

        fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            let count = entries
                .keys()
                .filter(|key| key.starts_with(prefix))
                .take(limit as usize)
                .count();
            #[allow(clippy::cast_possible_truncation)]
            Ok(count as u32)
        }

        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            let mut entries = self.entries.lock().expect("should lock in-memory store");
            entries.remove(key);
            Ok(())
        }
    }

    /// [`EcKvStore`] that fails every operation, mimicking a missing or
    /// unreachable platform store.
    pub(crate) struct FailingEcKv {
        name: String,
    }

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

    impl EcKvStore for FailingEcKv {
        fn store_name(&self) -> &str {
            &self.name
        }

        fn lookup(&self, _key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            Err(self.error("lookup"))
        }

        fn insert(
            &self,
            _key: &str,
            _write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            Err(self.error("insert"))
        }

        fn count_keys_with_prefix(
            &self,
            _prefix: &str,
            _limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            Err(self.error("list"))
        }

        fn delete(&self, _key: &str) -> Result<(), Report<TrustedServerError>> {
            Err(self.error("delete"))
        }
    }
}
