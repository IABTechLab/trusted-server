//! KV identity graph operations.
//!
//! This module provides [`KvIdentityGraph`] which implements the
//! read-modify-write operations for the EC identity graph on top of the
//! platform-neutral [`EcKvStore`] primitives. The platform adapter supplies
//! the concrete store backend (e.g. the Fastly KV Store implementation in
//! `trusted-server-adapter-fastly`).
//!
//! All methods return `Result` — callers decide whether to swallow errors
//! (organic request paths) or propagate them (sync endpoints). See the
//! per-operation error handling policy in the spec §7.5.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use error_stack::{Report, ResultExt};

use crate::error::TrustedServerError;

use super::current_timestamp;
use super::generation::ec_hash;
use super::kv_backend::{EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use super::kv_types::{KvEntry, KvMetadata, KvNetwork};
use super::{EcKvSnapshot, log_id};

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
/// Like [`KvIdentityGraph::upsert_partner_id`], this method fails closed when
/// the root entry is missing. This enum encodes the per-mapping rejection
/// reasons needed by the S2S batch sync endpoint.
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

/// Outcome of atomically creating an identity-graph root when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateIfAbsentOutcome {
    /// The candidate entry was persisted.
    Written,
    /// A row already exists for the candidate key.
    AlreadyExists,
}

/// Partner UID update to apply to a KV identity graph entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PartnerIdUpdate {
    /// Partner namespace key in [`KvEntry::ids`].
    pub(crate) partner_id: String,
    /// Partner-scoped user ID value.
    pub(crate) uid: String,
}

impl PartnerIdUpdate {
    /// Creates a partner UID update.
    pub(crate) fn new(partner_id: impl Into<String>, uid: impl Into<String>) -> Self {
        Self {
            partner_id: partner_id.into(),
            uid: uid.into(),
        }
    }
}

pub(crate) fn apply_partner_id_updates(entry: &mut KvEntry, updates: &[PartnerIdUpdate]) -> bool {
    let mut latest_updates = BTreeMap::new();
    for update in updates {
        latest_updates.insert(update.partner_id.as_str(), update.uid.as_str());
    }

    let mut changed = false;
    for (partner_id, uid) in latest_updates {
        if entry
            .ids
            .get(partner_id)
            .is_some_and(|existing| existing.uid == uid)
        {
            continue;
        }

        entry.ids.insert(
            partner_id.to_owned(),
            super::kv_types::KvPartnerId {
                uid: uid.to_owned(),
            },
        );
        changed = true;
    }

    changed
}

/// EC identity graph on top of the platform KV store primitives.
///
/// Each EC ID (`{64hex}.{6alnum}`) maps to a JSON-encoded [`KvEntry`]
/// containing consent state, geo location, and accumulated partner IDs.
///
/// Methods use optimistic concurrency (generation markers) for safe
/// read-modify-write operations on concurrent requests.
#[derive(Clone)]
pub struct KvIdentityGraph {
    store: Arc<dyn EcKvStore>,
}

impl fmt::Debug for KvIdentityGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KvIdentityGraph")
            .field("store_name", &self.store.store_name())
            .finish()
    }
}

impl KvIdentityGraph {
    /// Creates a new identity graph backed by the given store primitives.
    #[must_use]
    pub fn new(store: impl EcKvStore + 'static) -> Self {
        Self {
            store: Arc::new(store),
        }
    }

    /// Returns the configured store name.
    #[must_use]
    pub fn store_name(&self) -> &str {
        self.store.store_name()
    }

