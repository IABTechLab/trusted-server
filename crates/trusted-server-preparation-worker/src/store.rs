//! The worker's durable state: one exclusively owned redb database whose
//! transactions serialize admission, fixed-capacity slots, claim fencing,
//! attempts, damping, inventory, retirement and the marker outbox.
//!
//! Every write is one redb write transaction, which commits atomically or
//! not at all, and redb admits one writer at a time, so no lock or
//! compare-and-swap is fabricated over anything else. A second process
//! cannot open the same file: redb holds an exclusive lock on it, and the
//! worker refuses to start without it. Management and the CLI go through
//! the owner's HTTP routes, never through a second open.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use redb::{
    Database, ReadableDatabase as _, ReadableTable as _, ReadableTableMetadata as _,
    TableDefinition,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use trusted_server_provenance_encypher::preparation::pipeline::{FailureClass, PlannedMedia};
use trusted_server_provenance_encypher::preparation::protocol::{
    MarkerEnvelope, ObservedItem, PREPARATION_PROTOCOL, ResultEnvelope, Target, is_digest,
};

#[path = "store_files.rs"]
mod files;
#[path = "store_maintenance.rs"]
mod maintenance;

const JOBS: TableDefinition<&str, &[u8]> = TableDefinition::new("jobs");
const OUTCOMES: TableDefinition<&str, &[u8]> = TableDefinition::new("outcomes");
const ARTIFACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("artifacts");
const PLANS: TableDefinition<&str, &[u8]> = TableDefinition::new("plans");
const INVENTORY: TableDefinition<&str, &[u8]> = TableDefinition::new("inventory");
const RETIRED: TableDefinition<&str, &[u8]> = TableDefinition::new("retired");
/// Hard byte reservations by owner: `source:<sha256>`, `probe:<sha256>`,
/// `artifact:<identity>` for retained material, `temp:<identity>` for a
/// large source being read. Reserved before anything is written.
const QUOTA: TableDefinition<&str, u64> = TableDefinition::new("quota");
const OUTBOX: TableDefinition<&str, &[u8]> = TableDefinition::new("outbox");
const SOURCES: TableDefinition<&str, &[u8]> = TableDefinition::new("sources");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const TEXT_OPERATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("text_operations");

fn reserve_in(
    tx: &redb::WriteTransaction,
    owner: &str,
    bytes: u64,
    limit: u64,
) -> Result<bool, StoreError> {
    let mut quota = tx.open_table(QUOTA).map_err(storage)?;
    let mut used = 0_u64;
    let mut held = 0_u64;
    for entry in quota.iter().map_err(storage)? {
        let (key, value) = entry.map_err(storage)?;
        used = used
            .checked_add(value.value())
            .ok_or_else(|| storage("quota overflow"))?;
        if key.value() == owner {
            held = value.value();
        }
    }
    let wanted = held.max(bytes);
    if used
        .saturating_sub(held)
        .checked_add(wanted)
        .is_none_or(|total| total > limit)
    {
        return Ok(false);
    }
    quota.insert(owner, wanted).map_err(storage)?;
    Ok(true)
}
const PROBE_REFERENCES: TableDefinition<&str, &[u8]> = TableDefinition::new("probe_references");

fn put_outcome(
    tx: &redb::WriteTransaction,
    target: &Target,
    identity: &str,
    outcome: &Outcome,
    limit: u64,
) -> Result<(), StoreError> {
    let key = outcome_key(target, identity);
    let encoded = encode(outcome)?;
    if !reserve_in(
        tx,
        &format!("outcome:{key}"),
        (key.len() + encoded.len()) as u64,
        limit,
    )? {
        return Err(StoreError::Quota);
    }
    tx.open_table(OUTCOMES)
        .map_err(storage)?
        .insert(key.as_str(), encoded.as_slice())
        .map_err(storage)?;
    Ok(())
}

fn damp_exhausted(
    tx: &redb::WriteTransaction,
    job: &Job,
    code: &str,
    now: u64,
    limit: u64,
) -> Result<(), StoreError> {
    let outcome = Outcome {
        code: code.to_owned(),
        class: OutcomeClass::Exhausted,
        until: now.saturating_add(3600),
    };
    for item in &job.observed {
        put_outcome(tx, &job.target, &item.identity, &outcome, limit)?;
    }
    put_outcome(tx, &job.target, "*", &outcome, limit)
}

/// Why a store operation failed.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
pub enum StoreError {
    /// Another process owns the database.
    #[display("the preparation store is owned by another process")]
    AlreadyOwned,
    /// The database failed.
    #[display("the preparation store failed: {_0}")]
    Storage(String),
    /// A hard byte reservation could not be obtained.
    #[display("the preparation artifact quota is exhausted")]
    Quota,
    /// Independent inventory capacity has been reached.
    #[display("the preparation inventory is full")]
    InventoryFull,
}

impl core::error::Error for StoreError {}

fn storage<E: std::fmt::Display>(error: E) -> StoreError {
    StoreError::Storage(error.to_string())
}

/// The fixed bounds the store enforces in its transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds {
    /// Most jobs, of any origin.
    pub max_jobs: usize,
    /// Most queued job metadata bytes.
    pub max_queue_bytes: usize,
    /// Slots reader hints can never take.
    pub reserved_jobs: usize,
    /// Bytes reader hints can never take.
    pub reserved_bytes: usize,
    /// Most attempts of one job.
    pub max_attempts: u32,
    /// Longest one job is retried, in seconds.
    pub retry_window_seconds: u64,
    /// Longest backoff, in seconds.
    pub max_backoff_seconds: u64,
    /// Claim lease, in seconds.
    pub lease_seconds: u64,
    /// Most inventory targets.
    pub max_inventory: usize,
    /// Most items one job holds.
    pub max_items_per_job: usize,
    /// The hard byte quota of sources, temporary files, probes and retained
    /// outputs together.
    pub quota_bytes: u64,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_jobs: 1024,
            max_queue_bytes: 8 * 1024 * 1024,
            reserved_jobs: 256,
            reserved_bytes: 2 * 1024 * 1024,
            max_attempts: 6,
            retry_window_seconds: 24 * 3600,
            max_backoff_seconds: 300,
            lease_seconds: 120,
            max_inventory: 1024,
            max_items_per_job: 64,
            quota_bytes: 10 * 1024 * 1024 * 1024,
        }
    }
}

/// What started a job, which decides its capacity and stability rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobOrigin {
    /// A reader's hint: only observed items, only while they match.
    Signal,
    /// An authenticated publication event or operator warm: the current
    /// snapshot is intentionally selected.
    Warm,
    /// Periodic reconciliation: only items stable across two snapshots.
    Reconcile,
}

/// A job's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    /// Waiting until `ready_at`.
    Pending {
        /// Unix seconds it may be claimed from.
        ready_at: u64,
    },
    /// Claimed under `token` until `lease_until`.
    Processing {
        /// The claim fence.
        token: u64,
        /// Unix seconds the lease ends.
        lease_until: u64,
    },
}

/// One durable job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// Its key, the target's.
    pub key: String,
    /// The target.
    pub target: Target,
    /// What started it.
    pub origin: JobOrigin,
    /// Items a reader observed, for a signal job.
    pub observed: Vec<ObservedItem>,
    /// Bumped whenever a newer observation replaces the desired state.
    pub generation: u64,
    /// A newer observation arrived while it was processing.
    pub follow_up: bool,
    /// Attempts made.
    pub attempts: u32,
    /// Unix seconds it was first admitted.
    pub admitted_at: u64,
    /// Its state.
    pub state: JobState,
    /// Whether it holds a reserved slot.
    pub reserved: bool,
}

/// A claimed job, fenced by its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The job as claimed.
    pub job: Job,
    /// The fence.
    pub token: u64,
}

/// The damped outcome of one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// Its code.
    pub code: String,
    /// Its class.
    pub class: OutcomeClass,
    /// Unix seconds it stops suppressing hints, `u64::MAX` until the
    /// identity changes or an operator retries.
    pub until: u64,
}

/// How an item's outcome damps further work on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    /// Its snapshot did not match the observation: five minutes.
    SourceUnstable,
    /// Retries exhausted: one hour.
    Exhausted,
    /// Integrity, privacy, unsupported carrier or existing provenance:
    /// until the identity changes or an operator retries.
    Terminal,
}

/// What admission decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Admission {
    /// Committed as a new job.
    Queued {
        /// The job key.
        key: String,
    },
    /// Merged into an existing job for the same target.
    Coalesced {
        /// The job key.
        key: String,
    },
    /// Every item is damped, retired or already published.
    Damped,
    /// No capacity; a later submission is safe.
    QueueFull,
}

/// A published identity's durable artifact: the envelope the delivery
/// instance verified, kept to republish after cache eviction without a
/// new sign operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The envelope.
    pub envelope: ResultEnvelope,
    /// The target it belongs to.
    pub target: Target,
    /// Whether the delivery instance acknowledged it.
    pub acknowledged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArtifactFile {
    sha256: String,
    bytes: u64,
    acknowledged: bool,
}

