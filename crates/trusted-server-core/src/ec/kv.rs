//! KV identity graph operations.
//!
//! This module provides [`KvIdentityGraph`] which wraps a Fastly KV Store
//! and implements the read-modify-write operations for the EC identity graph.
//!
//! All methods return `Result` — callers decide whether to swallow errors
//! (organic request paths) or propagate them (sync endpoints). See the
//! per-operation error handling policy in the spec §7.5.

use std::collections::HashMap;
use std::time::Duration;

use error_stack::{Report, ResultExt};
use fastly::kv_store::{InsertMode, KVStore};

use crate::error::TrustedServerError;

use super::current_timestamp;
use super::generation::ec_hash;
use super::kv_types::{
    KvDomainVisit, KvEntry, KvMetadata, KvNetwork, KvPubProperties, MAX_SEEN_DOMAINS,
};

/// Maximum number of CAS retry attempts before giving up.
const MAX_CAS_RETRIES: u32 = 3;

/// Minimum interval (seconds) between `last_seen` KV writes for the same key.
/// Prevents write thrashing under bursty traffic — Fastly KV enforces a
/// 1 write/sec limit per key.
const LAST_SEEN_DEBOUNCE_SECS: u64 = 300;

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
    /// The request timestamp is not newer than the stored `synced` value,
    /// so the write was skipped. Counted as accepted by the batch endpoint.
    Stale,
}

/// Truncates an EC ID for safe inclusion in log messages.
///
/// Returns the first 8 characters followed by `…` to aid debugging without
/// writing the full user identifier to logs (satisfies the `CodeQL`
/// "cleartext logging of sensitive information" rule).
fn log_id(ec_id: &str) -> &str {
    ec_id.get(..8).unwrap_or(ec_id)
}

