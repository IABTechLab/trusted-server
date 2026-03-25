//! KV identity graph operations.
//!
//! This module provides [`KvIdentityGraph`] which wraps a Fastly KV Store
//! and implements the read-modify-write operations for the EC identity graph.
//!
//! All methods return `Result` — callers decide whether to swallow errors
//! (organic request paths) or propagate them (sync endpoints). See the
//! per-operation error handling policy in the spec §7.5.

use std::time::Duration;

use error_stack::{Report, ResultExt};
use fastly::kv_store::{InsertMode, KVStore};

use crate::error::TrustedServerError;

use super::kv_types::{KvEntry, KvMetadata};

/// Maximum number of CAS retry attempts before giving up.
const MAX_CAS_RETRIES: u32 = 3;

/// Minimum interval (seconds) between `last_seen` KV writes for the same key.
/// Prevents write thrashing under bursty traffic — Fastly KV enforces a
/// 1 write/sec limit per key.
const LAST_SEEN_DEBOUNCE_SECS: u64 = 300;

/// TTL for live entries (1 year), matching the EC cookie `Max-Age`.
const ENTRY_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// TTL for withdrawal tombstones (24 hours).
const TOMBSTONE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Outcome of an [`KvIdentityGraph::upsert_partner_id_if_exists`] call.
///
/// Unlike [`KvIdentityGraph::upsert_partner_id`] (which auto-creates entries),
/// this enum encodes the per-mapping rejection reasons needed by the S2S
/// batch sync endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertResult {
    /// The partner ID was successfully written.
    Written,
    /// The KV key does not exist — S2S must not create new entries.
    NotFound,
    /// The entry's `consent.ok` is `false` (withdrawal tombstone).
    ConsentWithdrawn,
    /// The request timestamp is not newer than the stored `synced` value,
    /// so the write was skipped. Counted as accepted by the batch endpoint.
    Stale,
}

/// Wraps a Fastly KV Store for EC identity graph operations.
///
/// Each EC hash (64-char hex prefix) maps to a JSON-encoded [`KvEntry`]
/// containing consent state, geo location, and accumulated partner IDs.
///
/// Methods use optimistic concurrency (generation markers) for safe
/// read-modify-write operations on concurrent requests.
pub struct KvIdentityGraph {
    store_name: String,
}