/// A retired identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retirement {
    /// Its epoch.
    pub epoch: u64,
    /// Whether the delivery instance acknowledged its marker. This is not
    /// proof every replica or downstream cache changed.
    pub acknowledged: bool,
}

/// One inventory target and the identities of its last snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryEntry {
    /// The target.
    pub target: Target,
    /// Unix seconds it was last reconciled.
    pub last_scan: u64,
    /// The item identities its last snapshot held.
    pub last_identities: Vec<String>,
}

/// A source snapshot held on the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// Its media type.
    pub mime: String,
    /// Whether the page and asset passed public admission, so the publisher
    /// route may serve it.
    pub public: bool,
    /// Identities whose retained artifacts reference it.
    pub references: Vec<String>,
}

/// The store's counts, for status.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Pending jobs.
    pub pending: usize,
    /// Processing jobs.
    pub processing: usize,
    /// Retained artifacts, independently of publication acknowledgement.
    pub artifacts: usize,
    /// Retained artifacts awaiting an acknowledged result PUT.
    pub publication_pending: usize,
    /// Result PUTs acknowledged by the delivery instance.
    pub publication_acknowledged: usize,
    /// Retirements awaiting marker acknowledgement.
    pub retirement_pending: usize,
    /// Retirements acknowledged by the delivery instance.
    pub retirement_acknowledged: usize,
    /// Inventory targets.
    pub inventory: usize,
    /// Hard artifact bytes reserved, including temporary and probe material.
    pub reserved_bytes: u64,
}

/// The durable store.
pub struct Store {
    db: Database,
    bounds: Bounds,
    root: PathBuf,
    /// Serializes filesystem ownership changes with retirement and cleanup.
    objects: Mutex<()>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Store")
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(value).map_err(storage)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    serde_json::from_slice(bytes).map_err(storage)
}

/// A target's job key.
#[must_use]
pub fn target_key(target: &Target) -> String {
    format!("{}?{}", target.path, target.variant)
}

fn outcome_key(target: &Target, identity: &str) -> String {
    format!("{}#{identity}", target_key(target))
}

impl Store {
    /// Opens or creates the store at `path` as its only owner.
    ///
    /// # Errors
    ///
    /// [`StoreError::AlreadyOwned`] where another process holds it, and
    /// [`StoreError::InventoryFull`] where the persisted inventory exceeds
    /// `bounds.max_inventory`; nothing is removed, and raising the bound
    /// reopens it unchanged.
    pub fn open(path: &Path, bounds: Bounds) -> Result<Self, StoreError> {
        if bounds.max_jobs == 0
            || bounds.reserved_jobs > bounds.max_jobs
            || bounds.reserved_bytes > bounds.max_queue_bytes
            || bounds.max_items_per_job == 0
            || bounds.max_attempts == 0
            || bounds.lease_seconds < 3
        {
            return Err(storage("invalid worker store bounds"));
        }
        let db = Database::create(path).map_err(|error| match error {
            redb::DatabaseError::DatabaseAlreadyOpen => StoreError::AlreadyOwned,
            other => storage(other),
        })?;
        let root = path.parent().unwrap_or_else(|| Path::new(".")).to_owned();
        let store = Self {
            db,
            bounds,
            root,
            objects: Mutex::new(()),
        };
        let tx = store.db.begin_write().map_err(storage)?;
        for table in [
            JOBS,
            OUTCOMES,
            ARTIFACTS,
            PLANS,
            INVENTORY,
            RETIRED,
            OUTBOX,
            SOURCES,
            META,
            TEXT_OPERATIONS,
            PROBE_REFERENCES,
        ] {
            tx.open_table(table).map_err(storage)?;
        }
        tx.open_table(QUOTA).map_err(storage)?;
        let inventory = tx
            .open_table(INVENTORY)
            .map_err(storage)?
            .len()
            .map_err(storage)? as usize;
        if inventory > bounds.max_inventory {
            tx.abort().map_err(storage)?;
            return Err(StoreError::InventoryFull);
        }
        tx.commit().map_err(storage)?;
        Ok(store)
    }

