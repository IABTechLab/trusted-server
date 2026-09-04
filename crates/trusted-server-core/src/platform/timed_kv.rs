//! Latency-only timing decorator for KV store handles.
//!
//! [`TimedKvStore`] wraps an inner store plus a [`RequestTimings`] handle and
//! records [`Phase::EcKv`] around every call. It implements both
//! [`PlatformKvStore`] (for consent-store access obtained through
//! [`RuntimeServices`](super::RuntimeServices)) and [`EcKvStore`] (for
//! [`KvIdentityGraph`](crate::ec::kv::KvIdentityGraph) construction sites),
//! because no single existing abstraction covers the whole `ts-kv` taxonomy:
//! EC graph operations go through [`EcKvStore`] while consent persistence
//! uses [`PlatformKvStore`] directly.
//!
//! The decorator measures store-call latency only: it never reads, parses,
//! or logs any value passing through it.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edgezero_core::key_value_store::{KvError, KvPage, KvStore as PlatformKvStore};
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

#[async_trait(?Send)]
impl PlatformKvStore for TimedKvStore<Arc<dyn PlatformKvStore>> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.get_bytes(key).await
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.put_bytes(key, value).await
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        ttl: Duration,
    ) -> Result<(), KvError> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.put_bytes_with_ttl(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.delete(key).await
    }

    async fn list_keys_page(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<KvPage, KvError> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.list_keys_page(prefix, cursor, limit).await
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

    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let _span = self.timings.span(Phase::EcKv);
        self.inner.insert(key, write)
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
    use std::time::Duration as StdDuration;

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
                    ttl: StdDuration::from_secs(60),
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

    #[test]
    fn platform_kv_store_operations_accumulate_into_ec_kv_phase() {
        let timings = RequestTimings::new();
        let inner: Arc<dyn PlatformKvStore> = Arc::new(crate::platform::UnavailableKvStore);
        let store = TimedKvStore::new(inner, timings.clone());

        // UnavailableKvStore errors on every call; the decorator still times
        // the attempt regardless of outcome.
        futures::executor::block_on(async {
            let _ = store.get_bytes("key").await;
            let _ = store.put_bytes("key", Bytes::from_static(b"value")).await;
        });

        timings.mark_headers_ready();
        assert!(
            timings.snapshot().kv_ms.is_some(),
            "should record Phase::EcKv even when the inner store errors"
        );
    }
}