impl KvIdentityGraph {
    /// Creates a new identity graph backed by the named KV store.
    #[must_use]
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
        }
    }

    /// Returns the configured store name.
    #[must_use]
    pub fn store_name(&self) -> &str {
        &self.store_name
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

    /// Serializes an entry body and metadata for insertion.
    fn serialize_entry(
        entry: &KvEntry,
        store_name: &str,
    ) -> Result<(String, String), Report<TrustedServerError>> {
        let body = serde_json::to_string(entry).change_context(TrustedServerError::KvStore {
            store_name: store_name.to_owned(),
            message: "Failed to serialize KV entry body".to_owned(),
        })?;
        let meta = KvMetadata::from_entry(entry);
        let meta_str =
            serde_json::to_string(&meta).change_context(TrustedServerError::KvStore {
                store_name: store_name.to_owned(),
                message: "Failed to serialize KV entry metadata".to_owned(),
            })?;
        Ok((body, meta_str))
    }

    /// Reads the full entry and its generation marker for CAS writes.
    ///
    /// Returns `Ok(None)` when the key does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or read failure.
    pub fn get(&self, ec_hash: &str) -> Result<Option<(KvEntry, u64)>, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let mut response = match store.lookup(ec_hash) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to read key '{ec_hash}'"),
                    }),
                );
            }
        };

        let generation = response.current_generation();
        let body_bytes = response.take_body_bytes();
        let entry: KvEntry =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to deserialize entry for key '{ec_hash}'"),
            })?;

        Ok(Some((entry, generation)))
    }

    /// Reads only the metadata for an EC hash (no body streaming).
    ///
    /// Returns `Ok(None)` when the key does not exist or has no metadata.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or read failure.
    pub fn get_metadata(
        &self,
        ec_hash: &str,
    ) -> Result<Option<KvMetadata>, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let response = match store.lookup(ec_hash) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to read metadata for key '{ec_hash}'"),
                    }),
                );
            }
        };

        let meta_bytes = match response.metadata() {
            Some(bytes) => bytes,
            None => return Ok(None),
        };

        let meta: KvMetadata =
            serde_json::from_slice(&meta_bytes).change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to deserialize metadata for key '{ec_hash}'"),
            })?;

        Ok(Some(meta))
    }

    /// Creates a new entry. Fails if the key already exists.
    ///
    /// Uses `InsertMode::Add` so concurrent creates for the same EC hash
    /// are safely rejected (only one wins).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error or if the
    /// key already exists (`ItemPreconditionFailed`).
    pub fn create(&self, ec_hash: &str, entry: &KvEntry) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(entry, &self.store_name)?;
        let created = Self::try_insert_add(&store, ec_hash, &body, &meta_str, &self.store_name)?;
        if created {
            Ok(())
        } else {
            Err(Report::new(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Key '{ec_hash}' already exists"),
            }))
        }
    }

    /// Low-level create using a pre-opened store and pre-serialized data.
    ///
    /// Returns `true` if the entry was created, `false` if the key already
    /// exists (`ItemPreconditionFailed`). Other errors are propagated.
    fn try_insert_add(
        store: &KVStore,
        ec_hash: &str,
        body: &str,
        meta_str: &str,
        store_name: &str,
    ) -> Result<bool, Report<TrustedServerError>> {
        match store
            .build_insert()
            .mode(InsertMode::Add)
            .metadata(meta_str)
            .time_to_live(ENTRY_TTL)
            .execute(ec_hash, body)
        {
            Ok(()) => Ok(true),
            Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => Ok(false),
            Err(err) => Err(
                Report::new(err).change_context(TrustedServerError::KvStore {
                    store_name: store_name.to_owned(),
                    message: format!("Failed to create entry for key '{ec_hash}'"),
                }),
            ),
        }
    }

    /// Creates a new entry, or overwrites an existing tombstone on re-consent.
    ///
    /// Three-way behavior:
    /// - **No existing key** — creates the entry (same as [`create`](Self::create)).
    /// - **Existing live entry** (`consent.ok = true`) — no-op, returns `Ok(())`.
    /// - **Existing tombstone** (`consent.ok = false`) — CAS overwrite with
    ///   the new entry. Retries up to [`MAX_CAS_RETRIES`] on conflict.
    ///
    /// Called by `generate_if_needed()` instead of `create()` so that a
    /// user who re-consents within the 24-hour tombstone window recovers
    /// immediately.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error or CAS
    /// exhaustion.
    pub fn create_or_revive(
        &self,
        ec_hash: &str,
        entry: &KvEntry,
    ) -> Result<(), Report<TrustedServerError>> {
        // Serialize once and reuse across the fast path and CAS loop.
        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(entry, &self.store_name)?;

        // Try create first — fast path for new entries.
        if Self::try_insert_add(&store, ec_hash, &body, &meta_str, &self.store_name)? {
            return Ok(());
        }

        // Key exists — read it to determine if it's live or a tombstone.
        let (existing, generation) = match self.get(ec_hash)? {
            Some(pair) => pair,
            // Raced with a delete — try create again.
            None => return self.create(ec_hash, entry),
        };

        // Live entry — nothing to do.
        if existing.consent.ok {
            log::debug!("create_or_revive: live entry exists for '{ec_hash}', no-op");
            return Ok(());
        }

        // Tombstone — CAS overwrite to revive.
        log::info!("create_or_revive: reviving tombstone for '{ec_hash}'");

        let mut current_gen = generation;
        for attempt in 0..MAX_CAS_RETRIES {
            match store
                .build_insert()
                .if_generation_match(current_gen)
                .metadata(&meta_str)
                .time_to_live(ENTRY_TTL)
                .execute(ec_hash, body.as_str())
            {
                Ok(()) => return Ok(()),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "create_or_revive: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{ec_hash}'",
                        attempt + 1,
                    );
                    // Re-read to get fresh generation.
                    match self.get(ec_hash)? {
                        Some((refreshed, gen)) => {
                            if refreshed.consent.ok {
                                // Someone else revived it — done.
                                return Ok(());
                            }
                            current_gen = gen;
                        }
                        None => return self.create(ec_hash, entry),
                    }
                }
                Err(err) => {
                    return Err(
                        Report::new(err).change_context(TrustedServerError::KvStore {
                            store_name: self.store_name.clone(),
                            message: format!(
                                "Failed to revive tombstone for key '{ec_hash}' on attempt {}",
                                attempt + 1,
                            ),
                        }),
                    );
                }
            }
        }

        Err(Report::new(TrustedServerError::KvStore {
            store_name: self.store_name.clone(),
            message: format!(
                "CAS conflict after {MAX_CAS_RETRIES} retries reviving tombstone for '{ec_hash}'"
            ),
        }))
    }

    /// Atomically merges a partner ID into the existing entry.
    ///
    /// Uses CAS (generation markers) to avoid clobbering concurrent writes
    /// from other partners. Retries up to [`MAX_CAS_RETRIES`] on conflict.
    ///
    /// If the root entry does not exist (e.g. the initial `create_or_revive`
    /// failed), creates a minimal live entry first — this is the recovery
    /// path for best-effort EC creation misses.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error or CAS
    /// exhaustion after [`MAX_CAS_RETRIES`] attempts.
    pub fn upsert_partner_id(
        &self,
        ec_hash: &str,
        partner_id: &str,
        uid: &str,
        synced: u64,
    ) -> Result<(), Report<TrustedServerError>> {
        // Open store once for write operations. Note: `self.get()` opens
        // its own handle internally — this is intentional since `KVStore::open`
        // is a cheap name lookup, and keeping the read/write APIs independent
        // simplifies the method signatures.
        let store = self.open_store()?;

        for attempt in 0..MAX_CAS_RETRIES {
            let (mut entry, generation) = match self.get(ec_hash)? {
                Some(pair) => pair,
                None => {
                    // Root entry missing — create a minimal entry.
                    log::info!(
                        "upsert_partner_id: no entry for '{ec_hash}', creating minimal entry"
                    );
                    let minimal = KvEntry::minimal(partner_id, uid, synced);
                    let (min_body, min_meta) = Self::serialize_entry(&minimal, &self.store_name)?;
                    if Self::try_insert_add(
                        &store,
                        ec_hash,
                        &min_body,
                        &min_meta,
                        &self.store_name,
                    )? {
                        return Ok(());
                    }
                    // Key appeared between get() and create — re-read on next iteration.
                    log::debug!(
                        "upsert_partner_id: minimal create raced for '{ec_hash}', retrying (attempt {}/{})",
                        attempt + 1,
                        MAX_CAS_RETRIES,
                    );
                    continue;
                }
            };

            // Reject upserts on withdrawn entries — a late sync must not
            // repopulate partner IDs after consent withdrawal.
            if !entry.consent.ok {
                log::info!(
                    "upsert_partner_id: entry for '{ec_hash}' is a tombstone, rejecting upsert"
                );
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: format!(
                        "Cannot upsert partner '{partner_id}' for withdrawn key '{ec_hash}'"
                    ),
                }));
            }

            // Merge the partner ID.
            entry.ids.insert(
                partner_id.to_owned(),
                super::kv_types::KvPartnerId {
                    uid: uid.to_owned(),
                    synced,
                },
            );

            let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

            match store
                .build_insert()
                .if_generation_match(generation)
                .metadata(&meta_str)
                .time_to_live(ENTRY_TTL)
                .execute(ec_hash, body.as_str())
            {
                Ok(()) => return Ok(()),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "upsert_partner_id: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{ec_hash}'",
                        attempt + 1,
                    );
                    // Loop will re-read on next iteration.
                }
                Err(err) => {
                    return Err(
                        Report::new(err).change_context(TrustedServerError::KvStore {
                            store_name: self.store_name.clone(),
                            message: format!(
                                "Failed to upsert partner '{partner_id}' for key '{ec_hash}'"
                            ),
                        }),
                    );
                }
            }
        }

        Err(Report::new(TrustedServerError::KvStore {
            store_name: self.store_name.clone(),
            message: format!(
                "CAS conflict after {MAX_CAS_RETRIES} retries upserting partner '{partner_id}' for '{ec_hash}'"
            ),
        }))
    }

    /// Upserts a partner ID only if the KV entry already exists.
    ///
    /// Unlike [`Self::upsert_partner_id`], this method does **not** create
    /// entries for missing keys. Used by the S2S batch sync endpoint where
    /// the KV entry must have been created by the organic EC flow.
    ///
    /// Returns [`UpsertResult::Stale`] when the stored `synced` timestamp
    /// for this partner is already >= `synced`, skipping the write.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store I/O or CAS
    /// exhaustion errors.
    pub fn upsert_partner_id_if_exists(
        &self,
        ec_hash: &str,
        partner_id: &str,
        uid: &str,
        synced: u64,
    ) -> Result<UpsertResult, Report<TrustedServerError>> {
        let store = self.open_store()?;

        for attempt in 0..MAX_CAS_RETRIES {
            let (mut entry, generation) = match self.get(ec_hash)? {
                Some(pair) => pair,
                None => return Ok(UpsertResult::NotFound),
            };

            if !entry.consent.ok {
                return Ok(UpsertResult::ConsentWithdrawn);
            }

            // Skip if existing sync is at least as fresh as the request.
            if let Some(existing) = entry.ids.get(partner_id) {
                if existing.synced >= synced {
                    return Ok(UpsertResult::Stale);
                }
            }

            entry.ids.insert(
                partner_id.to_owned(),
                super::kv_types::KvPartnerId {
                    uid: uid.to_owned(),
                    synced,
                },
            );

            let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

            match store
                .build_insert()
                .if_generation_match(generation)
                .metadata(&meta_str)
                .time_to_live(ENTRY_TTL)
                .execute(ec_hash, body.as_str())
            {
                Ok(()) => return Ok(UpsertResult::Written),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "upsert_partner_id_if_exists: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{ec_hash}'",
                        attempt + 1,
                    );
                }
                Err(err) => {
                    return Err(
                        Report::new(err).change_context(TrustedServerError::KvStore {
                            store_name: self.store_name.clone(),
                            message: format!(
                                "Failed to upsert partner '{partner_id}' for key '{ec_hash}'"
                            ),
                        }),
                    );
                }
            }
        }

        Err(Report::new(TrustedServerError::KvStore {
            store_name: self.store_name.clone(),
            message: format!(
                "CAS conflict after {MAX_CAS_RETRIES} retries upserting partner '{partner_id}' for '{ec_hash}'"
            ),
        }))
    }

    /// Updates the `last_seen` timestamp with a 300-second debounce.
    ///
    /// Skips the write if the stored `last_seen` is within
    /// [`LAST_SEEN_DEBOUNCE_SECS`] of the new timestamp, or if the entry
    /// does not exist.
    ///
    /// Does **not** retry on CAS conflict — if someone else wrote more
    /// recently, the debounce condition is satisfied anyway.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error or CAS
    /// conflict.
    pub fn update_last_seen(
        &self,
        ec_hash: &str,
        timestamp: u64,
    ) -> Result<(), Report<TrustedServerError>> {
        let (mut entry, generation) = match self.get(ec_hash)? {
            Some(pair) => pair,
            None => {
                log::debug!("update_last_seen: no entry for '{ec_hash}', skipping");
                return Ok(());
            }
        };

        // Skip tombstones — a stale cookie should not extend a 24h tombstone
        // back to 1-year TTL.
        if !entry.consent.ok {
            log::debug!("update_last_seen: entry for '{ec_hash}' is a tombstone, skipping");
            return Ok(());
        }

        // Guard against stale/out-of-order timestamps.
        if timestamp <= entry.last_seen {
            log::trace!(
                "update_last_seen: stale timestamp for '{ec_hash}' (stored={}, incoming={timestamp})",
                entry.last_seen,
            );
            return Ok(());
        }

        // Debounce: skip if the stored value is recent enough.
        if timestamp - entry.last_seen < LAST_SEEN_DEBOUNCE_SECS {
            log::trace!(
                "update_last_seen: debounced for '{ec_hash}' (delta={}s)",
                timestamp - entry.last_seen,
            );
            return Ok(());
        }

        entry.last_seen = timestamp;
        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

        store
            .build_insert()
            .if_generation_match(generation)
            .metadata(&meta_str)
            .time_to_live(ENTRY_TTL)
            .execute(ec_hash, body)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to update last_seen for key '{ec_hash}'"),
            })
    }

    /// Writes a withdrawal tombstone for consent enforcement.
    ///
    /// Overwrites the entry with `consent.ok = false`, empty partner IDs,
    /// and a 24-hour TTL. Uses unconditional overwrite (no CAS) since the
    /// entry is being withdrawn regardless of concurrent state.
    ///
    /// The tombstone allows batch sync clients (`POST /api/v1/sync`) to
    /// distinguish `consent_withdrawn` from `ec_hash_not_found` for 24 hours.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error. Callers on
    /// the browser path should log at `error` level and continue — cookie
    /// deletion is the primary enforcement mechanism.
    pub fn write_withdrawal_tombstone(
        &self,
        ec_hash: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        let entry = KvEntry::tombstone(current_timestamp());
        let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

        store
            .build_insert()
            .metadata(&meta_str)
            .time_to_live(TOMBSTONE_TTL)
            .execute(ec_hash, body)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to write tombstone for key '{ec_hash}'"),
            })
    }

    /// Hard-deletes the entry.
    ///
    /// Reserved for the IAB data deletion framework (deferred). For consent
    /// withdrawal, use [`write_withdrawal_tombstone`](Self::write_withdrawal_tombstone).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error.
    pub fn delete(&self, ec_hash: &str) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        store
            .delete(ec_hash)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to delete key '{ec_hash}'"),
            })
    }
}

/// Returns the current Unix timestamp in seconds.
///
/// Uses `std::time::SystemTime` which is supported on `wasm32-wasip1`.
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(MAX_CAS_RETRIES, 3);
        assert_eq!(LAST_SEEN_DEBOUNCE_SECS, 300);
        assert_eq!(ENTRY_TTL, Duration::from_secs(31_536_000));
        assert_eq!(TOMBSTONE_TTL, Duration::from_secs(86_400));
    }

    #[test]
    fn current_timestamp_is_nonzero() {
        let ts = current_timestamp();
        assert!(ts > 0, "should return a nonzero timestamp");
    }

    #[test]
    fn serialize_entry_produces_valid_json() {
        let entry = KvEntry::tombstone(1000);
        let (body, meta) =
            KvIdentityGraph::serialize_entry(&entry, "test-store").expect("should serialize entry");

        // Verify body is valid JSON.
        let _: KvEntry =
            serde_json::from_str(&body).expect("should deserialize body back to KvEntry");

        // Verify metadata is valid JSON.
        let _: KvMetadata =
            serde_json::from_str(&meta).expect("should deserialize metadata back to KvMetadata");
    }
}