    /// The bounds.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    fn read_value<T: DeserializeOwned>(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        let table = tx.open_table(table).map_err(storage)?;
        table
            .get(key)
            .map_err(storage)?
            .map(|value| decode(value.value()))
            .transpose()
    }

    fn read_all<T: DeserializeOwned>(
        &self,
        table: TableDefinition<&str, &[u8]>,
    ) -> Result<Vec<(String, T)>, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        let table = tx.open_table(table).map_err(storage)?;
        let mut out = Vec::new();
        for entry in table.iter().map_err(storage)? {
            let (key, value) = entry.map_err(storage)?;
            out.push((key.value().to_owned(), decode(value.value())?));
        }
        Ok(out)
    }

    /// Admits a job for `target` in one transaction: drops retired, damped
    /// and already published items, coalesces with the target's existing
    /// job, and enforces the total and reserved bounds. A reader hint never
    /// takes a reserved slot or byte.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn admit(
        &self,
        target: &Target,
        origin: JobOrigin,
        observed: &[ObservedItem],
        now: u64,
    ) -> Result<Admission, StoreError> {
        let key = target_key(target);
        let tx = self.db.begin_write().map_err(storage)?;
        let admission = {
            let mut jobs = tx.open_table(JOBS).map_err(storage)?;
            let outcomes = tx.open_table(OUTCOMES).map_err(storage)?;
            let retired = tx.open_table(RETIRED).map_err(storage)?;
            if let Some(value) = outcomes
                .get(outcome_key(target, "*").as_str())
                .map_err(storage)?
            {
                let outcome: Outcome = decode(value.value())?;
                if outcome.class == OutcomeClass::Exhausted
                    && outcome.until > now
                    && origin != JobOrigin::Signal
                {
                    return Ok(Admission::Damped);
                }
            }
            let mut kept = Vec::new();
            for item in observed {
                if retired
                    .get(item.identity.as_str())
                    .map_err(storage)?
                    .is_some()
                {
                    continue;
                }
                if let Some(outcome) = outcomes
                    .get(outcome_key(target, &item.identity).as_str())
                    .map_err(storage)?
                {
                    let outcome: Outcome = decode(outcome.value())?;
                    if now < outcome.until {
                        continue;
                    }
                }
                // An item already published but still missed by a reader was
                // evicted from delivery storage; the job republishes its
                // durable artifact and never signs it again.
                if !kept.contains(item) {
                    kept.push(item.clone());
                }
            }
            let damped = origin == JobOrigin::Signal && kept.is_empty();
            let existing_job = if damped {
                None
            } else {
                let existing = jobs.get(key.as_str()).map_err(storage)?;
                existing
                    .map(|value| decode::<Job>(value.value()))
                    .transpose()?
            };
            if damped {
                Admission::Damped
            } else if let Some(mut job) = existing_job {
                // Existing work already covers reconciliation. A pending
                // publisher warm will acquire current content itself; a reader
                // cannot grow that job using the publisher's reserved bytes.
                if origin == JobOrigin::Reconcile
                    || (origin == JobOrigin::Signal
                        && job.reserved
                        && matches!(job.state, JobState::Pending { .. }))
                {
                    return Ok(Admission::Coalesced { key });
                }
                let desired: Vec<_> = kept
                    .into_iter()
                    .take(self.bounds.max_items_per_job)
                    .collect();
                let desired_origin = origin;
                if job.observed != desired || job.origin != desired_origin {
                    job.observed = desired;
                    job.origin = desired_origin;
                    job.reserved = origin != JobOrigin::Signal;
                    job.generation = job.generation.saturating_add(1);
                    job.follow_up |= matches!(job.state, JobState::Processing { .. });
                    if !matches!(job.state, JobState::Processing { .. }) {
                        job.attempts = 0;
                        job.admitted_at = now;
                        job.state = JobState::Pending { ready_at: now };
                    }
                }
                let encoded = encode(&job)?;
                let mut total = encoded.len() + 128;
                let mut readers = if job.reserved { 0 } else { encoded.len() + 128 };
                let mut reader_count = usize::from(!job.reserved);
                for entry in jobs.iter().map_err(storage)? {
                    let (other_key, value) = entry.map_err(storage)?;
                    if other_key.value() == key {
                        continue;
                    }
                    total += value.value().len() + 128;
                    if !decode::<Job>(value.value())?.reserved {
                        readers += value.value().len() + 128;
                        reader_count += 1;
                    }
                }
                if total > self.bounds.max_queue_bytes
                    || (!job.reserved
                        && readers
                            > self
                                .bounds
                                .max_queue_bytes
                                .saturating_sub(self.bounds.reserved_bytes))
                    || (!job.reserved
                        && reader_count
                            > self
                                .bounds
                                .max_jobs
                                .saturating_sub(self.bounds.reserved_jobs))
                {
                    return Ok(Admission::QueueFull);
                }
                jobs.insert(key.as_str(), encoded.as_slice())
                    .map_err(storage)?;
                Admission::Coalesced { key: key.clone() }
            } else {
                let mut count = 0_usize;
                let mut bytes = 0_usize;
                let mut reader_count = 0_usize;
                let mut reader_bytes = 0_usize;
                for entry in jobs.iter().map_err(storage)? {
                    let (_, value) = entry.map_err(storage)?;
                    let job: Job = decode(value.value())?;
                    count += 1;
                    bytes += value.value().len() + 128;
                    if !job.reserved {
                        reader_count += 1;
                        reader_bytes += value.value().len() + 128;
                    }
                }
                let job = Job {
                    key: key.clone(),
                    target: target.clone(),
                    origin,
                    observed: kept
                        .into_iter()
                        .take(self.bounds.max_items_per_job)
                        .collect(),
                    generation: 0,
                    follow_up: false,
                    attempts: 0,
                    admitted_at: now,
                    state: JobState::Pending { ready_at: now },
                    reserved: origin != JobOrigin::Signal,
                };
                let encoded = encode(&job)?;
                let size = encoded.len() + 128;
                let total_ok =
                    count < self.bounds.max_jobs && bytes + size <= self.bounds.max_queue_bytes;
                let reader_ok = reader_count
                    < self
                        .bounds
                        .max_jobs
                        .saturating_sub(self.bounds.reserved_jobs)
                    && reader_bytes + size
                        <= self
                            .bounds
                            .max_queue_bytes
                            .saturating_sub(self.bounds.reserved_bytes);
                if !total_ok || (origin == JobOrigin::Signal && !reader_ok) {
                    Admission::QueueFull
                } else {
                    jobs.insert(key.as_str(), encoded.as_slice())
                        .map_err(storage)?;
                    Admission::Queued { key: key.clone() }
                }
            }
        };
        tx.commit().map_err(storage)?;
        Ok(admission)
    }

    /// Claims the next ready job, fencing it with a fresh token.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn claim(&self, now: u64, token: u64) -> Result<Option<Claim>, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let claimed = {
            let mut jobs = tx.open_table(JOBS).map_err(storage)?;
            let mut ready = None;
            let mut exhausted = Vec::new();
            for entry in jobs.iter().map_err(storage)? {
                let (key, value) = entry.map_err(storage)?;
                let job: Job = decode(value.value())?;
                if let JobState::Pending { ready_at } = job.state
                    && ready_at <= now
                {
                    if job.attempts >= self.bounds.max_attempts
                        || now.saturating_sub(job.admitted_at) >= self.bounds.retry_window_seconds
                    {
                        exhausted.push((key.value().to_owned(), job));
                        continue;
                    }
                    let older = ready.as_ref().is_none_or(|(_, prior): &(String, Job)| {
                        let prior_ready = match prior.state {
                            JobState::Pending { ready_at } => ready_at,
                            JobState::Processing { .. } => u64::MAX,
                        };
                        (ready_at, job.admitted_at) < (prior_ready, prior.admitted_at)
                    });
                    if older {
                        ready = Some((key.value().to_owned(), job));
                    }
                }
            }
            for (key, job) in exhausted {
                damp_exhausted(
                    &tx,
                    &job,
                    "retry_window_exhausted",
                    now,
                    self.bounds.quota_bytes,
                )?;
                jobs.remove(key.as_str()).map_err(storage)?;
            }
            match ready {
                Some((key, mut job)) => {
                    job.attempts += 1;
                    job.state = JobState::Processing {
                        token,
                        lease_until: now + self.bounds.lease_seconds,
                    };
                    jobs.insert(key.as_str(), encode(&job)?.as_slice())
                        .map_err(storage)?;
                    Some(Claim { job, token })
                }
                None => None,
            }
        };
        tx.commit().map_err(storage)?;
        Ok(claimed)
    }

    /// Renews a claim's lease, only for its owner. Whether it still owns it.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn renew(&self, key: &str, token: u64, now: u64) -> Result<bool, StoreError> {
        self.with_claimed(key, token, |job| {
            if matches!(job.state, JobState::Processing { lease_until, .. } if lease_until <= now) {
                return Ok(None);
            }
            job.state = JobState::Processing {
                token,
                lease_until: now + self.bounds_lease(),
            };
            Ok(Some(()))
        })
        .map(|renewed| renewed.is_some())
    }

    fn bounds_lease(&self) -> u64 {
        self.bounds.lease_seconds
    }

    /// Applies `change` to a job still claimed under `token`; a stale claim
    /// changes nothing.
    fn with_claimed<R>(
        &self,
        key: &str,
        token: u64,
        change: impl FnOnce(&mut Job) -> Result<Option<R>, StoreError>,
    ) -> Result<Option<R>, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let result = {
            let mut jobs = tx.open_table(JOBS).map_err(storage)?;
            let current = jobs
                .get(key)
                .map_err(storage)?
                .map(|value| decode::<Job>(value.value()))
                .transpose()?;
            match current {
                Some(mut job) if matches!(job.state, JobState::Processing { token: held, .. } if held == token) =>
                {
                    let result = change(&mut job)?;
                    jobs.insert(key, encode(&job)?.as_slice())
                        .map_err(storage)?;
                    result
                }
                _ => None,
            }
        };
        tx.commit().map_err(storage)?;
        Ok(result)
    }

    /// Settles a claim that finished its work: removes the job, unless a
    /// newer observation arrived, which is scheduled at once. A stale claim
    /// settles nothing. Whether the claim was current.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn complete(&self, key: &str, token: u64, now: u64) -> Result<bool, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let current = {
            let mut jobs = tx.open_table(JOBS).map_err(storage)?;
            let job = jobs
                .get(key)
                .map_err(storage)?
                .map(|value| decode::<Job>(value.value()))
                .transpose()?;
            match job {
                Some(mut job) if matches!(job.state, JobState::Processing { token: held, lease_until } if held == token && lease_until > now) =>
                {
                    if job.follow_up {
                        job.follow_up = false;
                        job.attempts = 0;
                        job.admitted_at = now;
                        job.state = JobState::Pending { ready_at: now };
                        jobs.insert(key, encode(&job)?.as_slice())
                            .map_err(storage)?;
                    } else {
                        jobs.remove(key).map_err(storage)?;
                    }
                    true
                }
                _ => false,
            }
        };
        tx.commit().map_err(storage)?;
        Ok(current)
    }

    /// Settles a claim whose attempt failed transiently: retried with
    /// bounded exponential backoff while attempts and the retry window
    /// last, else removed with each observed item damped for an hour.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn retry(&self, key: &str, token: u64, now: u64, code: &str) -> Result<bool, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut jobs = tx.open_table(JOBS).map_err(storage)?;
            let current = jobs
                .get(key)
                .map_err(storage)?
                .map(|value| decode::<Job>(value.value()))
                .transpose()?;
            let Some(mut job) = current else {
                return Ok(false);
            };
            if !matches!(job.state, JobState::Processing { token: held, lease_until } if held == token && lease_until > now)
            {
                return Ok(false);
            }
            if job.follow_up {
                // Failure of an older generation never damps or deletes the desired one.
                job.follow_up = false;
                job.attempts = 0;
                job.admitted_at = now;
                job.state = JobState::Pending { ready_at: now };
                jobs.insert(key, encode(&job)?.as_slice())
                    .map_err(storage)?;
            } else if job.attempts >= self.bounds.max_attempts
                || now.saturating_sub(job.admitted_at) >= self.bounds.retry_window_seconds
            {
                damp_exhausted(&tx, &job, code, now, self.bounds.quota_bytes)?;
                jobs.remove(key).map_err(storage)?;
            } else {
                let delay = 5_u64
                    .saturating_mul(1_u64 << job.attempts.min(16))
                    .min(self.bounds.max_backoff_seconds);
                job.state = JobState::Pending {
                    ready_at: now.saturating_add(delay),
                };
                jobs.insert(key, encode(&job)?.as_slice())
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)?;
        Ok(true)
    }

    /// One job.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn job(&self, key: &str) -> Result<Option<Job>, StoreError> {
        self.read_value(JOBS, key)
    }

    /// Returns every processing job whose lease ended to pending, as on
    /// restart. How many were recovered.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn recover_expired(&self, now: u64) -> Result<usize, StoreError> {
        self.recover_claims(now, false)
    }

    /// Recovers every claim at exclusive-owner startup, using a finite retry time.
    ///
    /// # Errors
    /// A store transaction failure.
    pub fn recover_all(&self, now: u64) -> Result<usize, StoreError> {
        self.recover_claims(now, true)
    }

    fn recover_claims(&self, now: u64, all: bool) -> Result<usize, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let mut recovered = 0;
        {
            let mut jobs = tx.open_table(JOBS).map_err(storage)?;
            let mut expired = Vec::new();
            for entry in jobs.iter().map_err(storage)? {
                let (key, value) = entry.map_err(storage)?;
                let job: Job = decode(value.value())?;
                if let JobState::Processing { lease_until, .. } = job.state
                    && (all || lease_until <= now)
                {
                    expired.push((key.value().to_owned(), job));
                }
            }
            for (key, mut job) in expired {
                if job.follow_up {
                    job.follow_up = false;
                    job.attempts = 0;
                    job.admitted_at = now;
                } else if job.attempts >= self.bounds.max_attempts
                    || now.saturating_sub(job.admitted_at) >= self.bounds.retry_window_seconds
                {
                    damp_exhausted(
                        &tx,
                        &job,
                        "retry_window_exhausted",
                        now,
                        self.bounds.quota_bytes,
                    )?;
                    jobs.remove(key.as_str()).map_err(storage)?;
                    recovered += 1;
                    continue;
                }
                job.state = JobState::Pending { ready_at: now };
                jobs.insert(key.as_str(), encode(&job)?.as_slice())
                    .map_err(storage)?;
                recovered += 1;
            }
        }
        tx.commit().map_err(storage)?;
        Ok(recovered)
    }

    /// Records one item's damped outcome.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn record_outcome(
        &self,
        target: &Target,
        identity: &str,
        code: &str,
        class: OutcomeClass,
        now: u64,
    ) -> Result<(), StoreError> {
        let until = match class {
            OutcomeClass::SourceUnstable => now + 300,
            OutcomeClass::Exhausted => now + 3600,
            OutcomeClass::Terminal => u64::MAX,
        };
        let outcome = Outcome {
            code: code.to_owned(),
            class,
            until,
        };
        let tx = self.db.begin_write().map_err(storage)?;
        put_outcome(&tx, target, identity, &outcome, self.bounds.quota_bytes)?;
        tx.commit().map_err(storage)
    }

    /// Records an outcome only for the current claim and desired generation.
    /// Late failures cannot damp an operator retry or a newer observation.
    ///
    /// # Errors
    /// A failed store transaction.
    pub fn record_claim_outcome(
        &self,
        claim: &Claim,
        identity: &str,
        code: &str,
        class: OutcomeClass,
        now: u64,
    ) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let current = tx
            .open_table(JOBS)
            .map_err(storage)?
            .get(claim.job.key.as_str())
            .map_err(storage)?
            .map(|value| decode::<Job>(value.value()))
            .transpose()?;
        if current.is_some_and(|job| job.generation == claim.job.generation
            && matches!(job.state, JobState::Processing { token, lease_until } if token == claim.token && lease_until > now))
        {
            let until = match class {
                OutcomeClass::SourceUnstable => now.saturating_add(300),
                OutcomeClass::Exhausted => now.saturating_add(3600),
                OutcomeClass::Terminal => u64::MAX,
            };
            let outcome = Outcome { code: code.to_owned(), class, until };
            put_outcome(&tx, &claim.job.target, identity, &outcome, self.bounds.quota_bytes)?;
        }
        tx.commit().map_err(storage)
    }

    /// Invalidates terminal damping when relevant configured behavior or the
    /// authenticated probe release changes. Retirement tombstones are never cleared.
    ///
    /// # Errors
    /// A failed store transaction.
    pub fn refresh_context(&self, scope: &str, identity: &str) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut meta = tx.open_table(META).map_err(storage)?;
            let key = format!("damping_context:{scope}");
            let prior = meta
                .get(key.as_str())
                .map_err(storage)?
                .map(|value| decode::<String>(value.value()))
                .transpose()?;
            if prior.as_deref().is_some_and(|prior| prior != identity) {
                let mut outcomes = tx.open_table(OUTCOMES).map_err(storage)?;
                let mut stale = Vec::new();
                for entry in outcomes.iter().map_err(storage)? {
                    let (key, value) = entry.map_err(storage)?;
                    if decode::<Outcome>(value.value())?.class == OutcomeClass::Terminal {
                        stale.push(key.value().to_owned());
                    }
                }
                for key in stale {
                    outcomes.remove(key.as_str()).map_err(storage)?;
                    tx.open_table(QUOTA)
                        .map_err(storage)?
                        .remove(format!("outcome:{key}").as_str())
                        .map_err(storage)?;
                }
            }
            meta.insert(key.as_str(), encode(&identity)?.as_slice())
                .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }

    /// Clears every outcome of `target`, for an operator's explicit retry.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn clear_outcomes(&self, target: &Target, now: u64) -> Result<(), StoreError> {
        let prefix = format!("{}#", target_key(target));
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut outcomes = tx.open_table(OUTCOMES).map_err(storage)?;
            let mut keys = Vec::new();
            for entry in outcomes.iter().map_err(storage)? {
                let (key, _) = entry.map_err(storage)?;
                if key.value().starts_with(&prefix) {
                    keys.push(key.value().to_owned());
                }
            }
            for key in keys {
                outcomes.remove(key.as_str()).map_err(storage)?;
                tx.open_table(QUOTA)
                    .map_err(storage)?
                    .remove(format!("outcome:{key}").as_str())
                    .map_err(storage)?;
            }
        }
        {
            let mut jobs = tx.open_table(JOBS).map_err(storage)?;
            let key = target_key(target);
            let current = jobs
                .get(key.as_str())
                .map_err(storage)?
                .map(|value| decode::<Job>(value.value()))
                .transpose()?;
            if let Some(mut job) = current {
                job.generation = job.generation.saturating_add(1);
                if matches!(job.state, JobState::Processing { .. }) {
                    job.follow_up = true;
                } else {
                    job.attempts = 0;
                    job.admitted_at = now;
                    job.state = JobState::Pending { ready_at: now };
                }
                jobs.insert(key.as_str(), encode(&job)?.as_slice())
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)
    }

    /// The outcome recorded for an item.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn outcome(&self, target: &Target, identity: &str) -> Result<Option<Outcome>, StoreError> {
        self.read_value(OUTCOMES, &outcome_key(target, identity))
    }

    /// Records a verified plan before its finalize, so a crash after
    /// finalize recovers the receipt rather than planning again.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn record_plan(&self, identity: &str, planned: &PlannedMedia) -> Result<(), StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        self.reserve_identity(identity)?;
        let encoded = encode(planned)?;
        if !self.reserve(&format!("plan:{identity}"), encoded.len() as u64)? {
            return Err(StoreError::Quota);
        }
        let tx = self.db.begin_write().map_err(storage)?;
        if tx
            .open_table(RETIRED)
            .map_err(storage)?
            .get(identity)
            .map_err(storage)?
            .is_some()
        {
            return Err(StoreError::Storage(
                "identity retired during planning".to_owned(),
            ));
        }
        tx.open_table(PLANS)
            .map_err(storage)?
            .insert(identity, encoded.as_slice())
            .map_err(storage)?;
        let mut references = tx.open_table(PROBE_REFERENCES).map_err(storage)?;
        let sha = planned.release.sha256.as_str();
        let mut identities: Vec<String> = references
            .get(sha)
            .map_err(storage)?
            .map(|value| decode(value.value()))
            .transpose()?
            .unwrap_or_default();
        if !identities.iter().any(|existing| existing == identity) {
            identities.push(identity.to_owned());
        }
        let encoded = encode(&identities)?;
        if !reserve_in(
            &tx,
            &format!("probe-record:{sha}"),
            encoded.len() as u64,
            self.bounds.quota_bytes,
        )? {
            return Err(StoreError::Quota);
        }
        references
            .insert(sha, encoded.as_slice())
            .map_err(storage)?;
        drop(references);
        tx.commit().map_err(storage)
    }

    /// The plan recorded for an identity.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn plan(&self, identity: &str) -> Result<Option<PlannedMedia>, StoreError> {
        self.read_value(PLANS, identity)
    }

    /// The distinct pinned releases retained plans require, without retaining all geometry in memory.
    ///
    /// # Errors
    /// A failed store read.
    pub fn probe_releases(
        &self,
    ) -> Result<
        Vec<trusted_server_provenance_encypher::preparation::pipeline::ProbeRelease>,
        StoreError,
    > {
        let tx = self.db.begin_read().map_err(storage)?;
        let table = tx.open_table(PLANS).map_err(storage)?;
        let mut releases = Vec::new();
        for entry in table.iter().map_err(storage)? {
            let (_, value) = entry.map_err(storage)?;
            let plan: PlannedMedia = decode(value.value())?;
            if !releases.contains(&plan.release) {
                releases.push(plan.release);
            }
        }
        Ok(releases)
    }

    /// Whether interrupted text operations can recover without creation capability.
    ///
    /// # Errors
    /// A failed store read.
    pub fn has_text_operations(&self) -> Result<bool, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        Ok(tx
            .open_table(TEXT_OPERATIONS)
            .map_err(storage)?
            .len()
            .map_err(storage)?
            > 0)
    }

    /// Whether an identity owns reconstruction inputs or a durable output.
    ///
    /// # Errors
    /// A failed store read.
    pub fn has_reconstruction(&self, identity: &str) -> Result<bool, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        for table in [PLANS, TEXT_OPERATIONS, ARTIFACTS] {
            if tx
                .open_table(table)
                .map_err(storage)?
                .get(identity)
                .map_err(storage)?
                .is_some()
            {
                return Ok(true);
            }
        }
        Ok(self
            .root
            .join("artifacts")
            .join(format!("{identity}.json"))
            .exists())
    }

    /// Drops failed pre-plan source references without creating a retirement or spent key.
    ///
    /// # Errors
    /// A failed store transaction or file removal; uncertain bytes remain charged.
    pub fn discard_unplanned_sources(&self, identity: &str) -> Result<(), StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        if self.has_reconstruction(identity)? {
            return Ok(());
        }
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut sources = tx.open_table(SOURCES).map_err(storage)?;
            let mut changed = Vec::new();
            for entry in sources.iter().map_err(storage)? {
                let (sha, value) = entry.map_err(storage)?;
                let mut source: SourceRecord = decode(value.value())?;
                source.references.retain(|reference| reference != identity);
                source.public &= !source.references.is_empty();
                changed.push((sha.value().to_owned(), source));
            }
            for (sha, source) in changed {
                sources
                    .insert(sha.as_str(), encode(&source)?.as_slice())
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)?;
        self.remove_unreferenced_sources()?;
        if self.retirement(identity)?.is_none() {
            self.release(&format!("identity:{identity}"))?;
        }
        Ok(())
    }

    /// Records a text attempt before its first network write. Returns false
    /// for an existing operation, which must use recovery rather than signing.
    ///
    /// # Errors
    /// Refuses retired identities, conflicting requests and exhausted storage.
    pub fn begin_text(
        &self,
        identity: &str,
        request: &trusted_server_provenance_encypher::preparation::pipeline::TextSignRequest,
    ) -> Result<bool, StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        if self.retirement(identity)?.is_some() {
            return Err(storage("text identity retired"));
        }
        self.reserve_identity(identity)?;
        let file = self.write_text_file(identity, request)?;
        let tx = self.db.begin_write().map_err(storage)?;
        if tx
            .open_table(RETIRED)
            .map_err(storage)?
            .get(identity)
            .map_err(storage)?
            .is_some()
        {
            return Err(storage("text identity retired"));
        }
        let created = {
            let mut operations = tx.open_table(TEXT_OPERATIONS).map_err(storage)?;
            let existing = operations
                .get(identity)
                .map_err(storage)?
                .map(|value| decode::<ArtifactFile>(value.value()))
                .transpose()?;
            if let Some(existing) = existing {
                if existing.sha256 != file.sha256 || existing.bytes != file.bytes {
                    return Err(storage("text operation conflict"));
                }
                false
            } else {
                operations
                    .insert(identity, encode(&file)?.as_slice())
                    .map_err(storage)?;
                true
            }
        };
        tx.commit().map_err(storage)?;
        Ok(created)
    }

    /// The identities retaining a pinned probe release, for lifecycle checks.
    ///
    /// # Errors
    /// A store read failure.
    pub fn probe_references(&self, sha256: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .read_value(PROBE_REFERENCES, sha256)?
            .unwrap_or_default())
    }

    fn put<T: Serialize>(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        tx.open_table(table)
            .map_err(storage)?
            .insert(key, encode(value)?.as_slice())
            .map_err(storage)?;
        tx.commit().map_err(storage)
    }

    /// Records a published artifact, unless its identity was retired after
    /// it was prepared: a late result never outlives a retirement.
    /// Whether it was recorded.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn record_artifact(&self, artifact: &Artifact) -> Result<bool, StoreError> {
        let identity = artifact.envelope.identity.as_str();
        let _guard = self.objects.lock().map_err(storage)?;
        if self.retirement(identity)?.is_some() {
            return Ok(false);
        }
        self.reserve_identity(identity)?;
        // Stream JSON into an owned filesystem object, never a large redb value.
        let file = self.write_artifact_file(identity, artifact)?;
        let tx = self.db.begin_write().map_err(storage)?;
        tx.open_table(ARTIFACTS)
            .map_err(storage)?
            .insert(identity, encode(&file)?.as_slice())
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
        self.shrink_reservation(
            &format!("artifact:{identity}"),
            file.bytes + encode(&file)?.len() as u64,
        )?;
        Ok(true)
    }

    /// The artifact of an identity.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn artifact(&self, identity: &str) -> Result<Option<Artifact>, StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        let Some(file) = self.read_value::<ArtifactFile>(ARTIFACTS, identity)? else {
            return Ok(None);
        };
        self.read_artifact_file(identity, &file).map(Some)
    }

    /// Records publication acknowledgement without rewriting immutable artifact bytes.
    ///
    /// # Errors
    /// A failed store transaction.
    pub fn acknowledge_artifact(&self, identity: &str) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut artifacts = tx.open_table(ARTIFACTS).map_err(storage)?;
            let current = artifacts
                .get(identity)
                .map_err(storage)?
                .map(|value| decode::<ArtifactFile>(value.value()))
                .transpose()?;
            if let Some(mut file) = current {
                file.acknowledged = true;
                artifacts
                    .insert(identity, encode(&file)?.as_slice())
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)
    }

    /// The current retirement epoch.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn retirement_epoch(&self) -> Result<u64, StoreError> {
        Ok(self
            .read_value::<u64>(META, "retirement_epoch")?
            .unwrap_or(0))
    }

    /// Retires `identity` in one transaction: increments the epoch, records
    /// the tombstone, drops its artifact and plan, and queues its marker in
    /// the outbox before anything else is cleaned up.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn retire(&self, identity: &str, deployment_id: &str) -> Result<u64, StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        if let Some(retirement) = self.retirement(identity)? {
            return Ok(retirement.epoch);
        }
        self.reserve_identity(identity)?;
        let tx = self.db.begin_write().map_err(storage)?;
        let epoch = {
            let mut meta = tx.open_table(META).map_err(storage)?;
            let epoch = meta
                .get("retirement_epoch")
                .map_err(storage)?
                .map(|value| decode::<u64>(value.value()))
                .transpose()?
                .unwrap_or(0)
                + 1;
            meta.insert("retirement_epoch", encode(&epoch)?.as_slice())
                .map_err(storage)?;
            tx.open_table(RETIRED)
                .map_err(storage)?
                .insert(
                    identity,
                    encode(&Retirement {
                        epoch,
                        acknowledged: false,
                    })?
                    .as_slice(),
                )
                .map_err(storage)?;
            let marker = MarkerEnvelope {
                protocol: PREPARATION_PROTOCOL.to_owned(),
                deployment_id: deployment_id.to_owned(),
                identity: identity.to_owned(),
                epoch,
            };
            tx.open_table(OUTBOX)
                .map_err(storage)?
                .insert(identity, encode(&marker)?.as_slice())
                .map_err(storage)?;
            tx.open_table(ARTIFACTS)
                .map_err(storage)?
                .remove(identity)
                .map_err(storage)?;
            tx.open_table(PLANS)
                .map_err(storage)?
                .remove(identity)
                .map_err(storage)?;
            tx.open_table(QUOTA)
                .map_err(storage)?
                .remove(format!("plan:{identity}").as_str())
                .map_err(storage)?;
            tx.open_table(TEXT_OPERATIONS)
                .map_err(storage)?
                .remove(identity)
                .map_err(storage)?;
            let mut sources = tx.open_table(SOURCES).map_err(storage)?;
            let mut changed = Vec::new();
            for entry in sources.iter().map_err(storage)? {
                let (sha, value) = entry.map_err(storage)?;
                let mut source: SourceRecord = decode(value.value())?;
                source.references.retain(|reference| reference != identity);
                source.public &= !source.references.is_empty();
                changed.push((sha.value().to_owned(), source));
            }
            for (sha, source) in changed {
                sources
                    .insert(sha.as_str(), encode(&source)?.as_slice())
                    .map_err(storage)?;
            }
            drop(sources);
            let mut probes = tx.open_table(PROBE_REFERENCES).map_err(storage)?;
            let mut changed = Vec::new();
            for entry in probes.iter().map_err(storage)? {
                let (sha, value) = entry.map_err(storage)?;
                let mut references: Vec<String> = decode(value.value())?;
                references.retain(|reference| reference != identity);
                changed.push((sha.value().to_owned(), references));
            }
            for (sha, references) in changed {
                probes
                    .insert(sha.as_str(), encode(&references)?.as_slice())
                    .map_err(storage)?;
            }
            epoch
        };
        tx.commit().map_err(storage)?;
        // The tombstone and outbox commit first. A failed filesystem removal
        // stays charged and can be retried safely on restart.
        self.remove_artifact_file(identity)?;
        self.remove_text_file(identity)?;
        self.remove_unreferenced_sources()?;
        Ok(epoch)
    }

    /// Whether an identity is retired.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn retirement(&self, identity: &str) -> Result<Option<Retirement>, StoreError> {
        self.read_value(RETIRED, identity)
    }

    /// Markers waiting for the delivery instance's acknowledgement.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn outbox(&self) -> Result<Vec<MarkerEnvelope>, StoreError> {
        Ok(self
            .read_all::<MarkerEnvelope>(OUTBOX)?
            .into_iter()
            .map(|(_, marker)| marker)
            .collect())
    }

    /// A bounded round-robin marker batch; failed markers cannot starve later ones.
    ///
    /// # Errors
    /// A failed cursor transaction or outbox read.
    pub fn outbox_batch(&self, limit: usize) -> Result<Vec<MarkerEnvelope>, StoreError> {
        let cursor: String = self.read_value(META, "outbox_cursor")?.unwrap_or_default();
        let markers = self.outbox()?;
        let batch: Vec<_> = markers
            .iter()
            .filter(|marker| marker.identity > cursor)
            .chain(markers.iter().filter(|marker| marker.identity <= cursor))
            .take(limit.min(16))
            .cloned()
            .collect();
        if let Some(last) = batch.last() {
            self.put(META, "outbox_cursor", &last.identity)?;
        }
        Ok(batch)
    }

    /// Records the delivery instance's acknowledgement of a marker at
    /// `epoch`; an older epoch's acknowledgement settles nothing newer.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn acknowledge_marker(&self, identity: &str, epoch: u64) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut retired = tx.open_table(RETIRED).map_err(storage)?;
            let current = retired
                .get(identity)
                .map_err(storage)?
                .map(|value| decode::<Retirement>(value.value()))
                .transpose()?;
            if let Some(mut retirement) = current
                && retirement.epoch == epoch
            {
                retirement.acknowledged = true;
                retired
                    .insert(identity, encode(&retirement)?.as_slice())
                    .map_err(storage)?;
                tx.open_table(OUTBOX)
                    .map_err(storage)?
                    .remove(identity)
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)
    }

    /// Registers inventory targets, within the bound. How many were added.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn register(&self, targets: &[Target]) -> Result<usize, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let mut added = 0;
        {
            let mut inventory = tx.open_table(INVENTORY).map_err(storage)?;
            let existing = inventory.len().map_err(storage)? as usize;
            for target in targets {
                let key = target_key(target);
                if inventory.get(key.as_str()).map_err(storage)?.is_some() {
                    continue;
                }
                if existing + added >= self.bounds.max_inventory {
                    return Err(StoreError::InventoryFull);
                }
                let entry = InventoryEntry {
                    target: target.clone(),
                    last_scan: 0,
                    last_identities: Vec::new(),
                };
                let encoded = encode(&entry)?;
                if !reserve_in(
                    &tx,
                    &format!("inventory:{key}"),
                    encoded.len() as u64,
                    self.bounds.quota_bytes,
                )? {
                    return Err(StoreError::Quota);
                }
                inventory
                    .insert(key.as_str(), encoded.as_slice())
                    .map_err(storage)?;
                added += 1;
            }
        }
        tx.commit().map_err(storage)?;
        Ok(added)
    }

    /// Up to `limit` inventory targets after the persisted cursor whose last
    /// reconciliation is older than `interval`, advancing the cursor.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn due_inventory(
        &self,
        now: u64,
        interval: u64,
        limit: usize,
    ) -> Result<Vec<Target>, StoreError> {
        let cursor: String = self
            .read_value(META, "inventory_cursor")?
            .unwrap_or_default();
        let entries = self.read_all::<InventoryEntry>(INVENTORY)?;
        let limit = limit.min(128);
        if limit == 0 {
            return Ok(Vec::new());
        }
        // Leave one cursor revolution of slack so a target cannot miss the
        // configured maximum merely by falling just before the scan cursor.
        let threshold =
            interval.saturating_sub((entries.len().div_ceil(limit) as u64).saturating_mul(60));
        let mut ordered: Vec<&(String, InventoryEntry)> =
            entries.iter().filter(|(key, _)| *key > cursor).collect();
        ordered.extend(entries.iter().filter(|(key, _)| *key <= cursor));
        let mut due = Vec::new();
        let mut last = cursor.clone();
        for (key, entry) in ordered.into_iter().take(limit) {
            last.clone_from(key);
            if entry.last_scan == 0 || now.saturating_sub(entry.last_scan) >= threshold {
                due.push(entry.target.clone());
            }
        }
        self.put(META, "inventory_cursor", &last)?;
        Ok(due)
    }

    fn reserve_identity(&self, identity: &str) -> Result<(), StoreError> {
        // This survives artifact deletion and funds the spent key and immutable
        // retirement outbox even when the rest of the hard quota is full.
        if self.reserve(&format!("identity:{identity}"), 1024)? {
            Ok(())
        } else {
            Err(StoreError::Quota)
        }
    }

    /// The identities of a target's last snapshot, and records this one's.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn observe_snapshot(
        &self,
        target: &Target,
        identities: &[String],
        now: u64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let previous = Self::observe_in(&tx, target, identities, now, self.bounds.quota_bytes)?;
        tx.commit().map_err(storage)?;
        Ok(previous)
    }

    /// Observes an admitted snapshot only for the live claim and desired generation.
    /// A stale completion cannot overwrite stability evidence for its follow-up.
    ///
    /// # Errors
    /// A failed transaction.
    pub fn observe_claim_snapshot(
        &self,
        claim: &Claim,
        identities: &[String],
        now: u64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let current = tx
            .open_table(JOBS)
            .map_err(storage)?
            .get(claim.job.key.as_str())
            .map_err(storage)?
            .map(|value| decode::<Job>(value.value()))
            .transpose()?;
        if !current.is_some_and(|job| job.generation == claim.job.generation
            && matches!(job.state, JobState::Processing { token, lease_until } if token == claim.token && lease_until > now))
        {
            return Ok(None);
        }
        {
            let mut outcomes = tx.open_table(OUTCOMES).map_err(storage)?;
            let key = outcome_key(&claim.job.target, "*");
            let terminal = outcomes
                .get(key.as_str())
                .map_err(storage)?
                .map(|value| decode::<Outcome>(value.value()))
                .transpose()?
                .is_some_and(|outcome| outcome.class == OutcomeClass::Terminal);
            if terminal {
                outcomes.remove(key.as_str()).map_err(storage)?;
                tx.open_table(QUOTA)
                    .map_err(storage)?
                    .remove(format!("outcome:{key}").as_str())
                    .map_err(storage)?;
            }
        }
        let previous = Self::observe_in(
            &tx,
            &claim.job.target,
            identities,
            now,
            self.bounds.quota_bytes,
        )?;
        tx.commit().map_err(storage)?;
        Ok(previous)
    }

    fn observe_in(
        tx: &redb::WriteTransaction,
        target: &Target,
        identities: &[String],
        now: u64,
        limit: u64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        let key = target_key(target);
        let mut inventory = tx.open_table(INVENTORY).map_err(storage)?;
        let current = inventory
            .get(key.as_str())
            .map_err(storage)?
            .map(|value| decode::<InventoryEntry>(value.value()))
            .transpose()?;
        let Some(mut entry) = current else {
            return Ok(None);
        };
        let previous = std::mem::replace(&mut entry.last_identities, identities.to_vec());
        entry.last_scan = now;
        let encoded = encode(&entry)?;
        if !reserve_in(tx, &format!("inventory:{key}"), encoded.len() as u64, limit)? {
            return Err(StoreError::Quota);
        }
        inventory
            .insert(key.as_str(), encoded.as_slice())
            .map_err(storage)?;
        Ok(Some(previous))
    }

    /// Records a source snapshot, public only after page and asset
    /// admission, referenced by `identity`.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn record_source(
        &self,
        sha256: &str,
        mime: &str,
        public: bool,
        identity: &str,
    ) -> Result<(), StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        self.reserve_identity(identity)?;
        self.record_source_locked(sha256, mime, public, identity)
    }

    fn record_source_locked(
        &self,
        sha256: &str,
        mime: &str,
        public: bool,
        identity: &str,
    ) -> Result<(), StoreError> {
        if self.retirement(identity)?.is_some() {
            return Err(storage("source owner retired"));
        }
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut sources = tx.open_table(SOURCES).map_err(storage)?;
            let mut record: SourceRecord = sources
                .get(sha256)
                .map_err(storage)?
                .map(|value| decode(value.value()))
                .transpose()?
                .unwrap_or(SourceRecord {
                    mime: mime.to_owned(),
                    public,
                    references: Vec::new(),
                });
            if record.mime != mime {
                return Err(storage("source media type conflict"));
            }
            record.public |= public;
            if !record
                .references
                .iter()
                .any(|existing| existing == identity)
            {
                record.references.push(identity.to_owned());
            }
            let encoded = encode(&record)?;
            if !reserve_in(
                &tx,
                &format!("source-record:{sha256}"),
                encoded.len() as u64,
                self.bounds.quota_bytes,
            )? {
                return Err(StoreError::Quota);
            }
            sources
                .insert(sha256, encoded.as_slice())
                .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }

    /// A source snapshot's record.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn source(&self, sha256: &str) -> Result<Option<SourceRecord>, StoreError> {
        self.read_value(SOURCES, sha256)
    }

    /// Counts for status.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where a read fails.
    pub fn counts(&self) -> Result<Counts, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        let mut counts = Counts::default();
        for entry in tx
            .open_table(JOBS)
            .map_err(storage)?
            .iter()
            .map_err(storage)?
        {
            let (_, value) = entry.map_err(storage)?;
            match decode::<Job>(value.value())?.state {
                JobState::Pending { .. } => counts.pending += 1,
                JobState::Processing { .. } => counts.processing += 1,
            }
        }
        for entry in tx
            .open_table(ARTIFACTS)
            .map_err(storage)?
            .iter()
            .map_err(storage)?
        {
            let (_, value) = entry.map_err(storage)?;
            counts.artifacts += 1;
            if decode::<ArtifactFile>(value.value())?.acknowledged {
                counts.publication_acknowledged += 1;
            } else {
                counts.publication_pending += 1;
            }
        }
        for entry in tx
            .open_table(RETIRED)
            .map_err(storage)?
            .iter()
            .map_err(storage)?
        {
            let (_, value) = entry.map_err(storage)?;
            if decode::<Retirement>(value.value())?.acknowledged {
                counts.retirement_acknowledged += 1;
            } else {
                counts.retirement_pending += 1;
            }
        }
        counts.inventory = usize::try_from(
            tx.open_table(INVENTORY)
                .map_err(storage)?
                .len()
                .map_err(storage)?,
        )
        .map_err(storage)?;
        for entry in tx
            .open_table(QUOTA)
            .map_err(storage)?
            .iter()
            .map_err(storage)?
        {
            counts.reserved_bytes = counts
                .reserved_bytes
                .saturating_add(entry.map_err(storage)?.1.value());
        }
        Ok(counts)
    }
}

