//! Process-local EC identity store and rate limiter for the Axum dev server.
//!
//! The Axum dev server has no platform KV store, so the EC identity graph is
//! kept in process memory. Every operation takes one mutex, which makes reads
//! strongly consistent and gives the generation-based compare-and-swap that
//! [`EcKvStore`] requires. Data is lost when the process exits, so this store
//! is only for local development and tests.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use error_stack::Report;
use trusted_server_core::ec::kv_backend::{
    EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
};
use trusted_server_core::ec::rate_limiter::RateLimiter;
use trusted_server_core::error::TrustedServerError;
use web_time::Instant;

/// One stored row.
struct StoredEntry {
    body: Vec<u8>,
    metadata: Vec<u8>,
    generation: u64,
    expires_at: Option<Instant>,
}

impl StoredEntry {
    fn is_live(&self, now: Instant) -> bool {
        self.expires_at.is_none_or(|expires_at| now < expires_at)
    }
}

#[derive(Default)]
struct StoreState {
    entries: BTreeMap<String, StoredEntry>,
    /// Last generation handed out. Generations are never reused, even after
    /// a delete, so a stale compare-and-swap marker can never match a newer
    /// row for the same key.
    last_generation: u64,
}

impl StoreState {
    /// Drops `key` when its TTL has passed, so reads never see expired rows.
    fn purge_if_expired(&mut self, key: &str, now: Instant) {
        if self
            .entries
            .get(key)
            .is_some_and(|stored| !stored.is_live(now))
        {
            self.entries.remove(key);
        }
    }
}

/// Process-local [`EcKvStore`] for the Axum dev server.
///
/// Cloning is cheap and every clone shares the same rows, so one store can
/// back the identity graph built for each request.
#[derive(Clone)]
pub struct LocalEcKvStore {
    name: Arc<str>,
    state: Arc<Mutex<StoreState>>,
}

impl LocalEcKvStore {
    /// Creates an empty store reported under `name` in logs and errors.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: Arc::from(name),
            state: Arc::new(Mutex::new(StoreState::default())),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, StoreState>, Report<TrustedServerError>> {
        self.state.lock().map_err(|_| {
            Report::new(TrustedServerError::KvStore {
                store_name: self.name.to_string(),
                message: "local EC store lock is poisoned".to_owned(),
            })
        })
    }
}

impl EcKvStore for LocalEcKvStore {
    fn store_name(&self) -> &str {
        &self.name
    }

    fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        let mut state = self.lock()?;
        state.purge_if_expired(key, Instant::now());
        Ok(state.entries.get(key).map(|stored| EcKvLookup {
            body: stored.body.clone(),
            metadata: Some(stored.metadata.clone()),
            generation: stored.generation,
        }))
    }

    fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        // Reads and writes share one lock, so this answer is strongly
        // consistent with every completed write.
        let mut state = self.lock()?;
        state.purge_if_expired(key, Instant::now());
        Ok(state.entries.contains_key(key))
    }

    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let now = Instant::now();
        let mut state = self.lock()?;
        state.purge_if_expired(key, now);
        let existing_generation = state.entries.get(key).map(|stored| stored.generation);

        let precondition_holds = match write.mode {
            EcKvWriteMode::Add => existing_generation.is_none(),
            EcKvWriteMode::Overwrite => true,
            EcKvWriteMode::IfGenerationMatch(expected) => existing_generation == Some(expected),
        };
        if !precondition_holds {
            return Ok(EcKvWriteOutcome::PreconditionFailed);
        }

        state.last_generation += 1;
        let generation = state.last_generation;
        state.entries.insert(
            key.to_owned(),
            StoredEntry {
                body: write.body.as_bytes().to_vec(),
                metadata: write.metadata.as_bytes().to_vec(),
                generation,
                expires_at: now.checked_add(write.ttl),
            },
        );
        Ok(EcKvWriteOutcome::Written)
    }

    fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        let now = Instant::now();
        let state = self.lock()?;
        Ok(state
            .entries
            .range(prefix.to_owned()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .filter(|(_, stored)| stored.is_live(now))
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .map(|(key, _)| key.clone())
            .collect())
    }

    fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.lock()?.entries.remove(key);
        Ok(())
    }
}

/// Length of one rate-limit window.
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Process-local fixed-window [`RateLimiter`] for the Axum dev server.
///
/// Mirrors the Fastly limiter: an hourly budget is enforced as a per-minute
/// budget rounded up to at least one request.
#[derive(Clone, Default)]
pub struct LocalRateLimiter {
    windows: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
}

