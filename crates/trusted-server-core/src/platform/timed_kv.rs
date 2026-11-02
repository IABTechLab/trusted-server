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

    struct DelayingCountingEcKv {
        inner: InMemoryEcKv,
        calls: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl DelayingCountingEcKv {
        fn record(&self, operation: &'static str) {
            self.calls
                .lock()
                .expect("should lock KV operation calls")
                .push(operation);
            std::thread::sleep(Duration::from_millis(12));
        }
    }

    impl EcKvStore for DelayingCountingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.record("lookup");
            self.inner.lookup(key)
        }

        fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.record("key_exists");
            self.inner.key_exists(key)
        }

        fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            self.record("insert");
            self.inner.insert(key, write)
        }

        fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.record("list_keys_with_prefix");
            self.inner.list_keys_with_prefix(prefix, limit)
        }

        fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            self.record("count_keys_with_prefix");
            self.inner.count_keys_with_prefix(prefix, limit)
        }

        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.record("delete");
            self.inner.delete(key)
        }
    }

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
        assert!(store.key_exists("key-a").expect("should check key"));
        assert_eq!(
            store
                .list_keys_with_prefix("key-", 10)
                .expect("should list keys"),
            vec!["key-a".to_string()]
        );
        assert_eq!(
            store
                .count_keys_with_prefix("key-", 10)
                .expect("should count keys"),
            1
        );
        store.delete("key-a").expect("should delete key");
        assert!(!store.key_exists("key-a").expect("should check deletion"));

        timings.mark_headers_ready();
        assert!(
            timings.snapshot().kv_ms.is_some(),
            "should accumulate Phase::EcKv across lookup, key_exists, insert, list, count, and delete"
        );
    }

    #[test]
    fn every_store_operation_contributes_a_separate_timed_span() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let timings = RequestTimings::new();
        let store = TimedKvStore::new(
            DelayingCountingEcKv {
                inner: InMemoryEcKv::new("test-store"),
                calls: std::sync::Arc::clone(&calls),
            },
            timings.clone(),
        );
        let mut previous_ms = 0;
        let mut assert_span_added = |operation: &str| {
            let current_ms = timings
                .snapshot()
                .kv_ms
                .expect("each store operation should record an EcKv span");
            assert!(
                current_ms > previous_ms,
                "{operation} should add its own delayed span: previous={previous_ms}ms current={current_ms}ms"
            );
            previous_ms = current_ms;
        };

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
            .expect("should insert into the delaying store");
        assert_span_added("insert");
        store.lookup("key-a").expect("should look up key");
        assert_span_added("lookup");
        store.key_exists("key-a").expect("should check key");
        assert_span_added("key_exists");
        store
            .list_keys_with_prefix("key-", 10)
            .expect("should list keys");
        assert_span_added("list_keys_with_prefix");
        store
            .count_keys_with_prefix("key-", 10)
            .expect("should count keys");
        assert_span_added("count_keys_with_prefix");
        store.delete("key-a").expect("should delete key");
        assert_span_added("delete");

        assert_eq!(
            *calls.lock().expect("should lock KV operation calls"),
            vec![
                "insert",
                "lookup",
                "key_exists",
                "list_keys_with_prefix",
                "count_keys_with_prefix",
                "delete",
            ],
            "the wrapper should forward every operation exactly once"
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