impl Store {
    /// Reserves `bytes` for `owner` in one transaction, against every
    /// reservation every task holds. Whether it fit: a full quota refuses
    /// the work rather than evicting a retained result. An owner that
    /// already holds a reservation keeps the larger of the two.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn reserve(&self, owner: &str, bytes: u64) -> Result<bool, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let fits = reserve_in(&tx, owner, bytes, self.bounds.quota_bytes)?;
        tx.commit().map_err(storage)?;
        Ok(fits)
    }

    /// Returns conservative surplus after its durable object has been measured.
    ///
    /// # Errors
    /// Refuses growth or a failed transaction.
    pub fn shrink_reservation(&self, owner: &str, bytes: u64) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut quota = tx.open_table(QUOTA).map_err(storage)?;
            let held = quota
                .get(owner)
                .map_err(storage)?
                .map_or(0, |value| value.value());
            if held < bytes {
                return Err(storage("cannot grow by shrinking reservation"));
            }
            quota.insert(owner, bytes).map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }

    /// Releases `owner`'s reservation.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    pub fn release(&self, owner: &str) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        tx.open_table(QUOTA)
            .map_err(storage)?
            .remove(owner)
            .map_err(storage)?;
        tx.commit().map_err(storage)
    }

    fn reservation(&self, owner: &str) -> Result<Option<u64>, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        Ok(tx
            .open_table(QUOTA)
            .map_err(storage)?
            .get(owner)
            .map_err(storage)?
            .map(|value| value.value()))
    }

    /// The bytes reserved now.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the read fails.
    pub fn reserved(&self) -> Result<u64, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        let quota = tx.open_table(QUOTA).map_err(storage)?;
        let mut used = 0_u64;
        for entry in quota.iter().map_err(storage)? {
            used = used.saturating_add(entry.map_err(storage)?.1.value());
        }
        Ok(used)
    }

    /// Reconciles reservations on restart: every temporary reservation is
    /// released, since no task survives a restart and its file is removed
    /// with it, and every artifact reservation whose artifact is gone, as
    /// after a retirement, is released. Retained material keeps its bytes.
    /// How many were released.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] where the transaction fails.
    fn reconcile_reservations(&self) -> Result<usize, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let released = {
            let artifacts = tx.open_table(ARTIFACTS).map_err(storage)?;
            let plans = tx.open_table(PLANS).map_err(storage)?;
            let text = tx.open_table(TEXT_OPERATIONS).map_err(storage)?;
            let sources = tx.open_table(SOURCES).map_err(storage)?;
            let probes = tx.open_table(PROBE_REFERENCES).map_err(storage)?;
            let retired = tx.open_table(RETIRED).map_err(storage)?;
            let mut quota = tx.open_table(QUOTA).map_err(storage)?;
            let mut stale = Vec::new();
            for entry in quota.iter().map_err(storage)? {
                let (key, _) = entry.map_err(storage)?;
                let key = key.value().to_owned();
                let orphan = match key.split_once(':') {
                    Some(("temp", _)) => true,
                    Some(("artifact", identity)) => {
                        artifacts.get(identity).map_err(storage)?.is_none()
                            && !self
                                .root
                                .join("artifacts")
                                .join(format!("{identity}.json"))
                                .try_exists()
                                .map_err(storage)?
                            && plans.get(identity).map_err(storage)?.is_none()
                            && text.get(identity).map_err(storage)?.is_none()
                    }
                    Some(("identity", identity)) if is_digest(identity) => {
                        retired.get(identity).map_err(storage)?.is_none()
                            && artifacts.get(identity).map_err(storage)?.is_none()
                            && plans.get(identity).map_err(storage)?.is_none()
                            && text.get(identity).map_err(storage)?.is_none()
                            && !self
                                .root
                                .join("artifacts")
                                .join(format!("{identity}.json"))
                                .try_exists()
                                .map_err(storage)?
                    }
                    Some(("text", identity)) if is_digest(identity) => {
                        text.get(identity).map_err(storage)?.is_none()
                            && !self
                                .root
                                .join("texts")
                                .join(format!("{identity}.json"))
                                .try_exists()
                                .map_err(storage)?
                    }
                    Some(("source", digest)) if is_digest(digest) => {
                        sources.get(digest).map_err(storage)?.is_none()
                            && !self
                                .root
                                .join("sources")
                                .join(digest)
                                .try_exists()
                                .map_err(storage)?
                    }
                    Some(("probe", digest)) if is_digest(digest) => {
                        probes.get(digest).map_err(storage)?.is_none()
                            && !self
                                .root
                                .join("probes")
                                .join(format!("{digest}.wasm"))
                                .try_exists()
                                .map_err(storage)?
                    }
                    _ => false,
                };
                if orphan {
                    stale.push(key);
                }
            }
            for key in &stale {
                quota.remove(key.as_str()).map_err(storage)?;
            }
            stale.len()
        };
        tx.commit().map_err(storage)?;
        Ok(released)
    }
}