/// Wraps a Fastly KV Store for EC identity graph operations.
///
/// Each EC ID (`{64hex}.{6alnum}`) maps to a JSON-encoded [`KvEntry`]
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
        let entry: KvEntry =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to deserialize entry for key '{ec_id}'"),
            })?;

        Ok(Some((entry, generation)))
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
                "create_or_revive: live entry exists for '{}…', no-op",
                log_id(ec_id)
            );
            return Ok(());
        }

        // Tombstone — CAS overwrite to revive.
        log::info!(
            "create_or_revive: reviving tombstone for '{}…'",
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
                        "create_or_revive: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{ec_id}'",
                        attempt + 1,
                    );
                    // Re-read to get fresh generation.
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
        synced: u64,
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
                        "upsert_partner_id: no entry for '{}…', creating minimal entry",
                        log_id(ec_id)
                    );
                    let minimal = KvEntry::minimal(partner_id, uid, synced);
                    let (min_body, min_meta) = Self::serialize_entry(&minimal, &self.store_name)?;
                    if Self::try_insert_add(&store, ec_id, &min_body, &min_meta, &self.store_name)?
                    {
                        return Ok(());
                    }
                    // Key appeared between get() and create — re-read on next iteration.
                    log::debug!(
                        "upsert_partner_id: minimal create raced for '{ec_id}', retrying (attempt {}/{})",
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
                    "upsert_partner_id: entry for '{ec_id}' is a tombstone, rejecting upsert"
                );
                return Err(Report::new(TrustedServerError::KvStore {
                    store_name: self.store_name.clone(),
                    message: format!(
                        "Cannot upsert partner '{partner_id}' for withdrawn key '{ec_id}'"
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
                .execute(ec_id, body.as_str())
            {
                Ok(()) => return Ok(()),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "upsert_partner_id: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{ec_id}'",
                        attempt + 1,
                    );
                    // Loop will re-read on next iteration.
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
    /// Returns [`UpsertResult::Stale`] when the stored `synced` timestamp
    /// for this partner is already >= `synced`, skipping the write.
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
        synced: u64,
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
                .execute(ec_id, body.as_str())
            {
                Ok(()) => return Ok(UpsertResult::Written),
                Err(fastly::kv_store::KVStoreError::ItemPreconditionFailed) => {
                    log::debug!(
                        "upsert_partner_id_if_exists: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{ec_id}'",
                        attempt + 1,
                    );
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

    /// Updates the `last_seen` timestamp with a 300-second debounce.
    ///
    /// Also updates the [`KvPubProperties::seen_domains`] entry for the
    /// given `domain`, incrementing visits and updating the `last` timestamp.
    /// New domains are added if the [`MAX_SEEN_DOMAINS`] cap has not been
    /// reached; otherwise the new domain is silently dropped.
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
        ec_id: &str,
        timestamp: u64,
        domain: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let (mut entry, generation) = match self.get(ec_id)? {
            Some(pair) => pair,
            None => {
                log::debug!(
                    "update_last_seen: no entry for '{}…', skipping",
                    log_id(ec_id)
                );
                return Ok(());
            }
        };

        // Skip tombstones — a stale cookie should not extend a 24h tombstone
        // back to 1-year TTL.
        if !entry.consent.ok {
            log::debug!(
                "update_last_seen: entry for '{}…' is a tombstone, skipping",
                log_id(ec_id)
            );
            return Ok(());
        }

        // Guard against stale/out-of-order timestamps.
        if timestamp <= entry.last_seen {
            log::trace!(
                "update_last_seen: stale timestamp for '{ec_id}' (stored={}, incoming={timestamp})",
                entry.last_seen,
            );
            return Ok(());
        }

        // Debounce: skip if the stored value is recent enough.
        if timestamp - entry.last_seen < LAST_SEEN_DEBOUNCE_SECS {
            log::trace!(
                "update_last_seen: debounced for '{ec_id}' (delta={}s)",
                timestamp - entry.last_seen,
            );
            return Ok(());
        }

        entry.last_seen = timestamp;

        // Update publisher domain visit history.
        // When `pub_properties` is `None` (entry created before this field
        // was added), backfill it with the current domain as origin.
        match entry.pub_properties {
            Some(ref mut props) => match props.seen_domains.get_mut(domain) {
                Some(visit) => {
                    visit.last = timestamp;
                    visit.visits = visit.visits.saturating_add(1);
                }
                None => {
                    if props.seen_domains.len() < MAX_SEEN_DOMAINS {
                        props.seen_domains.insert(
                            domain.to_owned(),
                            KvDomainVisit {
                                first: timestamp,
                                last: timestamp,
                                visits: 1,
                            },
                        );
                    } else {
                        log::debug!(
                            "update_last_seen: seen_domains cap ({MAX_SEEN_DOMAINS}) reached \
                             for '{}…', dropping domain '{domain}'",
                            log_id(ec_id),
                        );
                    }
                }
            },
            None => {
                let mut seen_domains = HashMap::new();
                seen_domains.insert(
                    domain.to_owned(),
                    KvDomainVisit {
                        first: timestamp,
                        last: timestamp,
                        visits: 1,
                    },
                );
                entry.pub_properties = Some(KvPubProperties {
                    origin_domain: domain.to_owned(),
                    seen_domains,
                });
            }
        }

        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(&entry, &self.store_name)?;

        store
            .build_insert()
            .if_generation_match(generation)
            .metadata(&meta_str)
            .time_to_live(ENTRY_TTL)
            .execute(ec_id, body)
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("Failed to update last_seen for key '{ec_id}'"),
            })
    }

    /// Writes a withdrawal tombstone for consent enforcement.
    ///
    /// Overwrites the entry with `consent.ok = false`, empty partner IDs,
    /// and a 24-hour TTL. Uses unconditional overwrite (no CAS) since the
    /// entry is being withdrawn regardless of concurrent state.
    ///
    /// The tombstone allows batch sync clients (`POST /_ts/api/v1/batch-sync`) to
    /// distinguish `consent_withdrawn` from `ec_id_not_found` for 24 hours.
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

    /// Evaluates and caches the cluster size for an EC entry.
    ///
    /// If the entry already has a `cluster_checked` timestamp within
    /// `recheck_secs` of now, the cached `cluster_size` is returned
    /// without performing a list API call.
    ///
    /// Otherwise, counts the number of keys sharing the same hash prefix
    /// via [`count_hash_prefix_keys`](Self::count_hash_prefix_keys) and
    /// writes the result back to the entry via CAS. The CAS write is
    /// best-effort — on conflict, the computed value is still returned;
    /// it will simply be re-evaluated on the next call.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store or list failure.
    pub fn evaluate_cluster(
        &self,
        ec_id: &str,
        entry: &KvEntry,
        generation: u64,
        recheck_secs: u64,
    ) -> Result<Option<u32>, Report<TrustedServerError>> {
        let now = current_timestamp();

        // Check TTL — skip re-evaluation if the cached value is fresh enough.
        if let Some(ref network) = entry.network {
            if let Some(checked) = network.cluster_checked {
                if now.saturating_sub(checked) < recheck_secs {
                    log::trace!(
                        "evaluate_cluster: cached cluster_size for '{}…' \
                         (age={}s, ttl={recheck_secs}s)",
                        log_id(ec_id),
                        now.saturating_sub(checked),
                    );
                    return Ok(network.cluster_size);
                }
            }
        }

        // Compute cluster size via prefix list.
        let hash_prefix = ec_hash(ec_id);
        let cluster_size = self.count_hash_prefix_keys(hash_prefix)?;

        log::debug!(
            "evaluate_cluster: computed cluster_size={cluster_size} for '{}…'",
            log_id(ec_id)
        );

        // Best-effort CAS write-back — update the network field.
        let mut updated_entry = entry.clone();
        updated_entry.network = Some(KvNetwork {
            cluster_size: Some(cluster_size),
            cluster_checked: Some(now),
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
                    "evaluate_cluster: CAS conflict writing cluster_size for '{ec_id}', \
                     returning computed value anyway"
                );
            }
            Err(err) => {
                // Log but don't fail — the computed value is still valid.
                log::warn!(
                    "evaluate_cluster: failed to write cluster_size for '{}…': {err}",
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
        assert_eq!(MAX_CAS_RETRIES, 3);
        assert_eq!(LAST_SEEN_DEBOUNCE_SECS, 300);
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
    fn evaluate_cluster_returns_cached_value_when_ttl_fresh() {
        // When the entry has a recent cluster_checked timestamp, evaluate_cluster
        // should return the cached value without touching the KV store.
        // We verify this by using a non-existent store — if it tried to open
        // the store, it would fail. The TTL-skip path exits before any store I/O.
        let kv = KvIdentityGraph::new("nonexistent_store_for_ttl_test");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let now = current_timestamp();

        let mut entry = KvEntry::tombstone(now);
        entry.consent.ok = true;
        entry.network = Some(KvNetwork {
            cluster_size: Some(5),
            cluster_checked: Some(now), // just checked
        });

        // With a 3600s TTL, the cached value (checked just now) should be returned.
        let result = kv.evaluate_cluster(&ec_id, &entry, 0, 3600);
        let cluster_size = result.expect("should return cached value without store I/O");
        assert_eq!(
            cluster_size,
            Some(5),
            "should return cached cluster_size when within TTL"
        );
    }

    #[test]
    fn evaluate_cluster_requires_recheck_when_ttl_expired() {
        // When cluster_checked is older than the TTL, evaluate_cluster must
        // perform a list API call. Since the store doesn't exist, this should
        // fail — confirming it didn't take the cached path.
        let kv = KvIdentityGraph::new("nonexistent_store_for_ttl_test");
        let ec_id = format!("{}.ABC123", "a".repeat(64));

        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.network = Some(KvNetwork {
            cluster_size: Some(5),
            cluster_checked: Some(1000), // very old timestamp
        });

        // With a 3600s TTL, the old timestamp should trigger re-evaluation,
        // which will fail because the store doesn't exist.
        let result = kv.evaluate_cluster(&ec_id, &entry, 0, 3600);
        assert!(
            result.is_err(),
            "should attempt store I/O when TTL expired, failing on missing store"
        );
    }
}