impl RateLimiter for LocalRateLimiter {
    fn exceeded(&self, key: &str, hourly_limit: u32) -> Result<bool, Report<TrustedServerError>> {
        if hourly_limit == 0 {
            return Ok(true);
        }
        let per_minute_limit = (hourly_limit.saturating_add(59) / 60).max(1);
        let now = Instant::now();
        let mut windows = self.windows.lock().map_err(|_| {
            Report::new(TrustedServerError::KvStore {
                store_name: "local-rate-limiter".to_owned(),
                message: "local rate limiter lock is poisoned".to_owned(),
            })
        })?;
        let window = windows.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(window.0) >= RATE_WINDOW {
            *window = (now, 0);
        }
        if window.1 >= per_minute_limit {
            return Ok(true);
        }
        window.1 += 1;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(body: &str, mode: EcKvWriteMode) -> EcKvWrite<'_> {
        EcKvWrite {
            body,
            metadata: "{}",
            ttl: Duration::from_secs(60),
            mode,
        }
    }

    #[test]
    fn add_fails_when_the_key_exists() {
        let store = LocalEcKvStore::new("example-store");

        let first = store
            .insert("key", write("a", EcKvWriteMode::Add))
            .expect("should insert");
        let second = store
            .insert("key", write("b", EcKvWriteMode::Add))
            .expect("should report the precondition");

        assert_eq!(first, EcKvWriteOutcome::Written, "should create the key");
        assert_eq!(
            second,
            EcKvWriteOutcome::PreconditionFailed,
            "should refuse to add an existing key"
        );
    }

    #[test]
    fn generation_match_applies_only_to_the_current_generation() {
        let store = LocalEcKvStore::new("example-store");
        store
            .insert("key", write("a", EcKvWriteMode::Add))
            .expect("should insert");
        let generation = store
            .lookup("key")
            .expect("should read")
            .expect("should find the key")
            .generation;

        let applied = store
            .insert(
                "key",
                write("b", EcKvWriteMode::IfGenerationMatch(generation)),
            )
            .expect("should write");
        let stale = store
            .insert(
                "key",
                write("c", EcKvWriteMode::IfGenerationMatch(generation)),
            )
            .expect("should report the precondition");

        assert_eq!(applied, EcKvWriteOutcome::Written, "should apply the CAS");
        assert_eq!(
            stale,
            EcKvWriteOutcome::PreconditionFailed,
            "should reject a stale generation"
        );
        let body = store
            .lookup("key")
            .expect("should read")
            .expect("should find the key")
            .body;
        assert_eq!(body, b"b", "should keep the CAS winner");
    }

    #[test]
    fn generations_are_not_reused_after_delete() {
        let store = LocalEcKvStore::new("example-store");
        store
            .insert("key", write("a", EcKvWriteMode::Add))
            .expect("should insert");
        let old = store
            .lookup("key")
            .expect("should read")
            .expect("should find the key")
            .generation;
        store.delete("key").expect("should delete");
        store
            .insert("key", write("b", EcKvWriteMode::Add))
            .expect("should insert again");

        let outcome = store
            .insert("key", write("c", EcKvWriteMode::IfGenerationMatch(old)))
            .expect("should report the precondition");

        assert_eq!(
            outcome,
            EcKvWriteOutcome::PreconditionFailed,
            "should not match a generation from a deleted row"
        );
    }

    #[test]
    fn expired_rows_are_absent() {
        let store = LocalEcKvStore::new("example-store");
        store
            .insert(
                "key",
                EcKvWrite {
                    body: "a",
                    metadata: "{}",
                    ttl: Duration::ZERO,
                    mode: EcKvWriteMode::Add,
                },
            )
            .expect("should insert");

        assert!(
            !store.key_exists("key").expect("should check"),
            "should treat an expired row as absent"
        );
        assert!(
            store.lookup("key").expect("should read").is_none(),
            "should not return an expired row"
        );
    }

    #[test]
    fn prefix_listing_is_bounded() {
        let store = LocalEcKvStore::new("example-store");
        for key in ["a.1", "a.2", "a.3", "b.1"] {
            store
                .insert(key, write("x", EcKvWriteMode::Overwrite))
                .expect("should insert");
        }

        let keys = store
            .list_keys_with_prefix("a.", 2)
            .expect("should list keys");

        assert_eq!(keys, vec!["a.1", "a.2"], "should return one bounded page");
    }

    #[test]
    fn rate_limiter_enforces_the_per_minute_budget() {
        let limiter = LocalRateLimiter::default();

        let first = limiter.exceeded("key", 60).expect("should check");
        let second = limiter.exceeded("key", 60).expect("should check");

        assert!(!first, "should allow the first request");
        assert!(second, "should limit the second request in the window");
    }
}