/// How a pipeline failure damps the item.
#[must_use]
pub fn outcome_class(class: FailureClass) -> Option<OutcomeClass> {
    match class {
        FailureClass::Transient | FailureClass::PolicyChanged | FailureClass::Missing => None,
        FailureClass::Integrity
        | FailureClass::Privacy
        | FailureClass::Unsupported
        | FailureClass::ExistingProvenance => Some(OutcomeClass::Terminal),
    }
}

#[cfg(test)]
mod tests {
    use trusted_server_provenance_encypher::preparation::pipeline::TextCacheScope;
    use trusted_server_provenance_encypher::preparation::protocol::ItemKind;

    use super::*;

    fn target(path: &str) -> Target {
        Target {
            path: path.to_owned(),
            variant: String::new(),
        }
    }

    fn item(identity: &str) -> ObservedItem {
        ObservedItem {
            kind: ItemKind::Embedded,
            identity: identity.repeat(64 / identity.len()),
        }
    }

    fn open(dir: &tempfile::TempDir, bounds: Bounds) -> Store {
        Store::open(&dir.path().join("prep.redb"), bounds).expect("should open the store")
    }

    #[test]
    fn quota_reservations_are_shared_hard_and_reconciled_after_restart() {
        let dir = tempfile::tempdir().expect("should make a directory");
        let bounds = Bounds {
            quota_bytes: 100,
            ..Bounds::default()
        };
        {
            let store = open(&dir, bounds);
            assert!(
                store.reserve("source:a", 40).expect("reserve"),
                "should fit"
            );
            assert!(store.reserve("temp:b", 50).expect("reserve"), "should fit");
            assert!(
                !store.reserve("artifact:c", 20).expect("reserve"),
                "a reservation past the quota is refused, never squeezed in"
            );
            assert!(
                store.reserve("source:a", 30).expect("reserve"),
                "an owner keeps the larger of its reservations"
            );
            assert_eq!(store.reserved().expect("read"), 90);
            assert!(store.reserve("artifact:c", 10).expect("reserve"));
        }
        let store = open(&dir, bounds);
        assert_eq!(
            store.reconcile_reservations().expect("reconcile"),
            2,
            "a temporary reservation and an artifact reservation without its artifact are released"
        );
        assert_eq!(
            store.reserved().expect("read"),
            40,
            "retained source bytes stay reserved across a restart"
        );
    }

