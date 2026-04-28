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

use super::current_timestamp;
use super::generation::ec_hash;
use super::kv_types::{KvEntry, KvMetadata, KvNetwork};

/// Maximum number of CAS retry attempts before giving up.
const MAX_CAS_RETRIES: u32 = 5;

/// Maximum number of keys to request when counting hash-prefix matches
/// for cluster size evaluation. Anything above this is clearly a large
/// shared network; the exact count doesn't matter.
const CLUSTER_LIST_LIMIT: u32 = 100;

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
    /// The partner ID already had the requested UID, so no write was needed.
    Unchanged,
}

use super::log_id;

/// Wraps a Fastly KV Store for EC identity graph operations.
///
/// Each EC ID (`{64hex}.{6alnum}`) maps to a JSON-encoded [`KvEntry`]
/// containing consent state, geo location, and accumulated partner IDs.
///
/// Methods use optimistic concurrency (generation markers) for safe
/// read-modify-write operations on concurrent requests.
#[derive(Debug)]
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
        entry.validate().map_err(|message| {
            Report::new(TrustedServerError::KvStore {
                store_name: store_name.to_owned(),
                message: format!("Refusing to serialize invalid KV entry: {message}"),
            })
        })?;

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
    pub fn get(&self, ec_id: &str) -> Result<Option<(KvEntry, u64)>, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let mut response = match store.lookup(ec_id) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to read key '{ec_id}'"),
                    }),
                );
            }
        };

        let generation = response.current_generation();
        let body_bytes = response.take_body_bytes();
        let entry = Self::deserialize_entry(&self.store_name, ec_id, &body_bytes)?;

        Ok(Some((entry, generation)))
    }

    fn deserialize_entry(
        store_name: &str,
        ec_id: &str,
        body_bytes: &[u8],
    ) -> Result<KvEntry, Report<TrustedServerError>> {
        let entry: KvEntry =
            serde_json::from_slice(body_bytes).change_context(TrustedServerError::KvStore {
                store_name: store_name.to_owned(),
                message: format!("Failed to deserialize entry for key '{ec_id}'"),
            })?;

        entry.validate().map_err(|message| {
            Report::new(TrustedServerError::KvStore {
                store_name: store_name.to_owned(),
                message: format!("Loaded invalid entry for key '{ec_id}': {message}"),
            })
        })?;

        Ok(entry)
    }

    /// Reads only the metadata for an EC ID key (no body streaming).
    ///
    /// Returns `Ok(None)` when the key does not exist or has no metadata.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or read failure.
    pub fn get_metadata(
        &self,
        ec_id: &str,
    ) -> Result<Option<KvMetadata>, Report<TrustedServerError>> {
        let store = self.open_store()?;
        let response = match store.lookup(ec_id) {
            Ok(resp) => resp,
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => return Ok(None),
            Err(err) => {
                return Err(
                    Report::new(err).change_context(TrustedServerError::KvStore {
                        store_name: self.store_name.clone(),
                        message: format!("Failed to read metadata for key '{ec_id}'"),
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
                message: format!("Failed to deserialize metadata for key '{ec_id}'"),
            })?;

        Ok(Some(meta))
    }

    /// Creates a new entry. Fails if the key already exists.
    ///
    /// Uses `InsertMode::Add` so concurrent creates for the same EC ID
    /// are safely rejected (only one wins).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error or if the
    /// key already exists (`ItemPreconditionFailed`).
    pub fn create(&self, ec_id: &str, entry: &KvEntry) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(entry, &self.store_name)?;
        let created = Self::try_insert_add(&store, ec_id, &body, &meta_str, &self.store_name)?;
        if created {
            Ok(())
        } else {
            Err(Report::new(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Key '{ec_id}' already exists"),
            }))
        }
    }

    /// Low-level create using a pre-opened store and pre-serialized data.
    ///
    /// Returns `true` if the entry was created, `false` if the key already
    /// exists (`ItemPreconditionFailed`). Other errors are propagated.
    fn try_insert_add(
        store: &KVStore,
        ec_id: &str,
        body: &str,
        meta_str: &str,
        store_name: &str,
    ) -> Result<bool, Report<TrustedServerError>> {
        match store
            .build_insert()
            .mode(InsertMode::Add)
            .metadata(meta_str)
            .time_to_live(ENTRY_TTL)
            .execute(ec_id, body)
        {
            Ok(()) => Ok(true),
            Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => Ok(false),
            Err(err) => Err(
                Report::new(err).change_context(TrustedServerError::KvStore {
                    store_name: store_name.to_owned(),
                    message: format!("Failed to create entry for key '{ec_id}'"),
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
        ec_id: &str,
        entry: &KvEntry,
    ) -> Result<(), Report<TrustedServerError>> {
        // Serialize once and reuse across the fast path and CAS loop.
        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(entry, &self.store_name)?;

        // Try create first — fast path for new entries.
        if Self::try_insert_add(&store, ec_id, &body, &meta_str, &self.store_name)? {
            return Ok(());
        }

        // Key exists — read it to determine if it's live or a tombstone.
        let (existing, generation) = match self.get(ec_id)? {
            Some(pair) => pair,
            // Raced with a delete — try create again.
            None => return self.create(ec_id, entry),
        };

        // Live entry — nothing to do.
        if existing.consent.ok {
            log::debug!(
                "create_or_revive: live entry exists for '{}', no-op",
                log_id(ec_id)
            );
            return Ok(());
        }

        // Tombstone — CAS overwrite to revive.
        log::info!(
            "create_or_revive: reviving tombstone for '{}'",
            log_id(ec_id)
        );

        let mut current_gen = generation;
        for attempt in 0..MAX_CAS_RETRIES {
            match store
                .build_insert()
                .if_generation_match(current_gen)
                .metadata(&meta_str)
                .time_to_live(ENTRY_TTL)
                .execute(ec_id, body.as_str())
            {
                Ok(()) => return Ok(()),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "create_or_revive: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{}'",
                        attempt + 1,
                        log_id(ec_id),
                    );
                    // Re-read immediately to get a fresh generation. Sleeping in
                    // the CAS loop would block the Fastly Compute request worker.
                    match self.get(ec_id)? {
                        Some((refreshed, gen)) => {
                            if refreshed.consent.ok {
                                // Someone else revived it — done.
                                return Ok(());
                            }
                            current_gen = gen;
                        }
                        None => return self.create(ec_id, entry),
                    }
                }
                Err(err) => {
                    return Err(
                        Report::new(err).change_context(TrustedServerError::KvStore {
                            store_name: self.store_name.clone(),
                            message: format!(
                                "Failed to revive tombstone for key '{ec_id}' on attempt {}",
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
                "CAS conflict after {MAX_CAS_RETRIES} retries reviving tombstone for '{ec_id}'"
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
        ec_id: &str,
        partner_id: &str,
        uid: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        // Open store once for write operations. Note: `self.get()` opens
        // its own handle internally — this is intentional since `KVStore::open`
        // is a cheap name lookup, and keeping the read/write APIs independent
        // simplifies the method signatures.
        let store = self.open_store()?;

        for attempt in 0..MAX_CAS_RETRIES {
            let (mut entry, generation) = match self.get(ec_id)? {
                Some(pair) => pair,
                None => {
                    // Root entry missing — create a minimal entry.
                    log::info!(
                        "upsert_partner_id: no entry for '{}', creating minimal entry",
                        log_id(ec_id)
                    );
                    let minimal = KvEntry::minimal(partner_id, uid, current_timestamp());
                    let (min_body, min_meta) = Self::serialize_entry(&minimal, &self.store_name)?;
                    if Self::try_insert_add(&store, ec_id, &min_body, &min_meta, &self.store_name)?
                    {
                        return Ok(());
                    }
                    // Key appeared between get() and create — re-read on next iteration.
                    log::debug!(
                        "upsert_partner_id: minimal create raced for '{}', retrying (attempt {}/{})",
                        log_id(ec_id),
                        attempt + 1,
                        MAX_CAS_RETRIES,
                    );
                    // Retry immediately; the bounded retry count prevents an
                    // unbounded loop without blocking the request worker.
                    continue;
                }
            };

            // Reject upserts on withdrawn entries — a late sync must not
            // repopulate partner IDs after consent withdrawal.
            if !entry.consent.ok {
                log::info!(
                    "upsert_partner_id: entry for '{}' is a tombstone, rejecting upsert",
                    log_id(ec_id),
                );
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: format!(
                        "Cannot upsert partner '{partner_id}' for withdrawn key '{ec_id}'"
                    ),
                }));
            }

            if entry
                .ids
                .get(partner_id)
                .is_some_and(|existing| existing.uid == uid)
            {
                return Ok(());
            }

            // Merge the partner ID.
            entry.ids.insert(
                partner_id.to_owned(),
                super::kv_types::KvPartnerId {
                    uid: uid.to_owned(),
                },
            );

            let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

            match store
                .build_insert()
                .if_generation_match(generation)
                .metadata(&meta_str)
                .time_to_live(ENTRY_TTL)
                .execute(ec_id, body.as_str())
            {
                Ok(()) => return Ok(()),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "upsert_partner_id: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{}'",
                        attempt + 1,
                        log_id(ec_id),
                    );
                    // Loop will re-read on next iteration. Do not sleep here:
                    // blocking sleeps burn edge compute while holding the request worker.
                }
                Err(err) => {
                    return Err(
                        Report::new(err).change_context(TrustedServerError::KvStore {
                            store_name: self.store_name.clone(),
                            message: format!(
                                "Failed to upsert partner '{partner_id}' for key '{ec_id}'"
                            ),
                        }),
                    );
                }
            }
        }

        Err(Report::new(TrustedServerError::KvStore {
            store_name: self.store_name.clone(),
            message: format!(
                "CAS conflict after {MAX_CAS_RETRIES} retries upserting partner '{partner_id}' for '{ec_id}'"
            ),
        }))
    }

    /// Upserts a partner ID only if the KV entry already exists.
    ///
    /// Unlike [`Self::upsert_partner_id`], this method does **not** create
    /// entries for missing keys. Used by the S2S batch sync endpoint where
    /// the KV entry must have been created by the organic EC flow.
    ///
    /// Returns [`UpsertResult::Unchanged`] when the existing UID already
    /// matches the incoming UID, skipping the write.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store I/O or CAS
    /// exhaustion errors.
    pub fn upsert_partner_id_if_exists(
        &self,
        ec_id: &str,
        partner_id: &str,
        uid: &str,
    ) -> Result<UpsertResult, Report<TrustedServerError>> {
        let store = self.open_store()?;

        for attempt in 0..MAX_CAS_RETRIES {
            let (mut entry, generation) = match self.get(ec_id)? {
                Some(pair) => pair,
                None => return Ok(UpsertResult::NotFound),
            };

            if !entry.consent.ok {
                return Ok(UpsertResult::ConsentWithdrawn);
            }

            if entry
                .ids
                .get(partner_id)
                .is_some_and(|existing| existing.uid == uid)
            {
                return Ok(UpsertResult::Unchanged);
            }

            entry.ids.insert(
                partner_id.to_owned(),
                super::kv_types::KvPartnerId {
                    uid: uid.to_owned(),
                },
            );

            let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

            match store
                .build_insert()
                .if_generation_match(generation)
                .metadata(&meta_str)
                .time_to_live(ENTRY_TTL)
                .execute(ec_id, body.as_str())
            {
                Ok(()) => return Ok(UpsertResult::Written),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "upsert_partner_id_if_exists: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{}'",
                        attempt + 1,
                        log_id(ec_id),
                    );
                    // Retry immediately; sleeping here blocks the edge worker.
                }
                Err(err) => {
                    return Err(
                        Report::new(err).change_context(TrustedServerError::KvStore {
                            store_name: self.store_name.clone(),
                            message: format!(
                                "Failed to upsert partner '{partner_id}' for key '{ec_id}'"
                            ),
                        }),
                    );
                }
            }
        }

        Err(Report::new(TrustedServerError::KvStore {
            store_name: self.store_name.clone(),
            message: format!(
                "CAS conflict after {MAX_CAS_RETRIES} retries upserting partner '{partner_id}' for '{ec_id}'"
            ),
        }))
    }

    /// Writes a withdrawal tombstone for consent enforcement.
    ///
    /// Overwrites the entry with `consent.ok = false`, empty partner IDs,
    /// and a 24-hour TTL. Uses unconditional overwrite (no CAS) since the
    /// entry is being withdrawn regardless of concurrent state.
    ///
    /// The tombstone preserves consent enforcement for batch sync clients
    /// (`POST /_ts/api/v1/batch-sync`) during the 24-hour revocation window.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error. Callers on
    /// the browser path should log at `error` level and continue — cookie
    /// deletion is the primary enforcement mechanism.
    pub fn write_withdrawal_tombstone(
        &self,
        ec_id: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        let entry = KvEntry::tombstone(current_timestamp());
        let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

        store
            .build_insert()
            .metadata(&meta_str)
            .time_to_live(TOMBSTONE_TTL)
            .execute(ec_id, body)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to write tombstone for key '{ec_id}'"),
            })
    }

    /// Counts the number of keys sharing the same EC hash prefix.
    ///
    /// Uses the Fastly KV list API with a prefix filter, limited to
    /// [`CLUSTER_LIST_LIMIT`] keys. If the limit is reached, the count
    /// is capped — the exact number beyond the limit is not meaningful
    /// for disambiguation.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error.
    pub fn count_hash_prefix_keys(
        &self,
        hash_prefix: &str,
    ) -> Result<u32, Report<TrustedServerError>> {
        let store = self.open_store()?;

        // Request a single page of up to CLUSTER_LIST_LIMIT keys.
        // The prefix ensures we only match EC IDs derived from the same
        // IP+passphrase (i.e. same 64-hex hash).
        let page = store
            .build_list()
            .prefix(hash_prefix)
            .limit(CLUSTER_LIST_LIMIT)
            .execute()
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!(
                    "Failed to list keys with prefix '{}'",
                    &hash_prefix[..hash_prefix.len().min(8)],
                ),
            })?;

        #[allow(clippy::cast_possible_truncation)]
        let count = page.keys().len() as u32;
        Ok(count)
    }

    /// Evaluates the cluster size for an EC entry.
    ///
    /// Returns the stored `cluster_size` when it has already been evaluated.
    /// Otherwise, counts the number of keys sharing the same hash prefix via
    /// [`count_hash_prefix_keys`](Self::count_hash_prefix_keys) and writes the
    /// result back to the entry. The CAS write is best-effort — on conflict,
    /// the computed value is still returned.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store or list failure.
    pub fn evaluate_cluster(
        &self,
        ec_id: &str,
        entry: &KvEntry,
        generation: u64,
    ) -> Result<Option<u32>, Report<TrustedServerError>> {
        if let Some(cluster_size) = entry
            .network
            .as_ref()
            .and_then(|network| network.cluster_size)
        {
            log::trace!("evaluate_cluster: using stored cluster_size");
            return Ok(Some(cluster_size));
        }

        // Compute cluster size via prefix list.
        let hash_prefix = ec_hash(ec_id);
        let cluster_size = self.count_hash_prefix_keys(hash_prefix)?;

        log::debug!(
            "evaluate_cluster: computed cluster_size={cluster_size} for '{}'",
            log_id(ec_id)
        );

        // Best-effort CAS write-back — update the network field.
        let mut updated_entry = entry.clone();
        updated_entry.network = Some(KvNetwork {
            cluster_size: Some(cluster_size),
        });

        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(&updated_entry, &self.store_name)?;

        match store
            .build_insert()
            .if_generation_match(generation)
            .metadata(&meta_str)
            .time_to_live(ENTRY_TTL)
            .execute(ec_id, body.as_str())
        {
            Ok(()) => {}
            Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                log::debug!(
                    "evaluate_cluster: CAS conflict writing cluster_size for '{}', \
                     returning computed value anyway",
                    log_id(ec_id),
                );
            }
            Err(err) => {
                // Log but don't fail — the computed value is still valid.
                log::warn!(
                    "evaluate_cluster: failed to write cluster_size for '{}': {err}",
                    log_id(ec_id)
                );
            }
        }

        Ok(Some(cluster_size))
    }

    /// Hard-deletes the entry.
    ///
    /// Reserved for the IAB data deletion framework (deferred). For consent
    /// withdrawal, use [`write_withdrawal_tombstone`](Self::write_withdrawal_tombstone).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error.
    pub fn delete(&self, ec_id: &str) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        store
            .delete(ec_id)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to delete key '{ec_id}'"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(MAX_CAS_RETRIES, 5);
        assert_eq!(ENTRY_TTL, Duration::from_secs(31_536_000));
        assert_eq!(TOMBSTONE_TTL, Duration::from_secs(86_400));
        assert_eq!(CLUSTER_LIST_LIMIT, 100);
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

    #[test]
    fn deserialize_entry_rejects_invalid_legacy_values() {
        let mut entry = KvEntry::tombstone(1000);
        entry.ids.insert(
            "ssp_x".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "x".repeat(crate::ec::kv_types::MAX_UID_LENGTH + 1),
            },
        );
        let body = serde_json::to_vec(&entry).expect("should serialize invalid entry payload");

        let err = KvIdentityGraph::deserialize_entry("test-store", "ec-id", &body)
            .expect_err("should reject invalid legacy entry values");
        let err_text = format!("{err}");
        assert!(
            err_text.contains("Loaded invalid entry"),
            "should report validation failure for loaded entries"
        );
    }

    #[test]
    fn deserialize_entry_rejects_unsupported_schema_version() {
        let mut entry = KvEntry::tombstone(1000);
        entry.v = crate::ec::kv_types::SCHEMA_VERSION + 1;
        let body = serde_json::to_vec(&entry).expect("should serialize future-version entry");

        let err = KvIdentityGraph::deserialize_entry("test-store", "ec-id", &body)
            .expect_err("should reject unsupported schema versions");
        let err_text = format!("{err}");
        assert!(
            err_text.contains("unsupported KV entry schema version"),
            "should surface schema version validation failures on load"
        );
    }

    #[test]
    fn serialize_entry_rejects_invalid_values() {
        let mut entry = KvEntry::tombstone(1000);
        entry.ids.insert(
            "ssp_x".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "x".repeat(crate::ec::kv_types::MAX_UID_LENGTH + 1),
            },
        );

        let err = KvIdentityGraph::serialize_entry(&entry, "test-store")
            .expect_err("should reject invalid entries before writing");
        let err_text = format!("{err}");
        assert!(
            err_text.contains("Refusing to serialize invalid KV entry"),
            "should fail closed before serializing invalid KV writes"
        );
    }

    #[test]
    fn evaluate_cluster_returns_stored_value_without_store_io() {
        let kv = KvIdentityGraph::new("nonexistent_store_for_cluster_cache_test");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut entry = KvEntry::tombstone(1000);
        entry.network = Some(KvNetwork {
            cluster_size: Some(5),
        });

        let cluster_size = kv
            .evaluate_cluster(&ec_id, &entry, 0)
            .expect("should not touch store when cluster_size is already known");

        assert_eq!(
            cluster_size,
            Some(5),
            "should return stored cluster_size without re-listing keys"
        );
    }
}
