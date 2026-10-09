//! Latency-only timing decorator for Edge Cookie KV store handles.
//!
//! [`TimedKvStore`] wraps an [`EcKvStore`] plus a [`RequestTimings`] handle and
//! records [`Phase::EcKv`] around every store call at
//! [`KvIdentityGraph`](crate::ec::kv::KvIdentityGraph) construction sites.
//! The decorator measures latency only: it never reads, parses, or logs values.

use error_stack::Report;

use crate::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteOutcome};
use crate::error::TrustedServerError;
use crate::request_timing::{Phase, RequestTimings};

/// Wraps `inner` plus a [`RequestTimings`] handle, recording [`Phase::EcKv`]
/// around every store call made through it.
pub struct TimedKvStore<S> {
    /// The wrapped store handle.
    inner: S,
    /// The request's phase-timing collector.
    timings: RequestTimings,
}

impl<S> TimedKvStore<S> {
    /// Creates a decorator around `inner` that records into `timings`.
    #[must_use]
    pub fn new(inner: S, timings: RequestTimings) -> Self {
        Self { inner, timings }
    }
}

impl<S: EcKvStore> EcKvStore for TimedKvStore<S> {
    fn store_name(&self) -> &str {
        self.inner.store_name()
    }

    fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.lookup(key)
    }

    fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.key_exists(key)
    }

    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.insert(key, write)
    }

    fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.list_keys_with_prefix(prefix, limit)
    }

    fn count_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<u32, Report<TrustedServerError>> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.count_keys_with_prefix(prefix, limit)
    }

    fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.delete(key)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;

    #[test]
    fn ec_kv_store_operations_accumulate_into_ec_kv_phase() {
        let timings = RequestTimings::new();
        let store = TimedKvStore::new(InMemoryEcKv::new("test-store"), timings.clone());

        store
            .insert(
                "key-a",
                EcKvWrite {
                    body: "{}",
                    metadata: "{}",
                    ttl: Duration::from_secs(60),
                    mode: crate::ec::kv_backend::EcKvWriteMode::Add,
                },
            )
            .expect("should insert into the in-memory store");
        store.lookup("key-a").expect("should read back the entry");

        timings.mark_headers_ready();
        assert!(
            timings.snapshot().kv_ms.is_some(),
            "should record Phase::EcKv across both store calls"
        );
    }

    #[test]
    fn store_name_is_not_timed() {
        let timings = RequestTimings::new();
        let store = TimedKvStore::new(InMemoryEcKv::new("test-store"), timings.clone());

        assert_eq!(store.store_name(), "test-store");
        timings.mark_headers_ready();
        assert!(
            timings.snapshot().kv_ms.is_none(),
            "store_name is a metadata accessor, not a store operation"
        );
    }
}