    #[test]
    fn a_second_owner_cannot_open_the_store() {
        let dir = tempfile::tempdir().expect("should make a directory");
        let _owner = open(&dir, Bounds::default());
        let second = Store::open(&dir.path().join("prep.redb"), Bounds::default());
        assert_eq!(
            second.expect_err("should refuse a second owner"),
            StoreError::AlreadyOwned
        );
    }

    #[test]
    fn identical_hints_coalesce_and_a_hint_during_processing_schedules_one_follow_up() {
        let dir = tempfile::tempdir().expect("should make a directory");
        let store = open(&dir, Bounds::default());
        let page = target("/news/a");
        assert_eq!(
            store
                .admit(&page, JobOrigin::Signal, &[item("a")], 10)
                .expect("admit"),
            Admission::Queued {
                key: target_key(&page)
            }
        );
        assert_eq!(
            store
                .admit(&page, JobOrigin::Signal, &[item("a")], 11)
                .expect("admit"),
            Admission::Coalesced {
                key: target_key(&page)
            },
            "an identical hint should coalesce"
        );
        let claim = store.claim(12, 7).expect("claim").expect("should claim");
        store
            .admit(&page, JobOrigin::Signal, &[item("b")], 13)
            .expect("admit");
        assert!(store.complete(&claim.job.key, 7, 14).expect("complete"));
        let job = store
            .job(&claim.job.key)
            .expect("read")
            .expect("should keep a follow-up");
        assert!(
            matches!(job.state, JobState::Pending { .. }),
            "follow-up is pending"
        );
        assert!(!job.follow_up, "exactly one follow-up is scheduled");
        assert!(
            job.observed.contains(&item("b")),
            "the newer observation is kept"
        );
    }