    fn kv_error(&self, message: String) -> Report<TrustedServerError> {
        Report::new(TrustedServerError::KvStore {
            store_name: self.store_name().to_owned(),
            message,
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
        let Some(lookup) = self.store.lookup(ec_id)? else {
            return Ok(None);
        };

        let entry = Self::deserialize_entry(self.store_name(), ec_id, &lookup.body)?;
        Ok(Some((entry, lookup.generation)))
    }

    /// Loads one request-scoped snapshot, preserving miss versus failure at the caller boundary.
    #[must_use]
    pub fn load_snapshot(&self, ec_id: &str) -> EcKvSnapshot {
        match self.get(ec_id) {
            Ok(Some((entry, generation))) => EcKvSnapshot::Present {
                ec_id: ec_id.to_owned(),
                entry: Box::new(entry),
                generation: Some(generation),
            },
            Ok(None) => EcKvSnapshot::Missing {
                ec_id: ec_id.to_owned(),
            },
            Err(err) => {
                log::warn!(
                    "EC KV snapshot read failed for '{}': {err:?}",
                    log_id(ec_id)
                );
                EcKvSnapshot::Failed {
                    ec_id: ec_id.to_owned(),
                }
            }
        }
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

    /// Reads only the metadata for an EC ID key.
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
        let Some(lookup) = self.store.lookup(ec_id)? else {
            return Ok(None);
        };

        let Some(meta_bytes) = lookup.metadata else {
            return Ok(None);
        };

        let meta: KvMetadata =
            serde_json::from_slice(&meta_bytes).change_context(TrustedServerError::KvStore {
                store_name: self.store_name().to_owned(),
                message: format!("Failed to deserialize metadata for key '{ec_id}'"),
            })?;

        Ok(Some(meta))
    }

    /// Creates a new entry. Fails if the key already exists.
    ///
    /// Uses [`EcKvWriteMode::Add`] so concurrent creates for the same EC ID
    /// are safely rejected (only one wins).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error or if the
    /// key already exists.
    pub fn create(&self, ec_id: &str, entry: &KvEntry) -> Result<(), Report<TrustedServerError>> {
        let (body, meta_str) = Self::serialize_entry(entry, self.store_name())?;
        match self.write_entry(ec_id, &body, &meta_str, ENTRY_TTL, EcKvWriteMode::Add)? {
            EcKvWriteOutcome::Written => Ok(()),
            EcKvWriteOutcome::PreconditionFailed => {
                Err(self.kv_error(format!("Key '{ec_id}' already exists")))
            }
        }
    }

    /// Atomically creates an entry while preserving collision as normal control flow.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] when serialization or store I/O fails.
    pub fn create_if_absent(
        &self,
        ec_id: &str,
        entry: &KvEntry,
    ) -> Result<CreateIfAbsentOutcome, Report<TrustedServerError>> {
        let (body, meta_str) = Self::serialize_entry(entry, self.store_name())?;
        match self.write_entry(ec_id, &body, &meta_str, ENTRY_TTL, EcKvWriteMode::Add)? {
            EcKvWriteOutcome::Written => Ok(CreateIfAbsentOutcome::Written),
            EcKvWriteOutcome::PreconditionFailed => Ok(CreateIfAbsentOutcome::AlreadyExists),
        }
    }

    /// Low-level write with shared error context.
    fn write_entry(
        &self,
        ec_id: &str,
        body: &str,
        meta_str: &str,
        ttl: Duration,
        mode: EcKvWriteMode,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        self.store.insert(
            ec_id,
            EcKvWrite {
                body,
                metadata: meta_str,
                ttl,
                mode,
            },
        )
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
        let (body, meta_str) = Self::serialize_entry(entry, self.store_name())?;

        // Try create first — fast path for new entries.
        if self.write_entry(ec_id, &body, &meta_str, ENTRY_TTL, EcKvWriteMode::Add)?
            == EcKvWriteOutcome::Written
        {
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
            match self.write_entry(
                ec_id,
                &body,
                &meta_str,
                ENTRY_TTL,
                EcKvWriteMode::IfGenerationMatch(current_gen),
            )? {
                EcKvWriteOutcome::Written => return Ok(()),
                EcKvWriteOutcome::PreconditionFailed => {
                    log::debug!(
                        "create_or_revive: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{}'",
                        attempt + 1,
                        log_id(ec_id),
                    );
                    // Re-read immediately to get a fresh generation. Sleeping in
                    // the CAS loop would block the edge compute request worker.
                    match self.get(ec_id)? {
                        Some((refreshed, generation)) => {
                            if refreshed.consent.ok {
                                // Someone else revived it — done.
                                return Ok(());
                            }
                            current_gen = generation;
                        }
                        None => return self.create(ec_id, entry),
                    }
                }
            }
        }

        Err(self.kv_error(format!(
            "CAS conflict after {MAX_CAS_RETRIES} retries reviving tombstone for '{ec_id}'"
        )))
    }

    /// Atomically merges multiple partner IDs into the existing entry.
    ///
    /// Uses one read-modify-write operation for all updates so request-local
    /// EID cookie ingestion does not perform a KV read per matched partner.
    /// Duplicate partner IDs are collapsed with the last value winning.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store error, missing root
    /// entry, withdrawn root entry, or CAS exhaustion after
    /// [`MAX_CAS_RETRIES`] attempts.
    pub(crate) fn upsert_partner_ids(
        &self,
        ec_id: &str,
        updates: &[PartnerIdUpdate],
    ) -> Result<(), Report<TrustedServerError>> {
        if updates.is_empty() {
            return Ok(());
        }

        for attempt in 0..MAX_CAS_RETRIES {
            let (mut entry, generation) = match self.get(ec_id)? {
                Some(pair) => pair,
                None => {
                    log::info!(
                        "upsert_partner_ids: no entry for '{}', rejecting {} partner updates",
                        log_id(ec_id),
                        updates.len(),
                    );
                    return Err(self.kv_error(format!(
                        "Cannot upsert {} partner IDs for missing key '{ec_id}'",
                        updates.len(),
                    )));
                }
            };

            // Reject upserts on withdrawn entries — a late sync must not
            // repopulate partner IDs after consent withdrawal.
            if !entry.consent.ok {
                log::info!(
                    "upsert_partner_ids: entry for '{}' is a tombstone, rejecting {} partner updates",
                    log_id(ec_id),
                    updates.len(),
                );
                return Err(self.kv_error(format!(
                    "Cannot upsert {} partner IDs for withdrawn key '{ec_id}'",
                    updates.len(),
                )));
            }

            if !apply_partner_id_updates(&mut entry, updates) {
                return Ok(());
            }

            let (body, meta_str) = Self::serialize_entry(&entry, self.store_name())?;

            match self.write_entry(
                ec_id,
                &body,
                &meta_str,
                ENTRY_TTL,
                EcKvWriteMode::IfGenerationMatch(generation),
            )? {
                EcKvWriteOutcome::Written => return Ok(()),
                EcKvWriteOutcome::PreconditionFailed => {
                    log::debug!(
                        "upsert_partner_ids: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{}'",
                        attempt + 1,
                        log_id(ec_id),
                    );
                    // Retry immediately; sleeping here blocks the edge worker.
                }
            }
        }

        Err(self.kv_error(format!(
            "CAS conflict after {MAX_CAS_RETRIES} retries upserting {} partner IDs for '{ec_id}'",
            updates.len(),
        )))
    }

    /// Merges partner IDs using request-scoped persisted state as the first CAS input.
    pub(crate) fn upsert_partner_ids_from_snapshot(
        &self,
        ec_id: &str,
        updates: &[PartnerIdUpdate],
        snapshot: EcKvSnapshot,
    ) -> EcKvSnapshot {
        if updates.is_empty() {
            return snapshot;
        }

        // Resolve the initial usable snapshot without spending a CAS attempt. A
        // not-read, generation-unavailable, or foreign-ID snapshot is refreshed
        // once; an authoritative miss or failure for this EC ID is returned
        // as-is (the hot path never retries a failed lookup). This keeps all
        // `MAX_CAS_RETRIES` iterations available for actual writes.
        let mut current = match snapshot {
            EcKvSnapshot::Present {
                ec_id: ref snapshot_id,
                generation: Some(_),
                ..
            } if snapshot_id == ec_id => snapshot,
            EcKvSnapshot::Missing {
                ec_id: ref snapshot_id,
            }
            | EcKvSnapshot::Failed {
                ec_id: ref snapshot_id,
            } if snapshot_id == ec_id => return snapshot,
            _ => self.load_snapshot(ec_id),
        };

        for _attempt in 0..MAX_CAS_RETRIES {
            let (mut entry, generation) = match current {
                EcKvSnapshot::Present {
                    ec_id: ref snapshot_id,
                    ref entry,
                    generation: Some(generation),
                } if snapshot_id == ec_id => (entry.as_ref().clone(), generation),
                // A refreshed read that is absent or unreadable is authoritative
                // for this write: never create or overwrite a missing root.
                EcKvSnapshot::Missing { .. } | EcKvSnapshot::Failed { .. } => return current,
                // `load_snapshot` never yields `NotRead` or a generation-less
                // `Present`; fail closed if that invariant is ever violated.
                EcKvSnapshot::Present { .. } | EcKvSnapshot::NotRead => {
                    return EcKvSnapshot::Failed {
                        ec_id: ec_id.to_owned(),
                    };
                }
            };

            if !entry.consent.ok {
                return current;
            }
            if !apply_partner_id_updates(&mut entry, updates) {
                return current;
            }
            let Ok((body, meta_str)) = Self::serialize_entry(&entry, self.store_name()) else {
                return EcKvSnapshot::Failed {
                    ec_id: ec_id.to_owned(),
                };
            };
            match self.write_entry(
                ec_id,
                &body,
                &meta_str,
                ENTRY_TTL,
                EcKvWriteMode::IfGenerationMatch(generation),
            ) {
                Ok(EcKvWriteOutcome::Written) => {
                    return EcKvSnapshot::Present {
                        ec_id: ec_id.to_owned(),
                        entry: Box::new(entry),
                        generation: None,
                    };
                }
                Ok(EcKvWriteOutcome::PreconditionFailed) => {
                    current = self.load_snapshot(ec_id);
                }
                Err(err) => {
                    log::warn!(
                        "snapshot partner upsert failed for '{}': {err:?}",
                        log_id(ec_id)
                    );
                    return EcKvSnapshot::Failed {
                        ec_id: ec_id.to_owned(),
                    };
                }
            }
        }

        EcKvSnapshot::Failed {
            ec_id: ec_id.to_owned(),
        }
    }

    /// Atomically merges a partner ID into the existing entry.
    ///
    /// Uses CAS (generation markers) to avoid clobbering concurrent writes
    /// from other partners. Retries up to [`MAX_CAS_RETRIES`] on conflict.
    ///
    /// If the root entry does not exist, returns an error. This method
    /// intentionally fails closed to prevent phantom identity entries.
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
        for attempt in 0..MAX_CAS_RETRIES {
            let (mut entry, generation) = match self.get(ec_id)? {
                Some(pair) => pair,
                None => {
                    log::info!(
                        "upsert_partner_id: no entry for '{}', rejecting partner upsert",
                        log_id(ec_id)
                    );
                    return Err(self.kv_error(format!(
                        "Cannot upsert partner '{partner_id}' for missing key '{ec_id}'"
                    )));
                }
            };

            // Reject upserts on withdrawn entries — a late sync must not
            // repopulate partner IDs after consent withdrawal.
            if !entry.consent.ok {
                log::info!(
                    "upsert_partner_id: entry for '{}' is a tombstone, rejecting upsert",
                    log_id(ec_id),
                );
                return Err(self.kv_error(format!(
                    "Cannot upsert partner '{partner_id}' for withdrawn key '{ec_id}'"
                )));
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

            let (body, meta_str) = Self::serialize_entry(&entry, self.store_name())?;

            match self.write_entry(
                ec_id,
                &body,
                &meta_str,
                ENTRY_TTL,
                EcKvWriteMode::IfGenerationMatch(generation),
            )? {
                EcKvWriteOutcome::Written => return Ok(()),
                EcKvWriteOutcome::PreconditionFailed => {
                    log::debug!(
                        "upsert_partner_id: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{}'",
                        attempt + 1,
                        log_id(ec_id),
                    );
                    // Loop will re-read on next iteration. Do not sleep here:
                    // blocking sleeps burn edge compute while holding the request worker.
                }
            }
        }

        Err(self.kv_error(format!(
            "CAS conflict after {MAX_CAS_RETRIES} retries upserting partner '{partner_id}' for '{ec_id}'"
        )))
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

            let (body, meta_str) = Self::serialize_entry(&entry, self.store_name())?;

            match self.write_entry(
                ec_id,
                &body,
                &meta_str,
                ENTRY_TTL,
                EcKvWriteMode::IfGenerationMatch(generation),
            )? {
                EcKvWriteOutcome::Written => return Ok(UpsertResult::Written),
                EcKvWriteOutcome::PreconditionFailed => {
                    log::debug!(
                        "upsert_partner_id_if_exists: CAS conflict on attempt {}/{MAX_CAS_RETRIES} for '{}'",
                        attempt + 1,
                        log_id(ec_id),
                    );
                    // Retry immediately; sleeping here blocks the edge worker.
                }
            }
        }

        Err(self.kv_error(format!(
            "CAS conflict after {MAX_CAS_RETRIES} retries upserting partner '{partner_id}' for '{ec_id}'"
        )))
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
        let entry = KvEntry::tombstone(current_timestamp());
        let (body, meta_str) = Self::serialize_entry(&entry, self.store_name())?;

        match self.write_entry(
            ec_id,
            &body,
            &meta_str,
            TOMBSTONE_TTL,
            EcKvWriteMode::Overwrite,
        ) {
            Ok(_) => Ok(()),
            Err(report) => Err(report.change_context(TrustedServerError::KvStore {
                store_name: self.store_name().to_owned(),
                message: format!("Failed to write tombstone for key '{ec_id}'"),
            })),
        }
    }

    /// Writes a tombstone only when an authoritative row already exists.
    ///
    /// An authoritative `Missing` snapshot is a no-op (nothing to withdraw). A
    /// non-authoritative snapshot — a prior read that `Failed`, or one lacking a
    /// usable generation — is re-read so a transient read error never silently
    /// drops a consent withdrawal.
    pub(crate) fn tombstone_existing_from_snapshot(
        &self,
        ec_id: &str,
        snapshot: EcKvSnapshot,
    ) -> EcKvSnapshot {
        // Resolve the initial usable snapshot without spending a CAS attempt. An
        // authoritative missing row is a no-op; any non-authoritative state — a
        // failed read or a snapshot lacking a usable generation — is re-read once
        // so a transient error never silently drops a withdrawal, and all
        // `MAX_CAS_RETRIES` iterations stay available for the tombstone write.
        let mut current = match snapshot {
            EcKvSnapshot::Present {
                ec_id: ref snapshot_id,
                generation: Some(_),
                ..
            } if snapshot_id == ec_id => snapshot,
            EcKvSnapshot::Missing {
                ec_id: ref snapshot_id,
            } if snapshot_id == ec_id => return snapshot,
            _ => self.load_snapshot(ec_id),
        };

        for _attempt in 0..MAX_CAS_RETRIES {
            let generation = match current {
                EcKvSnapshot::Present {
                    ec_id: ref snapshot_id,
                    generation: Some(generation),
                    ..
                } if snapshot_id == ec_id => generation,
                // An authoritative missing row (including one that disappeared
                // mid-retry) is a no-op.
                EcKvSnapshot::Missing {
                    ec_id: ref snapshot_id,
                } if snapshot_id == ec_id => return current,
                // A refreshed read that failed (or any other unusable state)
                // fails closed rather than silently dropping the withdrawal.
                _ => {
                    return EcKvSnapshot::Failed {
                        ec_id: ec_id.to_owned(),
                    };
                }
            };
            let tombstone = KvEntry::tombstone(current_timestamp());
            let Ok((body, meta_str)) = Self::serialize_entry(&tombstone, self.store_name()) else {
                return EcKvSnapshot::Failed {
                    ec_id: ec_id.to_owned(),
                };
            };
            match self.write_entry(
                ec_id,
                &body,
                &meta_str,
                TOMBSTONE_TTL,
                EcKvWriteMode::IfGenerationMatch(generation),
            ) {
                Ok(EcKvWriteOutcome::Written) => {
                    return EcKvSnapshot::Present {
                        ec_id: ec_id.to_owned(),
                        entry: Box::new(tombstone),
                        generation: None,
                    };
                }
                Ok(EcKvWriteOutcome::PreconditionFailed) => {
                    current = self.load_snapshot(ec_id);
                }
                Err(err) => {
                    log::warn!(
                        "conditional withdrawal tombstone failed for '{}': {err:?}",
                        log_id(ec_id)
                    );
                    return EcKvSnapshot::Failed {
                        ec_id: ec_id.to_owned(),
                    };
                }
            }
        }
        EcKvSnapshot::Failed {
            ec_id: ec_id.to_owned(),
        }
    }

    /// Counts the number of keys sharing the same EC hash prefix.
    ///
    /// Uses the platform KV list API with a prefix filter, limited to
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
        // The prefix ensures we only match EC IDs derived from the same
        // IP+passphrase (i.e. same 64-hex hash). The backend already attaches
        // store context to list failures, so propagate without re-wrapping.
        self.store
            .count_keys_with_prefix(hash_prefix, CLUSTER_LIST_LIMIT)
    }

