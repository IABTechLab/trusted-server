//! Fastly KV Store implementation of the core [`EcKvStore`] primitives.
//!
//! Maps the platform-neutral identity-graph store operations onto the
//! Fastly KV Store API, including generation markers for compare-and-swap
//! writes (`if_generation_match`).

use error_stack::{Report, ResultExt};
use fastly::kv_store::{InsertMode, KVStore, KVStoreError, ListPage};
use trusted_server_core::ec::kv_backend::{
    EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
};
use trusted_server_core::ec::log_id;
use trusted_server_core::error::TrustedServerError;

/// Bounds the work a withdrawal can trigger when keys share its prefix.
const EXISTENCE_PAGE_SIZE: u32 = 100;
const EXISTENCE_MAX_PAGES: usize = 4;

/// Checks exact equality while driving one strong list page at a time.
///
/// Explicit pagination avoids the SDK iterator reissuing a failed page.
fn contains_exact_key(
    store_name: &str,
    key: &str,
    mut fetch_page: impl FnMut(Option<&str>) -> Result<ListPage, KVStoreError>,
) -> Result<bool, Report<TrustedServerError>> {
    let mut cursor = None;
    for _ in 0..EXISTENCE_MAX_PAGES {
        let page = match fetch_page(cursor.as_deref()) {
            Ok(page) => page,
            Err(KVStoreError::ItemNotFound) => return Ok(false),
            Err(error) => {
                return Err(
                    Report::new(error).change_context(TrustedServerError::KvStore {
                        store_name: store_name.to_owned(),
                        message: format!("Failed to confirm existence of key '{}'", log_id(key)),
                    }),
                );
            }
        };
        if page.keys().iter().any(|listed| listed == key) {
            return Ok(true);
        }
        cursor = page.next_cursor();
        if cursor.is_none() {
            return Ok(false);
        }
    }
    Err(Report::new(TrustedServerError::KvStore {
        store_name: store_name.to_owned(),
        message: format!(
            "Existence check exceeds page budget for key '{}'",
            log_id(key)
        ),
    }))
}

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
                        message: format!("Failed to read key '{}'", log_id(key)),
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

    fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        let store = self.open_store()?;
        contains_exact_key(&self.store_name, key, |cursor| {
            // build_list defaults to ListMode::Strong, reading primary state.
            let mut request = store.build_list().prefix(key).limit(EXISTENCE_PAGE_SIZE);
            if let Some(cursor) = cursor {
                request = request.cursor(cursor);
            }
            request.execute()
        })
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
                    message: format!("Failed to write entry for key '{}'", log_id(key)),
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
                message: format!("Failed to list keys with prefix '{}'", log_id(prefix)),
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
                message: format!("Failed to delete key '{}'", log_id(key)),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// KV store declared for the local simulator in `fastly.toml`.
    const TEST_STORE: &str = "ec_identity_store";

    /// Entry metadata. Opaque to the backend, which only round-trips bytes.
    const METADATA: &str = "entry-metadata";

    fn store() -> FastlyEcKvStore {
        FastlyEcKvStore::new(TEST_STORE)
    }

    fn write(key: &str, body: &str, mode: EcKvWriteMode) -> EcKvWriteOutcome {
        store()
            .insert(
                key,
                EcKvWrite {
                    body,
                    metadata: METADATA,
                    ttl: Duration::from_secs(60),
                    mode,
                },
            )
            .expect("should reach the store")
    }

    fn page(keys: &[&str], next_cursor: Option<&str>) -> ListPage {
        serde_json::from_value(serde_json::json!({
            "data": keys,
            "meta": { "limit": EXISTENCE_PAGE_SIZE, "next_cursor": next_cursor, "prefix": "key" }
        }))
        .expect("should build a list page")
    }

    #[test]
    fn exact_existence_follows_cursors_and_rejects_prefix_collisions() {
        let mut cursors = Vec::new();
        let found = contains_exact_key(TEST_STORE, "key", |cursor| {
            cursors.push(cursor.map(str::to_owned));
            Ok(if cursor.is_none() {
                page(&["key-longer"], Some("next"))
            } else {
                page(&["key"], None)
            })
        })
        .expect("should find an exact match on a later page");
        assert!(found, "should find the exact key");
        assert_eq!(
            cursors,
            [None, Some("next".to_string())],
            "should pass the returned cursor to the next request"
        );
        assert!(
            !contains_exact_key(TEST_STORE, "key", |_| Ok(page(&["key-longer"], None)))
                .expect("should finish listing"),
            "should not accept a longer prefix match"
        );
    }

    #[test]
    fn exact_existence_stops_at_a_first_page_match() {
        let mut calls = 0;
        assert!(
            contains_exact_key(TEST_STORE, "key", |_| {
                calls += 1;
                Ok(page(&["key"], Some("unused")))
            })
            .expect("should find the key"),
            "should recognize exact equality"
        );
        assert_eq!(calls, 1, "should not fetch another page after a match");
    }

    #[test]
    fn exact_existence_terminates_on_item_not_found() {
        let mut calls = 0;
        assert!(
            !contains_exact_key(TEST_STORE, "key", |_| {
                calls += 1;
                Err(KVStoreError::ItemNotFound)
            })
            .expect("should report absence"),
            "should return absence immediately"
        );
        assert_eq!(calls, 1, "should not reissue a failed page");
    }

    #[test]
    fn exact_existence_reports_errors_without_retrying() {
        let mut calls = 0;
        assert!(
            contains_exact_key(TEST_STORE, "key", |_| {
                calls += 1;
                Err(KVStoreError::TooManyRequests)
            })
            .is_err(),
            "should propagate list failures"
        );
        assert_eq!(calls, 1, "should not retry a store error");
    }

    #[test]
    fn exact_existence_reports_exhaustion_instead_of_absence() {
        let mut calls = 0;
        let key = format!("{}.ABC123", "a".repeat(64));
        let error = contains_exact_key(TEST_STORE, &key, |_| {
            calls += 1;
            Ok(page(&[], Some("more")))
        })
        .expect_err("should report an inconclusive bounded check");
        assert_eq!(
            calls, EXISTENCE_MAX_PAGES,
            "should enforce the page budget even for empty pages"
        );
        assert!(
            !format!("{error:?}").contains(&key),
            "should redact the identifier in budget errors"
        );
    }

    #[test]
    fn exact_existence_accepts_a_match_on_the_last_allowed_page() {
        let mut calls = 0;
        let found = contains_exact_key(TEST_STORE, "key", |_| {
            calls += 1;
            Ok(if calls == EXISTENCE_MAX_PAGES {
                page(&["key"], Some("more"))
            } else {
                page(&[], Some("next"))
            })
        })
        .expect("should inspect the last allowed page");
        assert!(found, "should not discard a match at the budget boundary");
        assert_eq!(
            calls, EXISTENCE_MAX_PAGES,
            "should not request a page beyond the budget"
        );
    }

    #[test]
    fn strong_existence_checks_real_store_keys_exactly() {
        let backend = store();
        let key = "existence-test-key";
        let longer = "existence-test-key-longer";
        write(longer, "body", EcKvWriteMode::Overwrite);
        assert!(
            !backend.key_exists(key).expect("should list the prefix"),
            "should not confuse a prefix with an exact key"
        );
        write(key, "body", EcKvWriteMode::Overwrite);
        assert!(
            backend.key_exists(key).expect("should list the issued key"),
            "should see a completed issuance"
        );
        backend.delete(key).expect("should delete exact key");
        backend.delete(longer).expect("should delete longer key");
        assert!(
            !backend.key_exists(key).expect("should list after deletion"),
            "should report absence after deletion"
        );
    }

    #[test]
    fn opening_a_store_this_service_does_not_have_is_an_error() {
        let error = FastlyEcKvStore::new("no_such_store")
            .lookup("any-key")
            .expect_err("should not resolve against a store that is not linked");

        assert!(
            matches!(
                error.current_context(),
                TrustedServerError::KvStore { store_name, .. } if store_name == "no_such_store"
            ),
            "should name the store it could not open: {error:?}"
        );
    }

    #[test]
    fn a_missing_key_is_absent_rather_than_an_error() {
        let absent = format!("{}.ABC123", "1".repeat(64));

        assert!(
            store()
                .lookup(&absent)
                .expect("should reach the store")
                .is_none(),
            "a key the store does not hold is absent, not a failure"
        );
    }

    #[test]
    fn an_entry_round_trips_through_insert_lookup_and_delete() {
        let key = format!("{}.ABC123", "2".repeat(64));
        let backend = store();

        assert_eq!(
            write(&key, "entry-body-1", EcKvWriteMode::Overwrite),
            EcKvWriteOutcome::Written,
            "should write the entry"
        );

        let found = backend
            .lookup(&key)
            .expect("should reach the store")
            .expect("should hold the entry just written");
        assert_eq!(found.body, b"entry-body-1", "should read back the body");
        assert_eq!(
            found.metadata.as_deref(),
            Some(METADATA.as_bytes()),
            "should read back the metadata"
        );

        backend.delete(&key).expect("should delete the entry");
        assert!(
            backend
                .lookup(&key)
                .expect("should reach the store")
                .is_none(),
            "a deleted key is absent"
        );
    }

    #[test]
    fn add_mode_refuses_a_key_that_already_exists() {
        let key = format!("{}.ABC123", "3".repeat(64));
        let backend = store();

        assert_eq!(
            write(&key, "entry-body-1", EcKvWriteMode::Add),
            EcKvWriteOutcome::Written,
            "should create a key nothing holds"
        );
        assert_eq!(
            write(&key, "entry-body-2", EcKvWriteMode::Add),
            EcKvWriteOutcome::PreconditionFailed,
            "a precondition failure is control flow, not an error"
        );

        backend.delete(&key).expect("should delete the entry");
    }

    #[test]
    fn a_generation_mismatch_is_reported_as_a_precondition_failure() {
        let key = format!("{}.ABC123", "4".repeat(64));
        let backend = store();

        write(&key, "entry-body-1", EcKvWriteMode::Overwrite);
        let generation = backend
            .lookup(&key)
            .expect("should reach the store")
            .expect("should hold the entry just written")
            .generation;

        assert_eq!(
            write(
                &key,
                "entry-body-2",
                EcKvWriteMode::IfGenerationMatch(generation)
            ),
            EcKvWriteOutcome::Written,
            "should write when the generation still matches"
        );
        assert_eq!(
            write(
                &key,
                "entry-body-3",
                EcKvWriteMode::IfGenerationMatch(generation)
            ),
            EcKvWriteOutcome::PreconditionFailed,
            "the generation moved on with the previous write"
        );

        backend.delete(&key).expect("should delete the entry");
    }

    #[test]
    fn counting_a_prefix_counts_only_the_keys_under_it() {
        let hash = "5".repeat(64);
        let backend = store();
        let keys = [format!("{hash}.AAA111"), format!("{hash}.BBB222")];
        for key in &keys {
            write(key, "entry-body-1", EcKvWriteMode::Overwrite);
        }

        assert_eq!(
            backend
                .count_keys_with_prefix(&hash, 100)
                .expect("should list the prefix"),
            2,
            "should count both keys issued under this hash"
        );
        assert_eq!(
            backend
                .count_keys_with_prefix(&"6".repeat(64), 100)
                .expect("should list the prefix"),
            0,
            "should count nothing under a hash nothing was issued for"
        );

        for key in &keys {
            backend.delete(key).expect("should delete the entry");
        }
    }
}