    #[test]
    fn a_stale_claim_cannot_settle_or_renew_a_newer_one() {
        let dir = tempfile::tempdir().expect("should make a directory");
        let store = open(&dir, Bounds::default());
        let page = target("/news/a");
        store.admit(&page, JobOrigin::Warm, &[], 0).expect("admit");
        let first = store.claim(0, 1).expect("claim").expect("should claim");
        assert_eq!(
            store.recover_expired(1000).expect("recover"),
            1,
            "expired lease returns"
        );
        let second = store
            .claim(1000, 2)
            .expect("claim")
            .expect("should reclaim");
        assert!(
            !store.complete(&first.job.key, 1, 1001).expect("complete"),
            "stale token"
        );
        assert!(
            !store.renew(&first.job.key, 1, 1001).expect("renew"),
            "stale renewal"
        );
        assert!(
            store.job(&second.job.key).expect("read").is_some(),
            "newer claim survives"
        );
        assert!(store.complete(&second.job.key, 2, 1002).expect("complete"));
    }

    #[test]
    fn reader_hints_cannot_take_reserved_capacity() {
        let dir = tempfile::tempdir().expect("should make a directory");
        let bounds = Bounds {
            max_jobs: 3,
            reserved_jobs: 1,
            ..Bounds::default()
        };
        let store = open(&dir, bounds);
        for index in 0..2 {
            let admitted = store
                .admit(
                    &target(&format!("/r/{index}")),
                    JobOrigin::Signal,
                    &[item("a")],
                    0,
                )
                .expect("admit");
            assert!(matches!(admitted, Admission::Queued { .. }));
        }
        assert_eq!(
            store
                .admit(&target("/r/9"), JobOrigin::Signal, &[item("a")], 0)
                .expect("admit"),
            Admission::QueueFull,
            "a hint should not take the reserved slot"
        );
        assert!(
            matches!(
                store
                    .admit(&target("/w/1"), JobOrigin::Warm, &[], 0)
                    .expect("admit"),
                Admission::Queued { .. }
            ),
            "publication work should use the reserved slot"
        );
    }