    /// Evaluates the cluster size for an EC entry.
    ///
    /// Returns the stored `cluster_size` when it has already been evaluated
    /// for a live entry. Tombstone entries return `None` without store I/O so
    /// their 24-hour withdrawal TTL is not extended. Otherwise, counts the
    /// number of keys sharing the same hash prefix via
    /// [`count_hash_prefix_keys`](Self::count_hash_prefix_keys) and writes the
    /// result back to the entry. The CAS write is best-effort — on conflict
    /// or write failure, the computed value is still returned.
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
        if !entry.consent.ok {
            log::trace!("evaluate_cluster: skipping tombstone entry");
            return Ok(None);
        }

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

        // Best-effort CAS write-back — update only the cluster size so any
        // future `network` fields are preserved across this lazy write.
        let mut updated_entry = entry.clone();
        let mut network = updated_entry
            .network
            .unwrap_or(KvNetwork { cluster_size: None });
        network.cluster_size = Some(cluster_size);
        updated_entry.network = Some(network);

        let (body, meta_str) = Self::serialize_entry(&updated_entry, self.store_name())?;

        match self.write_entry(
            ec_id,
            &body,
            &meta_str,
            ENTRY_TTL,
            EcKvWriteMode::IfGenerationMatch(generation),
        ) {
            Ok(EcKvWriteOutcome::Written) => {}
            Ok(EcKvWriteOutcome::PreconditionFailed) => {
                log::debug!(
                    "evaluate_cluster: CAS conflict writing cluster_size for '{}', \
                     returning computed value anyway",
                    log_id(ec_id),
                );
            }
            Err(report) => {
                // Log but don't fail — the computed value is still valid.
                log::warn!(
                    "evaluate_cluster: failed to write cluster_size for '{}': {report}",
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
        // The backend's delete already attaches store context, so propagate
        // without re-wrapping the same message.
        self.store.delete(ec_id)
    }
}

#[cfg(test)]
impl KvIdentityGraph {
    /// Test helper: a graph whose every store operation fails, mimicking a
    /// missing or unreachable platform store.
    pub(crate) fn failing(store_name: impl Into<String>) -> Self {
        Self::new(super::kv_backend::test_support::FailingEcKv::new(
            store_name,
        ))
    }

    /// Test helper: a graph backed by an in-memory store with generation
    /// tracking.
    pub(crate) fn in_memory(store_name: impl Into<String>) -> Self {
        Self::new(super::kv_backend::test_support::InMemoryEcKv::new(
            store_name,
        ))
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

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }

    // -----------------------------------------------------------------------
    // CAS-conflict injection tests
    // -----------------------------------------------------------------------

    use crate::ec::kv_backend::EcKvLookup;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;

    /// [`EcKvStore`] wrapper that injects generation conflicts: the first
    /// `conflicts_remaining` `IfGenerationMatch` inserts return
    /// [`EcKvWriteOutcome::PreconditionFailed`] without writing, optionally
    /// reviving the underlying entry to simulate a concurrent writer.
    struct ConflictInjectingEcKv {
        inner: InMemoryEcKv,
        conflicts_remaining: std::sync::Mutex<u32>,
        revive_on_conflict: bool,
    }

    impl ConflictInjectingEcKv {
        fn new(conflicts: u32, revive_on_conflict: bool) -> Self {
            Self {
                inner: InMemoryEcKv::new("conflict-store"),
                conflicts_remaining: std::sync::Mutex::new(conflicts),
                revive_on_conflict,
            }
        }

        fn seed_tombstone(&self, ec_id: &str) {
            let (body, meta) = KvIdentityGraph::serialize_entry(
                &KvEntry::tombstone(1000),
                self.inner.store_name(),
            )
            .expect("should serialize tombstone");
            self.inner
                .insert(
                    ec_id,
                    EcKvWrite {
                        body: &body,
                        metadata: &meta,
                        ttl: TOMBSTONE_TTL,
                        mode: EcKvWriteMode::Add,
                    },
                )
                .expect("should seed tombstone");
        }
    }

    impl EcKvStore for ConflictInjectingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key)
        }

        fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            if matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_)) {
                let mut remaining = self
                    .conflicts_remaining
                    .lock()
                    .expect("should lock conflict counter");
                if *remaining > 0 {
                    *remaining -= 1;
                    if self.revive_on_conflict {
                        // Simulate a concurrent writer reviving the entry
                        // between this writer's read and its CAS write.
                        let (body, meta) = KvIdentityGraph::serialize_entry(
                            &live_entry(),
                            self.inner.store_name(),
                        )
                        .expect("should serialize concurrent live entry");
                        self.inner
                            .insert(
                                key,
                                EcKvWrite {
                                    body: &body,
                                    metadata: &meta,
                                    ttl: ENTRY_TTL,
                                    mode: EcKvWriteMode::Overwrite,
                                },
                            )
                            .expect("should apply concurrent revive");
                    }
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
            }
            self.inner.insert(key, write)
        }

        fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            self.inner.count_keys_with_prefix(prefix, limit)
        }

        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key)
        }
    }

    #[test]
    fn create_or_revive_retries_cas_conflict_and_succeeds() {
        let store = ConflictInjectingEcKv::new(2, false);
        store.seed_tombstone("ec-1");
        let graph = KvIdentityGraph::new(store);

        graph
            .create_or_revive("ec-1", &live_entry())
            .expect("should revive after re-reading a fresh generation");

        let (entry, _) = graph
            .get("ec-1")
            .expect("should read entry")
            .expect("entry should exist");
        assert!(
            entry.consent.ok,
            "tombstone should be revived after CAS retries"
        );
    }

    #[test]
    fn create_or_revive_short_circuits_on_concurrent_revive() {
        // Inject more conflicts than MAX_CAS_RETRIES so the only way the call
        // can succeed is the concurrent-revive short-circuit on re-read.
        let store = ConflictInjectingEcKv::new(MAX_CAS_RETRIES + 1, true);
        store.seed_tombstone("ec-2");
        let graph = KvIdentityGraph::new(store);

        graph
            .create_or_revive("ec-2", &live_entry())
            .expect("should return Ok when a concurrent writer already revived the entry");
    }

    #[test]
    fn create_or_revive_errors_after_cas_exhaustion() {
        let store = ConflictInjectingEcKv::new(MAX_CAS_RETRIES + 1, false);
        store.seed_tombstone("ec-3");
        let graph = KvIdentityGraph::new(store);

        let err = graph
            .create_or_revive("ec-3", &live_entry())
            .expect_err("should fail after exhausting CAS retries");
        assert!(
            format!("{err}").contains("CAS conflict after"),
            "should report CAS exhaustion as the terminal error"
        );
    }

    #[test]
    fn apply_partner_id_updates_returns_unchanged_for_empty_updates() {
        let mut entry = live_entry();

        let changed = apply_partner_id_updates(&mut entry, &[]);

        assert!(!changed, "should not change entry for empty updates");
        assert!(entry.ids.is_empty(), "should not add partner IDs");
    }

    #[test]
    fn apply_partner_id_updates_skips_matching_existing_uid() {
        let mut entry = live_entry();
        entry.ids.insert(
            "ssp_x".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "uid-1".to_owned(),
            },
        );
        let updates = vec![PartnerIdUpdate::new("ssp_x", "uid-1")];

        let changed = apply_partner_id_updates(&mut entry, &updates);

        assert!(!changed, "should not change when UID already matches");
        assert_eq!(entry.ids["ssp_x"].uid, "uid-1");
    }

    #[test]
    fn apply_partner_id_updates_inserts_new_partner_uid() {
        let mut entry = live_entry();
        let updates = vec![PartnerIdUpdate::new("ssp_x", "uid-1")];

        let changed = apply_partner_id_updates(&mut entry, &updates);

        assert!(changed, "should report changed entry");
        assert_eq!(entry.ids["ssp_x"].uid, "uid-1");
    }

    #[test]
    fn apply_partner_id_updates_overwrites_different_uid() {
        let mut entry = live_entry();
        entry.ids.insert(
            "ssp_x".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "old-uid".to_owned(),
            },
        );
        let updates = vec![PartnerIdUpdate::new("ssp_x", "new-uid")];

        let changed = apply_partner_id_updates(&mut entry, &updates);

        assert!(changed, "should report changed entry");
        assert_eq!(entry.ids["ssp_x"].uid, "new-uid");
    }

    #[test]
    fn apply_partner_id_updates_applies_multiple_updates() {
        let mut entry = live_entry();
        let updates = vec![
            PartnerIdUpdate::new("ssp_x", "uid-x"),
            PartnerIdUpdate::new("ssp_y", "uid-y"),
        ];

        let changed = apply_partner_id_updates(&mut entry, &updates);

        assert!(changed, "should report changed entry");
        assert_eq!(entry.ids["ssp_x"].uid, "uid-x");
        assert_eq!(entry.ids["ssp_y"].uid, "uid-y");
    }

    #[test]
    fn apply_partner_id_updates_uses_last_duplicate_value() {
        let mut entry = live_entry();
        entry.ids.insert(
            "ssp_x".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "original".to_owned(),
            },
        );
        let updates = vec![
            PartnerIdUpdate::new("ssp_x", "intermediate"),
            PartnerIdUpdate::new("ssp_x", "original"),
        ];

        let changed = apply_partner_id_updates(&mut entry, &updates);

        assert!(
            !changed,
            "should not write when the final duplicate value matches existing state"
        );
        assert_eq!(entry.ids["ssp_x"].uid, "original");
    }

    #[test]
    fn evaluate_cluster_returns_stored_value_without_store_io() {
        let kv = KvIdentityGraph::failing("nonexistent_store_for_cluster_cache_test");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut entry = live_entry();
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

    #[test]
    fn evaluate_cluster_skips_tombstone_without_store_io() {
        let kv = KvIdentityGraph::failing("nonexistent_store_for_tombstone_cluster_test");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let entry = KvEntry::tombstone(1000);

        let cluster_size = kv
            .evaluate_cluster(&ec_id, &entry, 0)
            .expect("should not touch store for tombstone entries");

        assert_eq!(
            cluster_size, None,
            "should not evaluate or write cluster_size for tombstones"
        );
    }

    #[test]
    fn create_then_get_roundtrips_entry() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let entry = live_entry();

        kv.create(&ec_id, &entry).expect("should create new entry");
        let (loaded, generation) = kv
            .get(&ec_id)
            .expect("should read entry back")
            .expect("should find created entry");

        assert!(loaded.consent.ok, "should preserve consent state");
        assert!(generation > 0, "should expose a generation marker");
    }

    #[test]
    fn create_rejects_existing_key() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let entry = live_entry();

        kv.create(&ec_id, &entry).expect("should create new entry");
        let err = kv
            .create(&ec_id, &entry)
            .expect_err("should reject duplicate create");
        assert!(
            format!("{err}").contains("already exists"),
            "should report duplicate key"
        );
    }

    #[test]
    fn create_if_absent_reports_written_and_collision() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));

        assert_eq!(
            kv.create_if_absent(&ec_id, &live_entry())
                .expect("should create absent entry"),
            CreateIfAbsentOutcome::Written
        );
        assert_eq!(
            kv.create_if_absent(&ec_id, &live_entry())
                .expect("should report collision"),
            CreateIfAbsentOutcome::AlreadyExists
        );
    }

    #[test]
    fn create_if_absent_propagates_store_error() {
        let kv = KvIdentityGraph::failing("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));

        assert!(
            kv.create_if_absent(&ec_id, &live_entry()).is_err(),
            "should preserve store failures instead of reporting a collision"
        );
    }

    #[test]
    fn create_or_revive_revives_tombstone() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));

        kv.create(&ec_id, &KvEntry::tombstone(1000))
            .expect("should create tombstone");
        kv.create_or_revive(&ec_id, &live_entry())
            .expect("should revive tombstone");

        let (loaded, _) = kv
            .get(&ec_id)
            .expect("should read entry back")
            .expect("should find revived entry");
        assert!(loaded.consent.ok, "should be live after revive");
    }

    #[test]
    fn upsert_partner_id_if_exists_reports_missing_key() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));

        let result = kv
            .upsert_partner_id_if_exists(&ec_id, "ssp_x", "uid-1")
            .expect("should not error on missing key");
        assert_eq!(result, UpsertResult::NotFound);
    }

    #[test]
    fn upsert_partner_id_if_exists_writes_and_detects_unchanged() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        kv.create(&ec_id, &live_entry()).expect("should create");

        let first = kv
            .upsert_partner_id_if_exists(&ec_id, "ssp_x", "uid-1")
            .expect("should write partner id");
        assert_eq!(first, UpsertResult::Written);

        let second = kv
            .upsert_partner_id_if_exists(&ec_id, "ssp_x", "uid-1")
            .expect("should detect unchanged uid");
        assert_eq!(second, UpsertResult::Unchanged);
    }

    #[test]
    fn upsert_partner_id_if_exists_rejects_tombstone() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        kv.create(&ec_id, &KvEntry::tombstone(1000))
            .expect("should create tombstone");

        let result = kv
            .upsert_partner_id_if_exists(&ec_id, "ssp_x", "uid-1")
            .expect("should not error on tombstone");
        assert_eq!(result, UpsertResult::ConsentWithdrawn);
    }

    #[test]
    fn snapshot_bulk_upsert_returns_persisted_entry_without_stale_generation() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        kv.create(&ec_id, &live_entry()).expect("should create");
        let snapshot = kv.load_snapshot(&ec_id);
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = kv.upsert_partner_ids_from_snapshot(&ec_id, &updates, snapshot);

        let entry = outcome
            .entry_for(&ec_id)
            .expect("should retain persisted entry");
        assert_eq!(
            entry.ids.get("ssp_x").map(|id| id.uid.as_str()),
            Some("uid-1")
        );
        assert_eq!(
            outcome.generation_for(&ec_id),
            None,
            "backend does not return the post-write generation"
        );
    }

    #[test]
    fn snapshot_bulk_upsert_does_not_create_missing_root() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = kv.upsert_partner_ids_from_snapshot(
            &ec_id,
            &updates,
            EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            },
        );

        assert!(matches!(outcome, EcKvSnapshot::Missing { .. }));
        assert!(kv.get(&ec_id).expect("should read store").is_none());
    }

    #[test]
    fn write_withdrawal_tombstone_overwrites_live_entry() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        kv.create(&ec_id, &live_entry()).expect("should create");

        kv.write_withdrawal_tombstone(&ec_id)
            .expect("should write tombstone");

        let (loaded, _) = kv
            .get(&ec_id)
            .expect("should read entry back")
            .expect("should find tombstone entry");
        assert!(!loaded.consent.ok, "should be withdrawn after tombstone");
    }

    #[test]
    fn tombstone_existing_from_snapshot_never_creates_missing_key() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let snapshot = EcKvSnapshot::Missing {
            ec_id: ec_id.clone(),
        };

        let outcome = kv.tombstone_existing_from_snapshot(&ec_id, snapshot);

        assert!(matches!(outcome, EcKvSnapshot::Missing { .. }));
        assert!(
            kv.get(&ec_id).expect("should read store").is_none(),
            "withdrawal must not create a tombstone for an absent key"
        );
    }

    #[test]
    fn tombstone_existing_from_snapshot_uses_existing_generation() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        kv.create(&ec_id, &live_entry()).expect("should create");
        let snapshot = kv.load_snapshot(&ec_id);

        let outcome = kv.tombstone_existing_from_snapshot(&ec_id, snapshot);

        assert!(
            outcome
                .entry_for(&ec_id)
                .is_some_and(|entry| !entry.consent.ok),
            "should return the persisted tombstone"
        );
        let (stored, _) = kv
            .get(&ec_id)
            .expect("should read store")
            .expect("should preserve existing key");
        assert!(!stored.consent.ok, "should persist withdrawal state");
    }

    // -----------------------------------------------------------------------
    // Snapshot-aware mutation stores and tests
    // -----------------------------------------------------------------------

    /// [`EcKvStore`] wrapper that counts `lookup` calls through a shared counter
    /// so tests can prove exactly how many reads a mutation performs.
    struct CountingEcKv {
        inner: InMemoryEcKv,
        lookups: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl CountingEcKv {
        fn new(lookups: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
            Self {
                inner: InMemoryEcKv::new("counting-store"),
                lookups,
            }
        }
    }

    impl EcKvStore for CountingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }
        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.lookups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.lookup(key)
        }
        fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            self.inner.insert(key, write)
        }
        fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            self.inner.count_keys_with_prefix(prefix, limit)
        }
        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key)
        }
    }

    /// [`EcKvStore`] whose reads succeed but every write fails, simulating a
    /// store that becomes unwritable mid-request.
    struct WriteFailingEcKv {
        inner: InMemoryEcKv,
    }

    impl WriteFailingEcKv {
        fn new() -> Self {
            Self {
                inner: InMemoryEcKv::new("write-failing-store"),
            }
        }
    }

    impl EcKvStore for WriteFailingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }
        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key)
        }
        fn insert(
            &self,
            _key: &str,
            _write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            Err(Report::new(TrustedServerError::KvStore {
                store_name: self.inner.store_name().to_owned(),
                message: "write failing test store".to_owned(),
            }))
        }
        fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            self.inner.count_keys_with_prefix(prefix, limit)
        }
        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key)
        }
    }

    /// [`EcKvStore`] wrapper whose first CAS write both fails the precondition
    /// and deletes the key, simulating a concurrent withdrawal that removes the
    /// row between this writer's read and its write.
    struct DisappearOnConflictEcKv {
        inner: InMemoryEcKv,
        conflicts_remaining: std::sync::Mutex<u32>,
    }

    impl DisappearOnConflictEcKv {
        fn new(conflicts: u32) -> Self {
            Self {
                inner: InMemoryEcKv::new("disappear-store"),
                conflicts_remaining: std::sync::Mutex::new(conflicts),
            }
        }
        fn seed_live(&self, ec_id: &str) {
            let (body, meta) =
                KvIdentityGraph::serialize_entry(&live_entry(), self.inner.store_name())
                    .expect("should serialize seeded entry");
            self.inner
                .insert(
                    ec_id,
                    EcKvWrite {
                        body: &body,
                        metadata: &meta,
                        ttl: ENTRY_TTL,
                        mode: EcKvWriteMode::Add,
                    },
                )
                .expect("should seed live entry");
        }
    }

    impl EcKvStore for DisappearOnConflictEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }
        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key)
        }
        fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            if matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_)) {
                let mut remaining = self
                    .conflicts_remaining
                    .lock()
                    .expect("should lock conflict counter");
                if *remaining > 0 {
                    *remaining -= 1;
                    self.inner.delete(key).expect("should delete on conflict");
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
            }
            self.inner.insert(key, write)
        }
        fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            self.inner.count_keys_with_prefix(prefix, limit)
        }
        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key)
        }
    }

    fn snapshot_ec_id() -> String {
        format!("{}.ABC123", "a".repeat(64))
    }

    #[test]
    fn snapshot_upsert_with_generation_writes_without_reading() {
        let lookups = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let graph = KvIdentityGraph::new(CountingEcKv::new(lookups.clone()));
        let ec_id = snapshot_ec_id();
        graph.create(&ec_id, &live_entry()).expect("should seed");
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(live_entry()),
            generation: Some(1),
        };
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = graph.upsert_partner_ids_from_snapshot(&ec_id, &updates, snapshot);

        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a usable generation must avoid the initial read"
        );
        assert_eq!(
            outcome
                .entry_for(&ec_id)
                .and_then(|entry| entry.ids.get("ssp_x"))
                .map(|id| id.uid.as_str()),
            Some("uid-1")
        );
        assert_eq!(outcome.generation_for(&ec_id), None);
    }

    #[test]
    fn snapshot_upsert_unchanged_updates_preserve_generation() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = snapshot_ec_id();
        let mut seeded = live_entry();
        apply_partner_id_updates(&mut seeded, &[PartnerIdUpdate::new("ssp_x", "uid-1")]);
        kv.create(&ec_id, &seeded).expect("should seed");
        let snapshot = kv.load_snapshot(&ec_id);
        assert_eq!(snapshot.generation_for(&ec_id), Some(1));

        let outcome = kv.upsert_partner_ids_from_snapshot(
            &ec_id,
            &[PartnerIdUpdate::new("ssp_x", "uid-1")],
            snapshot,
        );

        assert_eq!(
            outcome.generation_for(&ec_id),
            Some(1),
            "an unchanged merge preserves the usable generation and performs no write"
        );
    }

    #[test]
    fn snapshot_upsert_refreshes_unavailable_generation_exactly_once() {
        let lookups = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let graph = KvIdentityGraph::new(CountingEcKv::new(lookups.clone()));
        let ec_id = snapshot_ec_id();
        graph.create(&ec_id, &live_entry()).expect("should seed");
        // Finalize-written style snapshot: entry known, generation unavailable.
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(live_entry()),
            generation: None,
        };
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = graph.upsert_partner_ids_from_snapshot(&ec_id, &updates, snapshot);

        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "an unavailable generation refreshes exactly once before CAS"
        );
        assert!(
            outcome
                .entry_for(&ec_id)
                .is_some_and(|e| e.ids.contains_key("ssp_x"))
        );
    }

    #[test]
    fn snapshot_upsert_gen_unavailable_survives_four_conflicts_then_writes() {
        // A generation-unavailable snapshot (finalize-written style) refreshes
        // once to obtain a usable generation. That refresh must not consume a
        // CAS attempt, so all five write attempts remain: four conflicts
        // followed by a successful fifth write still persist the update.
        let graph = KvIdentityGraph::new(ConflictInjectingEcKv::new(4, false));
        let ec_id = snapshot_ec_id();
        graph.create(&ec_id, &live_entry()).expect("should seed");
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(live_entry()),
            generation: None,
        };
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = graph.upsert_partner_ids_from_snapshot(&ec_id, &updates, snapshot);

        assert_eq!(
            outcome
                .entry_for(&ec_id)
                .and_then(|entry| entry.ids.get("ssp_x"))
                .map(|id| id.uid.as_str()),
            Some("uid-1"),
            "the fifth CAS attempt must still succeed after a refresh and four conflicts"
        );
    }

    #[test]
    fn snapshot_upsert_cas_conflict_remerges_concurrent_data() {
        let graph = KvIdentityGraph::new(ConflictInjectingEcKv::new(1, true));
        let ec_id = snapshot_ec_id();
        graph.create(&ec_id, &live_entry()).expect("should seed");
        let snapshot = graph.load_snapshot(&ec_id);
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = graph.upsert_partner_ids_from_snapshot(&ec_id, &updates, snapshot);

        let entry = outcome
            .entry_for(&ec_id)
            .expect("should persist re-merged entry");
        assert_eq!(
            entry.ids.get("ssp_x").map(|id| id.uid.as_str()),
            Some("uid-1"),
            "conflict must re-merge our update onto the concurrently revived row"
        );
        assert!(entry.consent.ok, "concurrent revive keeps the row live");
    }

    #[test]
    fn snapshot_upsert_rejects_tombstone() {
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = snapshot_ec_id();
        kv.create(&ec_id, &KvEntry::tombstone(1000))
            .expect("should seed tombstone");
        let snapshot = kv.load_snapshot(&ec_id);
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = kv.upsert_partner_ids_from_snapshot(&ec_id, &updates, snapshot);

        assert!(
            outcome
                .entry_for(&ec_id)
                .is_some_and(|entry| entry.ids.is_empty()),
            "a tombstone must reject partner enrichment"
        );
        let (stored, _) = kv
            .get(&ec_id)
            .expect("should read store")
            .expect("tombstone should remain");
        assert!(stored.ids.is_empty(), "no update should reach the store");
    }

    #[test]
    fn snapshot_upsert_store_failure_returns_failed_not_request_local() {
        let graph = KvIdentityGraph::new(WriteFailingEcKv::new());
        let ec_id = snapshot_ec_id();
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(live_entry()),
            generation: Some(1),
        };
        let updates = [PartnerIdUpdate::new("ssp_x", "uid-1")];

        let outcome = graph.upsert_partner_ids_from_snapshot(&ec_id, &updates, snapshot);

        assert!(
            matches!(outcome, EcKvSnapshot::Failed { .. }),
            "a store write failure must not claim request-local IDs were persisted"
        );
    }

    #[test]
    fn tombstone_existing_from_snapshot_retries_cas_conflict() {
        let graph = KvIdentityGraph::new(ConflictInjectingEcKv::new(1, false));
        let ec_id = snapshot_ec_id();
        graph.create(&ec_id, &live_entry()).expect("should seed");
        let snapshot = graph.load_snapshot(&ec_id);

        let outcome = graph.tombstone_existing_from_snapshot(&ec_id, snapshot);

        assert!(
            outcome
                .entry_for(&ec_id)
                .is_some_and(|entry| !entry.consent.ok),
            "should retry the conflict and persist the tombstone"
        );
    }

    #[test]
    fn tombstone_gen_unavailable_survives_four_conflicts_then_writes() {
        // A generation-unavailable snapshot refreshes once before its CAS. That
        // refresh must not spend a CAS attempt, so a withdrawal tombstone still
        // persists after four conflicts and a successful fifth write.
        let graph = KvIdentityGraph::new(ConflictInjectingEcKv::new(4, false));
        let ec_id = snapshot_ec_id();
        graph.create(&ec_id, &live_entry()).expect("should seed");
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(live_entry()),
            generation: None,
        };

        let outcome = graph.tombstone_existing_from_snapshot(&ec_id, snapshot);

        assert!(
            outcome
                .entry_for(&ec_id)
                .is_some_and(|entry| !entry.consent.ok),
            "the fifth CAS attempt must persist the tombstone after a refresh and four conflicts"
        );
    }

    #[test]
    fn tombstone_existing_from_snapshot_store_failure_returns_failed() {
        let graph = KvIdentityGraph::new(WriteFailingEcKv::new());
        let ec_id = snapshot_ec_id();
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(live_entry()),
            generation: Some(1),
        };

        let outcome = graph.tombstone_existing_from_snapshot(&ec_id, snapshot);

        assert!(matches!(outcome, EcKvSnapshot::Failed { .. }));
    }

    #[test]
    fn tombstone_existing_from_snapshot_noop_when_row_disappears_on_retry() {
        let store = DisappearOnConflictEcKv::new(1);
        store.seed_live(&snapshot_ec_id());
        let graph = KvIdentityGraph::new(store);
        let ec_id = snapshot_ec_id();
        let snapshot = graph.load_snapshot(&ec_id);

        let outcome = graph.tombstone_existing_from_snapshot(&ec_id, snapshot);

        assert!(
            matches!(outcome, EcKvSnapshot::Missing { .. }),
            "a row that disappears during retry becomes a no-op"
        );
        assert!(
            graph.get(&ec_id).expect("should read store").is_none(),
            "must not recreate the disappeared key"
        );
    }

    #[test]
    fn tombstone_existing_from_snapshot_reretries_failed_snapshot_read() {
        // A prior request-scoped read failed, so the snapshot is `Failed`. A
        // withdrawal must not silently drop consent removal: re-read the store
        // and tombstone the row if it is authoritatively present.
        let kv = KvIdentityGraph::in_memory("test_store");
        let ec_id = snapshot_ec_id();
        kv.create(&ec_id, &live_entry()).expect("should seed live");

        let outcome = kv.tombstone_existing_from_snapshot(
            &ec_id,
            EcKvSnapshot::Failed {
                ec_id: ec_id.clone(),
            },
        );

        assert!(
            outcome
                .entry_for(&ec_id)
                .is_some_and(|entry| !entry.consent.ok),
            "a failed snapshot must re-read and persist the tombstone"
        );
        let (stored, _) = kv
            .get(&ec_id)
            .expect("should read store")
            .expect("should preserve existing key");
        assert!(!stored.consent.ok, "withdrawal must reach the store");
    }
}