    #[test]
    fn damping_is_per_item_and_retirement_survives_restart() {
        let dir = tempfile::tempdir().expect("should make a directory");
        let page = target("/news/a");
        {
            let store = open(&dir, Bounds::default());
            store
                .record_outcome(
                    &page,
                    &item("a").identity,
                    "source_unstable",
                    OutcomeClass::SourceUnstable,
                    100,
                )
                .expect("record");
            assert_eq!(
                store
                    .admit(&page, JobOrigin::Signal, &[item("a")], 101)
                    .expect("admit"),
                Admission::Damped,
                "the unstable item is damped"
            );
            assert!(
                matches!(
                    store
                        .admit(&page, JobOrigin::Signal, &[item("a"), item("b")], 102)
                        .expect("admit"),
                    Admission::Queued { .. }
                ),
                "its stable sibling still queues"
            );
            assert_eq!(
                store
                    .retire(&item("c").identity, "deployment")
                    .expect("retire"),
                1
            );
        }
        let store = open(&dir, Bounds::default());
        assert!(
            store
                .retirement(&item("c").identity)
                .expect("read")
                .is_some()
        );
        assert_eq!(
            store.outbox().expect("outbox").len(),
            1,
            "the marker stays queued"
        );
        store
            .acknowledge_marker(&item("c").identity, 1)
            .expect("ack");
        assert!(store.outbox().expect("outbox").is_empty());
        let artifact = Artifact {
            envelope: ResultEnvelope {
                protocol: PREPARATION_PROTOCOL.to_owned(),
                deployment_id: "deployment".to_owned(),
                identity: item("c").identity,
                retirement_epoch: 0,
                variant: String::new(),
                item: trusted_server_provenance_encypher::preparation::protocol::EnvelopeItem::Text {
                    request: trusted_server_provenance_encypher::preparation::pipeline::TextSignRequest {
                        text: "t".to_owned(),
                        page_url: "https://www.example.com/news/a".to_owned(),
                        document_title: None,
                        boundary_selector: "article".to_owned(),
                        expected_policy_digest: "a".repeat(64),
                        cache_scope: TextCacheScope::ExactRequest,
                    },
                    idempotency_key: "k".to_owned(),
                },
            },
            target: page.clone(),
            acknowledged: true,
        };
        assert!(
            !store.record_artifact(&artifact).expect("record"),
            "a late result never outlives its retirement"
        );
    }

    #[test]
    fn crash_after_artifact_rename_before_metadata_commit_adopts_exact_old_epoch() {
        let dir = tempfile::tempdir().expect("should create durable storage");
        let identity = "a".repeat(64);
        let artifact = Artifact {
            envelope: ResultEnvelope {
                protocol: PREPARATION_PROTOCOL.to_owned(), deployment_id: "deployment".to_owned(),
                identity: identity.clone(), retirement_epoch: 0, variant: String::new(),
                item: trusted_server_provenance_encypher::preparation::protocol::EnvelopeItem::Text {
                    request: trusted_server_provenance_encypher::preparation::pipeline::TextSignRequest {
                        text: "durable exact text".to_owned(), page_url: "https://www.example.com/article".to_owned(),
                        document_title: None, boundary_selector: "article".to_owned(), expected_policy_digest: "b".repeat(64),
                        cache_scope: TextCacheScope::ExactRequest,
                    },
                    idempotency_key: "exact-operation".to_owned(),
                },
            },
            target: target("/article"), acknowledged: false,
        };
        {
            let store = open(&dir, Bounds::default());
            {
                let _guard = store
                    .objects
                    .lock()
                    .expect("should own filesystem mutation");
                store
                    .reserve_identity(&identity)
                    .expect("should fund immutable identity");
                store
                    .write_artifact_file(&identity, &artifact)
                    .expect("should fsync and rename real artifact");
                // Simulate death before ARTIFACTS metadata insertion, exactly
                // between the two durable stages of record_artifact.
            }
            assert!(
                store
                    .artifact(&identity)
                    .expect("should lack metadata before recovery")
                    .is_none()
            );
            store
                .retire(&"c".repeat(64), "deployment")
                .expect("should advance unrelated retirement epoch");
        }
        let store = open(&dir, Bounds::default());
        store
            .recover_storage()
            .expect("should adopt owned orphan after restart");
        let restored = store
            .artifact(&identity)
            .expect("should read durable result")
            .expect("should recover completed artifact");
        assert_eq!(
            restored.envelope, artifact.envelope,
            "recovery preserves exact operation, bytes and old epoch"
        );
        assert_eq!(
            store
                .retirement_epoch()
                .expect("should retain unrelated retirement"),
            1
        );
        assert_eq!(restored.envelope.retirement_epoch, 0);
        assert!(
            store
                .reserved()
                .expect("should retain artifact and spent-key charges")
                > 2048
        );
    }
}
